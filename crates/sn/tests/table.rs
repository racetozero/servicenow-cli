//! Table commands, output formats and the error contract.

mod support;

use serde_json::json;
use support::{Cli, on, query, result};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const ID: &str = "9d385017c611228701d22104cc95c371";

#[tokio::test(flavor = "multi_thread")]
async fn list_sends_parameters_and_reports_the_next_page() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/now/table/incident"))
        .respond_with(|request: &Request| {
            let q = query(request);
            assert_eq!(q["sysparm_query"], "active=true^priority=1");
            assert_eq!(q["sysparm_fields"], "number,state");
            assert_eq!(q["sysparm_limit"], "2");
            assert_eq!(q["sysparm_display_value"], "true");
            assert_eq!(q["sysparm_exclude_reference_link"], "true");
            result(
                json!([{ "number": "INC1", "state": "New" }, { "number": "INC2", "state": "New" }]),
            )
            .insert_header("x-total-count", "5")
        })
        .mount(&server)
        .await;
    let cli = Cli::basic(&server);
    let base = [
        "table",
        "list",
        "incident",
        "-q",
        "active=true^priority=1",
        "-f",
        "number,state",
        "-n",
        "2",
    ];
    let run = cli.run(&[&base[..], &["--agent"]].concat()).await.ok();
    let envelope = run.envelope();
    assert_eq!(envelope["command"], "table list");
    assert_eq!(
        envelope["meta"],
        json!({ "total": 5, "next_offset": 2, "next": "add --offset 2", "count": 2 })
    );
    let piped = cli.run(&base).await.ok();
    assert_eq!(piped.json()[1]["number"], "INC2", "a pipe gets JSON");
    let csv = cli.run(&[&base[..], &["-o", "csv"]].concat()).await.ok();
    assert_eq!(csv.stdout, "number,state\nINC1,New\nINC2,New\n");
    let table = cli.run(&[&base[..], &["-o", "table"]].concat()).await.ok();
    assert!(
        table.stdout.starts_with("NUMBER  STATE\nINC1"),
        "{}",
        table.stdout
    );
    let lines = cli.run(&[&base[..], &["-o", "jsonl"]].concat()).await.ok();
    assert_eq!(lines.stdout.lines().count(), 2);
    let yaml = cli.run(&[&base[..], &["-o", "yaml"]].concat()).await.ok();
    assert!(yaml.stdout.starts_with("- number: INC1"), "{}", yaml.stdout);
    let jq = cli
        .run(&[&base[..], &["--jq", ".[].number"]].concat())
        .await
        .ok();
    assert_eq!(jq.stdout, "INC1\nINC2\n");
    let select = cli
        .run(&[&base[..], &["--select", "number"]].concat())
        .await
        .ok();
    assert_eq!(select.json()[0], json!({ "number": "INC1" }));
}

#[tokio::test(flavor = "multi_thread")]
async fn all_pages_by_key_and_resumes() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/now/table/incident"))
        .respond_with(|request: &Request| {
            let q = query(request)["sysparm_query"].clone();
            match q.as_str() {
                "a=1^NQb=2^ORDERBYsys_id" => result(json!([{ "sys_id": "1" }, { "sys_id": "2" }])),
                "a=1^sys_id>2^NQb=2^sys_id>2^ORDERBYsys_id" => result(json!([{ "sys_id": "3" }])),
                "a=1^sys_id>3^NQb=2^sys_id>3^ORDERBYsys_id" => result(json!([])),
                other => panic!("unexpected query {other}"),
            }
        })
        .mount(&server)
        .await;
    let cli = Cli::basic(&server);
    let args = [
        "table",
        "list",
        "incident",
        "-q",
        "a=1^NQb=2",
        "--all",
        "-n",
        "2",
    ];
    let run = cli.run(&args).await.ok();
    assert_eq!(
        run.stdout,
        "{\"sys_id\":\"1\"}\n{\"sys_id\":\"2\"}\n{\"sys_id\":\"3\"}\n"
    );
    let run = cli
        .run(&[&args[..], &["--resume-from", "2"]].concat())
        .await
        .ok();
    assert_eq!(run.stdout, "{\"sys_id\":\"3\"}\n");
    let run = cli.run(&[&args[..], &["--agent"]].concat()).await.ok();
    assert_eq!(
        run.envelope()["meta"],
        json!({ "complete": true, "count": 3 })
    );
    let run = cli
        .run(&[&args[..], &["--max-records", "1", "--array"]].concat())
        .await
        .ok();
    assert_eq!(run.json().as_array().map(Vec::len), Some(1));
    assert!(run.stderr.contains("stopped after 1 records"));
}

#[tokio::test(flavor = "multi_thread")]
async fn an_interrupted_walk_says_where_to_continue() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/now/table/incident"))
        .respond_with(
            |request: &Request| match query(request)["sysparm_query"].as_str() {
                "ORDERBYsys_id" => result(json!([{ "sys_id": "7" }])),
                _ => ResponseTemplate::new(400)
                    .set_body_json(json!({ "error": { "message": "boom" } })),
            },
        )
        .mount(&server)
        .await;
    let run = Cli::basic(&server)
        .run(&["table", "list", "incident", "--all"])
        .await;
    assert_eq!(run.code, 1);
    let error: serde_json::Value = serde_json::from_str(run.stderr.trim()).expect("stderr JSON");
    assert_eq!(
        error["error"]["data"],
        json!({ "resume_from": "7", "records": 1 })
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_reader_that_leaves_ends_the_walk() {
    let server = MockServer::start().await;
    let next = std::sync::atomic::AtomicUsize::new(0);
    Mock::given(method("GET"))
        .and(path("/api/now/table/incident"))
        .respond_with(move |_: &Request| {
            let id = next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            result(json!([{ "sys_id": format!("{id:032}"), "text": "x".repeat(5000) }]))
        })
        .mount(&server)
        .await;
    let mut child = Cli::basic(&server)
        .command(&[
            "table",
            "list",
            "incident",
            "--all",
            "-n",
            "1",
            "--max-records",
            "2000",
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn");
    let status = tokio::task::spawn_blocking(move || {
        let mut out = child.stdout.take().expect("stdout");
        std::io::Read::read_exact(&mut out, &mut [0; 10]).expect("read");
        drop(out);
        child.wait().expect("wait")
    })
    .await
    .expect("join");
    assert_eq!(status.code(), Some(0));
    let requests = server.received_requests().await.expect("requests").len();
    assert!(requests < 500, "{requests}");
}

#[tokio::test(flavor = "multi_thread")]
async fn numbers_and_links_name_records() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/now/table/incident"))
        .respond_with(|request: &Request| {
            let number = query(request)["sysparm_query"]
                .trim_start_matches("number=")
                .to_owned();
            match number.as_str() {
                "INC0010001" => result(json!([{ "sys_id": ID, "number": number }])),
                // The instance ignored the filter.
                "INC0099999" => result(json!([{ "sys_id": ID, "number": "INC0000001" }])),
                _ => result(json!([])),
            }
        })
        .mount(&server)
        .await;
    on(
        &server,
        "GET",
        &format!("/api/now/table/incident/{ID}"),
        result(json!({ "sys_id": ID, "number": "INC0010001" })),
    )
    .await;
    let cli = Cli::basic(&server);
    let link = format!("{}/nav_to.do?uri=incident.do%3Fsys_id%3D{ID}", server.uri());
    for args in [
        vec!["table", "get", "INC0010001"],
        vec!["table", "get", "incident:INC0010001"],
        vec!["table", "get", "incident", "INC0010001"],
        vec!["table", "get", &link],
        vec!["table", "incident", ID],
        vec!["table", "incident:INC0010001"],
    ] {
        let run = cli.run(&[&args[..], &["--agent"]].concat()).await.ok();
        assert_eq!(run.envelope()["data"]["number"], "INC0010001", "{args:?}");
    }
    let run = cli.run(&["table", "get", "INC0010009", "--agent"]).await;
    assert_eq!(
        (run.code, run.envelope()["error"]["code"].clone()),
        (4, json!("not_found"))
    );
    let run = cli.run(&["table", "get", "INC0099999", "--agent"]).await;
    assert_eq!(run.code, 3, "a record that does not match is never used");
    let run = cli.run(&["table", "lst", "incident", "--agent"]).await;
    assert!(
        run.envelope()["error"]["message"]
            .as_str()
            .expect("text")
            .contains("'list'")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn writes_confirm_dry_run_and_respect_read_only() {
    let server = MockServer::start().await;
    on(
        &server,
        "GET",
        &format!("/api/now/table/incident/{ID}"),
        result(json!({ "number": "INC1" })),
    )
    .await;
    on(
        &server,
        "DELETE",
        &format!("/api/now/table/incident/{ID}"),
        ResponseTemplate::new(204),
    )
    .await;
    Mock::given(method("POST"))
        .and(path("/api/now/table/incident"))
        .respond_with(|request: &Request| {
            let body: serde_json::Value = serde_json::from_slice(&request.body).expect("json");
            assert_eq!(
                body,
                json!({ "urgency": 2, "short_description": "Disk full" })
            );
            result(json!({ "sys_id": ID, "number": "INC1" }))
        })
        .expect(1)
        .mount(&server)
        .await;
    let cli = Cli::basic(&server);
    let create = [
        "table",
        "create",
        "incident",
        "-D",
        "-",
        "-F",
        "short_description=Disk full",
        "--agent",
    ];
    let run = cli.run_with(&create, Some("{\"urgency\":2}")).await.ok();
    assert_eq!(run.envelope()["data"]["number"], "INC1");
    let dry = cli
        .run_with(
            &[&create[..], &["--dry-run"]].concat(),
            Some("{\"urgency\":2}"),
        )
        .await
        .ok();
    assert_eq!(dry.envelope()["data"]["request"]["method"], "POST");
    let read_only = cli
        .run_with(&[&create[..], &["--read-only"]].concat(), Some("{}"))
        .await;
    assert_eq!(
        (
            read_only.code,
            read_only.envelope()["error"]["code"].clone()
        ),
        (3, json!("read_only"))
    );
    let delete = ["table", "delete", "incident", ID, "--agent"];
    let run = cli.run(&delete).await;
    assert_eq!(run.envelope()["error"]["code"], "confirmation_required");
    cli.run(&[&delete[..], &["--yes"]].concat()).await.ok();
    let run = cli.run(&["table", "create", "incident", "--agent"]).await;
    assert_eq!(run.envelope()["error"]["code"], "input_required");
}

#[tokio::test(flavor = "multi_thread")]
async fn errors_have_one_shape_everywhere() {
    let server = MockServer::start().await;
    on(&server, "GET", "/api/now/table/incident", ResponseTemplate::new(403).insert_header("x-transaction-id", "tx1").set_body_json(
        json!({ "error": { "message": "Insufficient rights", "detail": "ACL" }, "status": "failure" }),
    )).await;
    on(
        &server,
        "GET",
        "/api/now/table/problem",
        ResponseTemplate::new(200).set_body_string("<html>Your instance is hibernating</html>"),
    )
    .await;
    on(
        &server,
        "GET",
        "/api/now/table/task",
        ResponseTemplate::new(302).insert_header("location", "https://idp.example.com/saml"),
    )
    .await;
    let cli = Cli::basic(&server);
    let run = cli.run(&["table", "list", "incident", "--agent"]).await;
    assert_eq!(run.code, 2);
    let error = &run.envelope()["error"];
    assert_eq!(
        (&error["code"], &error["status"], &error["detail"]),
        (&json!("permission"), &json!(403), &json!("ACL"))
    );
    assert_eq!(error["transaction_id"], "tx1");
    let run = cli.run(&["table", "list", "problem", "--agent"]).await;
    assert_eq!(run.envelope()["error"]["code"], "network");
    let run = cli.run(&["table", "list", "task", "--agent"]).await;
    assert_eq!(run.envelope()["error"]["code"], "auth");
    for args in [
        vec!["table", "list", "--agent"],
        vec!["nonsense", "--agent"],
        vec!["--agent"],
        vec!["table", "list", "a/b", "--agent"],
    ] {
        let run = cli.run(&args).await;
        assert_eq!(
            (run.code, run.envelope()["error"]["code"].clone()),
            (3, json!("invalid")),
            "{args:?}"
        );
    }
    let piped = cli.run(&["table", "list"]).await;
    assert!(
        piped.stdout.is_empty()
            && serde_json::from_str::<serde_json::Value>(piped.stderr.trim()).is_ok()
    );
    let run = Cli::empty()
        .run(&["table", "list", "incident", "--agent"])
        .await;
    assert_eq!(
        (run.code, run.envelope()["error"]["code"].clone()),
        (2, json!("auth"))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn rate_limits_are_waited_out() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/now/table/incident"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "0"))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    on(
        &server,
        "POST",
        "/api/now/table/incident",
        result(json!({ "number": "INC1" })),
    )
    .await;
    Cli::basic(&server)
        .run(&["table", "create", "incident", "-F", "a=1", "--agent"])
        .await
        .ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn raw_and_batch_reach_any_path() {
    let server = MockServer::start().await;
    on(
        &server,
        "GET",
        "/api/x/y",
        ResponseTemplate::new(200).set_body_string("plain text"),
    )
    .await;
    on(&server, "POST", "/api/now/v1/batch", ResponseTemplate::new(200).set_body_json(json!({
        "batch_request_id": "1", "unserviced_requests": [],
        "serviced_requests": [{ "id": "1", "status_code": 200, "execution_time": 3, "body": "eyJyZXN1bHQiOltdfQ==" }],
    }))).await;
    let cli = Cli::basic(&server);
    assert_eq!(
        cli.run(&["raw", "GET", "/api/x/y"]).await.ok().stdout,
        "plain text\n"
    );
    let dry = cli
        .run(&[
            "raw",
            "PUT",
            "/api/x/y",
            "-D",
            "[1]",
            "--dry-run",
            "--agent",
        ])
        .await
        .ok();
    assert_eq!(dry.envelope()["data"]["request"]["body"], json!([1]));
    let run = cli
        .run(&[
            "raw",
            "GET",
            "/api/x/y",
            "-H",
            "Authorization: x",
            "--agent",
        ])
        .await;
    assert_eq!(run.code, 3);
    // Reads in a batch work on a read-only profile.
    let run = cli
        .run(&[
            "batch",
            "GET /api/now/table/incident",
            "--read-only",
            "--agent",
        ])
        .await
        .ok();
    assert_eq!(run.envelope()["data"][0]["body"], json!({ "result": [] }));
}

#[tokio::test(flavor = "multi_thread")]
async fn updates_send_only_the_given_fields() {
    let server = MockServer::start().await;
    for verb in ["PATCH", "PUT"] {
        Mock::given(method(verb))
            .and(path(format!("/api/now/table/incident/{ID}")))
            .respond_with(|request: &Request| {
                let body: serde_json::Value = serde_json::from_slice(&request.body).expect("json");
                assert_eq!(body, json!({ "state": "2", "description": "" }));
                assert_eq!(query(request)["sysparm_input_display_value"], "true");
                result(json!({ "sys_id": ID, "state": "2" }))
            })
            .expect(1)
            .mount(&server)
            .await;
    }
    let cli = Cli::basic(&server);
    for verb in ["update", "replace"] {
        let args = [
            "table",
            verb,
            "incident",
            ID,
            "-F",
            "state=2",
            "-F",
            "description=",
            "--input-display-value",
            "--agent",
        ];
        assert_eq!(cli.run(&args).await.ok().envelope()["data"]["state"], "2");
    }
}

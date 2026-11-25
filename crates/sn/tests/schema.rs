//! Schema, the offline copy, `open`, `ping` and `doctor`.

mod support;

use serde_json::{Value, json};
use support::{Cli, mount_me, on, query, result};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

async fn schema_server() -> MockServer {
    let server = MockServer::start().await;
    on(
        &server,
        "GET",
        "/api/now/doc/table/schema",
        result(json!([
            { "label": "Incident", "value": "incident", "reference": true },
            { "label": "Audit", "value": "sys_audit" },
        ])),
    )
    .await;
    on(
        &server,
        "GET",
        "/api/now/ui/meta/incident",
        result(json!({ "columns": {
            "number": { "label": "Number", "type": "string", "mandatory": false, "read_only": true },
            "state": { "label": "State", "type": "integer", "mandatory": true,
                "choices": [{ "label": "New", "value": "1" }] },
            "caller_id": { "label": "Caller", "type": "reference", "reference": "sys_user" },
        } })),
    )
    .await;
    server
}

fn names(run: support::Run) -> Vec<String> {
    run.envelope()["data"]
        .as_array()
        .expect("rows")
        .iter()
        .map(|row| {
            let name = row.get("name").or_else(|| row.get("value")).unwrap_or(row);
            name.as_str().expect("name").to_owned()
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn tables_and_columns_filter() {
    let server = schema_server().await;
    let cli = Cli::basic(&server);
    for (args, want) in [
        (
            &["schema", "tables", "--filter", "INC"][..],
            &["incident"][..],
        ),
        (&["schema", "tables", "--reference-only"], &["incident"]),
        (
            &["schema", "columns", "incident", "--writable"],
            &["state", "caller_id"],
        ),
        (&["schema", "incident", "--mandatory"], &["state"]),
        (
            &["schema", "columns", "incident", "--choices-only"],
            &["state"],
        ),
        (
            &["schema", "columns", "incident", "--references-only"],
            &["caller_id"],
        ),
        (
            &["schema", "columns", "incident", "--type", "STRING"],
            &["number"],
        ),
        (
            &["schema", "columns", "incident", "--filter", "call"],
            &["caller_id"],
        ),
        (&["schema", "choices", "incident", "state"], &["1"]),
        (&["choices", "incident", "state", "--refresh"], &["1"]),
    ] {
        let run = cli.run(&[args, &["--agent"]].concat()).await.ok();
        assert_eq!(names(run), want, "{args:?}");
    }
    let run = cli
        .run(&["choices", "incident", "number", "--agent"])
        .await
        .ok();
    assert_eq!(run.envelope()["data"], json!([]));
    let run = cli.run(&["choices", "incident", "stat", "--agent"]).await;
    assert_eq!(
        run.envelope()["error"]["input"]["choices"][0]["value"],
        "caller_id"
    );
    let run = cli
        .run(&["schema", "columns", "incident", "-o", "table"])
        .await
        .ok();
    assert!(run.stdout.starts_with("NAME"), "{}", run.stdout);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_last_read_is_there_offline() {
    let server = schema_server().await;
    let cli = Cli::basic(&server);
    let args = ["schema", "columns", "incident", "--offline", "--agent"];
    let run = cli.run(&args).await;
    assert!(
        run.code == 4 && run.stdout.contains("cache refresh"),
        "{}",
        run.stdout
    );
    cli.run(&["schema", "columns", "incident"]).await.ok();
    let before = server.received_requests().await.expect("requests").len();
    assert_eq!(names(cli.run(&args).await.ok()).len(), 3);
    let run = cli
        .run(&["choices", "incident", "state", "--offline", "--agent"])
        .await
        .ok();
    assert_eq!(run.envelope()["data"][0]["label"], "New");
    assert_eq!(
        server.received_requests().await.expect("requests").len(),
        before
    );
}

fn group(fields: &[(&str, &str)], count: u64) -> Value {
    let fields: Vec<Value> = fields
        .iter()
        .map(|(f, v)| json!({ "field": f, "value": v }))
        .collect();
    json!({ "groupby_fields": fields, "stats": { "count": count.to_string() } })
}

const TERMS: &str = "elementISNOTEMPTY^nameNOT LIKEvar__m_";

/// A dictionary of two tables with `count` rows each; `answer` gives the
/// rows of a range.
async fn dictionary(
    count: u64,
    answer: impl Fn(&str) -> ResponseTemplate + Send + Sync + 'static,
) -> MockServer {
    let server = MockServer::start().await;
    on(
        &server,
        "GET",
        "/api/now/stats/sys_db_object",
        result(json!([
            group(&[("name", "task"), ("super_class.name", "")], 1),
            group(&[("name", "incident"), ("super_class.name", "task")], 1),
        ])),
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/api/now/stats/sys_dictionary"))
        .respond_with(move |request: &Request| {
            let q = query(request);
            match q["sysparm_group_by"].as_str() {
                "name" => {
                    assert_eq!(q["sysparm_order_by"], "name");
                    result(json!([
                        group(&[("name", "incident")], count),
                        group(&[("name", "task")], count),
                    ]))
                }
                _ => answer(&q["sysparm_query"]),
            }
        })
        .mount(&server)
        .await;
    server
}

fn column(table: &str, name: &str) -> Value {
    group(&[("name", table), ("element", name)], 1)
}

#[tokio::test(flavor = "multi_thread")]
async fn the_cache_answers_without_the_network() {
    let server = dictionary(9000, |filter| {
        if filter == format!("{TERMS}^name<task") {
            return result(json!([column("incident", "caller_id")]));
        }
        assert_eq!(filter, format!("{TERMS}^name>=task"));
        result(json!([column("task", "number"), column("task", "state")]))
    })
    .await;
    let cli = Cli::basic(&server);
    let run = cli.run(&["cache", "status", "--agent"]).await.ok();
    assert_eq!(run.envelope()["data"]["exists"], false);
    let run = cli.run(&["cache", "refresh", "--agent"]).await.ok();
    let data = run.envelope()["data"].clone();
    assert_eq!(
        (&data["tables"], &data["columns"], &data["requests"]),
        (&json!(2), &json!(3), &json!(4))
    );
    let before = server.received_requests().await.expect("requests").len();
    let run = cli
        .run(&["cache", "columns", "incident", "--agent"])
        .await
        .ok();
    assert_eq!(
        names(run),
        ["caller_id", "number", "state"],
        "inherited too"
    );
    let run = cli.run(&["cache", "tables", "inc"]).await.ok();
    assert_eq!(run.json(), json!(["incident"]));
    let run = cli
        .run(&[
            "schema",
            "tables",
            "--offline",
            "--filter",
            "tas",
            "--agent",
        ])
        .await
        .ok();
    assert_eq!(names(run), ["task"]);
    assert_eq!(cli.run(&["cache", "columns", "none"]).await.code, 4);
    assert_eq!(
        server.received_requests().await.expect("requests").len(),
        before
    );
    let path = data["path"].as_str().expect("path");
    std::fs::write(path, "{").expect("write");
    let run = cli.run(&["cache", "tables", "--agent"]).await;
    assert!(
        run.code == 3 && run.stdout.contains("damaged"),
        "{}",
        run.stdout
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_ignored_range_makes_no_cache() {
    let everything: Vec<Value> = (0..9001)
        .map(|i| column("other", &format!("c{i}")))
        .collect();
    let server = dictionary(9000, move |_| result(json!(everything))).await;
    let cli = Cli::basic(&server);
    let run = cli.run(&["cache", "refresh", "--agent"]).await;
    assert!(
        run.code == 1 && run.stdout.contains("ignored the range"),
        "{}",
        run.stdout
    );
    let run = cli.run(&["cache", "status", "--agent"]).await.ok();
    assert_eq!(run.envelope()["data"]["exists"], false);
}

#[tokio::test(flavor = "multi_thread")]
async fn cut_answers_are_split_and_users_without_the_dictionary_get_names() {
    let server = dictionary(4000, |filter| match filter.contains("name<task") {
        _ if filter == TERMS => ResponseTemplate::new(200).set_body_string("{\"result\":[{"),
        true => result(json!([column("incident", "caller_id")])),
        false => result(json!([column("task", "number")])),
    })
    .await;
    let run = Cli::basic(&server)
        .run(&["cache", "refresh", "--agent"])
        .await
        .ok();
    let data = run.envelope()["data"].clone();
    assert_eq!(
        (&data["columns"], &data["requests"]),
        (&json!(2), &json!(5)),
        "one range, then its halves"
    );

    let server = MockServer::start().await;
    on(
        &server,
        "GET",
        "/api/now/stats/sys_db_object",
        result(json!([group(
            &[("name", "task"), ("super_class.name", "")],
            1
        )])),
    )
    .await;
    on(
        &server,
        "GET",
        "/api/now/stats/sys_dictionary",
        ResponseTemplate::new(403).set_body_json(json!({ "error": { "message": "no" } })),
    )
    .await;
    let cli = Cli::basic(&server);
    let run = cli.run(&["cache", "refresh", "--agent"]).await.ok();
    assert_eq!(run.envelope()["data"]["columns_indexed"], false);
    assert!(run.stderr.contains("sys_dictionary"));
    let run = cli.run(&["cache", "columns", "task", "--agent"]).await;
    assert!(
        run.code == 3 && run.stdout.contains("administrator"),
        "{}",
        run.stdout
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn open_builds_links_and_never_opens_for_agents() {
    let server = MockServer::start().await;
    on(
        &server,
        "GET",
        "/api/now/table/incident",
        result(json!([{ "sys_id": "9d385017c611228701d22104cc95c371", "number": "INC0010001" }])),
    )
    .await;
    let cli = Cli::basic(&server);
    let record = "uri=%2Fincident.do%3Fsys_id%3D9d385017c611228701d22104cc95c371";
    for (args, tail) in [
        (&["incident"][..], "uri=%2Fincident_list.do"),
        (
            &["incident", "-q", "active=true^name=a&b"],
            "uri=%2Fincident_list.do%3Fsysparm_query%3Dactive%253Dtrue%255Ename%253Da%2526b",
        ),
        (&["INC0010001"], record),
        (&["incident:INC0010001"], record),
        (&["incident", "9d385017c611228701d22104cc95c371"], record),
    ] {
        let run = cli
            .run(&[&["open"], args, &["--print-url"]].concat())
            .await
            .ok();
        assert_eq!(
            run.stdout.trim(),
            format!("{}/nav_to.do?{tail}", server.uri()),
            "{args:?}"
        );
    }
    let run = cli.run(&["open", "incident", "--agent"]).await.ok();
    assert_eq!(run.envelope()["data"]["opened"], false);
    // SN_BROWSER=none: a headless session.
    let run = cli.run(&["open", "incident"]).await.ok();
    assert_eq!(run.json()["opened"], false);
    for bad in [
        &["open", "a/b"][..],
        &["open", "incident:INC1", "-q", "a=1"],
        &["open", "incident", "x", "-q", "a=1"],
    ] {
        assert_eq!(cli.run(bad).await.code, 3, "{bad:?}");
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn open_hands_the_whole_link_to_the_browser() {
    let server = MockServer::start().await;
    let mut cli = Cli::basic(&server);
    let browser = support::fake_browser(cli.dir.path());
    cli.env.push(("SN_BROWSER".into(), browser.clone()));
    let run = cli.run(&["open", "incident", "-q", "a=1^b=2"]).await.ok();
    assert_eq!(run.json()["opened"], true);
    let log = std::fs::read_to_string(format!("{browser}.log")).expect("log");
    assert_eq!(
        log.trim(),
        format!("1:{}", run.json()["url"].as_str().expect("url"))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn ping_names_the_session_and_hides_its_token() {
    let server = MockServer::start().await;
    on(
        &server,
        "GET",
        "/api/now/sg/impersonation/session",
        ResponseTemplate::new(200).set_body_json(json!({
            "CurrentUser": "abel", "OriginalUser": "admin", "SessionToken": "SECRET", "admin": false,
        })),
    )
    .await;
    on(
        &server,
        "GET",
        "/api/now/table/sys_properties",
        result(json!([{ "name": "glide.buildtag", "value": "glide-zurich" }])),
    )
    .await;
    let cli = Cli::basic(&server);
    let run = cli.run(&["ping", "--agent", "-vv"]).await.ok();
    let data = run.envelope()["data"].clone();
    assert_eq!(
        (
            &data["username"],
            &data["impersonating"],
            &data["build_tag"]
        ),
        (&json!("abel"), &json!(true), &json!("glide-zurich"))
    );
    assert!(!run.stdout.contains("SECRET") && !run.stderr.contains("SECRET"));
    let run = cli.run(&["ping", "-o", "table"]).await.ok();
    assert!(
        run.stdout
            .contains("you are abel (signed in as admin), build glide-zurich"),
        "{}",
        run.stdout
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn ping_falls_back_only_when_the_endpoint_is_missing() {
    let server = MockServer::start().await;
    mount_me(&server).await;
    let run = Cli::basic(&server).run(&["ping", "--agent"]).await.ok();
    let data = run.envelope()["data"].clone();
    assert_eq!(
        (
            &data["username"],
            &data["identity_source"],
            &data["build_tag"]
        ),
        (&json!("ada"), &json!("sys_user"), &Value::Null)
    );
    on(
        &server,
        "GET",
        "/api/now/sg/impersonation/session",
        ResponseTemplate::new(401),
    )
    .await;
    let run = Cli::basic(&server).run(&["ping"]).await;
    assert_eq!(run.code, 2, "{}", run.stdout);
}

async fn healthy(graphql: Value) -> MockServer {
    let server = MockServer::start().await;
    mount_me(&server).await;
    on(&server, "GET", "/api/now/table/incident", result(json!([]))).await;
    on(
        &server,
        "POST",
        "/api/now/graphql",
        ResponseTemplate::new(200).set_body_json(graphql),
    )
    .await;
    server
}

fn check<'a>(report: &'a Value, name: &str) -> &'a Value {
    report["checks"]
        .as_array()
        .expect("checks")
        .iter()
        .find(|c| c["name"] == name)
        .expect("check")
}

#[tokio::test(flavor = "multi_thread")]
async fn doctor_passes_with_everything_in_place() {
    let server = healthy(json!({ "data": {
        "now": { "sessionUser": { "admin": [], "held": ["itil"] } },
        "GlideRecord_Query": { "role0": { "_rowCount": 1 } },
        "snWorkflowStudio": { "workflowStudio": { "canary": true, "plugin0": true } },
        "snDecisionTable": { "sysProperties": { "build": "tag", "property0": "true" } },
    } }))
    .await;
    let cli = Cli::basic(&server);
    let run = cli
        .run(&[
            "doctor",
            "--need-role",
            "itil",
            "--need-plugin",
            "com.snc.incident",
            "--need-property",
            "glide.x=true",
            "--agent",
        ])
        .await
        .ok();
    let report = run.envelope()["data"].clone();
    assert_eq!(
        (&report["ok"], &report["build_tag"]),
        (&json!(true), &json!("tag"))
    );
    for name in [
        "configuration",
        "profile",
        "credential",
        "network",
        "identity",
        "table_api",
        "itil",
        "com.snc.incident",
        "glide.x",
    ] {
        assert_eq!(check(&report, name)["status"], "pass", "{name}");
    }
    let graphql: Vec<_> = server
        .received_requests()
        .await
        .expect("requests")
        .into_iter()
        .filter(|r| r.url.path() == "/api/now/graphql")
        .collect();
    assert_eq!(graphql.len(), 1, "one document for all needs");
    let body: Value = serde_json::from_slice(&graphql[0].body).expect("json");
    assert_eq!(body["variables"]["role0"], "name=itil");
    let run = cli.run(&["doctor", "-o", "table"]).await.ok();
    assert!(
        run.stdout.contains("identity          ada"),
        "{}",
        run.stdout
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn doctor_reports_what_failed_in_one_error() {
    // Without the missing field, the second document is answered.
    let server = MockServer::start().await;
    mount_me(&server).await;
    on(&server, "GET", "/api/now/table/incident", result(json!([]))).await;
    Mock::given(method("POST"))
        .and(path("/api/now/graphql"))
        .respond_with(|request: &Request| {
            let body: Value = serde_json::from_slice(&request.body).expect("json");
            let answer = match body["query"].as_str().expect("query").contains("snWorkflowStudio") {
                true => json!({ "errors": [{
                    "message": "Validation error (FieldUndefined@[snWorkflowStudio]) : Field 'snWorkflowStudio' in type 'QueryType' is undefined",
                }] }),
                false => json!({ "data": { "now": { "sessionUser": { "admin": ["admin"] } } } }),
            };
            ResponseTemplate::new(200).set_body_json(answer)
        })
        .mount(&server)
        .await;
    let cli = Cli::basic(&server);
    let args = ["doctor", "--need-plugin", "com.x"];
    let run = cli.run(&[&args[..], &["--agent"]].concat()).await;
    let error = run.envelope()["error"].clone();
    assert_eq!((run.code, &error["code"]), (1, &json!("check_failed")));
    assert_eq!(check(&error["data"], "com.x")["status"], "unavailable");
    let run = cli.run(&[&args[..], &["-o", "table"]].concat()).await;
    assert!(
        run.code == 1 && run.stdout.contains("unknown  plugin com.x"),
        "{}",
        run.stdout
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn doctor_works_offline_and_without_a_profile() {
    let server = MockServer::start().await;
    let cli = Cli::basic(&server);
    let run = cli.run(&["doctor", "--offline", "--agent"]).await.ok();
    assert_eq!(
        check(&run.envelope()["data"], "identity")["status"],
        "skipped"
    );
    support::assert_untouched(&server).await;
    let run = Cli::empty().run(&["doctor", "--agent"]).await;
    let error = run.envelope()["error"].clone();
    assert_eq!(run.code, 1);
    assert_eq!(check(&error["data"], "profile")["status"], "fail");
    let run = cli.run(&["doctor", "--need-role", "a^b", "--agent"]).await;
    assert_eq!(run.code, 3);
    // Nothing listens there.
    let mut dead = Cli::empty();
    dead.env = vec![
        ("SN_INSTANCE".into(), "http://127.0.0.1:9".into()),
        ("SN_USERNAME".into(), "a".into()),
        ("SN_PASSWORD".into(), "b".into()),
    ];
    let run = dead.run(&["doctor", "--agent"]).await;
    let error = run.envelope()["error"].clone();
    assert_eq!(check(&error["data"], "network")["status"], "fail");
    assert_eq!(check(&error["data"], "identity")["status"], "unavailable");
}

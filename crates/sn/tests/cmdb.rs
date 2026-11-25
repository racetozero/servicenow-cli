//! `cmdb`, `identify`, `import` and `scores`.

mod support;

use serde_json::{Value, json};
use support::{Cli, on, query, result};
use wiremock::matchers::{body_json, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const ID: &str = "9d385017c611228701d22104cc95c371";
const RELATION: &str = "11111111111111111111111111111111";
const CLASS: &str = "/api/now/cmdb/instance/cmdb_ci_server";

#[tokio::test(flavor = "multi_thread")]
async fn items_are_listed_and_read_with_an_implied_verb() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(CLASS))
        .respond_with(|request: &Request| {
            assert_eq!(query(request)["sysparm_query"], "nameLIKEweb");
            result(json!([{ "name": "web1", "sys_id": ID }])).insert_header("x-total-count", "2")
        })
        .mount(&server)
        .await;
    on(
        &server,
        "GET",
        &format!("{CLASS}/{ID}"),
        result(json!({
            "attributes": { "name": "web1", "sys_id": ID },
            "outbound_relations": [{ "type": { "display_value": "Runs on::Runs" }, "target": { "display_value": "host1" } }],
            "inbound_relations": [],
        })),
    )
    .await;
    on(
        &server,
        "GET",
        "/api/now/cmdb/meta/cmdb_ci_server",
        result(json!({ "name": "cmdb_ci_server" })),
    )
    .await;
    let cli = Cli::basic(&server);
    let run = cli
        .run(&[
            "cmdb",
            "cmdb_ci_server",
            "-q",
            "nameLIKEweb",
            "-n",
            "1",
            "--agent",
        ])
        .await
        .ok();
    assert_eq!(run.envelope()["command"], "cmdb list");
    assert_eq!(run.envelope()["meta"]["next_offset"], 1);
    let run = cli
        .run(&["cmdb", "cmdb_ci_server", ID, "-o", "table"])
        .await
        .ok();
    assert!(
        run.stdout.contains("-> host1 (Runs on::Runs)"),
        "{}",
        run.stdout
    );
    let run = cli
        .run(&["cmdb", "get", &format!("cmdb_ci_server:{ID}")])
        .await
        .ok();
    assert_eq!(run.json()["attributes"]["name"], "web1");
    cli.run(&["cmdb", "meta", "cmdb_ci_server"]).await.ok();
    assert_eq!(cli.run(&["cmdb", "meta", "a/b"]).await.code, 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn writes_go_out_as_an_envelope() {
    let server = MockServer::start().await;
    let item = result(json!({ "attributes": { "name": "web1", "sys_id": ID } }));
    Mock::given(method("POST"))
        .and(path(CLASS))
        .and(body_json(
            json!({ "attributes": { "name": "web1" }, "source": "Manual Entry" }),
        ))
        .respond_with(item.clone())
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path(format!("{CLASS}/{ID}")))
        .and(body_json(
            json!({ "attributes": { "cpu_count": "8" }, "source": "Tool" }),
        ))
        .respond_with(item)
        .expect(1)
        .mount(&server)
        .await;
    let cli = Cli::basic(&server);
    let run = cli
        .run(&[
            "cmdb",
            "create",
            "cmdb_ci_server",
            "-F",
            "name=web1",
            "-o",
            "table",
        ])
        .await
        .ok();
    assert_eq!(run.stdout, format!("Created cmdb_ci_server web1 ({ID})\n"));
    cli.run(&[
        "cmdb",
        "update",
        "cmdb_ci_server",
        ID,
        "--data",
        r#"{"cpu_count":8}"#,
        "--source",
        "Tool",
    ])
    .await
    .ok();
    let run = cli
        .run(&["cmdb", "update", "cmdb_ci_server", ID, "-F", "source=x"])
        .await;
    assert_eq!(run.code, 3);
    assert!(run.stderr.contains("--source"), "{}", run.stderr);
}

#[tokio::test(flavor = "multi_thread")]
async fn relations_are_added_and_deleted_in_both_forms() {
    let server = MockServer::start().await;
    let at = format!("{CLASS}/{ID}/relation");
    Mock::given(method("POST"))
        .and(path(at.clone()))
        .and(body_json(
            json!({ "source": "x", "outbound_relations": [] }),
        ))
        .respond_with(result(json!({})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path(format!("{at}/{RELATION}")))
        .respond_with(ResponseTemplate::new(204))
        .expect(2)
        .mount(&server)
        .await;
    let cli = Cli::basic(&server);
    let combined = format!("cmdb_ci_server:{ID}");
    let data = r#"{"source":"x","outbound_relations":[]}"#;
    cli.run(&["cmdb", "relation", "add", &combined, "--data", data])
        .await
        .ok();
    let delete = ["cmdb", "relation", "delete"];
    for words in [
        vec![combined.as_str(), RELATION],
        vec!["cmdb_ci_server", ID, RELATION],
    ] {
        let args = [&delete[..], &words[..], &["--agent"]].concat();
        let run = cli.run(&args).await;
        assert_eq!(run.envelope()["error"]["code"], "confirmation_required");
        cli.run(&[&args[..], &["--yes"]].concat()).await.ok();
    }
    for words in [
        vec!["cmdb_ci_server", ID],
        vec![combined.as_str(), ID, RELATION],
        vec![combined.as_str()],
        vec![combined.as_str(), "not-an-id"],
    ] {
        let run = cli
            .run(&[&delete[..], &words[..], &["--yes"]].concat())
            .await;
        assert_eq!(run.code, 3, "{words:?}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn identification_queries_read_and_the_others_write() {
    let server = MockServer::start().await;
    let at = "/api/now/identifyreconcile";
    for path in ["", "/query", "/enhanced", "/queryEnhanced"] {
        on(
            &server,
            "POST",
            &format!("{at}{path}"),
            result(json!({ "items": [] })),
        )
        .await;
    }
    let cli = Cli::basic(&server);
    let body = ["--data", r#"{"items":[]}"#, "--data-source", "Tool"];
    let options = ["--options", "partial_payloads:true"];
    for (verb, path, options, reads) in [
        ("create-update", "", &[][..], false),
        ("query", "/query", &[], true),
        ("create-update-enhanced", "/enhanced", &options, false),
        ("query-enhanced", "/queryEnhanced", &options, true),
    ] {
        let args = [&["identify", verb], &body[..], options].concat();
        cli.run(&args).await.ok();
        let requests = server.received_requests().await.expect("requests");
        let last = requests.last().expect("request");
        assert_eq!(last.url.path(), format!("{at}{path}"));
        assert_eq!(query(last)["sysparm_data_source"], "Tool");
        assert_eq!(query(last).contains_key("options"), !options.is_empty());
        let run = cli.run(&[&args[..], &["--read-only"]].concat()).await;
        assert_eq!(run.code == 0, reads, "{verb}");
    }
    assert_eq!(
        cli.run(&["identify", "query"]).await.code,
        3,
        "a payload is required"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn imports_keep_the_names_of_the_import_set() {
    let server = MockServer::start().await;
    let at = "/api/now/import/u_staging";
    let answer = json!({
        "import_set": "ISET1", "staging_table": "u_staging",
        "result": [{ "status": "inserted", "table": "sys_user", "sys_id": ID }],
    });
    Mock::given(method("POST"))
        .and(path(at))
        .and(body_json(json!({ "u_name": "Ada" })))
        .respond_with(ResponseTemplate::new(201).set_body_json(&answer))
        .expect(2)
        .mount(&server)
        .await;
    on(
        &server,
        "GET",
        &format!("{at}/{ID}"),
        ResponseTemplate::new(200).set_body_json(&answer),
    )
    .await;
    Mock::given(method("POST"))
        .and(path(format!("{at}/insertMultiple")))
        .and(body_json(json!({ "records": [{ "a": 1 }] })))
        .respond_with(
            ResponseTemplate::new(201)
                .set_body_json(json!({ "import_set_id": ID, "multi_import_set_id": RELATION })),
        )
        .expect(2)
        .mount(&server)
        .await;
    let cli = Cli::basic(&server);
    let create = ["import", "create", "u_staging", "-F", "u_name=Ada"];
    let run = cli.run(&[&create[..], &["--agent"]].concat()).await.ok();
    assert_eq!(
        run.envelope()["meta"],
        json!({ "import_set": "ISET1", "staging_table": "u_staging", "count": 1 })
    );
    let run = cli.run(&[&create[..], &["--raw"]].concat()).await.ok();
    assert_eq!(run.json(), answer);
    let run = cli.run(&["import", "get", "u_staging", ID]).await.ok();
    assert_eq!(run.json()[0]["status"], "inserted");
    for data in [r#"[{"a":1}]"#, r#"{"records":[{"a":1}]}"#] {
        let run = cli
            .run(&["import", "bulk", "u_staging", "--data", data])
            .await
            .ok();
        assert_eq!(run.json()["import_set_id"], ID);
    }
    for data in ["1", "{}", "["] {
        assert_eq!(
            cli.run(&["import", "bulk", "u_staging", "--data", data])
                .await
                .code,
            3
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_row_that_could_not_be_transformed_is_an_error() {
    let server = MockServer::start().await;
    on(
        &server,
        "POST",
        "/api/now/import/u_staging",
        ResponseTemplate::new(201).set_body_json(json!({
            "import_set": "ISET1", "result": [{ "status": "error", "error_message": "no map" }],
        })),
    )
    .await;
    let run = Cli::basic(&server)
        .run(&["import", "create", "u_staging", "-F", "a=1", "--agent"])
        .await;
    assert_eq!(run.code, 1);
    let error = &run.envelope()["error"];
    assert!(error["message"].to_string().contains("no map"));
    assert_eq!(error["payload"]["import_set"], "ISET1");
}

#[tokio::test(flavor = "multi_thread")]
async fn scorecards_take_every_parameter() {
    let server = MockServer::start().await;
    let at = "/api/now/pa/scorecards";
    on(
        &server,
        "GET",
        at,
        result(json!([{ "name": "Open incidents", "value": "3" }])),
    )
    .await;
    let cli = Cli::basic(&server);
    let run = cli
        .run(&[
            "scores",
            "list",
            "--uuid",
            "u",
            "--favorites",
            "--sort-by",
            "indicator_group",
            "--sort-dir",
            "DESC",
            "--per-page",
            "1",
            "--page",
            "2",
            "--include-scores",
            "--from",
            "2026-01-01",
            "--limit",
            "-1",
            "--display-value",
            "all",
            "--include-forecast-scores",
            "--agent",
        ])
        .await
        .ok();
    assert_eq!(run.envelope()["meta"]["next_page"], 3);
    let requests = server.received_requests().await.expect("requests");
    let sent: Vec<(String, String)> = query(&requests[0]).into_iter().collect();
    for (name, value) in [
        ("sysparm_uuid", "u"),
        ("sysparm_favorites", "true"),
        ("sysparm_sortby", "INDICATOR_GROUP"),
        ("sysparm_sortdir", "DESC"),
        ("sysparm_per_page", "1"),
        ("sysparm_page", "2"),
        ("sysparm_include_scores", "true"),
        ("sysparm_from", "2026-01-01"),
        ("sysparm_limit", "-1"),
        ("sysparm_display_value", "all"),
        ("sysparm_include_forecast_scores", "true"),
    ] {
        assert!(
            sent.contains(&(name.into(), value.into())),
            "{name}: {sent:?}"
        );
    }
    assert_eq!(sent.len(), 11, "{sent:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn favorites_are_set_and_removed() {
    let server = MockServer::start().await;
    let at = "/api/now/pa/scorecards";
    on(&server, "POST", at, result(json!({ "uuid": "u" }))).await;
    on(&server, "DELETE", at, ResponseTemplate::new(204)).await;
    let cli = Cli::basic(&server);
    cli.run(&["scores", "favorite", "u"]).await.ok();
    let run = cli.run(&["scores", "unfavorite", "u"]).await.ok();
    assert_eq!(run.json(), json!({ "uuid": "u", "favorite": false }));
    let requests = server.received_requests().await.expect("requests");
    assert!(requests.iter().all(|r| query(r)["sysparm_uuid"] == "u"));
    let empty: Value = serde_json::from_slice(&requests[0].body).expect("body");
    assert_eq!(empty, json!({}));
}

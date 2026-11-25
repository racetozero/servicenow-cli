//! Code search, API search, and the Application Manager.

mod support;

use serde_json::{Value, json};
use support::{Cli, on, query, result};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const MISSING: &str = "Requested URI does not represent any resource";

fn missing() -> ResponseTemplate {
    ResponseTemplate::new(400).set_body_json(json!({ "error": { "message": MISSING } }))
}

async fn received(server: &MockServer) -> Vec<String> {
    let requests = server.received_requests().await.expect("requests");
    requests
        .iter()
        .map(|r| {
            let query = r.url.query().map(|q| format!("?{q}")).unwrap_or_default();
            format!("{} {}{query}", r.method, r.url.path())
        })
        .collect()
}

fn code_hit() -> Value {
    json!({ "recordType": "sys_script_include", "hits": [{
        "className": "sys_script_include", "sysId": "1", "name": "Util",
        "matches": [{ "field": "script", "count": 2.0, "lineMatches": [{ "line": 3.0, "context": "GlideRecord" }] }],
    }] })
}

#[tokio::test(flavor = "multi_thread")]
async fn codesearch_asks_for_every_scope_and_flattens() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/sn_codesearch/code_search/search"))
        .respond_with(|request: &Request| {
            let q = query(request);
            assert_eq!(
                (&*q["term"], &*q["search_all_scopes"], &*q["limit"]),
                ("gliderecord", "true", "500")
            );
            assert_eq!(q["search_group"], "sn_codesearch.Default Search Group");
            result(json!([code_hit()]))
        })
        .mount(&server)
        .await;
    let cli = Cli::basic(&server);
    let run = cli
        .run(&["codesearch", "gliderecord", "--agent"])
        .await
        .ok();
    let row = run.envelope()["data"][0].clone();
    assert_eq!(
        (&row["table"], &row["count"], &row["lines"][0]["line"]),
        (&json!("sys_script_include"), &json!(2), &json!(3))
    );
    let run = cli
        .run(&["codesearch", "gliderecord", "-o", "table"])
        .await
        .ok();
    assert!(run.stdout.contains("3: GlideRecord"), "{}", run.stdout);
    for bad in [
        &["codesearch", "a^b"][..],
        &["codesearch", " "],
        &["codesearch", "a", "--scope", "x^y"],
        &["codesearch", "a", "--limit", "0"],
    ] {
        assert_eq!(cli.run(bad).await.code, 3, "{bad:?}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn codesearch_checks_tables_and_scopes() {
    let server = MockServer::start().await;
    on(
        &server,
        "GET",
        "/api/sn_codesearch/code_search/tables",
        result(json!([{ "name": "sys_script" }, { "name": "sys_ui_page" }])),
    )
    .await;
    on(
        &server,
        "GET",
        "/api/sn_codesearch/code_search/search",
        result(json!({ "recordType": "sys_script", "hits": [] })),
    )
    .await;
    Mock::given(path("/api/now/table/sys_scope"))
        .respond_with(|request: &Request| {
            // A dropped filter answers with another scope.
            match query(request)["sysparm_query"].as_str() {
                "scope=global^ORsys_id=global" => {
                    result(json!([{ "sys_id": "global", "scope": "global" }]))
                }
                _ => result(json!([{ "sys_id": "x", "scope": "other" }])),
            }
        })
        .mount(&server)
        .await;
    let cli = Cli::basic(&server);
    let run = cli
        .run(&["codesearch", "a", "--table", "incident", "--agent"])
        .await;
    assert_eq!(
        (
            run.code,
            run.envelope()["error"]["input"]["choices"][0]["value"].clone()
        ),
        (3, json!("sys_script"))
    );
    let args = [
        "codesearch",
        "a",
        "--table",
        "sys_script",
        "--agent",
        "--scope",
    ];
    let run = cli.run(&[&args[..], &["global"]].concat()).await.ok();
    assert_eq!(run.envelope()["data"], json!([]));
    assert_eq!(cli.run(&[&args[..], &["gloal"]].concat()).await.code, 3);
    let bare = MockServer::start().await;
    on(
        &bare,
        "GET",
        "/api/sn_codesearch/code_search/search",
        missing(),
    )
    .await;
    let run = Cli::basic(&bare).run(&["codesearch", "a", "--agent"]).await;
    assert!(
        run.code == 1 && run.stdout.contains("no Code Search API"),
        "{}",
        run.stdout
    );
}

fn catalog() -> Value {
    let resource =
        |verb: &str, route: &str| json!({ "httpMethod": verb, "route": route, "description": "" });
    json!({
        "now": {
            "Table API": { "apiName": "now/table", "description": "Records", "versions": {
                "v1": { "resources": [resource("GET", "/now/v1/table/{tableName}")] },
                "v2": { "resources": [resource("GET", "/now/v2/table/{tableName}"), resource("POST", "/now/v2/table/{tableName}")] },
            } },
            "Attachment API": { "apiName": "now/attachment", "description": "Files", "versions": {
                "v1": { "resources": [resource("GET", "/now/v1/attachment")] },
            } },
        },
        "sn_chg_rest": { "Change": { "apiName": "sn_chg_rest/change", "scripted": true, "versions": {
            "v1": { "resources": [resource("GET", "/sn_chg_rest/change")] },
        } } },
    })
}

async fn doc_server() -> MockServer {
    let server = MockServer::start().await;
    on(&server, "GET", "/api/now/doc", result(catalog())).await;
    on(
        &server,
        "GET",
        "/api/now/doc/namespaces",
        result(json!(["now", "sn_chg_rest", "empty"])),
    )
    .await;
    Mock::given(path("/api/now/doc/services"))
        .respond_with(|request: &Request| {
            let name = query(request)["namespace"].clone();
            let catalog = catalog();
            result(match catalog.get(&name) {
                Some(apis) => json!({ name: apis }),
                None => json!({}),
            })
        })
        .mount(&server)
        .await;
    Mock::given(path("/api/now/doc/oas_3"))
        .respond_with(|request: &Request| {
            assert_eq!(
                request.headers["accept"], "*/*",
                "JSON is answered with 406"
            );
            let q = query(request);
            assert_eq!((&*q["namespace"], &*q["name"]), ("now", "Table API"));
            match (q["format"].as_str(), q.get("version").map(String::as_str)) {
                (_, Some("v99")) => ResponseTemplate::new(404).set_body_json(json!({ "error": {
                    "message": "Not found", "detail": "Version v99 not found for now/Table API",
                } })),
                ("yaml", _) => ResponseTemplate::new(200).set_body_string("openapi: 3.0.1\n"),
                _ => ResponseTemplate::new(200).set_body_json(json!({ "openapi": "3.0.1" })),
            }
        })
        .mount(&server)
        .await;
    server
}

#[tokio::test(flavor = "multi_thread")]
async fn apisearch_finds_endpoints_with_and_without_the_verb() {
    let server = doc_server().await;
    let cli = Cli::basic(&server);
    for args in [
        &["apisearch", "TABLE", "--method", "get", "--agent"][..],
        &["apisearch", "search", "table", "-m", "GET", "--agent"],
    ] {
        let envelope = cli.run(args).await.ok().envelope();
        assert_eq!(envelope["command"], "apisearch search");
        let routes: Vec<&str> = envelope["data"]
            .as_array()
            .expect("rows")
            .iter()
            .map(|r| r["route"].as_str().expect("route"))
            .collect();
        assert_eq!(
            routes,
            ["/now/v1/table/{tableName}", "/now/v2/table/{tableName}"]
        );
    }
    // A search text can look like a verb.
    let run = cli.run(&["apisearch", "lists", "--agent"]).await.ok();
    assert_eq!(run.envelope()["data"], json!([]));
    let run = cli.run(&["apisearch", "list", "--agent"]).await.ok();
    assert_eq!(
        run.envelope()["data"][0],
        json!({ "namespace": "now", "apis": 2, "endpoints": 4 })
    );
    let run = cli
        .run(&["apisearch", "list", "--namespace", "now", "--agent"])
        .await
        .ok();
    assert_eq!(run.envelope()["data"][0]["versions"], json!(["v1", "v2"]));
}

#[tokio::test(flavor = "multi_thread")]
async fn apisearch_explains_unknown_namespaces_and_versions() {
    let server = doc_server().await;
    let cli = Cli::basic(&server);
    let run = cli
        .run(&["apisearch", "list", "--namespace", "nwo", "--agent"])
        .await;
    assert!(
        run.code == 3 && run.stdout.contains("did you mean now?"),
        "{}",
        run.stdout
    );
    let run = cli
        .run(&["apisearch", "x", "--namespace", "empty", "--agent"])
        .await;
    assert!(run.stdout.contains("has no REST API"), "{}", run.stdout);
    let run = cli
        .run(&[
            "apisearch",
            "spec",
            "table api",
            "--version",
            "v99",
            "--agent",
        ])
        .await;
    assert!(
        run.code == 4 && run.stdout.contains("Version v99 not found"),
        "{}",
        run.stdout
    );
    let old = MockServer::start().await;
    let run = Cli::basic(&old)
        .run(&["apisearch", "list", "--agent"])
        .await;
    assert!(run.stdout.contains("no REST API catalog"), "{}", run.stdout);
}

#[tokio::test(flavor = "multi_thread")]
async fn specifications_come_as_json_or_yaml() {
    let server = doc_server().await;
    let cli = Cli::basic(&server);
    let run = cli
        .run(&["apisearch", "spec", "table api", "--agent"])
        .await
        .ok();
    assert_eq!(run.envelope()["data"]["openapi"], "3.0.1");
    for yaml in [["--as", "yaml"], ["--format", "yaml"]] {
        let run = cli
            .run(&[&["apisearch", "spec", "now/table"], &yaml[..]].concat())
            .await
            .ok();
        assert_eq!(run.stdout, "openapi: 3.0.1\n");
    }
}

const APP_ID: &str = "2d146921dbd80010caf55268dc9619d8";

async fn appmanager_server() -> MockServer {
    let server = MockServer::start().await;
    for (tab, apps, plugins) in [
        (
            "installed",
            json!([{ "name": "Workflow Studio", "scope": "sn_studio", "sys_id": APP_ID, "version": "1.0", "latest_version": "1.1", "update_available": "1" }]),
            json!([{ "app_name": "Incident", "app_id": "com.snc.incident", "active": "1", "description": "Service desk" }]),
        ),
        (
            "available_for_you",
            json!([]),
            json!([{ "app_name": "Signature Pad", "app_id": "com.sn_signature_pad", "active": "0" }]),
        ),
    ] {
        for (route, body) in [("apps", json!({ "apps": apps })), ("plugins", plugins)] {
            Mock::given(method("POST"))
                .and(path(format!("/api/sn_appclient/appmanager/{route}")))
                .and(query_param("tab_context", tab))
                .respond_with(result(body))
                .mount(&server)
                .await;
        }
    }
    for (id, plugin) in [
        (
            "com.snc.incident",
            json!({ "id": "com.snc.incident", "isInstalled": true, "version": "5.0" }),
        ),
        (
            "com.sn_signature_pad",
            json!({ "id": "com.sn_signature_pad", "isInstalled": false }),
        ),
        ("sn_studio", json!({})),
        ("com.none", json!({})),
        ("com.swapped", json!({ "id": "com.other" })),
    ] {
        on(
            &server,
            "GET",
            &format!("/api/sn_appclient/appmanager/plugin/{id}"),
            result(json!({ "plugin": plugin })),
        )
        .await;
    }
    on(
        &server,
        "GET",
        &format!("/api/sn_appclient/appmanager/app/{APP_ID}"),
        result(json!({ "app_info_on_instance": {
            "sys_id": APP_ID, "scope": "sn_studio", "name": "Workflow Studio", "isInstalled": true,
            "dependencies": "com.a:sys",
        } })),
    )
    .await;
    server
}

#[tokio::test(flavor = "multi_thread")]
async fn appmanager_lists_both_kinds_and_never_pages_there() {
    let server = appmanager_server().await;
    let mut cli = Cli::basic(&server);
    // The lists are POSTs that only read.
    cli.env.push(("SN_READ_ONLY".into(), "1".into()));
    let run = cli
        .run(&["appmanager", "list", "--limit", "1", "--agent"])
        .await
        .ok();
    let envelope = run.envelope();
    assert_eq!(envelope["data"][0]["id"], "com.snc.incident");
    assert_eq!(
        envelope["meta"],
        json!({ "total": 2, "next_offset": 1, "next": "add --offset 1", "count": 1 })
    );
    let run = cli
        .run(&["appmanager", "search", "DESK", "--agent"])
        .await
        .ok();
    assert_eq!(run.envelope()["data"][0]["name"], "Incident");
    let run = cli
        .run(&[
            "appmanager",
            "list",
            "--available",
            "-k",
            "plugin",
            "--agent",
        ])
        .await
        .ok();
    assert_eq!(run.envelope()["data"][0]["installed"], false);
    assert!(
        received(&server)
            .await
            .iter()
            .all(|r| !r.contains("limit") && !r.contains("search"))
    );
    assert_eq!(cli.run(&["appmanager", "list", "--raw"]).await.code, 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn appmanager_shows_what_was_asked_for() {
    let server = appmanager_server().await;
    let cli = Cli::basic(&server);
    for id in ["sn_studio", APP_ID] {
        let run = cli.run(&["appmanager", "show", id, "--agent"]).await.ok();
        assert_eq!(
            run.envelope()["data"]["dependencies"],
            json!([{ "id": "com.a", "version": "sys" }])
        );
    }
    let run = cli
        .run(&["appmanager", "show", "com.snc.incident", "--agent"])
        .await
        .ok();
    assert_eq!(run.envelope()["data"]["version"], "5.0");
    for (id, code) in [
        ("com.none", 4),
        ("com.swapped", 1),
        ("install", 3),
        ("a/b", 3),
    ] {
        let run = cli.run(&["appmanager", "show", id, "--agent"]).await;
        assert_eq!(run.code, code, "{id}: {}", run.stdout);
    }
    assert!(
        !received(&server)
            .await
            .iter()
            .any(|r| r.ends_with("/install"))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn appmanager_activates_only_resolved_plugins() {
    let server = appmanager_server().await;
    Mock::given(method("GET"))
        .and(path("/api/sn_appclient/appmanager/plugin/activate"))
        .respond_with(|request: &Request| {
            let q = query(request);
            assert_eq!(q["load_demo_data"], "true");
            result(json!({ "status": "0", "links": { "progress": { "id": "p1" } } }))
        })
        .expect(1)
        .mount(&server)
        .await;
    on(
        &server,
        "GET",
        "/api/sn_cicd/progress/p1",
        result(json!({ "status": "2", "status_message": "Plugin activated" })),
    )
    .await;
    let cli = Cli::basic(&server);
    let args = [
        "appmanager",
        "install",
        "com.sn_signature_pad",
        "--demo-data",
        "--agent",
    ];
    let run = cli.run(&args).await;
    assert_eq!(run.envelope()["error"]["code"], "confirmation_required");
    // The route is a GET; the guards still hold.
    let run = cli
        .run(&[&args[..], &["--yes", "--read-only"]].concat())
        .await;
    assert_eq!(run.envelope()["error"]["code"], "read_only");
    let run = cli.run(&[&args[..], &["--dry-run"]].concat()).await.ok();
    assert!(
        run.envelope()["data"]["request"]["url"]
            .as_str()
            .expect("url")
            .contains("plugin/activate?plugin_id=com.sn_signature_pad")
    );
    let run = cli
        .run(&[&args[..], &["--yes", "--wait"]].concat())
        .await
        .ok();
    assert_eq!(run.envelope()["data"]["status"], "2");
    for (id, code) in [("com.snc.incident", 3), ("com.none", 4)] {
        let run = cli
            .run(&["appmanager", "install", id, "--yes", "--agent"])
            .await;
        assert_eq!(run.code, code, "{id}: {}", run.stdout);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_activation_is_sent_once() {
    let slow = result(json!({})).set_delay(std::time::Duration::from_secs(3));
    for (answer, code) in [(ResponseTemplate::new(503), "api"), (slow, "timeout")] {
        let server = appmanager_server().await;
        Mock::given(method("GET"))
            .and(path("/api/sn_appclient/appmanager/plugin/activate"))
            .respond_with(answer)
            .expect(1)
            .mount(&server)
            .await;
        let run = Cli::basic(&server)
            .run(&[
                "appmanager",
                "install",
                "com.sn_signature_pad",
                "--yes",
                "--timeout",
                "1",
                "--agent",
            ])
            .await;
        assert_eq!(run.envelope()["error"]["code"], code, "{}", run.stdout);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn appmanager_names_the_roles_and_the_missing_route() {
    let server = MockServer::start().await;
    on(
        &server,
        "POST",
        "/api/sn_appclient/appmanager/apps",
        ResponseTemplate::new(403)
            .set_body_json(json!({ "error": { "message": "User Not Authorized" } })),
    )
    .await;
    let args = ["appmanager", "list", "-k", "app", "--agent"];
    let run = Cli::basic(&server).run(&args).await;
    assert!(
        run.code == 2 && run.stdout.contains("app_client_user"),
        "{}",
        run.stdout
    );
    let server = MockServer::start().await;
    on(
        &server,
        "POST",
        "/api/sn_appclient/appmanager/apps",
        missing(),
    )
    .await;
    let run = Cli::basic(&server).run(&args).await;
    assert!(
        run.stdout.contains("no Application Manager API"),
        "{}",
        run.stdout
    );
}

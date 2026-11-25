//! `change`, `approval` and `knowledge`.

mod support;

use serde_json::{Value, json};
use support::{Cli, ME, mount_caller, on, pair, query, result};
use wiremock::matchers::{body_json, method, path};
use wiremock::{Mock, MockServer, Request};

const ID: &str = "9d385017c611228701d22104cc95c371";
const TASK: &str = "11111111111111111111111111111111";
const BASE: &str = "/api/sn_chg_rest/change";

#[tokio::test(flavor = "multi_thread")]
async fn lists_drop_the_meta_element_and_warn_about_ignored_fields() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("{BASE}/normal")))
        .respond_with(|request: &Request| {
            let q = query(request);
            assert_eq!(q["sysparm_query"], "active=true^zzz=1");
            assert_eq!(q["sysparm_limit"], "1");
            assert!(!q.contains_key("sysparm_display_value"));
            result(json!([
                { "number": pair("CHG1", "CHG1"), "state": pair("-5", "New") },
                { "__meta": { "fields": { "ignored": ["", "sysparm_limit", "zzz"] } } },
            ]))
        })
        .mount(&server)
        .await;
    let cli = Cli::basic(&server);
    let args = [
        "change",
        "list",
        "--type",
        "normal",
        "-q",
        "active=true^zzz=1",
        "--setlimit",
        "1",
    ];
    let run = cli.run(&[&args[..], &["--agent"]].concat()).await.ok();
    let envelope = run.envelope();
    assert_eq!(envelope["data"].as_array().map(Vec::len), Some(1));
    assert_eq!(envelope["meta"]["ignored_query_fields"], json!(["zzz"]));
    assert_eq!(envelope["meta"]["next_offset"], 1);
    assert!(run.stderr.contains("zzz") && !run.stderr.contains("sysparm_limit"));
    let run = cli.run(&[&args[..], &["-o", "table"]].concat()).await.ok();
    assert!(
        run.stdout.contains("CHG1") && run.stdout.contains("New"),
        "{}",
        run.stdout
    );
    let run = cli.run(&[&args[..], &["--raw"]].concat()).await.ok();
    assert_eq!(
        run.json()["result"][1]["__meta"]["fields"]["ignored"][2],
        "zzz"
    );
    let run = cli
        .run(&["change", "list", "-q", "a=1^ORDERBYDESCnumber"])
        .await;
    assert_eq!(run.code, 3, "the API would ignore the order");
    assert!(
        run.stderr.contains("table list change_request"),
        "{}",
        run.stderr
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn every_call_has_its_own_path() {
    let server = MockServer::start().await;
    let cli = Cli::basic(&server);
    let at = format!("{BASE}/{ID}");
    let task = format!("{at}/task/{TASK}");
    let field = ["-F", "a=1"];
    let body = Some(json!({ "a": "1" }));
    for (words, flags, verb, at, body) in [
        (
            vec!["get", ID, "--type", "emergency"],
            &[][..],
            "GET",
            format!("{BASE}/emergency/{ID}"),
            None,
        ),
        (
            vec!["create"],
            &field,
            "POST",
            format!("{BASE}/normal"),
            body.clone(),
        ),
        (
            vec!["create", "--type", "standard", "--template", TASK],
            &[],
            "POST",
            format!("{BASE}/standard/{TASK}"),
            Some(json!({})),
        ),
        (
            vec!["update", ID],
            &field,
            "PATCH",
            at.clone(),
            body.clone(),
        ),
        (vec!["delete", ID, "--yes"], &[], "DELETE", at.clone(), None),
        (
            vec!["nextstates", ID],
            &[],
            "GET",
            format!("{at}/nextstates"),
            None,
        ),
        (
            vec!["approvals", ID],
            &field,
            "PATCH",
            format!("{at}/approvals"),
            body.clone(),
        ),
        (
            vec!["risk", ID],
            &field,
            "PATCH",
            format!("{at}/risk"),
            body.clone(),
        ),
        (
            vec!["schedule", ID],
            &[],
            "GET",
            format!("{at}/schedule"),
            None,
        ),
        (
            vec!["task", "list", ID],
            &[],
            "GET",
            format!("{at}/task"),
            None,
        ),
        (
            vec!["task", "get", ID, TASK],
            &[],
            "GET",
            task.clone(),
            None,
        ),
        (
            vec!["task", "create", ID],
            &field,
            "POST",
            format!("{at}/task"),
            body.clone(),
        ),
        (
            vec!["task", "update", ID, TASK],
            &field,
            "PATCH",
            task.clone(),
            body.clone(),
        ),
        (
            vec!["task", "delete", ID, TASK, "--yes"],
            &[],
            "DELETE",
            task,
            None,
        ),
        (vec!["ci", "list", ID], &[], "GET", format!("{at}/ci"), None),
        (
            vec!["ci", "add", ID],
            &field,
            "POST",
            format!("{at}/ci"),
            body.clone(),
        ),
        (
            vec!["conflict", "get", ID],
            &[],
            "GET",
            format!("{at}/conflict"),
            None,
        ),
        (
            vec!["conflict", "add", ID],
            &[],
            "POST",
            format!("{at}/conflict"),
            Some(json!({})),
        ),
        (
            vec!["conflict", "remove", ID, "--yes"],
            &[],
            "DELETE",
            format!("{at}/conflict"),
            None,
        ),
        (vec!["models"], &[], "GET", format!("{BASE}/model"), None),
        (
            vec!["models", TASK],
            &[],
            "GET",
            format!("{BASE}/model/{TASK}"),
            None,
        ),
        (
            vec!["templates"],
            &[],
            "GET",
            format!("{BASE}/standard/template"),
            None,
        ),
    ] {
        server.reset().await;
        on(&server, verb, &at, result(json!({ "sys_id": ID }))).await;
        cli.run(&[&["change", "--agent"], &words[..], flags].concat())
            .await
            .ok();
        let requests = server.received_requests().await.expect("requests");
        assert_eq!(requests.len(), 1, "{words:?}");
        if let Some(body) = body {
            let sent: Value = serde_json::from_slice(&requests[0].body).expect("body");
            assert_eq!(sent, body, "{words:?}");
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn writes_are_checked_before_anything_is_sent() {
    let server = MockServer::start().await;
    let cli = Cli::basic(&server);
    for words in [
        vec!["update", ID],
        vec!["create", "--type", "standard"],
        vec!["create", "--template", TASK],
        vec!["create", "--type", "other"],
    ] {
        assert_eq!(
            cli.run(&[&["change"], &words[..]].concat()).await.code,
            3,
            "{words:?}"
        );
    }
    support::assert_untouched(&server).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_change_is_found_by_number_and_its_states_have_labels() {
    let server = MockServer::start().await;
    on(
        &server,
        "GET",
        "/api/now/table/change_request",
        result(json!([{ "sys_id": ID, "number": "CHG0000001" }])),
    )
    .await;
    on(
        &server,
        "GET",
        &format!("{BASE}/{ID}/nextstates"),
        result(json!({ "available_states": ["-1", "4"], "state_label": { "-1": "Implement", "4": "Canceled" } })),
    )
    .await;
    let run = Cli::basic(&server)
        .run(&["change", "nextstates", "CHG0000001", "-o", "table"])
        .await
        .ok();
    assert_eq!(run.stdout, "Implement (-1)\nCanceled (4)\n");
}

fn approval(state: &str, approver: &str) -> Value {
    json!({
        "sys_id": pair(TASK, TASK), "state": pair(state, "Requested"), "approver": pair(approver, "Ada Lovelace"),
        "sysapproval": pair(ID, "CHG0000001"), "sysapproval.short_description": pair("Patch", "Patch"),
        "document_id": pair(ID, "Change: CHG0000001"), "source_table": pair("change_request", "Change"),
        "sys_created_on": pair("2026-01-01 00:00:00", "local"),
    })
}

const APPROVALS: &str = "/api/now/table/sysapproval_approver";

#[tokio::test(flavor = "multi_thread")]
async fn approvals_that_wait_for_you() {
    let server = MockServer::start().await;
    mount_caller(&server).await;
    Mock::given(method("GET"))
        .and(path(APPROVALS))
        .respond_with(|request: &Request| {
            match query(request)["sysparm_query"].as_str() {
                "ORDERBYDESCsys_created_on" => {}
                mine => assert_eq!(
                    mine,
                    format!("approver={ME}^state=requested^ORDERBYDESCsys_created_on")
                ),
            }
            result(json!([approval("requested", ME)]))
        })
        .mount(&server)
        .await;
    let cli = Cli::basic(&server);
    let run = cli.run(&["approval", "list", "--agent"]).await.ok();
    let row = &run.envelope()["data"][0];
    assert_eq!(row["record"], "CHG0000001");
    assert_eq!(row["record_sys_id"], ID);
    assert_eq!(row["short_description"], "Patch");
    let run = cli
        .run(&[
            "approvals",
            "list",
            "--everyone",
            "--state",
            "any",
            "-o",
            "table",
        ])
        .await
        .ok();
    assert!(run.stdout.contains("Ada Lovelace"), "{}", run.stdout);
    server.reset().await;
    mount_caller(&server).await;
    on(
        &server,
        "GET",
        APPROVALS,
        result(json!([approval("requested", TASK)])),
    )
    .await;
    assert_eq!(
        cli.run(&["approval", "list"]).await.code,
        1,
        "someone else's approval"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn approving_by_record_number_confirms_and_checks_the_result() {
    let server = MockServer::start().await;
    mount_caller(&server).await;
    on(
        &server,
        "GET",
        "/api/now/table/change_request",
        result(json!([{ "sys_id": ID, "number": "CHG0000001" }])),
    )
    .await;
    Mock::given(method("GET"))
        .and(path(APPROVALS))
        .respond_with(|request: &Request| {
            assert_eq!(
                query(request)["sysparm_query"],
                format!("sysapproval={ID}^ORdocument_id={ID}^approver={ME}^state=requested^ORDERBYDESCsys_created_on")
            );
            result(json!([approval("requested", ME)]))
        })
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path(format!("{APPROVALS}/{TASK}")))
        .and(body_json(json!({ "state": "approved", "comments": "ok" })))
        .respond_with(result(approval("approved", ME)))
        .expect(1)
        .mount(&server)
        .await;
    let cli = Cli::basic(&server);
    let args = ["approval", "approve", "CHG0000001", "-m", "ok", "--agent"];
    let run = cli.run(&args).await;
    let error = &run.envelope()["error"];
    assert_eq!(error["code"], "confirmation_required");
    assert!(
        error["message"]
            .to_string()
            .contains("approve CHG0000001 Patch"),
        "{error}"
    );
    let run = cli.run(&[&args[..], &["--yes"]].concat()).await.ok();
    assert_eq!(run.envelope()["data"]["state"], "approved");
}

#[tokio::test(flavor = "multi_thread")]
async fn rejecting_needs_a_comment_and_an_open_approval() {
    let server = MockServer::start().await;
    let at = format!("{APPROVALS}/{TASK}");
    on(&server, "GET", &at, result(approval("requested", ME))).await;
    // A business rule refused the change without an error.
    on(&server, "PATCH", &at, result(approval("requested", ME))).await;
    let cli = Cli::basic(&server);
    let run = cli
        .run(&["approval", "reject", TASK, "--yes", "--agent"])
        .await;
    assert_eq!(run.envelope()["error"]["input"]["arg"], "comment");
    let run = cli
        .run(&["approval", "reject", TASK, "-m", "no", "--yes", "--agent"])
        .await;
    assert_eq!(run.envelope()["error"]["code"], "api");
    server.reset().await;
    on(&server, "GET", &at, result(approval("approved", ME))).await;
    let run = cli
        .run(&["approval", "reject", TASK, "-m", "no", "--yes", "--agent"])
        .await;
    assert_eq!(run.envelope()["error"]["code"], "conflict");
    assert_eq!(
        cli.run(&["approval", "show", TASK]).await.ok().json()["state"],
        "approved"
    );
}

const ARTICLES: &str = "/api/sn_km_api/knowledge/articles";

#[tokio::test(flavor = "multi_thread")]
async fn articles_are_found_and_listed() {
    let server = MockServer::start().await;
    let page = json!({
        "meta": { "count": 19 },
        "articles": [{ "id": "kb_knowledge:x", "number": "KB0000020", "title": "Windows", "score": 14.8, "fields": { "author": { "display_value": "Ada", "value": "1" } } }],
    });
    let answer = page.clone();
    Mock::given(method("GET"))
        .and(path(ARTICLES))
        .respond_with(move |request: &Request| {
            let q = query(request);
            assert_eq!(q["query"], "windows");
            assert_eq!(q["filter"], "workflow_state=published");
            assert_eq!(q["kb"], "a,b");
            assert_eq!(q["language"], "en");
            assert_eq!(q["fields"], "author");
            assert_eq!(q["limit"], "1");
            result(answer.clone())
        })
        .mount(&server)
        .await;
    for at in ["featured", "most_viewed"] {
        on(
            &server,
            "GET",
            &format!("{ARTICLES}/{at}"),
            result(page.clone()),
        )
        .await;
    }
    let cli = Cli::basic(&server);
    let args = [
        "knowledge",
        "search",
        "windows",
        "-q",
        "workflow_state=published",
        "--kb",
        "a,b",
        "--language",
        "en",
        "-f",
        "author",
        "-n",
        "1",
    ];
    let run = cli.run(&[&args[..], &["--agent"]].concat()).await.ok();
    assert_eq!(
        run.envelope()["meta"],
        json!({ "total": 19, "next_offset": 1, "next": "add --offset 1", "count": 1 })
    );
    let run = cli.run(&[&args[..], &["-o", "table"]].concat()).await.ok();
    assert!(
        run.stdout.contains("AUTHOR") && run.stdout.contains("Ada"),
        "{}",
        run.stdout
    );
    for flag in ["--featured", "--most-viewed"] {
        let run = cli.run(&["kb", "list", flag]).await.ok();
        assert_eq!(run.json()[0]["number"], "KB0000020");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_article_is_text_for_people_and_html_for_machines() {
    let server = MockServer::start().await;
    on(
        &server,
        "GET",
        &format!("{ARTICLES}/KB0000011"),
        result(json!({
            "number": "KB0000011", "short_description": "Spam\t", "content": "<p>Do <b>not</b> reply</p>",
            "attachments": [{ "sys_id": ID, "file_name": "a.txt", "size_bytes": "75" }],
        })),
    )
    .await;
    let cli = Cli::basic(&server);
    let run = cli
        .run(&["knowledge", "get", "KB0000011", "-o", "table"])
        .await
        .ok();
    assert_eq!(
        run.stdout,
        format!("KB0000011  Spam\n\nDo not reply\n\nAttachments\n  {ID}  75 B  a.txt\n")
    );
    let run = cli.run(&["knowledge", "get", "KB0000011"]).await.ok();
    assert_eq!(run.json()["content"], "<p>Do <b>not</b> reply</p>");
    let run = cli
        .run(&["knowledge", "get", "KB0000011", "--text", "--agent"])
        .await
        .ok();
    assert_eq!(run.envelope()["data"]["content"], "Do not reply");
    assert_eq!(cli.run(&["knowledge", "get", "../x"]).await.code, 3);
    let run = cli.run(&["knowledge", "get", "KB9"]).await;
    assert_eq!(run.code, 4, "{}", run.stderr);
}

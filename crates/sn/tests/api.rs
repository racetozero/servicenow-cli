//! Catalog commands.

mod support;

use serde_json::json;
use support::{Cli, on, query, result};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

#[tokio::test(flavor = "multi_thread")]
async fn flags_go_to_path_query_and_body() {
    let server = MockServer::start().await;
    Mock::given(method("PATCH"))
        .and(path("/api/sn_chg_rest/change/a%2Fb/approvals"))
        .respond_with(|request: &Request| {
            let body: serde_json::Value = serde_json::from_slice(&request.body).expect("json");
            assert_eq!(body["state"], "approved");
            assert_eq!(query(request)["x"], "1");
            result(json!({ "done": true }))
        })
        .mount(&server)
        .await;
    let cli = Cli::basic(&server);
    let run = cli
        .run(&[
            "api",
            "change-management",
            "update-approvals",
            "--sys-id",
            "a/b",
            "--state",
            "approved",
            "-P",
            "x=1",
            "--agent",
        ])
        .await
        .ok();
    assert_eq!(run.envelope()["data"], json!({ "done": true }));
    assert_eq!(
        run.envelope()["command"],
        "api change-management update-approvals"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn uploads_send_the_file_and_downloads_write_one() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/now/attachment/file"))
        .respond_with(|request: &Request| {
            assert_eq!(request.body, b"bytes");
            assert_eq!(request.headers["content-type"], "text/csv");
            assert_eq!(query(request)["file_name"], "a.csv");
            result(json!({ "sys_id": "1" })).set_body_json(json!({ "result": { "sys_id": "1" } }))
        })
        .mount(&server)
        .await;
    on(
        &server,
        "GET",
        "/api/now/attachment/7/file",
        ResponseTemplate::new(200).set_body_raw(b"\xff\x00raw".to_vec(), "image/png"),
    )
    .await;
    let cli = Cli::basic(&server);
    let file = cli.dir.path().join("a.csv");
    std::fs::write(&file, "bytes").expect("write");
    let file = file.display().to_string();
    cli.run(&[
        "api",
        "attachment",
        "create-file",
        "--table-name",
        "incident",
        "--table-sys-id",
        "x",
        "--file-name",
        "a.csv",
        "--file",
        &file,
        "-H",
        "Content-Type: text/csv",
        "--agent",
    ])
    .await
    .ok();
    let out = cli.dir.path().join("out.png").display().to_string();
    cli.run(&[
        "api",
        "attachment",
        "list-file",
        "--sys-id",
        "7",
        "--out",
        &out,
        "--agent",
    ])
    .await
    .ok();
    assert_eq!(std::fs::read(&out).expect("file"), b"\xff\x00raw");
}

#[tokio::test(flavor = "multi_thread")]
async fn deletes_confirm_and_mistakes_list_the_choices() {
    let server = MockServer::start().await;
    let cli = Cli::basic(&server);
    let run = cli
        .run(&[
            "api",
            "table",
            "delete",
            "--table-name",
            "incident",
            "--sys-id",
            "x",
            "--agent",
        ])
        .await;
    assert_eq!(run.envelope()["error"]["code"], "confirmation_required");
    let run = cli
        .run(&["api", "describe", "table", "nope", "--agent"])
        .await;
    let error = &run.envelope()["error"];
    assert_eq!(error["code"], "input_required");
    assert!(
        error["input"]["choices"]
            .as_array()
            .expect("choices")
            .iter()
            .any(|c| c["value"] == "list")
    );
    let run = cli.run(&["api", "nope", "x", "--agent"]).await;
    assert_eq!(run.envelope()["error"]["code"], "invalid");
    let run = cli.run(&["api", "search", "xyzzy", "--agent"]).await.ok();
    assert_eq!(run.envelope()["meta"]["total"], 0);
    let run = cli
        .run(&["api", "describe", "table", "list", "--agent"])
        .await
        .ok();
    assert_eq!(run.envelope()["data"]["params"][0]["flag"], "--table-name");
}

//! Background scripts.

mod support;

use serde_json::{Value, json};
use support::{Cli, Run, form, on, result};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const TOKEN: &str = "CSRFTOKEN0xDEADBEEF";
const HISTORY: &str = "48169e6a2fe303107efd1d707fa4e3f4";
const SCOPE: &str = "0c4e7b573b331300ad3cc9bb34efc461";

fn page(scope: &str, pre: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_string(format!(
        "[0:00:00.951] <HTML><BODY>Script completed in scope {scope}: script<HR/>Script execution history <A target='blank' HREF='sys_script_execution_history.do?sys_id={HISTORY}'>available here</A><HR/><PRE>{pre}</PRE><HR/></BODY></HTML>"
    ))
}

async fn server() -> MockServer {
    let server = MockServer::start().await;
    on(
        &server,
        "GET",
        "/api/now/sg/impersonation/session",
        ResponseTemplate::new(200)
            .append_header("set-cookie", "JSESSIONID=S1; Path=/")
            .set_body_json(json!({ "CurrentUser": "admin", "SessionToken": TOKEN })),
    )
    .await;
    on(
        &server,
        "GET",
        &format!("/api/now/table/sys_script_execution_history/{HISTORY}"),
        result(json!({ "rollback_context": "ctx1" })),
    )
    .await;
    // Whatever the filter, as an instance that ignores it.
    on(
        &server,
        "GET",
        "/api/now/table/sys_scope",
        result(json!([{ "sys_id": SCOPE, "scope": "x_acme", "name": "Acme" }])),
    )
    .await;
    server
}

/// Answers the form, and gives its fields to `check`.
async fn scripts(
    server: &MockServer,
    answer: ResponseTemplate,
    check: impl Fn(&std::collections::HashMap<String, String>) + Send + Sync + 'static,
) {
    Mock::given(method("POST"))
        .and(path("/sys.scripts.do"))
        .respond_with(move |request: &Request| {
            let header = |name: &str| request.headers.get(name).and_then(|v| v.to_str().ok());
            assert_eq!(header("cookie"), Some("JSESSIONID=S1"));
            assert_eq!(header("x-usertoken"), Some(TOKEN));
            let fields = form(request);
            assert_eq!(fields["sysparm_ck"], TOKEN);
            check(&fields);
            answer.clone()
        })
        .mount(server)
        .await;
}

fn error(run: &Run) -> Value {
    run.envelope()["error"].clone()
}

#[tokio::test(flavor = "multi_thread")]
async fn output_messages_and_rollback_come_back() {
    let server = server().await;
    let answer = page(
        "global",
        "*** Script: hello<BR/>*** Script: {&quot;n&quot;:3}<BR/>Slow business rule<BR/>",
    );
    scripts(&server, answer, |fields| {
        assert_eq!(
            fields["script"],
            "gs.info('hello')\n// Reason: CHG1 clean up\n"
        );
        assert_eq!(
            (&*fields["sys_scope"], &*fields["record_for_rollback"]),
            ("global", "on")
        );
    })
    .await;
    let cli = Cli::basic(&server);
    let args = [
        "exec",
        "gs.info('hello')",
        "--reason",
        "CHG1\nclean up",
        "--yes",
    ];
    let run = cli.run(&[&args[..], &["--agent"]].concat()).await.ok();
    assert_eq!(
        run.envelope()["data"],
        json!({
            "ok": true, "scope": "global", "output": ["hello", "{\"n\":3}"],
            "messages": ["Slow business rule"], "error": null, "elapsed_ms": 951,
            "history_id": HISTORY, "rollback_context": "ctx1",
        })
    );
    let run = cli.run(&[&args[..], &["-o", "table"]].concat()).await.ok();
    assert_eq!(run.stdout, "hello\n{\"n\":3}\n");
    assert!(
        run.stderr.contains("rollback context ctx1"),
        "{}",
        run.stderr
    );
    assert!(!run.stderr.contains(TOKEN));
    let run = cli
        .run(&[&args[..], &["--agent", "--dry-run"]].concat())
        .await
        .ok();
    assert!(!run.stdout.contains(TOKEN), "{}", run.stdout);
}

#[tokio::test(flavor = "multi_thread")]
async fn scripts_come_from_stdin_and_files() {
    let server = server().await;
    scripts(&server, page("global", "*** Script: ok<BR/>"), |fields| {
        assert_eq!(fields["script"].trim_end(), "gs.info('x')");
        assert!(!fields.contains_key("record_for_rollback"));
    })
    .await;
    let cli = Cli::basic(&server);
    let file = cli.dir.path().join("job.js");
    std::fs::write(&file, "gs.info('x')").expect("write");
    let file = file.display().to_string();
    let at = format!("@{file}");
    let tail = ["--no-rollback", "--yes", "--agent"];
    for source in [&["-"][..], &["@-"], &["--file", "-"]] {
        cli.run_with(
            &[&["script", "run"], source, &tail[..]].concat(),
            Some("gs.info('x')"),
        )
        .await
        .ok();
    }
    for source in [&["--file", &file][..], &[&at]] {
        cli.run(&[&["exec"], source, &tail[..]].concat()).await.ok();
    }
    // stdin is never read unasked.
    let run = cli
        .run_with(&["exec", "--yes", "--agent"], Some("gs.info('x')"))
        .await;
    assert_eq!(
        (run.code, error(&run)["input"]["arg"].clone()),
        (3, json!("script"))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_script_error_is_one_error_with_the_whole_result() {
    let server = server().await;
    let answer = page(
        "global",
        "*** Script: before<BR/>Script execution error: Script Identifier: null.null.script, Error Description: Error: boom (null.null.script; line 2), Script ES Level: 0<BR/>Root cause<BR/>",
    );
    scripts(&server, answer, |_| ()).await;
    let cli = Cli::basic(&server);
    let args = ["exec", "x", "--yes"];
    let run = cli
        .run(&[&args[..], &["--agent", "--server-logs"]].concat())
        .await;
    let err = error(&run);
    assert_eq!((run.code, &err["code"]), (1, &json!("script_error")));
    assert_eq!(err["data"]["output"], json!(["before"]));
    assert_eq!(err["data"]["error"]["line"], 2);
    assert_eq!(err["data"]["log"][1], "Root cause");
    let run = cli.run(&[&args[..], &["-o", "table"]].concat()).await;
    assert_eq!(
        (run.code, run.stdout.as_str()),
        (1, "before\n"),
        "people see what was printed"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn scopes_are_resolved_and_checked() {
    let server = server().await;
    scripts(&server, page("x_acme", "x_acme: hi<BR/>"), |fields| {
        assert_eq!(fields["sys_scope"], SCOPE);
    })
    .await;
    let cli = Cli::basic(&server);
    let args = ["exec", "x", "--yes", "--agent", "--scope"];
    let run = cli.run(&[&args[..], &["x_acme"]].concat()).await.ok();
    assert_eq!(run.envelope()["data"]["output"], json!(["hi"]));
    let run = cli.run(&[&args[..], &["x_none"]].concat()).await;
    assert_eq!(
        run.code, 4,
        "an unknown scope never runs in global: {}",
        run.stdout
    );
    // A project here names its scope.
    std::fs::write(
        cli.dir.path().join("now.config.json"),
        json!({ "scopeId": SCOPE }).to_string(),
    )
    .expect("write");
    let mut command = cli.command(&["exec", "x", "--yes", "--agent"]);
    command.current_dir(cli.dir.path());
    let output = tokio::task::spawn_blocking(move || command.output().expect("run"))
        .await
        .expect("join");
    let envelope: Value = serde_json::from_slice(&output.stdout).expect("json");
    assert_eq!(envelope["data"]["scope"], "x_acme", "{envelope}");

    let wrong = self::server().await;
    scripts(&wrong, page("global", ""), |_| ()).await;
    let run = Cli::basic(&wrong)
        .run(&[&args[..], &["x_acme"]].concat())
        .await;
    assert_eq!(run.code, 1);
    assert!(
        run.envelope()["error"]["message"]
            .as_str()
            .expect("message")
            .contains("ran the script in global"),
        "{}",
        run.stdout
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cookie_profile_uses_its_own_session() {
    let server = MockServer::start().await;
    on(
        &server,
        "GET",
        "/api/now/sg/impersonation/session",
        ResponseTemplate::new(200)
            .set_body_json(json!({ "CurrentUser": "a", "SessionToken": TOKEN })),
    )
    .await;
    on(
        &server,
        "GET",
        &format!("/api/now/table/sys_script_execution_history/{HISTORY}"),
        result(json!({})),
    )
    .await;
    scripts(&server, page("global", "*** Script: ok<BR/>"), |_| ()).await;
    let mut cli = Cli::empty();
    cli.env = vec![
        ("SN_INSTANCE".into(), server.uri()),
        ("SN_COOKIE".into(), "JSESSIONID=S1".into()),
    ];
    let run = cli.run(&["exec", "x", "--yes", "--agent"]).await.ok();
    assert_eq!(run.envelope()["data"]["output"], json!(["ok"]));
}

#[tokio::test(flavor = "multi_thread")]
async fn refusals_say_that_nothing_ran() {
    let redirect = ResponseTemplate::new(302).insert_header("location", "/logout_redirect.do");
    for (answer, code) in [
        (ResponseTemplate::new(403), "permission"),
        (redirect, "permission"),
        (
            ResponseTemplate::new(200).set_body_string("not authorized"),
            "permission",
        ),
        (ResponseTemplate::new(200), "auth"),
        (
            ResponseTemplate::new(200).set_body_string("<html>maintenance</html>"),
            "api",
        ),
    ] {
        let server = server().await;
        on(
            &server,
            "GET",
            "/logout_redirect.do",
            ResponseTemplate::new(200).set_body_string("<html>login</html>"),
        )
        .await;
        scripts(&server, answer, |_| ()).await;
        let run = Cli::basic(&server)
            .run(&["exec", "x", "--yes", "--agent"])
            .await;
        assert_eq!(error(&run)["code"], code, "{}", run.stdout);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_script_needs_confirmation_and_a_writable_profile() {
    let server = MockServer::start().await;
    let mut cli = Cli::basic(&server);
    let run = cli.run(&["exec", "x", "--agent"]).await;
    assert_eq!(error(&run)["code"], "confirmation_required");
    support::assert_untouched(&server).await;
    let server = self::server().await;
    cli = Cli::basic(&server);
    let run = cli
        .run(&["exec", "x", "--yes", "--agent", "--read-only"])
        .await;
    assert_eq!(error(&run)["code"], "read_only");
}

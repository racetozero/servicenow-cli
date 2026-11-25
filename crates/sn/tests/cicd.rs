//! The CI/CD API.

mod support;

use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use support::{Cli, on, query, result};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

fn started() -> Value {
    json!({ "links": { "progress": { "id": "p1" } }, "status": "0", "status_label": "Pending" })
}

/// Progress that is pending once, then `end`.
async fn progress(server: &MockServer, end: Value) {
    let calls = AtomicUsize::new(0);
    Mock::given(method("GET"))
        .and(path("/api/sn_cicd/progress/p1"))
        .respond_with(
            move |_: &Request| match calls.fetch_add(1, Ordering::SeqCst) {
                0 => result(
                    json!({ "status": "1", "percent_complete": "40", "status_message": "Running" }),
                ),
                _ => result(end.clone()),
            },
        )
        .mount(server)
        .await;
}

/// Every operation: its arguments, path, query and body.
#[tokio::test(flavor = "multi_thread")]
async fn each_verb_calls_its_endpoint() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(result(started()))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .respond_with(result(json!({ "status": "2" })))
        .mount(&server)
        .await;
    let cli = Cli::basic(&server);
    let cases: &[(&str, &str, &str)] = &[
        (
            "app install --scope x_acme --version 1.2 --auto-upgrade-base-app --base-app-version 3",
            "POST app_repo/install",
            "scope=x_acme&version=1.2&auto_upgrade_base_app=true&base_app_version=3",
        ),
        (
            "app install --app-sys-id a1 --app-version 2",
            "POST app_repo/install",
            "sys_id=a1&version=2",
        ),
        (
            "app publish --sys-id a1 --dev-notes hi",
            "POST app_repo/publish",
            "sys_id=a1&dev_notes=hi",
        ),
        (
            "app rollback --scope x --version 1.0 --yes",
            "POST app_repo/rollback",
            "scope=x&version=1.0",
        ),
        (
            "updateset create --update-set-name Fix --scope x",
            "POST update_set/create",
            "update_set_name=Fix&scope=x",
        ),
        (
            "update-set retrieve --update-set-id u --update-source-id s --auto-preview --cleanup-retrieved",
            "POST update_set/retrieve",
            "update_set_id=u&update_source_id=s&auto_preview=true&cleanup_retrieved=true",
        ),
        ("updateset preview r1", "POST update_set/preview/r1", ""),
        ("updateset commit r1", "POST update_set/commit/r1", ""),
        (
            "updateset commit-multiple --ids r1,r2",
            "POST update_set/commitMultiple",
            "remote_update_set_ids=r1%2Cr2",
        ),
        (
            "updateset back-out --update-set-id u --rollback-installs --yes",
            "POST update_set/back_out",
            "update_set_id=u&rollback_installs=true",
        ),
        (
            "atf run --suite-name Smoke --browser-name chrome --performance-run --run-in-cloud",
            "POST testsuite/run",
            "test_suite_name=Smoke&browser_name=chrome&run_in_cloud=true&is_performance_run=true",
        ),
        (
            "testsuite run --test-suite-sys-id s1",
            "POST testsuite/run",
            "test_suite_sys_id=s1",
        ),
        ("atf results r1", "GET testsuite/results/r1", ""),
        (
            "testsuite result --result-id r1",
            "GET testsuite/results/r1",
            "",
        ),
        (
            "test run --test-sys-id t1 --capture-node-logs",
            "POST tests/run_test",
            "test_sys_id=t1&capture_node_logs=true",
        ),
        ("test result r1", "GET tests/test/results/r1", ""),
        (
            "test logs r1 --pattern fail --limit 5",
            "GET tests/test/results/r1/logs",
            "sysparm_regex=fail&sysparm_limit=5",
        ),
        ("batch results r1", "GET app/batch/results/r1", ""),
        ("batch rollback b1 --yes", "POST app/batch/rollback/b1", ""),
        (
            "plugin activate com.snc.x --yes",
            "POST plugin/com.snc.x/activate",
            "",
        ),
        (
            "plugin rollback com.snc.x --yes",
            "POST plugin/com.snc.x/rollback",
            "",
        ),
        ("scan full", "POST instance_scan/full_scan", ""),
        (
            "scan point --table incident --sys-id i1",
            "POST instance_scan/point_scan",
            "target_table=incident&target_sys_id=i1",
        ),
        (
            "scan combo c1",
            "POST instance_scan/suite_scan/combo/c1",
            "",
        ),
        ("scan results p9", "GET instance_scan/result/p9", ""),
        (
            "sc apply --scope x --branch main --no-auto-upgrade-base-app",
            "POST sc/apply_changes",
            "scope=x&branch_name=main&auto_upgrade_base_app=false",
        ),
        (
            "source-control apply-stash s1",
            "POST sc/apply_stash/s1",
            "",
        ),
        (
            "sc import --repo-url https://g/r.git --branch main --credential c",
            "POST sc/import",
            "repo_url=https%3A%2F%2Fg%2Fr.git&branch_name=main&credential_sys_id=c",
        ),
        ("progress p9", "GET progress/p9", ""),
    ];
    for (args, call, sent) in cases {
        let words: Vec<&str> = args.split(' ').collect();
        cli.run(&[&["cicd"], &words[..], &["--agent"]].concat())
            .await
            .ok()
            .envelope();
        let requests = server.received_requests().await.expect("requests");
        let last = requests.last().expect("request");
        let got = format!(
            "{} {}",
            last.method,
            last.url.path().trim_start_matches("/api/sn_cicd/")
        );
        assert_eq!(
            (got.as_str(), last.url.query().unwrap_or("")),
            (*call, *sent),
            "{args}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn bodies_name_packages_and_scan_targets() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(result(started()))
        .mount(&server)
        .await;
    let cli = Cli::basic(&server);
    let cases: &[(&[&str], Value)] = &[
        (
            &[
                "batch", "install", "--name", "R1", "--app", "a1@1.0.2", "--plugin", "com.x",
            ],
            json!({ "name": "R1", "packages": [
                { "id": "a1", "type": "application", "requested_version": "1.0.2" },
                { "id": "com.x", "type": "plugin" },
            ] }),
        ),
        (
            &["scan", "suite", "s1", "--apps", "a,b"],
            json!({ "app_scope_sys_ids": ["a", "b"] }),
        ),
        (
            &["scan", "suite", "s1", "--update-sets", "u"],
            json!({ "update_set_sys_ids": ["u"] }),
        ),
    ];
    for (args, body) in cases {
        cli.run(&[&["cicd"], *args, &["--agent"]].concat())
            .await
            .ok();
        let requests = server.received_requests().await.expect("requests");
        let sent: Value =
            serde_json::from_slice(&requests.last().expect("request").body).expect("json");
        assert_eq!(sent, *body, "{args:?}");
    }
    let count = server.received_requests().await.expect("requests").len();
    for bad in [
        &["cicd", "batch", "install", "--name", "x"][..],
        &["cicd", "batch", "install", "--name", "x", "--app", "a1"],
        &["cicd", "scan", "suite", "s1"],
        &["cicd", "updateset", "commit", "../x"],
        &["cicd", "atf", "run"],
        &[
            "cicd",
            "app",
            "install",
            "--wait-timeout",
            "5",
            "--scope",
            "x",
        ],
    ] {
        assert_eq!(cli.run(bad).await.code, 3, "{bad:?}");
    }
    assert_eq!(
        server.received_requests().await.expect("requests").len(),
        count
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn without_wait_the_answer_says_how_to_follow() {
    let server = MockServer::start().await;
    on(
        &server,
        "POST",
        "/api/sn_cicd/app_repo/install",
        result(started()),
    )
    .await;
    let cli = Cli::basic(&server);
    let run = cli
        .run(&["cicd", "app", "install", "--scope", "x", "-o", "table"])
        .await
        .ok();
    assert!(
        run.stdout.contains("sn cicd progress p1 --wait"),
        "{}",
        run.stdout
    );
    let run = cli
        .run(&["cicd", "app", "install", "--scope", "x", "--raw"])
        .await
        .ok();
    assert_eq!(run.json()["result"]["status"], "0");
}

#[tokio::test(flavor = "multi_thread")]
async fn waiting_ends_with_the_result() {
    let server = MockServer::start().await;
    on(
        &server,
        "POST",
        "/api/sn_cicd/testsuite/run",
        result(started()),
    )
    .await;
    progress(
        &server,
        json!({ "status": "2", "status_message": "done", "links": { "results": { "id": "r1" } } }),
    )
    .await;
    on(
        &server,
        "GET",
        "/api/sn_cicd/testsuite/results/r1",
        result(json!({
            "test_suite_status": "success", "test_suite_name": "Smoke",
            "rolledup_test_success_count": 3, "rolledup_test_failure_count": 0,
        })),
    )
    .await;
    let cli = Cli::basic(&server);
    let run = cli
        .run(&[
            "cicd",
            "atf",
            "run",
            "--suite-name",
            "Smoke",
            "--wait",
            "-o",
            "table",
        ])
        .await
        .ok();
    assert!(run.stderr.contains("40% Running"), "{}", run.stderr);
    assert!(
        run.stdout
            .contains("suite Smoke: success, 3 passed, 0 failed"),
        "{}",
        run.stdout
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn failures_are_errors_with_the_progress() {
    let ends: HashMap<&str, (Value, i32)> = [
        ("failed", (json!({ "status": "3", "status_message": "Install failed", "error": "no such version" }), 1)),
        ("cancelled", (json!({ "status": "4", "status_message": "stopped" }), 1)),
    ]
    .into();
    for (word, (end, code)) in ends {
        let server = MockServer::start().await;
        on(
            &server,
            "POST",
            "/api/sn_cicd/app_repo/install",
            result(started()),
        )
        .await;
        progress(&server, end.clone()).await;
        let run = Cli::basic(&server)
            .run(&[
                "cicd", "app", "install", "--scope", "x", "--wait", "--agent",
            ])
            .await;
        assert_eq!(run.code, code);
        let error = run.envelope()["error"].clone();
        assert!(
            error["message"].as_str().expect("message").contains(word),
            "{error}"
        );
        assert_eq!(error["payload"], end);
    }
    // Refused at once, under HTTP 200.
    let server = MockServer::start().await;
    on(
        &server,
        "POST",
        "/api/sn_cicd/app_repo/install",
        result(json!({ "status": "3", "error": "refused" })),
    )
    .await;
    let run = Cli::basic(&server)
        .run(&["cicd", "app", "install", "--scope", "x"])
        .await;
    assert_eq!(run.code, 1, "{}", run.stdout);
}

#[tokio::test(flavor = "multi_thread")]
async fn failed_tests_fail_even_when_the_run_ended_well() {
    let server = MockServer::start().await;
    progress(
        &server,
        json!({ "status": "2", "links": { "results": { "id": "r1" } } }),
    )
    .await;
    on(
        &server,
        "GET",
        "/api/sn_cicd/tests/test/results/r1",
        result(json!({ "test_status": "failure", "output": "step 3" })),
    )
    .await;
    let run = Cli::basic(&server)
        .run(&["cicd", "test", "watch", "--progress-id", "p1", "--agent"])
        .await;
    let error = run.envelope()["error"].clone();
    assert_eq!((run.code, &error["code"]), (1, &json!("check_failed")));
    assert_eq!(error["data"]["results"]["output"], "step 3");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_run_without_a_known_result_did_not_pass() {
    let links = json!({ "status": "2", "links": { "results": { "id": "r1" } } });
    for (end, results, part) in [
        (
            links.clone(),
            ResponseTemplate::new(403),
            "could not be read",
        ),
        (
            json!({ "status": "2" }),
            result(json!({})),
            "named no result",
        ),
        (
            links.clone(),
            result(json!({ "test_suite_name": "Smoke" })),
            "no test status",
        ),
        (
            links.clone(),
            result(json!({ "test_suite_status": "canceled" })),
            "did not pass",
        ),
        (
            links,
            result(json!({ "test_suite_status": "Cancelled" })),
            "did not pass",
        ),
    ] {
        let server = MockServer::start().await;
        progress(&server, end).await;
        on(&server, "GET", "/api/sn_cicd/testsuite/results/r1", results).await;
        let run = Cli::basic(&server)
            .run(&["cicd", "atf", "watch", "p1", "--agent"])
            .await;
        let error = run.envelope()["error"].clone();
        assert_eq!((run.code, &error["code"]), (1, &json!("check_failed")));
        assert!(error["message"].to_string().contains(part), "{error}");
        assert_eq!(error["data"]["status"], "2");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn only_tests_need_a_result_and_warnings_pass() {
    let server = MockServer::start().await;
    progress(
        &server,
        json!({ "status": "2", "links": { "results": { "id": "r1" } } }),
    )
    .await;
    on(
        &server,
        "GET",
        "/api/sn_cicd/testsuite/results/r1",
        result(json!({ "test_suite_status": "success_with_warnings" })),
    )
    .await;
    on(
        &server,
        "POST",
        "/api/sn_cicd/app/batch/install",
        result(started()),
    )
    .await;
    let cli = Cli::basic(&server);
    cli.run(&["cicd", "atf", "watch", "p1"]).await.ok();
    let run = cli
        .run(&[
            "cicd", "batch", "install", "--name", "R", "--plugin", "com.x", "--wait",
        ])
        .await
        .ok();
    assert!(run.stderr.contains("could not be read"), "{}", run.stderr);
}

#[tokio::test(flavor = "multi_thread")]
async fn waiting_has_an_end() {
    let server = MockServer::start().await;
    on(
        &server,
        "GET",
        "/api/sn_cicd/progress/p1",
        result(json!({ "status": "1", "percent_complete": 5 })),
    )
    .await;
    let run = Cli::basic(&server)
        .run(&[
            "cicd",
            "progress",
            "p1",
            "--wait",
            "--wait-timeout",
            "1",
            "--agent",
        ])
        .await;
    let error = run.envelope()["error"].clone();
    assert_eq!((run.code, &error["code"]), (5, &json!("timeout")));
    assert!(
        error["message"]
            .as_str()
            .expect("message")
            .contains("sn cicd progress p1 --wait")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn test_names_and_projects_resolve() {
    let server = MockServer::start().await;
    on(
        &server,
        "GET",
        "/api/now/table/sys_atf_test",
        result(json!([{ "sys_id": "t1", "name": "Login" }])),
    )
    .await;
    Mock::given(method("POST"))
        .respond_with(|request: &Request| result(json!({ "sent": query(request) })))
        .mount(&server)
        .await;
    let cli = Cli::basic(&server);
    let run = cli
        .run(&["cicd", "test", "run", "--test-name", "Login", "--agent"])
        .await
        .ok();
    assert_eq!(run.envelope()["data"]["sent"]["test_sys_id"], "t1");
    let run = cli
        .run(&["cicd", "test", "run", "--test-name", "Logout", "--agent"])
        .await;
    assert_eq!(run.code, 4, "the filter was ignored: {}", run.stdout);
    let sub = cli.dir.path().join("src");
    std::fs::create_dir_all(&sub).expect("dir");
    for (name, content) in [
        ("now.config.json", json!({ "scopeId": "a1" })),
        ("package.json", json!({ "version": "1.4.0" })),
    ] {
        std::fs::write(cli.dir.path().join(name), content.to_string()).expect("write");
    }
    let mut command = cli.command(&["cicd", "app", "publish", "--agent"]);
    command.current_dir(sub);
    let output = tokio::task::spawn_blocking(move || command.output().expect("run"))
        .await
        .expect("join");
    let envelope: Value = serde_json::from_slice(&output.stdout).expect("json");
    assert_eq!(
        envelope["data"]["sent"],
        json!({ "sys_id": "a1", "version": "1.4.0" })
    );
}

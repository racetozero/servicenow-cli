//! Signing in, end to end, with the real binary.

mod support;

use serde_json::json;
use support::{Cli, form, mount_me, on, query, result};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

/// The authorization endpoint sends the browser back with a code.
async fn mount_oauth(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/oauth_auth.do"))
        .respond_with(|request: &Request| {
            let q = query(request);
            assert_eq!(q["response_type"], "code");
            assert_eq!(q["code_challenge_method"], "S256");
            let to = format!("{}?code=the%2Bcode&state={}", q["redirect_uri"], q["state"]);
            ResponseTemplate::new(302).insert_header("location", to)
        })
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/oauth_token.do"))
        .respond_with(|request: &Request| {
            let f = form(request);
            assert!(request.headers.get("authorization").is_none());
            match f["grant_type"].as_str() {
                "authorization_code" => {
                    assert_eq!(f["code"], "the+code");
                    assert!(f["code_verifier"].len() >= 43);
                    ResponseTemplate::new(200).set_body_json(json!({
                        "access_token": "at1", "refresh_token": "rt1", "expires_in": 1800, "token_type": "Bearer",
                    }))
                }
                "refresh_token" if f["refresh_token"] == "rt1" => ResponseTemplate::new(200)
                    .set_body_json(json!({ "access_token": "at2", "refresh_token": "rt2", "expires_in": 1800 })),
                "client_credentials" if f["client_secret"] == "s3cret" => {
                    ResponseTemplate::new(200).set_body_json(json!({ "access_token": "svc", "expires_in": "1800" }))
                }
                _ => ResponseTemplate::new(401).set_body_json(json!({ "error": "invalid_grant" })),
            }
        })
        .mount(server)
        .await;
}

#[cfg(unix)]
fn free_port() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    listener.local_addr().expect("addr").port().to_string()
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn browser_sign_in_returns_through_the_callback() {
    let server = MockServer::start().await;
    mount_oauth(&server).await;
    mount_me(&server).await;
    let mut cli = Cli::empty();
    let browser = support::fake_browser(cli.dir.path());
    cli.env.push(("SN_BROWSER".into(), browser.clone()));
    let port = free_port();
    let args = [
        "auth",
        "login",
        "--instance",
        &server.uri(),
        "--client-id",
        "cid",
        "--port",
        &port,
        "--wait",
        "--agent",
    ];
    let run = cli.run(&args).await.ok();
    let data = &run.envelope()["data"];
    assert_eq!(data["status"], "signed_in");
    assert_eq!(data["user"]["user_name"], "ada");
    // The link is always printed, and the browser got exactly one argument.
    assert!(
        run.stderr
            .contains("/oauth_auth.do?response_type=code&client_id=cid"),
        "{}",
        run.stderr
    );
    let log = std::fs::read_to_string(format!("{browser}.log")).expect("log");
    assert!(log.starts_with("1:http"), "{log}");
    let page = std::fs::read_to_string(format!("{browser}.page")).expect("page");
    assert!(page.contains("You are signed in"), "{page}");
    // Settings and secrets are apart.
    assert!(cli.config().contains("client_id = \"cid\"") && !cli.config().contains("at1"));
    assert!(
        std::fs::read_to_string(cli.credentials())
            .expect("secrets")
            .contains("rt1")
    );
    let status = cli.run(&["auth", "status", "--agent"]).await.ok();
    assert_eq!(status.envelope()["data"]["signed_in"], true);
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn port_zero_takes_a_free_port() {
    let server = MockServer::start().await;
    mount_oauth(&server).await;
    mount_me(&server).await;
    let mut cli = Cli::empty();
    cli.env
        .push(("SN_BROWSER".into(), support::fake_browser(cli.dir.path())));
    let args = [
        "auth",
        "login",
        "-i",
        &server.uri(),
        "--client-id",
        "cid",
        "--port",
        "0",
        "--wait",
        "--agent",
    ];
    let run = cli.run(&args).await.ok();
    assert!(!run.stderr.contains("localhost%3A0%2F"), "{}", run.stderr);
}

#[tokio::test(flavor = "multi_thread")]
async fn without_a_terminal_the_link_comes_back_and_a_code_finishes() {
    let server = MockServer::start().await;
    mount_oauth(&server).await;
    mount_me(&server).await;
    let cli = Cli::empty();
    let run = cli
        .run(&["auth", "login", "--instance", &server.uri(), "--agent"])
        .await
        .ok();
    let data = run.envelope()["data"].clone();
    assert_eq!(data["status"], "authorization_required");
    let url = url::Url::parse(data["url"].as_str().expect("url")).expect("url");
    let q: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(
        q["redirect_uri"], "/sdk-oauth.do",
        "no client id: the SDK client"
    );
    let wrong = format!("http://localhost/x?code=c&state={}x", q["state"]);
    let run = cli
        .run(&["auth", "login", "--code", &wrong, "--agent"])
        .await;
    assert_eq!(
        (run.code, run.envelope()["error"]["code"].clone()),
        (2, json!("auth"))
    );
    let run = cli
        .run(&["auth", "login", "--code", "the+code", "--agent"])
        .await
        .ok();
    assert_eq!(run.envelope()["data"]["status"], "signed_in");
    assert_eq!(run.envelope()["data"]["profile"], "default");
    let again = cli
        .run(&["auth", "login", "--code", "the+code", "--agent"])
        .await;
    assert_eq!(
        again.envelope()["error"]["code"],
        "invalid",
        "a code works once"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_code_only_finishes_the_sign_in_of_its_profile() {
    let server = MockServer::start().await;
    mount_oauth(&server).await;
    mount_me(&server).await;
    let cli = Cli::empty();
    let uri = server.uri();
    for profile in ["one", "two"] {
        cli.run(&["auth", "login", "--profile", profile, "-i", &uri, "--agent"])
            .await
            .ok();
    }
    let code = ["auth", "login", "--code", "the+code", "--agent"];
    let run = cli.run(&[&code[..], &["--profile", "one"]].concat()).await;
    assert_eq!(run.envelope()["error"]["code"], "invalid");
    assert!(run.stdout.contains("profile `two`"), "{}", run.stdout);
    assert!(!cli.config().contains("profiles.two"));
    let run = cli
        .run(&[&code[..], &["--profile", "two"]].concat())
        .await
        .ok();
    assert_eq!(run.envelope()["data"]["profile"], "two");
}

#[tokio::test(flavor = "multi_thread")]
async fn another_sign_in_ends_the_one_that_waits() {
    let server = MockServer::start().await;
    mount_oauth(&server).await;
    mount_me(&server).await;
    let cli = Cli::empty();
    let uri = server.uri();
    let login = ["auth", "login", "-i", &uri, "--agent"];
    let waiting = cli.dir.path().join("pending-login.json");
    cli.run(&login).await.ok();
    let other = [
        &login[..],
        &["--profile", "other", "-u", "a", "--secret-stdin"],
    ]
    .concat();
    cli.run_with(&other, Some("pw")).await.ok();
    assert!(waiting.exists(), "it belongs to another profile");
    let same = [
        &login[..],
        &["--profile", "default", "-u", "a", "--secret-stdin"],
    ]
    .concat();
    cli.run_with(&same, Some("pw")).await.ok();
    assert!(!waiting.exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_fixed_token_is_not_refreshed() {
    let server = MockServer::start().await;
    mount_me(&server).await;
    let cli = Cli::empty();
    let uri = server.uri();
    let args = [
        "auth",
        "login",
        "-i",
        &uri,
        "-m",
        "token",
        "--secret-stdin",
        "--agent",
    ];
    cli.run_with(&args, Some("t")).await.ok();
    let run = cli.run(&["auth", "refresh", "--agent"]).await;
    assert_eq!(run.envelope()["error"]["code"], "invalid", "{}", run.stdout);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_command_that_waits_for_stdin_blocks_no_other() {
    let server = MockServer::start().await;
    mount_me(&server).await;
    let cli = Cli::empty();
    let uri = server.uri();
    let args = [
        "auth",
        "login",
        "-i",
        &uri,
        "-u",
        "a",
        "--secret-stdin",
        "--agent",
    ];
    cli.run_with(&args, Some("pw")).await.ok();
    let mut waiting = cli
        .command(&["profile", "set", "default", "--proxy-user", "u"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .spawn()
        .expect("spawn");
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    let run = cli.run(&["alias", "set", "x", "ping", "--agent"]).await;
    waiting.kill().expect("kill");
    waiting.wait().expect("wait");
    run.ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn an_instance_without_the_client_is_explained_before_a_browser_opens() {
    let server = MockServer::start().await;
    on(
        &server,
        "GET",
        "/oauth_auth.do",
        ResponseTemplate::new(401).set_body_json(json!({ "error": "invalid_client" })),
    )
    .await;
    let cli = Cli::empty();
    let run = cli
        .run(&["auth", "login", "-i", &server.uri(), "--agent"])
        .await;
    assert_eq!(run.code, 2);
    let message = run.envelope()["error"]["message"]
        .as_str()
        .expect("message")
        .to_owned();
    assert!(message.contains("sn auth register"), "{message}");
}

async fn signed_in(server: &MockServer) -> Cli {
    let cli = Cli::empty();
    cli.run(&["auth", "login", "--instance", &server.uri(), "--agent"])
        .await
        .ok();
    cli.run(&["auth", "login", "--code", "the+code", "--agent"])
        .await
        .ok();
    cli
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rejected_token_is_renewed_once_and_stored() {
    let server = MockServer::start().await;
    mount_oauth(&server).await;
    mount_me(&server).await;
    let cli = signed_in(&server).await;
    Mock::given(method("GET"))
        .and(path("/api/now/table/incident"))
        .and(header("authorization", "Bearer at1"))
        .respond_with(
            ResponseTemplate::new(401).set_body_json(json!({ "error": { "message": "expired" } })),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/now/table/incident"))
        .and(header("authorization", "Bearer at2"))
        .respond_with(result(json!([{ "number": "INC1" }])))
        .mount(&server)
        .await;
    let run = cli
        .run(&["table", "list", "incident", "--agent"])
        .await
        .ok();
    assert_eq!(run.envelope()["data"][0]["number"], "INC1");
    let secrets = std::fs::read_to_string(cli.credentials()).expect("secrets");
    assert!(
        secrets.contains("at2") && secrets.contains("rt2"),
        "the rotated refresh token is kept"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn parallel_commands_refresh_once() {
    let server = MockServer::start().await;
    mount_oauth(&server).await;
    mount_me(&server).await;
    let cli = signed_in(&server).await;
    // Expire the token on disk.
    let text = std::fs::read_to_string(cli.credentials()).expect("read");
    let at = text
        .lines()
        .find(|l| l.starts_with("expires_at"))
        .expect("expiry");
    std::fs::write(cli.credentials(), text.replace(at, "expires_at = 1")).expect("write");
    on(&server, "GET", "/api/now/table/incident", result(json!([]))).await;
    let args = ["table", "list", "incident", "--agent"];
    let (a, b, c) = tokio::join!(cli.run(&args), cli.run(&args), cli.run(&args));
    for run in [a, b, c] {
        run.ok();
    }
    let refreshes = server
        .received_requests()
        .await
        .expect("requests")
        .iter()
        .filter(|r| r.url.path() == "/oauth_token.do" && form(r)["grant_type"] == "refresh_token")
        .count();
    assert_eq!(
        refreshes, 1,
        "refresh tokens rotate, so only one process may use it"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn every_headless_method_signs_in() {
    let server = MockServer::start().await;
    mount_oauth(&server).await;
    mount_me(&server).await;
    let uri = server.uri();
    for (method, extra, secret, sent) in [
        (
            "basic",
            vec!["-u", "ada"],
            "pw",
            ("authorization", "Basic YWRhOnB3"),
        ),
        ("api-key", vec![], "key1", ("x-sn-apikey", "key1")),
        ("token", vec![], "tok1", ("authorization", "Bearer tok1")),
        (
            "client-credentials",
            vec!["--client-id", "cid"],
            "s3cret",
            ("authorization", "Bearer svc"),
        ),
        (
            "cookie",
            vec!["--user-token", "ck"],
            "JSESSIONID=a; glide=b",
            ("cookie", "JSESSIONID=a; glide=b"),
        ),
    ] {
        server.reset().await;
        mount_oauth(&server).await;
        mount_me(&server).await;
        let cli = Cli::empty();
        let mut args = vec![
            "auth",
            "login",
            "-i",
            &uri,
            "-m",
            method,
            "--secret-stdin",
            "--agent",
        ];
        args.extend(extra);
        let run = cli.run_with(&args, Some(&format!("{secret}\n"))).await.ok();
        assert_eq!(
            run.envelope()["data"]["user"]["user_name"],
            "ada",
            "{method}"
        );
        assert!(
            !run.stdout.contains(secret),
            "{method}: the secret is not printed"
        );
        let requests = server.received_requests().await.expect("requests");
        let me = requests
            .iter()
            .find(|r| r.url.path().ends_with("current_user"))
            .expect("verified");
        assert_eq!(
            me.headers.get(sent.0).map(|v| v.to_str().expect("ascii")),
            Some(sent.1),
            "{method}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn mfa_session_signs_in_through_the_login_form() {
    let server = MockServer::start().await;
    mount_me(&server).await;
    Mock::given(method("POST"))
        .and(path("/angular.do"))
        .respond_with(|request: &Request| {
            if query(request)["sysparm_type"] == "get_user" {
                return ResponseTemplate::new(401).insert_header("x-usertoken-response", "ck9");
            }
            let f = form(request);
            assert_eq!(
                (f["user_name"].as_str(), f["user_password"].as_str()),
                ("ada", "pw")
            );
            ResponseTemplate::new(200)
                .insert_header("set-cookie", "JSESSIONID=s1; Path=/; HttpOnly")
                .set_body_json(json!({ "status": "mfa_code_required" }))
        })
        .mount(&server)
        .await;
    on(&server, "GET", "/validate_multifactor_auth_code.do", ResponseTemplate::new(200).set_body_string(
        "<html><input type='hidden' name='sysparm_ck' value='c1'><input name=\"sysparm_process_nonce\" value=\"n1\"></html>",
    )).await;
    Mock::given(method("POST"))
        .and(path("/validate_mfa_code.do"))
        .respond_with(|request: &Request| {
            let f = form(request);
            assert_eq!(
                (f["txtResponse"].as_str(), f["sysparm_ck"].as_str()),
                ("123456", "c1")
            );
            ResponseTemplate::new(302)
                .insert_header("location", "/nav_to.do")
                .insert_header("set-cookie", "glide_user_session=u1; Path=/")
        })
        .mount(&server)
        .await;
    let cli = Cli::empty();
    let args = [
        "auth",
        "login",
        "-i",
        &server.uri(),
        "-u",
        "ada",
        "--mfa-code",
        "123456",
        "--secret-stdin",
        "--agent",
    ];
    cli.run_with(&args, Some("pw")).await.ok();
    let requests = server.received_requests().await.expect("requests");
    let me = requests
        .iter()
        .find(|r| r.url.path().ends_with("current_user"))
        .expect("verified");
    assert_eq!(me.headers["cookie"], "JSESSIONID=s1; glide_user_session=u1");
    assert_eq!(me.headers["x-usertoken"], "ck9");
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_token_command_runs_and_its_expiry_is_respected() {
    let server = MockServer::start().await;
    mount_me(&server).await;
    on(&server, "GET", "/api/now/table/incident", result(json!([]))).await;
    let cli = Cli::empty();
    let counter = cli.dir.path().join("n");
    let command = format!(
        "echo x >> {}; printf '{{\"access_token\":\"broker\",\"expires_in\":3600}}'",
        counter.display()
    );
    let args = [
        "auth",
        "login",
        "-i",
        &server.uri(),
        "--token-command",
        &command,
        "--agent",
    ];
    cli.run(&args).await.ok();
    cli.run(&["table", "list", "incident", "--agent"])
        .await
        .ok();
    cli.run(&["table", "list", "incident", "--agent"])
        .await
        .ok();
    // The token of the check at sign-in is kept.
    assert_eq!(
        std::fs::read_to_string(&counter)
            .expect("count")
            .lines()
            .count(),
        1
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn wrong_credentials_are_not_saved() {
    let server = MockServer::start().await;
    on(&server, "GET", "/api/now/ui/user/current_user", ResponseTemplate::new(401).set_body_json(
        json!({ "error": { "message": "User Not Authenticated", "detail": "Required to provide Auth information" } }),
    )).await;
    let cli = Cli::empty();
    let args = [
        "auth",
        "login",
        "-i",
        &server.uri(),
        "-m",
        "basic",
        "-u",
        "ada",
        "--secret-stdin",
        "--agent",
    ];
    let run = cli.run_with(&args, Some("bad")).await;
    assert_eq!(run.code, 2);
    assert_eq!(run.envelope()["error"]["code"], "auth");
    assert!(!cli.config().contains("profiles") && !cli.credentials().exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stored_credential_never_goes_to_another_instance() {
    let server = MockServer::start().await;
    mount_oauth(&server).await;
    mount_me(&server).await;
    let mut cli = signed_in(&server).await;
    cli.env.push(("SN_INSTANCE".into(), "other".into()));
    let run = cli.run(&["table", "list", "incident", "--agent"]).await;
    assert_eq!(run.envelope()["error"]["code"], "invalid");
    cli.env.clear();
    let run = cli
        .run(&["raw", "GET", "https://evil.example.com/x", "--agent"])
        .await;
    assert_eq!(run.envelope()["error"]["code"], "invalid");
}

#[tokio::test(flavor = "multi_thread")]
async fn profiles_list_switch_and_remove() {
    let server = MockServer::start().await;
    mount_me(&server).await;
    let cli = Cli::empty();
    for name in ["one", "two"] {
        let args = [
            "profile",
            "add",
            name,
            "-i",
            &server.uri(),
            "-m",
            "api-key",
            "--secret-stdin",
            "--agent",
        ];
        cli.run_with(&args, Some("k")).await.ok();
    }
    let list = cli.run(&["profile", "list", "--agent"]).await.ok();
    assert_eq!(list.envelope()["data"][0]["default"], true);
    cli.run(&["profile", "use", "two", "--agent"]).await.ok();
    let run = cli.run(&["profile", "remove", "one", "--agent"]).await;
    assert_eq!(run.envelope()["error"]["code"], "confirmation_required");
    cli.run(&["profile", "remove", "one", "--yes", "--agent"])
        .await
        .ok();
    let run = cli.run(&["profile", "use", "one", "--agent"]).await;
    assert_eq!(run.envelope()["error"]["code"], "input_required");
    assert_eq!(
        run.envelope()["error"]["input"]["choices"][0]["value"],
        "two"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn register_creates_a_public_client() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/now/table/oauth_entity"))
        .respond_with(|request: &Request| {
            let body: serde_json::Value = serde_json::from_slice(&request.body).expect("json");
            assert_eq!(body["public_client"], "true");
            assert_eq!(
                body["redirect_url"],
                "http://localhost:8400/callback,http://127.0.0.1:8400/callback"
            );
            result(json!({ "sys_id": "1", "client_id": "newid", "public_client": "true" }))
        })
        .mount(&server)
        .await;
    let cli = Cli::basic(&server);
    let run = cli.run(&["auth", "register", "--agent"]).await;
    assert_eq!(run.envelope()["error"]["code"], "confirmation_required");
    let run = cli
        .run(&["auth", "register", "--yes", "--agent"])
        .await
        .ok();
    assert_eq!(run.envelope()["data"]["client_id"], "newid");
}

#[tokio::test(flavor = "multi_thread")]
async fn logout_revokes_the_token_and_keeps_the_settings() {
    let server = MockServer::start().await;
    mount_oauth(&server).await;
    mount_me(&server).await;
    on(
        &server,
        "GET",
        "/oauth_revoke.do",
        ResponseTemplate::new(200),
    )
    .await;
    let cli = signed_in(&server).await;
    let bearer = cli
        .run(&["auth", "token", "--as", "bearer", "--agent"])
        .await
        .ok();
    assert_eq!(bearer.envelope()["data"], "at1");
    cli.run(&["profile", "refresh", "--agent"]).await.ok();
    let shown = cli.run(&["profile", "show", "--agent"]).await.ok();
    assert_eq!(shown.envelope()["data"]["credential"], true);
    assert!(!shown.stdout.contains("at2") && !shown.stdout.contains("rt2"));
    let status = cli.run(&["profile", "status", "--agent"]).await.ok();
    assert_eq!(status.envelope()["data"]["user"]["user_name"], "ada");
    let run = cli.run(&["auth", "logout", "--agent"]).await.ok();
    assert_eq!(run.envelope()["data"]["revoked"], true);
    let revoked: Vec<String> = server
        .received_requests()
        .await
        .expect("requests")
        .iter()
        .filter(|r| r.url.path() == "/oauth_revoke.do")
        .map(|r| query(r)["token"].clone())
        .collect();
    assert_eq!(revoked, ["rt2", "at2"]);
    assert!(
        cli.config().contains("profiles.default")
            && !std::fs::read_to_string(cli.credentials())
                .expect("file")
                .contains("rt2")
    );
    let run = cli.run(&["table", "list", "incident", "--agent"]).await;
    assert_eq!(run.envelope()["error"]["code"], "auth");
    let run = cli.run(&["config", "show", "--agent"]).await.ok();
    assert_eq!(run.envelope()["data"]["profiles"][0]["name"], "default");
    let run = cli.run(&["config", "path"]).await.ok();
    assert!(run.stdout.trim().ends_with("config.toml"));
}

#[tokio::test(flavor = "multi_thread")]
async fn aliases_run_their_command() {
    let server = MockServer::start().await;
    on(
        &server,
        "GET",
        "/api/now/table/incident",
        result(json!([{ "number": "INC1" }])),
    )
    .await;
    let cli = Cli::basic(&server);
    cli.run(&[
        "alias",
        "set",
        "inc",
        "table list incident -q 'a=1^b=x y'",
        "--agent",
    ])
    .await
    .ok();
    let run = cli.run(&["inc", "--agent"]).await.ok();
    assert_eq!(run.envelope()["command"], "table list");
    let run = cli.run(&["alias", "set", "table", "ping", "--agent"]).await;
    assert_eq!(run.envelope()["error"]["code"], "invalid");
    cli.run(&["alias", "delete", "inc", "--agent"]).await.ok();
}

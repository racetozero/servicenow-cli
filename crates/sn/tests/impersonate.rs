//! `impersonate USER -- COMMAND`: the command runs on the impersonated
//! session alone, and the impersonation ends however the command ends.

mod support;

use serde_json::json;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use support::{Cli, query};
use wiremock::matchers::any;
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const REAL: &str = "admin";
const REAL_ID: &str = "218517762f7903107efd1d707fa4e3de";
const ABEL: &str = "abel.tuter";
const ABEL_ID: &str = "62826bf03710200044e0bfc8bcbe5df1";
const SESSION: &str = "SESSIONCOOKIE123";
/// Must never reach stdout or stderr.
const TOKEN: &str = "LEAKYSESSIONTOKEN0xC5RF";

#[derive(Default)]
struct State {
    current: Option<&'static str>,
    logged_out: bool,
    /// Requests after the first that still had the profile's credential.
    credentialed: u32,
    events: Vec<String>,
}

#[derive(Clone)]
struct Instance {
    state: Arc<Mutex<State>>,
    can_impersonate: bool,
    /// Answer 201 without switching, as a live instance does.
    ignores: bool,
    delay: Duration,
    logout_delay: Duration,
    incident_status: u16,
}

impl Instance {
    fn new() -> Self {
        Self {
            state: Arc::default(),
            can_impersonate: true,
            ignores: false,
            delay: Duration::ZERO,
            logout_delay: Duration::ZERO,
            incident_status: 200,
        }
    }

    async fn serve(&self) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(any())
            .respond_with(self.clone())
            .mount(&server)
            .await;
        server
    }

    fn events(&self) -> Vec<String> {
        self.state.lock().expect("lock").events.clone()
    }
}

impl Respond for Instance {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let mut s = self.state.lock().expect("lock");
        let path = request.url.path().to_owned();
        let header = |name: &str| request.headers.get(name).and_then(|v| v.to_str().ok());
        let session = |current: &str| {
            json!({
                "CurrentUser": current, "OriginalUser": REAL, "SessionToken": TOKEN,
                "CanImpersonate": self.can_impersonate,
            })
        };
        let credentialed = header("authorization").is_some();
        if credentialed && path == "/api/now/sg/impersonation/session" {
            s.current = Some(REAL);
            s.logged_out = false;
            s.events.push("mint".into());
            return ResponseTemplate::new(200)
                .append_header("set-cookie", format!("JSESSIONID={SESSION}; Path=/"))
                .append_header("set-cookie", "glide_user_route=node1; Path=/")
                .set_body_json(session(REAL));
        }
        s.credentialed += u32::from(credentialed);
        let has_session =
            header("cookie").is_some_and(|c| c.contains(&format!("JSESSIONID={SESSION}")));
        if !has_session || s.logged_out {
            s.events.push(format!("401 {path}"));
            return ResponseTemplate::new(401)
                .set_body_json(json!({ "error": { "message": "User is not authenticated" } }));
        }
        let current = s.current.unwrap_or(REAL);
        match (request.method.as_str(), path.as_str()) {
            ("GET", "/api/now/sg/impersonation/session") => {
                s.events.push(format!("session {current}"));
                ResponseTemplate::new(200).set_body_json(session(current))
            }
            ("GET", "/api/now/table/sys_user") => {
                let row = |id: &str, name: &str| json!({ "sys_id": id, "user_name": name });
                let rows = match query(request)["sysparm_query"].as_str() {
                    "user_name=admin" => json!([row(REAL_ID, REAL)]),
                    "user_name=everyone" => json!([row(REAL_ID, REAL), row(ABEL_ID, ABEL)]),
                    "user_name=abel.tuter" => json!([row(ABEL_ID, ABEL)]),
                    q if q == format!("sys_id={ABEL_ID}") => json!([row(ABEL_ID, ABEL)]),
                    _ => json!([]),
                };
                support::result(rows)
            }
            ("POST", p) if p.starts_with("/api/now/ui/impersonate/") => {
                assert_eq!(header("x-usertoken"), Some(TOKEN));
                let to = if p.ends_with(ABEL_ID) { ABEL } else { REAL };
                s.events.push(format!("impersonate {to}"));
                if !self.ignores {
                    s.current = Some(to);
                }
                ResponseTemplate::new(201).set_body_json(json!({ "result": {} }))
            }
            ("GET", "/logout.do") => {
                s.logged_out = true;
                s.events.push("logout".into());
                // The page also deletes the cookies.
                ResponseTemplate::new(200)
                    .append_header("set-cookie", "JSESSIONID=; Max-Age=0")
                    .set_body_string("<html>logged out</html>")
                    .set_delay(self.logout_delay)
            }
            (verb, "/api/now/table/incident") => {
                s.events.push(format!("{verb} incident as {current}"));
                ResponseTemplate::new(self.incident_status)
                    .set_body_json(json!({ "result": [{ "number": format!("INC-{current}") }] }))
                    .set_delay(self.delay)
            }
            other => panic!("unexpected request {other:?}"),
        }
    }
}

const LIST: [&str; 6] = ["impersonate", ABEL, "--", "table", "list", "incident"];

fn assert_ended(events: &[String]) {
    assert_eq!(
        events[events.len() - 3..],
        [
            "impersonate admin",
            "logout",
            "401 /api/now/sg/impersonation/session"
        ],
        "{events:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_command_runs_as_the_user_on_the_session_alone() {
    let instance = Instance::new();
    let server = instance.serve().await;
    let run = Cli::basic(&server)
        .run(&[&["-vv", "--agent"], &LIST[..]].concat())
        .await
        .ok();
    let envelope = run.envelope();
    assert_eq!(
        (&envelope["command"], &envelope["data"][0]["number"]),
        (&json!("table list"), &json!("INC-abel.tuter"))
    );
    let events = instance.events();
    assert_eq!(
        events[..4],
        [
            "mint",
            "impersonate abel.tuter",
            "session abel.tuter",
            "GET incident as abel.tuter"
        ]
    );
    assert_ended(&events);
    assert_eq!(instance.state.lock().expect("lock").credentialed, 0);
    for secret in [TOKEN, SESSION] {
        assert!(
            !run.stdout.contains(secret) && !run.stderr.contains(secret),
            "{}",
            run.stderr
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn sys_ids_dry_runs_and_output_options_work() {
    let instance = Instance::new();
    let server = instance.serve().await;
    let cli = Cli::basic(&server);
    let run = cli
        .run(&[
            "impersonate",
            ABEL_ID,
            "--raw",
            "--",
            "sn",
            "table",
            "list",
            "incident",
        ])
        .await
        .ok();
    assert_eq!(run.json()["result"][0]["number"], "INC-abel.tuter");
    let run = cli
        .run(&[
            "impersonate",
            ABEL,
            "--dry-run",
            "--",
            "table",
            "create",
            "incident",
            "-F",
            "a=b",
        ])
        .await
        .ok();
    assert_eq!(run.json()["method"], "POST");
    let events = instance.events();
    assert!(
        !events.iter().any(|e| e.starts_with("POST incident")),
        "{events:?}"
    );
    assert_ended(&events);
}

#[tokio::test(flavor = "multi_thread")]
async fn safety_options_after_the_dashes_stop_the_write() {
    let instance = Instance::new();
    let server = instance.serve().await;
    let cli = Cli::basic(&server);
    let create = [
        "impersonate",
        ABEL,
        "--",
        "table",
        "create",
        "incident",
        "-F",
        "a=b",
    ];
    let run = cli.run(&[&create[..], &["--dry-run"]].concat()).await.ok();
    assert_eq!(run.json()["method"], "POST");
    let run = cli.run(&[&create[..], &["--read-only"]].concat()).await;
    assert_eq!(run.code, 3, "{}", run.stderr);
    let run = cli.run(&[&create[..], &["-v"]].concat()).await.ok();
    assert!(run.stderr.contains("> POST"), "{}", run.stderr);
    let events = instance.events();
    let writes = events.iter().filter(|e| e.starts_with("POST incident"));
    assert_eq!(writes.count(), 1, "only the last one: {events:?}");
    assert_ended(&events);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failing_command_keeps_its_exit_code_and_ends_the_session() {
    let mut instance = Instance::new();
    instance.incident_status = 403;
    let server = instance.serve().await;
    let run = Cli::basic(&server).run(&LIST).await;
    assert_eq!(run.code, 2, "{}", run.stderr);
    assert_ended(&instance.events());
}

#[tokio::test(flavor = "multi_thread")]
async fn refusals_never_run_the_command() {
    let mut instance = Instance::new();
    instance.can_impersonate = false;
    let server = instance.serve().await;
    let run = Cli::basic(&server)
        .run(&[&["--agent"], &LIST[..]].concat())
        .await;
    assert_eq!(
        (run.code, run.envelope()["error"]["code"].clone()),
        (2, json!("permission"))
    );
    let events = instance.events();
    assert!(
        !events.iter().any(|e| e.contains("impersonate ")),
        "{events:?}"
    );
    assert!(events.contains(&"logout".to_owned()));

    // The instance says 201 and does nothing.
    let mut instance = Instance::new();
    instance.ignores = true;
    let server = instance.serve().await;
    let run = Cli::basic(&server).run(&LIST).await;
    assert_eq!(run.code, 1, "{}", run.stderr);
    assert!(run.stderr.contains("did not switch"), "{}", run.stderr);
    let events = instance.events();
    assert!(!events.iter().any(|e| e.contains("incident")), "{events:?}");
    assert_ended(&events);

    let instance = Instance::new();
    let server = instance.serve().await;
    for (user, code) in [("nobody", 4), ("everyone", 1)] {
        let run = Cli::basic(&server)
            .run(&["impersonate", user, "--", "ping"])
            .await;
        assert_eq!(run.code, code, "{user}: {}", run.stderr);
    }
    let events = instance.events();
    assert!(!events.iter().any(|e| e.contains("impersonate ")));
}

#[tokio::test(flavor = "multi_thread")]
async fn wrong_arguments_send_nothing() {
    let instance = Instance::new();
    let server = instance.serve().await;
    let cli = Cli::basic(&server);
    for (args, needle) in [
        (&["profile", "list"][..], "own sign-in"),
        (&["watch", "incident", "-q", "a=1"], "new sessions"),
        (&["flow", "tail", "f"], "new sessions"),
        (&["cache", "refresh"], "offline copy"),
        (&["impersonate", "x", "--", "ping"], "do not nest"),
        (&["open", "incident"], "browser"),
        (&["exec", "gs.info(1)", "--yes"], "session of their own"),
        (&["ping", "--profile", "p"], "--profile after --"),
        (&["ping", "--timeout", "5"], "--timeout after --"),
        (&["ping", "--agent"], "--agent after --"),
        (&["frobnicate"], "frobnicate"),
        (&["sn"], "no command"),
    ] {
        let run = cli
            .run(&[&["impersonate", ABEL, "--"], args].concat())
            .await;
        assert_eq!(run.code, 3, "{args:?}: {}", run.stderr);
        assert!(run.stderr.contains(needle), "{args:?}: {}", run.stderr);
    }
    assert_eq!(cli.run(&["impersonate", ABEL]).await.code, 3);
    assert!(instance.events().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_shared_browser_session_is_never_switched() {
    let instance = Instance::new();
    let server = instance.serve().await;
    let mut cli = Cli::empty();
    cli.env = vec![
        ("SN_INSTANCE".into(), server.uri()),
        ("SN_COOKIE".into(), format!("JSESSIONID={SESSION}")),
    ];
    let run = cli.run(&LIST).await;
    assert_eq!(run.code, 3, "{}", run.stderr);
    assert!(instance.events().is_empty());
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn an_interrupt_ends_the_session() {
    let mut instance = Instance::new();
    instance.delay = Duration::from_secs(20);
    let server = instance.serve().await;
    let child = Cli::basic(&server)
        .command(&LIST)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn");
    let started = std::time::Instant::now();
    while !instance.events().iter().any(|e| e.contains("incident")) {
        assert!(
            started.elapsed() < Duration::from_secs(15),
            "{:?}",
            instance.events()
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    std::process::Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .expect("kill");
    let output = tokio::task::spawn_blocking(move || child.wait_with_output().expect("wait"))
        .await
        .expect("join");
    assert_eq!(
        output.status.code(),
        Some(130),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(started.elapsed() < Duration::from_secs(15));
    assert_ended(&instance.events());
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_second_interrupt_does_not_wait_for_the_instance() {
    let mut instance = Instance::new();
    instance.logout_delay = Duration::from_secs(20);
    let server = instance.serve().await;
    let child = Cli::basic(&server)
        .command(&LIST)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn");
    let started = std::time::Instant::now();
    while !instance.events().contains(&"logout".to_owned()) {
        assert!(started.elapsed() < Duration::from_secs(15));
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    std::process::Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .expect("kill");
    let output = tokio::task::spawn_blocking(move || child.wait_with_output().expect("wait"))
        .await
        .expect("join");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("you pressed Ctrl-C before the instance confirmed"),
        "{stderr}"
    );
    assert!(started.elapsed() < Duration::from_secs(15));
}

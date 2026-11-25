//! Runs the real binary against a fake instance.
#![allow(dead_code)]

use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

pub const ME: &str = "6816f79cc0a8016401c5a33be04be441";

/// A temporary configuration directory and the instance it points to.
pub struct Cli {
    pub dir: tempfile::TempDir,
    pub env: Vec<(String, String)>,
}

pub struct Run {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Run {
    pub fn json(&self) -> Value {
        serde_json::from_str(self.stdout.trim()).unwrap_or_else(|err| {
            panic!(
                "stdout is not JSON ({err}): {}\nstderr: {}",
                self.stdout, self.stderr
            )
        })
    }

    /// The one envelope of agent mode.
    pub fn envelope(&self) -> Value {
        let lines: Vec<&str> = self.stdout.lines().collect();
        assert_eq!(
            lines.len(),
            1,
            "agent mode prints one line\nstdout: {}\nstderr: {}",
            self.stdout,
            self.stderr
        );
        self.json()
    }

    pub fn ok(self) -> Self {
        assert_eq!(
            self.code, 0,
            "stdout: {}\nstderr: {}",
            self.stdout, self.stderr
        );
        self
    }
}

impl Cli {
    /// No profile at all.
    pub fn empty() -> Self {
        Self {
            dir: tempfile::tempdir().expect("dir"),
            env: Vec::new(),
        }
    }

    /// Basic sign-in to `server` through the environment.
    pub fn basic(server: &MockServer) -> Self {
        let mut cli = Self::empty();
        for (name, value) in [
            ("SN_INSTANCE", server.uri()),
            ("SN_USERNAME", "admin".into()),
            ("SN_PASSWORD", "pw".into()),
        ] {
            cli.env.push((name.into(), value));
        }
        cli
    }

    pub fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_sn"));
        for (name, _) in std::env::vars() {
            if name.starts_with("SN_") {
                command.env_remove(name);
            }
        }
        command
            .env("SN_CONFIG_DIR", self.dir.path())
            .env("SN_BROWSER", "none")
            .envs(self.env.iter().cloned())
            .args(args);
        command
    }

    /// Runs with stdin closed.
    pub async fn run(&self, args: &[&str]) -> Run {
        self.run_with(args, None).await
    }

    pub async fn run_with(&self, args: &[&str], stdin: Option<&str>) -> Run {
        let mut command = self.command(args);
        command.stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
        let stdin = stdin.map(str::to_owned);
        let output: Output = tokio::task::spawn_blocking(move || {
            let mut child = command.spawn().expect("spawn");
            if let Some(text) = stdin {
                use std::io::Write;
                let _ = child
                    .stdin
                    .take()
                    .expect("stdin")
                    .write_all(text.as_bytes());
            }
            child.wait_with_output().expect("wait")
        })
        .await
        .expect("join");
        Run {
            code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }

    pub fn config(&self) -> String {
        std::fs::read_to_string(self.dir.path().join("config.toml")).unwrap_or_default()
    }

    pub fn credentials(&self) -> PathBuf {
        self.dir.path().join("credentials.toml")
    }
}

pub fn result(value: Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({ "result": value }))
}

pub async fn on(server: &MockServer, verb: &str, at: &str, answer: ResponseTemplate) {
    Mock::given(method(verb))
        .and(path(at))
        .respond_with(answer)
        .mount(server)
        .await;
}

/// The endpoints that say who the caller is.
pub async fn mount_me(server: &MockServer) {
    on(
        server,
        "GET",
        "/api/now/ui/user/current_user",
        result(json!({ "user_sys_id": ME })),
    )
    .await;
    on(
        server,
        "GET",
        &format!("/api/now/table/sys_user/{ME}"),
        result(json!({ "sys_id": ME, "user_name": "ada", "name": "Ada Lovelace", "email": "ada@example.com" })),
    )
    .await;
}

/// The endpoint that names the caller, with the user name.
pub async fn mount_caller(server: &MockServer) {
    on(
        server,
        "GET",
        "/api/now/ui/user/current_user",
        result(json!({ "user_sys_id": ME, "user_name": "ada" })),
    )
    .await;
}

/// Answers each GraphQL document, given its whole body, with what `pick`
/// returns.
pub async fn graphql(
    server: &MockServer,
    pick: impl Fn(&str, &Value) -> Value + Send + Sync + 'static,
) {
    Mock::given(method("POST"))
        .and(path("/api/now/graphql"))
        .respond_with(move |request: &Request| {
            let body: Value = serde_json::from_slice(&request.body).expect("body");
            let answer = pick(body["query"].as_str().expect("query"), &body);
            ResponseTemplate::new(200).set_body_json(answer)
        })
        .mount(server)
        .await;
}

pub async fn assert_untouched(server: &MockServer) {
    let requests = server.received_requests().await.expect("requests");
    assert!(requests.is_empty(), "{requests:?}");
}

/// A field with `--display-value all`.
pub fn pair(value: &str, label: &str) -> Value {
    json!({ "value": value, "display_value": label })
}

pub fn form(request: &Request) -> std::collections::HashMap<String, String> {
    url::form_urlencoded::parse(&request.body)
        .into_owned()
        .collect()
}

pub fn query(request: &Request) -> std::collections::HashMap<String, String> {
    request.url.query_pairs().into_owned().collect()
}

/// A browser: follows redirects like a person who is signed in already.
#[cfg(unix)]
pub fn fake_browser(dir: &std::path::Path) -> String {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("browser.sh");
    std::fs::write(
        &path,
        "#!/bin/sh\necho \"$#:$1\" >> \"$0.log\"\ncurl -sL -o \"$0.page\" \"$1\"\n",
    )
    .expect("script");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("mode");
    path.display().to_string()
}

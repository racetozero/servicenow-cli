//! Record watchers against a fake message bus.

mod support;

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use support::Cli;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};

/// What the bus saw.
#[derive(Default)]
struct Seen {
    /// `origin` and `cookie` of each upgrade.
    upgrades: Vec<(String, String)>,
    subscriptions: Vec<String>,
    disconnects: usize,
    connects: usize,
    tunnels: usize,
}

#[derive(Clone)]
struct Bus {
    seen: Arc<Mutex<Seen>>,
    address: String,
    /// The events of each websocket; the connection closes after them
    /// unless it is the last one.
    sessions: Arc<Vec<Vec<Value>>>,
    status: &'static str,
    subscribe: Subscribe,
}

/// What the bus does with a subscription.
#[derive(Clone, Copy, PartialEq)]
enum Subscribe {
    Accept,
    Refuse,
    Ignore,
}

impl Bus {
    async fn start(sessions: Vec<Vec<Value>>) -> Self {
        Self::start_with(sessions, "session.logged.in", Subscribe::Accept).await
    }

    async fn start_with(
        sessions: Vec<Vec<Value>>,
        status: &'static str,
        subscribe: Subscribe,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let bus = Self {
            seen: Arc::default(),
            address: listener.local_addr().expect("address").to_string(),
            sessions: Arc::new(sessions),
            status,
            subscribe,
        };
        let shared = bus.clone();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let bus = shared.clone();
                tokio::spawn(async move { bus.serve(stream).await });
            }
        });
        bus
    }

    fn cli(&self) -> Cli {
        let mut cli = Cli::empty();
        for (name, value) in [
            ("SN_INSTANCE", format!("http://{}", self.address)),
            ("SN_USERNAME", "admin".into()),
            ("SN_PASSWORD", "pw".into()),
            ("NO_PROXY", "127.0.0.1".into()),
        ] {
            cli.env.push((name.into(), value));
        }
        cli
    }

    async fn serve(self, mut stream: TcpStream) {
        loop {
            let mut head = vec![0; 8192];
            let length = stream.peek(&mut head).await.unwrap_or(0);
            let text = String::from_utf8_lossy(&head[..length]).into_owned();
            let Some(end) = text.find("\r\n\r\n") else {
                if length == 0 {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
                continue;
            };
            let line = text.lines().next().unwrap_or_default().to_owned();
            if line.contains("/amb ") {
                return self.websocket(stream).await;
            }
            let mut read = vec![0; end + 4];
            stream.read_exact(&mut read).await.expect("request");
            if line.starts_with("CONNECT ") {
                self.seen.lock().expect("lock").tunnels += 1;
                stream
                    .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
                    .await
                    .expect("tunnel");
                continue;
            }
            assert!(
                line.contains("/api/now/sg/impersonation/session"),
                "unexpected request {line}"
            );
            let count = self.seen.lock().expect("lock").upgrades.len();
            let body = json!({ "CurrentUser": "admin", "SessionToken": "t" }).to_string();
            let answer = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\nset-cookie: JSESSIONID=S{count}; Path=/\r\nset-cookie: glide_user_route=node1; Path=/\r\ncontent-length: {}\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(answer.as_bytes()).await.expect("answer");
        }
    }

    #[allow(clippy::result_large_err)]
    async fn websocket(self, stream: TcpStream) {
        let mut index = 0;
        let seen = Arc::clone(&self.seen);
        let mut socket =
            tokio_tungstenite::accept_hdr_async(stream, |request: &Request, response: Response| {
                let header = |name: &str| {
                    request
                        .headers()
                        .get(name)
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or_default()
                        .to_owned()
                };
                let mut seen = seen.lock().expect("lock");
                index = seen.upgrades.len();
                seen.upgrades.push((header("origin"), header("cookie")));
                Ok(response)
            })
            .await
            .expect("upgrade");
        let mut armed = 0;
        while let Some(Ok(Message::Text(text))) = socket.next().await {
            let messages: Vec<Value> = serde_json::from_str(&text).expect("messages");
            for message in messages {
                let channel = message["channel"].as_str().expect("channel").to_owned();
                let mut answers = Vec::new();
                match channel.as_str() {
                    "/meta/handshake" => answers.push(json!({
                        "channel": channel, "successful": true, "clientId": "c1",
                        "ext": { "glide.session.status": self.status },
                    })),
                    "/meta/connect" => {
                        self.seen.lock().expect("lock").connects += 1;
                        armed += 1;
                        // Only the first one is answered at once.
                        if armed == 1 {
                            answers.push(json!({ "channel": channel, "successful": true }));
                        }
                    }
                    "/meta/subscribe" => {
                        let name = message["subscription"].as_str().expect("name").to_owned();
                        self.seen
                            .lock()
                            .expect("lock")
                            .subscriptions
                            .push(name.clone());
                        match self.subscribe {
                            Subscribe::Ignore => {}
                            Subscribe::Refuse => answers.push(json!({ "channel": channel, "successful": false, "error": "404::message_deleted" })),
                            Subscribe::Accept => {
                                answers.push(json!({ "channel": channel, "successful": true }));
                                // Events may share the frame of the answer.
                                for event in self.sessions.get(index).into_iter().flatten() {
                                    answers.push(json!({ "channel": name, "data": event }));
                                }
                            }
                        }
                    }
                    "/meta/disconnect" => self.seen.lock().expect("lock").disconnects += 1,
                    other => panic!("unexpected channel {other}"),
                }
                if !answers.is_empty() {
                    let text = Value::Array(answers).to_string();
                    let _ = socket.send(Message::text(text)).await;
                }
                if channel == "/meta/subscribe" && index + 1 < self.sessions.len() {
                    let _ = socket.close(None).await;
                    return;
                }
            }
        }
    }
}

fn event(operation: &str, changes: &[&str]) -> Value {
    json!({ "operation": operation, "action": "change", "sys_id": "a1", "changes": changes, "record": {} })
}

#[tokio::test(flavor = "multi_thread")]
async fn streams_the_events_that_match_and_leaves() {
    let bus = Bus::start(vec![vec![
        event("insert", &["number"]),
        event("update", &["state"]),
        event("update", &["priority"]),
        event("update", &["state"]),
    ]])
    .await;
    let run = bus
        .cli()
        .run(&[
            "watch",
            "incident",
            "-q",
            "active=true",
            "--operation",
            "update",
            "--on-change",
            "state",
            "--max-events",
            "2",
            "--agent",
        ])
        .await
        .ok();
    let lines: Vec<Value> = run
        .stdout
        .lines()
        .map(|l| serde_json::from_str(l).expect("envelope"))
        .collect();
    assert_eq!(lines.len(), 2, "{}", run.stdout);
    assert_eq!(lines[0]["command"], "watch");
    assert_eq!(lines[0]["data"], event("update", &["state"]));
    let seen = bus.seen.lock().expect("lock");
    assert_eq!(
        seen.upgrades,
        [(
            format!("http://{}", bus.address),
            "JSESSIONID=S0; glide_user_route=node1".to_owned()
        )]
    );
    assert_eq!(
        seen.subscriptions,
        ["/rw/default/incident/YWN0aXZlPXRydWU-"]
    );
    assert_eq!(seen.disconnects, 1);
    assert_eq!(seen.connects, 2, "the long poll is armed");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_new_session_follows_a_closed_one_with_a_marker() {
    let bus = Bus::start(vec![
        vec![event("insert", &["number"])],
        vec![event("update", &["state"])],
    ])
    .await;
    let run = bus
        .cli()
        .run(&[
            "watch",
            "incident",
            "-q",
            "active=true",
            "--max-events",
            "2",
        ])
        .await
        .ok();
    let lines: Vec<Value> = run
        .stdout
        .lines()
        .map(|l| serde_json::from_str(l).expect("line"))
        .collect();
    assert_eq!(lines.len(), 3, "{}", run.stdout);
    assert_eq!(
        (&lines[1]["sn_watch"], &lines[1]["attempt"]),
        (&json!("reconnected"), &json!(1))
    );
    let seen = bus.seen.lock().expect("lock");
    assert_eq!(
        seen.upgrades[1].1, "JSESSIONID=S1; glide_user_route=node1",
        "a new session for a new connection"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn refusals_are_errors_at_once() {
    let bus = Bus::start_with(vec![vec![]], "session.invalidated", Subscribe::Accept).await;
    let args = ["watch", "incident", "-q", "active=true", "--agent"];
    let run = bus.cli().run(&args).await;
    assert_eq!(
        (run.code, run.envelope()["error"]["code"].clone()),
        (2, json!("auth"))
    );
    let bus = Bus::start_with(vec![vec![]], "session.logged.in", Subscribe::Refuse).await;
    let run = bus.cli().run(&args).await;
    assert_eq!(run.code, 1);
    assert!(
        run.stdout.contains("404::message_deleted"),
        "{}",
        run.stdout
    );
    let bus = Bus::start_with(vec![vec![]], "session.logged.in", Subscribe::Ignore).await;
    let run = bus
        .cli()
        .run(&[&args[..], &["--timeout", "1"]].concat())
        .await;
    assert_eq!(
        (run.code, run.envelope()["error"]["code"].clone()),
        (5, json!("network"))
    );
}

/// A line longer than the buffer of stdout fails in another way than a short
/// one.
#[tokio::test(flavor = "multi_thread")]
async fn a_reader_that_leaves_ends_the_stream() {
    for size in [1, 5000] {
        let mut large = event("update", &["state"]);
        large["record"] = "x".repeat(size).into();
        let bus = Bus::start(vec![vec![large; 100_000 / size.max(100)]]).await;
        let mut child = bus
            .cli()
            .command(&["watch", "incident", "-q", "active=true", "--duration", "20"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn");
        let started = std::time::Instant::now();
        let status = tokio::task::spawn_blocking(move || {
            let mut out = child.stdout.take().expect("stdout");
            std::io::Read::read_exact(&mut out, &mut [0; 10]).expect("read");
            drop(out);
            child.wait().expect("wait")
        })
        .await
        .expect("join");
        assert_eq!(status.code(), Some(0));
        assert!(started.elapsed() < Duration::from_secs(10), "{size}");
        assert_eq!(bus.seen.lock().expect("lock").disconnects, 1);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn wrong_arguments_never_connect() {
    let cli = Cli::empty();
    let run = cli.run(&["watch", "incident", "--agent"]).await;
    assert_eq!(
        (run.code, run.envelope()["error"]["input"]["arg"].clone()),
        (3, json!("query"))
    );
    let run = cli
        .run(&["watch", "incident:INC1", "-q", "a=1", "--agent"])
        .await;
    assert_eq!(run.code, 3);
    let run = cli.run(&["watch", "a/b", "-q", "a=1", "--agent"]).await;
    assert_eq!(run.code, 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn proxies_are_tunnelled_or_refused() {
    let bus = Bus::start(vec![vec![event("insert", &["number"])]]).await;
    let mut cli = bus.cli();
    cli.env.retain(|(name, _)| name != "NO_PROXY");
    let args = [
        "watch",
        "incident",
        "-q",
        "active=true",
        "--max-events",
        "1",
        "--agent",
    ];
    let proxy = format!("http://{}", bus.address);
    let run = cli
        .run(&[&args[..], &["--proxy", &proxy]].concat())
        .await
        .ok();
    assert_eq!(run.envelope()["data"]["operation"], "insert");
    assert_eq!(bus.seen.lock().expect("lock").tunnels, 1);
    let run = cli
        .run(&[&args[..], &["--proxy", "socks5://127.0.0.1:9"]].concat())
        .await;
    assert_eq!(run.code, 3);
    assert!(
        run.stdout.contains("only through http:// proxies"),
        "{}",
        run.stdout
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn an_interrupt_leaves_the_bus_and_is_no_error() {
    let bus = Bus::start(vec![vec![event("insert", &["number"])]]).await;
    let mut child = bus
        .cli()
        .command(&["watch", "incident", "-q", "active=true"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn");
    for _ in 0..200 {
        if bus.seen.lock().expect("lock").connects >= 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    std::process::Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .expect("kill");
    let status = tokio::task::spawn_blocking(move || child.wait().expect("wait"))
        .await
        .expect("join");
    assert_eq!(status.code(), Some(0));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(bus.seen.lock().expect("lock").disconnects, 1);
}

//! Promises that hold for every command, checked over the whole tree.

mod support;

use serde_json::{Value, json};
use support::Cli;
use wiremock::matchers::any;
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Every command that runs, with the arguments it requires.
fn leaves(command: &Value, path: &[String], out: &mut Vec<Vec<String>>) {
    let args: Vec<&Value> = command["args"].as_array().into_iter().flatten().collect();
    if let Some(subs) = command["commands"].as_array() {
        for sub in subs {
            let mut next = path.to_vec();
            next.push(sub["name"].as_str().expect("name").to_owned());
            leaves(sub, &next, out);
        }
        return;
    }
    let mut argv = path.to_vec();
    for arg in args
        .iter()
        .filter(|a| a["required"] == true && a["flag"].is_null())
    {
        let first = arg["values"][0].as_str().unwrap_or("incident");
        argv.push(first.to_owned());
    }
    for arg in args
        .iter()
        .filter(|a| a["required"] == true && !a["flag"].is_null())
    {
        argv.push(arg["flag"].as_str().expect("flag").to_owned());
        argv.push(arg["values"][0].as_str().unwrap_or("x").to_owned());
    }
    out.push(argv);
}

/// What `--help` and `introspect` say that a command does.
async fn effects(cli: &Cli) -> std::collections::HashMap<String, String> {
    let run = cli.run(&["introspect", "--brief", "--agent"]).await.ok();
    run.envelope()["data"]
        .as_array()
        .expect("rows")
        .iter()
        .map(|row| {
            let text = |name: &str| row[name].as_str().unwrap_or_default().to_owned();
            (text("command"), text("effect"))
        })
        .collect()
}

async fn all_commands(cli: &Cli) -> Vec<Vec<String>> {
    let tree = cli.run(&["introspect", "--agent"]).await.ok().envelope()["data"].clone();
    let mut out = Vec::new();
    leaves(&tree, &[], &mut out);
    out
}

/// One JSON envelope on stdout, the documented exit code, and an end: no
/// question, no read from stdin, no full-screen view.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_command_answers_agents_with_one_envelope() {
    let server = MockServer::start().await;
    Mock::given(any())
        .respond_with(
            ResponseTemplate::new(404)
                .set_body_json(json!({ "error": { "message": "No such thing" } })),
        )
        .mount(&server)
        .await;
    let cli = Cli::basic(&server);
    let commands = all_commands(&cli).await;
    assert!(commands.len() > 180, "only {} commands", commands.len());
    let mut failures = Vec::new();
    for argv in commands {
        let mut args: Vec<&str> = argv.iter().map(String::as_str).collect();
        // These run other programs.
        if args[0] == "sdk" {
            continue;
        }
        args.push("--agent");
        // `impersonate USER -- COMMAND`: flags go in front.
        if args[0] == "impersonate" {
            args = vec!["impersonate", "--agent", "ada", "--", "ping"];
        }
        // stdin stays open and silent: a command that reads it would hang.
        let mut command = cli.command(&args);
        command
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = command.spawn().expect("spawn");
        let _open = child.stdin.take();
        let wait = tokio::task::spawn_blocking(move || child.wait_with_output());
        let Ok(output) = tokio::time::timeout(std::time::Duration::from_secs(40), wait).await
        else {
            failures.push(format!("{args:?} did not end"));
            continue;
        };
        let output = output.expect("join").expect("output");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let lines: Vec<&str> = stdout.lines().collect();
        let envelope: Option<Value> = (lines.len() == 1)
            .then(|| serde_json::from_str(lines[0]).ok())
            .flatten();
        let Some(envelope) = envelope else {
            failures.push(format!(
                "{args:?} printed {} lines: {stdout:.200}",
                lines.len()
            ));
            continue;
        };
        let code = output.status.code().unwrap_or(-1);
        let consistent = match envelope["ok"].as_bool() {
            Some(true) => code == 0 && envelope.get("data").is_some(),
            Some(false) => {
                (1..=5).contains(&code)
                    && envelope["error"]["code"].is_string()
                    && envelope["error"]["message"].is_string()
            }
            None => false,
        };
        if !consistent || envelope["command"].as_str().unwrap_or_default().is_empty() {
            failures.push(format!("{args:?} exit {code}: {}", lines[0]));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Nothing that writes may reach a read-only profile's instance, whatever
/// the flags say.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn read_only_runs_never_write() {
    let server = MockServer::start().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "result": [] })))
        .mount(&server)
        .await;
    let cli = Cli::basic(&server);
    let effects = effects(&cli).await;
    let mut failures = Vec::new();
    for argv in all_commands(&cli).await {
        let mut args: Vec<&str> = argv.iter().map(String::as_str).collect();
        if matches!(args[0], "sdk" | "impersonate" | "watch" | "update")
            || args.starts_with(&["flow", "tail"])
            || args.starts_with(&["incident", "watch"])
        {
            continue;
        }
        args.extend(["--agent", "--read-only", "--yes"]);
        let run = cli.run(&args).await;
        // Here the instance answers: the other end of the envelope contract.
        let lines: Vec<&str> = run.stdout.lines().collect();
        if lines.len() != 1 || serde_json::from_str::<Value>(lines[0]).is_err() {
            failures.push(format!("{args:?}: {:.300}", run.stdout));
            continue;
        }
        let envelope = run.envelope();
        // A line that does not parse is named by its first word only.
        let Some(effect) = effects.get(envelope["command"].as_str().unwrap_or_default()) else {
            continue;
        };
        let refused = envelope["error"]["code"] == "read_only";
        let wrong = match effect.as_str() {
            "read" | "local" | "confirm-local" => refused,
            "write" | "confirm" => envelope["ok"] == true,
            _ => false,
        };
        if wrong {
            failures.push(format!("{args:?} is `{effect}`: {:.300}", run.stdout));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    let wrote: Vec<String> = server
        .received_requests()
        .await
        .expect("requests")
        .iter()
        .filter(|r| !matches!(r.method.as_str(), "GET" | "HEAD"))
        .map(|r| format!("{} {}", r.method, r.url))
        // Queries sent as a body.
        .filter(|line| {
            ![
                "/api/now/graphql",
                "/oauth_token.do",
                "/batch",
                "identifyreconcile/query",
                "appmanager/apps?",
                "appmanager/plugins?",
            ]
            .iter()
            .any(|read| line.contains(read))
        })
        .collect();
    assert!(wrote.is_empty(), "{}", wrote.join("\n"));
}

#[tokio::test]
async fn introspect_knows_aliases() {
    let cli = Cli::empty();
    let mut answers = Vec::new();
    for group in ["table", "tables"] {
        let run = cli.run(&["introspect", group, "list", "--agent"]).await;
        answers.push(run.ok().envelope()["data"].clone());
    }
    assert_eq!(answers[1]["effect"], "read");
    assert_eq!(answers[0], answers[1]);
}

/// Completion offers table and column names of the offline copy.
#[tokio::test(flavor = "multi_thread")]
async fn completion_knows_tables_and_columns() {
    let server = MockServer::start().await;
    support::mount_me(&server).await;
    let mut cli = Cli::empty();
    let args = [
        "auth",
        "login",
        "-i",
        &server.uri(),
        "-m",
        "api-key",
        "--secret-stdin",
        "--agent",
    ];
    cli.run_with(&args, Some("k")).await.ok();
    let key = server.uri().replace("http://", "").replace(':', "_");
    let dir = cli.dir.path().join("cache").join(&key);
    std::fs::create_dir_all(&dir).expect("dir");
    let index = json!({
        "format": 1, "instance": server.uri(), "built_at": 1, "columns_indexed": true,
        "tables": { "task": { "columns": ["number", "state"] }, "incident": { "parent": "task", "columns": ["caller_id"] } },
    });
    std::fs::write(dir.join("schema.json"), index.to_string()).expect("write");
    cli.env.push(("SN_COMPLETE".into(), "fish".into()));
    let run = cli.run(&["--", "sn", "table", "list", "in"]).await;
    assert!(
        run.stdout.lines().any(|l| l == "incident"),
        "{} {}",
        run.stdout,
        run.stderr
    );
    let run = cli
        .run(&[
            "--",
            "sn",
            "table",
            "list",
            "incident",
            "--fields",
            "number,ca",
        ])
        .await;
    assert!(
        run.stdout.lines().any(|l| l == "number,caller_id"),
        "{}",
        run.stdout
    );
    let run = cli
        .run(&[
            "--",
            "sn",
            "-p",
            "api",
            "api",
            "change-management",
            "list-conf",
        ])
        .await;
    assert!(
        run.stdout.lines().any(|l| l.starts_with("list-conflict")),
        "{}",
        run.stdout
    );
}

/// Destructive and irreversible commands stop until `--yes`, before anything
/// reaches the instance that is not a read.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn destructive_commands_wait_for_yes() {
    const ID: &str = "9d385017c611228701d22104cc95c371";
    let server = MockServer::start().await;
    let record = json!({ "sys_id": ID, "number": "INC0010001", "name": "x", "id": ID, "state": "0", "cart_id": ID, "items": [{ "cart_item_id": ID, "name": "x" }] });
    Mock::given(any())
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "result": record })))
        .mount(&server)
        .await;
    let cli = Cli::basic(&server);
    let file = cli.dir.path().join("in.xml");
    std::fs::write(
        &file,
        "<unload><incident action=\"INSERT_OR_UPDATE\"/></unload>",
    )
    .expect("write");
    let file = file.display().to_string();
    let commands: Vec<Vec<&str>> = vec![
        vec!["table", "delete", "incident", ID],
        vec!["attachment", "delete", ID],
        vec!["change", "delete", ID],
        vec!["change", "task", "delete", ID, ID],
        vec!["change", "conflict", "remove", ID],
        vec!["catalog", "cart-remove", ID],
        vec!["catalog", "cart-empty", ID],
        vec!["catalog", "cart", "remove", ID],
        vec!["catalog", "cart", "clear"],
        vec!["catalog", "cart", "submit"],
        vec!["catalog", "checkout"],
        vec!["catalog", "submit-order"],
        vec!["catalog", "order", ID, "-D", "{\"sysparm_quantity\":\"1\"}"],
        vec!["cmdb", "relation", "delete", "cmdb_ci", ID, ID],
        vec!["xml", "import", &file],
        vec![
            "cicd",
            "app",
            "rollback",
            "--scope",
            "x_a",
            "--version",
            "1.0.0",
        ],
        vec!["cicd", "updateset", "back-out", "--update-set-id", ID],
        vec!["cicd", "batch", "rollback", ID],
        vec!["cicd", "plugin", "activate", "com.x"],
        vec!["cicd", "plugin", "rollback", "com.x"],
        vec!["script", "run", "gs.info(1)"],
        vec!["exec", "gs.info(1)"],
        vec!["profile", "remove", "x"],
        vec!["auth", "register"],
        vec![
            "api",
            "table",
            "delete",
            "--table-name",
            "incident",
            "--sys-id",
            ID,
        ],
        vec!["api", "custom-connector-gateway", "delete-all-documents"],
    ];
    let mut failures = Vec::new();
    for command in commands {
        let run = cli.run(&[&command[..], &["--agent"]].concat()).await;
        let envelope: Value = serde_json::from_str(run.stdout.trim()).unwrap_or_default();
        if envelope["error"]["code"] != "confirmation_required" {
            failures.push(format!("{command:?}: {:.300}", run.stdout));
        }
    }
    let wrote: Vec<String> = server
        .received_requests()
        .await
        .expect("requests")
        .iter()
        .filter(|r| !matches!(r.method.as_str(), "GET" | "HEAD"))
        .map(|r| format!("{} {}", r.method, r.url))
        .collect();
    assert!(
        failures.is_empty() && wrote.is_empty(),
        "{}\n{}",
        failures.join("\n"),
        wrote.join("\n")
    );
}

/// Some documented GET operations change something.
#[tokio::test(flavor = "multi_thread")]
async fn a_get_that_changes_something_is_a_write() {
    let server = MockServer::start().await;
    let cli = Cli::basic(&server);
    let run = cli
        .run(&[
            "api",
            "agent-client-collector",
            "get-activate",
            "--policy-id",
            "x",
            "--read-only",
            "--agent",
        ])
        .await;
    assert_eq!(run.envelope()["error"]["code"], "read_only");
    support::assert_untouched(&server).await;
}

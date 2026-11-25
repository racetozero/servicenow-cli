//! `incident`.

mod support;

use serde_json::{Value, json};
use support::{Cli, ME, mount_me, on, pair, query, result};
use wiremock::matchers::{body_json, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const ID: &str = "9d385017c611228701d22104cc95c371";
const GROUP: &str = "11111111111111111111111111111111";

/// The incident INC1 by number, with its state.
async fn incident(server: &MockServer, state: (&str, &str)) {
    on(
        server,
        "GET",
        "/api/now/table/incident",
        result(json!([{
            "sys_id": pair(ID, ID), "number": pair("INC0000001", "INC0000001"),
            "state": pair(state.0, state.1), "short_description": pair("Disk full", "Disk full"),
            "assigned_to": pair(ME, "Ada Lovelace"),
        }])),
    )
    .await;
}

async fn choices(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/api/now/table/sys_choice"))
        .respond_with(|request: &Request| {
            let asked = query(request)["sysparm_query"].clone();
            let rows: Vec<Value> = [
                ("state", "2", "In Progress"),
                ("state", "16", "Resolved"),
                ("state", "7", "Closed"),
                ("close_code", "fixed", "Solved (Permanently)"),
                ("impact", "1", "High"),
            ]
            .into_iter()
            .filter(|(element, ..)| asked.contains(element))
            .map(|(element, value, label)| {
                json!({ "name": "incident", "element": element, "value": value, "label": label })
            })
            .collect();
            result(rows.into())
        })
        .mount(server)
        .await;
}

/// The only write that may happen.
async fn patch(server: &MockServer, body: Value) {
    Mock::given(method("PATCH"))
        .and(path(format!("/api/now/table/incident/{ID}")))
        .and(body_json(body))
        .respond_with(result(json!({ "sys_id": ID, "number": "INC0000001" })))
        .expect(1)
        .mount(server)
        .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn lists_filter_every_alternative_and_sort_by_default() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/now/table/incident"))
        .respond_with(|request: &Request| {
            let q = query(request);
            assert_eq!(
                q["sysparm_query"],
                "a=1^active=true^NQb=2^active=true^ORDERBYDESCsys_updated_on"
            );
            assert_eq!(q["sysparm_limit"], "3");
            result(json!([{ "number": "INC1", "state": "New", "short_description": "x" }]))
                .insert_header("x-total-count", "1")
        })
        .mount(&server)
        .await;
    let run = Cli::basic(&server)
        .run(&[
            "incident",
            "list",
            "--active",
            "-q",
            "a=1^NQb=2",
            "-n",
            "3",
            "-o",
            "table",
        ])
        .await
        .ok();
    assert!(
        run.stdout.starts_with("NUMBER  PRIORITY  STATE"),
        "{}",
        run.stdout
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn mine_asks_for_your_sys_id_and_checks_the_rows() {
    let server = MockServer::start().await;
    mount_me(&server).await;
    Mock::given(method("GET"))
        .and(path("/api/now/table/incident"))
        .respond_with(|request: &Request| {
            let q = query(request);
            assert_eq!(
                q["sysparm_query"],
                format!("active=true^assigned_to={ME}^ORDERBYDESCsys_updated_on")
            );
            result(json!([{ "number": "INC1", "assigned_to.sys_id": ME }]))
        })
        .mount(&server)
        .await;
    let cli = Cli::basic(&server);
    let run = cli
        .run(&["incidents", "mine", "-f", "number", "--agent"])
        .await
        .ok();
    assert_eq!(run.envelope()["data"], json!([{ "number": "INC1" }]));
    server.reset().await;
    mount_me(&server).await;
    on(
        &server,
        "GET",
        "/api/now/table/incident",
        result(json!([{ "number": "INC1", "assigned_to.sys_id": GROUP }])),
    )
    .await;
    let run = cli.run(&["incident", "mine"]).await;
    assert_eq!(run.code, 1, "the instance ignored the filter");
}

#[tokio::test(flavor = "multi_thread")]
async fn show_takes_a_number_in_one_request_and_never_another_record() {
    let server = MockServer::start().await;
    incident(&server, ("2", "In Progress")).await;
    let cli = Cli::basic(&server);
    let run = cli
        .run(&[
            "incident",
            "show",
            "inc0000001",
            "--display-value",
            "all",
            "--agent",
        ])
        .await
        .ok();
    assert_eq!(run.envelope()["data"]["sys_id"]["value"], ID);
    assert_eq!(server.received_requests().await.expect("requests").len(), 1);
    let run = cli.run(&["incident", "show", "INC0000002"]).await;
    assert_eq!(run.code, 1, "{}", run.stdout);
    let wrong = format!("{}/problem.do?sys_id={ID}", server.uri());
    assert_eq!(cli.run(&["incident", "show", &wrong]).await.code, 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn create_resolves_names_and_labels() {
    let server = MockServer::start().await;
    mount_me(&server).await;
    choices(&server).await;
    on(
        &server,
        "GET",
        "/api/now/table/sys_user_group",
        result(json!([{ "sys_id": GROUP, "name": "Network" }])),
    )
    .await;
    Mock::given(method("POST"))
        .and(path("/api/now/table/incident"))
        .and(body_json(json!({
            "u_site": "HQ", "short_description": "Disk full", "impact": "1", "urgency": "2",
            "assigned_to": ME, "assignment_group": GROUP,
        })))
        .respond_with(result(json!({ "sys_id": ID, "number": "INC0000001" })))
        .expect(1)
        .mount(&server)
        .await;
    let cli = Cli::basic(&server);
    let run = cli
        .run(&[
            "incident",
            "create",
            "-s",
            "Disk full",
            "--impact",
            "high",
            "--urgency",
            "2",
            "--assignee",
            "@me",
            "--group",
            "Network",
            "--field",
            "u_site=HQ",
            "-o",
            "table",
        ])
        .await
        .ok();
    assert_eq!(run.stdout, format!("Created incident INC0000001 ({ID})\n"));
    let run = cli.run(&["incident", "create", "--agent"]).await;
    assert_eq!(run.envelope()["error"]["input"]["arg"], "short-description");
    let run = cli
        .run(&[
            "incident", "create", "-s", "x", "--impact", "huge", "--agent",
        ])
        .await;
    assert_eq!(
        run.envelope()["error"]["input"],
        json!({ "arg": "impact", "choices": [{ "value": "1", "label": "High" }] })
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn update_needs_something_to_change() {
    let server = MockServer::start().await;
    choices(&server).await;
    patch(&server, json!({ "work_notes": "hi", "state": "2" })).await;
    let cli = Cli::basic(&server);
    let run = cli.run(&["incident", "update", ID, "--agent"]).await;
    assert_eq!(run.envelope()["error"]["code"], "input_required");
    cli.run(&[
        "incident",
        "update",
        ID,
        "--state",
        "in progress",
        "--work-notes",
        "hi",
    ])
    .await
    .ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn resolve_uses_the_instances_own_values() {
    let server = MockServer::start().await;
    incident(&server, ("2", "In Progress")).await;
    choices(&server).await;
    patch(
        &server,
        json!({ "state": "16", "close_code": "fixed", "close_notes": "Cleaned\n", "u_x": "1" }),
    )
    .await;
    let cli = Cli::basic(&server);
    let notes = cli.dir.path().join("notes");
    std::fs::write(&notes, "Cleaned\n").expect("notes");
    let notes = notes.to_str().expect("path");
    let args = [
        "incident",
        "resolve",
        "INC0000001",
        "--code",
        "solved (permanently)",
        "--notes-file",
        notes,
        "--field",
        "u_x=1",
    ];
    let run = cli
        .run(&[&args[..], &["--dry-run", "--agent"]].concat())
        .await
        .ok();
    assert_eq!(run.envelope()["data"]["request"]["body"]["state"], "16");
    let run = cli.run(&args).await.ok();
    assert_eq!(run.json()["number"], "INC0000001");
    for (extra, part) in [
        (vec!["--code", "nope", "--notes", "x"], "fixed"),
        (vec!["--notes", "x"], "Solved (Permanently)"),
        (vec!["--code", "fixed"], "resolution notes"),
        (vec!["--code", "fixed", "--notes", " "], "empty"),
        (
            vec!["--code", "fixed", "--notes", "x", "--field", "state=1"],
            "--state",
        ),
    ] {
        let run = cli
            .run(
                &[
                    &["incident", "resolve", "INC0000001", "--agent"],
                    &extra[..],
                ]
                .concat(),
            )
            .await;
        assert_eq!(run.code, 3, "{extra:?}");
        assert!(run.stdout.contains(part), "{extra:?} {}", run.stdout);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_ended_incident_is_not_resolved_again() {
    for state in [
        ("16", "Resolved"),
        ("6", "Resolved"),
        ("7", "Closed"),
        ("8", "Canceled"),
        ("99", "Cancelled"),
    ] {
        let server = MockServer::start().await;
        incident(&server, state).await;
        choices(&server).await;
        let run = Cli::basic(&server)
            .run(&[
                "incident",
                "resolve",
                "INC0000001",
                "--code",
                "fixed",
                "--notes",
                "x",
                "--agent",
            ])
            .await;
        assert_eq!(run.envelope()["error"]["code"], "conflict", "{state:?}");
        assert_eq!(run.code, 1);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn resolve_works_without_access_to_the_choices() {
    let server = MockServer::start().await;
    incident(&server, ("2", "In Progress")).await;
    on(
        &server,
        "GET",
        "/api/now/table/sys_choice",
        ResponseTemplate::new(403),
    )
    .await;
    patch(
        &server,
        json!({ "state": "6", "close_code": "any", "close_notes": "x" }),
    )
    .await;
    Cli::basic(&server)
        .run(&[
            "incident",
            "resolve",
            "INC0000001",
            "--code",
            "any",
            "--notes",
            "x",
        ])
        .await
        .ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn close_asks_and_reopen_goes_back_in_progress() {
    let server = MockServer::start().await;
    incident(&server, ("16", "Resolved")).await;
    choices(&server).await;
    patch(&server, json!({ "state": "7" })).await;
    patch(&server, json!({ "state": "2", "work_notes": "again" })).await;
    let cli = Cli::basic(&server);
    let run = cli
        .run(&["incident", "close", "INC0000001", "--agent"])
        .await;
    assert_eq!(run.envelope()["error"]["code"], "confirmation_required");
    cli.run(&["incident", "close", "INC0000001", "--yes"])
        .await
        .ok();
    cli.run(&["incident", "reopen", ID, "--note", "again"])
        .await
        .ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn notes_and_assignments() {
    let server = MockServer::start().await;
    mount_me(&server).await;
    patch(&server, json!({ "work_notes": "from stdin\n" })).await;
    patch(&server, json!({ "comments": "hello" })).await;
    patch(&server, json!({ "assigned_to": ME })).await;
    let cli = Cli::basic(&server);
    cli.run_with(
        &["incident", "note", ID, "--file", "-"],
        Some("from stdin\n"),
    )
    .await
    .ok();
    cli.run(&["incident", "note", ID, "hello", "--comment"])
        .await
        .ok();
    cli.run(&["incident", "assign", ID, "--to", "@me"])
        .await
        .ok();
    for args in [
        vec!["incident", "note", ID],
        vec!["incident", "note", ID, " "],
        vec!["incident", "note", ID, "x", "--file", "y"],
        vec!["incident", "assign", ID],
    ] {
        assert_eq!(cli.run(&args).await.code, 3, "{args:?}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn edit_writes_only_what_changed() {
    let server = MockServer::start().await;
    mount_me(&server).await;
    incident(&server, ("2", "In Progress")).await;
    choices(&server).await;
    patch(
        &server,
        json!({ "short_description": "Disk: full", "state": "16" }),
    )
    .await;
    let cli = Cli::basic(&server);
    // The assignee is named, but is the same person.
    let edited = "short_description: 'Disk: full'\nstate: Resolved\nassigned_to: '@me'\n";
    let args = ["incident", "edit", "INC0000001", "--file", "-"];
    let run = cli
        .run_with(&[&args[..], &["--agent"]].concat(), Some(edited))
        .await;
    assert_eq!(run.envelope()["error"]["code"], "confirmation_required");
    let run = cli
        .run_with(&[&args[..], &["--yes"]].concat(), Some(edited))
        .await
        .ok();
    assert!(
        run.stderr.contains("- Disk full") && run.stderr.contains("+ Disk: full"),
        "{}",
        run.stderr
    );
    let same = Some("state: 2\nshort_description: Disk full\n");
    let run = cli
        .run_with(&[&args[..], &["--agent"]].concat(), same)
        .await
        .ok();
    assert_eq!(run.envelope()["data"]["changed"], false);
    let run = cli.run_with(&args, Some("sys_id: x\n")).await;
    assert_eq!(run.code, 3);
    let run = cli
        .run(&["incident", "edit", "INC0000001", "--agent"])
        .await;
    assert_eq!(
        run.envelope()["error"]["input"]["arg"],
        "file",
        "no editor without a terminal"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn open_prints_the_link_for_machines() {
    let server = MockServer::start().await;
    let run = Cli::basic(&server)
        .run(&["incident", "open", ID, "--print", "--agent"])
        .await
        .ok();
    let data = &run.envelope()["data"];
    assert_eq!(
        data["url"],
        format!("{}/incident.do?sys_id={ID}", server.uri())
    );
    assert_eq!(data["opened"], false);
}

#[tokio::test(flavor = "multi_thread")]
async fn watch_streams_one_envelope_per_event() {
    let server = MockServer::start().await;
    incident(&server, ("1", "New")).await;
    let at = format!("/api/now/table/incident/{ID}");
    let record = |state: (&str, &str)| result(json!({ "state": pair(state.0, state.1) }));
    Mock::given(method("GET"))
        .and(path(at.clone()))
        .respond_with(record(("1", "New")))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    on(&server, "GET", &at, record(("2", "In Progress"))).await;
    let cli = Cli::basic(&server);
    let args = [
        "incident",
        "watch",
        "INC0000001",
        "--interval",
        "1",
        "--count",
        "2",
    ];
    let run = cli.run(&[&args[..], &["--agent"]].concat()).await.ok();
    let events: Vec<Value> = run
        .stdout
        .lines()
        .map(|line| serde_json::from_str(line).expect("envelope"))
        .collect();
    assert_eq!(events.len(), 2, "{}", run.stdout);
    assert_eq!(events[0]["data"]["event"], "snapshot");
    assert_eq!(events[1]["data"]["poll"], 2);
    assert_eq!(
        events[1]["data"]["changes"],
        json!([{ "field": "state", "before": "New", "after": "In Progress" }])
    );
    assert_eq!(cli.run(&[&args[..], &["-o", "csv"]].concat()).await.code, 3);
    assert_eq!(
        cli.run(&["incident", "watch", "INC0000001", "--interval", "0"])
            .await
            .code,
        3
    );
}

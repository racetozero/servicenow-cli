//! Flows, decision tables and playbooks.

mod support;

use serde_json::{Value, json};
use support::{Cli, on, query, result};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const FLOW: &str = "7d124cc7b7e8f2107df5c0cd2e11a9f6";
const RUN: &str = "11111111111111111111111111111111";
const INC: &str = "9d385017c611228701d22104cc95c371";
const TRIGGER: &str = "22222222222222222222222222222222";

fn flow_row() -> Value {
    json!({
        "sys_id": FLOW, "name": "Notify Caller", "internal_name": "notify_caller", "type": "flow",
        "active": "true", "status": "published", "remote_trigger_id": TRIGGER, "sys_scope.scope": "global",
    })
}

async fn table(server: &MockServer, name: &str, filter: &str, rows: Value) {
    Mock::given(method("GET"))
        .and(path(format!("/api/now/table/{name}")))
        .and(query_param("sysparm_query", filter))
        .respond_with(result(rows))
        .mount(server)
        .await;
}

async fn named_flow(server: &MockServer) {
    let filter = "name=global.notify_caller^ORinternal_name=global.notify_caller^NQinternal_name=notify_caller^sys_scope.scope=global";
    table(server, "sys_hub_flow", filter, json!([flow_row()])).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn list_filters_and_shows_raw_values() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/now/table/sys_hub_flow"))
        .respond_with(|request: &Request| {
            let q = query(request);
            assert_eq!(
                q["sysparm_query"],
                "sys_scope.scope=global^active=true^ORDERBYname"
            );
            assert_eq!(
                (&*q["sysparm_limit"], &*q["sysparm_display_value"]),
                ("2", "false")
            );
            result(json!([flow_row()]))
        })
        .mount(&server)
        .await;
    let run = Cli::basic(&server)
        .run(&[
            "flow",
            "list",
            "--scope",
            "global",
            "--active",
            "--setlimit",
            "2",
            "-o",
            "table",
        ])
        .await
        .ok();
    assert!(run.stdout.starts_with("NAME"), "{}", run.stdout);
    assert!(run.stdout.contains("notify_caller"));
}

#[tokio::test(flavor = "multi_thread")]
async fn get_reads_the_model_or_its_outline() {
    let server = MockServer::start().await;
    named_flow(&server).await;
    let model = json!({
        "id": FLOW, "name": "Notify Caller", "internalName": "notify_caller",
        "actionInstances": [{ "order": "1", "name": "Send Email", "parent": "", "uiUniqueIdentifier": "u" }],
    });
    on(
        &server,
        "GET",
        &format!("/api/now/processflow/flow/{FLOW}"),
        result(json!({ "data": model, "errorMessage": "", "errorCode": 0 })),
    )
    .await;
    let cli = Cli::basic(&server);
    let run = cli.run(&["flow", "get", FLOW, "--agent"]).await.ok();
    assert_eq!(run.envelope()["data"], model);
    let run = cli
        .run(&[
            "flow",
            "get",
            "global.notify_caller",
            "--outline",
            "--agent",
        ])
        .await
        .ok();
    assert_eq!(
        run.envelope()["data"]["steps"][0],
        json!({ "order": 1, "depth": 0, "kind": "action", "name": "Send Email", "parent": null, "id": null })
    );
    let run = cli.run(&["flow", "get", FLOW, "-o", "table"]).await.ok();
    assert!(
        run.stdout.contains("   1  Send Email [action]"),
        "{}",
        run.stdout
    );
    let run = cli.run(&["flow", "get", FLOW, "--outline", "--raw"]).await;
    assert_eq!(run.code, 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn processflow_failures_say_why() {
    let server = MockServer::start().await;
    on(
        &server,
        "GET",
        &format!("/api/now/processflow/flow/{FLOW}"),
        ResponseTemplate::new(404).set_body_json(
            json!({ "result": { "errorMessage": "Flow abc not found.", "errorCode": 0 } }),
        ),
    )
    .await;
    on(
        &server,
        "GET",
        &format!("/api/now/processflow/flow/{RUN}"),
        result(json!({ "data": null, "errorMessage": "boom" })),
    )
    .await;
    on(
        &server,
        "GET",
        &format!("/api/now/processflow/versioning/{FLOW}"),
        result(json!({ "data": [], "errorMessage": "" })),
    )
    .await;
    on(
        &server,
        "GET",
        &format!("/api/now/table/sys_hub_flow_base/{FLOW}"),
        ResponseTemplate::new(404)
            .set_body_json(json!({ "error": { "message": "No Record found" } })),
    )
    .await;
    let cli = Cli::basic(&server);
    let error = |run: support::Run| {
        let envelope = run.envelope();
        (
            run.code,
            envelope["error"]["message"]
                .as_str()
                .expect("message")
                .to_owned(),
        )
    };
    assert_eq!(
        error(cli.run(&["flow", "get", FLOW, "--agent"]).await),
        (4, "Flow abc not found.".into())
    );
    assert_eq!(
        error(cli.run(&["flow", "get", RUN, "--agent"]).await),
        (1, "boom".into())
    );
    let run = cli.run(&["flow", "versions", FLOW, "--agent"]).await;
    assert_eq!(
        run.code, 4,
        "an empty history of an unknown flow: {}",
        run.stdout
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn names_never_resolve_to_another_flow() {
    let server = MockServer::start().await;
    table(
        &server,
        "sys_hub_flow",
        "name=other^ORinternal_name=other",
        json!([flow_row()]),
    )
    .await;
    let mut second = flow_row();
    second["sys_id"] = RUN.into();
    table(
        &server,
        "sys_hub_flow",
        "name=Notify Caller^ORinternal_name=Notify Caller",
        json!([flow_row(), second]),
    )
    .await;
    table(
        &server,
        "sys_hub_flow",
        "name=none^ORinternal_name=none",
        json!([]),
    )
    .await;
    let cli = Cli::basic(&server);
    for (name, code) in [("other", 1), ("Notify Caller", 3), ("none", 4)] {
        let run = cli.run(&["flow", "runs", name, "--agent"]).await;
        assert_eq!(run.code, code, "{name}: {}", run.stdout);
        if code == 3 {
            assert_eq!(
                run.envelope()["error"]["input"]["choices"][0]["value"],
                FLOW
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn runs_filter_by_flow_and_record() {
    let server = MockServer::start().await;
    named_flow(&server).await;
    table(
        &server,
        "incident",
        "number=INC0010001",
        json!([{ "sys_id": INC, "number": "INC0010001" }]),
    )
    .await;
    let filter =
        format!("flow={FLOW}^source_record={INC}^stateINERROR,WAITING^ORDERBYDESCsys_created_on");
    table(
        &server,
        "sys_flow_context",
        &filter,
        json!([{ "sys_id": RUN, "state": "ERROR" }]),
    )
    .await;
    let cli = Cli::basic(&server);
    let run = cli
        .run(&[
            "flow",
            "runs",
            "global.notify_caller",
            "--record",
            "INC0010001",
            "--state",
            "error,waiting",
            "--limit",
            "1",
            "--agent",
        ])
        .await
        .ok();
    let envelope = run.envelope();
    assert_eq!(envelope["data"][0]["sys_id"], RUN);
    assert_eq!(envelope["meta"]["next"], "raise --limit above 1");
    for bad in [
        &["flow", "runs"][..],
        &["flow", "runs", "x", "--since", "soon"],
        &["flow", "runs", "x", "--errors", "--state", "error"],
    ] {
        assert_eq!(cli.run(bad).await.code, 3, "{bad:?}");
    }
}

fn context(reporting: &str, state: &str) -> Value {
    json!([{ "sys_id": RUN, "name": "Notify Caller", "state": state, "reporting": reporting }])
}

#[tokio::test(flavor = "multi_thread")]
async fn debug_joins_the_run_its_failed_step_and_its_log() {
    let server = MockServer::start().await;
    table(
        &server,
        "sys_flow_context",
        &format!("sys_id={RUN}"),
        context("FULL", "ERROR"),
    )
    .await;
    table(
        &server,
        "sys_flow_report",
        &format!("context={RUN}^ORDERBYorder"),
        json!([{ "sys_id": "s1", "state": "COMPLETE" }, { "sys_id": "s2", "state": "ERROR" }]),
    )
    .await;
    table(
        &server,
        "sys_flow_report",
        "sys_id=s2",
        json!([{ "sys_id": "s2", "state": "ERROR", "input": "{\"a\":1}", "output": "cut {" }]),
    )
    .await;
    table(
        &server,
        "sys_flow_log",
        &format!("context={RUN}^ORDERBYDESCorder"),
        json!([{ "order": "2", "level": "2", "message": "later" }, { "order": "1", "level": "0", "message": "first" }]),
    )
    .await;
    let cli = Cli::basic(&server);
    let run = cli.run(&["flow", "debug", RUN, "--agent"]).await.ok();
    let data = run.envelope()["data"].clone();
    assert_eq!(data["failed_step"]["input"], json!({ "a": 1 }));
    assert_eq!(data["failed_step"]["output"], "cut {");
    assert_eq!(
        data["steps"],
        json!({ "total": 2, "by_state": { "COMPLETE": 1, "ERROR": 1 } })
    );
    assert_eq!(data["logs"][0]["message"], "first", "oldest first");
    assert_eq!(data["logs"][1]["level"], "error");
    let run = cli.run(&["flow", "debug", RUN, "-o", "table"]).await.ok();
    assert!(run.stdout.contains("failed step"), "{}", run.stdout);
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_steps_are_explained() {
    let server = MockServer::start().await;
    table(
        &server,
        "sys_flow_context",
        &format!("sys_id={RUN}"),
        context("OFF", "COMPLETE"),
    )
    .await;
    table(
        &server,
        "sys_flow_context",
        &format!("sys_id={INC}"),
        json!([]),
    )
    .await;
    on(
        &server,
        "GET",
        "/api/now/table/sys_flow_report",
        result(json!([])),
    )
    .await;
    table(
        &server,
        "sys_flow_log",
        &format!("context={RUN}^levelIN1,2^ORDERBYorder"),
        json!([{ "level": "1" }]),
    )
    .await;
    let cli = Cli::basic(&server);
    let run = cli.run(&["flow", "steps", RUN, "--agent"]).await;
    assert_eq!(run.code, 4);
    assert!(run.stdout.contains("reporting level OFF"), "{}", run.stdout);
    let run = cli.run(&["flow", "steps", INC, "--agent"]).await;
    assert_eq!(run.code, 4);
    assert!(run.stdout.contains("flow_operator"), "{}", run.stdout);
    let run = cli
        .run(&["flow", "logs", RUN, "--level", "warn", "--agent"])
        .await
        .ok();
    assert_eq!(run.envelope()["data"][0]["level"], "warn");
    assert_eq!(cli.run(&["flow", "logs", "abc"]).await.code, 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn why_not_names_the_clause_that_fails() {
    let server = MockServer::start().await;
    named_flow(&server).await;
    table(
        &server,
        "sys_flow_context",
        &format!("flow={FLOW}^source_record={INC}^ORDERBYDESCsys_created_on"),
        json!([]),
    )
    .await;
    let trigger = json!([{
        "sys_id": TRIGGER, "sys_class_name": "sys_flow_record_trigger", "active": "true",
        "table": "task", "condition": "priority=1^stateCHANGESTO6^active=true^EQ",
        "on_insert": "true", "on_update": "false", "on_delete": "false", "run_on_extended": "true",
    }]);
    let filter = format!("sys_id={TRIGGER}");
    table(&server, "sys_flow_trigger", &filter, trigger.clone()).await;
    table(&server, "sys_flow_record_trigger", &filter, trigger).await;
    table(
        &server,
        "task",
        &format!("sys_id={INC}"),
        json!([{ "sys_id": INC, "sys_class_name": "incident" }]),
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/api/now/stats/task"))
        .respond_with(|request: &Request| {
            let q = query(request);
            let count = match q.get("sysparm_query").map(String::as_str) {
                None => "90",
                Some(q) if q == format!("priority=1^sys_id={INC}") => "0",
                Some(q) if q == format!("active=true^sys_id={INC}") => "1",
                Some("active=true") => "90",
                other => panic!("unexpected count {other:?}"),
            };
            result(json!({ "stats": { "count": count } }))
        })
        .mount(&server)
        .await;
    let cli = Cli::basic(&server);
    let args = [
        "flow",
        "why-not",
        "global.notify_caller",
        "--record",
        &format!("incident:{INC}"),
    ];
    let run = cli.run(&[&args[..], &["--agent"]].concat()).await.ok();
    let data = run.envelope()["data"].clone();
    let clauses = &data["condition"]["segments"][0]["clauses"];
    assert_eq!(
        [
            &clauses[0]["matches"],
            &clauses[1]["matches"],
            &clauses[2]["matches"]
        ],
        [&json!(false), &Value::Null, &json!(true)]
    );
    assert!(
        clauses[2]["note"]
            .as_str()
            .expect("note")
            .contains("every record"),
        "a clause that may be ignored is flagged"
    );
    assert_eq!(data["condition"]["matches"], false);
    assert_eq!(data["blockers"].as_array().map(Vec::len), Some(1), "{data}");
    let run = cli.run(&[&args[..], &["-o", "table"]].concat()).await.ok();
    assert!(
        run.stdout.contains("condition: priority=1 does not match"),
        "{}",
        run.stdout
    );
}

const DECISION: &str = "33333333333333333333333333333333";

fn definition() -> Value {
    let row = |id: &str, value: &str| {
        json!([{
            "sys_id": id, "label": format!("row {id}"), "order": 1, "active": true, "condition": "risk=2",
            "answer": { "value": value, "displayValue": format!("Answer {value}"), "answerElementValues": [] },
        }])
    };
    json!({ "data": {
        "snDtableDesigner": {
            "decisionInput": { "getDecisionInputsByDecisionTable": [
                { "sys_id": { "value": "i1" }, "element": { "value": "u_risk" }, "label": { "value": "Risk" },
                  "internal_type": { "value": "choice" }, "mandatory": { "value": true }, "active": { "value": true },
                  "order": { "value": 1 }, "reference": { "value": "" },
                  "choices": [{ "value": { "value": "2" }, "label": { "value": "High" } }] },
                { "sys_id": { "value": "i2" }, "element": { "value": "u_change" }, "label": { "value": "Change" },
                  "internal_type": { "value": "reference" }, "mandatory": { "value": false }, "active": { "value": true },
                  "order": { "value": 2 }, "reference": { "value": "change_request" }, "choices": [] },
            ] },
            "decisionCondition": { "getDecisionConditionsByDecisionTable": [] },
            "decision": { "rows": row("d1", "a"), "fallback": row("d2", "b") },
        },
        "snDecisionTable": { "answerElement": { "getAnswerElementsOfDecisionTable": [] } },
    } })
}

async fn decision_server(definition: Value) -> MockServer {
    let server = MockServer::start().await;
    table(
        &server,
        "sys_decision",
        "name=Risk Table",
        json!([{ "sys_id": DECISION, "name": "Risk Table", "answer_table": "x_answer", "active": "true", "sys_scope.scope": "global" }]),
    )
    .await;
    on(
        &server,
        "POST",
        "/api/now/graphql",
        ResponseTemplate::new(200).set_body_json(definition),
    )
    .await;
    server
}

#[tokio::test(flavor = "multi_thread")]
async fn decisions_show_and_run() {
    let server = decision_server(definition()).await;
    table(
        &server,
        "change_request",
        "number=CHG0000001",
        json!([{ "sys_id": INC, "number": "CHG0000001" }]),
    )
    .await;
    Mock::given(method("POST"))
        .and(path(
            "/api/sn_decision_table/generic/evaluate_decision_table",
        ))
        .respond_with(|request: &Request| {
            assert_eq!(query(request)["decisionTableId"], DECISION);
            let body: Value = serde_json::from_slice(&request.body).expect("json");
            assert_eq!(
                body,
                json!({
                    "testInputs": { "u_risk": "2", "u_change": INC }, "isFirstMatch": true,
                    "includeDelta": false, "isMultiResultTable": false,
                })
            );
            result(json!({ "d1": [{ "value": "a", "displayValue": "Answer a" }] }))
        })
        .expect(1)
        .mount(&server)
        .await;
    let mut cli = Cli::basic(&server);
    // Reading and evaluating are no writes.
    cli.env.push(("SN_READ_ONLY".into(), "1".into()));
    let run = cli
        .run(&["decision", "show", "Risk Table", "--agent"])
        .await
        .ok();
    let data = run.envelope()["data"].clone();
    assert_eq!(data["decisions"][1]["default"], true);
    assert_eq!(data["inputs"][0]["choices"][0]["value"], "2");
    let args = ["decision", "run", "Risk Table", "--agent", "-i", "u_risk=2"];
    let run = cli
        .run(&[&args[..], &["-i", "u_change=CHG0000001"]].concat())
        .await
        .ok();
    let data = run.envelope()["data"].clone();
    assert_eq!(
        data["matches"],
        json!([{ "sys_id": "d1", "label": "row d1", "order": 1, "default": false,
            "answer": { "value": "a", "display_value": "Answer a" } }])
    );
    assert_eq!(data["resolved_from"], json!({ "u_change": "CHG0000001" }));
    let run = cli
        .run(&[
            "decision",
            "run",
            "Risk Table",
            "-i",
            "u_risk=High",
            "--agent",
        ])
        .await;
    assert_eq!(
        (
            run.code,
            run.envelope()["error"]["input"]["choices"][0]["value"].clone()
        ),
        (3, json!("2"))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn decision_refusals_name_the_role_or_the_plugin() {
    let mut empty = definition();
    empty["data"]["snDtableDesigner"]["decision"]["rows"] = Value::Null;
    let server = decision_server(empty).await;
    let run = Cli::basic(&server)
        .run(&["decision", "show", "Risk Table", "--agent"])
        .await;
    assert_eq!(run.code, 2);
    assert!(
        run.stdout.contains("decision_table_reader"),
        "{}",
        run.stdout
    );
    let server = decision_server(json!({ "errors": [{
        "message": "Validation error (FieldUndefined@[snDtableDesigner]) : Field 'snDtableDesigner' in type 'QueryType' is undefined",
    }] }))
    .await;
    let run = Cli::basic(&server)
        .run(&["decision", "show", "Risk Table", "--agent"])
        .await;
    assert_eq!(run.code, 1);
    assert!(run.stdout.contains("sn_decision_table"), "{}", run.stdout);
}

async fn playbook_server(operation: &'static str, value: Value) -> MockServer {
    let server = MockServer::start().await;
    on(
        &server,
        "POST",
        "/api/now/graphql",
        ResponseTemplate::new(200).set_body_json(
            json!({ "data": { "snPlaybookExp": { "playbook": { operation: value } } } }),
        ),
    )
    .await;
    server
}

#[tokio::test(flavor = "multi_thread")]
async fn playbooks_list_and_tell_missing_from_empty() {
    let record = format!("incident:{INC}");
    let args = ["playbook", "list", &record, "--agent"];
    let rows = json!([{ "sys_id": "c1", "title": "Steps", "state": { "value": "ACTIVE", "displayValue": "Active" } }]);
    let server = playbook_server("getPlaybooksForParentRecord", rows.clone()).await;
    let run = Cli::basic(&server).run(&args).await.ok();
    assert_eq!(run.envelope()["data"], rows);
    let body: Value =
        serde_json::from_slice(&server.received_requests().await.expect("requests")[0].body)
            .expect("json");
    assert_eq!(
        body["variables"],
        json!({ "table": "incident", "record": INC })
    );
    let server = playbook_server("getPlaybooksForParentRecord", Value::Null).await;
    assert_eq!(Cli::basic(&server).run(&args).await.code, 4);
}

#[tokio::test(flavor = "multi_thread")]
async fn playbooks_start_skip_and_fail() {
    let record = format!("incident:{INC}");
    let args = [
        "playbook",
        "trigger",
        &record,
        "--scoped-name",
        "sn_vsc.steps",
        "--only-if-none",
        "--agent",
    ];
    let started = "snPlaybookExp_playbook_PlaybookContext";
    for (id, triggered) in [(json!("c1"), true), (Value::Null, false)] {
        let answer = json!({ "__typename": started, "sys_id": id, "parent_table": "incident", "parent_record": INC });
        let server = playbook_server("triggerPlaybook", answer).await;
        let run = Cli::basic(&server).run(&args).await.ok();
        assert_eq!(run.envelope()["data"]["triggered"], triggered);
        let run = Cli::basic(&server)
            .run(&[&args[..], &["--dry-run"]].concat())
            .await
            .ok();
        assert_eq!(
            run.envelope()["data"]["request"]["body"]["variables"]["onlyIfNone"],
            true
        );
    }
    let failed = json!({
        "__typename": "snPlaybookExp_playbook_TriggerPlaybookError",
        "errorType": "INVALID_PROCESS_DEFINITION", "message": "no such playbook",
    });
    let server = playbook_server("triggerPlaybook", failed).await;
    let run = Cli::basic(&server).run(&args).await;
    assert_eq!(run.code, 1);
    assert_eq!(
        run.envelope()["error"]["payload"]["errorType"],
        "INVALID_PROCESS_DEFINITION"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_launch_encodes_its_inputs_twice() {
    let answer = json!({ "__typename": "snPlaybookExp_playbook_PlaybookContext", "sys_id": "c1" });
    let server = playbook_server("launchPlaybook", answer).await;
    let cli = Cli::basic(&server);
    let run = cli
        .run(&[
            "playbook", "launch", DECISION, "--record", INC, "--input", "note=a&b", "--agent",
        ])
        .await
        .ok();
    assert_eq!(run.envelope()["data"]["launched"], true);
    let body: Value =
        serde_json::from_slice(&server.received_requests().await.expect("requests")[0].body)
            .expect("json");
    assert_eq!(body["variables"]["input"], "note=a%2526b");
    assert_eq!(cli.run(&["playbook", "launch", "abc"]).await.code, 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_start_that_names_no_run_is_an_error() {
    let answer = json!({ "__typename": "snPlaybookExp_playbook_PlaybookContext" });
    let server = playbook_server("launchPlaybook", answer).await;
    let run = Cli::basic(&server)
        .run(&["playbook", "launch", DECISION, "--agent"])
        .await;
    assert_eq!(run.envelope()["error"]["code"], "api", "{}", run.stdout);
}

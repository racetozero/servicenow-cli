//! `get` (records and nouns), `set` and `resolve`.

mod support;

use serde_json::{Value, json};
use support::{Cli, ME, graphql, mount_caller, mount_me, on, query, result};
use wiremock::matchers::{body_json, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const ID: &str = "9d385017c611228701d22104cc95c371";
const OTHER: &str = "11111111111111111111111111111111";

async fn journal(server: &MockServer, table: &'static str, comments: &'static str) {
    graphql(server, move |_, _| {
        json!({ "data": { "GlideRecord_Query": { table: { "_results": [
            { "comments": { "displayValue": comments }, "work_notes": { "displayValue": "" } }
        ] } } } })
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_record_comes_with_its_variables_and_journal() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/now/table/incident"))
        .respond_with(|request: &Request| {
            assert_eq!(query(request)["sysparm_query"], "number=INC0010001");
            result(json!([{ "sys_id": ID, "number": "INC0010001" }]))
        })
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/api/now/table/incident/{ID}")))
        .respond_with(|request: &Request| {
            assert_eq!(query(request)["sysparm_fields"], "number,state");
            result(json!({ "number": "INC0010001", "state": "New" }))
        })
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/now/table/question_answer"))
        .respond_with(|request: &Request| {
            assert_eq!(
                query(request)["sysparm_query"],
                format!("table_name=incident^table_sys_id={ID}")
            );
            result(json!([
                { "table_sys_id": ID, "question.name": "urgent", "question.question_text": "Urgent?", "value": "yes" },
                { "table_sys_id": ID, "question.name": "", "value": "" },
            ]))
        })
        .mount(&server)
        .await;
    journal(
        &server,
        "incident",
        "2026-08-11 15:40:37 - Ada Lovelace (Additional comments)\nhello\n",
    )
    .await;
    let cli = Cli::basic(&server);
    let run = cli
        .run(&["get", "INC0010001", "-f", "number,state", "--agent"])
        .await
        .ok();
    assert_eq!(
        run.envelope()["data"],
        json!({
            "table": "incident", "sys_id": ID,
            "record": { "number": "INC0010001", "state": "New" },
            "variables": [{ "name": "urgent", "label": "Urgent?", "value": "yes" }],
            "journal": [{
                "created_on": "2026-08-11 15:40:37", "author": "Ada Lovelace",
                "element": "comments", "label": "Additional comments", "text": "hello",
            }],
        })
    );
    let run = cli
        .run(&[
            "get",
            "incident",
            "INC0010001",
            "-f",
            "number,state",
            "-o",
            "table",
        ])
        .await
        .ok();
    for part in [
        "INC0010001",
        "Variables\nurgent  yes",
        "Journal\n2026-08-11 15:40:37  Ada Lovelace",
    ] {
        assert!(run.stdout.contains(part), "{}", run.stdout);
    }
    let run = cli.run(&["get", "INC0010001", "--raw"]).await;
    assert_eq!(run.code, 3, "{}", run.stderr);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_part_that_is_forbidden_does_not_hide_the_record() {
    let server = MockServer::start().await;
    on(
        &server,
        "GET",
        &format!("/api/now/table/incident/{ID}"),
        result(json!({ "number": "INC1" })),
    )
    .await;
    on(
        &server,
        "GET",
        "/api/now/table/question_answer",
        ResponseTemplate::new(403).set_body_json(json!({ "error": { "message": "no" } })),
    )
    .await;
    journal(&server, "incident", "").await;
    let run = Cli::basic(&server)
        .run(&["get", &format!("incident:{ID}"), "--agent"])
        .await
        .ok();
    let envelope = run.envelope();
    assert_eq!(envelope["data"]["variables"], Value::Null);
    assert_eq!(envelope["data"]["journal"], json!([]));
    let warning = envelope["meta"]["warnings"][0].as_str().expect("warning");
    assert!(warning.contains("variables"), "{warning}");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unknown_prefix_leads_to_the_records_own_table() {
    let server = MockServer::start().await;
    on(
        &server,
        "GET",
        "/api/now/table/sys_number",
        result(json!([{
            "prefix": { "value": "TASK", "display_value": "TASK" },
            "category": { "value": "task", "display_value": "Task" },
        }])),
    )
    .await;
    on(
        &server,
        "GET",
        "/api/now/table/task",
        result(json!([{ "sys_id": ID, "number": "TASK0000001" }])),
    )
    .await;
    on(
        &server,
        "GET",
        &format!("/api/now/table/task/{ID}"),
        result(json!({ "sys_class_name": "sc_task" })),
    )
    .await;
    // Also answers the read of its request item.
    on(
        &server,
        "GET",
        &format!("/api/now/table/sc_task/{ID}"),
        result(json!({ "number": "TASK0000001", "request_item": OTHER })),
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/api/now/table/sc_item_option_mtom"))
        .respond_with(|request: &Request| {
            assert_eq!(query(request)["sysparm_query"], format!("request_item={OTHER}"));
            result(json!([{ "request_item": OTHER, "sc_item_option.item_option_new.name": "os", "sc_item_option.value": "mac" }]))
        })
        .mount(&server)
        .await;
    journal(&server, "sc_task", "").await;
    let run = Cli::basic(&server)
        .run(&["get", "TASK0000001", "--agent"])
        .await
        .ok();
    let data = &run.envelope()["data"];
    assert_eq!(data["table"], "sc_task");
    assert_eq!(data["variables"], json!([{ "name": "os", "value": "mac" }]));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_word_that_is_neither_record_nor_noun_lists_the_nouns() {
    let server = MockServer::start().await;
    let run = Cli::basic(&server)
        .run(&["get", "updatset", "--agent"])
        .await;
    assert_eq!(run.code, 3);
    let message = run.envelope()["error"]["message"].to_string();
    assert!(
        message.contains("updateset") && message.contains("INC0010001"),
        "{message}"
    );
    support::assert_untouched(&server).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn me_has_roles_and_groups() {
    let server = MockServer::start().await;
    mount_me(&server).await;
    on(
        &server,
        "GET",
        "/api/now/table/sys_user_has_role",
        result(json!([
            { "user": ME, "role.name": "itil", "inherited": "false" },
            { "user": ME, "role.name": "admin", "inherited": "true" },
            { "user": ME, "role.name": "itil", "inherited": "true" },
        ])),
    )
    .await;
    on(
        &server,
        "GET",
        "/api/now/table/sys_user_grmember",
        result(json!([{ "user": ME, "group": OTHER, "group.name": "Network" }])),
    )
    .await;
    let cli = Cli::basic(&server);
    let run = cli.run(&["get", "me", "--agent"]).await.ok();
    let data = &run.envelope()["data"];
    assert_eq!(data["user_name"], "ada");
    assert_eq!(data["admin"], true);
    assert_eq!(data["roles"], json!(["admin", "itil"]));
    assert_eq!(data["direct_roles"], json!(["itil"]));
    assert_eq!(
        data["groups"],
        json!([{ "sys_id": OTHER, "name": "Network" }])
    );

    server.reset().await;
    mount_me(&server).await;
    on(
        &server,
        "GET",
        "/api/now/table/sys_user_grmember",
        result(json!([])),
    )
    .await;
    // The instance ignored the filter and answers with someone else's roles.
    on(
        &server,
        "GET",
        "/api/now/table/sys_user_has_role",
        result(json!([{ "user": OTHER, "role.name": "admin" }])),
    )
    .await;
    let run = cli.run(&["get", "me", "--agent"]).await;
    assert_eq!(run.code, 1, "{}", run.stdout);
}

#[tokio::test(flavor = "multi_thread")]
async fn mine_merges_your_tasks_with_those_of_your_groups() {
    let server = MockServer::start().await;
    mount_me(&server).await;
    on(
        &server,
        "GET",
        "/api/now/table/sys_user_grmember",
        result(json!([{ "user": ME, "group": OTHER, "group.name": "Network" }])),
    )
    .await;
    let task = |number: &str, assignee: &str, group: &str, updated: &str| {
        let pair = |v: &str, d: &str| json!({ "value": v, "display_value": d });
        json!({
            "number": pair(number, number), "active": pair("true", "true"),
            "assigned_to": pair(assignee, "Ada"), "assignment_group": pair(group, "Network"),
            "state": pair("2", "In Progress"), "sys_updated_on": pair(updated, "local"),
        })
    };
    let own = task("INC1", ME, "", "2026-01-01 00:00:00");
    let groups = task("INC2", "", OTHER, "2026-01-02 00:00:00");
    Mock::given(method("GET"))
        .and(path("/api/now/table/task"))
        .respond_with(move |request: &Request| {
            let q = query(request)["sysparm_query"].clone();
            if q == format!("active=true^assigned_to={ME}^ORDERBYDESCsys_updated_on") {
                return result(json!([own]));
            }
            assert_eq!(
                q,
                format!("active=true^assigned_toISEMPTY^assignment_groupIN{OTHER}^ORDERBYDESCsys_updated_on")
            );
            result(json!([groups]))
        })
        .mount(&server)
        .await;
    let cli = Cli::basic(&server);
    let run = cli.run(&["get", "mine"]).await.ok();
    let rows = run.json();
    assert_eq!(rows[0]["number"], "INC2", "newest first");
    assert_eq!(rows[1]["state"], "In Progress");
    let run = cli
        .run(&[
            "get",
            "mine",
            "--me-only",
            "-o",
            "csv",
            "--select",
            "number",
        ])
        .await
        .ok();
    assert_eq!(run.stdout, "number\nINC1\n");
}

#[tokio::test(flavor = "multi_thread")]
async fn last_checks_who_updated_the_record() {
    let server = MockServer::start().await;
    mount_caller(&server).await;
    on(
        &server,
        "GET",
        "/api/now/table/task",
        result(json!([{ "sys_id": ID, "sys_class_name": "incident", "sys_updated_by": "bob" }])),
    )
    .await;
    let run = Cli::basic(&server).run(&["get", "last", "--agent"]).await;
    assert_eq!(run.code, 1);
    assert!(
        run.envelope()["error"]["message"]
            .to_string()
            .contains("sys_updated_by")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn logs_filter_on_the_server_and_check_the_answer() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/now/table/syslog"))
        .respond_with(|request: &Request| {
            let q = query(request);
            assert_eq!(
                q["sysparm_query"],
                "sys_created_onRELATIVEGT@hour@ago@48^levelIN2,3^messageLIKEboom^ORDERBYDESCsys_created_on"
            );
            assert_eq!(q["sysparm_limit"], "5");
            result(json!([{
                "sys_id": ID, "sys_created_on": "2099-01-01 00:00:00", "level": "2",
                "source": "x", "message": "it went BOOM", "sys_created_by": "ada",
            }]))
        })
        .mount(&server)
        .await;
    let cli = Cli::basic(&server);
    let args = [
        "get", "logs", "--since", "2d", "--level", "error", "-g", "boom", "-l", "5",
    ];
    let run = cli.run(&args).await.ok();
    assert_eq!(run.json()[0]["level"], "error");
    server.reset().await;
    on(
        &server,
        "GET",
        "/api/now/table/syslog",
        result(
            json!([{ "sys_created_on": "2001-01-01 00:00:00", "level": "2", "message": "boom" }]),
        ),
    )
    .await;
    let run = cli.run(&args).await;
    assert_eq!(
        run.code, 1,
        "an old entry proves that the filter was ignored"
    );
    let run = cli.run(&["get", "logs", "--since", "soon"]).await;
    assert_eq!(run.code, 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn logs_ask_for_the_largest_whole_unit() {
    for (since, unit) in [
        ("90s", "second@ago@90"),
        ("15m", "minute@ago@15"),
        ("2h", "hour@ago@2"),
    ] {
        let server = MockServer::start().await;
        on(&server, "GET", "/api/now/table/syslog", result(json!([]))).await;
        Cli::basic(&server)
            .run(&["get", "logs", "--since", since])
            .await
            .ok();
        let requests = server.received_requests().await.expect("requests");
        let filter = query(&requests[0])["sysparm_query"].clone();
        assert!(
            filter.starts_with(&format!("sys_created_onRELATIVEGT@{unit}^")),
            "{filter}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn properties_come_from_the_table_or_the_runtime() {
    let server = MockServer::start().await;
    let row = |name: &str, scope: &str| {
        json!({
            "name": { "value": name, "display_value": name }, "value": { "value": "15", "display_value": "15" },
            "sys_scope": { "value": OTHER, "display_value": scope }, "is_private": { "value": "false" },
        })
    };
    let (one, two, three) = (
        row("a.b", "Global"),
        row("dup", "Global"),
        row("dup", "Acme"),
    );
    Mock::given(method("GET"))
        .and(path("/api/now/table/sys_properties"))
        .respond_with(
            move |request: &Request| match query(request)["sysparm_query"].as_str() {
                "name=a.b^ORDERBYname" | "nameSTARTSWITHa.^ORDERBYname" => result(json!([one])),
                "name=dup^ORDERBYname" => result(json!([two, three])),
                "name=wrong^ORDERBYname" => result(json!([one])),
                _ => result(json!([])),
            },
        )
        .mount(&server)
        .await;
    graphql(&server, |_, body| {
        let variables = &body["variables"];
        let value = match variables["n"].as_str() {
            Some("glide.buildname") => json!("Brazil"),
            _ => variables["d"].clone(),
        };
        json!({ "data": { "snDecisionTable": { "sysProperties": { "v": value } } } })
    })
    .await;
    let cli = Cli::basic(&server);
    let run = cli.run(&["get", "property", "a.b"]).await.ok();
    assert_eq!(run.json()["value"], "15");
    assert_eq!(run.json()["scope"], "Global");
    let run = cli.run(&["get", "property", "a.*"]).await.ok();
    assert_eq!(run.json()[0]["name"], "a.b");
    let run = cli.run(&["get", "property", "glide.buildname"]).await.ok();
    assert_eq!(
        run.json(),
        json!({ "name": "glide.buildname", "value": "Brazil", "source": "runtime" })
    );
    for (name, code) in [("none", 4), ("dup", 3), ("wrong", 1), ("a^b", 3)] {
        assert_eq!(
            cli.run(&["get", "property", name]).await.code,
            code,
            "{name}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn scripts_are_found_by_name_in_one_document() {
    let server = MockServer::start().await;
    graphql(&server, |document, body| {
        let variables = &body["variables"];
        assert_eq!(
            variables["byApiName"],
            "name=global.JSUtil^NQname=JSUtil^sys_scope.scope=global"
        );
        assert!(document.contains("sys_ui_script(queryConditions: $byName"), "{document}");
        let row = json!({
            "sys_id": { "value": ID }, "name": { "value": "JSUtil" }, "api_name": { "value": "global.JSUtil" },
            "script": { "value": "var a = 1;\n" }, "sys_scope": { "displayValue": "Global" },
        });
        json!({ "data": { "GlideRecord_Query": {
            "sys_script_include": { "_results": [row] }, "sys_script": { "_results": [] },
        } } })
    })
    .await;
    let cli = Cli::basic(&server);
    let run = cli
        .run(&["get", "script", "global.JSUtil", "--source"])
        .await
        .ok();
    assert_eq!(run.stdout, "var a = 1;\n");
    let run = cli
        .run(&["get", "script", "global.JSUtil", "--agent"])
        .await
        .ok();
    assert_eq!(run.envelope()["data"]["type"], "include");

    server.reset().await;
    graphql(&server, |_, _| {
        let row = json!({ "sys_id": { "value": ID }, "name": { "value": "Rule" } });
        json!({ "data": { "GlideRecord_Query": {
            "sys_script": { "_results": [row, row] },
        } } })
    })
    .await;
    let run = cli.run(&["get", "script", "Rule", "--agent"]).await;
    assert_eq!(run.code, 3);
    let choices = &run.envelope()["error"]["input"]["choices"];
    assert_eq!(choices[0]["value"], format!("sys_script:{ID}"));
}

#[tokio::test(flavor = "multi_thread")]
async fn instance_and_choices() {
    let server = MockServer::start().await;
    graphql(&server, |_, body| {
        let variables = &body["variables"];
        json!({ "data": { "snDecisionTable": { "sysProperties": {
            "release": "Brazil", "war": "glide-brazil-08-25-2026__patch3-09-01-2026.zip", "node": variables["d"],
        } } } })
    })
    .await;
    on(
        &server,
        "GET",
        "/api/now/ui/meta/incident",
        result(json!({ "columns": {
            "state": { "choices": [{ "value": "1", "label": "New" }] }, "number": {},
        } })),
    )
    .await;
    let cli = Cli::basic(&server);
    let data = cli.run(&["get", "instance"]).await.ok().json();
    assert_eq!(data["release"], "Brazil");
    assert_eq!(data["patch"], "patch3");
    assert_eq!(data["node"], Value::Null);
    for args in [
        vec!["get", "choices", "incident.state"],
        vec!["get", "choices", "incident", "state"],
    ] {
        assert_eq!(
            cli.run(&args).await.ok().json(),
            json!([{ "value": "1", "label": "New" }])
        );
    }
    assert_eq!(
        cli.run(&["get", "choices", "incident.number"]).await.code,
        4
    );
    assert_eq!(cli.run(&["get", "choices", "incident"]).await.code, 3);
}

/// The picker's answer for a scope and an update set.
fn current(scope: &str, set: &str) -> ResponseTemplate {
    result(json!({
        "currentApplication": { "sysId": scope, "name": "Acme", "scopeName": "x_acme" },
        "currentUpdateSet": { "sysId": set, "name": "Work [Acme]" },
    }))
}

#[tokio::test(flavor = "multi_thread")]
async fn the_current_update_set_has_its_newest_changes() {
    let server = MockServer::start().await;
    on(
        &server,
        "GET",
        "/api/now/ui/concoursepicker/current",
        current(OTHER, ID),
    )
    .await;
    on(
        &server,
        "GET",
        &format!("/api/now/table/sys_update_set/{ID}"),
        result(json!({
            "name": { "value": "Work", "display_value": "Work" }, "state": { "value": "in progress", "display_value": "In progress" },
            "application": { "value": OTHER, "display_value": "Acme" },
        })),
    )
    .await;
    on(
        &server,
        "GET",
        "/api/now/stats/sys_update_xml",
        result(json!([{ "groupby_fields": [{ "field": "update_set", "value": ID }], "stats": { "count": "12" } }])),
    )
    .await;
    on(
        &server,
        "GET",
        "/api/now/table/sys_update_xml",
        result(json!([{ "update_set": ID, "type": "Business Rule", "target_name": "x", "action": "INSERT_OR_UPDATE" }])),
    )
    .await;
    let cli = Cli::basic(&server);
    let data = cli.run(&["get", "updateset"]).await.ok().json();
    assert_eq!(data["in_progress"], true);
    assert_eq!(data["change_count"], 12);
    assert_eq!(data["changes"][0]["type"], "Business Rule");
    assert_eq!(data["scope"], json!({ "sys_id": OTHER, "name": "Acme" }));
    assert_eq!(data["current"], true);
}

#[tokio::test(flavor = "multi_thread")]
async fn set_scope_writes_the_preference_and_reads_it_back() {
    let server = MockServer::start().await;
    mount_me(&server).await;
    let at = "/api/now/ui/concoursepicker";
    Mock::given(method("GET"))
        .and(path(format!("{at}/current")))
        .respond_with(current(ID, ID))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    on(&server, "GET", &format!("{at}/current"), current(OTHER, ID)).await;
    Mock::given(method("PUT"))
        .and(path(format!("{at}/application")))
        .and(body_json(json!({ "app_id": OTHER })))
        .respond_with(result(json!({})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/now/table/sys_scope"))
        .respond_with(|request: &Request| {
            assert_eq!(
                query(request)["sysparm_query"],
                "scope=x_acme^ORname=x_acme^ORsys_id=x_acme"
            );
            result(json!([{ "sys_id": OTHER, "name": "Acme", "scope": "x_acme" }]))
        })
        .mount(&server)
        .await;
    on(
        &server,
        "GET",
        "/api/now/table/sys_user_preference",
        result(json!([{ "sys_id": ID, "user": ME, "name": "apps.current_app" }])),
    )
    .await;
    Mock::given(method("PATCH"))
        .and(path(format!("/api/now/table/sys_user_preference/{ID}")))
        .and(body_json(json!({ "value": OTHER })))
        .respond_with(result(json!({})))
        .expect(1)
        .mount(&server)
        .await;
    let run = Cli::basic(&server)
        .run(&["set", "scope", "x_acme", "--agent"])
        .await
        .ok();
    let data = &run.envelope()["data"];
    assert_eq!(data["scope"]["scope"], "x_acme");
    assert_eq!(
        data["update_set"]["name"], "Work",
        "without the picker's suffix"
    );
    assert_eq!(data["previous"]["scope"]["sys_id"], ID);
}

#[tokio::test(flavor = "multi_thread")]
async fn set_scope_writes_nothing_before_it_knows_who_you_are() {
    let server = MockServer::start().await;
    on(
        &server,
        "GET",
        "/api/now/ui/concoursepicker/current",
        current(ID, ID),
    )
    .await;
    on(
        &server,
        "GET",
        "/api/now/table/sys_scope",
        result(json!([{ "sys_id": OTHER, "name": "Acme", "scope": "x_acme" }])),
    )
    .await;
    on(&server, "GET", "/api/now/table/sys_user", result(json!([]))).await;
    Mock::given(method("PUT"))
        .respond_with(result(json!({})))
        .expect(0)
        .mount(&server)
        .await;
    let run = Cli::basic(&server).run(&["set", "scope", "x_acme"]).await;
    assert_eq!(run.code, 4, "{}", run.stderr);
}

#[tokio::test(flavor = "multi_thread")]
async fn set_updateset_skips_the_current_one_and_explains_a_miss() {
    let server = MockServer::start().await;
    on(
        &server,
        "GET",
        "/api/now/ui/concoursepicker/current",
        current(OTHER, ID),
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/api/now/table/sys_update_set"))
        .respond_with(move |request: &Request| {
            match query(request)["sysparm_query"].as_str() {
                "name=Work^application=11111111111111111111111111111111^state=in progress" => {
                    result(json!([{ "sys_id": ID, "name": "Work", "application": OTHER, "state": "in progress" }]))
                }
                "name=Done" => result(json!([{ "sys_id": ID, "name": "Done", "application": OTHER, "state": "complete" }])),
                _ => result(json!([])),
            }
        })
        .mount(&server)
        .await;
    let cli = Cli::basic(&server);
    // No PUT is mounted: the picker refuses the set that is current.
    cli.run(&["set", "updateset", "Work"]).await.ok();
    let run = cli.run(&["set", "updateset", "Done", "--agent"]).await;
    assert_eq!(run.code, 3);
    assert!(
        run.envelope()["error"]["message"]
            .to_string()
            .contains("complete")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn resolve_finds_people_and_records() {
    let server = MockServer::start().await;
    mount_me(&server).await;
    on(
        &server,
        "GET",
        "/api/now/table/sys_user",
        result(json!([{ "sys_id": ID, "user_name": "bob", "email": "b@x", "name": "Bob" }])),
    )
    .await;
    on(
        &server,
        "GET",
        &format!("/api/now/table/sys_user/{ID}"),
        result(json!({ "sys_id": ID, "user_name": "bob", "name": "Bob" })),
    )
    .await;
    let cli = Cli::basic(&server);
    let run = cli.run(&["resolve", "user", "B@x"]).await.ok();
    assert_eq!(
        run.json(),
        json!({ "table": "sys_user", "sys_id": ID, "user_name": "bob", "name": "Bob" })
    );
    let run = cli.run(&["resolve", "user", "@me", "--agent"]).await.ok();
    assert_eq!(run.envelope()["data"]["sys_id"], ME);
    let run = cli.run(&["resolve", "user", "nobody"]).await;
    assert_eq!(run.code, 4, "{}", run.stderr);
}

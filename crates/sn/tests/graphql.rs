//! `graphql`, `gr`, `aggregate`, `journal` and `variables`.

mod support;

use serde_json::{Value, json};
use support::{Cli, graphql, on, query, result};
use wiremock::matchers::{body_json, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const ID: &str = "9d385017c611228701d22104cc95c371";
const RITM: &str = "11111111111111111111111111111111";

fn records(table: &str, rows: Value) -> Value {
    json!({ "data": { "GlideRecord_Query": { table: { "_rowCount": 7, "_results": rows } } } })
}

fn undefined(field: &str) -> Value {
    json!({ "errors": [{ "message": format!("Validation error (FieldUndefined@[x]) : Field '{field}' in type 'T' is undefined") }], "data": null })
}

#[tokio::test(flavor = "multi_thread")]
async fn documents_go_out_with_variables_and_come_back_as_data() {
    let server = MockServer::start().await;
    graphql(&server, |_, body| {
        assert_eq!(
            *body,
            json!({
                "query": "query A($n: Int, $q: String) { a }", "operationName": "A",
                "variables": { "n": 2, "q": "x=1" },
            })
        );
        json!({ "data": { "a": 1 } })
    })
    .await;
    let cli = Cli::basic(&server);
    let args = [
        "graphql",
        "-",
        "--variables",
        r#"{"n":2,"q":"old"}"#,
        "--var",
        "q=x=1",
        "--operation",
        "A",
    ];
    let document = Some("query A($n: Int, $q: String) { a }");
    assert_eq!(
        cli.run_with(&args, document).await.ok().json(),
        json!({ "a": 1 })
    );
    let run = cli
        .run_with(&[&args[..], &["--raw"]].concat(), document)
        .await
        .ok();
    assert_eq!(run.json(), json!({ "data": { "a": 1 } }));
    let run = cli.run(&["graphql", "{ a }", "--variables", "[1]"]).await;
    assert_eq!(run.code, 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn errors_in_a_successful_answer_fail_and_keep_partial_data() {
    let server = MockServer::start().await;
    graphql(&server, |_, _| {
        json!({ "data": { "a": 1, "b": null }, "errors": [{ "message": "b broke" }, { "message": "more" }] })
    })
    .await;
    let cli = Cli::basic(&server);
    let run = cli.run(&["graphql", "{ a b }"]).await;
    assert_eq!(run.code, 1);
    assert_eq!(run.json()["a"], 1, "a pipe still gets the data");
    assert!(run.stderr.contains("b broke"));
    let run = cli.run(&["graphql", "{ a b }", "--agent"]).await;
    let payload = &run.envelope()["error"]["payload"];
    assert_eq!(payload["data"]["a"], 1);
    assert_eq!(payload["errors"][1]["message"], "more");
}

#[tokio::test(flavor = "multi_thread")]
async fn only_mutations_are_writes() {
    let server = MockServer::start().await;
    graphql(&server, |_, _| json!({ "data": {} })).await;
    let cli = Cli::basic(&server);
    cli.run(&["graphql", "{ a }", "--read-only"]).await.ok();
    let run = cli.run(&["graphql", "mutation { a }", "--read-only"]).await;
    assert_eq!(run.code, 3, "{}", run.stdout);
    let run = cli
        .run(&["graphql", "mutation { a }", "--dry-run", "--agent"])
        .await
        .ok();
    assert_eq!(run.envelope()["data"]["dry_run"], true);
}

#[tokio::test(flavor = "multi_thread")]
async fn gr_reads_related_fields_into_flat_rows() {
    let server = MockServer::start().await;
    graphql(&server, |document, body| {
        assert_eq!(
            document,
            "query ($qc: String!) { GlideRecord_Query { incident(queryConditions: $qc, pagination: { limit: 2, offset: 4 }) { _results { number { displayValue } caller_id { _reference { name { displayValue } manager { _reference { email { displayValue } } } } } } } } }"
        );
        assert_eq!(body["variables"], json!({ "qc": "active=true" }));
        records("incident", json!([
            { "number": { "displayValue": "INC1" }, "caller_id": { "_reference": { "name": { "displayValue": "Ada" }, "manager": { "_reference": null } } } },
            { "number": { "displayValue": "INC2" }, "caller_id": { "_reference": null } },
        ]))
    })
    .await;
    let run = Cli::basic(&server)
        .run(&[
            "gr",
            "incident",
            "-f",
            "number,caller_id.name,caller_id.manager.email",
            "-q",
            "active=true",
            "--setlimit",
            "2",
            "--offset",
            "4",
            "--agent",
        ])
        .await
        .ok();
    let envelope = run.envelope();
    assert_eq!(
        envelope["data"],
        json!([
            { "number": "INC1", "caller_id.name": "Ada", "caller_id.manager.email": null },
            { "number": "INC2", "caller_id.name": null, "caller_id.manager.email": null },
        ])
    );
    assert_eq!(envelope["meta"]["next_offset"], 6);
}

#[tokio::test(flavor = "multi_thread")]
async fn gr_counts_and_explains_wrong_names() {
    let server = MockServer::start().await;
    graphql(&server, |document, _| {
        if document.contains("not_a_table") {
            return json!({ "errors": [{ "message": "Validation error (UnknownArgument@[GlideRecord_Query/not_a_table]) : Unknown field argument 'pagination'" }] });
        }
        if document.contains("_reference") {
            return undefined("_reference");
        }
        assert!(document.contains("{ incident(pagination: { limit: 1, offset: 0 }) { _rowCount } }"), "{document}");
        records("incident", json!([]))
    })
    .await;
    let cli = Cli::basic(&server);
    assert_eq!(
        cli.run(&["gr", "incident", "--count"]).await.ok().json(),
        json!({ "count": 7 })
    );
    let run = cli.run(&["gr", "not_a_table", "--count"]).await;
    assert_eq!(run.code, 1);
    assert!(run.stderr.contains("not_a_table"), "{}", run.stderr);
    let run = cli.run(&["gr", "incident", "-f", "number.x"]).await;
    assert!(run.stderr.contains("not a reference"), "{}", run.stderr);
    for fields in ["number,", "a..b", "a b", "x{y}", "1a"] {
        let run = cli.run(&["gr", "incident", "-f", fields]).await;
        assert_eq!(run.code, 3, "{fields}");
    }
    assert_eq!(
        cli.run(&["gr", "incident"]).await.code,
        3,
        "fields or --count"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn aggregate_groups_as_a_table_and_as_the_server_sent_them() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/now/stats/incident"))
        .respond_with(|request: &Request| {
            let q = query(request);
            assert_eq!(q["sysparm_group_by"], "priority");
            assert_eq!(q["sysparm_avg_fields"], "reopen_count");
            assert_eq!(q["sysparm_having"], "count^priority^>^1");
            result(json!([{
                "stats": { "count": "3", "avg": { "reopen_count": "0.5" } },
                "groupby_fields": [{ "field": "priority", "value": "1" }],
            }]))
        })
        .mount(&server)
        .await;
    let cli = Cli::basic(&server);
    let args = [
        "aggregate",
        "incident",
        "--count",
        "--avg",
        "reopen_count",
        "-g",
        "priority",
        "--having",
        "count^priority^>^1",
    ];
    let run = cli.run(&args).await.ok();
    assert_eq!(run.json()[0]["stats"]["count"], "3");
    let run = cli.run(&[&args[..], &["-o", "csv"]].concat()).await.ok();
    assert_eq!(run.stdout, "priority,count,avg.reopen_count\n1,3,0.5\n");
    server.reset().await;
    Mock::given(method("GET"))
        .and(path("/api/now/stats/incident"))
        .respond_with(|request: &Request| {
            assert_eq!(
                query(request)["sysparm_count"],
                "true",
                "the default function"
            );
            result(json!({ "stats": { "count": "9" } }))
        })
        .mount(&server)
        .await;
    let run = cli.run(&["stats", "incident", "-o", "table"]).await.ok();
    assert_eq!(run.stdout, "COUNT\n9\n");
}

const STREAM: &str = "2026-08-11 15:40:37 - Ada Lovelace (Work notes)\nnewest\n\n2026-08-10 10:00:00 - Bob (Additional comments)\nolder\n";

#[tokio::test(flavor = "multi_thread")]
async fn the_journal_is_parsed_from_the_record() {
    let server = MockServer::start().await;
    graphql(&server, |document, body| {
        assert!(
            document.contains(
                "incident(sys_id: $id) { _results { comments_and_work_notes { displayValue }"
            ),
            "{document}"
        );
        assert_eq!(body["variables"]["id"], ID);
        records(
            "incident",
            json!([{ "comments_and_work_notes": { "displayValue": STREAM } }]),
        )
    })
    .await;
    let cli = Cli::basic(&server);
    let run = cli
        .run(&["journal", "incident", ID, "--limit", "1", "--agent"])
        .await
        .ok();
    assert_eq!(
        run.envelope()["data"],
        json!([{ "created_on": "2026-08-11 15:40:37", "author": "Ada Lovelace", "element": "work_notes", "label": "Work notes", "text": "newest" }])
    );
    let run = cli
        .run(&["journal", &format!("incident:{ID}"), "--raw"])
        .await
        .ok();
    assert_eq!(run.stdout, STREAM);
    let run = cli
        .run(&["journal", "incident", ID, "-o", "table"])
        .await
        .ok();
    assert!(
        run.stdout.contains("Bob (Additional comments)\n  older"),
        "{}",
        run.stdout
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_journal_copes_with_tables_that_lack_a_field() {
    let server = MockServer::start().await;
    graphql(&server, |document, _| {
        for missing in ["comments_and_work_notes", "comments"] {
            if document.contains(&format!(" {missing} {{")) {
                return undefined(missing);
            }
        }
        records(
            "u_thing",
            json!([{ "work_notes": { "displayValue": STREAM } }]),
        )
    })
    .await;
    let cli = Cli::basic(&server);
    let run = cli.run(&["journal", "u_thing", ID]).await.ok();
    assert_eq!(run.json().as_array().map(Vec::len), Some(2));
    let run = cli.run(&["journal", "u_thing", ID, "--comments"]).await;
    assert_eq!(run.code, 1);
    assert!(run.stderr.contains("no comments field"), "{}", run.stderr);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_entry_table_says_when_access_rules_hide_the_rows() {
    let server = MockServer::start().await;
    graphql(&server, |_, body| {
        assert_eq!(
            body["variables"]["qc"],
            format!("element_id={ID}^element=work_notes^ORDERBYDESCsys_created_on")
        );
        records("sys_journal_field", json!([]))
    })
    .await;
    let run = Cli::basic(&server)
        .run(&[
            "journal",
            "incident",
            ID,
            "--source",
            "table",
            "--work-notes",
        ])
        .await;
    assert_eq!(run.code, 1);
    assert!(run.stderr.contains("7 journal entries"), "{}", run.stderr);
}

/// The variables of a request item, which the PUT changes.
async fn variables(server: &MockServer, after: &'static str) {
    let row = |value: &str| {
        json!([
            { "request_item": RITM, "sc_item_option.item_option_new.name": "size", "sc_item_option.item_option_new.question_text": "Size", "sc_item_option.value": value },
            { "request_item": RITM, "sc_item_option.item_option_new.name": "color", "sc_item_option.value": "red" },
        ])
    };
    let at = "/api/now/table/sc_item_option_mtom";
    Mock::given(method("GET"))
        .and(path(at))
        .respond_with(result(row("M")))
        .up_to_n_times(1)
        .mount(server)
        .await;
    on(server, "GET", at, result(row(after))).await;
    on(
        server,
        "GET",
        &format!("/api/now/table/sc_task/{ID}"),
        result(json!({ "request_item": RITM })),
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn variables_are_written_through_the_request_item_and_verified() {
    let server = MockServer::start().await;
    variables(&server, "L").await;
    Mock::given(method("PUT"))
        .and(path(format!(
            "/api/sn_sc/servicecatalog/variables/sc_req_item/{RITM}"
        )))
        .and(body_json(json!({ "size": "L", "color": "red" })))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    let run = Cli::basic(&server)
        .run(&[
            "variables",
            "set",
            "sc_task",
            ID,
            "-F",
            "size=L",
            "-F",
            "color=red",
            "--agent",
        ])
        .await
        .ok();
    assert_eq!(
        run.envelope()["data"],
        json!({
            "table": "sc_req_item", "sys_id": RITM,
            "resolved_from": { "table": "sc_task", "sys_id": ID },
            "updated": { "size": { "from": "M", "to": "L" } }, "unchanged": { "color": "red" },
        })
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn variables_that_do_not_exist_or_do_not_stick_are_errors() {
    let server = MockServer::start().await;
    variables(&server, "M").await;
    let cli = Cli::basic(&server);
    let run = cli
        .run(&["variables", "set", "sc_task", ID, "-F", "Size=L", "--agent"])
        .await;
    assert_eq!(run.code, 3, "no PUT is mounted");
    assert_eq!(
        run.envelope()["error"]["input"]["choices"][0]["value"],
        "color"
    );
    on(
        &server,
        "PUT",
        &format!("/api/sn_sc/servicecatalog/variables/sc_req_item/{RITM}"),
        ResponseTemplate::new(200),
    )
    .await;
    let run = cli
        .run(&["variables", "set", "sc_task", ID, "-F", "size=L", "--agent"])
        .await;
    assert_eq!(run.code, 1);
    assert_eq!(
        run.envelope()["error"]["payload"],
        json!({ "size": { "requested": "L", "stored": "M" } })
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_record_without_variables_must_exist() {
    let server = MockServer::start().await;
    on(
        &server,
        "GET",
        "/api/now/table/question_answer",
        result(json!([])),
    )
    .await;
    on(
        &server,
        "GET",
        &format!("/api/now/table/incident/{ID}"),
        ResponseTemplate::new(404)
            .set_body_json(json!({ "error": { "message": "No Record found" } })),
    )
    .await;
    let cli = Cli::basic(&server);
    assert_eq!(cli.run(&["variables", "get", "incident", ID]).await.code, 4);
    server.reset().await;
    // The instance ignored the filter.
    on(
        &server,
        "GET",
        "/api/now/table/question_answer",
        result(json!([{ "table_sys_id": RITM, "question.name": "x", "value": "1" }])),
    )
    .await;
    assert_eq!(cli.run(&["variables", "get", "incident", ID]).await.code, 1);
}

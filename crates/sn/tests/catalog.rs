//! `catalog`: browsing, the cart, and orders that never claim more than
//! happened.

mod support;

use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use support::{Cli, ME, Run, mount_caller, mount_me, on, query, result};
use wiremock::matchers::{body_json, method, path, query_param};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const BASE: &str = "/api/sn_sc/servicecatalog";
const AWS: &str = "5a1efa5947520510f53d37d2846d439b";
const ZOOM: &str = "54c9ead947dec110f53d37d2846d43ea";
const USER: &str = "62826bf03710200044e0bfc8bcbe5df1";
const CART: &str = "90ed51b23b50dbd0b8094b44c3e45aad";
const LINE: &str = "346489831b181710b97aa975624bcb75";
const OTHER: &str = "456489831b181710b97aa975624bcbb2";
const REQUEST: &str = "b54ae46e1b181310b97aa975624bcbf2";
const VARS: &[&str] = &[
    "--var",
    "division=IT",
    "--var",
    "environment=dev",
    "--var",
    "describe_briefly_the_purpose_of_this_account=Sandbox",
];

fn fixture(name: &str) -> Value {
    let path = format!(
        "{}/tests/fixtures/catalog/{name}.json",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_str(&std::fs::read_to_string(path).expect("fixture")).expect("JSON")
}

async fn item(server: &MockServer, name: &str) {
    let document = fixture(name);
    let id = document["result"]["sys_id"].as_str().expect("sys_id");
    let at = format!("{BASE}/items/{id}");
    on(
        server,
        "GET",
        &at,
        ResponseTemplate::new(200).set_body_json(&document),
    )
    .await;
}

/// The Zoom item with nothing mandatory.
async fn optional_zoom(server: &MockServer) {
    fn relax(node: &mut Value) {
        match node {
            Value::Object(map) => {
                if map.contains_key("mandatory") {
                    map.insert("mandatory".into(), false.into());
                }
                map.values_mut().for_each(relax);
            }
            Value::Array(items) => items.iter_mut().for_each(relax),
            _ => {}
        }
    }
    let mut document = fixture("zoom_webinar");
    relax(&mut document);
    let at = format!("{BASE}/items/{ZOOM}");
    on(
        server,
        "GET",
        &at,
        ResponseTemplate::new(200).set_body_json(document),
    )
    .await;
}

fn line(id: &str, name: &str, item: &str) -> Value {
    json!({ "cart_item_id": id, "item_name": name, "quantity": "1", "can_order": true, "catalog_item_id": item })
}

fn cart(lines: &[Value]) -> Value {
    json!({ "cart_id": CART, "none": { "items": lines }, "subtotal_price": "-" })
}

async fn on_cart(server: &MockServer, lines: &[Value]) {
    on(server, "GET", &format!("{BASE}/cart"), result(cart(lines))).await;
}

/// The cart has `first` once and `then` afterwards.
async fn changing_cart(server: &MockServer, first: &[Value], then: &[Value]) {
    Mock::given(method("GET"))
        .and(path(format!("{BASE}/cart")))
        .respond_with(result(cart(first)))
        .up_to_n_times(1)
        .mount(server)
        .await;
    on_cart(server, then).await;
}

/// Any answer to a write that must happen once.
async fn once(server: &MockServer, verb: &str, at: String, answer: ResponseTemplate) {
    Mock::given(method(verb))
        .and(path(at))
        .respond_with(answer)
        .expect(1)
        .mount(server)
        .await;
}

fn order_at(item: &str) -> String {
    format!("{BASE}/items/{item}/order_now")
}

fn refusal(status: u16, message: &str) -> ResponseTemplate {
    ResponseTemplate::new(status).set_body_json(
        json!({ "error": { "message": message, "detail": "" }, "status": "failure" }),
    )
}

fn request_answer() -> ResponseTemplate {
    result(json!({
        "sys_id": REQUEST, "number": "REQ0010002", "request_number": "REQ0010002",
        "request_id": REQUEST, "table": "sc_request",
    }))
}

async fn requested_items(server: &MockServer) {
    on(
        server,
        "GET",
        "/api/now/table/sc_req_item",
        result(json!([{ "number": "RITM0010002", "sys_id": OTHER, "request": REQUEST }])),
    )
    .await;
}

async fn no_writes(server: &MockServer) {
    let requests = server.received_requests().await.expect("requests");
    let writes: Vec<&Request> = requests
        .iter()
        .filter(|r| r.method.as_str() != "GET")
        .collect();
    assert!(writes.is_empty(), "{writes:?}");
}

async fn order(server: &MockServer, extra: &[&str]) -> Run {
    Cli::basic(server)
        .run(&[&["catalog", "order", AWS, "--agent"], VARS, extra].concat())
        .await
}

#[tokio::test(flavor = "multi_thread")]
async fn search_and_show() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("{BASE}/items")))
        .and(query_param("sysparm_text", "laptop"))
        .and(query_param("sysparm_limit", "20"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("items")))
        .mount(&server)
        .await;
    item(&server, "zoom_webinar").await;
    let at = format!("{BASE}/items/{LINE}");
    on(
        &server,
        "GET",
        &at,
        refusal(400, "security constraints prevent access"),
    )
    .await;
    let cli = Cli::basic(&server);
    let rows = cli.run(&["catalog", "search", "laptop"]).await.ok().json();
    assert_eq!(rows.as_array().map(Vec::len), Some(10));
    assert!(
        rows[0]["kind"].is_string() && rows[0]["orderable"].is_boolean(),
        "{rows}"
    );
    let run = cli.run(&["catalog", "show", ZOOM, "--agent"]).await.ok();
    let data = &run.envelope()["data"];
    assert_eq!(data["orderable"], true);
    assert_eq!(data["client_logic"], true);
    let hosts = data["variables"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|v| v["name"] == "add_co_hosts")
        .expect("variable");
    assert_eq!(hosts["reference_table"], "sys_user");
    let run = cli
        .run(&["catalog", "show", ZOOM, "-o", "table"])
        .await
        .ok();
    assert!(run.stdout.contains("-> sys_user"), "{}", run.stdout);
    let run = cli.run(&["catalog", "show", LINE]).await;
    assert_eq!(run.code, 4, "{}", run.stderr);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_order_is_sent_once_and_names_its_request() {
    let server = MockServer::start().await;
    item(&server, "aws_account_request").await;
    requested_items(&server).await;
    Mock::given(method("POST"))
        .and(path(order_at(AWS)))
        .and(body_json(json!({
            "sysparm_quantity": "2",
            "variables": {
                "division": "IT", "environment": "Dev",
                "describe_briefly_the_purpose_of_this_account": "Sandbox",
            },
        })))
        .respond_with(request_answer())
        .expect(1)
        .mount(&server)
        .await;
    let run = order(&server, &["--yes", "--quantity", "2"]).await.ok();
    let data = &run.envelope()["data"];
    assert_eq!(data["request"]["number"], "REQ0010002");
    assert_eq!(data["requested_items"][0]["number"], "RITM0010002");
    assert_eq!(
        data["url"],
        format!("{}/sc_request.do?sys_id={REQUEST}", server.uri())
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_order_can_be_for_someone_else() {
    let server = MockServer::start().await;
    item(&server, "aws_account_request").await;
    requested_items(&server).await;
    on(
        &server,
        "GET",
        "/api/now/table/sys_user",
        result(json!([{ "sys_id": USER, "user_name": "abel" }])),
    )
    .await;
    Mock::given(method("POST"))
        .and(path(order_at(AWS)))
        .respond_with(|request: &Request| {
            let body: Value = serde_json::from_slice(&request.body).expect("body");
            assert_eq!(body["sysparm_requested_for"], USER);
            request_answer()
        })
        .expect(1)
        .mount(&server)
        .await;
    order(&server, &["--yes", "--requested-for", "abel"])
        .await
        .ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn nothing_is_sent_without_a_confirmation_or_with_dry_run_or_read_only() {
    let server = MockServer::start().await;
    item(&server, "aws_account_request").await;
    let run = order(&server, &[]).await;
    assert_eq!(run.envelope()["error"]["code"], "confirmation_required");
    let run = order(&server, &["--yes", "--read-only"]).await;
    assert_eq!(run.envelope()["error"]["code"], "read_only");
    let run = order(&server, &["--dry-run"]).await.ok();
    let data = &run.envelope()["data"];
    assert_eq!(data["dry_run"], true);
    assert_eq!(data["item"]["name"], "AWS account request");
    assert_eq!(data["variables"]["environment"], "Dev");
    no_writes(&server).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn every_problem_is_reported_before_ordering() {
    let server = MockServer::start().await;
    item(&server, "aws_account_request").await;
    let run = Cli::basic(&server)
        .run(&[
            "catalog",
            "order",
            AWS,
            "--var",
            "division=Bogus",
            "--var",
            "Environment=Dev",
            "--yes",
            "--agent",
        ])
        .await;
    assert_eq!(run.code, 3);
    let error = &run.envelope()["error"];
    assert_eq!(error["code"], "invalid");
    let problems = error["data"]["problems"].to_string();
    for part in [
        "Bogus",
        "did you mean `environment`",
        "environment: required",
    ] {
        assert!(problems.contains(part), "{problems}");
    }
    no_writes(&server).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_name_must_be_exact_and_unique_to_be_ordered() {
    let server = MockServer::start().await;
    item(&server, "aws_account_request").await;
    let hit = |id: &str, name: &str| json!({ "sys_id": id, "name": name, "sys_class_name": "sc_cat_item" });
    Mock::given(method("GET"))
        .and(path(format!("{BASE}/items")))
        .respond_with(move |request: &Request| {
            let q = query(request);
            let offset = q.get("sysparm_offset").map(String::as_str);
            match (q["sysparm_text"].as_str(), offset) {
                ("aws account REQUEST", _) => result(json!([
                    hit(AWS, "AWS account request"),
                    hit(OTHER, "AWS account request form")
                ])),
                ("account", _) => result(json!([hit(AWS, "AWS account request")])),
                // The second item of this name is on the second page.
                ("twice", Some("100")) => result(json!([hit(OTHER, "Twice")])),
                ("twice", _) => {
                    let mut page = vec![hit(LINE, "x"); 99];
                    page.push(hit(AWS, "Twice"));
                    result(page.into())
                }
                _ => result(json!([])),
            }
        })
        .mount(&server)
        .await;
    let cli = Cli::basic(&server);
    let try_order = async |name: &str| {
        cli.run(&[&["catalog", "order", name, "--dry-run", "--agent"], VARS].concat())
            .await
    };
    try_order("aws account REQUEST").await.ok();
    let fuzzy = try_order("account").await;
    assert_eq!(
        fuzzy.envelope()["error"]["input"]["choices"][0]["value"],
        AWS
    );
    cli.run(&["catalog", "show", "account"]).await.ok();
    let twice = try_order("twice").await;
    assert_eq!(twice.code, 3);
    assert_eq!(
        twice.envelope()["error"]["input"]["choices"]
            .as_array()
            .map(Vec::len),
        Some(2)
    );
    assert_eq!(try_order("nothing").await.code, 4);
}

#[tokio::test(flavor = "multi_thread")]
async fn references_are_confirmed_and_dates_need_a_known_format() {
    let server = MockServer::start().await;
    item(&server, "zoom_webinar").await;
    Mock::given(method("GET"))
        .and(path("/api/now/table/sys_user"))
        .respond_with(|request: &Request| {
            match query(request)["sysparm_query"].as_str() {
                q if q.starts_with("user_name=abel") => {
                    result(json!([{ "sys_id": USER, "user_name": "abel" }]))
                }
                q if q == format!("sys_id={USER}") => result(json!([{ "sys_id": USER }])),
                // A record that is not the one asked for.
                _ => result(json!([{ "sys_id": OTHER, "user_name": "other" }])),
            }
        })
        .mount(&server)
        .await;
    mount_caller(&server).await;
    let me = format!("/api/now/table/sys_user/{ME}");
    let format = |date: &str| result(json!({ "date_format": date, "time_format": "" }));
    let property = result(json!([{ "name": "glide.sys.time_format", "value": "HH:mm:ss" }]));
    on(&server, "GET", "/api/now/table/sys_properties", property).await;
    let cli = Cli::basic(&server);
    let times = [
        "--var",
        "topic=Launch",
        "--var",
        "select_webinar_start_time=2026-12-01 10:00:00",
        "--var",
        "select_webinar_end_time=2026-12-01 11:00:00",
        "--var",
    ];
    let args = async |hosts: &str, extra: &[&str]| {
        let hosts = format!("add_co_hosts={hosts}");
        let args = [
            &["catalog", "order", ZOOM, "--agent"],
            &times[..],
            &[hosts.as_str()],
            extra,
        ]
        .concat();
        cli.run(&args).await
    };
    let unreadable = args("abel", &["--yes"]).await;
    assert!(
        unreadable.stdout.contains("--allow-unverified-formats"),
        "{}",
        unreadable.stdout
    );
    let preview = args("abel", &["--dry-run"]).await.ok();
    let data = &preview.envelope()["data"];
    assert_eq!(data["variables"]["add_co_hosts"], USER);
    assert!(
        data["warnings"]
            .to_string()
            .contains("--allow-unverified-formats")
    );
    let unknown = args(&format!("abel,{LINE}"), &["--dry-run"]).await;
    assert!(unknown.stdout.contains(LINE), "{}", unknown.stdout);
    assert_eq!(unknown.code, 3);

    Mock::given(method("GET"))
        .and(path(me.clone()))
        .respond_with(format("dd-MM-yyyy"))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    let wrong = args("abel", &["--yes"]).await;
    assert!(wrong.stdout.contains("dd-MM-yyyy"), "{}", wrong.stdout);
    on(&server, "GET", &me, format("yyyy-MM-dd")).await;
    once(&server, "POST", order_at(ZOOM), request_answer()).await;
    requested_items(&server).await;
    args("abel", &["--yes"]).await.ok();
}

/// Ten names in a list collector: one read each, eight at a time, and the
/// problems in the order of the names.
#[tokio::test(flavor = "multi_thread")]
async fn names_of_a_list_are_confirmed_together() {
    let server = MockServer::start().await;
    optional_zoom(&server).await;
    let delay = Duration::from_millis(300);
    let arrivals: Arc<Mutex<Vec<Instant>>> = Arc::default();
    let seen = Arc::clone(&arrivals);
    Mock::given(method("GET"))
        .and(path("/api/now/table/sys_user"))
        .respond_with(move |request: &Request| {
            seen.lock().expect("lock").push(Instant::now());
            let filter = query(request)["sysparm_query"].clone();
            // A read by name, and one by the sys_id that it found.
            let number = filter
                .trim_start_matches("user_name=u")
                .trim_start_matches("sys_id=")
                .trim_start_matches('0')
                .split('^')
                .next()
                .unwrap_or_default()
                .to_owned();
            match number.as_str() {
                "3" => result(json!([])).set_delay(delay * 2),
                "7" => result(json!([])),
                n => {
                    let name = format!("u{}", n.parse::<u32>().unwrap_or(0));
                    result(json!([{ "sys_id": format!("{n:0>32}"), "user_name": name }]))
                        .set_delay(delay)
                }
            }
        })
        .mount(&server)
        .await;
    let cli = Cli::basic(&server);
    let order = async |from: u32, to: u32, skip: &[u32]| {
        arrivals.lock().expect("lock").clear();
        let names: Vec<String> = (from..to)
            .filter(|n| !skip.contains(n))
            .map(|n| format!("u{n}"))
            .collect();
        let hosts = format!("add_co_hosts={}", names.join(","));
        cli.run(&[
            "catalog",
            "order",
            ZOOM,
            "--var",
            &hosts,
            "--dry-run",
            "--agent",
        ])
        .await
    };
    let run = order(10, 20, &[]).await.ok();
    let ids: Vec<String> = (10..20).map(|n| format!("{n:0>32}")).collect();
    assert_eq!(
        run.envelope()["data"]["variables"]["add_co_hosts"],
        ids.join(",")
    );
    let at = arrivals.lock().expect("lock").clone();
    assert_eq!(at.len(), 10, "a name that was found is not read again");
    assert!(
        at[8] - at[0] >= delay - Duration::from_millis(20),
        "bounded"
    );
    assert!(at[9] - at[0] < delay * 5, "not one after the other");

    let run = order(0, 10, &[0]).await;
    assert_eq!(run.code, 3);
    let problems = run.envelope()["error"]["data"]["problems"].clone();
    assert_eq!(problems.as_array().map(Vec::len), Some(2), "{problems}");
    assert!(
        problems[0].to_string().contains("`u3`") && problems[1].to_string().contains("`u7`"),
        "{problems}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_order_that_only_fills_a_cart_is_not_an_order() {
    let server = MockServer::start().await;
    item(&server, "aws_account_request").await;
    once(
        &server,
        "POST",
        order_at(AWS),
        result(json!({ "cart_id": CART })),
    )
    .await;
    let run = order(&server, &["--yes"]).await;
    assert_eq!(run.code, 1);
    let error = &run.envelope()["error"];
    assert_eq!(error["code"], "not_ordered");
    assert_eq!(error["data"]["cart"]["sys_id"], CART);
    assert!(error["message"].to_string().contains("--checkout"));
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unclear_outcome_says_that_the_order_may_exist() {
    for answer in [
        refusal(503, "unavailable"),
        ResponseTemplate::new(200).set_body_string("<html>proxy</html>"),
        result(json!({ "status": "hunter2", "request_number": "REQ0010009" })),
        result(json!({})),
    ] {
        let server = MockServer::start().await;
        item(&server, "aws_account_request").await;
        // Once: a server error must not be repeated.
        once(&server, "POST", order_at(AWS), answer).await;
        let run = order(&server, &["--yes"]).await;
        assert_ne!(run.code, 0);
        let error = &run.envelope()["error"];
        assert_ne!(error["code"], "not_ordered");
        let message = error["message"].to_string();
        assert!(message.contains("may have been placed"), "{message}");
        assert!(!message.contains("hunter2"), "{message}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refused_order_gives_the_reason_and_no_doubt() {
    let server = MockServer::start().await;
    item(&server, "aws_account_request").await;
    once(
        &server,
        "POST",
        order_at(AWS),
        refusal(400, "Mandatory Variables are required"),
    )
    .await;
    let run = order(&server, &["--yes"]).await;
    assert!(
        run.stdout.contains("Mandatory Variables are required"),
        "{}",
        run.stdout
    );
    assert!(
        !run.stdout.contains("may have been placed"),
        "{}",
        run.stdout
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_lookup_after_the_order_is_not_a_failed_order() {
    let server = MockServer::start().await;
    item(&server, "aws_account_request").await;
    once(&server, "POST", order_at(AWS), request_answer()).await;
    on(
        &server,
        "GET",
        "/api/now/table/sc_req_item",
        ResponseTemplate::new(403),
    )
    .await;
    let run = order(&server, &["--yes"]).await.ok();
    let envelope = run.envelope();
    assert_eq!(envelope["data"]["requested_items"], Value::Null);
    assert!(
        envelope["meta"]["warnings"]
            .to_string()
            .contains("do not order again")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn errors_never_show_a_secret() {
    let quoted = "Value hunter2\"x is wrong";
    for (answer, shown) in [
        (
            refusal(400, "hunter2 is too short"),
            "[REDACTED] is too short",
        ),
        (refusal(400, "bad: {\\\"p\\\":1}"), "encoded text"),
        (refusal(400, "hunter&#50;"), "encoded text"),
        (refusal(400, quoted), "[REDACTED]"),
        (
            ResponseTemplate::new(500).set_body_string("<p>hunter2</p>"),
            "withheld",
        ),
    ] {
        let server = MockServer::start().await;
        optional_zoom(&server).await;
        once(&server, "POST", order_at(ZOOM), answer).await;
        let run = Cli::basic(&server)
            .run_with(
                &[
                    "catalog",
                    "order",
                    ZOOM,
                    "--vars-file",
                    "-",
                    "--yes",
                    "--agent",
                ],
                Some("set_password: hunter2\n"),
            )
            .await;
        assert_ne!(run.code, 0);
        assert!(run.stdout.contains(shown), "{shown}: {}", run.stdout);
        assert!(
            !run.stdout.contains("hunter2") && !run.stderr.contains("hunter2"),
            "{}",
            run.stdout
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_preview_never_shows_a_secret() {
    let server = MockServer::start().await;
    optional_zoom(&server).await;
    let cli = Cli::basic(&server);
    for format in ["json", "table"] {
        let run = cli
            .run_with(
                &[
                    "catalog",
                    "order",
                    ZOOM,
                    "--vars-file",
                    "-",
                    "--dry-run",
                    "-o",
                    format,
                ],
                Some("set_password: hunter2\ntopic: x\n"),
            )
            .await
            .ok();
        assert!(
            run.stdout.contains("[REDACTED]") && !run.stdout.contains("hunter2"),
            "{}",
            run.stdout
        );
    }
    let run = cli
        .run(&[
            "catalog",
            "order",
            ZOOM,
            "--var",
            "set_password=hunter2",
            "--dry-run",
        ])
        .await;
    assert_eq!(run.code, 3);
    assert!(!run.stderr.contains("hunter2"), "{}", run.stderr);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_raw_body_goes_out_as_it_is() {
    let server = MockServer::start().await;
    requested_items(&server).await;
    Mock::given(method("POST"))
        .and(path(order_at(AWS)))
        .and(body_json(
            json!({ "sysparm_quantity": "1", "variables": { "any": "thing" } }),
        ))
        .respond_with(request_answer())
        .expect(1)
        .mount(&server)
        .await;
    let cli = Cli::basic(&server);
    let body = r#"{"sysparm_quantity":"1","variables":{"any":"thing"}}"#;
    let args = ["catalog", "order", AWS, "--data", body, "--agent"];
    let run = cli.run(&args).await;
    assert_eq!(run.envelope()["error"]["code"], "confirmation_required");
    cli.run(&[&args[..], &["--yes"]].concat()).await.ok();
    for extra in [
        vec!["--var", "a=b"],
        vec!["--checkout"],
        vec!["--quantity", "2"],
    ] {
        assert_eq!(
            cli.run(&[&args[..], &extra[..]].concat()).await.code,
            3,
            "{extra:?}"
        );
    }
    let run = cli
        .run(&["catalog", "order", "Some name", "--data", "{}", "--yes"])
        .await;
    assert_eq!(run.code, 3);
}

/// `--checkout`: an empty cart, then `after`.
async fn checkout(server: &MockServer, after: &[Value]) {
    item(server, "aws_account_request").await;
    changing_cart(server, &[], after).await;
    let added = json!({ "cart_id": CART, "items": after });
    once(
        server,
        "POST",
        format!("{BASE}/items/{AWS}/add_to_cart"),
        result(added),
    )
    .await;
}

fn submit_at() -> String {
    format!("{BASE}/cart/submit_order")
}

#[tokio::test(flavor = "multi_thread")]
async fn checkout_adds_the_item_and_submits_the_cart() {
    let server = MockServer::start().await;
    // Ids are compared in any case.
    checkout(
        &server,
        &[line(LINE, "AWS account request", &AWS.to_uppercase())],
    )
    .await;
    once(&server, "POST", submit_at(), request_answer()).await;
    requested_items(&server).await;
    let run = order(&server, &["--checkout", "--yes"]).await.ok();
    assert_eq!(
        run.envelope()["data"]["requested_items"][0]["number"],
        "RITM0010002"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn checkout_needs_an_empty_cart_a_confirmation_and_your_own_order() {
    let server = MockServer::start().await;
    item(&server, "aws_account_request").await;
    changing_cart(&server, &[line(OTHER, "Zoom", ZOOM)], &[]).await;
    for extra in ["--yes", "--dry-run"] {
        let run = order(&server, &["--checkout", extra]).await;
        if extra == "--yes" {
            let error = &run.envelope()["error"];
            assert_eq!(error["code"], "conflict");
            assert!(error["message"].to_string().contains("Zoom"));
        } else {
            assert_eq!(run.ok().envelope()["data"]["checkout"], true);
        }
    }
    let run = order(&server, &["--checkout"]).await;
    assert_eq!(run.envelope()["error"]["code"], "confirmation_required");
    let before = server.received_requests().await.expect("requests").len();
    let run = order(&server, &["--checkout", "--requested-for", "abel", "--yes"]).await;
    assert_eq!(run.code, 3);
    let run = order(&server, &["--checkout", "--yes", "--read-only"]).await;
    assert_eq!(run.envelope()["error"]["code"], "read_only");
    let after = server.received_requests().await.expect("requests").len();
    assert_eq!(before, after, "refused before anything was read");
    no_writes(&server).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn checkout_never_submits_a_cart_that_it_does_not_know() {
    for after in [
        vec![
            line(LINE, "AWS account request", AWS),
            line(OTHER, "Zoom", ZOOM),
        ],
        vec![line(OTHER, "Zoom", ZOOM)],
    ] {
        let server = MockServer::start().await;
        checkout(&server, &after).await;
        let run = order(&server, &["--checkout", "--yes"]).await;
        let error = &run.envelope()["error"];
        assert_eq!(error["code"], "not_ordered");
        assert_eq!(error["data"]["cart"]["sys_id"], CART);
        assert!(error["message"].to_string().contains("still in your cart"));
    }
    for added in [
        result(json!({ "cart_id": CART })),
        result(json!({ "x": 1 })),
    ] {
        let server = MockServer::start().await;
        item(&server, "aws_account_request").await;
        on_cart(&server, &[]).await;
        once(
            &server,
            "POST",
            format!("{BASE}/items/{AWS}/add_to_cart"),
            added,
        )
        .await;
        let run = order(&server, &["--checkout", "--yes"]).await;
        let error = &run.envelope()["error"];
        assert_eq!(error["code"], "api");
        assert!(
            error["message"].to_string().contains("may be in your cart"),
            "{error}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_submission_says_what_is_known() {
    let added = line(LINE, "AWS account request", AWS);
    // Refused, and the item is still there: nothing was ordered.
    let server = MockServer::start().await;
    checkout(&server, std::slice::from_ref(&added)).await;
    once(&server, "POST", submit_at(), refusal(400, "Cart is empty!")).await;
    let run = order(&server, &["--checkout", "--yes"]).await;
    assert_eq!(run.envelope()["error"]["code"], "not_ordered");
    assert!(run.stdout.contains("Cart is empty!"), "{}", run.stdout);

    // Refused, but the item is gone: someone else ordered or removed it.
    let server = MockServer::start().await;
    item(&server, "aws_account_request").await;
    on_cart(&server, &[]).await;
    let answer = json!({ "cart_id": CART, "items": [added] });
    once(
        &server,
        "POST",
        format!("{BASE}/items/{AWS}/add_to_cart"),
        result(answer),
    )
    .await;
    once(&server, "POST", submit_at(), refusal(400, "Cart is empty!")).await;
    let run = order(&server, &["--checkout", "--yes"]).await;
    assert_eq!(run.envelope()["error"]["code"], "api");
    assert!(
        run.stdout.contains("ordered or removed elsewhere"),
        "{}",
        run.stdout
    );

    // A server error may have ordered.
    let server = MockServer::start().await;
    checkout(&server, &[added]).await;
    once(&server, "POST", submit_at(), refusal(503, "unavailable")).await;
    let run = order(&server, &["--checkout", "--yes"]).await;
    assert_eq!(run.envelope()["error"]["code"], "api");
    assert!(
        run.stdout.contains("may have been placed"),
        "{}",
        run.stdout
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_cart_shows_removes_and_clears() {
    let server = MockServer::start().await;
    on_cart(
        &server,
        &[line(LINE, "AWS", AWS), line(OTHER, "Zoom", ZOOM)],
    )
    .await;
    once(
        &server,
        "DELETE",
        format!("{BASE}/cart/{LINE}"),
        ResponseTemplate::new(204),
    )
    .await;
    once(
        &server,
        "DELETE",
        format!("{BASE}/cart/{OTHER}"),
        refusal(400, "no such item"),
    )
    .await;
    once(
        &server,
        "DELETE",
        format!("{BASE}/cart/{CART}/empty"),
        ResponseTemplate::new(204),
    )
    .await;
    let cli = Cli::basic(&server);
    let run = cli.run(&["catalog", "cart"]).await.ok();
    assert_eq!(run.json()["items"][0]["frequency"], "none");
    let run = cli
        .run(&["catalog", "cart", "show", "-o", "table"])
        .await
        .ok();
    assert!(
        run.stdout.contains(&format!("{LINE}  1 x AWS")),
        "{}",
        run.stdout
    );
    let run = cli.run(&["catalog", "cart", "--raw"]).await.ok();
    assert_eq!(run.json()["result"]["subtotal_price"], "-");
    let run = cli
        .run(&["catalog", "cart", "remove", LINE, "--agent"])
        .await;
    assert_eq!(run.envelope()["error"]["code"], "confirmation_required");
    cli.run(&["catalog", "cart-remove", &LINE.to_uppercase(), "--yes"])
        .await
        .ok();
    for gone in [OTHER, REQUEST] {
        assert_eq!(
            cli.run(&["catalog", "cart", "remove", gone, "--yes"])
                .await
                .code,
            4
        );
    }
    let run = cli
        .run(&["catalog", "cart", "clear", "--yes", "--agent"])
        .await
        .ok();
    assert_eq!(run.envelope()["data"]["items_removed"], 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_empty_cart_needs_no_confirmation_to_clear_and_cannot_be_submitted() {
    let server = MockServer::start().await;
    on_cart(&server, &[]).await;
    let cli = Cli::basic(&server);
    let run = cli.run(&["catalog", "cart", "clear", "--agent"]).await.ok();
    assert_eq!(run.envelope()["data"]["items_removed"], 0);
    assert_eq!(
        cli.run(&["catalog", "cart", "submit", "--yes"]).await.code,
        3
    );
    let run = cli
        .run(&[
            "catalog",
            "cart",
            "submit",
            "--yes",
            "--read-only",
            "--agent",
        ])
        .await;
    assert_eq!(run.envelope()["error"]["code"], "read_only");
    no_writes(&server).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_cart_is_submitted_unless_it_changed() {
    let server = MockServer::start().await;
    item(&server, "aws_account_request").await;
    on_cart(&server, &[line(LINE, "AWS", AWS)]).await;
    once(&server, "POST", submit_at(), request_answer()).await;
    requested_items(&server).await;
    let cli = Cli::basic(&server);
    let run = cli.run(&["catalog", "cart", "submit", "--agent"]).await;
    assert_eq!(run.envelope()["error"]["code"], "confirmation_required");
    cli.run(&["catalog", "cart", "submit", "--yes"]).await.ok();

    let mut more = line(LINE, "AWS", AWS);
    more["quantity"] = "2".into();
    for verb in ["submit", "clear"] {
        let server = MockServer::start().await;
        item(&server, "aws_account_request").await;
        changing_cart(
            &server,
            &[line(LINE, "AWS", AWS)],
            std::slice::from_ref(&more),
        )
        .await;
        let run = Cli::basic(&server)
            .run(&["catalog", "cart", verb, "--yes", "--agent"])
            .await;
        assert_eq!(run.envelope()["error"]["code"], "conflict", "{verb}");
        no_writes(&server).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_submission_error_is_withheld_when_the_cart_may_hold_secrets() {
    for (id, withheld) in [(AWS, false), (ZOOM, true), (OTHER, true)] {
        let server = MockServer::start().await;
        item(&server, "aws_account_request").await;
        item(&server, "zoom_webinar").await;
        on_cart(&server, &[line(LINE, "x", id)]).await;
        once(
            &server,
            "POST",
            submit_at(),
            refusal(400, "hunter2 is wrong"),
        )
        .await;
        let run = Cli::basic(&server)
            .run(&["catalog", "cart", "submit", "--yes", "--agent"])
            .await;
        assert_ne!(run.code, 0);
        assert_eq!(
            !run.stdout.contains("hunter2"),
            withheld,
            "{id}: {}",
            run.stdout
        );
    }
    let server = MockServer::start().await;
    item(&server, "aws_account_request").await;
    on_cart(&server, &[line(LINE, "x", AWS)]).await;
    once(
        &server,
        "POST",
        submit_at(),
        result(json!({ "cart_id": CART })),
    )
    .await;
    let run = Cli::basic(&server)
        .run(&["catalog", "cart", "submit", "--yes", "--agent"])
        .await;
    assert_eq!(
        run.envelope()["error"]["code"],
        "api",
        "a cart as the answer proves nothing"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn requested_items_are_yours() {
    let server = MockServer::start().await;
    mount_me(&server).await;
    Mock::given(method("GET"))
        .and(path("/api/now/table/sc_req_item"))
        .respond_with(|request: &Request| {
            assert_eq!(
                query(request)["sysparm_query"],
                format!("requested_for={ME}^ORopened_by={ME}^active=true^ORDERBYDESCsys_created_on")
            );
            result(json!([{ "number": "RITM1", "requested_for.sys_id": OTHER, "opened_by.sys_id": ME }]))
        })
        .mount(&server)
        .await;
    let cli = Cli::basic(&server);
    let run = cli
        .run(&["catalog", "requests", "mine", "--open", "--agent"])
        .await
        .ok();
    assert_eq!(run.envelope()["data"], json!([{ "number": "RITM1" }]));
    server.reset().await;
    mount_me(&server).await;
    let foreign =
        json!([{ "number": "RITM1", "requested_for.sys_id": OTHER, "opened_by.sys_id": OTHER }]);
    on(
        &server,
        "GET",
        "/api/now/table/sc_req_item",
        result(foreign),
    )
    .await;
    assert_eq!(cli.run(&["catalog", "request", "mine"]).await.code, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_variables_of_a_request_can_be_ordered_again() {
    let server = MockServer::start().await;
    item(&server, "zoom_webinar").await;
    Mock::given(method("GET"))
        .and(path(format!("/api/now/table/sc_req_item/{OTHER}")))
        .respond_with(|request: &Request| {
            let q = query(request);
            if q["sysparm_fields"].contains("cat_item") {
                return result(json!({ "sys_id": OTHER, "cat_item": ZOOM }));
            }
            assert_eq!(q["sysparm_display_value"], "all");
            assert!(q["sysparm_fields"].contains("variables.topic"));
            result(json!({
                "variables.topic": { "value": "Launch", "display_value": "Launch" },
                "variables.descripion": { "value": "", "display_value": "" },
                "variables.set_password": { "value": "hunter2", "display_value": "hunter2" },
                "variables.select_webinar_start_time": { "value": "2027-03-01 18:00:00", "display_value": "2027-03-01 10:00:00" },
                "variables.select_webinar_end_time": "2027-03-01 19:30:00",
            }))
        })
        .mount(&server)
        .await;
    let cli = Cli::basic(&server);
    let args = ["catalog", "requests", "vars", OTHER];
    let run = cli.run(&args).await.ok();
    assert_eq!(
        run.json(),
        json!({ "topic": "Launch", "select_webinar_start_time": "2027-03-01 10:00:00" })
    );
    assert!(
        run.stderr.contains("set_password") && run.stderr.contains("select_webinar_end_time"),
        "{}",
        run.stderr
    );
    let run = cli
        .run(&[&args[..], &["--include-empty", "--agent"]].concat())
        .await
        .ok();
    assert_eq!(run.envelope()["data"]["descripion"], "");
    assert!(!run.stdout.contains("hunter2"), "{}", run.stdout);
    let run = cli.run(&[&args[..], &["-o", "table"]].concat()).await.ok();
    assert!(
        run.stdout.contains("[REDACTED]") && !run.stdout.contains("hunter2"),
        "{}",
        run.stdout
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_apis_own_calls() {
    let server = MockServer::start().await;
    let cli = Cli::basic(&server);
    let items = format!("{BASE}/items");
    for (args, verb, at, sent) in [
        (
            vec!["list", "--text", "hr"],
            "GET",
            format!("{BASE}/catalogs"),
            "sysparm_text=hr",
        ),
        (
            vec!["get", AWS],
            "GET",
            format!("{BASE}/catalogs/{AWS}"),
            "",
        ),
        (
            vec![
                "categories",
                AWS,
                "--setlimit",
                "5",
                "--offset",
                "2",
                "--top-level-only",
            ],
            "GET",
            format!("{BASE}/catalogs/{AWS}/categories"),
            "sysparm_top_level_only=true&sysparm_limit=5&sysparm_offset=2",
        ),
        (
            vec!["category", AWS],
            "GET",
            format!("{BASE}/categories/{AWS}"),
            "",
        ),
        (
            vec![
                "items",
                "--text",
                "x",
                "--category",
                "c",
                "--catalog",
                "d",
                "--item-type",
                "t",
            ],
            "GET",
            items.clone(),
            "sysparm_text=x&sysparm_category=c&sysparm_catalog=d&sysparm_type=t&sysparm_limit=100",
        ),
        (vec!["item", AWS], "GET", format!("{items}/{AWS}"), ""),
        (
            vec!["item-variables", AWS],
            "GET",
            format!("{items}/{AWS}/variables"),
            "",
        ),
        (
            vec!["add-to-cart", AWS, "-F", "sysparm_quantity=1"],
            "POST",
            format!("{items}/{AWS}/add_to_cart"),
            "",
        ),
        (
            vec!["cart-update", LINE, "-F", "sysparm_quantity=2"],
            "PUT",
            format!("{BASE}/cart/{LINE}"),
            "",
        ),
        (
            vec!["cart-empty", CART, "--yes"],
            "DELETE",
            format!("{BASE}/cart/{CART}/empty"),
            "",
        ),
        (
            vec!["checkout", "--yes"],
            "POST",
            format!("{BASE}/cart/checkout"),
            "",
        ),
        (vec!["wishlist"], "GET", format!("{BASE}/wishlist"), ""),
    ] {
        server.reset().await;
        once(&server, verb, at, result(json!([{ "sys_id": "x" }]))).await;
        cli.run(&[&["catalog"], &args[..], &["--agent"]].concat())
            .await
            .ok();
        let requests = server.received_requests().await.expect("requests");
        assert_eq!(
            requests[0].url.query().unwrap_or_default(),
            sent,
            "{args:?}"
        );
        server.verify().await;
    }
    server.reset().await;
    once(&server, "POST", submit_at(), request_answer()).await;
    let run = cli
        .run(&["catalog", "submit-order", "--yes", "--agent"])
        .await
        .ok();
    assert_eq!(run.envelope()["data"]["number"], "REQ0010002");
    assert_eq!(cli.run(&["catalog", "item", "a/b"]).await.code, 3);
}

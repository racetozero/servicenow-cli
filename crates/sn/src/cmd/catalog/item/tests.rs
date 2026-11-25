use super::*;

const REQUEST: &str = "b54ae46e1b181310b97aa975624bcbf2";
const CART: &str = "90ed51b23b50dbd0b8094b44c3e45aad";
const CART_ITEM: &str = "346489831b181710b97aa975624bcb75";

fn fixture(name: &str) -> Item {
    let path = format!(
        "{}/tests/fixtures/catalog/{name}.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let text = std::fs::read_to_string(path).expect("fixture");
    let document: Value = serde_json::from_str(&text).expect("JSON");
    Item::parse(&document["result"]).expect("item")
}

fn inputs(pairs: &[(&str, &str)]) -> Inputs {
    pairs
        .iter()
        .map(|(name, value)| {
            let input = Input {
                value: (*value).to_owned(),
                from_command_line: false,
                list: false,
            };
            ((*name).to_owned(), input)
        })
        .collect()
}

fn aws(changes: &[(&str, &str)]) -> Inputs {
    let mut out = inputs(&[
        ("division", "IT"),
        ("environment", "Dev"),
        ("describe_briefly_the_purpose_of_this_account", "Sandbox"),
    ]);
    out.extend(inputs(changes));
    out
}

fn variable<'a>(item: &'a mut Item, name: &str) -> &'a mut Variable {
    item.variables
        .iter_mut()
        .find(|v| v.name == name)
        .expect("variable")
}

fn added(item: &mut Item, name: &str, kind: Kind, default: Option<&str>) {
    let mut variable = item.variables[0].clone();
    variable.name = name.into();
    variable.label = name.into();
    variable.type_name = "numeric_scale".into();
    variable.kind = kind;
    variable.mandatory = false;
    variable.default = default.map(str::to_owned);
    item.variables.push(variable);
}

#[test]
fn items_are_read_with_their_containers_and_types() {
    let item = fixture("aws_account_request");
    assert_eq!(item.class, "sc_cat_item");
    assert!(!item.client_logic);
    let division = item.variable("division").expect("division");
    assert_eq!((division.kind, division.mandatory), (Kind::Choice, true));
    let values: Vec<&str> = division.choices.iter().map(|(v, _)| v.as_str()).collect();
    assert_eq!(values, ["IT", "Product", "Marketing", "Sales", "Finance"]);

    let laptop = fixture("standard_laptop");
    let acrobat = laptop.variable("acrobat").expect("acrobat");
    assert_eq!(acrobat.container.as_deref(), Some("Optional Software"));
    assert_eq!(acrobat.default.as_deref(), Some("false"));
    assert!(laptop.variable("optional_label").is_none());

    let zoom = fixture("zoom_webinar");
    assert!(zoom.client_logic);
    let password = zoom.variable("set_password").expect("password");
    assert!(password.is_sensitive());
    let flag = zoom.variable("requires_password").expect("flag");
    assert!(!flag.is_sensitive());
    let hosts = zoom.variable("add_co_hosts").expect("hosts");
    assert_eq!(hosts.kind, Kind::ListCollector);
    assert_eq!(hosts.table.as_deref(), Some("sys_user"));

    let bulk = fixture("mobile_devices_bulk");
    let department = bulk.variable("department").expect("department");
    assert_eq!(department.table.as_deref(), Some("cmn_department"));
}

#[test]
fn layout_is_dropped_and_widgets_are_named() {
    // Shapes as an instance returns them, a misspelt type included.
    let item = Item::parse(&json!({
        "sys_id": "item",
        "variables": [
            { "name": "entry", "friendly_type": "macro_with_abel", "type": 17, "label": "Entry" },
            { "name": "widget", "friendly_type": "some_new_widget", "type": 14, "label": "Widget" },
            { "name": "text", "friendly_type": "odd_label", "type": "11", "label": "Text" },
            { "name": "task_sla", "friendly_type": "duration", "type": 29, "label": "Task SLA" },
        ],
    }))
    .expect("item");
    let names: Vec<&str> = item.variables.iter().map(|v| v.name.as_str()).collect();
    assert_eq!(names, ["task_sla"]);
    assert_eq!(item.variables[0].kind, Kind::Unknown);
    assert_eq!(item.widgets, ["Entry", "Widget"]);
}

#[test]
fn items_that_cannot_be_ordered_here_are_refused() {
    let mut item = fixture("mobile_devices_bulk");
    assert!(item.check_orderable("https://x").is_ok());
    variable(&mut item, "mobile_devices_set").mandatory = true;
    let message = item
        .check_orderable("https://x")
        .expect_err("set")
        .to_string();
    assert!(message.contains("mobile_devices_set"), "{message}");
    assert!(
        message.contains("https://x/sp?id=sc_cat_item&sys_id=e56a7ffe41011300964ff05369414ebd"),
        "{message}"
    );
    let mut item = fixture("aws_account_request");
    item.mandatory_attachment = true;
    assert!(item.check_orderable("https://x").is_err());
    item.mandatory_attachment = false;
    for class in ["sc_cat_item_producer", "sc_cat_item_guide"] {
        item.class = class.into();
        let message = item
            .check_orderable("https://x")
            .expect_err(class)
            .to_string();
        assert!(message.contains(class), "{message}");
    }
    assert!(is_orderable_class("u_custom_item"));
    assert_eq!(kind_label("sc_cat_item_guide"), "order guide");
}

#[test]
fn required_variables_and_choices_are_checked() {
    let item = fixture("aws_account_request");
    let good = validate(&item, &aws(&[]), 1, false);
    assert!(good.problems.is_empty(), "{:?}", good.problems);
    let missing = validate(&item, &inputs(&[("division", "IT")]), 1, false);
    assert_eq!(
        missing.problems,
        [
            "environment: required (Environment)",
            "describe_briefly_the_purpose_of_this_account: required (Describe the purpose of this account briefly)"
        ]
    );
    let blank = validate(&item, &aws(&[("environment", "   ")]), 1, false);
    assert_eq!(blank.problems, ["environment: required (Environment)"]);
    let wrong = validate(
        &item,
        &aws(&[("division", "Bogus"), ("environment", "prod")]),
        1,
        false,
    );
    assert_eq!(
        wrong.problems,
        ["division: \"Bogus\" is not one of the choices: IT, Product, Marketing, Sales, Finance"]
    );
    assert_eq!(wrong.variables["environment"], "Prod");
}

#[test]
fn unknown_names_are_problems_unless_allowed() {
    let item = fixture("aws_account_request");
    let values = aws(&[("Division", "IT")]);
    let strict = validate(&item, &values, 1, false);
    assert!(
        strict.problems[0].starts_with("unknown variable `Division` (did you mean `division`?)"),
        "{:?}",
        strict.problems
    );
    let lenient = validate(&item, &values, 1, true);
    assert!(lenient.problems.is_empty());
    assert!(!lenient.variables.contains_key("Division"));
    assert!(lenient.warnings[0].contains("not sent"));
    for (name, suggestion) in [
        ("enviroment", " (did you mean `environment`?)"),
        ("divsion", " (did you mean `division`?)"),
        ("region", ""),
        ("env", ""),
    ] {
        assert_eq!(item.suggestion(name), suggestion);
    }
}

#[test]
fn unknown_kinds_are_sent_and_read_back() {
    let mut item = fixture("standard_laptop");
    added(&mut item, "rating", Kind::Unknown, Some("3"));
    let result = validate(&item, &Inputs::new(), 1, false);
    assert_eq!(result.variables["rating"], "3");
    assert!(result.warnings.iter().any(|w| w.starts_with("rating:")));
    assert!(item.readable_names().contains(&"rating"));
    let read = ordered(
        &item,
        &json!({ "variables.rating": { "value": "5", "display_value": "5" } }),
    );
    assert_eq!(read.len(), 1);
    assert_eq!(read[0].order_value.as_deref(), Some("5"));
}

#[test]
fn booleans_are_normalized_and_a_required_box_must_be_ticked() {
    let mut item = fixture("standard_laptop");
    added(&mut item, "needs_vpn", Kind::YesNo, None);
    let values = inputs(&[
        ("acrobat", "YES"),
        ("photoshop", "0"),
        ("needs_vpn", "true"),
    ]);
    let result = validate(&item, &values, 1, false);
    assert!(result.problems.is_empty(), "{:?}", result.problems);
    assert_eq!(result.variables["acrobat"], "true");
    assert_eq!(result.variables["photoshop"], "false");
    assert_eq!(result.variables["needs_vpn"], "Yes");
    let bad = validate(&item, &inputs(&[("acrobat", "maybe")]), 1, false);
    assert_eq!(
        bad.problems,
        ["acrobat: \"maybe\" is not a boolean; use true or false"]
    );
    variable(&mut item, "acrobat").mandatory = true;
    for unticked in [inputs(&[("acrobat", "no")]), Inputs::new()] {
        let result = validate(&item, &unticked, 1, false);
        assert_eq!(result.problems, ["acrobat: required (Adobe Acrobat)"]);
    }
}

#[test]
fn secrets_are_refused_on_the_command_line() {
    let zoom = fixture("zoom_webinar");
    let given = |from_command_line| {
        Inputs::from([(
            "set_password".to_owned(),
            Input {
                value: "hunter2".into(),
                from_command_line,
                list: false,
            },
        )])
    };
    let flagged = validate(&zoom, &given(true), 1, false);
    let problem = flagged
        .problems
        .iter()
        .find(|p| p.starts_with("set_password:"))
        .expect("refused");
    assert!(problem.contains("--vars-file"), "{problem}");
    assert!(!flagged.problems.concat().contains("hunter2"));
    assert!(!flagged.variables.contains_key("set_password"));
    let from_file = validate(&zoom, &given(false), 1, false);
    assert_eq!(from_file.variables["set_password"], "hunter2");
    for assignment in ["hunter2", "=hunter2"] {
        let message = parse_assignment(assignment).expect_err("bad").to_string();
        assert!(!message.contains("hunter2"), "{message}");
    }
    let (name, input) = parse_assignment("query=a=b").expect("assignment");
    assert_eq!((name.as_str(), input.value.as_str()), ("query", "a=b"));
}

#[test]
fn documents_keep_values_as_written() {
    let yaml = parse_document(
        "zip: 01234\nversion: 1.10\nbig: 1e3\nflag: yes\nid: 12345678901234567890123\n",
    )
    .expect("yaml");
    for (name, value) in [
        ("zip", "01234"),
        ("version", "1.10"),
        ("big", "1e3"),
        ("flag", "yes"),
        ("id", "12345678901234567890123"),
    ] {
        assert_eq!(yaml[name].value, value);
    }
    let json = parse_document(r#"{"version": 1.10, "on": true, "text": "a\nb\"c"}"#).expect("json");
    assert_eq!(json["version"].value, "1.10");
    assert_eq!(json["on"].value, "true");
    assert_eq!(json["text"].value, "a\nb\"c");
    let list = parse_document("hosts:\n  - abel.tuter\n  - beth.anglin\n").expect("list");
    assert_eq!(list["hosts"].value, "abel.tuter,beth.anglin");
    assert!(list["hosts"].list);
}

#[test]
fn bad_documents_are_refused_without_showing_them() {
    for document in [
        r#"{"environment": "Prod", "environment": "Dev"}"#,
        "environment: Prod\nenvironment: Dev\n",
        "1: a\n\"1\": b\n",
        "a: ~",
        "- one\n- two",
        "a:\n  b: c",
        "set_password: [hunter2\nother: x",
        "hosts: [abel, \"beth,hunter2\"]\n",
        "{\"topic\": \"x\",\n  \"set_password\": }",
    ] {
        let message = parse_document(document).expect_err(document).to_string();
        assert!(!message.contains("hunter2"), "{message}");
        // Only a JSON parser's position is about the document's own syntax.
        assert_eq!(
            message.contains("line 2, column 19"),
            document.ends_with('}') && document.contains("topic"),
            "{message}"
        );
    }
}

#[test]
fn a_list_only_fits_a_list_collector() {
    let zoom = fixture("zoom_webinar");
    let values = parse_document("add_co_hosts: [abel.tuter]\ntopic: [a, b]\n").expect("document");
    let result = validate(&zoom, &values, 1, false);
    let about = |name: &str| result.problems.iter().any(|p| p.starts_with(name));
    assert!(about("topic: you passed a list"), "{:?}", result.problems);
    assert!(!about("add_co_hosts:"));
}

#[test]
fn defaults_are_checked_like_values() {
    let laptop = fixture("standard_laptop");
    let result = validate(&laptop, &Inputs::new(), 1, false);
    assert_eq!(result.variables["acrobat"], "false");

    let mut bulk = fixture("mobile_devices_bulk");
    variable(&mut bulk, "department").default = Some("Finance".into());
    let result = validate(&bulk, &inputs(&[("business_justification", "x")]), 1, false);
    assert_eq!(result.checks[0].values, ["Finance"]);

    let mut item = fixture("aws_account_request");
    variable(&mut item, "division").default = Some("Nowhere".into());
    let mut values = aws(&[]);
    values.remove("division");
    let result = validate(&item, &values, 1, false);
    assert_eq!(result.problems.len(), 1, "{:?}", result.problems);
    assert!(result.problems[0].contains("pass a value to replace it"));

    variable(&mut item, "division").read_only = true;
    let result = validate(&item, &values, 1, false);
    assert!(result.problems[0].contains("read-only default"));
    variable(&mut item, "division").default = Some("IT".into());
    let result = validate(&item, &values, 1, false);
    assert!(result.problems.is_empty(), "{:?}", result.problems);
    assert_eq!(result.variables["division"], "IT");
}

#[test]
fn blank_values_of_typed_variables_are_sent_empty() {
    let mut zoom = fixture("zoom_webinar");
    for variable in &mut zoom.variables {
        variable.mandatory = false;
    }
    let values = inputs(&[
        ("topic", "  "),
        ("select_webinar_end_time", " "),
        ("add_co_hosts", " \t"),
    ]);
    let result = validate(&zoom, &values, 1, false);
    assert!(result.problems.is_empty() && result.checks.is_empty());
    assert_eq!(result.variables["topic"], "  ");
    assert_eq!(result.variables["select_webinar_end_time"], "");
    assert_eq!(result.variables["add_co_hosts"], "");
}

#[test]
fn multi_row_sets_stay_empty() {
    let mut item = fixture("mobile_devices_bulk");
    item.client_logic = true;
    let result = validate(&item, &Inputs::new(), 1, false);
    assert!(!result.variables.contains_key("mobile_devices_set"));
    assert!(
        result.warnings[0].starts_with("mobile_devices_set: optional multi-row variable set left empty; the item's client logic may need rows"),
        "{:?}",
        result.warnings
    );
    let given = validate(&item, &inputs(&[("mobile_devices_set", "[{}]")]), 1, false);
    assert!(
        given
            .problems
            .iter()
            .any(|p| p.starts_with("mobile_devices_set: this command cannot fill"))
    );
}

#[test]
fn dates_are_strict() {
    for good in ["2026-12-01", "2024-02-29"] {
        assert!(is_date(good), "{good}");
    }
    for bad in [
        "2026-2-01",
        "01-12-2026",
        "2025-02-29",
        "2026-13-01",
        "2026-00-10",
        "2026-12-01 10:00:00",
        "2026-12-0\u{e9}",
    ] {
        assert!(!is_date(bad), "{bad}");
    }
    assert!(date_time("2028-02-29 23:59:59").is_ok());
    for (bad, why) in [
        ("2026-12-01T10:00:00Z", "ISO 8601"),
        ("2026-12-01 10:00:00+01:00", "ISO 8601"),
        ("01-12-2026 10:00", "not a date"),
        ("2026-12-01 10:00", "not a date"),
        ("2026-02-30 10:00:00", "not a date"),
        ("2026-12-01 24:00:00", "not a date"),
        ("2026-12-01 10:60:00", "not a date"),
        ("2026-12-01 10:00:0\u{e9}", "not a date"),
        ("2026-12-01 10:00:00\u{e9}+01", "not a date"),
    ] {
        let message = date_time(bad).expect_err(bad);
        assert!(message.contains(why), "{bad}: {message}");
    }
}

#[test]
fn references_become_checks() {
    let values = inputs(&[(
        "add_co_hosts",
        "abel.tuter, 62826bf03710200044e0bfc8bcbe5df1,",
    )]);
    let result = validate(&fixture("zoom_webinar"), &values, 1, false);
    assert_eq!(
        result.checks,
        [Check {
            variable: "add_co_hosts".into(),
            table: "sys_user".into(),
            qualifier: None,
            values: vec![
                "abel.tuter".into(),
                "62826bf03710200044e0bfc8bcbe5df1".into()
            ],
        }]
    );
}

#[test]
fn placeholders_read_only_values_and_quantities_are_refused() {
    let mut item = fixture("aws_account_request");
    item.variables[0].read_only = true;
    let result = validate(&item, &aws(&[("environment", REDACTED)]), 1, false);
    assert_eq!(result.problems.len(), 2, "{:?}", result.problems);
    assert!(result.problems[0].starts_with("division: read-only"));
    assert!(result.problems[1].starts_with("environment: the value is the placeholder"));
    item.variables[0].read_only = false;
    assert!(validate(&item, &aws(&[]), 3, false).problems.is_empty());
    assert!(!validate(&item, &aws(&[]), 0, false).problems.is_empty());
    item.show_quantity = Some(false);
    assert!(!validate(&item, &aws(&[]), 3, false).problems.is_empty());
    assert!(validate(&item, &aws(&[]), 1, false).problems.is_empty());
}

#[test]
fn plans_hide_secrets_and_bodies_do_not() {
    let plan = Plan {
        item_id: "a".repeat(32),
        item_name: "Zoom".into(),
        item_class: "sc_cat_item".into(),
        quantity: 1,
        requested_for: None,
        variables: BTreeMap::from([
            ("topic".into(), "abc".into()),
            ("pin".into(), "abc".into()),
            ("set_password".into(), "abcdef".into()),
            ("api_token".into(), String::new()),
        ]),
        sensitive: BTreeSet::from(["pin".into(), "set_password".into(), "api_token".into()]),
        warnings: Vec::new(),
    };
    let shown = plan.to_json();
    assert_eq!(shown["variables"]["set_password"], REDACTED);
    assert_eq!(shown["variables"]["topic"], "abc");
    assert_eq!(shown["variables"]["api_token"], "");
    assert_eq!(
        plan.body(),
        json!({
            "sysparm_quantity": "1",
            "variables": { "api_token": "", "pin": "abc", "set_password": "abcdef", "topic": "abc" },
        })
    );
    assert_eq!(plan.secrets(), ["abcdef", "abc"]);
}

#[test]
fn values_of_a_request_are_read_in_the_form_that_an_order_takes() {
    let record = json!({
        "variables.topic": { "value": "Launch", "display_value": "Launch" },
        "variables.add_co_hosts": { "value": "id1,id2", "display_value": "Abel, Beth" },
        "variables.set_password": { "value": "hunter2", "display_value": "hunter2" },
        // Stored in UTC, ordered in the user's time zone.
        "variables.select_webinar_start_time": { "value": "2027-03-01 18:00:00", "display_value": "2027-03-01 10:00:00" },
        "variables.select_webinar_end_time": "2027-03-01 19:30:00",
    });
    let values = ordered(&fixture("zoom_webinar"), &record);
    let of = |name: &str| values.iter().find(|v| v.name == name).expect("value");
    assert_eq!(values.len(), 5);
    assert_eq!(of("add_co_hosts").order_value.as_deref(), Some("id1,id2"));
    assert_eq!(of("add_co_hosts").display, "Abel, Beth");
    assert!(of("set_password").sensitive);
    assert_eq!(
        of("select_webinar_start_time").order_value.as_deref(),
        Some("2027-03-01 10:00:00")
    );
    assert_eq!(of("select_webinar_end_time").order_value, None);
}

#[test]
fn answers_to_an_order() {
    let request = |number: Option<&str>| Outcome::Request {
        sys_id: REQUEST.into(),
        number: number.map(str::to_owned),
    };
    let cart = Outcome::CartOnly {
        cart_id: CART.into(),
    };
    let unknown = |keys: &[&str], number: Option<&str>| Outcome::Unknown {
        keys: keys.iter().map(|k| (*k).to_owned()).collect(),
        number: number.map(str::to_owned),
    };
    for (answer, outcome) in [
        (
            json!({ "sys_id": REQUEST, "number": "REQ0010001", "table": "sc_request" }),
            request(Some("REQ0010001")),
        ),
        (json!({ "sys_id": REQUEST }), request(None)),
        (
            json!({ "request_id": REQUEST, "cart_id": CART }),
            request(None),
        ),
        (
            json!({ "request_id": REQUEST, "sys_id": CART, "table": "sc_cart" }),
            request(None),
        ),
        (json!({ "cart_id": CART }), cart.clone()),
        (
            json!({ "sys_id": CART, "table": "sc_cart", "cart_id": CART }),
            cart.clone(),
        ),
        (json!({ "sys_id": CART, "cart_id": CART }), cart),
        (
            json!({ "sys_id": CART, "cart_id": "" }),
            unknown(&["cart_id", "sys_id"], None),
        ),
        (
            json!({ "request_number": " REQ0010001 " }),
            unknown(&["request_number"], Some("REQ0010001")),
        ),
        // Anything else could be an echoed value.
        (
            json!({ "request_number": "hunter2 secret" }),
            unknown(&["request_number"], None),
        ),
        (
            json!({ "number": "REQ0010001" }),
            unknown(&["number"], None),
        ),
        (Value::Null, unknown(&[], None)),
    ] {
        assert_eq!(Outcome::of(&answer), outcome, "{answer}");
    }
}

#[test]
fn carts_are_read_from_groups_and_from_the_top() {
    let cart = Cart::parse(&json!({
        "cart_id": CART,
        "none": { "items": [{ "cart_item_id": CART_ITEM, "item_name": "AWS", "quantity": "1", "can_order": true }] },
        "monthly": { "items": [{ "cart_item_id": "456489831b181710b97aa975624bcbb2", "item_name": "Zoom", "can_order": false }] },
        // Listed twice, in another case: still one item.
        "items": [{ "cart_item_id": CART_ITEM.to_uppercase(), "item_name": "AWS" }],
        "subtotal_price": "-",
    }))
    .expect("cart");
    assert_eq!(cart.items.len(), 2);
    let aws = cart.item(&CART_ITEM.to_uppercase()).expect("item");
    assert_eq!(aws.details["frequency"], "none");
    assert_eq!(aws.can_order, Some(true));
    let zoom = cart.items.iter().find(|i| i.name == "Zoom").expect("zoom");
    assert_eq!(zoom.details["frequency"], "monthly");
    let added = Cart::parse(&json!({
        "cart_id": CART,
        "items": [{ "cart_item_id": CART_ITEM, "name": " ", "catalog_item_id": "5A1EFA5947520510F53D37D2846D439B" }],
    }))
    .expect("cart");
    assert_eq!(added.items[0].name, "(unnamed item)");
    assert_eq!(
        added.items[0].catalog_item.as_deref(),
        Some("5a1efa5947520510f53d37d2846d439b")
    );
    let empty = Cart::parse(&json!({ "cart_id": CART })).expect("cart");
    assert_eq!(empty.to_json(), json!({ "cart_id": CART, "items": [] }));
    for bad in [
        json!({}),
        json!({ "cart_id": "x" }),
        json!([CART]),
        json!({ "cart_id": CART, "items": [{ "cart_item_id": "x" }] }),
        json!({ "cart_id": CART, "items": ["x"] }),
    ] {
        assert!(Cart::parse(&bad).is_err(), "{bad}");
    }
}

#[test]
fn names_of_secrets() {
    for name in [
        "u_client_secret",
        "access_token",
        "api-key",
        "Set Password",
        "vpn_pass",
        "Enter your PIN:",
        "OTP (SMS)",
        "AWS Credential",
        "passphrase",
        "aws_access_key",
        "ssn",
    ] {
        assert!(is_sensitive_name(name), "{name}");
    }
    for name in [
        "tokenized_name",
        "cookies_enabled",
        "pinned",
        "shipping_address",
        "passport_number",
        "bypass_reason",
        "power_supply",
    ] {
        assert!(!is_sensitive_name(name), "{name}");
    }
}

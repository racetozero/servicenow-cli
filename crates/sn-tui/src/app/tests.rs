use super::*;
use crate::data::{Backend, Replies};
use crate::model::{Meta, Row};
use crate::msg::{Attachment, Candidate, Change, Entry, EntryKind, Page, Sla, Written};
use serde_json::{Value, json};
use std::path::PathBuf;

pub(crate) const ME: &str = "6816f79cc0a8016401c5a33be04be441";
pub(crate) const PLAIN: Caps = Caps {
    color: true,
    truecolor: false,
    unicode: true,
};

pub(crate) fn id(n: usize) -> String {
    format!("{n:032x}")
}

fn cell(value: &str, display: &str) -> Value {
    json!({ "value": value, "display_value": display })
}

/// An incident as the instance sends it. Every seventh is mine.
pub(crate) fn incident(n: usize) -> Value {
    let mine = n.is_multiple_of(7);
    json!({
        "sys_id": id(n),
        "sys_class_name": cell("incident", "Incident"),
        "number": format!("INC{n:07}"),
        "short_description": match n { 0 => "Email down in 東京".to_owned(), n => format!("Printer {n} is offline") },
        "description": "first line\nsecond line with\u{1b}[31m an escape",
        "state": cell("2", "In Progress"),
        "priority": cell(if n == 0 { "1" } else { "3" }, if n == 0 { "1 - Critical" } else { "3 - Moderate" }),
        "urgency": cell("2", "2 - Medium"),
        "active": "true",
        "assigned_to": if mine { cell(ME, "Ada Lovelace") } else { cell("", "") },
        "assignment_group": cell(&id(900), "Network"),
        "close_code": "", "close_notes": "",
        "due_date": "2026-11-20 08:00:00",
        "u_api_key": "do-not-show",
        "sys_updated_on": "2026-11-25 07:30:00",
        "sys_updated_by": "bob",
        "sys_mod_count": "3",
    })
}

pub(crate) fn meta() -> Meta {
    Meta::from_ui(&json!({ "columns": {
        "number": {"label": "Number", "type": "string", "read_only": true, "max_length": 40},
        "short_description": {"label": "Short description", "type": "string", "mandatory": true, "max_length": 160},
        "description": {"label": "Description", "type": "string", "max_length": 4000},
        "state": {"label": "State", "type": "integer", "choices": [
            {"value": "1", "label": "New"}, {"value": "2", "label": "In Progress"},
            {"value": "6", "label": "Resolved"}, {"value": "7", "label": "Closed"}]},
        "priority": {"label": "Priority", "type": "integer", "read_only": true, "choices": [
            {"value": "1", "label": "1 - Critical"}, {"value": "3", "label": "3 - Moderate"}]},
        "urgency": {"label": "Urgency", "type": "integer", "choices": [
            {"value": "1", "label": "1 - High"}, {"value": "2", "label": "2 - Medium"}]},
        "active": {"label": "Active", "type": "boolean"},
        "due_date": {"label": "Due date", "type": "due_date"},
        "assigned_to": {"label": "Assigned to", "type": "reference", "reference": "sys_user"},
        "assignment_group": {"label": "Assignment group", "type": "reference", "reference": "sys_user_group"},
        "close_code": {"label": "Resolution code", "type": "string", "max_length": 40, "choices": [
            {"value": "solved", "label": "Solution provided"}]},
        "close_notes": {"label": "Resolution notes", "type": "string", "max_length": 4000},
        "comments": {"label": "Comments", "type": "journal_input"},
        "work_notes": {"label": "Work notes", "type": "journal_input"},
        "u_api_key": {"label": "API key", "type": "string"},
        "sys_id": {"label": "Sys ID", "type": "GUID", "read_only": true},
        "sys_updated_on": {"label": "Updated", "type": "glide_date_time", "read_only": true},
    }}))
    .expect("meta")
}

fn approval(n: usize) -> Value {
    json!({
        "sys_id": id(500 + n),
        "sysapproval": cell(&id(n), &format!("CHG{n:07}")),
        "sysapproval.short_description": format!("Change {n}"),
        "state": cell("requested", "Requested"),
        "approver": cell(ME, "Ada Lovelace"),
        "sys_updated_on": "2026-11-25 07:30:00",
        "sys_mod_count": "0",
    })
}

/// Answers like a small instance, from memory.
pub(crate) struct Fake;

impl Fake {
    pub(crate) fn answer(call: Call) -> Vec<Reply> {
        let refused = || Fault {
            code: "permission",
            message: "GET /api/now/table/sys_hub_flow failed with HTTP 403: no access".into(),
        };
        vec![match call {
            Call::Me { req } => Reply::Me(
                req,
                Ok(Me {
                    sys_id: ME.into(),
                    name: "Ada Lovelace".into(),
                    groups: Some(vec![id(900)]),
                }),
            ),
            Call::Count { id, table, .. } => Reply::Count(
                id,
                match table.as_str() {
                    "sys_hub_flow" => Err(refused()),
                    _ => Ok(1234),
                },
            ),
            Call::Meta { table } => Reply::Meta(table, Ok(meta())),
            Call::List {
                req,
                table,
                offset,
                limit,
                query,
                ..
            } => {
                let total = 120;
                let rows = match table.as_str() {
                    "sys_hub_flow" => return vec![Reply::List(req, Err(refused()))],
                    "sc_request" => Vec::new(),
                    "sysapproval_approver" => {
                        (0..3).map(|n| Row::new(&table, &approval(n))).collect()
                    }
                    _ => (offset..(offset + limit).min(total))
                        .map(|n| Row::new(&table, &incident(n as usize)))
                        .collect::<Vec<_>>(),
                };
                let total = match table.as_str() {
                    "sc_request" => 0,
                    "sysapproval_approver" => 3,
                    _ if query.contains("assigned_to=") => 120,
                    _ => total,
                };
                Reply::List(
                    req,
                    Ok(Page {
                        rows,
                        total: Some(total),
                    }),
                )
            }
            Call::Record { req, table, sys_id } => {
                let n = usize::from_str_radix(&sys_id, 16).unwrap_or(0);
                Reply::Record(req, Ok(Row::new(&table, &incident(n))))
            }
            Call::Activity { req, .. } => Reply::Activity(
                req,
                Ok(vec![Entry {
                    kind: EntryKind::WorkNote,
                    by: "bob".into(),
                    at: "2026-11-25 07:00:00".into(),
                    time: Some(NOW - 5_400),
                    text: "Checked the gateway.\nIt is the DPD timer.".into(),
                }]),
            ),
            Call::Related { req, lists } => {
                return lists
                    .iter()
                    .enumerate()
                    .map(|(i, l)| {
                        Reply::Related(
                            req,
                            i,
                            match l.table {
                                "sysapproval_approver" => {
                                    Ok(vec![Row::new("sysapproval_approver", &approval(0))])
                                }
                                "incident" => Ok(vec![Row::new("incident", &incident(3))]),
                                _ => Ok(Vec::new()),
                            },
                        )
                    })
                    .collect();
            }
            Call::Attachments { req, .. } => Reply::Attachments(
                req,
                Ok(vec![Attachment {
                    sys_id: id(700),
                    name: "gateway.log".into(),
                    size: 48_213,
                    by: "bob".into(),
                    time: Some(NOW - 60),
                }]),
            ),
            Call::Slas { req, .. } => Reply::Slas(
                req,
                Ok(vec![Sla {
                    name: "Priority 1 resolution".into(),
                    stage: "In progress".into(),
                    breached: true,
                    percent: Some(141),
                    left: "0 Seconds".into(),
                    due: "2026-11-25 08:00:00".into(),
                    due_time: Some(NOW - 1_800),
                }]),
            ),
            Call::History { req, .. } => Reply::History(
                req,
                Ok(vec![Change {
                    field: "state".into(),
                    old: "1".into(),
                    new: "2".into(),
                    by: "bob".into(),
                    time: Some(NOW - 7_200),
                }]),
            ),
            Call::Write { req, writes } => Reply::Written(
                req,
                writes
                    .iter()
                    .map(|w| Written::Saved {
                        row: saved(w),
                        kept: Vec::new(),
                    })
                    .collect(),
            ),
            Call::Create { req, table, .. } => {
                Reply::Created(req, Ok(Row::new(&table, &incident(99))))
            }
            Call::Download { req, .. } => {
                Reply::Downloaded(req, Ok(PathBuf::from("/tmp/gateway.log")))
            }
            Call::References { req, .. } => Reply::References(
                req,
                Ok(vec![Candidate {
                    value: id(800),
                    label: "Bob Stone".into(),
                    detail: "bob".into(),
                }]),
            ),
            Call::Search { req, records, .. } => {
                let tables = Reply::Tables(req, Ok(vec![("u_vendor".into(), "Vendor".into())]));
                return match records {
                    true => vec![
                        tables,
                        Reply::Records(req, Ok(vec![Row::new("task", &incident(5))])),
                    ],
                    false => vec![tables],
                };
            }
            Call::Locate { req, .. } => Reply::Located(req, Ok(("incident".into(), id(42)))),
        }]
    }
}

fn saved(write: &crate::msg::Write) -> Row {
    let n = usize::from_str_radix(&write.sys_id, 16).unwrap_or(0);
    let mut value = incident(n);
    for (field, new) in &write.body {
        value[field] = new.clone();
    }
    value["sys_mod_count"] = "4".into();
    value["sys_updated_on"] = "2026-11-25 08:30:00".into();
    Row::new(&write.table, &value)
}

impl Backend for Fake {
    async fn run(&self, call: Call, replies: Replies) {
        for reply in Fake::answer(call) {
            let _ = replies.send(reply);
        }
    }
}

/// 2026-11-25 08:30:00 UTC.
pub(crate) const NOW: i64 = 1_795_595_400;

pub(crate) fn app(width: u16, height: u16) -> App {
    let connection = Connection {
        profile: "dev".into(),
        base: "https://acme.service-now.com".into(),
        host: "acme.service-now.com".into(),
        read_only: false,
        dry_run: false,
        signed_out: None,
    };
    let profiles = ["dev", "prod"]
        .map(|name| ProfileInfo {
            name: name.into(),
            host: format!("{name}.service-now.com"),
            read_only: name == "prod",
        })
        .to_vec();
    let settings = Settings {
        page_size: 50,
        download_dir: PathBuf::from("."),
        start: None,
        pins: vec!["u_vendor".into()],
    };
    let mut app = App::new(connection, profiles, settings, PLAIN);
    app.update(Msg::Tick { now: NOW, ms: 1 });
    app.update(Msg::Resize(width, height));
    app
}

/// Runs effects against the fake until nothing is on its way. Gives the
/// effects that are not calls.
pub(crate) fn settle(app: &mut App, effects: Vec<Effect>) -> Vec<Effect> {
    let mut rest = Vec::new();
    let mut queue = effects;
    while !queue.is_empty() {
        for effect in std::mem::take(&mut queue) {
            match effect {
                Effect::Call(call) => {
                    for reply in Fake::answer(call) {
                        queue.extend(app.update(Msg::Reply(reply)));
                    }
                }
                other => rest.push(other),
            }
        }
    }
    rest
}

pub(crate) fn started(width: u16, height: u16) -> App {
    let mut app = app(width, height);
    let effects = app.start();
    settle(&mut app, effects);
    wait(&mut app);
    app
}

/// Lets a second pass, so that what waits for a pause runs.
pub(crate) fn wait(app: &mut App) {
    let ms = app.ms + 1_000;
    let effects = app.update(Msg::Tick { now: NOW, ms });
    settle(app, effects);
}

fn press(app: &mut App, code: KeyCode, mods: KeyModifiers) -> Vec<Effect> {
    app.update(Msg::Key(KeyEvent::new(code, mods)))
}

/// Presses keys: a letter, or a name such as `Enter` or `C-s`.
pub(crate) fn keys(app: &mut App, keys: &[&str]) -> Vec<Effect> {
    let mut out = Vec::new();
    for key in keys {
        let (code, mods) = match *key {
            "Enter" => (KeyCode::Enter, KeyModifiers::NONE),
            "Esc" => (KeyCode::Esc, KeyModifiers::NONE),
            "Tab" => (KeyCode::Tab, KeyModifiers::NONE),
            "BackTab" => (KeyCode::BackTab, KeyModifiers::SHIFT),
            "Up" => (KeyCode::Up, KeyModifiers::NONE),
            "Down" => (KeyCode::Down, KeyModifiers::NONE),
            "Left" => (KeyCode::Left, KeyModifiers::NONE),
            "Right" => (KeyCode::Right, KeyModifiers::NONE),
            "End" => (KeyCode::End, KeyModifiers::NONE),
            "Space" => (KeyCode::Char(' '), KeyModifiers::NONE),
            key if key.starts_with("C-") && key.len() == 3 => (
                KeyCode::Char(key.chars().last().unwrap_or(' ')),
                KeyModifiers::CONTROL,
            ),
            text => {
                for c in text.chars() {
                    let effects = press(app, KeyCode::Char(c), KeyModifiers::NONE);
                    out.extend(settle(app, effects));
                }
                continue;
            }
        };
        let effects = press(app, code, mods);
        out.extend(settle(app, effects));
    }
    out
}

fn raw_keys(app: &mut App, code: KeyCode) -> Vec<Call> {
    calls(press(app, code, KeyModifiers::NONE))
}

fn calls(effects: Vec<Effect>) -> Vec<Call> {
    effects
        .into_iter()
        .filter_map(|e| match e {
            Effect::Call(call) => Some(call),
            _ => None,
        })
        .collect()
}

fn field_at(app: &mut App, name: &str) {
    let record = app.record.clone().expect("record");
    let at = app
        .detail_fields(&record)
        .iter()
        .filter(|d| matches!(d, crate::model::Detail::Field(_)))
        .position(|d| *d == crate::model::Detail::Field(name.into()))
        .unwrap_or_else(|| panic!("no field {name}"));
    if let Some(record) = &mut app.record {
        record.cursor = at;
    }
}

fn toasts(app: &App) -> String {
    app.toasts
        .iter()
        .map(|t| t.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

fn review(app: &App) -> &Review {
    match app.overlays.last() {
        Some(Overlay::Review(review)) => review,
        other => panic!("no review but {other:?}"),
    }
}

#[test]
fn personal_lists_wait_for_the_user_and_the_rest_does_not() {
    let mut app = app(120, 40);
    let asked = calls(app.start());
    assert!(matches!(asked[0], Call::Me { .. }));
    assert!(
        !asked.iter().any(|c| matches!(c, Call::List { .. })),
        "a list without the user would show everyone's work"
    );
    let counted: Vec<&str> = asked
        .iter()
        .filter_map(|c| match c {
            Call::Count { id, .. } => Some(id.as_str()),
            _ => None,
        })
        .collect();
    assert!(counted.contains(&"incidents") && counted.contains(&"pin:u_vendor"));
    assert!(!counted.contains(&"mine"));
    let Call::Me { req } = asked[0] else {
        panic!("{asked:?}");
    };
    let reply = Fake::answer(Call::Me { req }).remove(0);
    let next = calls(app.update(Msg::Reply(reply)));
    assert!(next.iter().any(|c| matches!(c, Call::List { query, .. }
        if query == &format!("active=true^assigned_to={ME}^ORDERBYpriority^ORDERBYDESCsys_updated_on"))));
    assert!(
        next.iter()
            .any(|c| matches!(c, Call::Count { id, .. } if id == "mine"))
    );
}

#[test]
fn a_late_answer_to_an_old_list_is_ignored() {
    let mut app = started(120, 40);
    press(&mut app, KeyCode::Char('g'), KeyModifiers::NONE);
    let first = calls(press(&mut app, KeyCode::Char('i'), KeyModifiers::NONE));
    press(&mut app, KeyCode::Char('g'), KeyModifiers::NONE);
    press(&mut app, KeyCode::Char('c'), KeyModifiers::NONE);
    for call in first {
        for reply in Fake::answer(call) {
            app.update(Msg::Reply(reply));
        }
    }
    assert_eq!(app.list.table, "change_request");
    assert!(app.list.rows.is_empty());
    assert!(matches!(app.list.phase, ListPhase::Loading(_)));
}

#[test]
fn the_next_page_loads_near_the_end_and_never_twice() {
    let mut app = started(120, 40);
    assert_eq!(app.list.rows.len(), 50);
    let asked = raw_keys(&mut app, KeyCode::End);
    let [
        Call::List {
            offset: 50,
            limit: 50,
            ..
        },
    ] = asked.as_slice()
    else {
        panic!("{asked:?}");
    };
    assert!(
        raw_keys(&mut app, KeyCode::End).is_empty(),
        "one page at a time"
    );
    for call in asked {
        for reply in Fake::answer(call) {
            app.update(Msg::Reply(reply));
        }
    }
    assert_eq!(app.list.rows.len(), 100);
    keys(&mut app, &["End", "End", "End"]);
    assert_eq!(app.list.rows.len(), 120);
    assert!(app.list.exhausted);
    assert!(raw_keys(&mut app, KeyCode::End).is_empty());
}

#[test]
fn the_search_covers_what_people_read() {
    let mut app = started(120, 40);
    keys(&mut app, &["/", "PRINTER 12"]);
    let shown: Vec<String> = app
        .list
        .visible()
        .iter()
        .map(|i| app.list.rows[*i].name())
        .collect();
    assert_eq!(shown, ["INC0000012"]);
    keys(&mut app, &["Enter", "Esc"]);
    assert_eq!(app.list.visible().len(), 50);
    // A stored key is not what people read.
    keys(&mut app, &["/", ME]);
    assert!(app.list.visible().is_empty());
    assert!(app.empty_text().contains("Ctrl-K"));
}

#[test]
fn changes_are_collected_reviewed_and_sent_as_one_write() {
    let mut app = started(120, 40);
    keys(&mut app, &["Enter"]);
    field_at(&mut app, "state");
    keys(&mut app, &["Enter", "resol", "Enter"]);
    field_at(&mut app, "short_description");
    keys(&mut app, &["Enter", "!", "Enter"]);
    field_at(&mut app, "active");
    keys(&mut app, &["Enter", "Enter"]);
    let record = app.record.as_ref().expect("record");
    let edited: Vec<&str> = record.edits.iter().map(|e| e.field.as_str()).collect();
    assert_eq!(
        edited,
        ["state", "short_description"],
        "a value put back is no change"
    );
    keys(&mut app, &["C-s"]);
    assert_eq!(review(&app).lines[0].old, "In Progress");
    assert_eq!(review(&app).lines[0].new, "Resolved");
    let asked = raw_keys(&mut app, KeyCode::Enter);
    let [Call::Write { req, writes }] = asked.as_slice() else {
        panic!("{asked:?}");
    };
    assert_eq!(writes.len(), 1);
    assert_eq!(
        Value::Object(writes[0].body.clone()),
        json!({ "state": "6", "short_description": "Email down in 東京!" })
    );
    assert_eq!(
        writes[0].baseline.as_ref().map(|b| b.mods.as_str()),
        Some("3")
    );
    assert!(
        raw_keys(&mut app, KeyCode::Enter).is_empty(),
        "a second Enter does not send it twice"
    );
    let reply = Reply::Written(
        *req,
        vec![Written::Saved {
            row: saved(&writes[0]),
            kept: vec!["state".into()],
        }],
    );
    app.update(Msg::Reply(reply));
    assert!(app.overlays.is_empty());
    let record = app.record.as_ref().expect("record");
    assert!(record.edits.is_empty());
    assert_eq!(record.baseline.mods, "4");
    assert!(
        toasts(&app).contains("State is still 6"),
        "{}",
        toasts(&app)
    );
    assert_eq!(
        app.list.rows[0].display("short_description"),
        "Email down in 東京!"
    );
}

#[test]
fn a_conflict_shows_the_new_values_and_needs_another_yes() {
    let mut app = started(120, 40);
    keys(&mut app, &["Enter"]);
    field_at(&mut app, "urgency");
    keys(&mut app, &["Enter", "high", "Enter", "C-s"]);
    let asked = raw_keys(&mut app, KeyCode::Enter);
    let Some(Call::Write { req, .. }) = asked.first() else {
        panic!("{asked:?}");
    };
    let now = Row::new(
        "incident",
        &json!({ "sys_id": id(0), "urgency": cell("3", "3 - Low"), "sys_updated_by": "grace",
            "sys_updated_on": "2026-11-25 08:28:00", "sys_mod_count": "9" }),
    );
    let sent = calls(app.update(Msg::Reply(Reply::Written(
        *req,
        vec![Written::Conflict(now)],
    ))));
    assert!(sent.is_empty(), "nothing goes out by itself");
    assert_eq!(
        review(&app).state,
        ReviewState::Conflict("grace".into(), "2m ago".into())
    );
    assert_eq!(review(&app).lines[0].old, "3 - Low");
    assert!(review(&app).lines[0].moved);
    let asked = raw_keys(&mut app, KeyCode::Enter);
    let Some(Call::Write { writes, .. }) = asked.first() else {
        panic!("{asked:?}");
    };
    assert_eq!(
        writes[0].baseline.as_ref().map(|b| b.mods.as_str()),
        Some("9")
    );
}

#[test]
fn notes_add_and_check_no_version() {
    let mut app = started(120, 40);
    keys(&mut app, &["Enter", "w", "C-s"]);
    let Some(Overlay::Form(form)) = app.overlays.last() else {
        panic!("{:?}", app.overlays);
    };
    assert!(form.error.is_some(), "an empty note is refused");
    keys(&mut app, &["line one", "Enter", "line two", "C-s"]);
    let asked = raw_keys(&mut app, KeyCode::Enter);
    let Some(Call::Write { writes, .. }) = asked.first() else {
        panic!("{asked:?}");
    };
    assert_eq!(writes[0].body["work_notes"], "line one\nline two");
    assert_eq!(writes[0].baseline, None);
}

#[test]
fn resolving_needs_a_code_and_notes_and_sends_them_together() {
    let mut app = started(120, 40);
    keys(&mut app, &["x"]);
    keys(&mut app, &["C-s"]);
    let Some(Overlay::Form(form)) = app.overlays.last() else {
        panic!("{:?}", app.overlays);
    };
    assert_eq!(form.error.as_deref(), Some("Resolution code is required"));
    assert_eq!(form.fields[0].cell.display, "Resolved");
    keys(&mut app, &["sol", "Enter", "Fixed it", "C-s"]);
    let asked = raw_keys(&mut app, KeyCode::Enter);
    let Some(Call::Write { writes, .. }) = asked.first() else {
        panic!("{asked:?}");
    };
    assert_eq!(
        Value::Object(writes[0].body.clone()),
        json!({ "state": "6", "close_code": "solved", "close_notes": "Fixed it" })
    );
}

#[test]
fn several_approvals_go_at_once_and_failed_ones_stay() {
    let mut app = started(120, 40);
    keys(&mut app, &["ga", "Space", "Space", "Space", "X", "C-s"]);
    let Some(Overlay::Form(form)) = app.overlays.last() else {
        panic!("{:?}", app.overlays);
    };
    assert!(form.error.is_some(), "a rejection needs a reason");
    keys(&mut app, &["Too risky", "C-s"]);
    let asked = raw_keys(&mut app, KeyCode::Enter);
    let Some(Call::Write { req, writes }) = asked.first() else {
        panic!("{asked:?}");
    };
    assert_eq!(writes.len(), 3);
    assert_eq!(writes[1].body["state"], "rejected");
    let results = vec![
        Written::Saved {
            row: saved(&writes[0]),
            kept: Vec::new(),
        },
        Written::Failed(Fault {
            code: "permission",
            message: "no access".into(),
        }),
        Written::Saved {
            row: saved(&writes[2]),
            kept: Vec::new(),
        },
    ];
    app.update(Msg::Reply(Reply::Progress(*req, 2)));
    assert_eq!(review(&app).state, ReviewState::Sending(*req, 2));
    app.update(Msg::Reply(Reply::Written(*req, results)));
    let ReviewWork::Write(left) = &review(&app).work else {
        panic!();
    };
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].name, "CHG0000001");
    assert!(toasts(&app).contains("2 of 3"));
    assert!(matches!(&review(&app).state, ReviewState::Failed(p) if p[0].contains("no access")));
}

#[test]
fn a_read_only_profile_changes_nothing() {
    let mut app = started(120, 40);
    app.connection.read_only = true;
    keys(&mut app, &["Enter"]);
    for act in ["w", "c", "m", "i", "x", "n"] {
        assert!(keys(&mut app, &[act]).is_empty());
        assert!(app.overlays.is_empty(), "{act} opened {:?}", app.overlays);
        assert!(toasts(&app).contains("read-only"), "{act}");
    }
    field_at(&mut app, "active");
    keys(&mut app, &["Enter"]);
    assert!(app.record.as_ref().is_some_and(|r| r.edits.is_empty()));
    let menu = app.menu();
    assert!(!menu.contains(&Act::WorkNote) && menu.contains(&Act::Browser));
}

#[test]
fn secrets_are_neither_shown_nor_edited() {
    let mut app = started(120, 40);
    keys(&mut app, &["Enter"]);
    let record = app.record.as_ref().expect("record");
    assert_eq!(record.row.value("u_api_key"), crate::model::REDACTED);
    field_at(&mut app, "u_api_key");
    keys(&mut app, &["Enter"]);
    assert!(toasts(&app).contains("secrets"));
    assert!(app.overlays.is_empty());
}

#[test]
fn a_dry_run_is_no_failure() {
    let mut app = started(120, 40);
    keys(&mut app, &["w", "x", "C-s"]);
    let asked = raw_keys(&mut app, KeyCode::Enter);
    let Some(Call::Write { req, .. }) = asked.first() else {
        panic!("{asked:?}");
    };
    let fault = Fault {
        code: "dry_run",
        message: "dry run: PATCH x was not sent".into(),
    };
    app.update(Msg::Reply(Reply::Written(
        *req,
        vec![Written::Failed(fault)],
    )));
    assert_eq!(review(&app).state, ReviewState::Ready);
    assert!(toasts(&app).contains("was not sent"));
}

#[test]
fn a_reference_is_followed_to_the_record_of_its_own_class() {
    let mut app = started(120, 40);
    keys(&mut app, &["Enter"]);
    field_at(&mut app, "assignment_group");
    keys(&mut app, &["f"]);
    assert_eq!(
        app.record
            .as_ref()
            .map(|r| (r.table.clone(), r.sys_id.clone())),
        // The fake answers with an incident, whatever is asked.
        Some(("incident".into(), id(900)))
    );
    assert_eq!(app.records.len(), 1);
    // A task is read again from its own table, which has all its fields.
    app.open_record("task", &id(5));
    let mut asked = Vec::new();
    let mut effects = std::mem::take(&mut app.effects);
    while !effects.is_empty() {
        let mut next = Vec::new();
        for call in calls(effects) {
            if let Call::Record { table, .. } = &call {
                asked.push(table.clone());
            }
            for reply in Fake::answer(call) {
                next.extend(app.update(Msg::Reply(reply)));
            }
        }
        effects = next;
    }
    assert_eq!(asked, ["task", "incident"]);
}

#[test]
fn an_ended_session_keeps_everything_typed() {
    let mut app = started(120, 40);
    keys(&mut app, &["Enter"]);
    field_at(&mut app, "urgency");
    keys(&mut app, &["Enter", "high", "Enter", "w", "half a thought"]);
    let gone = Fault {
        code: "auth",
        message: "GET x failed with HTTP 401: User Not Authenticated".into(),
    };
    app.list.extra = Extra::Refresh {
        req: 999,
        limit: 50,
    };
    app.update(Msg::Reply(Reply::List(999, Err(gone))));
    assert!(app.connection.signed_out.is_some());
    assert!(app.top().is_none(), "the sign-in card is on top");
    assert_eq!(keys(&mut app, &["Enter"]), [Effect::SignIn]);
    let mut connection = app.connection.clone();
    connection.signed_out = None;
    app.resume(connection, app.profiles.clone());
    let effects = app.start();
    assert!(
        calls(effects.clone())
            .iter()
            .any(|c| matches!(c, Call::List { .. }))
    );
    settle(&mut app, effects);
    let Some(Overlay::Form(form)) = app.overlays.last() else {
        panic!("{:?}", app.overlays);
    };
    assert_eq!(form.fields[0].editor.text, "half a thought");
    assert_eq!(app.record.as_ref().map(|r| r.edits.len()), Some(1));
}

#[test]
fn another_profile_keeps_the_section_and_drops_the_record() {
    let mut app = started(120, 40);
    keys(&mut app, &["gi", "Enter"]);
    let effects = keys(&mut app, &["P", "Down", "Enter"]);
    assert_eq!(effects, [Effect::SwitchProfile("prod".into())]);
    let connection = Connection {
        profile: "prod".into(),
        base: "https://prod.service-now.com".into(),
        ..app.connection.clone()
    };
    let mut other = app.switch(connection, app.profiles.clone());
    let effects = other.start();
    settle(&mut other, effects);
    assert_eq!(other.list.title, "Incidents");
    assert!(other.record.is_none() && other.back.is_empty());
    assert_eq!(other.list.rows.len(), 50);
}

#[test]
fn the_record_beside_the_list_follows_the_cursor() {
    let mut app = started(160, 50);
    assert_eq!(
        app.record.as_ref().map(|r| r.sys_id.clone()),
        Some(id(0)),
        "a wide terminal shows the first record"
    );
    let asked = raw_keys(&mut app, KeyCode::Down);
    assert!(asked.is_empty(), "nothing is loaded while the cursor moves");
    raw_keys(&mut app, KeyCode::Down);
    let asked = calls(app.update(Msg::Tick {
        now: NOW,
        ms: app.ms + 200,
    }));
    assert!(matches!(&asked[0], Call::Record { sys_id, .. } if *sys_id == id(2)));
    // With changes waiting, it stays.
    settle(&mut app, asked.into_iter().map(Effect::Call).collect());
    keys(&mut app, &["Enter"]);
    field_at(&mut app, "active");
    keys(&mut app, &["Enter", "Tab", "Tab", "Down"]);
    app.focus = Pane::List;
    keys(&mut app, &["Down"]);
    wait(&mut app);
    assert_eq!(app.record.as_ref().map(|r| r.sys_id.clone()), Some(id(2)));
    assert!(toasts(&app).contains("stays open"));
}

#[test]
fn a_related_record_opens_over_the_first_one_and_back() {
    let mut app = started(120, 40);
    keys(&mut app, &["Enter", "3"]);
    let record = app.record.as_mut().expect("record");
    let at = record
        .related_rows()
        .iter()
        .position(|(i, _)| record.related[*i].0.table == "incident")
        .expect("child incidents");
    record.related_cursor = at;
    keys(&mut app, &["Enter"]);
    assert_eq!(app.record.as_ref().map(|r| r.sys_id.clone()), Some(id(3)));
    assert_eq!(app.records.len(), 1);
    let effects = press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(calls(effects).is_empty(), "the first one is still loaded");
    let record = app.record.as_ref().expect("record");
    assert_eq!((record.sys_id.clone(), record.tab), (id(0), Tab::Related));
}

#[test]
fn tabs_load_when_opened_and_fail_on_their_own() {
    let mut app = started(120, 40);
    let asked = raw_keys(&mut app, KeyCode::Enter);
    assert!(
        asked
            .iter()
            .all(|c| matches!(c, Call::Record { .. } | Call::Meta { .. })),
        "{asked:?}"
    );
    settle(&mut app, asked.into_iter().map(Effect::Call).collect());
    let asked = raw_keys(&mut app, KeyCode::Char('2'));
    let [Call::Activity { req, .. }] = asked.as_slice() else {
        panic!("{asked:?}");
    };
    let fault = Fault {
        code: "permission",
        message: "no".into(),
    };
    app.update(Msg::Reply(Reply::Activity(*req, Err(fault))));
    keys(&mut app, &["5"]);
    let record = app.record.as_ref().expect("record");
    assert!(record.slas.ready().is_some());
    assert!(matches!(record.activity, Load::Failed(_)));
    assert!(raw_keys(&mut app, KeyCode::Char('5')).is_empty(), "once");
    keys(&mut app, &["2", "r"]);
    let record = app.record.as_ref().expect("record");
    assert!(record.activity.ready().is_some(), "r tries again");
}

#[test]
fn leaving_with_changes_asks_first() {
    let mut app = started(120, 40);
    keys(&mut app, &["Enter"]);
    field_at(&mut app, "active");
    keys(&mut app, &["Enter"]);
    assert!(keys(&mut app, &["q"]).is_empty());
    assert!(keys(&mut app, &["n"]).is_empty());
    assert!(app.overlays.is_empty());
    assert_eq!(keys(&mut app, &["q", "y"]), [Effect::Quit]);
    // Closing the record asks too.
    keys(&mut app, &["Esc", "y"]);
    assert!(app.record.is_none());
    assert_eq!(keys(&mut app, &["q"]), [Effect::Quit]);
}

#[test]
fn a_save_on_its_way_is_not_abandoned() {
    let mut app = started(120, 40);
    keys(&mut app, &["m"]);
    if app.overlays.is_empty() {
        keys(&mut app, &["Down", "m"]);
    }
    raw_keys(&mut app, KeyCode::Enter);
    assert!(app.is_sending());
    assert!(press(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL).is_empty());
    assert!(toasts(&app).contains("on its way"));
    keys(&mut app, &["Esc"]);
    assert!(app.is_sending(), "Esc does not hide it either");
}

#[test]
fn the_filter_builder_makes_the_query() {
    let mut app = started(120, 40);
    keys(
        &mut app,
        &["gi", "f", "n", "urgency", "Enter", "high", "Enter"],
    );
    keys(
        &mut app,
        &["n", "short desc", "Enter", "vpn^x", "Enter", "o"],
    );
    let asked = calls(press(&mut app, KeyCode::Char('s'), KeyModifiers::CONTROL));
    let Some(Call::List { query, .. }) = asked.first() else {
        panic!("{asked:?}");
    };
    assert_eq!(
        query,
        "active=true^urgency=1^ORshort_descriptionLIKEvpn^^x^ORDERBYpriority^ORDERBYDESCsys_updated_on"
    );
    settle(&mut app, asked.into_iter().map(Effect::Call).collect());
    keys(&mut app, &["0"]);
    assert_eq!(app.list.query, "active=true");
}

#[test]
fn the_palette_finds_sections_tables_actions_and_records() {
    let mut app = started(120, 40);
    let found = |app: &App| match app.overlays.last() {
        Some(Overlay::Palette(p)) => app.palette_items(p),
        other => panic!("{other:?}"),
    };
    keys(&mut app, &["C-k", "update set"]);
    assert_eq!(
        found(&app)[0].action,
        PaletteDo::Section("update-sets".into())
    );
    keys(&mut app, &["Esc", "C-k", "print"]);
    let before = found(&app);
    assert!(!before.iter().any(|i| i.group == "Record"), "not yet");
    wait(&mut app);
    let items = found(&app);
    let record = items
        .iter()
        .position(|i| i.group == "Record")
        .expect("record");
    for _ in 0..record {
        keys(&mut app, &["Down"]);
    }
    keys(&mut app, &["Enter"]);
    assert_eq!(app.record.as_ref().map(|r| r.sys_id.clone()), Some(id(5)));
    // A number goes straight to its record.
    keys(&mut app, &["C-k", "inc0000042", "Enter"]);
    assert_eq!(app.record.as_ref().map(|r| r.sys_id.clone()), Some(id(42)));
    // Any table opens by its name.
    keys(&mut app, &["t", "u_my_table", "Enter"]);
    assert_eq!(app.list.table, "u_my_table");
    assert!(!app.list.columns.is_empty());
}

#[test]
fn a_pasted_number_opens_the_record_and_other_text_does_nothing() {
    let mut app = started(120, 40);
    let effects = app.update(Msg::Paste("inc0000042\n".into()));
    assert!(
        matches!(&effects[..], [Effect::Call(Call::Locate { text, .. })] if text == "INC0000042")
    );
    assert!(app.update(Msg::Paste("qqq rm -rf".into())).is_empty());
}

#[test]
fn going_back_returns_to_the_same_list() {
    let mut app = started(120, 40);
    keys(&mut app, &["Down", "Down", "gi"]);
    assert_eq!(app.list.title, "Incidents");
    let effects = press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(calls(effects).is_empty(), "the rows are still there");
    assert_eq!(
        (app.list.title.as_str(), app.list.cursor),
        ("Assigned to me", 2)
    );
}

#[test]
fn sorting_asks_the_instance() {
    let mut app = started(120, 40);
    let asked = |app: &mut App| {
        app.sort_by("sys_updated_on");
        let effects = std::mem::take(&mut app.effects);
        let query = match &calls(effects.clone())[0] {
            Call::List { query, .. } => query.clone(),
            other => panic!("{other:?}"),
        };
        settle(app, effects);
        query
    };
    assert!(asked(&mut app).ends_with("^ORDERBYDESCsys_updated_on"));
    assert!(asked(&mut app).ends_with("^ORDERBYsys_updated_on"));
}

#[test]
fn a_new_record_names_what_is_missing_and_is_created_once() {
    let mut app = started(120, 40);
    keys(&mut app, &["n"]);
    assert!(
        toasts(&app).contains("one kind of record"),
        "a task has no form"
    );
    keys(&mut app, &["gi", "n"]);
    assert!(app.record.as_ref().is_some_and(RecordState::is_new));
    field_at(&mut app, "urgency");
    keys(&mut app, &["Enter", "high", "Enter", "C-s"]);
    assert!(toasts(&app).contains("Short description is required"));
    keys(&mut app, &["Enter", "New printer", "Enter", "C-s"]);
    let asked = raw_keys(&mut app, KeyCode::Enter);
    let [Call::Create { table, body, .. }] = asked.as_slice() else {
        panic!("{asked:?}");
    };
    assert_eq!(table, "incident");
    assert_eq!(
        Value::Object(body.clone()),
        json!({ "urgency": "1", "short_description": "New printer" })
    );
    settle(&mut app, asked.into_iter().map(Effect::Call).collect());
    assert_eq!(app.record.as_ref().map(|r| r.sys_id.clone()), Some(id(99)));
    assert!(app.overlays.is_empty());
}

#[test]
fn links_point_to_the_record_or_the_list() {
    let mut app = started(120, 40);
    let effects = keys(&mut app, &["y", "Y", "o"]);
    let link = format!(
        "https://acme.service-now.com/nav_to.do?uri=incident.do%3Fsys_id%3D{}",
        id(0)
    );
    assert_eq!(
        effects,
        [
            Effect::Copy("INC0000000".into()),
            Effect::Copy(link.clone()),
            Effect::Open(link)
        ]
    );
    keys(&mut app, &["gr"]);
    let [Effect::Open(url)] = &keys(&mut app, &["o"])[..] else {
        panic!();
    };
    assert!(
        url.starts_with(
            "https://acme.service-now.com/nav_to.do?uri=sc_request_list.do%3Fsysparm_query%3D"
        ),
        "{url}"
    );
}

#[test]
fn pins_are_kept_and_taken_out() {
    let mut app = started(120, 40);
    keys(&mut app, &["t", "u_new", "Enter"]);
    assert_eq!(
        keys(&mut app, &["p"]),
        [Effect::SavePins(vec!["u_vendor".into(), "u_new".into()])]
    );
    assert!(
        app.rail()
            .iter()
            .any(|i| matches!(i, RailItem::Section(id, _) if id == "pin:u_new"))
    );
    assert_eq!(
        keys(&mut app, &["p"]),
        [Effect::SavePins(vec!["u_vendor".into()])]
    );
}

#[test]
fn auto_refresh_keeps_the_row_under_the_cursor() {
    let mut app = started(120, 40);
    keys(&mut app, &["Down", "Down", "R"]);
    let effects = app.update(Msg::Tick {
        now: NOW,
        ms: app.ms + 40_000,
    });
    let Some(Call::List { req, offset: 0, .. }) = calls(effects)
        .into_iter()
        .find(|c| matches!(c, Call::List { .. }))
    else {
        panic!("no refresh");
    };
    assert_eq!(app.list.rows.len(), 50, "the rows stay until the answer");
    let rows = (1..40)
        .rev()
        .map(|n| Row::new("task", &incident(n)))
        .collect();
    app.update(Msg::Reply(Reply::List(
        req,
        Ok(Page {
            rows,
            total: Some(39),
        }),
    )));
    assert_eq!(
        app.list.current().map(|r| r.sys_id.clone()),
        Some(id(2)),
        "the same record"
    );
}

#[test]
fn the_mouse_reaches_what_is_drawn() {
    let mut app = started(120, 40);
    let draw = |app: &mut App| {
        let backend = ratatui::backend::TestBackend::new(app.size.0, app.size.1);
        let mut terminal = ratatui::Terminal::new(backend).expect("terminal");
        let mut places = None;
        terminal
            .draw(|f| places = Some(crate::ui::draw(f, app)))
            .expect("draw");
        app.update(Msg::Drawn(places.expect("places")));
    };
    let click = |app: &mut App, wanted: &Hit| {
        draw(app);
        let (area, _) = app
            .places
            .hits
            .iter()
            .rev()
            .find(|(_, h)| h == wanted)
            .unwrap_or_else(|| panic!("{wanted:?} is not on screen"))
            .clone();
        let effects = app.update(Msg::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: area.x + area.width / 2,
            row: area.y,
            modifiers: KeyModifiers::NONE,
        }));
        settle(app, effects);
    };
    click(&mut app, &Hit::Row(3));
    assert_eq!((app.list.cursor, app.record.is_some()), (3, false));
    click(&mut app, &Hit::Row(3));
    assert_eq!(app.record.as_ref().map(|r| r.sys_id.clone()), Some(id(3)));
    click(&mut app, &Hit::Tab(Tab::Slas));
    assert_eq!(app.record.as_ref().map(|r| r.tab), Some(Tab::Slas));
    let incidents = app
        .rail()
        .iter()
        .position(|i| matches!(i, RailItem::Section(id, _) if id == "incidents"))
        .expect("incidents");
    click(&mut app, &Hit::Rail(incidents));
    assert_eq!(app.list.title, "Incidents");
    click(&mut app, &Hit::Column("number".into()));
    assert_eq!(app.list.order, [("number".to_owned(), false)]);
    click(&mut app, &Hit::Mark(1));
    assert_eq!(app.list.selected.len(), 1);
    // A click outside closes an overlay and reaches nothing under it.
    keys(&mut app, &["C-k"]);
    draw(&mut app);
    app.update(Msg::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 1,
        row: 6,
        modifiers: KeyModifiers::NONE,
    }));
    assert!(app.overlays.is_empty());
    assert_eq!(app.list.title, "Incidents");
    // The wheel moves what is under the pointer.
    draw(&mut app);
    let before = app.list.cursor;
    app.update(Msg::Mouse(MouseEvent {
        kind: MouseEventKind::ScrollDown,
        column: 60,
        row: 20,
        modifiers: KeyModifiers::NONE,
    }));
    assert_eq!(app.list.cursor, before + 3);
}

#[tokio::test]
async fn the_runner_drops_calls_that_a_newer_one_replaces() {
    struct Slow;
    impl Backend for Slow {
        async fn run(&self, call: Call, replies: Replies) {
            let pause = match &call {
                Call::List { offset: 0, .. } => 200,
                _ => 1,
            };
            tokio::time::sleep(std::time::Duration::from_millis(pause)).await;
            for reply in Fake::answer(call) {
                let _ = replies.send(reply);
            }
        }
    }
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let mut runner = crate::data::Runner::new(std::sync::Arc::new(Slow), tx);
    let list = |req, offset| Call::List {
        req,
        table: "incident".into(),
        query: String::new(),
        fields: Vec::new(),
        view: None,
        offset,
        limit: 5,
    };
    runner.start(list(1, 0));
    runner.start(Call::Me { req: 9 });
    runner.start(list(2, 5));
    drop(runner);
    let mut seen = Vec::new();
    while let Some(reply) = rx.recv().await {
        seen.push(match reply {
            Reply::List(req, _) | Reply::Me(req, _) => req,
            other => panic!("{other:?}"),
        });
    }
    seen.sort_unstable();
    assert_eq!(seen, [2, 9]);
}

fn edit_urgency(app: &mut App) {
    field_at(app, "urgency");
    keys(app, &["Enter", "high", "Enter"]);
    let record = app.record.as_ref().expect("record");
    assert_eq!(record.edits.len(), 1);
}

/// Edits that wait in the open record or under it.
fn waiting_edits(app: &App) -> usize {
    app.record
        .iter()
        .chain(&app.records)
        .map(|r| r.edits.len())
        .sum()
}

#[test]
fn closing_a_record_never_drops_the_edits_under_it() {
    for (width, key) in [(120, "Esc"), (160, "v")] {
        let mut app = started(width, 50);
        keys(&mut app, &["Enter"]);
        edit_urgency(&mut app);
        field_at(&mut app, "assignment_group");
        keys(&mut app, &["f"]);
        assert_eq!(app.records.len(), 1);
        app.focus = Pane::List;
        keys(&mut app, &[key]);
        assert_eq!(waiting_edits(&app), 1, "{key} at {width} columns");
    }
}

#[test]
fn dropping_a_new_record_keeps_the_edits_under_it() {
    let mut app = started(120, 40);
    keys(&mut app, &["gi", "Enter"]);
    edit_urgency(&mut app);
    keys(&mut app, &["n"]);
    edit_urgency(&mut app);
    keys(&mut app, &["D", "y"]);
    let record = app.record.as_ref().expect("the record under the new one");
    assert!(!record.is_new());
    assert_eq!(record.edits.len(), 1);
}

#[test]
fn leaving_asks_about_a_field_that_is_being_typed() {
    let mut app = started(120, 40);
    keys(&mut app, &["Enter"]);
    field_at(&mut app, "short_description");
    keys(&mut app, &["Enter"]);
    assert!(!app.has_unsent(), "nothing is typed yet");
    assert!(keys(&mut app, &["!", "C-c"]).is_empty());
    keys(&mut app, &["n"]);
    let record = app.record.as_ref().expect("record");
    assert!(
        record
            .inline
            .as_ref()
            .is_some_and(|i| i.editor.text.ends_with('!'))
    );
}

#[test]
fn the_preview_does_not_replace_a_field_with_a_wrong_value() {
    let mut app = started(160, 50);
    keys(&mut app, &["Enter"]);
    field_at(&mut app, "due_date");
    keys(&mut app, &["Enter", "junk", "Enter"]);
    app.focus = Pane::List;
    keys(&mut app, &["Down"]);
    wait(&mut app);
    assert_eq!(app.record.as_ref().map(|r| r.sys_id.clone()), Some(id(0)));
    assert!(toasts(&app).contains("stays open"));
    // Closing it on request drops what was typed, after a question.
    app.focus = Pane::Record;
    keys(&mut app, &["Esc"]);
    assert!(app.record.as_ref().is_some_and(|r| r.inline.is_none()));
}

#[test]
fn signing_in_waits_for_a_save_that_is_on_its_way() {
    let mut app = started(120, 40);
    keys(&mut app, &["w", "a note", "C-s"]);
    raw_keys(&mut app, KeyCode::Enter);
    app.connection.signed_out = Some("The session ended.".into());
    for code in [KeyCode::Enter, KeyCode::Char('a')] {
        assert!(press(&mut app, code, KeyModifiers::NONE).is_empty());
    }
    assert!(toasts(&app).contains("on its way"));
}

#[test]
fn a_refresh_of_a_long_list_skips_no_row() {
    let page = |from: usize, to: usize, total| Page {
        rows: (from..to).map(|n| Row::new("task", &incident(n))).collect(),
        total: Some(total),
    };
    let mut app = started(120, 40);
    app.list.rows = page(0, 800, 900).rows;
    app.list.next_offset = 800;
    let refresh = |app: &mut App| {
        app.refresh_list();
        match &calls(std::mem::take(&mut app.effects))[0] {
            Call::List { req, limit, .. } => (*req, *limit),
            other => panic!("{other:?}"),
        }
    };
    let (req, limit) = refresh(&mut app);
    assert_eq!(limit, 500);
    app.update(Msg::Reply(Reply::List(req, Ok(page(0, 500, 900)))));
    assert_eq!(app.list.next_offset, 500);
    assert!(!app.list.exhausted);
    // Access rules hid 40 of the first 100 rows: the refresh covers the same
    // range, not only as many as are shown.
    app.list.rows.truncate(60);
    app.list.next_offset = 100;
    assert_eq!(refresh(&mut app).1, 100);
}

#[test]
fn a_reload_while_the_related_lists_load_asks_for_them_again() {
    let mut app = started(120, 40);
    keys(&mut app, &["Enter"]);
    let asked = raw_keys(&mut app, KeyCode::Char('3'));
    assert!(matches!(asked[0], Call::Related { .. }));
    let asked = raw_keys(&mut app, KeyCode::Char('r'));
    let mut next = Vec::new();
    for call in asked {
        for reply in Fake::answer(call) {
            next.extend(calls(app.update(Msg::Reply(reply))));
        }
    }
    assert!(
        next.iter().any(|c| matches!(c, Call::Related { .. })),
        "{next:?}"
    );
}

fn sign_in_again(app: &mut App) {
    app.resume(app.connection.clone(), app.profiles.clone());
    let effects = app.start();
    settle(app, effects);
}

#[test]
fn a_sign_in_forgets_answers_that_will_not_come() {
    let mut app = started(120, 40);
    keys(&mut app, &["Enter", "4"]);
    assert!(matches!(
        raw_keys(&mut app, KeyCode::Enter)[0],
        Call::Download { .. }
    ));
    app.update(Msg::Paste("INC0000042".into()));
    sign_in_again(&mut app);
    assert_eq!(app.activity(), None);
    assert!(matches!(
        raw_keys(&mut app, KeyCode::Enter)[0],
        Call::Download { .. }
    ));

    let mut app = started(120, 40);
    keys(&mut app, &["i"]);
    raw_keys(&mut app, KeyCode::Enter);
    assert_eq!(app.activity(), Some("searching"));
    sign_in_again(&mut app);
    wait(&mut app);
    assert_eq!(app.activity(), None, "the search runs again");
}

#[test]
fn the_palette_takes_its_answers_under_a_question() {
    let mut app = started(120, 40);
    keys(&mut app, &["Enter"]);
    edit_urgency(&mut app);
    for c in ["C-k", "print"] {
        keys(&mut app, &[c]);
    }
    let asked = calls(app.update(Msg::Tick {
        now: NOW,
        ms: app.ms + 1_000,
    }));
    assert!(matches!(asked[0], Call::Search { .. }));
    keys(&mut app, &["C-c"]);
    settle(&mut app, asked.into_iter().map(Effect::Call).collect());
    keys(&mut app, &["n"]);
    let Some(Overlay::Palette(palette)) = app.overlays.last() else {
        panic!("{:?}", app.overlays);
    };
    assert!(palette.tables.ready().is_some() && palette.records.ready().is_some());
}

#[test]
fn a_late_answer_does_not_take_over_what_came_after() {
    let mut app = started(120, 40);
    keys(&mut app, &["Enter"]);
    let asked = app.update(Msg::Paste("INC0000042".into()));
    field_at(&mut app, "description");
    keys(&mut app, &["Enter", "more"]);
    settle(&mut app, asked);
    keys(&mut app, &["C-s"]);
    let record = app.record.as_ref().expect("record");
    assert_eq!((record.sys_id.clone(), record.edits.len()), (id(0), 1));

    let mut app = started(120, 40);
    app.metas.clear();
    let mut asked = raw_keys(&mut app, KeyCode::Char('f'));
    assert!(matches!(&asked[..], [Call::Meta { table }] if table == "task"));
    raw_keys(&mut app, KeyCode::Char('g'));
    asked.extend(raw_keys(&mut app, KeyCode::Char('i')));
    settle(&mut app, asked.into_iter().map(Effect::Call).collect());
    assert!(app.overlays.is_empty(), "{:?}", app.overlays);
}

#[test]
fn refresh_asks_again_for_fields_that_did_not_come() {
    let mut app = started(120, 40);
    let fault = Fault {
        code: "timeout",
        message: "no answer".into(),
    };
    app.metas.insert("task".into(), Load::Failed(fault));
    keys(&mut app, &["r", "f"]);
    assert!(
        matches!(app.overlays.last(), Some(Overlay::Filter(_))),
        "{:?}",
        app.overlays
    );
}

#[test]
fn a_review_of_several_says_various_only_when_they_differ() {
    let mut app = started(120, 40);
    keys(&mut app, &["Down", "Space", "Space", "i"]);
    let group = app.list.rows[1].cell("assignment_group").clone();
    let Some(Overlay::Form(form)) = app.overlays.last_mut() else {
        panic!("{:?}", app.overlays);
    };
    form.fields[1].cell = crate::model::Cell {
        value: id(800),
        display: "Bob Stone".into(),
    };
    keys(&mut app, &["C-s"]);
    let old = |app: &App, field: &str| {
        let lines = &review(app).lines;
        lines
            .iter()
            .find(|l| l.field == field)
            .map(|l| l.old.clone())
    };
    assert_eq!(old(&app, "assigned_to").as_deref(), Some(""));
    assert_eq!(
        old(&app, "assignment_group").as_deref(),
        Some(group.display.as_str())
    );
    keys(&mut app, &["Esc", "Esc"]);
    app.list.rows[1].merge(Row::new(
        "task",
        &json!({ "assigned_to": cell(&id(7), "Grace Hopper") }),
    ));
    keys(&mut app, &["m"]);
    assert_eq!(old(&app, "assigned_to").as_deref(), Some("(various)"));
}

#[test]
fn a_click_on_another_field_keeps_what_was_typed() {
    let mut app = started(120, 40);
    keys(&mut app, &["Enter"]);
    field_at(&mut app, "due_date");
    let other = app.record.as_ref().expect("record").cursor - 1;
    keys(&mut app, &["Enter", "junk"]);
    for hit in [Hit::Detail(other), Hit::Tab(Tab::History)] {
        app.click(hit);
        let record = app.record.as_ref().expect("record");
        assert_eq!((record.cursor, record.tab), (other + 1, Tab::Details));
        assert!(record.inline.as_ref().is_some_and(|i| i.error.is_some()));
    }
    keys(&mut app, &["C-u", "tomorrow"]);
    app.click(Hit::Detail(other));
    let record = app.record.as_ref().expect("record");
    assert!(record.inline.is_none());
    assert_eq!((record.cursor, record.edits.len()), (other, 1));
}

#[test]
fn an_ended_session_shows_wherever_it_is_met() {
    let gone = || Fault {
        code: "auth",
        message: "GET x failed with HTTP 401: User Not Authenticated".into(),
    };
    let mut app = started(120, 40);
    keys(&mut app, &["Enter"]);
    let asked = raw_keys(&mut app, KeyCode::Char('2'));
    let [Call::Activity { req, .. }] = asked.as_slice() else {
        panic!("{asked:?}");
    };
    app.update(Msg::Reply(Reply::Activity(*req, Err(gone()))));
    assert!(app.connection.signed_out.take().is_some());
    sign_in_again(&mut app);
    let record = app.record.as_ref().expect("record");
    assert!(
        record.activity.ready().is_some(),
        "it loads after the sign-in"
    );
    let mut app = started(120, 40);
    app.update(Msg::Reply(Reply::Count("flows".into(), Err(gone()))));
    assert!(app.connection.signed_out.is_none(), "a count ends nothing");
}

//! Records and what a table is made of.

use crate::text;
use serde_json::{Map, Value};

pub const REDACTED: &str = "[REDACTED]";

/// True for a field name that says it holds a secret.
pub fn is_sensitive(field: &str) -> bool {
    let name: String = field
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    [
        "password",
        "passwd",
        "secret",
        "private_key",
        "api_key",
        "apikey",
        "access_key",
        "passphrase",
        "passcode",
    ]
    .iter()
    .any(|w| name.contains(w))
        || name.split('_').any(|part| {
            matches!(
                part,
                "token"
                    | "cookie"
                    | "credential"
                    | "credentials"
                    | "authorization"
                    | "pwd"
                    | "pw"
                    | "pass"
                    | "pin"
                    | "ssn"
                    | "otp"
            )
        })
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Cell {
    /// What the database stores.
    pub value: String,
    /// What a person reads.
    pub display: String,
}

impl Cell {
    pub fn is_empty(&self) -> bool {
        self.value.is_empty() && self.display.is_empty()
    }
}

static NO_CELL: Cell = Cell {
    value: String::new(),
    display: String::new(),
};

/// One record, in the order the instance sent its fields. Secrets are gone
/// before a row exists, so nothing can show, search or copy them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Row {
    /// The table of the record itself, which can extend the one that was read.
    pub table: String,
    pub sys_id: String,
    pub cells: Vec<(String, Cell)>,
}

impl Row {
    pub fn new(table: &str, value: &Value) -> Self {
        let empty = Map::new();
        let map = value.as_object().unwrap_or(&empty);
        // A property or a parameter keeps its secret in a plain `value` field.
        let names_secret = ["name", "label"]
            .iter()
            .filter_map(|f| map.get(*f))
            .any(|v| is_sensitive(&cell(v).value));
        let cells: Vec<(String, Cell)> = map
            .iter()
            .map(|(name, value)| {
                let mut cell = cell(value);
                let secret = is_sensitive(name) || (name == "value" && names_secret);
                if secret && !cell.is_empty() {
                    cell = Cell {
                        value: REDACTED.into(),
                        display: REDACTED.into(),
                    };
                }
                (name.clone(), cell)
            })
            .collect();
        let mut row = Self {
            table: table.to_owned(),
            sys_id: String::new(),
            cells,
        };
        row.sys_id = row.value("sys_id").to_owned();
        let class = row.value("sys_class_name");
        if !class.is_empty() && sn_core::query::check_name("table", class).is_ok() {
            row.table = class.to_owned();
        }
        row
    }

    pub fn cell(&self, field: &str) -> &Cell {
        self.cells
            .iter()
            .find(|(name, _)| name == field)
            .map_or(&NO_CELL, |(_, cell)| cell)
    }

    pub fn has(&self, field: &str) -> bool {
        self.cells.iter().any(|(name, _)| name == field)
    }

    pub fn value(&self, field: &str) -> &str {
        &self.cell(field).value
    }

    pub fn display(&self, field: &str) -> &str {
        &self.cell(field).display
    }

    /// Takes the fields of `newer`, and keeps those it does not have.
    pub fn merge(&mut self, newer: Row) {
        for (name, cell) in newer.cells {
            match self.cells.iter_mut().find(|(n, _)| *n == name) {
                Some((_, old)) => *old = cell,
                None => self.cells.push((name, cell)),
            }
        }
    }

    /// What people call the record: its number or its name.
    pub fn name(&self) -> String {
        for field in ["number", "name", "title", "label"] {
            let text = text::line(self.display(field));
            if !text.is_empty() {
                return text;
            }
        }
        let approval = text::line(self.display("sysapproval"));
        if !approval.is_empty() {
            return approval;
        }
        let summary = text::line(self.display("short_description"));
        match summary.is_empty() {
            true => self.sys_id.clone(),
            false => summary,
        }
    }

    /// The sentence that says what the record is about.
    pub fn summary(&self) -> String {
        [
            "short_description",
            "sysapproval.short_description",
            "description",
            "name",
        ]
        .into_iter()
        .map(|f| text::line(self.display(f)))
        .find(|t| !t.is_empty() && *t != self.name())
        .unwrap_or_default()
    }

    pub fn matches(&self, words: &[String]) -> bool {
        words.iter().all(|w| {
            self.cells
                .iter()
                .any(|(n, c)| n != "sys_id" && c.display.to_lowercase().contains(w))
        })
    }

    pub fn baseline(&self) -> Baseline {
        Baseline {
            updated: self.value("sys_updated_on").to_owned(),
            mods: self.value("sys_mod_count").to_owned(),
        }
    }
}

fn cell(value: &Value) -> Cell {
    let plain = |v: &Value| match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    };
    match value {
        Value::Object(map) if map.contains_key("display_value") || map.contains_key("value") => {
            let value = map.get("value").map(plain).unwrap_or_default();
            let display = map.get("display_value").map(plain).unwrap_or_default();
            Cell {
                display: match display.is_empty() {
                    true => value.clone(),
                    false => display,
                },
                value,
            }
        }
        other => {
            let text = plain(other);
            Cell {
                value: text.clone(),
                display: text,
            }
        }
    }
}

/// What tells one version of a record from the next.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Baseline {
    pub updated: String,
    pub mods: String,
}

impl Baseline {
    /// True when both say something and it differs. A list can lack
    /// `sys_mod_count`, so only what both have counts.
    pub fn conflicts(&self, now: &Baseline) -> bool {
        let differs = |a: &str, b: &str| !a.is_empty() && !b.is_empty() && a != b;
        differs(&self.updated, &now.updated) || differs(&self.mods, &now.mods)
    }
}

/// How a field is edited and shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Text,
    Long,
    Choice,
    Reference,
    Bool,
    Date,
    DateTime,
    Number,
    Journal,
    /// Shown, but not edited here: lists of references, conditions, secrets.
    Fixed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Field {
    pub name: String,
    pub label: String,
    pub kind: Kind,
    pub mandatory: bool,
    pub read_only: bool,
    /// The table a reference points to.
    pub reference: Option<String>,
    pub choices: Vec<(String, String)>,
    /// HTML is shown as text, so editing it as text would destroy it.
    pub html: bool,
}

impl Field {
    /// A field nothing is known about.
    pub fn guess(name: &str) -> Self {
        let kind = match name {
            "comments" | "work_notes" | "comments_and_work_notes" => Kind::Journal,
            "description" | "close_notes" | "script" | "text" => Kind::Long,
            "active" => Kind::Bool,
            "assigned_to" | "caller_id" | "opened_by" | "assignment_group" | "cmdb_ci" => {
                Kind::Reference
            }
            _ if name.starts_with("sys_") || name.contains('.') => Kind::Fixed,
            _ => Kind::Text,
        };
        Self {
            name: name.to_owned(),
            label: text::humanize(name),
            kind,
            mandatory: false,
            read_only: kind == Kind::Fixed,
            reference: match name {
                "assignment_group" => Some("sys_user_group".into()),
                "cmdb_ci" => Some("cmdb_ci".into()),
                _ if kind == Kind::Reference => Some("sys_user".into()),
                _ => None,
            },
            choices: Vec::new(),
            html: false,
        }
    }

    /// Why the field cannot be edited here, or `None` when it can.
    pub fn locked(&self) -> Option<&'static str> {
        if is_sensitive(&self.name) {
            Some("secrets are not shown or changed here")
        } else if self.html {
            Some("this is a formatted text; press o to edit it in the browser")
        } else if self.read_only || self.name.starts_with("sys_") {
            Some("this field is read-only")
        } else if self.kind == Kind::Fixed {
            Some("this kind of field is not edited here; press o to edit it in the browser")
        } else {
            None
        }
    }
}

/// The fields of a table.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Meta {
    pub fields: Vec<Field>,
}

impl Meta {
    pub fn field(&self, name: &str) -> Option<&Field> {
        self.fields.iter().find(|f| f.name == name)
    }

    /// From the columns of `GET /api/now/ui/meta/{table}`.
    pub fn from_ui(result: &Value) -> Option<Self> {
        let columns = result.get("columns")?.as_object()?;
        let fields = columns
            .iter()
            .filter(|(name, _)| !name.is_empty())
            .map(|(name, c)| {
                let text = |key: &str| c.get(key).and_then(Value::as_str).unwrap_or_default();
                let flag = |key: &str| match c.get(key) {
                    Some(Value::Bool(b)) => *b,
                    Some(Value::String(s)) => s == "true",
                    _ => false,
                };
                let choices = c
                    .get("choices")
                    .and_then(Value::as_array)
                    .map(|list| {
                        list.iter()
                            .filter_map(|c| {
                                Some((
                                    c.get("value").and_then(plain_text)?,
                                    c.get("label").and_then(plain_text)?,
                                ))
                            })
                            .filter(|(value, _)| !value.is_empty())
                            .collect()
                    })
                    .unwrap_or_default();
                let length = c
                    .get("max_length")
                    .and_then(|v| v.as_u64().or_else(|| v.as_str()?.parse().ok()))
                    .unwrap_or_default();
                let kind = match text("internal_type") {
                    "" => text("type"),
                    other => other,
                };
                field(
                    name,
                    text("label"),
                    kind,
                    flag("mandatory"),
                    flag("read_only") || flag("sys_readonly"),
                    Some(text("reference")).filter(|r| !r.is_empty()),
                    choices,
                    length,
                )
            })
            .collect();
        Some(Self { fields })
    }

    /// From `sys_dictionary` and `sys_choice` rows, when the instance has no
    /// `ui/meta`. `tables` goes from the table to its ancestors.
    pub fn from_dictionary(tables: &[String], dictionary: &[Row], choices: &[Row]) -> Self {
        let rank = |row: &Row| {
            tables
                .iter()
                .position(|t| t == row.value("name"))
                .unwrap_or(usize::MAX)
        };
        let mut rows: Vec<&Row> = dictionary
            .iter()
            .filter(|r| !r.value("element").is_empty() && rank(r) < usize::MAX)
            .collect();
        rows.sort_by_key(|r| rank(r));
        let mut fields: Vec<Field> = Vec::new();
        for row in rows {
            let name = row.value("element");
            // The nearest table wins.
            if fields.iter().any(|f| f.name == name) {
                continue;
            }
            let nearest = choices
                .iter()
                .filter(|c| c.value("element") == name && rank(c) < usize::MAX)
                .map(rank)
                .min();
            let mut list: Vec<&Row> = choices
                .iter()
                .filter(|c| c.value("element") == name && Some(rank(c)) == nearest)
                .collect();
            list.sort_by_key(|c| c.value("sequence").parse::<i64>().unwrap_or(i64::MAX));
            fields.push(field(
                name,
                row.value("column_label"),
                row.value("internal_type"),
                row.value("mandatory") == "true",
                row.value("read_only") == "true",
                Some(row.value("reference")).filter(|r| !r.is_empty()),
                list.iter()
                    .map(|c| (c.value("value").to_owned(), c.value("label").to_owned()))
                    .collect(),
                row.value("max_length").parse().unwrap_or_default(),
            ));
        }
        Self { fields }
    }
}

fn plain_text(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

#[allow(clippy::too_many_arguments)]
fn field(
    name: &str,
    label: &str,
    internal: &str,
    mandatory: bool,
    read_only: bool,
    reference: Option<&str>,
    choices: Vec<(String, String)>,
    length: u64,
) -> Field {
    let kind = match internal {
        _ if is_sensitive(name) => Kind::Fixed,
        "reference" => Kind::Reference,
        "boolean" => Kind::Bool,
        "glide_date" => Kind::Date,
        "glide_date_time" | "due_date" => Kind::DateTime,
        "journal" | "journal_input" | "journal_list" => Kind::Journal,
        _ if !choices.is_empty() => Kind::Choice,
        "integer" | "decimal" | "float" | "percent_complete" | "longint" => Kind::Number,
        "string" | "choice" | "email" | "url" | "ph_number" | "phone_number_e164"
        | "phone_number" | "translated_text" | "translated_field" | "string_full_utf8"
        | "ip_addr" | "ip_address" | "domain_path" | "name_values"
            if length <= 255 =>
        {
            Kind::Text
        }
        "string" | "translated_text" | "string_full_utf8" | "script" | "script_plain"
        | "script_server" | "xml" | "css" | "html" | "html_template" | "translated_html"
        | "json" | "multi_two_lines" | "multi_small" | "wiki_text" => Kind::Long,
        _ => Kind::Fixed,
    };
    Field {
        name: name.to_owned(),
        label: match text::line(label) {
            label if label.is_empty() => text::humanize(name),
            label => label,
        },
        kind,
        mandatory,
        read_only,
        reference: reference.map(str::to_owned),
        choices,
        html: matches!(internal, "html" | "translated_html" | "html_template"),
    }
}

/// The parts of the Details tab, in reading order.
const GROUPS: &[(&str, &[&str])] = &[
    (
        "Summary",
        &[
            "number",
            "name",
            "short_description",
            "description",
            "state",
            "incident_state",
            "problem_state",
            "request_state",
            "stage",
            "workflow_state",
            "on_hold",
            "on_hold_reason",
            "hold_reason",
            "priority",
            "impact",
            "urgency",
            "severity",
            "risk",
            "type",
            "category",
            "subcategory",
            "approval",
            "escalation",
            "active",
        ],
    ),
    (
        "People",
        &[
            "caller_id",
            "requested_for",
            "request.requested_for",
            "opened_by",
            "assigned_to",
            "assignment_group",
            "approver",
            "author",
            "manager",
            "owned_by",
            "managed_by",
            "support_group",
            "watch_list",
            "work_notes_list",
            "company",
            "department",
            "location",
            "contact_type",
        ],
    ),
    (
        "Subject",
        &[
            "cmdb_ci",
            "business_service",
            "service_offering",
            "parent",
            "parent_incident",
            "problem_id",
            "rfc",
            "caused_by",
            "change_request",
            "request",
            "request_item",
            "cat_item",
            "sysapproval",
            "source_table",
            "collection",
            "when",
            "condition",
            "filter_condition",
            "script",
            "text",
        ],
    ),
    (
        "Timing",
        &[
            "opened_at",
            "due_date",
            "sla_due",
            "expected_start",
            "start_date",
            "end_date",
            "work_start",
            "work_end",
            "follow_up",
            "resolved_at",
            "closed_at",
            "business_duration",
            "calendar_duration",
            "time_worked",
            "reopen_count",
            "made_sla",
        ],
    ),
    (
        "Plan",
        &[
            "justification",
            "implementation_plan",
            "risk_impact_analysis",
            "backout_plan",
            "test_plan",
        ],
    ),
    (
        "Outcome",
        &[
            "close_code",
            "resolution_code",
            "close_notes",
            "cause_notes",
            "fix_notes",
            "workaround",
            "resolved_by",
            "closed_by",
        ],
    ),
];

/// A line of the Details tab.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Detail {
    Group(&'static str),
    Field(String),
}

/// The fields of a record in groups. Without `empty`, fields without a value
/// are left out, except those in `keep`.
pub fn details(row: &Row, meta: Option<&Meta>, empty: bool, keep: &[String]) -> Vec<Detail> {
    let mut names: Vec<&str> = row.cells.iter().map(|(n, _)| n.as_str()).collect();
    if let Some(meta) = meta {
        for f in &meta.fields {
            if !row.has(&f.name) {
                names.push(&f.name);
            }
        }
    }
    let shown = |name: &str| {
        let journal = match meta.and_then(|m| m.field(name)) {
            Some(f) => f.kind == Kind::Journal,
            None => Field::guess(name).kind == Kind::Journal,
        };
        !journal
            && (empty
                || !row.cell(name).value.is_empty()
                || !row.cell(name).display.is_empty()
                || keep.iter().any(|k| k == name))
    };
    let mut out = Vec::new();
    let mut push = |title: &'static str, fields: Vec<&str>| {
        if !fields.is_empty() {
            out.push(Detail::Group(title));
            out.extend(fields.into_iter().map(|f| Detail::Field(f.to_owned())));
        }
    };
    for (title, fields) in GROUPS {
        push(
            title,
            fields
                .iter()
                .copied()
                .filter(|f| names.contains(f) && shown(f))
                .collect(),
        );
    }
    let grouped = |name: &str| GROUPS.iter().any(|(_, fields)| fields.contains(&name));
    let mut rest: Vec<&str> = names
        .iter()
        .copied()
        .filter(|n| !grouped(n) && shown(n))
        .collect();
    rest.sort_unstable();
    let (system, other): (Vec<&str>, Vec<&str>) =
        rest.into_iter().partition(|n| n.starts_with("sys_"));
    push("More", other);
    push("System", system);
    out
}

/// The fields a new record starts with: what is required, then what people
/// usually fill in.
pub fn new_record_fields(meta: &Meta) -> Vec<String> {
    const USUAL: &[&str] = &[
        "short_description",
        "caller_id",
        "requested_for",
        "name",
        "category",
        "type",
        "impact",
        "urgency",
        "assignment_group",
        "assigned_to",
        "cmdb_ci",
        "description",
    ];
    let editable = |f: &&Field| f.locked().is_none() && f.kind != Kind::Journal;
    let mut out: Vec<String> = meta
        .fields
        .iter()
        .filter(editable)
        .filter(|f| f.mandatory)
        .map(|f| f.name.clone())
        .collect();
    for name in USUAL {
        if meta.field(name).as_ref().is_some_and(editable) && !out.iter().any(|f| f == name) {
            out.push((*name).to_owned());
        }
    }
    out.sort_by_key(|n| USUAL.iter().position(|u| u == n).unwrap_or(USUAL.len()));
    out
}

/// States that end the work, best first: Resolved, then completed, then the
/// rest. The label decides, because numbers differ by table and instance.
pub fn closing_states(field: &Field) -> Vec<(String, String)> {
    let rank = |label: &str| {
        let label = label.to_lowercase();
        if label.contains("resolved") {
            Some(0)
        } else if label.contains("complete") && !label.contains("incomplete") {
            Some(1)
        } else if label == "closed" {
            Some(2)
        } else if ["closed", "incomplete", "cancel", "skipped"]
            .iter()
            .any(|w| label.contains(w))
        {
            Some(3)
        } else {
            None
        }
    };
    let mut out: Vec<(i32, (String, String))> = field
        .choices
        .iter()
        .filter_map(|c| Some((rank(&c.1)?, c.clone())))
        .collect();
    out.sort_by_key(|(rank, _)| *rank);
    out.into_iter().map(|(_, c)| c).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn secrets_never_reach_a_row() {
        let row = Row::new(
            "sys_user",
            &json!({
                "sys_id": "1", "user_password": {"display_value": "****", "value": "hash"},
                "api_key": "k", "token": "", "name": "Ada",
            }),
        );
        assert_eq!(row.value("user_password"), REDACTED);
        assert_eq!(row.value("api_key"), REDACTED);
        assert_eq!(row.value("token"), "", "an empty secret is just empty");
        assert_eq!(row.display("name"), "Ada");
        let property = Row::new(
            "sys_properties",
            &json!({ "name": "mail.smtp.password", "value": "hunter2" }),
        );
        assert_eq!(property.value("value"), REDACTED);
        assert!(!is_sensitive("tokenized_name"));
        assert!(Field::guess("client_secret").locked().is_some());
    }

    #[test]
    fn rows_know_their_class_and_name() {
        let row = Row::new(
            "task",
            &json!({
                "sys_id": {"display_value": "a", "value": "a"},
                "sys_class_name": {"display_value": "Incident", "value": "incident"},
                "number": {"display_value": "INC1", "value": "INC1"},
                "assigned_to": {"display_value": "Ada", "value": "6816"},
            }),
        );
        assert_eq!(
            (row.table.as_str(), row.name().as_str()),
            ("incident", "INC1")
        );
        assert!(row.matches(&["ada".into()]) && !row.matches(&["6816".into()]));
    }

    #[test]
    fn a_baseline_conflicts_only_on_what_both_sides_know() {
        let base = |u: &str, m: &str| Baseline {
            updated: u.into(),
            mods: m.into(),
        };
        assert!(base("1", "4").conflicts(&base("1", "5")));
        assert!(base("1", "").conflicts(&base("2", "5")));
        assert!(!base("1", "").conflicts(&base("1", "5")));
        assert!(!base("", "").conflicts(&base("1", "5")));
    }

    #[test]
    fn ui_meta_gives_each_field_its_editor() {
        let meta = Meta::from_ui(&json!({ "columns": {
            "short_description": {"label": "Short description", "type": "string", "mandatory": true, "max_length": 160},
            "description": {"label": "Description", "type": "string", "max_length": "4000"},
            "state": {"label": "State", "type": "integer", "choices": [{"value": 1, "label": "New"}, {"value": "6", "label": "Resolved"}]},
            "caller_id": {"label": "Caller", "type": "reference", "reference": "sys_user"},
            "watch_list": {"label": "Watch list", "type": "glide_list", "reference": "sys_user"},
            "sys_id": {"label": "Sys ID", "type": "GUID", "read_only": true},
            "text": {"label": "Body", "type": "html"},
        }}))
        .expect("meta");
        let kind = |name: &str| meta.field(name).expect(name).kind;
        assert_eq!(kind("short_description"), Kind::Text);
        assert_eq!(kind("description"), Kind::Long);
        assert_eq!(kind("state"), Kind::Choice);
        assert_eq!(kind("caller_id"), Kind::Reference);
        assert_eq!(kind("watch_list"), Kind::Fixed);
        assert!(meta.field("text").expect("text").locked().is_some());
        assert_eq!(new_record_fields(&meta)[0], "short_description");
        assert_eq!(
            closing_states(meta.field("state").expect("state")),
            [("6".to_owned(), "Resolved".to_owned())]
        );
    }

    #[test]
    fn the_dictionary_prefers_the_nearest_table() {
        let row = |v: Value| Row::new("x", &v);
        let tables = ["incident".to_owned(), "task".to_owned()];
        let dictionary = [
            row(
                json!({"name": "task", "element": "state", "column_label": "Task state", "internal_type": "integer"}),
            ),
            row(
                json!({"name": "incident", "element": "state", "column_label": "Incident state", "internal_type": "integer"}),
            ),
            row(json!({"name": "other", "element": "x", "internal_type": "string"})),
        ];
        let choices = [
            row(json!({"name": "task", "element": "state", "value": "3", "label": "Closed"})),
            row(
                json!({"name": "incident", "element": "state", "value": "7", "label": "Closed", "sequence": "2"}),
            ),
            row(
                json!({"name": "incident", "element": "state", "value": "1", "label": "New", "sequence": "1"}),
            ),
        ];
        let meta = Meta::from_dictionary(&tables, &dictionary, &choices);
        assert_eq!(meta.fields.len(), 1);
        assert_eq!(meta.fields[0].label, "Incident state");
        assert_eq!(meta.fields[0].choices[0].1, "New");
    }

    #[test]
    fn details_group_and_hide_empty_fields() {
        let row = Row::new(
            "incident",
            &json!({ "sys_id": "1", "short_description": "x", "close_notes": "", "comments": "c", "u_color": "red" }),
        );
        let shown = details(&row, None, false, &[]);
        assert_eq!(
            shown,
            [
                Detail::Group("Summary"),
                Detail::Field("short_description".into()),
                Detail::Group("More"),
                Detail::Field("u_color".into()),
                Detail::Group("System"),
                Detail::Field("sys_id".into()),
            ]
        );
        assert!(details(&row, None, true, &[]).contains(&Detail::Field("close_notes".into())));
        assert!(
            details(&row, None, false, &["close_notes".into()])
                .contains(&Detail::Field("close_notes".into()))
        );
    }
}

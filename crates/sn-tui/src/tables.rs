//! What the workspace knows about ServiceNow: its sections, the columns that
//! matter in each table, and how tables relate.

use crate::model::{Kind, Meta, Row};
use crate::query;
use crate::text;

/// Who is signed in.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Me {
    pub sys_id: String,
    pub name: String,
    /// `None` when the instance does not show the memberships.
    pub groups: Option<Vec<String>>,
}

/// How a section asks for its records.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Filter {
    Plain(&'static str),
    /// `{me}` is the signed-in user.
    Mine(&'static str),
    /// `{groups}` is the groups of the signed-in user.
    Groups(&'static str),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Section {
    pub id: &'static str,
    pub group: &'static str,
    pub label: &'static str,
    pub table: &'static str,
    pub filter: Filter,
    pub order: &'static [(&'static str, bool)],
    /// The letter after `g`.
    pub key: char,
    /// What an empty list means here.
    pub empty: &'static str,
}

const NEWEST: &[(&str, bool)] = &[("sys_updated_on", true)];
const URGENT: &[(&str, bool)] = &[("priority", false), ("sys_updated_on", true)];

pub const SECTIONS: &[Section] = &[
    Section {
        id: "mine",
        group: "My work",
        label: "Assigned to me",
        table: "task",
        filter: Filter::Mine("active=true^assigned_to={me}"),
        order: URGENT,
        key: 'm',
        empty: "You have no open work. Press g u for work that your groups have not taken yet.",
    },
    Section {
        id: "unassigned",
        group: "My work",
        label: "Unassigned",
        table: "task",
        filter: Filter::Groups("active=true^assigned_toISEMPTY^assignment_group{groups}"),
        order: URGENT,
        key: 'u',
        empty: "Nothing waits in your groups: every open task has an owner.",
    },
    Section {
        id: "approvals",
        group: "My work",
        label: "Approvals",
        table: "sysapproval_approver",
        filter: Filter::Mine("state=requested^approver={me}"),
        order: &[("sys_created_on", false)],
        key: 'a',
        empty: "No approval waits for you.",
    },
    Section {
        id: "requests",
        group: "My work",
        label: "My requests",
        table: "sc_request",
        filter: Filter::Mine("active=true^requested_for={me}"),
        order: NEWEST,
        key: 'r',
        empty: "You have no open request. Press g o to see what you can order.",
    },
    Section {
        id: "incidents",
        group: "Service desk",
        label: "Incidents",
        table: "incident",
        filter: Filter::Plain("active=true"),
        order: URGENT,
        key: 'i',
        empty: "No open incident.",
    },
    Section {
        id: "problems",
        group: "Service desk",
        label: "Problems",
        table: "problem",
        filter: Filter::Plain("active=true"),
        order: URGENT,
        key: 'p',
        empty: "No open problem.",
    },
    Section {
        id: "changes",
        group: "Service desk",
        label: "Changes",
        table: "change_request",
        filter: Filter::Plain("active=true"),
        order: &[("start_date", false)],
        key: 'c',
        empty: "No open change.",
    },
    Section {
        id: "tasks",
        group: "Service desk",
        label: "Catalog tasks",
        table: "sc_task",
        filter: Filter::Plain("active=true"),
        order: URGENT,
        key: 't',
        empty: "No open catalog task.",
    },
    Section {
        id: "knowledge",
        group: "Library",
        label: "Knowledge",
        table: "kb_knowledge",
        filter: Filter::Plain("workflow_state=published"),
        order: NEWEST,
        key: 'k',
        empty: "No published article that you can read.",
    },
    Section {
        id: "catalog",
        group: "Library",
        label: "Catalog items",
        table: "sc_cat_item",
        filter: Filter::Plain("active=true"),
        order: &[("name", false)],
        key: 'o',
        empty: "No catalog item that you can read.",
    },
    Section {
        id: "cmdb",
        group: "Library",
        label: "CMDB",
        table: "cmdb_ci",
        filter: Filter::Plain(""),
        order: &[("name", false)],
        key: 'b',
        empty: "No configuration item that you can read.",
    },
    Section {
        id: "update-sets",
        group: "Build",
        label: "Update sets",
        table: "sys_update_set",
        filter: Filter::Plain("state=in progress"),
        order: NEWEST,
        key: 's',
        empty: "No update set is in progress.",
    },
    Section {
        id: "script-includes",
        group: "Build",
        label: "Script includes",
        table: "sys_script_include",
        filter: Filter::Plain(""),
        order: NEWEST,
        key: 'j',
        empty: "No script include that you can read.",
    },
    Section {
        id: "business-rules",
        group: "Build",
        label: "Business rules",
        table: "sys_script",
        filter: Filter::Plain(""),
        order: NEWEST,
        key: 'l',
        empty: "No business rule that you can read.",
    },
    Section {
        id: "flows",
        group: "Build",
        label: "Flows",
        table: "sys_hub_flow",
        filter: Filter::Plain(""),
        order: NEWEST,
        key: 'f',
        empty: "No flow that you can read.",
    },
];

impl Section {
    pub fn find(id: &str) -> Option<&'static Section> {
        SECTIONS.iter().find(|s| s.id == id)
    }

    /// The query without its order; `None` until the user is known.
    pub fn query(&self, me: Option<&Me>) -> Option<String> {
        Some(match self.filter {
            Filter::Plain(text) => text.to_owned(),
            Filter::Mine(text) => text.replace("{me}", &me?.sys_id),
            Filter::Groups(text) => {
                let groups = match &me?.groups {
                    // `IN` without a value is dropped by the instance, which
                    // would show everyone's work.
                    Some(groups) if groups.is_empty() => "=no-group".to_owned(),
                    Some(groups) => format!("IN{}", groups.join(",")),
                    None => "=javascript:getMyGroups()".to_owned(),
                };
                text.replace("{groups}", &groups)
            }
        })
    }

    pub fn order(&self) -> Vec<(String, bool)> {
        self.order
            .iter()
            .map(|(f, d)| ((*f).to_owned(), *d))
            .collect()
    }
}

/// How the cells of a column look.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Look {
    Plain,
    /// The number or name that opens the record.
    Id,
    /// The sentence; takes the room that is left.
    Main,
    Priority,
    State,
    /// How long ago.
    Time,
    /// How long until, or since.
    Due,
    Class,
    Flag,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Column {
    pub field: String,
    pub label: String,
    pub look: Look,
    pub width: u16,
    /// Columns with a higher rank go first when there is no room.
    pub rank: u8,
}

type Spec = (&'static str, &'static str, Look, u16, u8);

const NUMBER: Spec = ("number", "Number", Look::Id, 10, 0);
/// Wide enough for SCTASK0010001.
const LONG_NUMBER: Spec = ("number", "Number", Look::Id, 13, 0);
const PRIORITY: Spec = ("priority", "Priority", Look::Priority, 12, 2);
const SUMMARY: Spec = ("short_description", "Short description", Look::Main, 18, 0);
const STATE: Spec = ("state", "State", Look::State, 15, 1);
const ASSIGNED: Spec = ("assigned_to", "Assigned to", Look::Plain, 16, 5);
const GROUP: Spec = ("assignment_group", "Group", Look::Plain, 16, 7);
const UPDATED: Spec = ("sys_updated_on", "Updated", Look::Time, 9, 3);
const NAME: Spec = ("name", "Name", Look::Id, 26, 0);
const ACTIVE: Spec = ("active", "Active", Look::Flag, 6, 2);

/// The columns of the tables the workspace knows, as the tables that people
/// read them in: the last one is the least important.
fn known(table: &str) -> Option<&'static [Spec]> {
    Some(match table {
        "task" => &[
            LONG_NUMBER,
            PRIORITY,
            SUMMARY,
            STATE,
            ("sys_class_name", "Type", Look::Class, 14, 4),
            ASSIGNED,
            GROUP,
            ("due_date", "Due", Look::Due, 9, 6),
            UPDATED,
        ],
        "incident" => &[
            NUMBER,
            PRIORITY,
            SUMMARY,
            STATE,
            ASSIGNED,
            GROUP,
            ("caller_id", "Caller", Look::Plain, 16, 8),
            UPDATED,
        ],
        "problem" => &[NUMBER, PRIORITY, SUMMARY, STATE, ASSIGNED, GROUP, UPDATED],
        "change_request" => &[
            NUMBER,
            ("type", "Type", Look::Plain, 10, 4),
            SUMMARY,
            STATE,
            ("risk", "Risk", Look::Plain, 9, 5),
            ASSIGNED,
            ("start_date", "Start", Look::Due, 9, 2),
            UPDATED,
        ],
        "change_task" | "problem_task" => &[LONG_NUMBER, SUMMARY, STATE, ASSIGNED, GROUP, UPDATED],
        "sc_task" => &[
            LONG_NUMBER,
            PRIORITY,
            SUMMARY,
            STATE,
            ("request_item", "Item", Look::Plain, 12, 4),
            ASSIGNED,
            GROUP,
            ("due_date", "Due", Look::Due, 9, 6),
            UPDATED,
        ],
        "sc_request" => &[
            NUMBER,
            SUMMARY,
            ("request_state", "State", Look::State, 16, 1),
            ("requested_for", "Requested for", Look::Plain, 16, 4),
            ("price", "Price", Look::Plain, 11, 5),
            ("opened_at", "Opened", Look::Time, 7, 2),
            UPDATED,
        ],
        "sc_req_item" => &[
            LONG_NUMBER,
            ("cat_item", "Item", Look::Main, 18, 0),
            STATE,
            ("stage", "Stage", Look::State, 18, 4),
            ("request", "Request", Look::Plain, 11, 5),
            ASSIGNED,
            UPDATED,
        ],
        "sysapproval_approver" => &[
            ("sysapproval", "Approve", Look::Id, 11, 0),
            ("sysapproval.short_description", "About", Look::Main, 18, 0),
            STATE,
            ("sysapproval.sys_class_name", "Type", Look::Class, 16, 3),
            ("sysapproval.opened_by", "Opened by", Look::Plain, 16, 5),
            ("sys_created_on", "Asked", Look::Time, 7, 2),
        ],
        "kb_knowledge" => &[
            NUMBER,
            SUMMARY,
            ("kb_knowledge_base", "Base", Look::Plain, 16, 4),
            ("workflow_state", "State", Look::State, 12, 2),
            ("author", "Author", Look::Plain, 16, 5),
            UPDATED,
        ],
        "sc_cat_item" => &[
            NAME,
            SUMMARY,
            ("category", "Category", Look::Plain, 16, 3),
            ("price", "Price", Look::Plain, 11, 4),
            UPDATED,
        ],
        "cmdb_ci" => &[
            NAME,
            ("sys_class_name", "Class", Look::Class, 18, 1),
            ("short_description", "Short description", Look::Main, 18, 5),
            ("operational_status", "Status", Look::State, 16, 2),
            ("ip_address", "IP address", Look::Plain, 15, 4),
            ("support_group", "Support group", Look::Plain, 16, 6),
            UPDATED,
        ],
        "sys_update_set" => &[
            ("name", "Name", Look::Main, 26, 0),
            ("application", "Scope", Look::Plain, 16, 3),
            STATE,
            ("sys_created_by", "Created by", Look::Plain, 14, 4),
            UPDATED,
        ],
        "sys_script_include" => &[
            ("name", "Name", Look::Id, 30, 0),
            ("api_name", "API name", Look::Main, 18, 3),
            ("client_callable", "Client", Look::Flag, 6, 5),
            ACTIVE,
            ("sys_updated_by", "By", Look::Plain, 14, 4),
            UPDATED,
        ],
        "sys_script" => &[
            ("name", "Name", Look::Main, 26, 0),
            ("collection", "Table", Look::Plain, 20, 1),
            ("when", "When", Look::Plain, 10, 3),
            ("order", "Order", Look::Plain, 6, 5),
            ACTIVE,
            ("sys_updated_by", "By", Look::Plain, 14, 4),
            UPDATED,
        ],
        "sys_hub_flow" => &[
            ("name", "Name", Look::Main, 26, 0),
            ("type", "Type", Look::Plain, 10, 3),
            ("status", "Status", Look::State, 14, 1),
            ("sys_scope", "Scope", Look::Plain, 16, 5),
            ACTIVE,
            ("sys_updated_by", "By", Look::Plain, 14, 4),
            UPDATED,
        ],
        "sys_user" => &[
            ("name", "Name", Look::Main, 22, 0),
            ("user_name", "User name", Look::Plain, 18, 1),
            ("email", "E-mail", Look::Plain, 28, 3),
            ("title", "Title", Look::Plain, 20, 5),
            ACTIVE,
            UPDATED,
        ],
        "sys_user_group" => &[
            ("name", "Name", Look::Id, 28, 0),
            ("description", "Description", Look::Main, 18, 2),
            ("manager", "Manager", Look::Plain, 18, 3),
            ACTIVE,
            UPDATED,
        ],
        t if t.starts_with("cmdb_ci_") => return known("cmdb_ci"),
        _ => return None,
    })
}

pub fn columns(table: &str) -> Option<Vec<Column>> {
    Some(
        known(table)?
            .iter()
            .map(|(field, label, look, width, rank)| Column {
                field: (*field).to_owned(),
                label: (*label).to_owned(),
                look: *look,
                width: *width,
                rank: *rank,
            })
            .collect(),
    )
}

/// Columns for a table the workspace does not know: what names the record,
/// what says its status, and what changed last.
pub fn guess_columns<'a>(names: impl Iterator<Item = &'a str>, meta: Option<&Meta>) -> Vec<Column> {
    let names: Vec<&str> = names.collect();
    let labels = |field: &str| match meta.and_then(|m| m.field(field)) {
        Some(f) => f.label.clone(),
        None => text::humanize(field),
    };
    let mut out: Vec<Column> = Vec::new();
    let mut add = |field: &str, look: Look, width: u16| {
        if names.contains(&field) && !out.iter().any(|c| c.field == field) {
            let rank = out.len() as u8;
            out.push(Column {
                field: field.to_owned(),
                label: labels(field),
                look,
                width,
                rank,
            });
        }
    };
    for field in ["number", "name"] {
        add(field, Look::Id, if field == "number" { 12 } else { 26 });
    }
    for field in [
        "short_description",
        "title",
        "label",
        "description",
        "subject",
    ] {
        add(field, Look::Main, 20);
    }
    for field in [
        "state",
        "status",
        "stage",
        "workflow_state",
        "install_status",
    ] {
        add(field, Look::State, 14);
    }
    add("priority", Look::Priority, 12);
    add("active", Look::Flag, 6);
    add("sys_class_name", Look::Class, 16);
    for field in [
        "assigned_to",
        "user",
        "owner",
        "category",
        "type",
        "table",
        "element",
    ] {
        add(field, Look::Plain, 16);
    }
    if let Some(meta) = meta {
        let mut more = meta
            .fields
            .iter()
            .filter(|f| {
                !f.name.starts_with("sys_")
                    && f.mandatory
                    && matches!(f.kind, Kind::Text | Kind::Choice | Kind::Reference)
            })
            .take(3);
        while out.len() < 7 {
            let Some(f) = more.next() else { break };
            if names.contains(&f.name.as_str()) && !out.iter().any(|c| c.field == f.name) {
                let rank = out.len() as u8;
                out.push(Column {
                    field: f.name.clone(),
                    label: f.label.clone(),
                    look: Look::Plain,
                    width: 16,
                    rank,
                });
            }
        }
    }
    // A table without any of these: its own fields, shortest story first.
    if out.len() < 4 {
        let short = |name: &str| match meta.and_then(|m| m.field(name)) {
            Some(f) => matches!(
                f.kind,
                Kind::Text | Kind::Choice | Kind::Reference | Kind::Number | Kind::Date
            ),
            None => meta.is_none(),
        };
        for field in names
            .iter()
            .filter(|n| !n.starts_with("sys_") && !n.contains('.') && short(n))
        {
            if out.len() >= 5 {
                break;
            }
            if !out.iter().any(|c| c.field == *field) {
                let rank = out.len() as u8;
                out.push(Column {
                    field: (*field).to_owned(),
                    label: labels(field),
                    look: Look::Plain,
                    width: 16,
                    rank,
                });
            }
        }
    }
    out.truncate(8);
    if !out.iter().any(|c| c.look == Look::Main)
        && let Some(first) = out.first_mut()
    {
        first.look = Look::Main;
    }
    let (field, rank) = ("sys_updated_on", 2.min(out.len() as u8));
    if names.contains(&field) || out.is_empty() {
        out.push(Column {
            field: field.to_owned(),
            label: "Updated".into(),
            look: Look::Time,
            width: 9,
            rank,
        });
    }
    out
}

/// Fields that every list needs, besides its columns.
pub const ROW_FIELDS: &[&str] = &[
    "sys_id",
    "sys_class_name",
    "sys_updated_on",
    "sys_mod_count",
    "number",
    "name",
    "short_description",
];

pub fn list_fields(table: &str, columns: &[Column]) -> Vec<String> {
    let mut out: Vec<String> = columns.iter().map(|c| c.field.clone()).collect();
    let mut add = |field: &str| {
        if !out.iter().any(|f| f == field) {
            out.push(field.to_owned());
        }
    };
    for field in ROW_FIELDS {
        add(field);
    }
    if is_task(table) {
        for field in ["priority", "state", "assigned_to", "assignment_group"] {
            add(field);
        }
    }
    if table == "sysapproval_approver" {
        for field in ["sysapproval", "source_table", "document_id", "approver"] {
            add(field);
        }
    }
    out
}

/// Tables with an owner, comments and work notes.
pub fn is_task(table: &str) -> bool {
    matches!(
        table,
        "task"
            | "incident"
            | "incident_task"
            | "problem"
            | "problem_task"
            | "change_request"
            | "change_task"
            | "sc_request"
            | "sc_req_item"
            | "sc_task"
            | "sn_si_incident"
            | "sn_customerservice_case"
            | "sn_hr_core_case"
            | "rm_story"
            | "kb_submission"
            | "universal_request"
            | "interaction"
    )
}

const LABELS: &[(&str, &str, &str)] = &[
    ("task", "task", "Tasks"),
    ("incident", "incident", "Incidents"),
    ("incident_task", "incident task", "Incident tasks"),
    ("problem", "problem", "Problems"),
    ("problem_task", "problem task", "Problem tasks"),
    ("change_request", "change", "Changes"),
    ("change_task", "change task", "Change tasks"),
    ("sc_request", "request", "Requests"),
    ("sc_req_item", "requested item", "Requested items"),
    ("sc_task", "catalog task", "Catalog tasks"),
    ("sysapproval_approver", "approval", "Approvals"),
    ("kb_knowledge", "article", "Knowledge"),
    ("sc_cat_item", "catalog item", "Catalog items"),
    ("cmdb_ci", "configuration item", "Configuration items"),
    ("sys_update_set", "update set", "Update sets"),
    ("sys_update_xml", "customer update", "Customer updates"),
    ("sys_script_include", "script include", "Script includes"),
    ("sys_script", "business rule", "Business rules"),
    ("sys_hub_flow", "flow", "Flows"),
    ("sys_user", "user", "Users"),
    ("sys_user_group", "group", "Groups"),
    ("rm_story", "story", "Stories"),
    ("sn_si_incident", "security incident", "Security incidents"),
    ("sn_customerservice_case", "case", "Customer cases"),
    ("sn_hr_core_case", "HR case", "HR cases"),
    ("task_ci", "affected CI", "Affected CIs"),
];

/// Tables offered before the instance answers with more.
pub fn known_tables() -> impl Iterator<Item = (&'static str, &'static str)> {
    LABELS.iter().map(|(t, _, many)| (*t, *many))
}

/// One record of the table, in words.
pub fn one(table: &str) -> String {
    match LABELS.iter().find(|(t, _, _)| *t == table) {
        Some((_, one, _)) => (*one).to_owned(),
        None => "record".to_owned(),
    }
}

pub fn many(table: &str) -> String {
    match LABELS.iter().find(|(t, _, _)| *t == table) {
        Some((_, _, many)) => (*many).to_owned(),
        None => table.to_owned(),
    }
}

/// One list of the Related tab.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Related {
    pub title: &'static str,
    pub table: &'static str,
    /// The field that points to the record.
    pub field: &'static str,
    pub value: String,
    pub fields: &'static [&'static str],
    /// The field of a link row that holds the record to open.
    pub link: Option<(&'static str, &'static str)>,
}

const TASK_FIELDS: &[&str] = &[
    "sys_id",
    "sys_class_name",
    "number",
    "short_description",
    "state",
    "priority",
    "assigned_to",
    "sys_updated_on",
];
const APPROVAL_FIELDS: &[&str] = &[
    "sys_id",
    "approver",
    "state",
    "comments",
    "sysapproval",
    "sysapproval.short_description",
    "sys_updated_on",
    "sys_mod_count",
];

/// The related lists that fit a record.
pub fn related(row: &Row) -> Vec<Related> {
    let id = row.sys_id.clone();
    let list = |title, table, field, value: &str, fields, link| Related {
        title,
        table,
        field,
        value: value.to_owned(),
        fields,
        link,
    };
    let table = row.table.as_str();
    let mut out = Vec::new();
    if table == "sysapproval_approver" {
        let target = row.value("sysapproval");
        if !target.is_empty() {
            out.push(list(
                "To approve",
                "task",
                "sys_id",
                target,
                TASK_FIELDS,
                None,
            ));
            out.push(list(
                "Other approvals",
                "sysapproval_approver",
                "sysapproval",
                target,
                APPROVAL_FIELDS,
                None,
            ));
        }
        return out;
    }
    let task = is_task(table) || (!row.value("number").is_empty() && row.has("assignment_group"));
    if task {
        out.push(list(
            "Approvals",
            "sysapproval_approver",
            "sysapproval",
            &id,
            APPROVAL_FIELDS,
            None,
        ));
    }
    match table {
        "incident" => out.push(list(
            "Child incidents",
            "incident",
            "parent_incident",
            &id,
            TASK_FIELDS,
            None,
        )),
        "problem" => {
            out.push(list(
                "Problem tasks",
                "problem_task",
                "problem",
                &id,
                TASK_FIELDS,
                None,
            ));
            out.push(list(
                "Incidents",
                "incident",
                "problem_id",
                &id,
                TASK_FIELDS,
                None,
            ));
        }
        "change_request" => {
            out.push(list(
                "Change tasks",
                "change_task",
                "change_request",
                &id,
                TASK_FIELDS,
                None,
            ));
            out.push(list(
                "Incidents fixed by this change",
                "incident",
                "rfc",
                &id,
                TASK_FIELDS,
                None,
            ));
        }
        "sc_request" => out.push(list(
            "Items",
            "sc_req_item",
            "request",
            &id,
            TASK_FIELDS,
            None,
        )),
        "sc_req_item" => out.push(list(
            "Catalog tasks",
            "sc_task",
            "request_item",
            &id,
            TASK_FIELDS,
            None,
        )),
        "sys_update_set" => out.push(list(
            "Customer updates",
            "sys_update_xml",
            "update_set",
            &id,
            &[
                "sys_id",
                "name",
                "type",
                "target_name",
                "action",
                "sys_updated_on",
            ],
            None,
        )),
        "sys_user_group" => out.push(list(
            "Members",
            "sys_user_grmember",
            "group",
            &id,
            &["sys_id", "user", "user.email", "user.title"],
            Some(("sys_user", "user")),
        )),
        "sys_user" => out.push(list(
            "Open work",
            "task",
            "assigned_to",
            &id,
            TASK_FIELDS,
            None,
        )),
        _ => {}
    }
    if task {
        if !matches!(
            table,
            "change_request" | "problem" | "sc_request" | "sc_req_item"
        ) {
            out.push(list(
                "Child tasks",
                "task",
                "parent",
                &id,
                TASK_FIELDS,
                None,
            ));
        }
        out.push(list(
            "Affected CIs",
            "task_ci",
            "task",
            &id,
            &[
                "sys_id",
                "ci_item",
                "ci_item.sys_class_name",
                "ci_item.operational_status",
            ],
            Some(("cmdb_ci", "ci_item")),
        ));
    }
    if table.starts_with("cmdb_ci") {
        out.push(list(
            "Open work on this CI",
            "task",
            "cmdb_ci",
            &id,
            TASK_FIELDS,
            None,
        ));
        out.push(list(
            "Depends on",
            "cmdb_rel_ci",
            "parent",
            &id,
            &["sys_id", "child", "child.sys_class_name", "type"],
            Some(("cmdb_ci", "child")),
        ));
        out.push(list(
            "Used by",
            "cmdb_rel_ci",
            "child",
            &id,
            &["sys_id", "parent", "parent.sys_class_name", "type"],
            Some(("cmdb_ci", "parent")),
        ));
    }
    out
}

impl Related {
    pub fn query(&self) -> String {
        let open = match self.title {
            "Open work" | "Open work on this CI" => "active=true",
            _ => "",
        };
        sn_core::query::all([
            sn_core::query::term(self.field, "=", &self.value).as_str(),
            open,
            query::order_text(&[(
                if self.fields.contains(&"sys_updated_on") {
                    "sys_updated_on".to_owned()
                } else {
                    "sys_created_on".to_owned()
                },
                true,
            )])
            .as_str(),
        ])
    }

    /// The record a row stands for: itself, or what it links to.
    pub fn target(&self, row: &Row) -> Option<(String, String)> {
        match self.link {
            Some((table, field)) => {
                let id = row.value(field);
                let class = row.value(&format!("{field}.sys_class_name"));
                let table = match class.is_empty() {
                    true => table,
                    false => class,
                };
                sn_core::is_sys_id(id).then(|| (table.to_owned(), id.to_owned()))
            }
            None => Some((row.table.clone(), row.sys_id.clone())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn section_letters_are_unique() {
        for (i, a) in SECTIONS.iter().enumerate() {
            for b in &SECTIONS[i + 1..] {
                assert_ne!(a.key, b.key, "{} and {}", a.label, b.label);
                assert_ne!(a.id, b.id);
            }
        }
    }

    #[test]
    fn personal_sections_never_ask_for_everyone() {
        let section = Section::find("unassigned").expect("section");
        assert_eq!(section.query(None), None);
        let me = |groups| Me {
            sys_id: "u1".into(),
            groups,
            ..Me::default()
        };
        let query = |groups| section.query(Some(&me(groups))).expect("query");
        assert!(query(Some(vec!["a".into(), "b".into()])).ends_with("assignment_groupINa,b"));
        assert!(query(Some(vec![])).ends_with("assignment_group=no-group"));
        assert!(query(None).contains("getMyGroups"));
        assert_eq!(
            Section::find("mine").and_then(|s| s.query(Some(&me(None)))),
            Some("active=true^assigned_to=u1".into())
        );
    }

    #[test]
    fn unknown_tables_get_useful_columns() {
        let names = ["sys_id", "u_color", "name", "state", "sys_updated_on"];
        let columns = guess_columns(names.into_iter(), None);
        let fields: Vec<&str> = columns.iter().map(|c| c.field.as_str()).collect();
        assert_eq!(fields, ["name", "state", "u_color", "sys_updated_on"]);
        assert_eq!(columns[0].look, Look::Main);
        // Nothing known at all still gives a column.
        assert_eq!(guess_columns(std::iter::empty(), None).len(), 1);
    }

    #[test]
    fn related_rows_open_what_they_link_to() {
        let row = Row::new("change_request", &json!({"sys_id": "1", "number": "CHG1"}));
        let lists = related(&row);
        assert!(lists.iter().any(|l| l.table == "change_task"));
        let cis = lists.iter().find(|l| l.table == "task_ci").expect("cis");
        let id = "a".repeat(32);
        let link = Row::new(
            "task_ci",
            &json!({"sys_id": "x", "ci_item": {"value": id, "display_value": "db1"},
                "ci_item.sys_class_name": {"value": "cmdb_ci_server", "display_value": "Server"}}),
        );
        assert_eq!(cis.target(&link), Some(("cmdb_ci_server".into(), id)));
    }
}

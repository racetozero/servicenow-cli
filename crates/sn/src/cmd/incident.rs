//! Incidents. Users, groups and choice values can be given by name or label.

use super::args::{BodyArgs, DisplayArg, ListArgs, dropped, record_of, show, text_or_file};
use super::table::written;
use crate::browser;
use crate::ctx::Ctx;
use crate::output::{Format, col, columns, fields_text, flex};
use clap::{ArgGroup, Args, Subcommand};
use serde_json::{Map, Value, json};
use sn_core::refs::caller;
use sn_core::refs::{self, Person};
use sn_core::table::{Walk, raw, text};
use sn_core::{Choice, Client, DisplayValue, Error, Result, TableQuery, query};
use std::collections::BTreeMap;
use std::io::Write as _;
use std::time::Duration;

const TABLE: &str = "incident";
const DEFAULT_LIMIT: u64 = 50;
const LIST_FIELDS: &[&str] = &[
    "number",
    "short_description",
    "state",
    "priority",
    "assigned_to",
    "assignment_group",
    "sys_updated_on",
    "sys_id",
];
/// What `edit` offers.
const EDITABLE: &[&str] = &[
    "short_description",
    "description",
    "state",
    "impact",
    "urgency",
    "category",
    "subcategory",
    "assigned_to",
    "assignment_group",
    "caller_id",
    "close_code",
    "close_notes",
];
/// Fields whose labels are accepted, with the values in `sys_choice`.
const CHOICES: &[&str] = &[
    "state",
    "impact",
    "urgency",
    "category",
    "subcategory",
    "close_code",
];
const RESOLVED: &str = "6";
const CLOSED: &str = "7";
const CANCELED: &str = "8";

#[derive(Subcommand, Debug)]
pub enum IncidentCommand {
    /// List incidents.
    #[command(visible_alias = "ls")]
    List {
        #[command(flatten)]
        list: ListArgs,
        /// Only active incidents.
        #[arg(long)]
        active: bool,
        /// Read all pages.
        #[arg(long)]
        all: bool,
    },
    /// List the active incidents assigned to you.
    Mine {
        #[command(flatten)]
        list: ListArgs,
        /// Closed ones too.
        #[arg(long)]
        inactive: bool,
        /// Read all pages.
        #[arg(long)]
        all: bool,
    },
    /// Read an incident.
    #[command(visible_alias = "get")]
    Show {
        /// Number (INC0010001), sys_id, or link.
        incident: String,
        /// Fields, comma-separated.
        #[arg(long, short = 'f', value_delimiter = ',')]
        fields: Vec<String>,
        #[command(flatten)]
        display: DisplayArg,
    },
    /// Create an incident.
    Create {
        #[command(flatten)]
        values: Values,
        #[command(flatten)]
        body: BodyArgs,
    },
    /// Change fields of an incident.
    Update {
        /// Number (INC0010001), sys_id, or link.
        incident: String,
        #[command(flatten)]
        values: Values,
        #[command(flatten)]
        body: BodyArgs,
    },
    /// Resolve an incident.
    Resolve(Resolve),
    /// Close a resolved incident.
    Close(Resolve),
    /// Put an incident back in progress.
    Reopen {
        /// Number (INC0010001), sys_id, or link.
        incident: String,
        /// Work note to add.
        #[arg(long, value_name = "TEXT")]
        note: Option<String>,
    },
    /// Edit an incident as YAML in your editor (VISUAL or EDITOR).
    Edit {
        /// Number (INC0010001), sys_id, or link.
        incident: String,
        /// Take the edited YAML from a file, or - for stdin, instead of an editor.
        #[arg(long, value_name = "FILE")]
        file: Option<String>,
    },
    /// Add a work note.
    Note {
        /// Number (INC0010001), sys_id, or link.
        incident: String,
        /// Text of the note.
        text: Option<String>,
        /// Read the text from a file, or - for stdin.
        #[arg(long, value_name = "FILE")]
        file: Option<String>,
        /// Add a comment, which the customer can see, instead of a work note.
        #[arg(long)]
        comment: bool,
    },
    /// Assign an incident.
    Assign {
        /// Number (INC0010001), sys_id, or link.
        incident: String,
        /// User name, e-mail address, full name, sys_id, or @me.
        #[arg(long = "to", value_name = "USER")]
        to: Option<String>,
        /// Group name or sys_id.
        #[arg(long)]
        group: Option<String>,
    },
    /// Open an incident in the browser.
    Open {
        /// Number (INC0010001), sys_id, or link.
        incident: String,
        /// Print the link instead.
        #[arg(long, visible_alias = "print-url")]
        print: bool,
    },
    /// Show field changes of an incident as they happen.
    Watch {
        /// Number (INC0010001), sys_id, or link.
        incident: String,
        /// Seconds between two looks.
        #[arg(long, default_value_t = 5, value_parser = clap::value_parser!(u64).range(1..))]
        interval: u64,
        /// Stop after this many looks.
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
        count: Option<u64>,
        /// Fields to watch, comma-separated.
        #[arg(long, short = 'f', value_delimiter = ',')]
        fields: Vec<String>,
    },
}

/// Fields by name; labels and names of people are resolved.
#[derive(Args, Debug, Default)]
pub struct Values {
    /// One line that says what is wrong.
    #[arg(long, short = 's', value_name = "TEXT")]
    short_description: Option<String>,
    /// Full description.
    #[arg(long, short = 'd', value_name = "TEXT")]
    description: Option<String>,
    /// Category, value or label.
    #[arg(long)]
    category: Option<String>,
    /// Subcategory, value or label.
    #[arg(long)]
    subcategory: Option<String>,
    /// Impact, value or label.
    #[arg(long)]
    impact: Option<String>,
    /// Urgency, value or label.
    #[arg(long)]
    urgency: Option<String>,
    /// State, value or label.
    #[arg(long)]
    state: Option<String>,
    /// User name, e-mail address, full name, sys_id, or @me.
    #[arg(long, value_name = "USER", visible_alias = "assigned-to")]
    assignee: Option<String>,
    /// Group name or sys_id.
    #[arg(long, value_name = "GROUP", visible_alias = "group")]
    assignment_group: Option<String>,
    /// Who reports it: a user as for --assignee.
    #[arg(long, value_name = "USER")]
    caller: Option<String>,
    /// Work note to add.
    #[arg(long, value_name = "TEXT", visible_alias = "note")]
    work_notes: Option<String>,
    /// Comment to add, which the customer can see.
    #[arg(long, value_name = "TEXT", visible_alias = "comments")]
    comment: Option<String>,
}

#[derive(Args, Debug)]
#[command(group(ArgGroup::new("text").args(["notes", "notes_file"])))]
pub struct Resolve {
    /// Number (INC0010001), sys_id, or link.
    incident: String,
    /// Resolution code, value or label.
    #[arg(long)]
    code: Option<String>,
    /// Resolution notes.
    #[arg(long, value_name = "TEXT")]
    notes: Option<String>,
    /// Read the notes from a file, or - for stdin.
    #[arg(long, value_name = "FILE")]
    notes_file: Option<String>,
    /// State to set, value or label (default: Resolved or Closed).
    #[arg(long)]
    state: Option<String>,
    #[command(flatten)]
    body: BodyArgs,
    /// Accepted for compatibility: choices are always read from the instance.
    #[arg(long, hide = true)]
    refresh: bool,
}

pub async fn run(ctx: &Ctx, command: IncidentCommand) -> Result<()> {
    match command {
        IncidentCommand::List { list, active, all } => {
            let filter = if active { "active=true" } else { "" };
            list_incidents(ctx, &list, filter, None, all).await
        }
        IncidentCommand::Mine {
            list,
            inactive,
            all,
        } => {
            let me = caller(ctx.client().await?).await?.sys_id;
            let own = query::term("assigned_to", "=", &me);
            let filter = match inactive {
                true => own,
                false => format!("active=true^{own}"),
            };
            list_incidents(ctx, &list, &filter, Some(&me), all).await
        }
        IncidentCommand::Show {
            incident,
            fields,
            display,
        } => {
            let params = TableQuery {
                fields,
                display: display.value(),
                ..TableQuery::default()
            };
            let client = ctx.client().await?;
            show(ctx, find(client, &incident, &params).await?)
        }
        IncidentCommand::Create { mut values, body } => {
            let extra = body.object()?;
            if values.short_description.is_none() && !extra.contains_key("short_description") {
                let text = ctx.need("short-description", "Short description", Vec::new())?;
                values.short_description = Some(text);
            }
            let client = ctx.client().await?;
            let body = values.body(client, extra).await?;
            let made = client.create(TABLE, body, &TableQuery::default()).await?;
            written(ctx, "Created", TABLE, made)
        }
        IncidentCommand::Update {
            incident,
            values,
            body,
        } => {
            let extra = body.object()?;
            let client = ctx.client().await?;
            let (id, body) = tokio::try_join!(
                record_of(client, TABLE, &incident),
                values.body(client, extra)
            )?;
            if body.is_empty() {
                return Err(Error::Input {
                    arg: "field".into(),
                    message: "there is nothing to change; pass a flag such as --state or --work-notes, or --field name=value".into(),
                    choices: Vec::new(),
                });
            }
            update(ctx, &id, body, "Updated").await
        }
        IncidentCommand::Resolve(args) => finish(ctx, args, "Resolved", RESOLVED).await,
        IncidentCommand::Close(args) => finish(ctx, args, "Closed", CLOSED).await,
        IncidentCommand::Reopen { incident, note } => {
            let client = ctx.client().await?;
            let (id, choices) = tokio::try_join!(
                record_of(client, TABLE, &incident),
                Choices::read(client, &["state"])
            )?;
            let mut body = Map::new();
            let state = choices.labeled("state", "In Progress").unwrap_or("2");
            body.insert("state".into(), state.into());
            if let Some(note) = note {
                body.insert("work_notes".into(), note.into());
            }
            update(ctx, &id, body, "Reopened").await
        }
        IncidentCommand::Edit { incident, file } => edit(ctx, &incident, file.as_deref()).await,
        IncidentCommand::Note {
            incident,
            text,
            file,
            comment,
        } => {
            let note = text_or_file(text.as_deref(), file.as_deref(), "the note")?;
            let id = record_of(ctx.client().await?, TABLE, &incident).await?;
            let field = if comment { "comments" } else { "work_notes" };
            let body = Map::from_iter([(field.to_owned(), note.into())]);
            update(ctx, &id, body, "Added a note to").await
        }
        IncidentCommand::Assign {
            incident,
            to,
            group,
        } => {
            if to.is_none() && group.is_none() {
                return Err(Error::Input {
                    arg: "to".into(),
                    message:
                        "the assignee is missing; pass --to USER (or @me), --group GROUP, or both"
                            .into(),
                    choices: Vec::new(),
                });
            }
            let values = Values {
                assignee: to,
                assignment_group: group,
                ..Values::default()
            };
            let client = ctx.client().await?;
            let (id, body) = tokio::try_join!(
                record_of(client, TABLE, &incident),
                values.body(client, Map::new())
            )?;
            update(ctx, &id, body, "Assigned").await
        }
        IncidentCommand::Open { incident, print } => {
            let client = ctx.client().await?;
            let id = record_of(client, TABLE, &incident).await?;
            let url = format!("{}/{TABLE}.do?sys_id={id}", client.base());
            let opened =
                !print && ctx.out.human() && tokio::task::block_in_place(|| browser::open(&url));
            let text = if opened {
                format!("Opened {url}")
            } else {
                url.clone()
            };
            let data = json!({ "incident": incident, "sys_id": id, "url": url, "opened": opened });
            ctx.out.done(data, &text)
        }
        IncidentCommand::Watch {
            incident,
            interval,
            count,
            fields,
        } => watch(ctx, &incident, interval, count, fields).await,
    }
}

async fn list_incidents(
    ctx: &Ctx,
    list: &ListArgs,
    filter: &str,
    assignee: Option<&str>,
    all: bool,
) -> Result<()> {
    let client = ctx.client().await?;
    let mut params = list.to_query(DEFAULT_LIMIT);
    params.query = query::bounded(&params.query, filter);
    if !all {
        params.query = query::ordered(&params.query, "ORDERBYDESCsys_updated_on");
    }
    if params.fields.is_empty() {
        params.fields = LIST_FIELDS.iter().map(|f| (*f).to_owned()).collect();
    }
    // The assignee's key in every display mode, to check the rows.
    const KEY: &str = "assigned_to.sys_id";
    if assignee.is_some() {
        params.fields.push(KEY.into());
    }
    let (mut rows, meta) = match all {
        false => {
            let page = client.list(TABLE, &params).await?;
            let meta = list.meta(page.rows.len(), page.total, DEFAULT_LIMIT);
            (page.rows, meta)
        }
        true => {
            params.limit = None;
            let mut walk = Walk::new(client, TABLE, params, 1000, false, None);
            let mut rows = Vec::new();
            while let Some(more) = walk.next().await? {
                rows.extend(more);
            }
            (rows, json!({ "complete": true }))
        }
    };
    if let Some(assignee) = assignee {
        for row in &mut rows {
            let key = row
                .as_object_mut()
                .and_then(|map| map.remove(KEY))
                .unwrap_or_default();
            let key = key.as_str().or_else(|| key["value"].as_str());
            if key.is_some_and(|key| !key.is_empty() && key != assignee) {
                return Err(dropped("assigned_to", TABLE));
            }
        }
    }
    if ctx.out.raw {
        return show(ctx, rows.into());
    }
    let cols = match list.fields.is_empty() {
        false => columns(&list.fields),
        true => vec![
            col("NUMBER", "number"),
            col("PRIORITY", "priority"),
            col("STATE", "state"),
            col("ASSIGNED TO", "assigned_to"),
            col("UPDATED", "sys_updated_on"),
            flex("SHORT DESCRIPTION", "short_description"),
        ],
    };
    ctx.out.list(rows, &cols, meta)
}

/// The incident with a number, sys_id or link, in one request for a number.
async fn find(client: &Client, incident: &str, params: &TableQuery) -> Result<Value> {
    if refs::prefix(incident).is_none() {
        let id = record_of(client, TABLE, incident).await?;
        return client.record(TABLE, &id, params).await;
    }
    let mut params = params.clone();
    params.query = query::term("number", "=", incident);
    params.limit = Some(2);
    for field in ["number", "sys_id"] {
        if !params.fields.is_empty() && !params.fields.iter().any(|f| f == field) {
            params.fields.push(field.into());
        }
    }
    let mut rows = client.list(TABLE, &params).await?.rows;
    if rows
        .iter()
        .any(|r| !raw(r, "number").eq_ignore_ascii_case(incident))
    {
        return Err(dropped("number", TABLE));
    }
    match rows.len() {
        1 => Ok(rows.remove(0)),
        0 => Err(Error::NotFound(format!(
            "there is no incident {incident} that you can read; check the number and your access"
        ))),
        _ => Err(Error::Other {
            code: "conflict",
            message: format!("several incidents have the number {incident}; pass the sys_id"),
            data: rows.iter().map(|r| r["sys_id"].clone()).collect(),
        }),
    }
}

async fn update(ctx: &Ctx, id: &str, body: Map<String, Value>, verb: &str) -> Result<()> {
    let client = ctx.client().await?;
    let changed = client
        .update(TABLE, id, body, &TableQuery::default())
        .await?;
    written(ctx, verb, TABLE, changed)
}

impl Values {
    /// The record body: `extra` with these fields over it.
    async fn body(self, client: &Client, extra: Map<String, Value>) -> Result<Map<String, Value>> {
        let mut body = extra;
        let mut given = Map::new();
        for (field, value) in [
            ("short_description", self.short_description),
            ("description", self.description),
            ("category", self.category),
            ("subcategory", self.subcategory),
            ("impact", self.impact),
            ("urgency", self.urgency),
            ("state", self.state),
            ("assigned_to", self.assignee),
            ("assignment_group", self.assignment_group),
            ("caller_id", self.caller),
            ("work_notes", self.work_notes),
            ("comments", self.comment),
        ] {
            if let Some(value) = value {
                given.insert(field.into(), value.into());
            }
        }
        resolve(client, &mut given).await?;
        body.extend(given);
        Ok(body)
    }
}

/// Replaces names of people and labels of choices with stored values.
async fn resolve(client: &Client, body: &mut Map<String, Value>) -> Result<()> {
    let given = |field: &str| {
        body.get(field)
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
    };
    let person = async |kind, value: Option<String>| match value {
        Some(value) => refs::person(client, kind, &value).await.map(Some),
        None => Ok(None),
    };
    // A number is a stored value already.
    let labels: Vec<(&str, String)> = CHOICES
        .iter()
        .filter_map(|field| Some((*field, given(field)?)))
        .filter(|(_, value)| value.parse::<i64>().is_err())
        .collect();
    let fields: Vec<&str> = labels.iter().map(|(field, _)| *field).collect();
    let (assignee, group, caller, choices) = tokio::try_join!(
        person(Person::User, given("assigned_to")),
        person(Person::Group, given("assignment_group")),
        person(Person::User, given("caller_id")),
        Choices::read(client, &fields),
    )?;
    for (field, value) in [
        ("assigned_to", assignee),
        ("assignment_group", group),
        ("caller_id", caller),
    ] {
        if let Some(value) = value {
            body.insert(field.into(), value.into());
        }
    }
    for (field, value) in labels {
        body.insert(field.into(), choices.value(field, &value)?.into());
    }
    Ok(())
}

/// Choice values of incident fields.
struct Choices(Vec<Value>);

impl Choices {
    async fn read(client: &Client, fields: &[&str]) -> Result<Self> {
        if fields.is_empty() {
            return Ok(Self(Vec::new()));
        }
        // An incident has its own choices, or those of its parent, task.
        let filter = format!(
            "nameIN{TABLE},task^elementIN{}^inactive=false^ORDERBYsequence",
            fields.join(",")
        );
        let params = TableQuery::new(filter)
            .fields(&["name", "element", "value", "label"])
            .limit(1000)
            .display(DisplayValue::False);
        let rows = match client.list("sys_choice", &params).await {
            Ok(page) => page.rows,
            // Without them, values are passed on as they are.
            Err(Error::Api(failure)) if failure.status == Some(403) => Vec::new(),
            Err(err) => return Err(err),
        };
        let asked = |row: &Value| {
            [TABLE, "task"].contains(&raw(row, "name")) && fields.contains(&raw(row, "element"))
        };
        match rows.iter().all(asked) {
            true => Ok(Self(rows)),
            false => Err(dropped("name and element", "sys_choice")),
        }
    }

    /// Value and label of each choice of a field.
    fn of(&self, field: &str) -> Vec<(&str, &str)> {
        let own = |table: &str| {
            self.0
                .iter()
                .filter(|r| raw(r, "element") == field && raw(r, "name") == table)
                .map(|r| (raw(r, "value"), raw(r, "label")))
                .collect::<Vec<_>>()
        };
        match own(TABLE) {
            rows if rows.is_empty() => own("task"),
            rows => rows,
        }
    }

    /// The stored value of a label.
    fn labeled(&self, field: &str, label: &str) -> Option<&str> {
        self.of(field)
            .into_iter()
            .find(|(_, have)| have.eq_ignore_ascii_case(label))
            .map(|(value, _)| value)
    }

    /// What to store for a value or label; anything when no choices are known.
    fn value(&self, field: &str, given: &str) -> Result<String> {
        let given = given.trim();
        let choices = self.of(field);
        if choices.is_empty() || choices.iter().any(|(value, _)| *value == given) {
            return Ok(given.to_owned());
        }
        match self.labeled(field, given) {
            Some(value) => Ok(value.to_owned()),
            None => Err(Error::Input {
                arg: field.replace('_', "-"),
                message: format!("you passed `{given}` as {field}; pass one of its choices"),
                choices: self.choices(field),
            }),
        }
    }

    fn choices(&self, field: &str) -> Vec<Choice> {
        let mut out: Vec<Choice> = Vec::new();
        for (value, label) in self.of(field) {
            if !value.is_empty() && !out.iter().any(|c| c.value == value) {
                out.push(Choice::new(value, label));
            }
        }
        out
    }
}

/// Resolves or closes: `label` is the state to reach, `value` its usual value.
async fn finish(ctx: &Ctx, args: Resolve, label: &str, value: &str) -> Result<()> {
    let closing = value == CLOSED;
    let notes = match (&args.notes, &args.notes_file) {
        // The resolution of a resolved incident is kept.
        (None, None) if closing => None,
        (notes, file) => Some(text_or_file(
            notes.as_deref(),
            file.as_deref(),
            "the resolution notes (--notes or --notes-file)",
        )?),
    };
    let mut body = args.body.object()?;
    for reserved in ["state", "close_code", "close_notes"] {
        if body.contains_key(reserved) {
            return Err(Error::invalid(format!(
                "you passed --field {reserved}; pass --state, --code or --notes instead"
            )));
        }
    }
    let client = ctx.client().await?;
    let params = TableQuery::default()
        .fields(&["sys_id", "number", "state"])
        .display(DisplayValue::All);
    let (existing, choices) = tokio::try_join!(
        find(client, &args.incident, &params),
        Choices::read(client, &["state", "close_code"])
    )?;
    let target = match &args.state {
        Some(state) => choices.value("state", state)?,
        None => choices.labeled("state", label).unwrap_or(value).to_owned(),
    };
    let now = raw(&existing, "state");
    let shown = text(&existing, "state");
    let is = |label: &str| shown.eq_ignore_ascii_case(label);
    let done = now == target || is(label);
    let ended = now == CLOSED || now == CANCELED || is("closed") || is("canceled");
    if done || ended || is("cancelled") {
        return Err(Error::Other {
            code: "conflict",
            message: format!(
                "the incident {} is already {}; {}",
                args.incident,
                if shown.is_empty() { now } else { shown },
                if done {
                    "there is nothing to do"
                } else {
                    "an ended incident cannot be resolved or closed"
                }
            ),
            data: existing,
        });
    }
    let code = match args.code {
        Some(code) => Some(choices.value("close_code", &code)?),
        None if closing => None,
        None => Some(ctx.need("code", "Resolution code", choices.choices("close_code"))?),
    };
    let id = raw(&existing, "sys_id");
    if closing {
        ctx.confirm(&format!(
            "close the incident {}; a closed incident cannot be opened again",
            args.incident
        ))?;
    }
    body.insert("state".into(), target.into());
    for (field, value) in [("close_code", code), ("close_notes", notes)] {
        if let Some(value) = value {
            body.insert(field.into(), value.into());
        }
    }
    update(ctx, id, body, label).await
}

/// The fields to edit, as text.
type Document = BTreeMap<String, Option<String>>;

async fn edit(ctx: &Ctx, incident: &str, file: Option<&str>) -> Result<()> {
    let client = ctx.client().await?;
    let mut fields = EDITABLE.to_vec();
    fields.extend(["sys_id", "number"]);
    let params = TableQuery::default()
        .fields(&fields)
        .display(DisplayValue::All);
    let record = find(client, incident, &params).await?;
    let edited = match file {
        Some(_) => parse_document(&text_or_file(None, file, "the edited YAML")?)?,
        None if ctx.interactive() => edit_in_editor(&document(&record)?)?,
        None => {
            return Err(Error::Input {
                arg: "file".into(),
                message: "an editor needs a terminal; pass the edited YAML with --file FILE (- for stdin), or change fields with `sn incident update`".into(),
                choices: Vec::new(),
            });
        }
    };
    let mut body = Map::new();
    for (field, value) in edited {
        let value = value.unwrap_or_default();
        if raw(&record, &field) != value {
            body.insert(field, value.into());
        }
    }
    resolve(client, &mut body).await?;
    // A name can stand for the value that is there already.
    body.retain(|field, value| raw(&record, field) != *value);
    if body.is_empty() {
        let data = json!({ "incident": incident, "changed": false });
        return ctx.out.done(data, "Nothing changed");
    }
    let mut diff = String::new();
    for (field, value) in &body {
        diff.push_str(&format!("{field}\n"));
        let after = value.as_str().unwrap_or_default();
        for (mark, side) in [('-', raw(&record, field)), ('+', after)] {
            for line in side.lines() {
                diff.push_str(&format!("  {mark} {line}\n"));
            }
        }
    }
    ctx.out.note(diff.trim_end());
    ctx.confirm(&format!("change {} fields of {incident}", body.len()))?;
    update(ctx, raw(&record, "sys_id"), body, "Updated").await
}

/// The editable fields as YAML, with labels as comments.
fn document(record: &Value) -> Result<String> {
    let mut out = String::from(
        "# Edit the values and save. Only changed fields are written.\n# Names of users and groups, and labels of choices, are accepted.\n",
    );
    let mut values: Document = Document::new();
    for field in EDITABLE {
        let value = raw(record, field);
        let label = text(record, field);
        if label != value && !label.is_empty() {
            out.push_str(&format!("# {field}: {}\n", label.replace('\n', " ")));
        }
        values.insert((*field).to_owned(), Some(value.to_owned()));
    }
    let yaml = serde_yaml_ng::to_string(&values)
        .map_err(|err| Error::Internal(format!("cannot write YAML: {err}")))?;
    Ok(out + &yaml)
}

fn parse_document(text: &str) -> Result<Document> {
    let document: Document = serde_yaml_ng::from_str(text).map_err(|err| {
        Error::invalid(format!(
            "the edited text is not YAML with one value per field ({err}); write lines such as `state: \"2\"`"
        ))
    })?;
    match document.keys().find(|f| !EDITABLE.contains(&f.as_str())) {
        Some(field) => Err(Error::invalid(format!(
            "the edited text has the field `{field}`, which `edit` does not change; use {}, or `sn incident update --field {field}=...`",
            EDITABLE.join(", ")
        ))),
        None => Ok(document),
    }
}

fn edit_in_editor(document: &str) -> Result<Document> {
    let failed = |what: &str, err: std::io::Error| {
        Error::invalid(format!(
            "{what} ({err}); set EDITOR to an editor that you have, or pass --file"
        ))
    };
    let mut file = tempfile::Builder::new()
        .prefix("incident-")
        .suffix(".yaml")
        .tempfile()
        .map_err(|err| failed("cannot write a temporary file", err))?;
    file.write_all(document.as_bytes())
        .and_then(|()| file.flush())
        .map_err(|err| failed("cannot write a temporary file", err))?;
    let editor = ["VISUAL", "EDITOR"]
        .iter()
        .find_map(|name| std::env::var(name).ok().filter(|v| !v.trim().is_empty()))
        .unwrap_or_else(|| if cfg!(windows) { "notepad" } else { "vi" }.to_owned());
    let words = super::profile::split_words(&editor)?;
    let Some((program, args)) = words.split_first() else {
        return Err(Error::invalid(
            "EDITOR is empty; set it to an editor that you have, or pass --file",
        ));
    };
    let path = file.into_temp_path();
    let status = tokio::task::block_in_place(|| {
        std::process::Command::new(program)
            .args(args)
            .arg(&path)
            .status()
    })
    .map_err(|err| failed(&format!("cannot start the editor `{editor}`"), err))?;
    if !status.success() {
        return Err(Error::invalid(format!(
            "the editor `{editor}` ended with {status}; nothing was changed"
        )));
    }
    let text =
        std::fs::read_to_string(&path).map_err(|err| failed("cannot read the edited file", err))?;
    if text.trim().is_empty() {
        return Err(Error::invalid(
            "the edited file is empty; nothing was changed",
        ));
    }
    parse_document(&text).map_err(|err| match path.keep() {
        // The work in the editor must not be lost.
        Ok(kept) => Error::invalid(format!(
            "{err}; your text is in {}, so fix it and pass it with --file",
            kept.display()
        )),
        Err(_) => err,
    })
}

async fn watch(
    ctx: &Ctx,
    incident: &str,
    interval: u64,
    count: Option<u64>,
    fields: Vec<String>,
) -> Result<()> {
    if matches!(ctx.out.format, Format::Csv | Format::Tsv) {
        return Err(Error::invalid(
            "you passed --output csv or tsv, but a stream has no columns; use json, jsonl or table",
        ));
    }
    let params = TableQuery {
        fields: match fields.is_empty() {
            true => LIST_FIELDS.iter().map(|f| (*f).to_owned()).collect(),
            false => fields,
        },
        display: DisplayValue::All,
        ..TableQuery::default()
    };
    let client = ctx.client().await?;
    let mut before = find(client, incident, &params).await?;
    let id = raw(&before, "sys_id").to_owned();
    if ctx.out.wants_text() {
        ctx.out.text(&format!("{}\n", fields_text(&before)))?;
    } else if !ctx.out.event(json!({
        "event": "snapshot", "incident": incident, "poll": 0, "record": before,
    }))? {
        return Ok(());
    }
    let mut poll = 0;
    while count.is_none_or(|count| poll < count) {
        tokio::time::sleep(Duration::from_secs(interval)).await;
        poll += 1;
        let now = client.record(TABLE, &id, &params).await?;
        let changes = changes(&before, &now);
        before = now;
        if changes.is_empty() {
            continue;
        }
        if !ctx.out.wants_text() {
            let read = ctx.out.event(json!({
                "event": "change", "incident": incident, "poll": poll, "changes": changes,
            }))?;
            if !read {
                return Ok(());
            }
            continue;
        }
        for change in &changes {
            ctx.out.text(&format!(
                "{incident}  {}: {} -> {}",
                text(change, "field"),
                text(change, "before"),
                text(change, "after")
            ))?;
        }
    }
    Ok(())
}

/// Fields whose stored value differs, shown by label.
fn changes(before: &Value, now: &Value) -> Vec<Value> {
    now.as_object()
        .into_iter()
        .flatten()
        .filter(|(field, _)| raw(before, field) != raw(now, field))
        .map(|(field, _)| {
            json!({ "field": field, "before": text(before, field), "after": text(now, field) })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record() -> Value {
        json!({
            "short_description": { "value": "Disk: full", "display_value": "Disk: full" },
            "description": { "value": "a\nb", "display_value": "a\nb" },
            "state": { "value": "2", "display_value": "In Progress" },
            "assigned_to": { "value": "", "display_value": "" },
        })
    }

    #[test]
    fn a_document_reads_back_as_it_was_written() {
        let text = document(&record()).expect("document");
        assert!(text.contains("# state: In Progress\n"), "{text}");
        let back = parse_document(&text).expect("parse");
        assert_eq!(back["state"].as_deref(), Some("2"));
        assert_eq!(back["description"].as_deref(), Some("a\nb"));
        assert_eq!(back["assigned_to"].as_deref(), Some(""));
    }

    #[test]
    fn edited_values_stay_as_written() {
        let back =
            parse_document("state: 2\nimpact: 01\nurgency:\nclose_notes: yes\n").expect("parse");
        assert_eq!(back["state"].as_deref(), Some("2"));
        assert_eq!(back["impact"].as_deref(), Some("01"));
        assert_eq!(back["urgency"].as_deref(), None);
        assert_eq!(back["close_notes"].as_deref(), Some("yes"));
        assert!(parse_document("sys_id: x\n").is_err());
        assert!(parse_document("state: [1]\n").is_err());
    }

    #[test]
    fn labels_become_values() {
        let row = |table: &str, value: &str, label: &str| json!({ "name": table, "element": "state", "value": value, "label": label });
        let choices = Choices(vec![
            row("task", "3", "Closed Complete"),
            row("incident", "6", "Resolved"),
            row("incident", "7", "Closed"),
        ]);
        assert_eq!(choices.value("state", "resolved").expect("label"), "6");
        assert_eq!(choices.value("state", "7").expect("value"), "7");
        assert!(choices.value("state", "Closed Complete").is_err());
        assert_eq!(choices.value("impact", "any").expect("unknown"), "any");
    }

    #[test]
    fn only_stored_values_count_as_changes() {
        let mut now = record();
        now["state"] = json!({ "value": "6", "display_value": "Resolved" });
        now["description"]["display_value"] = "other rendering".into();
        assert_eq!(
            changes(&record(), &now),
            [json!({ "field": "state", "before": "In Progress", "after": "Resolved" })]
        );
    }
}

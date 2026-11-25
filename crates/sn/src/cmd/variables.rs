//! Catalog variables of a record.
//!
//! The values of a requested item are in `sc_item_option`, joined through
//! `sc_item_option_mtom`; a record that a record producer made has them in
//! `question_answer`. Writes go through the one endpoint that is open to
//! users who may write the record. It skips unknown names and answers 200
//! whatever happened, so names are checked before and values after.

use super::args::{BodyArgs, RecordArgs, dropped};
use crate::ctx::Ctx;
use crate::output::{col, flex};
use clap::{Args, Subcommand};
use serde_json::{Map, Value, json};
use sn_core::refs::Record;
use sn_core::table::raw;
use sn_core::{Choice, Client, DisplayValue, Error, Result, TableQuery, query};
use std::collections::BTreeMap;

#[derive(Subcommand, Debug)]
pub enum VariablesCommand {
    /// Read the variables. A task (sc_task) shows those of its request item.
    Get {
        #[command(flatten)]
        record: RecordArgs,
    },
    /// Write variables and check that they were stored. Values are raw: a sys_id for a reference, true or false for a checkbox.
    Set(SetArgs),
}

#[derive(Args, Debug)]
pub struct SetArgs {
    #[command(flatten)]
    record: RecordArgs,
    #[command(flatten)]
    body: BodyArgs,
}

pub struct Variable {
    pub name: String,
    pub label: String,
    pub value: String,
}

impl Variable {
    pub fn to_json(&self) -> Value {
        match self.label.is_empty() {
            true => json!({ "name": self.name, "value": self.value }),
            false => json!({ "name": self.name, "label": self.label, "value": self.value }),
        }
    }
}

pub async fn run(ctx: &Ctx, command: VariablesCommand) -> Result<()> {
    match command {
        VariablesCommand::Get { record } => get(ctx, record).await,
        VariablesCommand::Set(args) => set(ctx, args).await,
    }
}

async fn get(ctx: &Ctx, record: RecordArgs) -> Result<()> {
    let client = ctx.client().await?;
    let record = record.record(client).await?;
    let owner = owner(client, &record).await?;
    let variables = pool(client, &owner).await?;
    if variables.is_empty() {
        // No variables, or no such record: a read tells the difference.
        let params = TableQuery::default().fields(&["sys_id"]);
        client.record(&owner.table, &owner.sys_id, &params).await?;
    }
    let cols = [
        col("NAME", "name"),
        col("LABEL", "label"),
        flex("VALUE", "value"),
    ];
    let rows = variables.iter().map(Variable::to_json).collect();
    ctx.out.list(rows, &cols, json!({}))
}

async fn set(ctx: &Ctx, args: SetArgs) -> Result<()> {
    let body = args.body.required()?;
    let client = ctx.client().await?;
    let record = args.record.record(client).await?;
    let owner = owner(client, &record).await?;
    let before = map(pool(client, &owner).await?);
    let unknown: Vec<&str> = body
        .keys()
        .filter(|name| !before.contains_key(*name))
        .map(String::as_str)
        .collect();
    if !unknown.is_empty() {
        return Err(Error::Input {
            arg: "field".into(),
            message: format!(
                "you passed {}, which {} {} does not have (names are case-sensitive); pass one of its variables",
                unknown.join(", "),
                owner.table,
                owner.sys_id
            ),
            choices: before.keys().map(|name| Choice::new(name, "")).collect(),
        });
    }
    let path = format!(
        "/api/sn_sc/servicecatalog/variables/{}/{}",
        owner.table, owner.sys_id
    );
    client.put(path, body.clone().into()).await?;
    let after = map(pool(client, &owner).await?);
    let mut updated = Map::new();
    let mut unchanged = Map::new();
    let mut failed = Map::new();
    for (name, value) in body {
        let wanted = match &value {
            Value::String(text) => text.clone(),
            Value::Null => String::new(),
            other => other.to_string(),
        };
        let was = before.get(&name).cloned().unwrap_or_default();
        let now = after.get(&name).cloned().unwrap_or_default();
        if now != wanted {
            failed.insert(name, json!({ "requested": wanted, "stored": now }));
        } else if was == now {
            unchanged.insert(name, now.into());
        } else {
            updated.insert(name, json!({ "from": was, "to": now }));
        }
    }
    if !failed.is_empty() {
        let names: Vec<&str> = failed.keys().map(String::as_str).collect();
        return Err(Error::api(
            format!(
                "the instance accepted the write but did not store {}; the variable is read-only, or the value is not in its raw form (a sys_id for a reference, true or false for a checkbox)",
                names.join(", ")
            ),
            failed.into(),
        ));
    }
    let text = format!(
        "Updated {} of {} variables of {} {}",
        updated.len(),
        updated.len() + unchanged.len(),
        owner.table,
        owner.sys_id
    );
    let mut out = json!({ "table": owner.table, "sys_id": owner.sys_id });
    if record != owner {
        out["resolved_from"] = json!({ "table": record.table, "sys_id": record.sys_id });
    }
    out["updated"] = updated.into();
    out["unchanged"] = unchanged.into();
    ctx.out.done(out, &text)
}

fn map(variables: Vec<Variable>) -> BTreeMap<String, String> {
    variables.into_iter().map(|v| (v.name, v.value)).collect()
}

/// The record that holds the variables: the request item of an `sc_task`,
/// the record itself otherwise.
pub async fn owner(client: &Client, record: &Record) -> Result<Record> {
    if record.table != "sc_task" {
        return Ok(record.clone());
    }
    let params = TableQuery::default()
        .fields(&["request_item"])
        .display(DisplayValue::False);
    let task = client.record("sc_task", &record.sys_id, &params).await?;
    match raw(&task, "request_item") {
        "" => Err(Error::NotFound(format!(
            "the task {} has no request item, so it has no variables; check the task",
            record.sys_id
        ))),
        id => Ok(Record {
            table: "sc_req_item".into(),
            sys_id: id.to_owned(),
        }),
    }
}

/// The variables of their owner, sorted by name.
pub async fn pool(client: &Client, owner: &Record) -> Result<Vec<Variable>> {
    let (table, filter, key, name, label, value) = match owner.table.as_str() {
        "sc_req_item" => (
            "sc_item_option_mtom",
            query::term("request_item", "=", &owner.sys_id),
            "request_item",
            "sc_item_option.item_option_new.name",
            "sc_item_option.item_option_new.question_text",
            "sc_item_option.value",
        ),
        _ => (
            "question_answer",
            query::all([
                query::term("table_name", "=", &owner.table).as_str(),
                &query::term("table_sys_id", "=", &owner.sys_id),
            ]),
            "table_sys_id",
            "question.name",
            "question.question_text",
            "value",
        ),
    };
    let params = TableQuery::new(filter)
        .fields(&[key, name, label, value])
        .limit(1000)
        .display(DisplayValue::False);
    let rows = client.list(table, &params).await?.rows;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        if !raw(row, key).is_empty() && raw(row, key) != owner.sys_id {
            return Err(dropped(key, table));
        }
        // Layout rows have no name.
        if raw(row, name).is_empty() {
            continue;
        }
        out.push(Variable {
            name: raw(row, name).to_owned(),
            label: raw(row, label).to_owned(),
            value: raw(row, value).to_owned(),
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

//! Tables, columns and choices. `schema incident` means
//! `schema columns incident` (`cli::implied_verb`).
//!
//! Reads go to the instance and leave a copy of the table in the cache;
//! `--offline` reads that copy.

use super::cache;
use crate::ctx::Ctx;
use crate::output::{Col, cell, col, flex};
use clap::{Args, Subcommand};
use serde_json::{Map, Value, json};
use sn_core::{Choice, Error, Result, query};
use std::path::PathBuf;

const TABLE_COLUMNS: &[Col] = &[flex("LABEL", "label"), col("TABLE", "value")];
const COLUMNS: &[Col] = &[
    col("NAME", "name"),
    flex("LABEL", "label"),
    col("TYPE", "type"),
    col("REFERENCE", "reference"),
    col("MANDATORY", "mandatory"),
    col("READ ONLY", "read_only"),
    col("CHOICES", "choices"),
];

#[derive(Subcommand, Debug)]
pub enum SchemaCommand {
    /// Tables of the instance.
    Tables {
        /// Text in the name or label.
        #[arg(long, value_name = "TEXT")]
        filter: Option<String>,
        /// Only tables that others refer to.
        #[arg(long)]
        reference_only: bool,
        /// Names from the offline copy.
        #[arg(long)]
        offline: bool,
    },
    /// Columns of a table (also `schema TABLE`).
    Columns(ColumnsArgs),
    /// Choice values of a column.
    Choices(ChoicesArgs),
}

#[derive(Args, Debug)]
pub struct ColumnsArgs {
    /// Table name.
    table: String,
    /// Text in the name or label.
    #[arg(long, value_name = "TEXT")]
    filter: Option<String>,
    /// Only this type, for example string or reference.
    #[arg(long = "type", value_name = "TYPE")]
    kind: Option<String>,
    /// Only mandatory columns.
    #[arg(long)]
    mandatory: bool,
    /// Only columns you can write.
    #[arg(long)]
    writable: bool,
    /// Only columns with choices.
    #[arg(long)]
    choices_only: bool,
    /// Only references.
    #[arg(long)]
    references_only: bool,
    #[command(flatten)]
    source: SourceArgs,
}

#[derive(Args, Debug)]
pub struct ChoicesArgs {
    /// Table name.
    table: String,
    /// Column name.
    field: String,
    #[command(flatten)]
    source: SourceArgs,
}

#[derive(Args, Debug)]
struct SourceArgs {
    /// Use the copy of the last read instead of the instance.
    #[arg(long)]
    offline: bool,
    /// Read from the instance (the default).
    #[arg(long, hide = true, conflicts_with = "offline")]
    refresh: bool,
}

pub async fn run(ctx: &Ctx, command: SchemaCommand) -> Result<()> {
    match command {
        SchemaCommand::Tables {
            filter,
            reference_only,
            offline,
        } => tables(ctx, filter.as_deref(), reference_only, offline).await,
        SchemaCommand::Columns(args) => columns(ctx, args).await,
        SchemaCommand::Choices(args) => choices(ctx, args).await,
    }
}

async fn tables(
    ctx: &Ctx,
    filter: Option<&str>,
    reference_only: bool,
    offline: bool,
) -> Result<()> {
    let rows = if offline {
        if reference_only {
            return Err(Error::invalid(
                "the offline copy does not know which tables others refer to; drop --offline or --reference-only",
            ));
        }
        let index = cache::required(ctx)?;
        index
            .table_names("")
            .map(|name| json!({ "label": null, "value": name }))
            .collect()
    } else {
        match ctx.client().await?.get("/api/now/doc/table/schema").await? {
            result if ctx.out.raw => return ctx.out.item(json!({ "result": result })),
            Value::Array(rows) => rows,
            // Never an empty list for an answer that has none.
            other => return ctx.out.any(other),
        }
    };
    let needle = filter.map(str::to_lowercase);
    let rows: Vec<Value> = rows
        .into_iter()
        .filter(|row| !reference_only || row["reference"] == true)
        .filter(|row| has_text(row, &["label", "value"], needle.as_deref()))
        .collect();
    ctx.out.list(rows, TABLE_COLUMNS, json!({}))
}

fn has_text(row: &Value, keys: &[&str], needle: Option<&str>) -> bool {
    needle.is_none_or(|needle| {
        keys.iter()
            .any(|key| cell(row, key).to_lowercase().contains(needle))
    })
}

fn copy_path(ctx: &Ctx, instance: &str, table: &str) -> PathBuf {
    cache::directory(&ctx.store, instance)
        .join("tables")
        .join(format!("{table}.json"))
}

/// `name -> column` of a table.
async fn meta(ctx: &Ctx, table: &str, offline: bool) -> Result<Map<String, Value>> {
    query::check_name("table", table)?;
    if offline {
        let instance = ctx.resolve()?.profile.instance;
        let copy = std::fs::read(copy_path(ctx, &instance, table)).ok();
        if let Some(map) = copy.and_then(|bytes| serde_json::from_slice(&bytes).ok()) {
            return Ok(map);
        }
        // Names are better than nothing.
        let names = cache::required_columns(ctx, table)?;
        return Ok(names.into_iter().map(|n| (n, json!({}))).collect());
    }
    let client = ctx.client().await?;
    let mut result = client.get(format!("/api/now/ui/meta/{table}")).await?;
    let Value::Object(columns) = result
        .get_mut("columns")
        .map(Value::take)
        .unwrap_or_default()
    else {
        return Err(Error::NotFound(format!(
            "the instance knows no columns of `{table}`; check the name with `sn schema tables --filter {table}`"
        )));
    };
    // The copy is a convenience: without it, only --offline has less.
    if let Err(err) = serde_json::to_vec(&columns)
        .map_err(Error::from)
        .and_then(|bytes| cache::write(&copy_path(ctx, client.base(), table), &bytes))
    {
        ctx.out.warn(&err.to_string());
    }
    Ok(columns)
}

async fn columns(ctx: &Ctx, args: ColumnsArgs) -> Result<()> {
    let columns = meta(ctx, &args.table, args.source.offline).await?;
    if ctx.out.raw {
        return ctx.out.item(json!({ "result": { "columns": columns } }));
    }
    let needle = args.filter.as_deref().map(str::to_lowercase);
    let has_choices = |column: &Value| column["choices"].as_array().is_some_and(|c| !c.is_empty());
    let mut rows: Vec<Value> = columns
        .into_iter()
        .map(|(name, mut column)| {
            if let Value::Object(map) = &mut column {
                map.insert("name".into(), name.into());
            }
            column
        })
        .filter(|column| !args.mandatory || column["mandatory"] == true)
        .filter(|column| !args.writable || column["read_only"] != true)
        .filter(|column| !args.choices_only || has_choices(column))
        .filter(|column| !args.references_only || column["type"] == "reference")
        .filter(|column| {
            args.kind
                .as_ref()
                .is_none_or(|kind| cell(column, "type").eq_ignore_ascii_case(kind))
        })
        .filter(|column| has_text(column, &["name", "label"], needle.as_deref()))
        .collect();
    if ctx.out.human() {
        for row in &mut rows {
            if let Some(count) = row["choices"].as_array().map(Vec::len) {
                row["choices"] = match count {
                    0 => Value::Null,
                    count => count.into(),
                };
            }
        }
    }
    ctx.out.list(rows, COLUMNS, json!({}))
}

pub async fn choices(ctx: &Ctx, args: ChoicesArgs) -> Result<()> {
    let columns = meta(ctx, &args.table, args.source.offline).await?;
    let Some(column) = columns.get(&args.field) else {
        let mut names: Vec<&String> = columns.keys().collect();
        names.sort();
        return Err(Error::Input {
            arg: "field".into(),
            message: format!(
                "`{}` has no column `{}`; `sn schema columns {}` shows them",
                args.table, args.field, args.table
            ),
            choices: names
                .into_iter()
                .map(|n| Choice::new(n, cell(&columns[n], "label")))
                .collect(),
        });
    };
    match &column["choices"] {
        Value::Array(rows) => ctx.out.list(rows.clone(), &[], json!({})),
        _ if args.source.offline && column.as_object().is_some_and(Map::is_empty) => {
            Err(Error::NotFound(format!(
                "there is no offline copy of the choices of `{}`; run this once without --offline",
                args.table
            )))
        }
        _ => {
            ctx.out
                .note(&format!("{}.{} has no choices", args.table, args.field));
            ctx.out.list(Vec::new(), &[], json!({}))
        }
    }
}

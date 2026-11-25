//! `get RECORD` reads a record with its catalog variables and journal;
//! `get NOUN` reads one piece of your context or of the instance. A noun is
//! never a reference: references have digits or a colon.

mod dev;
mod work;

use super::args::{DisplayArg, ReadArgs};
use super::{journal, variables};
use crate::ctx::Ctx;
use crate::output::{self, fields_text};
use clap::{Args, Subcommand};
use serde_json::{Value, json};
use sn_core::refs::{self, Record};
use sn_core::table::raw;
use sn_core::{Client, DisplayValue, Error, Result, TableQuery, query};

#[derive(Args, Debug)]
#[command(
    subcommand_negates_reqs = true,
    subcommand_value_name = "NOUN",
    override_usage = "sn get [OPTIONS] <RECORD>\n       sn get [OPTIONS] <TABLE> <SYS_ID|NUMBER>\n       sn get <NOUN> [OPTIONS]"
)]
pub struct GetArgs {
    #[command(subcommand)]
    noun: Option<Noun>,
    /// A record: a number (INC0010001), table:number, table:sys_id, a link, or a table and a number.
    #[arg(value_name = "RECORD", required = true)]
    reference: Option<String>,
    /// sys_id or number when the first argument is a table.
    #[arg(value_name = "SYS_ID|NUMBER")]
    id: Option<String>,
    #[command(flatten)]
    shape: Shape,
}

/// What the record part of a composite read has.
#[derive(Args, Debug, Default)]
pub struct Shape {
    /// Fields of the record, comma-separated.
    #[arg(long, short = 'f', value_delimiter = ',', alias = "sysparm-fields")]
    fields: Vec<String>,
    #[command(flatten)]
    display: DisplayArg,
    #[command(flatten)]
    extra: ReadArgs,
}

#[derive(Subcommand, Debug)]
pub enum Noun {
    /// Your current application scope.
    Scope,
    /// An update set (yours by default): state, scope, newest changes.
    Updateset(dev::UpdatesetArgs),
    /// A system property; end the name with * to list a prefix.
    Property(dev::PropertyArgs),
    /// The source of a script, by name.
    Script(dev::ScriptArgs),
    /// The choice values of a field: incident.state, or incident state.
    Choices {
        /// TABLE.FIELD, or TABLE.
        column: String,
        /// Field, when the first argument is a table.
        field: Option<String>,
    },
    /// Your user record, roles and groups.
    Me,
    /// Release, build and node of the instance.
    Instance,
    /// Open tasks assigned to you, and unassigned ones of your groups.
    Mine(work::MineArgs),
    /// The task you updated last.
    Last(work::LastArgs),
    /// Recent system log entries (administrators).
    Logs(work::LogsArgs),
}

const NOUNS: &str = "scope, updateset, property, script, choices, me, instance, mine, last, logs";

pub async fn run(ctx: &Ctx, args: GetArgs) -> Result<()> {
    if let Some(noun) = args.noun {
        if let Some(word) = args.reference {
            return Err(Error::invalid(format!(
                "you passed `{word}` before a noun; run `sn get RECORD` or `sn get NOUN`, with one of {NOUNS}"
            )));
        }
        return match noun {
            Noun::Scope => dev::scope(ctx).await,
            Noun::Updateset(args) => dev::update_set(ctx, args).await,
            Noun::Property(args) => dev::property(ctx, args).await,
            Noun::Script(args) => dev::script(ctx, args).await,
            Noun::Choices { column, field } => dev::choices(ctx, &column, field.as_deref()).await,
            Noun::Me => work::me(ctx).await,
            Noun::Instance => dev::instance(ctx).await,
            Noun::Mine(args) => work::mine(ctx, args).await,
            Noun::Last(args) => work::last(ctx, args).await,
            Noun::Logs(args) => work::logs(ctx, args).await,
        };
    }
    if ctx.out.raw {
        return Err(Error::invalid(
            "you passed --raw, but `get` answers with the parts of several responses; use `sn table get` for the response of one record",
        ));
    }
    let client = ctx.client().await?;
    let word = args.reference.as_deref().unwrap_or_default();
    let record = match &args.id {
        Some(id) => refs::record(client, word, Some(id)).await?,
        None => {
            let reference = refs::parse(client, word).map_err(|err| {
                match word.contains(':') || word.bytes().any(|b| b.is_ascii_digit()) {
                    true => err,
                    false => Error::invalid(format!(
                        "you passed `{word}`, which is neither a record nor a noun; pass a number (INC0010001), table:number, table:sys_id, a link, or one of {NOUNS}"
                    )),
                }
            })?;
            // The table of an unknown prefix is where the numbers come from;
            // the record can be in a table that extends it.
            let derived = reference.table.is_none();
            let record = refs::locate(client, reference).await?;
            match derived {
                true => concrete(client, record).await?,
                false => record,
            }
        }
    };
    composite(ctx, &record, &args.shape).await
}

/// The same record in its own table.
async fn concrete(client: &Client, mut record: Record) -> Result<Record> {
    let params = TableQuery::default()
        .fields(&["sys_class_name"])
        .display(DisplayValue::False);
    let row = client
        .record(&record.table, &record.sys_id, &params)
        .await?;
    let class = raw(&row, "sys_class_name");
    if query::check_name("table", class).is_ok() {
        record.table = class.to_owned();
    }
    Ok(record)
}

/// The record, its variables and its journal. A part that you may not read
/// is null and named in a warning; the record itself must be readable.
async fn composite(ctx: &Ctx, record: &Record, shape: &Shape) -> Result<()> {
    let client = ctx.client().await?;
    let mut params = TableQuery {
        fields: shape.fields.clone(),
        display: shape.display.value(),
        ..TableQuery::default()
    };
    shape.extra.fill(&mut params);
    let (body, variables, journal) = tokio::join!(
        client.record(&record.table, &record.sys_id, &params),
        async {
            let owner = variables::owner(client, record).await?;
            variables::pool(client, &owner).await
        },
        journal::entries(client, &record.table, &record.sys_id),
    );
    let body = body?;
    let mut warnings = Vec::new();
    let mut part = |name: &str, err: Error| {
        let text = format!("the {name} could not be read: {err}");
        ctx.out.warn(&text);
        warnings.push(Value::from(text));
        Value::Null
    };
    let variables = match variables {
        Ok(rows) => rows.iter().map(variables::Variable::to_json).collect(),
        Err(err) => part("variables", err),
    };
    let (journal, journal_text) = match journal {
        Ok(entries) => (serde_json::to_value(&entries)?, journal::text(&entries)),
        Err(err) => (part("journal", err), String::new()),
    };
    let meta = match warnings.is_empty() {
        true => json!({}),
        false => json!({ "warnings": warnings }),
    };
    let out = json!({
        "table": record.table, "sys_id": record.sys_id,
        "record": body, "variables": variables, "journal": journal,
    });
    ctx.out.item_with(out, meta, |data| {
        let mut text = fields_text(&data["record"]);
        let variables: serde_json::Map<String, Value> = data["variables"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| Some((v["name"].as_str()?.to_owned(), v["value"].clone())))
            .collect();
        let mut section = |title: &str, body: String| {
            if !body.is_empty() {
                let title = output::bold(title, ctx.out.tty);
                text.push_str(&format!("\n\n{title}\n{body}"));
            }
        };
        section("Variables", fields_text(&variables.into()));
        section("Journal", journal_text);
        text
    })
}

/// The display value of a field, or null.
pub(super) fn shown(row: &Value, field: &str) -> Value {
    match sn_core::table::text(row, field) {
        "" => Value::Null,
        text => text.into(),
    }
}

/// The raw value of a field, or null.
pub(super) fn stored(row: &Value, field: &str) -> Value {
    match raw(row, field) {
        "" => Value::Null,
        text => text.into(),
    }
}

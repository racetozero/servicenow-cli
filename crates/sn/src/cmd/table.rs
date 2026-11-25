//! Records of any table. `table incident` lists and `table incident SYS_ID`
//! reads: `cli::implied_verb` adds the verb.

use super::args::{BodyArgs, DisplayArg, ListArgs, ReadArgs, RecordArgs, show};
use crate::ctx::Ctx;
use crate::output::columns;
use clap::{Args, Subcommand, ValueEnum};
use serde_json::{Value, json};
use sn_core::refs::Record;
use sn_core::table::Walk;
use sn_core::{Error, Request, Result, TableQuery};

const DEFAULT_LIMIT: u64 = 100;
const WALK_PAGE: u64 = 1000;
const WALK_MAX: u64 = 100_000;

#[derive(Subcommand, Debug)]
pub enum TableCommand {
    /// List records.
    #[command(visible_alias = "ls")]
    List(Box<ListCommand>),
    /// Read one record.
    Get(GetCommand),
    /// Create a record.
    Create(CreateCommand),
    /// Change fields of a record; the other fields stay.
    Update(UpdateCommand),
    /// Replace a record (PUT).
    Replace(UpdateCommand),
    /// Delete a record.
    #[command(visible_alias = "rm")]
    Delete(DeleteCommand),
    /// How many records match.
    Count {
        /// Table name.
        table: String,
        /// Encoded query.
        #[arg(long, short = 'q')]
        query: Option<String>,
    },
}

#[derive(Args, Debug)]
pub struct ListCommand {
    /// Table name, for example incident or u_custom.
    pub table: String,
    #[command(flatten)]
    pub list: ListArgs,
    /// Read every match, page by page, one JSON record per line.
    #[arg(long)]
    pub all: bool,
    /// With --all: print one array at the end instead of lines.
    #[arg(long, requires = "all")]
    pub array: bool,
    /// With --all: stop after this many records (0: no limit; default 100000).
    #[arg(long, value_name = "N", requires = "all")]
    pub max_records: Option<u64>,
    /// With --all: how to page. Keyset never skips or repeats rows.
    #[arg(long, value_enum, default_value_t = Paginate::Keyset, requires = "all")]
    pub paginate: Paginate,
    /// With --all: continue after this sys_id (from a failed run's error).
    #[arg(long, value_name = "SYS_ID", requires = "all")]
    pub resume_from: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Paginate {
    Keyset,
    Offset,
}

#[derive(Args, Debug)]
pub struct GetCommand {
    #[command(flatten)]
    record: RecordArgs,
    /// Fields to read, comma-separated.
    #[arg(long, short = 'f', value_delimiter = ',')]
    fields: Vec<String>,
    #[command(flatten)]
    display: DisplayArg,
    #[command(flatten)]
    extra: ReadArgs,
}

/// Flags of a write that shape its answer.
#[derive(Args, Debug)]
pub struct WriteArgs {
    /// Fields to return, comma-separated.
    #[arg(long = "fields", short = 'f', value_delimiter = ',')]
    fields: Vec<String>,
    #[command(flatten)]
    display: DisplayArg,
    #[command(flatten)]
    extra: ReadArgs,
    /// Values are labels, not raw values.
    #[arg(long, alias = "sysparm-input-display-value")]
    input_display_value: bool,
    /// Do not set sys_created_*, sys_updated_* and similar fields.
    #[arg(long, alias = "sysparm-suppress-auto-sys-field")]
    suppress_auto_sys_field: bool,
}

impl WriteArgs {
    fn apply(&self, request: Request) -> Request {
        let mut query = TableQuery {
            fields: self.fields.clone(),
            display: self.display.value(),
            ..TableQuery::default()
        };
        self.extra.fill(&mut query);
        let request = request
            .query_opt(
                "sysparm_input_display_value",
                self.input_display_value.then_some("true"),
            )
            .query_opt(
                "sysparm_suppress_auto_sys_field",
                self.suppress_auto_sys_field.then_some("true"),
            );
        query.apply(request)
    }
}

#[derive(Args, Debug)]
pub struct CreateCommand {
    /// Table name.
    table: String,
    #[command(flatten)]
    body: BodyArgs,
    #[command(flatten)]
    write: WriteArgs,
}

#[derive(Args, Debug)]
pub struct UpdateCommand {
    #[command(flatten)]
    record: RecordArgs,
    #[command(flatten)]
    body: BodyArgs,
    #[command(flatten)]
    write: WriteArgs,
}

#[derive(Args, Debug)]
pub struct DeleteCommand {
    #[command(flatten)]
    record: RecordArgs,
    /// Find the record across domains.
    #[arg(long)]
    query_no_domain: bool,
}

pub async fn run(ctx: &Ctx, command: TableCommand) -> Result<()> {
    match command {
        TableCommand::List(args) => list(ctx, *args).await,
        TableCommand::Get(args) => get(ctx, args).await,
        TableCommand::Create(args) => create(ctx, args).await,
        TableCommand::Update(args) => update(ctx, args, sn_core::Method::PATCH).await,
        TableCommand::Replace(args) => update(ctx, args, sn_core::Method::PUT).await,
        TableCommand::Delete(args) => delete(ctx, args).await,
        TableCommand::Count { table, query } => {
            let count = ctx
                .client()
                .await?
                .count(&table, query.as_deref().unwrap_or(""))
                .await?;
            ctx.out
                .item_with(json!({ "table": table, "count": count }), json!({}), |_| {
                    count.to_string()
                })
        }
    }
}

async fn list(ctx: &Ctx, args: ListCommand) -> Result<()> {
    let client = ctx.client().await?;
    if args.all {
        return list_all(ctx, args).await;
    }
    let query = args.list.to_query(DEFAULT_LIMIT);
    sn_core::query::check_name("table", &args.table)?;
    let request = Request::new(sn_core::Method::GET, sn_core::table::path(&args.table));
    let response = client.send(query.apply(request)).await;
    if ctx.out.passes_through()
        && let Ok(response) = &response
        && let Some(rows) = verbatim(&response.body)
    {
        let meta = args.list.meta(rows.len(), response.total(), DEFAULT_LIMIT);
        return ctx.out.list_verbatim(&rows, meta);
    }
    let page = sn_core::Page::read(response)?;
    let meta = args.list.meta(page.rows.len(), page.total, DEFAULT_LIMIT);
    if ctx.out.raw && !ctx.out.agent {
        return ctx.out.item(json!({ "result": page.rows }));
    }
    ctx.out.list(page.rows, &columns(&args.list.fields), meta)
}

/// The records of `{"result": [...]}` without parsing them. `None` for any
/// other shape, and for text with line breaks, which would break the promise
/// of one JSON value per line.
fn verbatim(body: &[u8]) -> Option<Vec<&serde_json::value::RawValue>> {
    #[derive(serde::Deserialize)]
    struct Answer<'a> {
        #[serde(borrow)]
        result: Vec<&'a serde_json::value::RawValue>,
    }
    let text = std::str::from_utf8(body).ok()?;
    if text.bytes().any(|b| b == b'\n' || b == b'\r') {
        return None;
    }
    let answer: Answer<'_> = serde_json::from_str(text).ok()?;
    answer
        .result
        .iter()
        .all(|row| row.get().starts_with('{'))
        .then_some(answer.result)
}

async fn list_all(ctx: &Ctx, args: ListCommand) -> Result<()> {
    let client = ctx.client().await?;
    let page = args.list.limit.unwrap_or(WALK_PAGE);
    let mut query = args.list.to_query(page);
    query.offset = None;
    let max = match args.max_records {
        Some(0) => u64::MAX,
        Some(n) => n,
        None => WALK_MAX,
    };
    // Agents and tables need the whole list.
    let buffer = args.array || ctx.out.agent || (ctx.out.human() && ctx.out.jq.is_none());
    let mut walk = Walk::new(
        client,
        &args.table,
        query,
        page,
        args.paginate == Paginate::Offset,
        args.resume_from.clone(),
    );
    let mut rows: Vec<Value> = Vec::new();
    let mut count = 0_u64;
    let mut capped = false;
    loop {
        let next = match walk.next().await {
            Ok(Some(next)) => next,
            Ok(None) => break,
            Err(err) if count > 0 && !buffer => {
                let resume = walk.resume_key().map(str::to_owned);
                let hint = resume
                    .as_deref()
                    .map(|k| format!("; continue with --resume-from {k}"))
                    .unwrap_or_default();
                return Err(Error::Other {
                    code: err.code(),
                    message: format!("{err} (after {count} records{hint})"),
                    data: json!({ "resume_from": resume, "records": count }),
                });
            }
            Err(err) => return Err(err),
        };
        for row in next {
            if count >= max {
                capped = true;
                break;
            }
            count += 1;
            if buffer {
                rows.push(row);
            } else if !ctx.out.event(row)? {
                return Ok(());
            }
        }
        if capped {
            break;
        }
    }
    if capped {
        ctx.out.warn(&format!(
            "stopped after {max} records; raise --max-records (0: no limit)"
        ));
    }
    if buffer {
        return ctx.out.list(
            rows,
            &columns(&args.list.fields),
            json!({ "complete": !capped }),
        );
    }
    Ok(())
}

async fn get(ctx: &Ctx, args: GetCommand) -> Result<()> {
    let client = ctx.client().await?;
    let Record { table, sys_id } = args.record.record(client).await?;
    let mut query = TableQuery {
        fields: args.fields,
        display: args.display.value(),
        ..TableQuery::default()
    };
    args.extra.fill(&mut query);
    show(ctx, client.record(&table, &sys_id, &query).await?)
}

async fn create(ctx: &Ctx, args: CreateCommand) -> Result<()> {
    let client = ctx.client().await?;
    sn_core::query::check_name("table", &args.table)?;
    let request = sn_core::Request::new(sn_core::Method::POST, sn_core::table::path(&args.table))
        .json(args.body.required()?.into());
    let created = client.send(args.write.apply(request)).await?.result()?;
    written(ctx, "Created", &args.table, created)
}

async fn update(ctx: &Ctx, args: UpdateCommand, method: sn_core::Method) -> Result<()> {
    let client = ctx.client().await?;
    let body = args.body.required()?;
    let Record { table, sys_id } = args.record.record(client).await?;
    let request =
        sn_core::Request::new(method, format!("{}/{sys_id}", sn_core::table::path(&table)))
            .json(body.into());
    let updated = client.send(args.write.apply(request)).await?.result()?;
    written(ctx, "Updated", &table, updated)
}

/// The answer of a write: one line for people, the record for machines.
pub fn written(ctx: &Ctx, verb: &str, table: &str, record: Value) -> Result<()> {
    let name = ["number", "name", "sys_id"]
        .iter()
        .map(|f| sn_core::table::text(&record, f).to_owned())
        .find(|t| !t.is_empty())
        .unwrap_or_default();
    let id = sn_core::table::raw(&record, "sys_id").to_owned();
    let text = match id.is_empty() || id == name {
        true => format!("{verb} {table} {name}"),
        false => format!("{verb} {table} {name} ({id})"),
    };
    ctx.out.done(record, text.trim_end())
}

async fn delete(ctx: &Ctx, args: DeleteCommand) -> Result<()> {
    let client = ctx.client().await?;
    let Record { table, sys_id } = args.record.record(client).await?;
    let names = ["number", "name", "short_description"];
    let params = TableQuery {
        query_no_domain: args.query_no_domain,
        ..TableQuery::default().fields(&names)
    };
    let record = client.record(&table, &sys_id, &params).await?;
    let shown = names
        .iter()
        .map(|f| sn_core::table::text(&record, f))
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    ctx.confirm(format!("delete {table} {sys_id} {shown}").trim_end())?;
    client.remove(&table, &sys_id).await?;
    ctx.out.done(
        json!({ "table": table, "sys_id": sys_id, "deleted": true }),
        format!("Deleted {table} {shown} ({sys_id})").as_str(),
    )
}

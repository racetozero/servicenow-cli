//! Runs of flows, from the flow engine's documented tables.
//!
//! - `sys_flow_context`: one row per run; `flow` is a string with the flow's
//!   sys_id, `source_record` the record that triggered it.
//! - `sys_flow_report`: one row per step, written only when the run was
//!   recorded (property com.snc.process_flow.reporting.level, default OFF).
//! - `sys_flow_log`: the log; `level` is -1 (debug) to 2 (error).
//! - `sys_hub_flow.remote_trigger_id` names the trigger that is registered.

use super::{find, flow_id};
use crate::cmd::watch::{self, Limits, Plan};
use crate::ctx::Ctx;
use crate::output::{Col, cell, col, flex};
use clap::{Args, ValueEnum};
use futures_util::future::try_join_all;
use serde_json::{Map, Value, json};
use sn_core::refs::{self, Record};
use sn_core::table::raw;
use sn_core::{Client, DisplayValue, Error, Result, TableQuery, is_sys_id, now_unix, query};
use std::fmt::Write;

const RUN_FIELDS: &[&str] = &[
    "sys_id",
    "name",
    "flow",
    "state",
    "error_state",
    "error_message",
    "run_time",
    "calling_source",
    "source_table",
    "source_record",
    "is_test_run",
    "reporting",
    "sys_created_on",
    "sys_updated_on",
];
const RUN_COLUMNS: &[Col] = &[
    col("RUN", "sys_id"),
    col("FLOW", "name"),
    col("STATE", "state"),
    col("TABLE", "source_table"),
    col("STARTED", "sys_created_on"),
    flex("ERROR", "error_message"),
];
const STEP_FIELDS: &[&str] = &[
    "sys_id",
    "sys_class_name",
    "order",
    "state",
    "error",
    "start_time",
    "end_time",
    "run_time",
    "mid_time",
    "dependent_on",
    "dependents",
];
const STEP_COLUMNS: &[Col] = &[
    col("ORDER", "order"),
    col("STATE", "state"),
    col("KIND", "sys_class_name"),
    col("TIME", "run_time"),
    flex("ERROR", "error"),
];
const LOG_COLUMNS: &[Col] = &[
    col("ORDER", "order"),
    col("LEVEL", "level"),
    flex("MESSAGE", "message"),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum RunState {
    Waiting,
    Complete,
    Error,
    Cancelled,
    InProgress,
    Queued,
    ContinueSync,
    PresumedInterrupted,
    Paused,
    PausedInDebug,
}

impl RunState {
    const fn value(self) -> &'static str {
        match self {
            Self::Waiting => "WAITING",
            Self::Complete => "COMPLETE",
            Self::Error => "ERROR",
            Self::Cancelled => "CANCELLED",
            Self::InProgress => "IN_PROGRESS",
            Self::Queued => "QUEUED",
            Self::ContinueSync => "CONTINUE_SYNC",
            Self::PresumedInterrupted => "PRESUMED_INTERRUPTED",
            Self::Paused => "PAUSED",
            Self::PausedInDebug => "PAUSED_IN_DEBUG",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

#[derive(Args, Debug)]
pub struct RunsArgs {
    /// Flow: sys_id, internal name, scope.internal_name, or display name.
    #[arg(required_unless_present = "record")]
    pub flow: Option<String>,
    /// Only runs for this record: a number, table:number, link, or sys_id.
    #[arg(long, value_name = "RECORD")]
    pub record: Option<String>,
    /// Only runs that failed or caught an error.
    #[arg(long, conflicts_with = "state")]
    pub errors: bool,
    /// Only runs in these states (repeatable).
    #[arg(long, value_enum, value_delimiter = ',', value_name = "STATE")]
    pub state: Vec<RunState>,
    /// Only runs of the last 90m, 24h, 7d (s, m, h, d).
    #[arg(long, value_name = "TIME")]
    pub since: Option<String>,
    /// Most runs to return.
    #[arg(long, short = 'n', default_value_t = 20, value_name = "N", aliases = ["setlimit", "setLimit"])]
    pub limit: u64,
}

#[derive(Args, Debug)]
pub struct DebugArgs {
    /// Run: its sys_id from `flow runs`.
    #[arg(value_name = "RUN")]
    pub context: String,
    /// Newest log lines to include.
    #[arg(long, default_value_t = 20, value_name = "N")]
    pub log_lines: u64,
}

#[derive(Args, Debug)]
pub struct StepsArgs {
    /// Run: its sys_id from `flow runs`.
    #[arg(value_name = "RUN")]
    pub context: String,
    /// Only steps that failed.
    #[arg(long)]
    pub failed: bool,
    /// Also the input and output values (recorded at level FULL only).
    #[arg(long)]
    pub values: bool,
}

#[derive(Args, Debug)]
pub struct LogsArgs {
    /// Run: its sys_id from `flow runs`.
    #[arg(value_name = "RUN")]
    pub context: String,
    /// Lowest level to show.
    #[arg(long, value_enum)]
    pub level: Option<LogLevel>,
    /// Most lines to return.
    #[arg(long, short = 'n', default_value_t = 1000, value_name = "N", aliases = ["setlimit", "setLimit"])]
    pub limit: u64,
}

#[derive(Args, Debug)]
pub struct WhyNotArgs {
    /// Flow: sys_id, internal name, scope.internal_name, or display name.
    pub flow: String,
    /// The record the flow did not run for: a number, table:number, or link.
    #[arg(long, value_name = "RECORD")]
    pub record: String,
}

#[derive(Args, Debug)]
pub struct TailArgs {
    /// Flow: sys_id, internal name, scope.internal_name, or display name.
    pub flow: String,
    /// Only runs that fail.
    #[arg(long)]
    pub errors: bool,
    #[command(flatten)]
    pub limits: Limits,
}

async fn rows(
    client: &Client,
    table: &str,
    filter: &str,
    fields: &[&str],
    limit: u64,
) -> Result<Vec<Value>> {
    // Raw values: they fit the next command and stay in UTC.
    let mut params = TableQuery::new(filter)
        .fields(fields)
        .limit(limit)
        .display(DisplayValue::False);
    params.no_count = true;
    Ok(client.list(table, &params).await?.rows)
}

fn run_id(text: &str) -> Result<&str> {
    match is_sys_id(text) {
        true => Ok(text),
        false => Err(Error::invalid(format!(
            "you passed the run `{text}`; pass its sys_id, which `sn flow runs FLOW` shows"
        ))),
    }
}

async fn context(client: &Client, id: &str) -> Result<Value> {
    let found = rows(
        client,
        "sys_flow_context",
        &format!("sys_id={id}"),
        RUN_FIELDS,
        1,
    )
    .await?
    .pop();
    match found {
        Some(row) if raw(&row, "sys_id").eq_ignore_ascii_case(id) => Ok(row),
        Some(_) => Err(Error::api(
            format!("the instance ignored the filter for the run {id} and answered with another"),
            Value::Null,
        )),
        None => Err(Error::NotFound(format!(
            "there is no flow run {id} that you can read; runs need the role flow_operator or admin"
        ))),
    }
}

pub(super) async fn runs(ctx: &Ctx, args: RunsArgs) -> Result<()> {
    let since = args.since.as_deref().map(parse_since).transpose()?;
    let client = ctx.client().await?;
    let flow = async {
        match &args.flow {
            Some(flow) => flow_id(client, flow).await.map(Some),
            None => Ok(None),
        }
    };
    let record = async {
        match &args.record {
            Some(text) if is_sys_id(text) => Ok(Some(text.to_ascii_lowercase())),
            Some(text) => Ok(Some(refs::record(client, text, None).await?.sys_id)),
            None => Ok(None),
        }
    };
    let (flow, record) = tokio::try_join!(flow, record)?;
    let filter = runs_query(
        flow.as_deref(),
        record.as_deref(),
        args.errors,
        &args.state,
        since.map(|s| utc_timestamp(now_unix().saturating_sub(s))),
    );
    let found = rows(client, "sys_flow_context", &filter, RUN_FIELDS, args.limit).await?;
    let mut meta = json!({});
    if found.len() as u64 >= args.limit {
        meta["next"] = format!("raise --limit above {}", args.limit).into();
    }
    ctx.out.list(found, RUN_COLUMNS, meta)
}

fn runs_query(
    flow: Option<&str>,
    record: Option<&str>,
    errors: bool,
    states: &[RunState],
    since: Option<String>,
) -> String {
    let mut terms: Vec<String> = Vec::new();
    terms.extend(flow.map(|f| format!("flow={f}")));
    terms.extend(record.map(|r| format!("source_record={r}")));
    if errors {
        terms.push("state=ERROR^ORerror_stateISNOTEMPTY".into());
    }
    if !states.is_empty() {
        let values: Vec<&str> = states.iter().map(|s| s.value()).collect();
        terms.push(format!("stateIN{}", values.join(",")));
    }
    // An absolute time: the relative operators matched nothing when measured.
    terms.extend(since.map(|s| format!("sys_created_on>={s}")));
    terms.push("ORDERBYDESCsys_created_on".into());
    terms.join("^")
}

/// `90m` as seconds.
pub fn parse_since(text: &str) -> Result<u64> {
    let parsed = text.trim().char_indices().last().and_then(|(at, unit)| {
        let factor = match unit {
            's' => 1,
            'm' => 60,
            'h' => 3600,
            'd' => 86_400,
            _ => return None,
        };
        text.trim()[..at]
            .parse::<u64>()
            .ok()?
            .checked_mul(factor)
            .filter(|s| *s > 0)
    });
    parsed.ok_or_else(|| {
        Error::invalid(format!(
            "you passed --since {text}; pass a number and s, m, h or d, for example 90m or 24h"
        ))
    })
}

/// Seconds since 1970 as the UTC text that encoded queries compare.
pub fn utc_timestamp(seconds: u64) -> String {
    // Days to a date, after Howard Hinnant's civil_from_days.
    let z = (seconds / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    let rest = seconds % 86_400;
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

async fn step_rows(client: &Client, filter: &str, values: bool) -> Result<Vec<Value>> {
    let mut fields = STEP_FIELDS.to_vec();
    if values {
        fields.extend(["input", "output"]);
    }
    let mut found = rows(client, "sys_flow_report", filter, &fields, 10_000).await?;
    for row in found.iter_mut().filter(|_| values) {
        for key in ["input", "output"] {
            // JSON in a text field; the engine cuts long values.
            if let Some(Ok(parsed)) = row[key].as_str().map(serde_json::from_str::<Value>) {
                row[key] = parsed;
            }
        }
    }
    Ok(found)
}

/// Why a run has no steps. An empty list would read as "nothing ran".
fn no_steps(context: &Value) -> String {
    match raw(context, "reporting") {
        "OFF" | "" => "there is no step data: the run was recorded at reporting level OFF, the default; set the system property com.snc.process_flow.reporting.level to BASIC or FULL and run the flow again".into(),
        level => format!(
            "there is no step data: the run was recorded at level {level}, but this release keeps the steps where REST cannot read them; look at it with `sn open sys_flow_context {}`",
            raw(context, "sys_id")
        ),
    }
}

pub(super) async fn steps(ctx: &Ctx, args: StepsArgs) -> Result<()> {
    let id = run_id(&args.context)?;
    let client = ctx.client().await?;
    let failed = if args.failed { "^state=ERROR" } else { "" };
    let filter = format!("context={id}{failed}^ORDERBYorder");
    let (run, found) =
        tokio::try_join!(context(client, id), step_rows(client, &filter, args.values))?;
    if found.is_empty() && !args.failed {
        return Err(Error::Other {
            code: "not_found",
            message: no_steps(&run),
            data: run,
        });
    }
    ctx.out.list(found, STEP_COLUMNS, json!({}))
}

async fn log_rows(
    client: &Client,
    id: &str,
    level: Option<LogLevel>,
    newest: bool,
    limit: u64,
) -> Result<Vec<Value>> {
    let level = match level {
        None | Some(LogLevel::Debug) => "",
        Some(LogLevel::Info) => "levelIN0,1,2",
        Some(LogLevel::Warn) => "levelIN1,2",
        Some(LogLevel::Error) => "levelIN2",
    };
    let order = match newest {
        true => "ORDERBYDESCorder",
        false => "ORDERBYorder",
    };
    let filter = query::all([format!("context={id}").as_str(), level, order]);
    let fields = [
        "order",
        "level",
        "action",
        "operation",
        "message",
        "sys_created_on",
    ];
    let mut found = rows(client, "sys_flow_log", &filter, &fields, limit).await?;
    if newest {
        found.reverse();
    }
    for row in &mut found {
        let word = match raw(row, "level") {
            "-1" => "debug",
            "0" => "info",
            "1" => "warn",
            "2" => "error",
            _ => continue,
        };
        row["level"] = word.into();
    }
    Ok(found)
}

pub(super) async fn logs(ctx: &Ctx, args: LogsArgs) -> Result<()> {
    let id = run_id(&args.context)?;
    let client = ctx.client().await?;
    let found = log_rows(client, id, args.level, false, args.limit).await?;
    let mut meta = json!({});
    if found.len() as u64 >= args.limit {
        meta["next"] = format!("raise --limit above {}", args.limit).into();
    }
    ctx.out.list(found, LOG_COLUMNS, meta)
}

pub(super) async fn debug(ctx: &Ctx, args: DebugArgs) -> Result<()> {
    let id = run_id(&args.context)?;
    let client = ctx.client().await?;
    let filter = format!("context={id}^ORDERBYorder");
    let (run, steps, logs) = tokio::try_join!(
        context(client, id),
        step_rows(client, &filter, false),
        log_rows(client, id, None, true, args.log_lines),
    )?;
    // Values only of the failed step: they can carry record data.
    let failed = match steps.iter().find(|s| raw(s, "state") == "ERROR") {
        Some(step) => step_rows(client, &format!("sys_id={}", raw(step, "sys_id")), true)
            .await?
            .pop(),
        None => None,
    };
    ctx.out
        .item_with(document(run, &steps, failed, logs), json!({}), debug_text)
}

fn document(run: Value, steps: &[Value], failed: Option<Value>, logs: Vec<Value>) -> Value {
    let mut by_state: Map<String, Value> = Map::new();
    for step in steps {
        let count = by_state
            .entry(raw(step, "state").to_owned())
            .or_insert(json!(0));
        *count = (count.as_u64().unwrap_or(0) + 1).into();
    }
    let state = raw(&run, "state").to_owned();
    let mut notes = Vec::new();
    if steps.is_empty() {
        notes.push(no_steps(&run));
    } else if state == "ERROR" && failed.is_none() {
        notes.push(
            "the run failed but none of its steps did; the cause is in context.error_message"
                .into(),
        );
    }
    let errors = logs.iter().filter(|l| raw(l, "level") == "error").count();
    if errors > 0 && state != "ERROR" {
        notes.push(format!(
            "the run ended {state}, but its log has {errors} error line(s): a step logged a failure without failing the run"
        ));
    }
    json!({
        "context": run, "failed_step": failed,
        "steps": { "total": steps.len(), "by_state": by_state },
        "logs": logs, "notes": notes,
    })
}

fn debug_text(document: &Value) -> String {
    let mut out = crate::output::fields_text(&document["context"]);
    let _ = write!(out, "\n\nsteps: {}", document["steps"]["total"]);
    for (state, count) in document["steps"]["by_state"]
        .as_object()
        .into_iter()
        .flatten()
    {
        let _ = write!(out, ", {count} {state}");
    }
    if document["failed_step"].is_object() {
        let _ = write!(
            out,
            "\n\nfailed step\n{}",
            crate::output::fields_text(&document["failed_step"])
        );
    }
    if document["logs"].as_array().is_some_and(|l| !l.is_empty()) {
        out.push_str("\n\nlog\n");
    }
    for line in document["logs"].as_array().into_iter().flatten() {
        let _ = writeln!(out, "{:<5}  {}", cell(line, "level"), cell(line, "message"));
    }
    for note in document["notes"].as_array().into_iter().flatten() {
        let _ = write!(out, "\nnote: {}", note.as_str().unwrap_or_default());
    }
    out
}

/// A trigger condition as alternatives (`^NQ`) of AND clauses; an `^OR` term
/// belongs to the clause before it.
fn split_condition(condition: &str) -> Vec<Vec<String>> {
    query::alternatives(condition)
        .into_iter()
        .map(|terms| {
            let mut clauses: Vec<String> = Vec::new();
            for term in terms {
                if term.is_empty() || term == "EQ" || term.starts_with("ORDERBY") {
                    continue;
                }
                match clauses.last_mut() {
                    Some(last) if term.starts_with("OR") => {
                        last.push('^');
                        last.push_str(term);
                    }
                    _ => clauses.push(term.to_owned()),
                }
            }
            clauses
        })
        .filter(|clauses| !clauses.is_empty())
        .collect()
}

/// What speaks for and against a run.
#[derive(Default)]
struct Verdict {
    blockers: Vec<String>,
    notes: Vec<String>,
    condition: Value,
    trigger_table: Option<String>,
    class: Option<String>,
}

pub(super) async fn why_not(ctx: &Ctx, args: WhyNotArgs) -> Result<()> {
    let client = ctx.client().await?;
    let (flow, record) = tokio::try_join!(
        find(client, &args.flow),
        refs::record(client, &args.record, None)
    )?;
    let id = raw(&flow, "sys_id").to_owned();
    let mut verdict = Verdict::default();
    if raw(&flow, "active") != "true" {
        verdict.blockers.push("the flow is not active".into());
    }
    if raw(&flow, "status") != "published" {
        verdict.blockers.push(format!(
            "the flow is not published (status `{}`); only the trigger of a published flow runs",
            raw(&flow, "status")
        ));
    }
    let trigger_id = raw(&flow, "remote_trigger_id");
    let runs_filter = format!(
        "flow={id}^source_record={}^ORDERBYDESCsys_created_on",
        record.sys_id
    );
    let run_fields = ["sys_id", "state", "error_message", "sys_created_on"];
    let trigger = async {
        match is_sys_id(trigger_id) {
            true => trigger_row(client, trigger_id).await,
            false => Ok(Value::Null),
        }
    };
    let (runs, trigger) = tokio::try_join!(
        rows(client, "sys_flow_context", &runs_filter, &run_fields, 5),
        trigger
    )?;
    if !runs.is_empty() {
        verdict.notes.push(format!(
            "the flow has run for this record ({} recent runs are listed); look at one with `sn flow debug RUN`",
            runs.len()
        ));
    }
    if raw(&flow, "type") == "subflow" {
        verdict.notes.push(
            "this is a subflow: it has no trigger and runs only when a flow or a script calls it"
                .into(),
        );
    } else if trigger.is_null() {
        verdict.blockers.push("the flow has no registered trigger (sys_hub_flow.remote_trigger_id is empty); it was never activated with one".into());
    } else {
        if raw(&trigger, "active") != "true" {
            verdict.blockers.push("the trigger is not active".into());
        }
        match raw(&trigger, "sys_class_name") {
            "sys_flow_record_trigger" => {
                judge_trigger(client, &trigger, &record, &mut verdict).await?;
            }
            class => verdict.notes.push(format!(
                "the trigger is a {class}, not a record trigger, so no record starts it"
            )),
        }
    }
    let document = json!({
        "flow": flow,
        "trigger": trigger,
        "record": {
            "table": record.table, "sys_id": record.sys_id,
            "trigger_table": verdict.trigger_table, "sys_class_name": verdict.class,
        },
        "condition": verdict.condition,
        "runs": runs,
        "blockers": verdict.blockers,
        "notes": verdict.notes,
    });
    ctx.out.item_with(document, json!({}), why_not_text)
}

async fn trigger_row(client: &Client, id: &str) -> Result<Value> {
    let filter = format!("sys_id={id}");
    let fields = [
        "sys_id",
        "sys_class_name",
        "active",
        "table",
        "condition",
        "on_insert",
        "on_update",
        "on_delete",
        "run_on_extended",
        "run_when_setting",
        "run_when_user_setting",
    ];
    // The parent table has the class of any trigger; the child table has
    // the columns of a record trigger.
    let (mut base, mut record) = tokio::try_join!(
        rows(client, "sys_flow_trigger", &filter, &fields[..3], 1),
        rows(client, "sys_flow_record_trigger", &filter, &fields, 1)
    )?;
    let same = |row: &Value| raw(row, "sys_id") == id;
    record
        .pop()
        .filter(same)
        .or_else(|| base.pop().filter(same))
        .ok_or_else(|| {
            Error::NotFound(format!(
                "the trigger {id} of the flow is missing, or you cannot read it"
            ))
        })
}

/// The record trigger: its table, operations and condition.
async fn judge_trigger(
    client: &Client,
    trigger: &Value,
    record: &Record,
    verdict: &mut Verdict,
) -> Result<()> {
    let operations: Vec<&str> = ["insert", "update", "delete"]
        .into_iter()
        .filter(|o| raw(trigger, &format!("on_{o}")) == "true")
        .collect();
    verdict.notes.push(format!(
        "the trigger fires on {} only",
        match operations.is_empty() {
            true => "no operation".to_owned(),
            false => operations.join("/"),
        }
    ));
    for (field, open, what) in [
        ("run_when_setting", "both", "kind of session"),
        ("run_when_user_setting", "any", "user"),
    ] {
        let value = raw(trigger, field);
        if !value.is_empty() && value != open {
            verdict.notes.push(format!(
                "{field} is `{value}`: the {what} of the write decides too, which is not checked here"
            ));
        }
    }
    let table = raw(trigger, "table").to_owned();
    query::check_name("table", &table)?;
    let filter = format!("sys_id={}", record.sys_id);
    // Also finds records of child tables, and names their class.
    let found = rows(client, &table, &filter, &["sys_id", "sys_class_name"], 2).await?;
    verdict.class = found
        .first()
        .filter(|row| raw(row, "sys_id") == record.sys_id)
        .map(|row| raw(row, "sys_class_name").to_owned());
    verdict.trigger_table = Some(table.clone());
    let Some(class) = verdict.class.clone() else {
        verdict.blockers.push(format!(
            "the record is not in the trigger's table {table} or a table that extends it, or you cannot read it"
        ));
        return Ok(());
    };
    if !class.is_empty() && class != table && raw(trigger, "run_on_extended") != "true" {
        verdict.blockers.push(format!(
            "the record is a {class}, which extends {table}, and the trigger runs for {table} alone (run_on_extended is false)"
        ));
    }
    let condition = raw(trigger, "condition");
    let segments = split_condition(condition);
    if segments.is_empty() {
        verdict.condition = json!({ "condition": condition, "matches": true, "segments": [] });
        return Ok(());
    }
    let total = client.count(&table, "").await?;
    let checks = segments.iter().map(|clauses| {
        try_join_all(
            clauses
                .iter()
                .map(|clause| judge_clause(client, &table, &record.sys_id, clause, total)),
        )
    });
    // A clause that cannot be judged leaves its segment open.
    let mut overall = Some(false);
    let mut shown = Vec::new();
    for clauses in try_join_all(checks).await? {
        let verdicts: Vec<Option<bool>> = clauses.iter().map(|c| c["matches"].as_bool()).collect();
        let matches = if verdicts.contains(&Some(false)) {
            Some(false)
        } else if verdicts.contains(&None) {
            None
        } else {
            Some(true)
        };
        overall = match (overall, matches) {
            (Some(true), _) | (_, Some(true)) => Some(true),
            (None, _) | (_, None) => None,
            _ => Some(false),
        };
        shown.push(json!({ "matches": matches, "clauses": clauses }));
    }
    if overall == Some(false) {
        verdict.blockers.push("the record does not match the trigger condition now; condition.segments shows which clause fails".into());
    }
    verdict.notes.push("the condition was checked against the record as it is now; the trigger saw it as it was at the write".into());
    verdict.condition = json!({ "condition": condition, "matches": overall, "segments": shown });
    Ok(())
}

async fn judge_clause(
    client: &Client,
    table: &str,
    sys_id: &str,
    clause: &str,
    total: u64,
) -> Result<Value> {
    // These test the write, not the stored values; the API rejects them.
    if let Some(operator) = ["VALCHANGES", "CHANGESFROM", "CHANGESTO"]
        .into_iter()
        .find(|o| clause.contains(o))
    {
        return Ok(json!({
            "clause": clause, "matches": null,
            "note": format!("{operator} tests the write, not stored values, so it cannot be checked"),
        }));
    }
    let matches = client
        .count(table, &format!("{clause}^sys_id={sys_id}"))
        .await?
        > 0;
    let mut shown = json!({ "clause": clause, "matches": matches });
    if matches && total > 1 && client.count(table, clause).await? == total {
        shown["note"] = format!(
            "it matches every record of {table}: it is always true, or the instance ignored it"
        )
        .into();
    }
    Ok(shown)
}

fn why_not_text(document: &Value) -> String {
    let mut out = format!(
        "{} for {} {}\n",
        cell(&document["flow"], "name"),
        cell(&document["record"], "table"),
        cell(&document["record"], "sys_id")
    );
    let blockers = document["blockers"].as_array();
    if blockers.is_none_or(Vec::is_empty) {
        out.push_str("nothing found that stops the flow\n");
    }
    for blocker in blockers.into_iter().flatten() {
        let _ = writeln!(out, "stops it: {}", blocker.as_str().unwrap_or_default());
    }
    for clause in document["condition"]["segments"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|segment| segment["clauses"].as_array().into_iter().flatten())
    {
        let mark = match clause["matches"].as_bool() {
            Some(true) => "matches",
            Some(false) => "does not match",
            None => "cannot be checked",
        };
        let _ = writeln!(out, "condition: {} {mark}", cell(clause, "clause"));
    }
    for note in document["notes"].as_array().into_iter().flatten() {
        let _ = writeln!(out, "note: {}", note.as_str().unwrap_or_default());
    }
    out
}

/// A watcher on the runs. A row that starts to match the query arrives as an
/// update, so `state=ERROR` reports a run when it fails.
pub(super) async fn tail(ctx: &Ctx, args: TailArgs) -> Result<()> {
    let id = flow_id(ctx.client().await?, &args.flow).await?;
    let errors = if args.errors { "^state=ERROR" } else { "" };
    let filter = format!("flow={id}{errors}");
    watch::stream(ctx, Plan::new("sys_flow_context", &filter, args.limits)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_become_utc_text() {
        assert_eq!(parse_since("90m").expect("since"), 5400);
        assert_eq!(parse_since(" 7d ").expect("since"), 604_800);
        for bad in [
            "",
            "m",
            "0h",
            "-1h",
            "5w",
            "1.5h",
            "15",
            "99999999999999999999d",
            "é",
        ] {
            assert!(parse_since(bad).is_err(), "{bad}");
        }
        assert_eq!(utc_timestamp(0), "1970-01-01 00:00:00");
        assert_eq!(utc_timestamp(951_782_400), "2000-02-29 00:00:00");
        assert_eq!(utc_timestamp(1_704_067_199), "2023-12-31 23:59:59");
    }

    #[test]
    fn run_filters_keep_their_alternatives_together() {
        assert_eq!(
            runs_query(
                Some("f"),
                Some("r"),
                true,
                &[],
                Some("2024-01-01 00:00:00".into())
            ),
            "flow=f^source_record=r^state=ERROR^ORerror_stateISNOTEMPTY^sys_created_on>=2024-01-01 00:00:00^ORDERBYDESCsys_created_on"
        );
        assert_eq!(
            runs_query(
                None,
                Some("r"),
                false,
                &[RunState::InProgress, RunState::Queued],
                None
            ),
            "source_record=r^stateININ_PROGRESS,QUEUED^ORDERBYDESCsys_created_on"
        );
    }

    #[test]
    fn conditions_split_into_alternatives_and_clauses() {
        assert_eq!(
            split_condition("state=1^ORstate=2^priority=1^NQactive=true^EQ"),
            [vec!["state=1^ORstate=2", "priority=1"], vec!["active=true"]]
        );
        assert_eq!(split_condition("name=a^^b^EQ"), [["name=a^^b"]]);
        assert!(split_condition("^EQ").is_empty());
    }

    #[test]
    fn the_document_says_why_steps_are_missing_and_errors_hidden() {
        let run = json!({ "sys_id": "r", "state": "COMPLETE", "reporting": "OFF" });
        let logs = vec![json!({ "level": "error", "message": "boom" })];
        let document = document(run, &[], None, logs);
        let notes = document["notes"].as_array().expect("notes");
        assert!(
            notes[0]
                .as_str()
                .expect("note")
                .contains("reporting level OFF")
        );
        assert!(notes[1].as_str().expect("note").contains("1 error line(s)"));
    }
}

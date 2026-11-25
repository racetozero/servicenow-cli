//! Playbooks of a record, over the GraphQL schema of the Playbook
//! Experience application (`snPlaybookExp`).
//!
//! Expected failures arrive as data (`TriggerPlaybookError`) under HTTP 200.
//! `getPlaybooksForParentRecord` answers `null` for a record that is missing
//! or not readable, and `[]` for a record without playbooks.

use super::args::{RecordArgs, pairs};
use super::graphql;
use crate::ctx::Ctx;
use crate::output::{Col, col, flex};
use clap::{Args, Subcommand};
use serde_json::{Value, json};
use sn_core::{Error, Result, is_sys_id, percent};

/// The wire has the namespaced names of the union's members.
const STARTED: &str = "snPlaybookExp_playbook_PlaybookContext";
const FAILED: &str = "snPlaybookExp_playbook_TriggerPlaybookError";
const ANSWER: &str = "__typename \
    ... on snPlaybookExp_playbook_PlaybookContext { sys_id parent_table parent_record } \
    ... on snPlaybookExp_playbook_TriggerPlaybookError { errorType message parent_table parent_record process_definition_id input }";

const COLUMNS: &[Col] = &[
    flex("TITLE", "title"),
    col("STATE", "state"),
    col("PLAYBOOK", "scoped_name"),
    col("RUN", "sys_id"),
];

#[derive(Subcommand, Debug)]
pub enum PlaybookCommand {
    /// The playbooks of a record and their state.
    #[command(visible_alias = "ls")]
    List(RecordArgs),
    /// Start a playbook for a record, by its scoped name.
    Trigger(TriggerArgs),
    /// Start a playbook by its process definition.
    Launch(LaunchArgs),
}

#[derive(Args, Debug)]
pub struct TriggerArgs {
    #[command(flatten)]
    record: RecordArgs,
    /// Playbook, for example sn_vsc.task_steps_5. The instance does not check that it fits the record's table.
    #[arg(long, value_name = "SCOPE.NAME")]
    scoped_name: String,
    /// Start only when the record never had any playbook, in any state.
    #[arg(long)]
    only_if_none: bool,
}

#[derive(Args, Debug)]
pub struct LaunchArgs {
    /// sys_id of the process definition (`playbook_id` in `playbook list`).
    #[arg(value_name = "DEFINITION")]
    definition: String,
    /// sys_id of the parent record; the definition knows its table.
    #[arg(long, value_name = "SYS_ID")]
    record: Option<String>,
    /// Input name=value (repeatable).
    #[arg(long, value_name = "NAME=VALUE")]
    input: Vec<String>,
}

pub async fn run(ctx: &Ctx, command: PlaybookCommand) -> Result<()> {
    match command {
        PlaybookCommand::List(record) => list(ctx, record).await,
        PlaybookCommand::Trigger(args) => trigger(ctx, args).await,
        PlaybookCommand::Launch(args) => launch(ctx, args).await,
    }
}

async fn list(ctx: &Ctx, args: RecordArgs) -> Result<()> {
    let client = ctx.client().await?;
    let record = args.record(client).await?;
    let document = "query($table: String!, $record: ID!) { snPlaybookExp { playbook { \
        getPlaybooksForParentRecord(parentTable: $table, parentRecord: $record) { \
        sys_id title scoped_name playbook_id state { value displayValue } cancellation_reason can_read } } } }";
    let variables = json!({ "table": record.table, "record": record.sys_id });
    let answer = graphql::ask(client, document, variables).await?;
    match field(&answer, "getPlaybooksForParentRecord")? {
        Value::Null => Err(Error::NotFound(format!(
            "there is no {} record {} that you can read",
            record.table, record.sys_id
        ))),
        _ if ctx.out.raw => ctx.out.item(answer),
        Value::Array(mut rows) => {
            if ctx.out.human() {
                for row in &mut rows {
                    row["state"] = row["state"]["displayValue"].clone();
                }
            }
            ctx.out.list(rows, COLUMNS, json!({}))
        }
        other => Err(unexpected(other)),
    }
}

async fn trigger(ctx: &Ctx, args: TriggerArgs) -> Result<()> {
    if args.scoped_name.trim().is_empty() {
        return Err(Error::invalid(
            "--scoped-name is empty; pass SCOPE.NAME, for example sn_vsc.task_steps_5",
        ));
    }
    let client = ctx.client().await?;
    let record = args.record.record(client).await?;
    let document = format!(
        "mutation($scopedName: String!, $table: String!, $record: ID!, $onlyIfNone: Boolean) {{ \
         snPlaybookExp {{ playbook {{ triggerPlaybook(scopedName: $scopedName, parentTable: $table, \
         parentRecord: $record, onlyIfNoProcessRunning: $onlyIfNone) {{ {ANSWER} }} }} }} }}"
    );
    let variables = json!({
        "scopedName": args.scoped_name, "table": record.table,
        "record": record.sys_id, "onlyIfNone": args.only_if_none,
    });
    let answer = graphql::send(client, &document, variables, false).await?;
    let started = started(field(&answer, "triggerPlaybook")?)?;
    if ctx.out.raw {
        return ctx.out.item(answer);
    }
    // A skipped start (--only-if-none) is an answer without a sys_id.
    let text = match started["sys_id"].as_str() {
        Some(id) => format!("Started {} ({id})", args.scoped_name),
        None => "Not started: the record had a playbook before".to_owned(),
    };
    ctx.out.done(shown("triggered", &started), &text)
}

async fn launch(ctx: &Ctx, args: LaunchArgs) -> Result<()> {
    for (what, id) in [
        ("definition", Some(&args.definition)),
        ("--record", args.record.as_ref()),
    ] {
        if let Some(id) = id.filter(|id| !is_sys_id(id)) {
            return Err(Error::invalid(format!(
                "you passed the {what} {id}; pass a sys_id (32 hexadecimal digits)"
            )));
        }
    }
    let mut variables = json!({ "definition": args.definition });
    if let Some(record) = &args.record {
        variables["record"] = record.clone().into();
    }
    if !args.input.is_empty() {
        variables["input"] = encode_inputs(&pairs(&args.input, "--input")?).into();
    }
    let document = format!(
        "mutation($definition: String!, $record: ID, $input: String) {{ snPlaybookExp {{ playbook {{ \
         launchPlaybook(processDefinitionId: $definition, parentRecord: $record, input: $input) \
         {{ {ANSWER} }} }} }} }}"
    );
    let client = ctx.client().await?;
    let answer = graphql::send(client, &document, variables, false).await?;
    let started = started(field(&answer, "launchPlaybook")?)?;
    if started["sys_id"].is_null() {
        return Err(Error::api(
            "the instance reported a start but named no run; check the record's playbooks",
            started,
        ));
    }
    if ctx.out.raw {
        return ctx.out.item(answer);
    }
    let text = format!(
        "Started the playbook ({})",
        started["sys_id"].as_str().unwrap_or("")
    );
    ctx.out.done(shown("launched", &started), &text)
}

/// The value of one operation, after the GraphQL errors.
fn field(answer: &Value, operation: &str) -> Result<Value> {
    let errors = graphql::errors(answer);
    if graphql::undefined(answer, "snPlaybookExp") {
        return Err(Error::api(
            "this instance has no Playbook Experience API; install or activate the application sn_playbook_exp",
            errors.into(),
        ));
    }
    if let Some(first) = errors.first() {
        let thrown = errors.iter().any(|e| {
            e["errorType"] == "DataFetchingException"
                && e["path"].as_array().and_then(|p| p.last()) == Some(&json!(operation))
        });
        let hint = match operation {
            _ if !thrown => return Err(graphql::refusal(errors)),
            "launchPlaybook" => {
                "; a definition without a `parent_record` input fails this way for every record"
            }
            "getPlaybooksForParentRecord" => "; check the table name",
            _ => "",
        };
        let message = first["message"].as_str().unwrap_or("the resolver failed");
        return Err(Error::api(format!("{message}{hint}"), errors.into()));
    }
    answer
        .pointer(&format!("/data/snPlaybookExp/playbook/{operation}"))
        .cloned()
        .ok_or_else(|| unexpected(answer.clone()))
}

fn unexpected(value: Value) -> Error {
    Error::api(
        "the playbook API answered in a shape this command does not know",
        value,
    )
}

/// The started run, or the typed failure as an error.
fn started(value: Value) -> Result<Value> {
    let Value::Object(mut map) = value else {
        return Err(unexpected(value));
    };
    let kind = map.remove("__typename").unwrap_or_default();
    if kind == STARTED {
        return Ok(map.into());
    }
    if kind != FAILED {
        return Err(unexpected(map.into()));
    }
    let text = |name: &str| map.get(name).and_then(Value::as_str).unwrap_or_default();
    let message = format!(
        "the playbook was not started: {} ({})",
        text("message"),
        text("errorType")
    );
    map.retain(|_, v| !v.is_null());
    Err(Error::api(message, map.into()))
}

fn shown(key: &str, started: &Value) -> Value {
    json!({
        key: !started["sys_id"].is_null(), "sys_id": started["sys_id"],
        "parent_table": started["parent_table"], "parent_record": started["parent_record"],
    })
}

/// The resolver decodes the whole string, splits it at `&` and `=`, and
/// decodes each part again. So each part is encoded twice.
fn encode_inputs(inputs: &[(String, String)]) -> String {
    let twice = |text: &str| percent::encode(&percent::encode(text));
    inputs
        .iter()
        .map(|(name, value)| format!("{}={}", twice(name), twice(value)))
        .collect::<Vec<_>>()
        .join("&")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inputs_survive_two_decodings() {
        let encoded = encode_inputs(&[("k".into(), "a&b=c 100%é".into())]);
        let once = percent::decode(&encoded);
        let (name, value) = once.split_once('=').expect("pair");
        assert_eq!(
            (percent::decode(name), percent::decode(value)),
            ("k".into(), "a&b=c 100%é".into())
        );
    }

    #[test]
    fn typed_failures_become_errors_with_their_type() {
        let failure = json!({ "__typename": FAILED, "errorType": "INVALID_RECORD", "message": "no record", "input": null });
        let err = started(failure).expect_err("failure");
        assert!(err.to_string().contains("no record (INVALID_RECORD)"));
        assert_eq!(
            err.to_json()["payload"],
            json!({ "errorType": "INVALID_RECORD", "message": "no record" })
        );
        let skipped =
            started(json!({ "__typename": STARTED, "sys_id": null })).expect("a skip is no error");
        assert_eq!(shown("triggered", &skipped)["triggered"], false);
    }
}

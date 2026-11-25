//! Approvals (`sysapproval_approver`): those that wait for you, and your
//! answer.

use super::args::{ListArgs, dropped};
use super::get::{shown, stored};
use super::lookup::one;
use crate::ctx::Ctx;
use crate::output::{cell, col, flex};
use clap::{Args, Subcommand};
use serde_json::{Map, Value, json};
use sn_core::refs;
use sn_core::refs::caller;
use sn_core::table::raw;
use sn_core::{Client, DisplayValue, Error, Result, TableQuery, is_sys_id, query};

const TABLE: &str = "sysapproval_approver";
const DEFAULT_LIMIT: u64 = 50;
const FIELDS: &[&str] = &[
    "sys_id",
    "state",
    "approver",
    "sysapproval",
    "sysapproval.short_description",
    "document_id",
    "source_table",
    "due_date",
    "comments",
    "sys_created_on",
];

#[derive(Subcommand, Debug)]
pub enum ApprovalCommand {
    /// List the approvals that wait for you.
    #[command(visible_aliases = ["ls", "mine"])]
    List {
        #[command(flatten)]
        list: ListArgs,
        /// Approvals in this state: requested, approved, rejected, or any.
        #[arg(long, default_value = "requested")]
        state: String,
        /// Approvals of everyone, not only yours.
        #[arg(long)]
        everyone: bool,
    },
    /// Read an approval.
    #[command(visible_alias = "get")]
    Show {
        /// sys_id of the approval, or the record that it is for (CHG0000001).
        approval: String,
    },
    /// Approve.
    Approve(AnswerArgs),
    /// Reject; a comment is required.
    Reject(AnswerArgs),
}

#[derive(Args, Debug)]
pub struct AnswerArgs {
    /// sys_id of the approval, or the record that it is for (CHG0000001).
    approval: String,
    /// Why, for the person who asked.
    #[arg(
        long,
        short = 'm',
        visible_alias = "comments",
        visible_alias = "reason",
        value_name = "TEXT"
    )]
    comment: Option<String>,
}

pub async fn run(ctx: &Ctx, command: ApprovalCommand) -> Result<()> {
    match command {
        ApprovalCommand::List {
            list,
            state,
            everyone,
        } => list_approvals(ctx, list, &state, everyone).await,
        ApprovalCommand::Show { approval } => {
            let client = ctx.client().await?;
            let row = find(client, &approval, false).await?;
            ctx.out.item(summary(&row))
        }
        ApprovalCommand::Approve(args) => answer(ctx, args, "approved").await,
        ApprovalCommand::Reject(mut args) => {
            if args.comment.as_deref().is_none_or(|c| c.trim().is_empty()) {
                args.comment = Some(ctx.need("comment", "Why do you reject it?", Vec::new())?);
            }
            answer(ctx, args, "rejected").await
        }
    }
}

async fn list_approvals(ctx: &Ctx, list: ListArgs, state: &str, everyone: bool) -> Result<()> {
    let client = ctx.client().await?;
    let me = match everyone {
        true => None,
        false => Some(caller(client).await?),
    };
    let state_term = match state {
        "any" | "all" => String::new(),
        state => query::term("state", "=", state),
    };
    let approver = me
        .as_ref()
        .map(|me| query::term("approver", "=", &me.sys_id))
        .unwrap_or_default();
    let own = list.query.clone().unwrap_or_default();
    let filter = query::bounded(&own, &query::all([approver.as_str(), &state_term]));
    let mut params = list.to_query(DEFAULT_LIMIT);
    params.query = query::ordered(&filter, "ORDERBYDESCsys_created_on");
    // Both forms, to check the rows and to show them.
    let custom = !list.fields.is_empty();
    if !custom {
        params.fields = FIELDS.iter().map(|f| (*f).to_owned()).collect();
        params.display = DisplayValue::All;
    }
    let page = client.list(TABLE, &params).await?;
    let meta = list.meta(page.rows.len(), page.total, DEFAULT_LIMIT);
    if custom {
        let cols = crate::output::columns(&list.fields);
        return ctx.out.list(page.rows, &cols, meta);
    }
    for row in &page.rows {
        let other = me
            .as_ref()
            .is_some_and(|me| raw(row, "approver") != me.sys_id);
        if other || (!state_term.is_empty() && raw(row, "state") != state) {
            return Err(dropped("approver and state", TABLE));
        }
    }
    let rows = page.rows.iter().map(summary).collect();
    let mut cols = vec![
        col("RECORD", "record"),
        col("TABLE", "table"),
        col("STATE", "state"),
        col("REQUESTED", "requested_on"),
        flex("SHORT DESCRIPTION", "short_description"),
        col("SYS_ID", "sys_id"),
    ];
    if everyone {
        cols.insert(2, col("APPROVER", "approver"));
    }
    ctx.out.list(rows, &cols, meta)
}

/// The approval as output: what it is about comes first.
fn summary(row: &Value) -> Value {
    // `sysapproval` is empty when the record is not a task.
    let of = match raw(row, "sysapproval") {
        "" => "document_id",
        _ => "sysapproval",
    };
    json!({
        "record": shown(row, of),
        "short_description": shown(row, "sysapproval.short_description"),
        "state": stored(row, "state"),
        "approver": shown(row, "approver"),
        "due": shown(row, "due_date"),
        "requested_on": stored(row, "sys_created_on"),
        "comments": shown(row, "comments"),
        "table": stored(row, "source_table"),
        "record_sys_id": stored(row, of),
        "sys_id": stored(row, "sys_id"),
    })
}

/// The approval with this sys_id, or the caller's one for a record.
/// `pending` limits it to those that wait for an answer.
async fn find(client: &Client, approval: &str, pending: bool) -> Result<Value> {
    let params = TableQuery::default()
        .fields(FIELDS)
        .display(DisplayValue::All);
    if is_sys_id(approval) {
        return match client.record(TABLE, approval, &params).await {
            Err(Error::Api(failure)) if failure.status == Some(404) => {
                Err(Error::NotFound(format!(
                    "there is no approval {approval} that you can read; list yours with `sn approval list`"
                )))
            }
            other => other,
        };
    }
    let (record, me) = tokio::try_join!(refs::record(client, approval, None), caller(client))?;
    let of_record = format!(
        "{}^OR{}",
        query::term("sysapproval", "=", &record.sys_id),
        query::term("document_id", "=", &record.sys_id)
    );
    let state = if pending { "state=requested" } else { "" };
    let filter = query::all([
        of_record.as_str(),
        &query::term("approver", "=", &me.sys_id),
        state,
        "ORDERBYDESCsys_created_on",
    ]);
    let params = TableQuery {
        query: filter,
        limit: Some(20),
        ..params
    };
    let rows = client.list(TABLE, &params).await?.rows;
    let belongs = |row: &Value| {
        raw(row, "approver") == me.sys_id
            && [raw(row, "sysapproval"), raw(row, "document_id")].contains(&record.sys_id.as_str())
            && (!pending || raw(row, "state") == "requested")
    };
    if !rows.iter().all(belongs) {
        return Err(dropped("approver and sysapproval", TABLE));
    }
    let none = Error::NotFound(format!(
        "there is no approval of {approval} that waits for you ({}); list yours with `sn approval list`",
        me.user_name
    ));
    let several = format!(
        "you have {} approvals for {approval}; pass the sys_id of one",
        rows.len()
    );
    one(rows, "approval", none, several, |r| {
        format!("{} {}", raw(r, "state"), raw(r, "sys_created_on"))
    })
}

async fn answer(ctx: &Ctx, args: AnswerArgs, state: &str) -> Result<()> {
    let client = ctx.client().await?;
    let row = find(client, &args.approval, true).await?;
    let before = summary(&row);
    let now = raw(&row, "state");
    if now != "requested" {
        return Err(Error::Other {
            code: "conflict",
            message: format!(
                "the approval of {} is `{now}`, so it does not wait for an answer; list those that do with `sn approval list`",
                cell(&before, "record")
            ),
            data: before,
        });
    }
    let verb = if state == "approved" {
        "approve"
    } else {
        "reject"
    };
    ctx.confirm(
        format!(
            "{verb} {} {} (as {})",
            cell(&before, "record"),
            cell(&before, "short_description"),
            cell(&before, "approver")
        )
        .trim_end(),
    )?;
    let mut body = Map::new();
    body.insert("state".into(), state.into());
    if let Some(comment) = args.comment.filter(|c| !c.trim().is_empty()) {
        body.insert("comments".into(), comment.into());
    }
    let params = TableQuery::default()
        .fields(FIELDS)
        .display(DisplayValue::All);
    let after = client
        .update(TABLE, raw(&row, "sys_id"), body, &params)
        .await?;
    // Business rules can refuse the change without an error.
    if raw(&after, "state") != state {
        return Err(Error::api(
            format!(
                "the instance accepted the answer but the approval is still `{}`; check your rights on {}",
                raw(&after, "state"),
                cell(&before, "record")
            ),
            summary(&after),
        ));
    }
    let text = format!("{} is {state}", cell(&before, "record"));
    ctx.out.done(summary(&after), &text)
}

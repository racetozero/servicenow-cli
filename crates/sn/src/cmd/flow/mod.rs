//! Flow Designer: find and read flows here, debug their runs in `debug`.
//!
//! `list` reads `sys_hub_flow`. `get` and `versions` read the designer's own
//! `/api/now/processflow`, which is undocumented and slow (tens of seconds
//! for one flow) and reports failures inside HTTP 200.

use super::args::ListArgs;
use super::lookup::one;
use crate::ctx::Ctx;
use crate::output::{Col, col, columns, flex};
use clap::{Args, Subcommand, ValueEnum};
use serde_json::{Map, Value, json};
use sn_core::client::{Method, Request};
use sn_core::table::raw;
use sn_core::{Client, DisplayValue, Error, Result, TableQuery, is_sys_id, query};
use std::collections::HashMap;
use std::fmt::Write;
use std::time::Duration;

pub mod debug;

/// Reads of one flow took up to 52 seconds on a live instance.
const PROCESSFLOW_TIMEOUT: Duration = Duration::from_secs(180);

const LIST_FIELDS: &[&str] = &[
    "sys_id",
    "name",
    "internal_name",
    "type",
    "status",
    "active",
    "sys_scope.scope",
    "sys_updated_on",
    "sys_updated_by",
];

const LIST_COLUMNS: &[Col] = &[
    flex("NAME", "name"),
    col("INTERNAL NAME", "internal_name"),
    col("TYPE", "type"),
    col("STATUS", "status"),
    col("ACTIVE", "active"),
    col("SCOPE", "sys_scope.scope"),
    col("UPDATED", "sys_updated_on"),
];

#[derive(Subcommand, Debug)]
pub enum FlowCommand {
    /// List flows and subflows.
    #[command(visible_alias = "ls")]
    List(FlowListArgs),
    /// Read one flow: its triggers, inputs, outputs and steps.
    Get(FlowGetArgs),
    /// The saved and published versions of a flow.
    Versions {
        /// Flow: sys_id, internal name, scope.internal_name, or display name.
        flow: String,
    },
    /// Runs of a flow, newest first.
    Runs(debug::RunsArgs),
    /// Everything about one run: its state, failed step, and log.
    Debug(debug::DebugArgs),
    /// The steps of one run, in order.
    Steps(debug::StepsArgs),
    /// The log of one run.
    Logs(debug::LogsArgs),
    /// Why a flow did not run for a record.
    #[command(name = "why-not")]
    WhyNot(debug::WhyNotArgs),
    /// Stream the runs of a flow as they happen.
    Tail(debug::TailArgs),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum FlowType {
    Flow,
    Subflow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum FlowStatus {
    Draft,
    Published,
}

#[derive(Args, Debug)]
pub struct FlowListArgs {
    /// Only this application scope, for example global or x_acme_app.
    #[arg(long, value_name = "SCOPE")]
    pub scope: Option<String>,
    /// Only flows or only subflows.
    #[arg(long = "type", value_enum, value_name = "TYPE")]
    pub kind: Option<FlowType>,
    /// Only drafts or only published flows.
    #[arg(long, value_enum)]
    pub status: Option<FlowStatus>,
    /// Only active flows.
    #[arg(long)]
    pub active: bool,
    #[command(flatten)]
    pub list: ListArgs,
}

#[derive(Args, Debug)]
pub struct FlowGetArgs {
    /// Flow: sys_id, internal name, scope.internal_name, or display name.
    pub flow: String,
    /// A short outline instead of the whole model.
    #[arg(long)]
    pub outline: bool,
}

pub async fn run(ctx: &Ctx, command: FlowCommand) -> Result<()> {
    match command {
        FlowCommand::List(args) => list(ctx, args).await,
        FlowCommand::Get(args) => get(ctx, args).await,
        FlowCommand::Versions { flow } => versions(ctx, &flow).await,
        FlowCommand::Runs(args) => debug::runs(ctx, args).await,
        FlowCommand::Debug(args) => debug::debug(ctx, args).await,
        FlowCommand::Steps(args) => debug::steps(ctx, args).await,
        FlowCommand::Logs(args) => debug::logs(ctx, args).await,
        FlowCommand::WhyNot(args) => debug::why_not(ctx, args).await,
        FlowCommand::Tail(args) => debug::tail(ctx, args).await,
    }
}

async fn list(ctx: &Ctx, args: FlowListArgs) -> Result<()> {
    let filter = list_query(&args)?;
    let client = ctx.client().await?;
    let mut params = args.list.to_query(100);
    params.query = filter;
    if args.list.display.display_value.is_none() {
        params.display = DisplayValue::False;
    }
    let mut cols = columns(&args.list.fields);
    if params.fields.is_empty() {
        params.fields = LIST_FIELDS.iter().map(|f| (*f).to_owned()).collect();
        cols = LIST_COLUMNS.to_vec();
    }
    let page = client.list("sys_hub_flow", &params).await?;
    let meta = args.list.meta(page.rows.len(), page.total, 100);
    ctx.out.list(page.rows, &cols, meta)
}

fn list_query(args: &FlowListArgs) -> Result<String> {
    let scope = match &args.scope {
        Some(scope) if !is_identifier(scope) => {
            return Err(Error::invalid(format!(
                "you passed --scope {scope}; pass the name of an application scope, for example global or x_acme_app"
            )));
        }
        Some(scope) => query::term("sys_scope.scope", "=", scope),
        None => String::new(),
    };
    let kind = match args.kind {
        Some(FlowType::Flow) => "type=flow",
        Some(FlowType::Subflow) => "type=subflow",
        None => "",
    };
    let status = match args.status {
        Some(FlowStatus::Draft) => "status=draft",
        Some(FlowStatus::Published) => "status=published",
        None => "",
    };
    let active = if args.active { "active=true" } else { "" };
    let own = args.list.query.as_deref().unwrap_or_default();
    let filters = query::all([scope.as_str(), kind, status, active]);
    // `^NQ` starts a new query, so the filters would apply to one part only.
    if !filters.is_empty() && query::alternatives(own).len() > 1 {
        return Err(Error::invalid(
            "--query with ^NQ cannot be combined with --scope, --type, --status or --active: they would filter only one part; put all conditions into --query",
        ));
    }
    Ok(query::ordered(
        &query::all([filters.as_str(), own]),
        "ORDERBYname",
    ))
}

fn is_identifier(text: &str) -> bool {
    !text.is_empty()
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// The `sys_hub_flow` record that a person means.
async fn find(client: &Client, token: &str) -> Result<Value> {
    let token = token.trim();
    if token.is_empty() || token.contains(char::is_control) {
        return Err(Error::invalid(
            "the flow is empty; pass a sys_id, an internal name, scope.internal_name, or a display name",
        ));
    }
    let scoped = token
        .split_once('.')
        .filter(|(scope, name)| is_identifier(scope) && is_identifier(name));
    let filter = if is_sys_id(token) {
        query::term("sys_id", "=", token)
    } else {
        let mut filter = format!(
            "{}^OR{}",
            query::term("name", "=", token),
            query::term("internal_name", "=", token)
        );
        if let Some((scope, name)) = scoped {
            let _ = write!(
                filter,
                "^NQ{}^{}",
                query::term("internal_name", "=", name),
                query::term("sys_scope.scope", "=", scope)
            );
        }
        filter
    };
    let params = TableQuery::new(filter)
        .fields(&[
            "sys_id",
            "name",
            "internal_name",
            "type",
            "active",
            "status",
            "remote_trigger_id",
            "sys_scope.scope",
        ])
        .limit(10)
        .display(DisplayValue::False);
    let rows = client.list("sys_hub_flow", &params).await?.rows;
    let same = |row: &Value, field: &str, value: &str| raw(row, field).eq_ignore_ascii_case(value);
    let named = |row: &Value| {
        same(row, "sys_id", token)
            || same(row, "name", token)
            || same(row, "internal_name", token)
            || scoped.is_some_and(|(scope, name)| {
                same(row, "internal_name", name) && same(row, "sys_scope.scope", scope)
            })
    };
    if !rows.iter().all(named) {
        return Err(Error::api(
            format!(
                "the instance ignored the filter for the flow `{token}` and answered with others"
            ),
            Value::Null,
        ));
    }
    let none = Error::NotFound(format!(
        "there is no flow `{token}` that you can read; find it with `sn flow list -q nameLIKE{token}`"
    ));
    let several = format!("several flows match `{token}`; pass scope.internal_name or a sys_id");
    one(rows, "flow", none, several, |row| {
        format!(
            "{}.{} ({})",
            raw(row, "sys_scope.scope"),
            raw(row, "internal_name"),
            raw(row, "name")
        )
    })
}

/// The flow's sys_id; a sys_id needs no lookup.
async fn flow_id(client: &Client, token: &str) -> Result<String> {
    match is_sys_id(token) {
        true => Ok(token.to_ascii_lowercase()),
        false => Ok(raw(&find(client, token).await?, "sys_id").to_owned()),
    }
}

/// `result` of a processflow read, with its failures.
async fn processflow(ctx: &Ctx, path: &str) -> Result<Value> {
    let mut request = Request::new(Method::GET, format!("/api/now/processflow/{path}"));
    if ctx.globals.timeout.is_none() {
        request = request.timeout(PROCESSFLOW_TIMEOUT);
    }
    let result = match ctx.client().await?.send(request).await {
        Ok(response) => response.result()?,
        // The reason is in `result.errorMessage`, not in `error`.
        Err(Error::Api(mut failure)) => {
            if let Some(text) = failure
                .payload
                .pointer("/result/errorMessage")
                .and_then(Value::as_str)
                .filter(|t| !t.is_empty())
            {
                failure.message = text.to_owned();
            }
            return Err(Error::Api(failure));
        }
        Err(err) => return Err(err),
    };
    match result["errorMessage"].as_str() {
        Some(text) if !text.is_empty() => Err(Error::api(
            text.to_owned(),
            json!({ "errorCode": result["errorCode"], "errorMessage": text }),
        )),
        _ if result.get("data").is_none() => Err(Error::api(
            "Flow Designer answered without data; check the flow in the designer",
            result,
        )),
        _ => Ok(result),
    }
}

async fn get(ctx: &Ctx, args: FlowGetArgs) -> Result<()> {
    if args.outline && ctx.out.raw {
        return Err(Error::invalid(
            "--outline and --raw exclude each other; drop one of them",
        ));
    }
    let id = flow_id(ctx.client().await?, &args.flow).await?;
    let mut result = processflow(ctx, &format!("flow/{id}")).await?;
    if ctx.out.raw {
        return ctx.out.item(json!({ "result": result }));
    }
    let data = result.get_mut("data").map(Value::take).unwrap_or_default();
    // The whole model has hundreds of kilobytes; people get the outline.
    match args.outline || ctx.out.wants_text() {
        true => ctx.out.item_with(outline(&data), json!({}), outline_text),
        false => ctx.out.item(data),
    }
}

async fn versions(ctx: &Ctx, flow: &str) -> Result<()> {
    let client = ctx.client().await?;
    let id = flow_id(client, flow).await?;
    let mut result = processflow(ctx, &format!("versioning/{id}")).await?;
    let empty = result["data"].as_array().is_some_and(Vec::is_empty);
    if empty && is_sys_id(flow) {
        // An unknown sys_id gets an empty list too.
        let params = TableQuery::default().fields(&["sys_id"]);
        client.record("sys_hub_flow_base", &id, &params).await?;
    }
    match ctx.out.raw {
        true => ctx.out.item(json!({ "result": result })),
        false => ctx
            .out
            .any(result.get_mut("data").map(Value::take).unwrap_or_default()),
    }
}

fn items<'a>(value: &'a Value, key: &str) -> impl Iterator<Item = &'a Value> {
    value[key]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|item| item["deleted"] != true)
}

/// What a flow is, what starts it, and its steps in order with their depth.
fn outline(data: &Value) -> Value {
    let parameters = |key: &str| -> Vec<Value> {
        items(data, key)
            .map(|p| json!({ "name": p["name"], "label": p["label"], "type": p["type"] }))
            .collect()
    };
    let triggers: Vec<Value> = items(data, "triggerInstances")
        .map(|trigger| {
            let inputs: Map<String, Value> = items(trigger, "inputs")
                .filter(|input| !matches!(&input["value"], Value::Null) && input["value"] != "")
                .filter_map(|input| {
                    Some((input["name"].as_str()?.to_owned(), input["value"].clone()))
                })
                .collect();
            json!({
                "name": trigger["name"], "type": trigger["type"],
                "trigger_type": trigger["triggerType"], "inputs": inputs,
            })
        })
        .collect();
    json!({
        "id": data["id"], "name": data["name"], "internal_name": data["internalName"],
        "type": data["type"], "status": data["status"], "active": data["active"],
        "scope": data["scopeName"], "version": data["version"], "run_as": data["runAs"],
        "description": data["description"], "updated": data["updated"],
        "updated_by": data["updatedBy"],
        "inputs": parameters("inputs"), "outputs": parameters("outputs"),
        "triggers": triggers, "steps": steps(data),
    })
}

/// Actions, logic and subflows share one numbering. A step inside a block
/// names the block's `uiUniqueIdentifier` as its parent.
fn steps(data: &Value) -> Vec<Value> {
    let order = |step: &Value| match &step["order"] {
        Value::String(text) => text.trim().parse::<u64>().ok(),
        other => other.as_u64(),
    };
    let mut all: Vec<(&str, &Value)> = [
        ("actionInstances", "action"),
        ("flowLogicInstances", "flowlogic"),
        ("subFlowInstances", "subflow"),
    ]
    .into_iter()
    .flat_map(|(key, kind)| items(data, key).map(move |step| (kind, step)))
    .collect();
    all.sort_by_key(|(_, step)| order(step).unwrap_or(u64::MAX));
    let by_id: HashMap<&str, &Value> = all
        .iter()
        .filter_map(|(_, step)| Some((step["uiUniqueIdentifier"].as_str()?, *step)))
        .collect();
    all.iter()
        .map(|(kind, step)| {
            let parent = step["parent"].as_str().unwrap_or_default();
            let mut depth = 0;
            let mut at = parent;
            // Bounded: a broken model can have a cycle.
            while !at.is_empty() && depth < 64 {
                depth += 1;
                at = by_id
                    .get(at)
                    .and_then(|p| p["parent"].as_str())
                    .unwrap_or_default();
            }
            let mut row = json!({
                "order": order(step), "depth": depth, "kind": kind, "name": step["name"],
                "parent": by_id.get(parent).and_then(|p| order(p)), "id": step["id"],
            });
            if step["internalName"].as_str().is_some_and(|n| !n.is_empty()) {
                row["internal_name"] = step["internalName"].clone();
            }
            row
        })
        .collect()
}

fn outline_text(outline: &Value) -> String {
    let text = |key: &str| crate::output::cell(outline, key);
    let mut out = format!("{} ({})\n", text("name"), text("internal_name"));
    let _ = writeln!(
        out,
        "{} in {}, status {}, active {}, version {}, updated {} by {}",
        text("type"),
        text("scope"),
        text("status"),
        text("active"),
        text("version"),
        text("updated"),
        text("updated_by")
    );
    if !text("description").is_empty() {
        let _ = writeln!(out, "{}", text("description"));
    }
    for key in ["inputs", "outputs"] {
        for item in items(outline, key) {
            let cell = |k: &str| crate::output::cell(item, k);
            let _ = writeln!(
                out,
                "{}: {} ({})",
                &key[..key.len() - 1],
                cell("name"),
                cell("type")
            );
        }
    }
    for trigger in items(outline, "triggers") {
        let inputs: Vec<String> = trigger["inputs"]
            .as_object()
            .into_iter()
            .flatten()
            .map(|(name, value)| format!("{name}={}", crate::output::cell_text(value)))
            .collect();
        let _ = writeln!(
            out,
            "trigger: {} {}",
            crate::output::cell(trigger, "name"),
            inputs.join(" ")
        );
    }
    for step in items(outline, "steps") {
        let depth = step["depth"].as_u64().unwrap_or(0) as usize;
        let _ = writeln!(
            out,
            "{:>4}  {}{} [{}]",
            crate::output::cell(step, "order"),
            "  ".repeat(depth),
            crate::output::cell(step, "name"),
            crate::output::cell(step, "kind")
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape of the "Policy Review" flow of a live instance: a subflow,
    /// then an If with a nested If and an Else.
    fn model() -> Value {
        json!({
            "id": "f1", "name": "Policy Review", "internalName": "policy_review", "type": "flow",
            "scopeName": "sn_grc", "outputs": [{ "name": "o", "label": "O", "type": "string", "extra": 1 }],
            "triggerInstances": [{
                "name": "Created or Updated", "triggerType": "Record",
                "inputs": [
                    { "name": "table", "value": "sn_grc_policy" },
                    { "name": "run_when_user_list", "value": "" },
                ],
            }],
            "subFlowInstances": [
                { "order": "1", "name": "Add a Pause", "internalName": "add_a_pause", "parent": "", "uiUniqueIdentifier": "u1" },
            ],
            "flowLogicInstances": [
                { "order": "2", "name": "If: published", "parent": "", "uiUniqueIdentifier": "u2" },
                { "order": "3", "name": "If: reviewers", "parent": "u2", "uiUniqueIdentifier": "u3" },
                { "order": "5", "name": "Else", "parent": "u2", "uiUniqueIdentifier": "u5" },
                { "order": "9", "name": "gone", "deleted": true },
            ],
            "actionInstances": [
                { "order": "4", "name": "Update Record", "parent": "u3", "uiUniqueIdentifier": "u4" },
                { "order": "6", "name": "Update Record", "parent": "u5", "uiUniqueIdentifier": "u6" },
            ],
        })
    }

    #[test]
    fn the_outline_orders_and_nests_the_steps() {
        let outline = outline(&model());
        let depths: Vec<(u64, u64)> = items(&outline, "steps")
            .map(|s| {
                (
                    s["order"].as_u64().expect("order"),
                    s["depth"].as_u64().expect("depth"),
                )
            })
            .collect();
        assert_eq!(depths, [(1, 0), (2, 0), (3, 1), (4, 2), (5, 1), (6, 2)]);
        assert_eq!(outline["steps"][3]["parent"], 3);
        assert_eq!(
            outline["triggers"][0]["inputs"],
            json!({ "table": "sn_grc_policy" })
        );
        assert_eq!(
            outline["outputs"],
            json!([{ "name": "o", "label": "O", "type": "string" }])
        );
        assert!(outline_text(&outline).contains("     4      Update Record [action]".trim_start()));
        let cycle = json!({ "actionInstances": [
            { "order": "1", "parent": "b", "uiUniqueIdentifier": "a" },
            { "order": "x", "parent": "a", "uiUniqueIdentifier": "b" },
        ] });
        assert_eq!(steps(&cycle).len(), 2);
    }

    #[test]
    fn list_filters_come_before_the_own_query() {
        let mut args = FlowListArgs {
            scope: Some("global".into()),
            kind: Some(FlowType::Subflow),
            status: Some(FlowStatus::Published),
            active: true,
            list: ListArgs {
                query: Some("nameLIKEemail".into()),
                ..ListArgs::default()
            },
        };
        assert_eq!(
            list_query(&args).expect("query"),
            "sys_scope.scope=global^type=subflow^status=published^active=true^nameLIKEemail^ORDERBYname"
        );
        args.list.query = Some("type=flow^NQtype=subflow".into());
        assert!(list_query(&args).is_err());
        args.list.query = None;
        args.scope = Some("global^active=false".into());
        assert!(list_query(&args).is_err());
    }
}

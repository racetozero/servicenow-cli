//! Decision tables (`sys_decision`).
//!
//! The definition comes from the scripted GraphQL schemas of Decision
//! Builder. Their traps, all silent:
//!
//! - without `first_row` and `last_row` a table has no decisions;
//! - a caller without the roles gets `null`, not an error;
//! - the evaluator ignores unknown input names, choice labels and record
//!   numbers, and answers with the default decision. So inputs are checked
//!   against the table before it runs.

use super::args::{ListArgs, pairs};
use super::graphql;
use super::lookup::{PARALLEL, one};
use crate::ctx::Ctx;
use crate::output::{Col, cell, col, columns, flex};
use clap::{Args, Subcommand};
use futures_util::stream::{self, StreamExt, TryStreamExt};
use serde_json::{Map, Value, json};
use sn_core::client::{Method, Request};
use sn_core::refs::Reference;
use sn_core::table::raw;
use sn_core::{Choice, Client, DisplayValue, Error, Result, TableQuery, is_sys_id, query, refs};
use std::fmt::Write;

const TABLE: &str = "sys_decision";
/// The `answer_table` of a table with several named result elements.
const MULTI_RESULT: &str = "sys_decision_multi_result";
const MAX_DECISIONS: usize = 10_000;
const NO_PLUGIN: &str = "this instance has no Decision Builder API; install or activate the application sn_decision_table";

const LIST_FIELDS: &[&str] = &[
    "sys_id",
    "name",
    "description",
    "answer_table",
    "active",
    "access",
    "sys_scope",
    "sys_class_name",
    "sys_updated_on",
];
const LIST_COLUMNS: &[Col] = &[
    flex("NAME", "name"),
    col("ANSWER TABLE", "answer_table"),
    col("ACTIVE", "active"),
    col("SCOPE", "sys_scope"),
    col("UPDATED", "sys_updated_on"),
];

#[derive(Subcommand, Debug)]
pub enum DecisionCommand {
    /// List decision tables.
    #[command(visible_alias = "ls")]
    List(Box<ListArgs>),
    /// Show the inputs, answers and decisions of a table, in order.
    #[command(visible_alias = "get")]
    Show {
        /// Decision table: name or sys_id.
        #[arg(value_name = "TABLE")]
        table: String,
    },
    /// Evaluate a table and show the decision that matches.
    Run(RunArgs),
}

#[derive(Args, Debug)]
pub struct RunArgs {
    /// Decision table: name or sys_id.
    #[arg(value_name = "TABLE")]
    table: String,
    /// Input name=value (repeatable). Choices take the value, references a sys_id or number; name= sends it empty.
    #[arg(long, short = 'i', value_name = "NAME=VALUE")]
    input: Vec<String>,
    /// Every decision that matches, not only the first.
    #[arg(long)]
    all_matches: bool,
}

pub async fn run(ctx: &Ctx, command: DecisionCommand) -> Result<()> {
    match command {
        DecisionCommand::List(args) => list(ctx, *args).await,
        DecisionCommand::Show { table } => show(ctx, &table).await,
        DecisionCommand::Run(args) => evaluate(ctx, args).await,
    }
}

async fn list(ctx: &Ctx, args: ListArgs) -> Result<()> {
    let client = ctx.client().await?;
    let mut params = args.to_query(100);
    params.query = query::ordered(&params.query, "ORDERBYname");
    let mut cols = columns(&args.fields);
    if params.fields.is_empty() {
        params.fields = LIST_FIELDS.iter().map(|f| (*f).to_owned()).collect();
        cols = LIST_COLUMNS.to_vec();
    }
    let page = client.list(TABLE, &params).await?;
    let meta = args.meta(page.rows.len(), page.total, 100);
    ctx.out.list(page.rows, &cols, meta)
}

/// The `sys_decision` record that a person means.
async fn find(client: &Client, token: &str) -> Result<Value> {
    let token = token.trim();
    if token.is_empty() || token.contains(char::is_control) {
        return Err(Error::invalid(
            "the decision table is empty; pass its name or sys_id",
        ));
    }
    let params = TableQuery::default()
        .fields(&[
            "sys_id",
            "name",
            "description",
            "answer_table",
            "active",
            "sys_scope.scope",
        ])
        .display(DisplayValue::False);
    if is_sys_id(token) {
        return client.record(TABLE, token, &params).await;
    }
    let params = TableQuery {
        query: query::term("name", "=", token),
        limit: Some(20),
        ..params
    };
    let mut rows = client.list(TABLE, &params).await?.rows;
    let none = match rows.is_empty() {
        true => Error::NotFound(format!(
            "there is no decision table `{token}` that you can read; `sn decision list` shows them"
        )),
        false => Error::api(
            format!(
                "the instance ignored the filter for the decision table `{token}`; pass its sys_id"
            ),
            Value::Null,
        ),
    };
    rows.retain(|row| raw(row, "name").eq_ignore_ascii_case(token));
    let several = format!("several decision tables have the name `{token}`; pass a sys_id");
    one(rows, "table", none, several, |row| {
        format!("{} in {}", raw(row, "name"), raw(row, "sys_scope.scope"))
    })
}

/// A table's definition, already in the shape that `show` prints.
struct Definition {
    inputs: Vec<Value>,
    answer_elements: Vec<Value>,
    conditions: Vec<Value>,
    decisions: Vec<Value>,
    truncated: bool,
}

async fn definition(client: &Client, sys_id: &str, multi: bool) -> Result<Definition> {
    let row = "sys_id label order active condition answer { value displayValue answerElementValues { name value displayValue } }";
    // One more than is shown, to see that a table was cut.
    let window = MAX_DECISIONS + 1;
    let document = format!(
        "query ($id: String!) {{ \
         snDtableDesigner {{ \
         decisionInput {{ getDecisionInputsByDecisionTable(sys_id: $id) {{ \
         sys_id {{ value }} element {{ value }} label {{ value }} internal_type {{ value }} \
         mandatory {{ value }} active {{ value }} order {{ value }} reference {{ value }} \
         choices {{ value {{ value }} label {{ value }} }} }} }} \
         decisionCondition {{ getDecisionConditionsByDecisionTable(sys_id: $id) {{ \
         sys_id label field field_label order default_operator \
         type {{ value }} decision_input {{ value }} reference {{ value }} }} }} \
         decision {{ \
         rows: getDecisionsByDecisionTable(sys_id: $id, first_row: 0, last_row: {window}, default_answer: \"false\") {{ {row} }} \
         fallback: getDecisionsByDecisionTable(sys_id: $id, first_row: 0, last_row: {window}, default_answer: \"true\") {{ {row} }} \
         }} }} \
         snDecisionTable {{ answerElement {{ getAnswerElementsOfDecisionTable(sys_id: $id) {{ \
         sys_id {{ value }} element {{ value }} label {{ value }} internal_type {{ value }} order {{ value }} \
         }} }} }} }}"
    );
    let answer = graphql::ask(client, &document, json!({ "id": sys_id })).await?;
    let errors = graphql::errors(&answer);
    if !errors.is_empty() {
        let missing = ["snDtableDesigner", "snDecisionTable"];
        return Err(
            match missing.iter().any(|f| graphql::undefined(&answer, f)) {
                true => Error::api(NO_PLUGIN, errors.into()),
                false => graphql::refusal(errors),
            },
        );
    }
    let list = |path: &str| {
        answer.pointer(&format!("/data/{path}")).and_then(Value::as_array).ok_or_else(|| Error::Other {
            code: "permission",
            message: "Decision Builder showed nothing of this table: your user needs the role decision_table_admin, decision_table_reader or change_manager".into(),
            data: Value::Null,
        })
    };
    let rows = list("snDtableDesigner/decision/rows")?;
    let fallback = list("snDtableDesigner/decision/fallback")?;
    Ok(Definition {
        inputs: list("snDtableDesigner/decisionInput/getDecisionInputsByDecisionTable")?
            .iter()
            .map(input)
            .collect(),
        conditions: list(
            "snDtableDesigner/decisionCondition/getDecisionConditionsByDecisionTable",
        )?
        .iter()
        .map(|c| {
            json!({
                "label": leaf(c, "label"), "field": leaf(c, "field"),
                "field_label": leaf(c, "field_label"), "operator": leaf(c, "default_operator"),
                "type": leaf(c, "type"), "input": leaf(c, "decision_input"),
                "reference": leaf(c, "reference"), "order": leaf(c, "order"),
                "sys_id": leaf(c, "sys_id"),
            })
        })
        .collect(),
        answer_elements: list("snDecisionTable/answerElement/getAnswerElementsOfDecisionTable")?
            .iter()
            .map(|e| {
                json!({
                    "name": leaf(e, "element"), "label": leaf(e, "label"),
                    "type": leaf(e, "internal_type"), "order": leaf(e, "order"),
                    "sys_id": leaf(e, "sys_id"),
                })
            })
            .collect(),
        decisions: rows
            .iter()
            .take(MAX_DECISIONS)
            .map(|d| decision(d, false, multi))
            .chain(fallback.iter().map(|d| decision(d, true, multi)))
            .collect(),
        truncated: rows.len() > MAX_DECISIONS,
    })
}

/// A field that is a scalar or a `{value}` wrapper.
fn leaf(value: &Value, key: &str) -> Value {
    match &value[key] {
        Value::Object(map) => map.get("value").cloned().unwrap_or(Value::Null),
        other => other.clone(),
    }
}

fn leaf_text(value: &Value, key: &str) -> String {
    match leaf(value, key) {
        Value::String(text) => text,
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn input(value: &Value) -> Value {
    let mut out = json!({
        "name": leaf_text(value, "element"), "label": leaf_text(value, "label"),
        "type": leaf_text(value, "internal_type"), "mandatory": leaf(value, "mandatory") == true,
        "active": leaf(value, "active") != false, "order": leaf(value, "order"),
    });
    let reference = leaf_text(value, "reference");
    if !reference.is_empty() {
        out["reference"] = reference.into();
    }
    if let Some(choices) = value["choices"].as_array().filter(|c| !c.is_empty()) {
        out["choices"] = choices
            .iter()
            .map(|c| json!({ "value": leaf_text(c, "value"), "label": leaf_text(c, "label") }))
            .collect();
    }
    out["sys_id"] = leaf_text(value, "sys_id").into();
    out
}

fn decision(value: &Value, default: bool, multi: bool) -> Value {
    let answer = &value["answer"];
    let mut shown = json!({
        "value": leaf_text(answer, "value"), "display_value": leaf_text(answer, "displayValue"),
    });
    if multi {
        shown["elements"] = elements(answer["answerElementValues"].as_array());
    }
    json!({
        "sys_id": leaf_text(value, "sys_id"), "order": leaf(value, "order"),
        "label": leaf_text(value, "label"), "active": leaf(value, "active"),
        "default": default, "condition": leaf_text(value, "condition"), "answer": shown,
    })
}

fn elements(list: Option<&Vec<Value>>) -> Value {
    let map: Map<String, Value> = list
        .into_iter()
        .flatten()
        .map(|e| {
            let value = json!({ "value": leaf_text(e, "value"), "display_value": leaf_text(e, "displayValue") });
            (leaf_text(e, "name"), value)
        })
        .collect();
    map.into()
}

async fn show(ctx: &Ctx, table: &str) -> Result<()> {
    let client = ctx.client().await?;
    let row = find(client, table).await?;
    let multi = raw(&row, "answer_table") == MULTI_RESULT;
    let definition = definition(client, raw(&row, "sys_id"), multi).await?;
    if definition.truncated {
        ctx.out.warn(&format!(
            "the table has more than {MAX_DECISIONS} decisions; only the first {MAX_DECISIONS} are shown"
        ));
    }
    let data = json!({
        "sys_id": row["sys_id"], "name": row["name"], "description": row["description"],
        "active": raw(&row, "active") == "true", "scope": row["sys_scope.scope"],
        "answer_table": row["answer_table"], "multi_result": multi,
        "inputs": definition.inputs, "answer_elements": definition.answer_elements,
        "conditions": definition.conditions, "decisions": definition.decisions,
    });
    ctx.out.item_with(data, json!({}), show_text)
}

fn answer_text(answer: &Value) -> String {
    match answer["elements"].as_object() {
        Some(map) => map
            .iter()
            .map(|(name, value)| format!("{name}={}", cell(value, "display_value")))
            .collect::<Vec<_>>()
            .join(", "),
        None => match cell(answer, "display_value") {
            text if text.is_empty() => cell(answer, "value"),
            text => text,
        },
    }
}

fn show_text(data: &Value) -> String {
    let mut out = format!(
        "{} ({}), answers from {}\n{}\n",
        cell(data, "name"),
        cell(data, "scope"),
        cell(data, "answer_table"),
        cell(data, "description")
    );
    for input in data["inputs"].as_array().into_iter().flatten() {
        let mandatory = match input["mandatory"] == true {
            true => ", mandatory",
            false => "",
        };
        let _ = writeln!(
            out,
            "input {} ({}{mandatory}): {}",
            cell(input, "name"),
            cell(input, "type"),
            cell(input, "label")
        );
    }
    for decision in data["decisions"].as_array().into_iter().flatten() {
        let condition = match decision["default"] == true {
            true => "otherwise".to_owned(),
            false => cell(decision, "condition"),
        };
        let _ = writeln!(
            out,
            "{:>5}  {}: {condition} -> {}",
            cell(decision, "order"),
            cell(decision, "label"),
            answer_text(&decision["answer"])
        );
    }
    out
}

/// Checks the inputs against the table. Values that are record numbers come
/// back as references to look up.
fn check_inputs(
    inputs: &[Value],
    given: &[(String, String)],
) -> Result<Vec<(String, String, Option<Reference>)>> {
    let known = |name: &str| inputs.iter().find(|i| i["name"] == name);
    let names = || -> Vec<Choice> {
        inputs
            .iter()
            .map(|i| Choice::new(cell(i, "name"), cell(i, "label")))
            .collect()
    };
    let mut seen: Vec<&str> = Vec::new();
    let mut checked = Vec::new();
    for (name, value) in given {
        let bad = |why: String, choices: Vec<Choice>| Error::Input {
            arg: "input".into(),
            message: format!("you passed --input {name}={value}, but {why}"),
            choices,
        };
        let Some(input) = known(name) else {
            let all: Vec<String> = names().into_iter().map(|c| c.value).collect();
            return Err(bad(
                format!(
                    "the table has no input `{name}`; use one of: {}",
                    all.join(", ")
                ),
                names(),
            ));
        };
        if seen.contains(&name.as_str()) {
            return Err(bad("the input is there twice; pass it once".into(), vec![]));
        }
        seen.push(name);
        let mut lookup = None;
        match cell(input, "type").as_str() {
            _ if value.is_empty() => {}
            "boolean" if value != "true" && value != "false" => {
                return Err(bad("it is a boolean; use true or false".into(), vec![]));
            }
            "choice" => {
                let choices: Vec<Choice> = input["choices"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|c| Choice::new(cell(c, "value"), cell(c, "label")))
                    .collect();
                if !choices.is_empty() && !choices.iter().any(|c| c.value == *value) {
                    let hint = match choices.iter().find(|c| c.label.eq_ignore_ascii_case(value)) {
                        Some(c) => format!("; that is the label of the value {}", c.value),
                        None => String::new(),
                    };
                    let values: Vec<&str> = choices.iter().map(|c| c.value.as_str()).collect();
                    return Err(bad(
                        format!(
                            "that is not one of its values{hint}; use one of: {}",
                            values.join(", ")
                        ),
                        choices,
                    ));
                }
            }
            "reference" if !is_sys_id(value) => {
                let table = cell(input, "reference");
                if query::check_name("table", &table).is_err() {
                    return Err(bad(
                        "it refers to no known table; pass a sys_id".into(),
                        vec![],
                    ));
                }
                lookup = Some(Reference {
                    table: Some(table),
                    id: value.clone(),
                });
            }
            _ => {}
        }
        checked.push((name.clone(), value.clone(), lookup));
    }
    let missing: Vec<String> = inputs
        .iter()
        .filter(|i| i["mandatory"] == true && i["active"] == true)
        .map(|i| cell(i, "name"))
        .filter(|name| !seen.contains(&name.as_str()))
        .collect();
    match missing.is_empty() {
        true => Ok(checked),
        false => Err(Error::Input {
            arg: "input".into(),
            message: format!(
                "mandatory inputs are missing: {}; pass --input name=value for each (name= sends it empty)",
                missing.join(", ")
            ),
            choices: names(),
        }),
    }
}

async fn evaluate(ctx: &Ctx, args: RunArgs) -> Result<()> {
    let given = pairs(&args.input, "--input")?;
    let client = ctx.client().await?;
    let row = find(client, &args.table).await?;
    let sys_id = raw(&row, "sys_id");
    let multi = raw(&row, "answer_table") == MULTI_RESULT;
    let definition = definition(client, sys_id, multi).await?;
    let mut inputs = Map::new();
    let mut resolved = Map::new();
    let checked = check_inputs(&definition.inputs, &given)?;
    let mut located = stream::iter(checked)
        .map(async |(name, value, lookup)| match lookup {
            Some(reference) => {
                let record = refs::locate(client, reference).await?;
                Ok::<_, Error>((name, record.sys_id, Some(value)))
            }
            None => Ok((name, value, None)),
        })
        .buffered(PARALLEL);
    while let Some((name, sent, number)) = located.try_next().await? {
        if let Some(number) = number {
            resolved.insert(name.clone(), number.into());
        }
        inputs.insert(name, sent.into());
    }
    let mut body = json!({
        "testInputs": inputs, "isFirstMatch": !args.all_matches,
        "includeDelta": false, "isMultiResultTable": multi,
    });
    if multi {
        body["answerElementNames"] = definition
            .answer_elements
            .iter()
            .map(|e| e["name"].clone())
            .collect();
    }
    let request = Request::new(
        Method::POST,
        "/api/sn_decision_table/generic/evaluate_decision_table",
    )
    .query("decisionTableId", sys_id)
    .json(body)
    .reads();
    let answer = match client.send(request).await {
        Ok(answer) => answer.result()?,
        Err(Error::Api(mut failure)) if failure.missing_route() => {
            failure.message = NO_PLUGIN.into();
            return Err(Error::Api(failure));
        }
        Err(err) => return Err(err),
    };
    let Value::Object(matched) = answer else {
        return Err(Error::api(
            "the evaluation answered without a result; check the decision table",
            answer,
        ));
    };
    // The answer has no order; the definition does.
    let position = |id: &str| definition.decisions.iter().position(|d| d["sys_id"] == id);
    let mut matches: Vec<(usize, Value)> = matched
        .iter()
        .map(|(id, answers)| {
            let first = answers.as_array().and_then(|a| a.first());
            let answer = match multi {
                true => json!({ "elements": elements(answers.as_array()) }),
                false => json!({
                    "value": first.map(|a| leaf_text(a, "value")).unwrap_or_default(),
                    "display_value": first.map(|a| leaf_text(a, "displayValue")).unwrap_or_default(),
                }),
            };
            match position(id) {
                Some(at) => {
                    let d = &definition.decisions[at];
                    (at, json!({ "sys_id": id, "label": d["label"], "order": d["order"], "default": d["default"], "answer": answer }))
                }
                None => (usize::MAX, json!({ "sys_id": id, "answer": answer })),
            }
        })
        .collect();
    matches.sort_by_key(|(at, _)| *at);
    let matches: Vec<Value> = matches.into_iter().map(|(_, m)| m).collect();
    let mut data = json!({
        "sys_id": sys_id, "name": row["name"], "inputs": inputs, "matches": matches,
    });
    if !resolved.is_empty() {
        data["resolved_from"] = resolved.into();
    }
    ctx.out.item_with(data, json!({}), |data| {
        let lines: Vec<String> = data["matches"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|m| format!("{}: {}", cell(m, "label"), answer_text(&m["answer"])))
            .collect();
        match lines.is_empty() {
            true => "no decision matches".to_owned(),
            false => lines.join("\n"),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs() -> Vec<Value> {
        vec![
            json!({ "name": "risk", "label": "Risk", "type": "choice", "mandatory": true, "active": true,
                "choices": [{ "value": "2", "label": "High" }] }),
            json!({ "name": "urgent", "type": "boolean", "mandatory": false, "active": true }),
            json!({ "name": "change", "type": "reference", "reference": "change_request", "mandatory": false, "active": true }),
        ]
    }

    fn check(given: &[(&str, &str)]) -> Result<Vec<(String, String, Option<Reference>)>> {
        let given: Vec<(String, String)> = given
            .iter()
            .map(|(n, v)| ((*n).to_owned(), (*v).to_owned()))
            .collect();
        check_inputs(&inputs(), &given)
    }

    #[test]
    fn inputs_are_checked_because_the_evaluator_does_not() {
        let checked = check(&[("risk", "2"), ("change", "CHG0000001"), ("urgent", "")])
            .expect("valid inputs");
        assert_eq!(
            checked[1].2.as_ref().expect("lookup").table.as_deref(),
            Some("change_request")
        );
        let message = |given: &[(&str, &str)]| check(given).expect_err("invalid").to_string();
        assert!(message(&[("risk", "High")]).contains("label of the value 2"));
        assert!(message(&[("Risk", "2")]).contains("no input `Risk`"));
        assert!(message(&[("urgent", "yes"), ("risk", "2")]).contains("true or false"));
        assert!(message(&[("risk", "2"), ("risk", "2")]).contains("twice"));
        assert!(message(&[]).contains("mandatory inputs are missing: risk"));
    }
}

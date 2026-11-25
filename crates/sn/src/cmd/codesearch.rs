//! Text search over all script tables (Code Search API).
//!
//! The API has silent traps, so this checks:
//! - an unknown table is ignored and everything is searched;
//! - an unknown scope matches nothing; without one, only the current
//!   application is searched, so all scopes are asked for;
//! - the term goes into an encoded query as it is, so `^` would split it;
//! - the limit counts the records that were looked at, not the hits.

use crate::ctx::Ctx;
use crate::output::{Col, cell, col, flex};
use clap::Args;
use serde_json::{Value, json};
use sn_core::client::{Method, Request};
use sn_core::table::raw;
use sn_core::{Choice, Client, Error, Result, TableQuery, query};
use std::time::Duration;

const API: &str = "/api/sn_codesearch/code_search";
const GROUP: &str = "sn_codesearch.Default Search Group";
/// The instance's limit (`sn_codesearch.search.results.max`) unless changed.
const MAX: u64 = 500;
/// A search of all 31 tables takes 5 to 24 seconds on a small instance.
const TIMEOUT: Duration = Duration::from_secs(120);

const COLUMNS: &[Col] = &[
    col("TABLE", "table"),
    col("NAME", "name"),
    col("FIELD", "field"),
    col("HITS", "count"),
    flex("LINE", "first"),
];

#[derive(Args, Debug)]
pub struct CodesearchArgs {
    /// Text to find in scripts, upper and lower case alike; no wildcards.
    pub term: String,
    /// Only this table, for example sys_script_include.
    #[arg(long, value_name = "TABLE")]
    pub table: Option<String>,
    /// Only this application scope: name or sys_id.
    #[arg(long, value_name = "SCOPE")]
    pub scope: Option<String>,
    /// Most records to look at (the instance allows 500 unless changed).
    #[arg(long, short = 'n', default_value_t = MAX, value_name = "N", value_parser = clap::value_parser!(u64).range(1..))]
    pub limit: u64,
}

pub async fn run(ctx: &Ctx, args: CodesearchArgs) -> Result<()> {
    if args.term.trim().is_empty() || args.term.contains('^') {
        return Err(Error::invalid(format!(
            "you passed the text `{}`; pass text that is not empty and has no ^, which the instance reads as the end of a condition",
            args.term
        )));
    }
    if let Some(scope) = &args.scope {
        query::check_name("scope", scope)?;
    }
    let client = ctx.client().await?;
    let timeout = ctx.globals.timeout.is_none().then_some(TIMEOUT);
    if let Some(table) = &args.table {
        check_table(client, table).await?;
    }
    let mut request = Request::new(Method::GET, format!("{API}/search"))
        .query("term", &args.term)
        .query("search_group", GROUP)
        .query("limit", args.limit.to_string())
        .query(
            "search_all_scopes",
            if args.scope.is_some() {
                "false"
            } else {
                "true"
            },
        )
        .query_opt("current_app", args.scope.clone())
        .query_opt("table", args.table.clone());
    request.timeout = timeout;
    let result = client.send(request).await.map_err(explain)?.result()?;
    let groups: &[Value] = match (&result, &args.table) {
        (Value::Array(_), Some(table)) => {
            return Err(Error::api(
                format!(
                    "the instance ignored --table {table} and searched everything; search again without it"
                ),
                Value::Null,
            ));
        }
        (Value::Array(groups), None) => groups,
        (Value::Object(_), _) => std::slice::from_ref(&result),
        _ => {
            return Err(Error::api(
                "the Code Search API answered without a result",
                result,
            ));
        }
    };
    if let Some(warning) = truncated(groups, args.limit) {
        ctx.out.warn(&warning);
    }
    let rows = flatten(groups, &args.term);
    if rows.is_empty()
        && let Some(scope) = &args.scope
    {
        check_scope(ctx, client, scope).await?;
    }
    if ctx.out.raw {
        return ctx.out.item(json!({ "result": result }));
    }
    ctx.out.list(rows, COLUMNS, json!({}))
}

fn explain(err: Error) -> Error {
    match err {
        Error::Api(mut failure) if failure.missing_route() => {
            failure.message = "this instance has no Code Search API; install or activate the application sn_codesearch".into();
            Error::Api(failure)
        }
        other => other,
    }
}

async fn check_table(client: &Client, table: &str) -> Result<()> {
    let request = Request::new(Method::GET, format!("{API}/tables")).query("search_group", GROUP);
    let result = client.send(request).await.map_err(explain)?.result()?;
    let mut names: Vec<&str> = result
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|t| t["name"].as_str())
        .collect();
    if names.contains(&table) {
        return Ok(());
    }
    names.sort_unstable();
    Err(Error::Input {
        arg: "table".into(),
        message: format!(
            "you passed --table {table}, which Code Search does not cover; use one of: {}",
            names.join(", ")
        ),
        choices: names.iter().map(|n| Choice::new(*n, *n)).collect(),
    })
}

/// Nothing was found: is the scope empty, or mistyped?
async fn check_scope(ctx: &Ctx, client: &Client, scope: &str) -> Result<()> {
    let params = TableQuery::new(format!("scope={scope}^ORsys_id={scope}"))
        .fields(&["sys_id", "scope"])
        .limit(1);
    match client.list("sys_scope", &params).await {
        Ok(page) => match page.rows.iter().any(|row| {
            raw(row, "scope").eq_ignore_ascii_case(scope)
                || raw(row, "sys_id").eq_ignore_ascii_case(scope)
        }) {
            true => Ok(()),
            false => Err(Error::invalid(format!(
                "you passed --scope {scope}, which is no application scope of {}; pass a scope name such as global, or a sys_id",
                client.host()
            ))),
        },
        // Only administrators read sys_scope; the search itself ran.
        Err(err) => {
            ctx.out.warn(&format!(
                "nothing was found, and the scope {scope} could not be checked ({err}); a mistyped scope finds nothing either"
            ));
            Ok(())
        }
    }
}

fn hits(group: &Value) -> &[Value] {
    group["hits"].as_array().map_or(&[], Vec::as_slice)
}

/// One row per field that matches. Numbers arrive as floats.
fn flatten(groups: &[Value], term: &str) -> Vec<Value> {
    let term = term.to_lowercase();
    let int = |value: &Value| value.as_f64().map(|f| f as u64);
    let mut rows = Vec::new();
    for group in groups {
        for hit in hits(group) {
            for found in hit["matches"].as_array().into_iter().flatten() {
                let lines: Vec<Value> = found["lineMatches"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|line| {
                        let text = line["context"].as_str().unwrap_or_default();
                        json!({
                            "line": int(&line["line"]), "text": text,
                            "match": text.to_lowercase().contains(&term),
                        })
                    })
                    .collect();
                let first = lines
                    .iter()
                    .find(|l| l["match"] == true)
                    .map(|l| format!("{}: {}", cell(l, "line"), cell(l, "text").trim()));
                let table = match &hit["className"] {
                    Value::Null => &group["recordType"],
                    class => class,
                };
                rows.push(json!({
                    "table": table, "sys_id": hit["sysId"], "name": hit["name"],
                    "field": found["field"], "count": int(&found["count"]),
                    "first": first, "lines": lines,
                }));
            }
        }
    }
    rows
}

/// The tables whose result may be cut. Each table may look at what earlier
/// tables left of the limit; up to a quarter of the records are then dropped
/// as not readable, so many hits mean that the limit may have been reached.
fn truncated(groups: &[Value], limit: u64) -> Option<String> {
    let mut left = limit.min(MAX);
    let mut near = Vec::new();
    let mut skipped = Vec::new();
    for group in groups {
        let table = group["recordType"].as_str().unwrap_or("?");
        let found = hits(group).len() as u64;
        if left == 0 {
            skipped.push(table);
        } else if found * 5 >= left * 4 {
            near.push(format!("{table} ({found} of {left})"));
        }
        left = left.saturating_sub(found);
    }
    if near.is_empty() && skipped.is_empty() {
        return None;
    }
    let mut parts = Vec::new();
    if !near.is_empty() {
        parts.push(format!("close to the limit in {}", near.join(", ")));
    }
    if !skipped.is_empty() {
        parts.push(format!(
            "{} table(s) not searched ({})",
            skipped.len(),
            skipped.join(", ")
        ));
    }
    Some(format!(
        "the result may be incomplete: {}; narrow it with --table, --scope or a longer text",
        parts.join("; ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn group(table: &str, hits: usize) -> Value {
        json!({ "recordType": table, "hits": vec![json!({}); hits] })
    }

    #[test]
    fn cut_results_name_their_tables() {
        assert_eq!(truncated(&[group("a", 3), group("b", 0)], 500), None);
        let text = truncated(&[group("a", 8), group("b", 2), group("c", 0)], 10).expect("cut");
        assert!(
            text.contains("a (8 of 10), b (2 of 2)") && text.contains("not searched (c)"),
            "{text}"
        );
    }

    #[test]
    fn hits_become_one_row_per_field() {
        let groups = [json!({ "recordType": "sys_script", "hits": [{
            "className": "sys_script_include", "sysId": "1", "name": "Util",
            "matches": [{ "field": "script", "count": 1.0, "lineMatches": [
                { "line": 6.0, "context": "var a;" }, { "line": 7.0, "context": "  GlideRecord(x)" },
            ] }],
        }] })];
        let rows = flatten(&groups, "gliderecord");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["table"], "sys_script_include");
        assert_eq!(rows[0]["first"], "7: GlideRecord(x)");
        assert_eq!(
            rows[0]["lines"][0],
            json!({ "line": 6, "text": "var a;", "match": false })
        );
    }
}

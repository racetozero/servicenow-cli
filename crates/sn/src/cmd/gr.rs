//! Reads through GraphQL: fields of related records in one request.

use super::args::{DisplayArg, PageArgs};
use super::graphql;
use crate::ctx::Ctx;
use crate::output::columns;
use clap::Args;
use serde_json::{Map, Value, json};
use sn_core::{DisplayValue, Error, Result, query};
use std::fmt::Write;

const DEFAULT_LIMIT: u64 = 100;

#[derive(Args, Debug)]
pub struct GrArgs {
    /// Table, for example incident.
    pub table: String,
    /// Fields, comma-separated; dots follow references: number,caller_id.manager.email.
    #[arg(
        long,
        short = 'f',
        value_delimiter = ',',
        required_unless_present = "count",
        conflicts_with = "count"
    )]
    pub fields: Vec<String>,
    /// Encoded query; ORDERBY works.
    #[arg(long, short = 'q', alias = "sysparm-query")]
    pub query: Option<String>,
    /// Only how many records match.
    #[arg(long)]
    pub count: bool,
    #[command(flatten)]
    pub page: PageArgs,
    #[command(flatten)]
    pub display: DisplayArg,
}

pub async fn run(ctx: &Ctx, args: GrArgs) -> Result<()> {
    query::check_name("table", &args.table)?;
    // Names go into the document as they are, so they must be names.
    let mut fields: Vec<String> = Vec::with_capacity(args.fields.len());
    for field in args.fields.iter().map(|f| f.trim()) {
        for part in field.split('.') {
            if part.is_empty() || part.starts_with(|c: char| c.is_ascii_digit()) {
                return Err(Error::invalid(format!(
                    "you passed the field `{field}` in --fields {}; pass names such as number,caller_id.email",
                    args.fields.join(",")
                )));
            }
            query::check_name("field", part)?;
        }
        if !fields.iter().any(|have| have == field) {
            fields.push(field.to_owned());
        }
    }
    let table = args.table.as_str();
    let display = args.display.value();
    // A count still needs a page; one record is the cheapest.
    let (selection, limit, offset) = match args.count {
        true => ("_rowCount ".to_owned(), 1, 0),
        false => {
            let mut selection = "_results { ".to_owned();
            let paths: Vec<Vec<&str>> = fields.iter().map(|f| f.split('.').collect()).collect();
            let paths: Vec<&[&str]> = paths.iter().map(Vec::as_slice).collect();
            select(&paths, leaf(display), &mut selection);
            selection.push_str("} ");
            let limit = args.page.limit.unwrap_or(DEFAULT_LIMIT);
            (selection, limit, args.page.offset.unwrap_or(0))
        }
    };
    let (header, condition) = match args.query.is_some() {
        true => ("query ($qc: String!) ", "queryConditions: $qc, "),
        false => ("", ""),
    };
    let document = format!(
        "{header}{{ GlideRecord_Query {{ {table}({condition}pagination: {{ limit: {limit}, offset: {offset} }}) {{ {selection}}} }} }}"
    );
    let variables = match &args.query {
        Some(query) => json!({ "qc": query }),
        None => Value::Null,
    };
    let answer = graphql::ask(ctx.client().await?, &document, variables).await?;
    if !graphql::errors(&answer).is_empty() {
        if graphql::unknown_table(&answer, table) {
            return Err(graphql::unknown_table_error(&answer, table));
        }
        if graphql::undefined(&answer, "_reference") {
            return Err(Error::api(
                format!(
                    "you passed --fields {}, where a dot follows a field that is not a reference; only reference fields have fields of their own (see `sn schema columns {table}`)",
                    fields.join(",")
                ),
                json!({ "errors": graphql::errors(&answer) }),
            ));
        }
        return Err(graphql::failure(&answer));
    }
    if ctx.out.raw {
        return ctx.out.item(answer);
    }
    let found = &answer["data"]["GlideRecord_Query"][table];
    if found.is_null() {
        return Err(Error::api(
            format!(
                "the instance accepted the query but returned nothing for `{table}`; check that you can read this table"
            ),
            answer,
        ));
    }
    if args.count {
        let count = found["_rowCount"].as_u64().ok_or_else(|| {
            Error::api(
                format!("the instance answered without a count for `{table}`"),
                found.clone(),
            )
        })?;
        return ctx
            .out
            .item_with(json!({ "count": count }), json!({}), |_| count.to_string());
    }
    let rows: Vec<Value> = found["_results"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|record| {
            let row: Map<String, Value> = fields
                .iter()
                .map(|field| (field.clone(), pick(record, field, display)))
                .collect();
            row.into()
        })
        .collect();
    let meta = args.page.meta(rows.len(), None, DEFAULT_LIMIT);
    ctx.out.list(rows, &columns(&fields), meta)
}

const fn leaf(display: DisplayValue) -> &'static str {
    match display {
        DisplayValue::True => "displayValue",
        DisplayValue::False => "value",
        DisplayValue::All => "value displayValue",
    }
}

/// Writes the selection of paths, merging those with a common start. A part
/// that is not the last one must be a reference, so it nests through
/// `_reference`.
fn select(paths: &[&[&str]], leaf: &str, out: &mut String) {
    let mut seen: Vec<&str> = Vec::new();
    for first in paths.iter().filter_map(|p| p.first()) {
        if seen.contains(first) {
            continue;
        }
        seen.push(first);
        let same: Vec<&[&str]> = paths
            .iter()
            .filter(|p| p.first() == Some(first))
            .map(|p| &p[1..])
            .collect();
        let deeper: Vec<&[&str]> = same.iter().filter(|p| !p.is_empty()).copied().collect();
        let _ = write!(out, "{first} {{ ");
        if same.len() > deeper.len() {
            let _ = write!(out, "{leaf} ");
        }
        if !deeper.is_empty() {
            out.push_str("_reference { ");
            select(&deeper, leaf, out);
            out.push_str("} ");
        }
        out.push_str("} ");
    }
}

/// The value at a dotted path. An empty reference on the way gives null.
fn pick(record: &Value, path: &str, display: DisplayValue) -> Value {
    let mut at = record;
    let mut parts = path.split('.').peekable();
    while let Some(part) = parts.next() {
        at = &at[part];
        if parts.peek().is_some() {
            at = &at["_reference"];
        }
    }
    if at.is_null() {
        return Value::Null;
    }
    match display {
        DisplayValue::True => at["displayValue"].clone(),
        DisplayValue::False => at["value"].clone(),
        DisplayValue::All => {
            json!({ "display_value": at["displayValue"], "value": at["value"] })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_merge_into_one_selection() {
        let paths: [&[&str]; 4] = [
            &["number"],
            &["caller_id"],
            &["caller_id", "name"],
            &["caller_id", "manager", "email"],
        ];
        let mut out = String::new();
        select(&paths, "value", &mut out);
        assert_eq!(
            out,
            "number { value } caller_id { value _reference { name { value } manager { _reference { email { value } } } } } "
        );
    }

    #[test]
    fn an_empty_reference_gives_null() {
        let record = json!({ "caller_id": { "displayValue": "", "_reference": null }, "n": { "displayValue": "x", "value": "1" } });
        assert_eq!(
            pick(&record, "caller_id.name", DisplayValue::True),
            Value::Null
        );
        assert_eq!(
            pick(&record, "n", DisplayValue::All),
            json!({ "display_value": "x", "value": "1" })
        );
    }
}

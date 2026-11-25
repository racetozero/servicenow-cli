//! Counts, sums and averages that the server computes (Aggregate API).

use super::args::{DisplayArg, show};
use crate::ctx::Ctx;
use crate::output::Format;
use clap::Args;
use serde_json::{Map, Value, json};
use sn_core::client::{Method, Request};
use sn_core::{Result, query};

#[derive(Args, Debug)]
pub struct AggregateArgs {
    /// Table, for example incident.
    pub table: String,
    /// Encoded query.
    #[arg(long, short = 'q', alias = "sysparm-query")]
    pub query: Option<String>,
    /// Number of records (the default when no other function is given).
    #[arg(long, alias = "sysparm-count")]
    pub count: bool,
    /// Fields to average, comma-separated.
    #[arg(
        long,
        visible_alias = "avg",
        value_name = "FIELDS",
        alias = "sysparm-avg-fields"
    )]
    pub avg_fields: Option<String>,
    /// Fields to take the smallest value of.
    #[arg(
        long,
        visible_alias = "min",
        value_name = "FIELDS",
        alias = "sysparm-min-fields"
    )]
    pub min_fields: Option<String>,
    /// Fields to take the largest value of.
    #[arg(
        long,
        visible_alias = "max",
        value_name = "FIELDS",
        alias = "sysparm-max-fields"
    )]
    pub max_fields: Option<String>,
    /// Fields to sum.
    #[arg(
        long,
        visible_alias = "sum",
        value_name = "FIELDS",
        alias = "sysparm-sum-fields"
    )]
    pub sum_fields: Option<String>,
    /// Fields to group by, comma-separated.
    #[arg(long, short = 'g', value_name = "FIELDS", alias = "sysparm-group-by")]
    pub group_by: Option<String>,
    /// Fields to sort the groups by.
    #[arg(long, value_name = "FIELDS", alias = "sysparm-order-by")]
    pub order_by: Option<String>,
    /// Keep only groups such as count^priority^>^3.
    #[arg(long, value_name = "FILTER", alias = "sysparm-having")]
    pub having: Option<String>,
    #[command(flatten)]
    pub display: DisplayArg,
    /// Index hint for the query.
    #[arg(long, value_name = "NAME", alias = "sysparm-query-category")]
    pub query_category: Option<String>,
}

pub async fn run(ctx: &Ctx, args: AggregateArgs) -> Result<()> {
    query::check_name("table", &args.table)?;
    let functions = [
        &args.avg_fields,
        &args.min_fields,
        &args.max_fields,
        &args.sum_fields,
    ];
    let count = args.count || functions.iter().all(|f| f.is_none());
    let request = Request::new(Method::GET, format!("/api/now/stats/{}", args.table))
        .query_opt("sysparm_query", args.query)
        .query_opt("sysparm_count", count.then_some("true"))
        .query_opt("sysparm_avg_fields", args.avg_fields)
        .query_opt("sysparm_min_fields", args.min_fields)
        .query_opt("sysparm_max_fields", args.max_fields)
        .query_opt("sysparm_sum_fields", args.sum_fields)
        .query_opt("sysparm_group_by", args.group_by)
        .query_opt("sysparm_order_by", args.order_by)
        .query_opt("sysparm_having", args.having)
        .query_opt(
            "sysparm_display_value",
            args.display
                .display_value
                .map(|_| args.display.value().as_str()),
        )
        .query_opt("sysparm_query_category", args.query_category);
    let result = ctx.client().await?.send(request).await?.result()?;
    // Nested groups do not fit a table: people and CSV get one row per group.
    let flat = ctx.out.human() || matches!(ctx.out.format, Format::Csv | Format::Tsv);
    if ctx.out.agent || ctx.out.raw || !flat {
        return show(ctx, result);
    }
    let rows = match &result {
        Value::Array(groups) => groups.iter().map(flatten).collect(),
        other => vec![flatten(other)],
    };
    ctx.out.list(rows, &[], json!({}))
}

/// `{groupby_fields, stats}` as one flat row: `priority`, `count`,
/// `avg.reopen_count`.
fn flatten(group: &Value) -> Value {
    let mut row = Map::new();
    for field in group["groupby_fields"].as_array().into_iter().flatten() {
        let name = field["field"].as_str().unwrap_or("group");
        let shown = match &field["display_value"] {
            Value::Null => &field["value"],
            shown => shown,
        };
        row.insert(name.to_owned(), shown.clone());
    }
    for (function, value) in group["stats"].as_object().into_iter().flatten() {
        match value {
            Value::Object(fields) => {
                for (field, value) in fields {
                    row.insert(format!("{function}.{field}"), value.clone());
                }
            }
            value => {
                row.insert(function.clone(), value.clone());
            }
        }
    }
    row.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_become_rows() {
        let group = json!({
            "stats": { "count": "3", "avg": { "reopen_count": "0.5" } },
            "groupby_fields": [{ "field": "priority", "value": "1", "display_value": "1 - Critical" }],
        });
        assert_eq!(
            flatten(&group),
            json!({ "priority": "1 - Critical", "count": "3", "avg.reopen_count": "0.5" })
        );
    }
}

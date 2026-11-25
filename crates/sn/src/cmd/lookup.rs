//! Records that a person names instead of passing a sys_id.

use super::args::dropped;
use serde_json::Value;
use sn_core::table::raw;
use sn_core::{Choice, Client, DisplayValue, Error, Result, TableQuery, is_sys_id, query};

/// Lookups that run at the same time.
pub const PARALLEL: usize = 8;

/// Records whose sys_id or one of `names` is `target`, each row checked.
pub async fn find(
    client: &Client,
    table: &str,
    target: &str,
    names: &[&str],
    fields: &[&str],
    only: &str,
) -> Result<Vec<Value>> {
    if target.is_empty() {
        return Err(Error::invalid(
            "you passed an empty name; pass a name or a sys_id",
        ));
    }
    let mut terms: Vec<String> = names
        .iter()
        .map(|field| query::term(field, "=", target))
        .collect();
    // `global` is the sys_id of the Global scope.
    if is_sys_id(target) || table == "sys_scope" {
        terms.push(query::term("sys_id", "=", target));
    }
    // A trailing AND applies to the whole OR group.
    let params = TableQuery::new(query::all([terms.join("^OR").as_str(), only]))
        .fields(fields)
        .limit(10)
        .display(DisplayValue::False);
    let rows = client.list(table, &params).await?.rows;
    let hit = |row: &Value| {
        raw(row, "sys_id").eq_ignore_ascii_case(target)
            || names
                .iter()
                .any(|f| raw(row, f).eq_ignore_ascii_case(target))
    };
    match rows.iter().all(hit) {
        true => Ok(rows),
        false => Err(dropped(&names.join(", "), table)),
    }
}

/// The only row: `none` without a row, and an error that asks for `arg` and
/// lists the rows when there are several.
pub fn one(
    mut rows: Vec<Value>,
    arg: &str,
    none: Error,
    several: String,
    label: impl Fn(&Value) -> String,
) -> Result<Value> {
    match rows.len() {
        0 => Err(none),
        1 => Ok(rows.remove(0)),
        _ => Err(Error::Input {
            arg: arg.into(),
            message: several,
            choices: rows
                .iter()
                .map(|row| Choice::new(raw(row, "sys_id"), label(row)))
                .collect(),
        }),
    }
}

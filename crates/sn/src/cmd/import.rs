//! Import sets: rows go into a staging table and the instance transforms
//! them into their target table.

use super::args::{BodyArgs, parse_json};
use crate::ctx::{Ctx, read_input};
use crate::output::{col, flex};
use clap::Subcommand;
use serde_json::{Value, json};
use sn_core::client::{Method, Request};
use sn_core::{Error, Result, query};

#[derive(Subcommand, Debug)]
pub enum ImportCommand {
    /// Load one row.
    Create {
        /// Staging table.
        staging_table: String,
        #[command(flatten)]
        body: BodyArgs,
    },
    /// Load many rows in one request.
    Bulk {
        /// Staging table.
        staging_table: String,
        /// JSON array of rows (or {"records": [...]}), @file, or - for stdin.
        #[arg(long, short = 'D', value_name = "JSON")]
        data: String,
    },
    /// Read how a loaded row was transformed.
    Get {
        /// Staging table.
        staging_table: String,
        /// sys_id of the staging row.
        sys_id: String,
    },
}

pub async fn run(ctx: &Ctx, command: ImportCommand) -> Result<()> {
    let request = match command {
        ImportCommand::Create {
            staging_table,
            body,
        } => Request::new(Method::POST, path(&staging_table)?).json(body.required()?.into()),
        ImportCommand::Bulk {
            staging_table,
            data,
        } => {
            let body = match parse_json(&read_input(&data)?, "--data")? {
                Value::Array(rows) => json!({ "records": rows }),
                Value::Object(map) if map.get("records").is_some_and(Value::is_array) => map.into(),
                _ => {
                    return Err(Error::invalid(
                        "--data is JSON but not a list of rows; pass an array such as '[{\"u_name\":\"Ada\"}]', or {\"records\": [...]}",
                    ));
                }
            };
            Request::new(
                Method::POST,
                format!("{}/insertMultiple", path(&staging_table)?),
            )
            .json(body)
        }
        ImportCommand::Get {
            staging_table,
            sys_id,
        } => {
            query::check_name("sys_id", &sys_id)?;
            Request::new(Method::GET, format!("{}/{sys_id}", path(&staging_table)?))
        }
    };
    // The names of the import set are next to `result`, not in it.
    let mut answer = ctx.client().await?.send(request).await?.json()?;
    if ctx.out.raw {
        return ctx.out.item(answer);
    }
    let failed = answer["result"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|row| row["status"] == "error");
    if let Some(row) = failed {
        let why = ["error_message", "status_message"]
            .iter()
            .find_map(|f| row[*f].as_str())
            .unwrap_or("no reason was given");
        return Err(Error::api(
            format!(
                "the instance stored the rows but could not transform all of them: {why}; check the transform map and the values, then look at the import set {}",
                answer["import_set"].as_str().unwrap_or_default()
            ),
            answer,
        ));
    }
    match answer.get_mut("result").map(Value::take) {
        Some(Value::Array(rows)) => {
            let mut meta = json!({});
            for name in ["import_set", "staging_table"] {
                if let Some(text) = answer[name].as_str() {
                    meta[name] = text.into();
                }
            }
            let cols = [
                col("STATUS", "status"),
                col("TABLE", "table"),
                flex("RECORD", "display_value"),
                col("MAP", "transform_map"),
                col("SYS_ID", "sys_id"),
            ];
            ctx.out.list(rows, &cols, meta)
        }
        // Asynchronous loads answer with the sys_ids of the import sets.
        None => ctx.out.item(answer),
        Some(other) => ctx.out.any(other),
    }
}

fn path(staging_table: &str) -> Result<String> {
    query::check_name("table", staging_table)?;
    Ok(format!("/api/now/import/{staging_table}"))
}

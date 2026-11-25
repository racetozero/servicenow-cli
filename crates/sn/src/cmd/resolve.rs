//! Finds the sys_id behind what a person types.

use crate::ctx::Ctx;
use clap::Subcommand;
use serde_json::{Value, json};
use sn_core::refs::{self, Person};
use sn_core::table::text;
use sn_core::{DisplayValue, Error, Result, TableQuery};

#[derive(Subcommand, Debug)]
pub enum ResolveCommand {
    /// Find a user.
    User {
        /// User name, e-mail address, full name, sys_id, or @me.
        value: String,
    },
    /// Find a group.
    Group {
        /// Group name or sys_id.
        value: String,
    },
    /// Find a record.
    Record {
        /// Table, or the record itself.
        #[arg(value_name = "TABLE|RECORD")]
        table: String,
        /// Number or sys_id when the first argument is a table.
        #[arg(value_name = "SYS_ID|NUMBER")]
        id: Option<String>,
    },
}

pub async fn run(ctx: &Ctx, command: ResolveCommand) -> Result<()> {
    let client = ctx.client().await?;
    // Your own record may be closed to you; the instance still names you.
    if let ResolveCommand::User { value } = &command
        && value.trim().eq_ignore_ascii_case("@me")
    {
        let mut out = json!({ "table": "sys_user" });
        for (name, value) in refs::me(client).await?.as_object().into_iter().flatten() {
            out[name.as_str()] = value.clone();
        }
        return ctx.out.item(out);
    }
    let (table, sys_id, fields): (String, String, &[&str]) = match command {
        ResolveCommand::User { value } => (
            "sys_user".into(),
            refs::person(client, Person::User, &value).await?,
            &["sys_id", "user_name", "name", "email", "active"],
        ),
        ResolveCommand::Group { value } => (
            "sys_user_group".into(),
            refs::person(client, Person::Group, &value).await?,
            &["sys_id", "name", "description", "active"],
        ),
        ResolveCommand::Record { table, id } => {
            let record = refs::record(client, &table, id.as_deref()).await?;
            let fields: &[&str] = &["sys_id", "number", "short_description", "sys_class_name"];
            (record.table, record.sys_id, fields)
        }
    };
    let params = TableQuery::default()
        .fields(fields)
        .display(DisplayValue::False);
    let found = match client.record(&table, &sys_id, &params).await {
        Ok(found) => found,
        Err(Error::Api(failure)) if failure.status == Some(404) => {
            return Err(Error::NotFound(format!(
                "there is no {table} record {sys_id} that you can read; check the value and your access"
            )));
        }
        Err(err) => return Err(err),
    };
    let mut out = json!({ "table": table, "sys_id": sys_id });
    for field in fields {
        let value = text(&found, field);
        if !value.is_empty() && *field != "sys_id" {
            out[*field] = Value::from(value);
        }
    }
    ctx.out.item(out)
}

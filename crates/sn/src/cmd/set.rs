//! The application scope and update set that your tracked changes go into.
//!
//! This is state of your user on the instance, not of this CLI. The pickers
//! of the web interface read and write it. Two facts shape this file:
//! `GET current` is the platform's own answer about where changes land, and
//! the application PUT moves only its own session, so the preference row is
//! written too and everything is read back.

use super::lookup::{find, one};
use crate::ctx::Ctx;
use clap::Subcommand;
use serde_json::{Value, json};
use sn_core::client::{Method, Request};
use sn_core::refs::caller;
use sn_core::table::raw;
use sn_core::{Client, DisplayValue, Error, Result, TableQuery, query};

const PICKER: &str = "/api/now/ui/concoursepicker";
const PREFERENCES: &str = "sys_user_preference";
/// The state value has a space; `in_progress` matches nothing.
pub const IN_PROGRESS: &str = "in progress";

#[derive(Subcommand, Debug)]
pub enum SetCommand {
    /// Switch the application scope.
    Scope {
        /// Scope name (x_acme_app), display name, or sys_id; global for Global.
        target: String,
    },
    /// Switch the update set. It must be in progress and in the current scope.
    Updateset {
        /// Name or sys_id.
        target: String,
    },
}

/// Where tracked changes go.
#[derive(Debug, Clone, PartialEq)]
pub struct Context {
    pub scope_id: String,
    pub scope_name: String,
    pub scope: String,
    pub set_id: String,
    pub set_name: String,
}

impl Context {
    pub async fn read(client: &Client) -> Result<Self> {
        let current = client.get(format!("{PICKER}/current")).await?;
        let field = |of: &str, name: &str| {
            current[of][name]
                .as_str()
                .filter(|text| !text.is_empty())
                .map(str::to_owned)
        };
        let app = "currentApplication";
        let (Some(scope_id), Some(scope_name), Some(scope)) = (
            field(app, "sysId"),
            field(app, "name"),
            field(app, "scopeName"),
        ) else {
            return Err(Error::api(
                "the instance did not name your current application scope",
                current,
            ));
        };
        let set = "currentUpdateSet";
        let (Some(set_id), Some(label)) = (field(set, "sysId"), field(set, "name")) else {
            return Err(Error::api(
                "the instance names no current update set for you; only administrators and delegated developers have one",
                current,
            ));
        };
        // The picker shows `NAME [SCOPE]`.
        let set_name = label
            .strip_suffix(&format!(" [{scope_name}]"))
            .unwrap_or(&label)
            .to_owned();
        Ok(Self {
            scope_id,
            scope_name,
            scope,
            set_id,
            set_name,
        })
    }

    pub fn to_json(&self) -> Value {
        json!({
            "scope": { "sys_id": self.scope_id, "name": self.scope_name, "scope": self.scope },
            "update_set": { "sys_id": self.set_id, "name": self.set_name },
        })
    }
}

pub async fn run(ctx: &Ctx, command: SetCommand) -> Result<()> {
    let client = ctx.client().await?;
    let (before, after) = match command {
        SetCommand::Scope { target } => scope(client, target.trim()).await?,
        SetCommand::Updateset { target } => update_set(client, target.trim()).await?,
    };
    let mut out = after.to_json();
    out["previous"] = before.to_json();
    let text = format!(
        "Scope {} ({}), update set {}",
        after.scope_name, after.scope, after.set_name
    );
    ctx.out.done(out, &text)
}

async fn scope(client: &Client, target: &str) -> Result<(Context, Context)> {
    let (before, me, scopes) = tokio::try_join!(
        Context::read(client),
        caller(client),
        find(
            client,
            "sys_scope",
            target,
            &["scope", "name"],
            &["sys_id", "name", "scope"],
            ""
        )
    )?;
    let chosen = chosen(scopes, target, "application scope", "scope")?;
    let id = raw(&chosen, "sys_id");
    client
        .put(format!("{PICKER}/application"), json!({ "app_id": id }))
        .await?;
    let filter = query::all([
        query::term("user", "=", &me.sys_id).as_str(),
        "name=apps.current_app",
        "ORDERBYsys_created_on",
    ]);
    let params = TableQuery::new(filter)
        .fields(&["sys_id", "user", "name"])
        .limit(2)
        .display(DisplayValue::False);
    let rows = client.list(PREFERENCES, &params).await?.rows;
    let row = rows
        .iter()
        .find(|r| raw(r, "user") == me.sys_id && raw(r, "name") == "apps.current_app");
    let path = sn_core::table::path(PREFERENCES);
    match row {
        Some(row) => {
            let path = format!("{path}/{}", raw(row, "sys_id"));
            client.patch(path, json!({ "value": id })).await?;
        }
        None => {
            let body = json!({ "user": me.sys_id, "name": "apps.current_app", "value": id });
            client.post(path, body).await?;
        }
    }
    let after = Context::read(client).await?;
    if after.scope_id != id {
        return Err(Error::api(
            format!(
                "you asked for the scope {}, but the instance still reports {}; switch it in the web interface",
                raw(&chosen, "name"),
                after.scope_name
            ),
            after.to_json(),
        ));
    }
    Ok((before, after))
}

async fn update_set(client: &Client, target: &str) -> Result<(Context, Context)> {
    let before = Context::read(client).await?;
    let fields = ["sys_id", "name", "application", "state"];
    let only = format!("application={}^state={IN_PROGRESS}", before.scope_id);
    let sets = find(client, "sys_update_set", target, &["name"], &fields, &only).await?;
    let usable =
        |r: &Value| raw(r, "application") == before.scope_id && raw(r, "state") == IN_PROGRESS;
    if !sets.iter().all(usable) {
        return Err(super::args::dropped("application", "sys_update_set"));
    }
    if sets.is_empty() {
        // Says why: another scope, or not in progress.
        let near = find(client, "sys_update_set", target, &["name"], &fields, "").await?;
        let why = match near.first() {
            Some(r) if raw(r, "application") != before.scope_id => format!(
                "the update set `{}` belongs to another scope than {}; switch first with `sn set scope`",
                raw(r, "name"),
                before.scope_name
            ),
            Some(r) => format!(
                "the update set `{}` is `{}`; only a set in progress can be current",
                raw(r, "name"),
                raw(r, "state")
            ),
            None => format!("there is no update set `{target}`; pass its name or sys_id"),
        };
        return Err(Error::invalid(why));
    }
    let chosen = chosen(sets, target, "update set", "name")?;
    let id = raw(&chosen, "sys_id");
    // The picker refuses the set that is current already.
    if before.set_id == id {
        return Ok((before.clone(), before));
    }
    let request =
        Request::new(Method::PUT, format!("{PICKER}/updateset")).json(json!({ "sysId": id }));
    let answer = client.send(request).await?.result()?;
    if answer["success"] != Value::Bool(true) {
        return Err(Error::api(
            format!("the instance did not confirm the update set `{target}`"),
            answer,
        ));
    }
    let after = Context::read(client).await?;
    if after.set_id != id {
        return Err(Error::api(
            format!(
                "you asked for the update set {target}, but the instance still reports {}; switch it in the web interface",
                after.set_name
            ),
            after.to_json(),
        ));
    }
    Ok((before, after))
}

/// The only match of `target`, by its `label` field when there are several.
fn chosen(rows: Vec<Value>, target: &str, what: &str, label: &str) -> Result<Value> {
    one(
        rows,
        "target",
        Error::NotFound(format!(
            "there is no {what} `{target}`; pass its name or sys_id"
        )),
        format!("several records match the {what} `{target}`; pass the sys_id of one"),
        |row| raw(row, label).to_owned(),
    )
}

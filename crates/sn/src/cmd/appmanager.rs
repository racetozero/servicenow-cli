//! Store applications and plugins, over the API of the Application Manager
//! page (`/api/sn_appclient/appmanager`). It is undocumented:
//!
//! - paging and searching its lists give wrong rows, so whole lists are read
//!   and filtered here;
//! - its write routes are GETs under the same paths as ids, so an id is
//!   checked before it becomes a path;
//! - an unknown plugin answers 200 with an empty object, and a refused
//!   activation answers 200 with status 3.

use super::cicd::{self, Follow, WaitArgs};
use crate::ctx::Ctx;
use crate::output::{Col, col, flex};
use clap::{Args, Subcommand, ValueEnum};
use serde_json::{Value, json};
use sn_core::client::{Method, Request};
use sn_core::{Choice, Client, Error, Result, is_sys_id};

const API: &str = "/api/sn_appclient/appmanager";
/// Routes that change the instance, where an id could stand.
const ROUTES: &[&str] = &[
    "install",
    "update",
    "repair",
    "activate",
    "rollback",
    "sync_apps",
    "loaddemodata",
];

const COLUMNS: &[Col] = &[
    col("KIND", "kind"),
    col("ID", "id"),
    flex("NAME", "name"),
    col("VERSION", "version"),
    col("LATEST", "latest_version"),
    col("INSTALLED", "installed"),
    col("UPDATE", "update_available"),
];

#[derive(Subcommand, Debug)]
pub enum AppmanagerCommand {
    /// List applications and plugins: the installed ones, unless a flag says otherwise.
    #[command(visible_alias = "ls")]
    List(FilterArgs),
    /// Find applications and plugins by text.
    Search {
        /// Text in the id, name, scope, or description.
        term: String,
        #[command(flatten)]
        filter: FilterArgs,
    },
    /// One application or plugin.
    Show {
        /// Scope, plugin id, or sys_id of an application.
        id: String,
        /// Only applications or only plugins.
        #[arg(long, short = 'k', value_enum)]
        kind: Option<Kind>,
    },
    /// Activate a plugin; it cannot be deactivated.
    Install {
        /// Plugin id, for example com.snc.document_management.
        #[arg(value_name = "PLUGIN")]
        id: String,
        /// Also load its demo data.
        #[arg(long)]
        demo_data: bool,
        #[command(flatten)]
        wait: WaitArgs,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Kind {
    App,
    Plugin,
}

#[derive(Args, Debug)]
pub struct FilterArgs {
    /// Only those with an update.
    #[arg(long, conflicts_with = "available")]
    updates: bool,
    /// Those that are not installed.
    #[arg(long)]
    available: bool,
    /// Only applications or only plugins.
    #[arg(long, short = 'k', value_enum)]
    kind: Option<Kind>,
    /// Most rows (default: all).
    #[arg(long, short = 'n', short_alias = 'l', value_name = "N", value_parser = clap::value_parser!(u64).range(1..))]
    limit: Option<u64>,
    /// Rows to skip.
    #[arg(long, value_name = "N", default_value_t = 0)]
    offset: u64,
}

impl FilterArgs {
    fn tab(&self) -> &'static str {
        match (self.updates, self.available) {
            (true, _) => "updates",
            (_, true) => "available_for_you",
            _ => "installed",
        }
    }
}

pub async fn run(ctx: &Ctx, command: AppmanagerCommand) -> Result<()> {
    match command {
        AppmanagerCommand::List(filter) => list(ctx, &filter, None).await,
        AppmanagerCommand::Search { term, filter } => {
            let term = term.trim().to_lowercase();
            if term.is_empty() {
                return Err(Error::invalid(
                    "the search text is empty; pass a part of an id, name or description",
                ));
            }
            list(ctx, &filter, Some(&term)).await
        }
        AppmanagerCommand::Show { id, kind } => show(ctx, &id, kind).await,
        AppmanagerCommand::Install {
            id,
            demo_data,
            wait,
        } => install(ctx, &id, demo_data, &wait).await,
    }
}

fn explain(err: Error) -> Error {
    match err {
        Error::Api(mut failure) => {
            if failure.status == Some(403) {
                failure.message = "the Application Manager answers only to the roles admin, sn_appclient.app_client_user, sn_appclient.app_client_company_installer and unified_plugin_read_only".into();
            } else if failure.missing_route() {
                failure.message = format!(
                    "this instance has no Application Manager API ({API}); it may be too old, or its REST API is inactive"
                );
            }
            Error::Api(failure)
        }
        other => other,
    }
}

/// One whole list. The POST only reads.
async fn fetch(client: &Client, route: &str, tab: &str) -> Result<Value> {
    let request = Request::new(Method::POST, format!("{API}/{route}"))
        .query("tab_context", tab)
        .json(json!({}))
        .reads();
    client.send(request).await.map_err(explain)?.result()
}

fn no_list(route: &str, answer: &Value) -> Error {
    Error::api(
        format!("the Application Manager's list of {route} has an unknown shape"),
        answer.clone(),
    )
}

fn app_list(answer: &Value) -> Result<&Vec<Value>> {
    answer["apps"]
        .as_array()
        .ok_or_else(|| no_list("applications", answer))
}

async fn list(ctx: &Ctx, filter: &FilterArgs, term: Option<&str>) -> Result<()> {
    if ctx.out.raw && (filter.kind.is_none() || filter.limit.is_some() || filter.offset > 0) {
        return Err(Error::invalid(
            "--raw shows one answer of the instance; add --kind app or --kind plugin, and drop --limit and --offset",
        ));
    }
    let client = ctx.client().await?;
    let tab = filter.tab();
    let apps = async {
        match filter.kind {
            Some(Kind::Plugin) => Ok(None),
            _ => fetch(client, "apps", tab).await.map(Some),
        }
    };
    let plugins = async {
        match filter.kind {
            Some(Kind::App) => Ok(None),
            _ => fetch(client, "plugins", tab).await.map(Some),
        }
    };
    let (apps, plugins) = tokio::try_join!(apps, plugins)?;
    if ctx.out.raw {
        return ctx.out.item(json!({ "result": apps.or(plugins) }));
    }
    let mut rows = Vec::new();
    if let Some(answer) = &apps {
        rows.extend(app_list(answer)?.iter().map(|row| app_row(row, tab)));
    }
    if let Some(answer) = &plugins {
        let list = answer
            .as_array()
            .ok_or_else(|| no_list("plugins", answer))?;
        rows.extend(list.iter().map(plugin_row));
    }
    let lower = |row: &Value, key: &str| row[key].as_str().unwrap_or_default().to_lowercase();
    if let Some(term) = term {
        rows.retain(|row| {
            ["id", "name", "scope", "description"]
                .iter()
                .any(|key| lower(row, key).contains(term))
        });
    }
    rows.sort_by_cached_key(|row| (lower(row, "name"), lower(row, "id")));
    let total = rows.len() as u64;
    let page: Vec<Value> = rows
        .into_iter()
        .skip(usize::try_from(filter.offset).unwrap_or(usize::MAX))
        .take(
            filter
                .limit
                .map_or(usize::MAX, |n| usize::try_from(n).unwrap_or(usize::MAX)),
        )
        .collect();
    let mut meta = json!({ "total": total });
    let next = filter.offset + page.len() as u64;
    if next < total && !page.is_empty() {
        meta["next_offset"] = next.into();
        meta["next"] = format!("add --offset {next}").into();
    }
    ctx.out.list(page, COLUMNS, meta)
}

/// Text, or null: the instance sends both "" and null for nothing.
fn text(row: &Value, key: &str) -> Value {
    match &row[key] {
        Value::String(text) if !text.is_empty() => text.clone().into(),
        _ => Value::Null,
    }
}

/// "0" and "1" in lists, booleans in details.
fn flag(value: &Value) -> bool {
    match value {
        Value::Bool(on) => *on,
        Value::String(text) => matches!(text.as_str(), "1" | "true"),
        Value::Number(number) => number.as_f64().is_some_and(|n| n != 0.0),
        _ => false,
    }
}

fn app_row(row: &Value, tab: &str) -> Value {
    let id = match text(row, "scope") {
        Value::Null => text(row, "sys_id"),
        scope => scope,
    };
    json!({
        "kind": "app", "id": id, "name": text(row, "name"), "scope": text(row, "scope"),
        "version": text(row, "version"), "latest_version": text(row, "latest_version"),
        "installed": tab != "available_for_you",
        "update_available": flag(&row["update_available"]),
        "vendor": text(row, "vendor"), "description": text(row, "short_description"),
        "sys_id": text(row, "sys_id"),
    })
}

/// The list has no version of a plugin, and its sys_id is the plugin id.
fn plugin_row(row: &Value) -> Value {
    json!({
        "kind": "plugin", "id": text(row, "app_id"), "name": text(row, "app_name"),
        "scope": text(row, "scope"), "version": null, "latest_version": null,
        "installed": flag(&row["active"]), "update_available": flag(&row["update_available"]),
        "vendor": null, "description": text(row, "description"), "sys_id": null,
    })
}

fn check_id(id: &str) -> Result<&str> {
    let id = id.trim();
    let valid = id.bytes().any(|b| b != b'.')
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        && !ROUTES.contains(&id.to_ascii_lowercase().as_str());
    match valid {
        true => Ok(id),
        false => Err(Error::invalid(format!(
            "you passed `{id}`, which is no application scope, plugin id or sys_id; find one with `sn appmanager search TEXT`"
        ))),
    }
}

async fn show(ctx: &Ctx, id: &str, kind: Option<Kind>) -> Result<()> {
    let id = check_id(id)?;
    if is_sys_id(id) && kind == Some(Kind::Plugin) {
        return Err(Error::invalid(format!(
            "you passed the sys_id {id} with --kind plugin; a plugin is named by its id, for example com.snc.incident"
        )));
    }
    let client = ctx.client().await?;
    let app = async {
        match kind {
            Some(Kind::Plugin) => Ok(None),
            _ if is_sys_id(id) => Ok(Some(id.to_ascii_lowercase())),
            _ => find_app(client, id).await,
        }
    };
    let plugin = async {
        match kind {
            Some(Kind::App) => Ok(None),
            _ if is_sys_id(id) => Ok(None),
            _ => plugin_detail(client, id).await,
        }
    };
    let (answer, summary) = match tokio::try_join!(app, plugin)? {
        (Some(sys_id), None) => {
            let answer = app_detail(client, &sys_id, id).await?;
            let summary = app_summary(&answer["app_info_on_instance"]);
            (answer, summary)
        }
        (None, Some(answer)) => {
            let summary = plugin_summary(&answer["plugin"]);
            (answer, summary)
        }
        (Some(_), Some(_)) => {
            return Err(Error::Input {
                arg: "kind".into(),
                message: format!(
                    "`{id}` is both an application and a plugin; pass --kind app or --kind plugin"
                ),
                choices: vec![
                    Choice::new("app", "application"),
                    Choice::new("plugin", "plugin"),
                ],
            });
        }
        (None, None) => {
            return Err(Error::NotFound(format!(
                "there is no application or plugin `{id}`; ids are exact, so find it with `sn appmanager search {id}` (add --available for those not installed)"
            )));
        }
    };
    match ctx.out.raw {
        true => ctx.out.item(json!({ "result": answer })),
        false => ctx.out.item(summary),
    }
}

/// The sys_id of the application with this scope; the detail route takes
/// nothing else.
async fn find_app(client: &Client, scope: &str) -> Result<Option<String>> {
    for tab in ["installed", "available_for_you"] {
        let answer = fetch(client, "apps", tab).await?;
        let ids: Vec<&str> = app_list(&answer)?
            .iter()
            .filter(|row| row["scope"] == scope)
            .filter_map(|row| row["sys_id"].as_str())
            .collect();
        match ids.as_slice() {
            [] => {}
            [one] => return Ok(Some((*one).to_owned())),
            many => {
                return Err(Error::Input {
                    arg: "id".into(),
                    message: format!("several applications have the scope {scope}; pass a sys_id"),
                    choices: many.iter().map(|id| Choice::new(*id, scope)).collect(),
                });
            }
        }
    }
    Ok(None)
}

fn other_record(id: &str, answer: Value) -> Error {
    Error::api(
        format!("you asked for `{id}`, but the Application Manager answered with another record"),
        answer,
    )
}

async fn app_detail(client: &Client, sys_id: &str, wanted: &str) -> Result<Value> {
    let path = format!("{API}/app/{sys_id}");
    let answer = client.get(path).await.map_err(explain)?;
    let info = &answer["app_info_on_instance"];
    let same = info["sys_id"]
        .as_str()
        .is_some_and(|got| got.eq_ignore_ascii_case(sys_id))
        && (is_sys_id(wanted) || info["scope"] == wanted);
    match same {
        true => Ok(answer),
        false => Err(other_record(wanted, answer)),
    }
}

async fn plugin_detail(client: &Client, id: &str) -> Result<Option<Value>> {
    let path = format!("{API}/plugin/{id}");
    let answer = client.get(path).await.map_err(explain)?;
    let Some(plugin) = answer["plugin"].as_object() else {
        return Err(no_list("plugin details", &answer));
    };
    if plugin.is_empty() {
        return Ok(None);
    }
    match plugin.get("id").or_else(|| plugin.get("app_id")) {
        Some(got) if got == id => Ok(Some(answer)),
        _ => Err(other_record(id, answer)),
    }
}

async fn install(ctx: &Ctx, id: &str, demo_data: bool, wait: &WaitArgs) -> Result<()> {
    let id = check_id(id)?;
    if is_sys_id(id) {
        return Err(Error::invalid(format!(
            "you passed the sys_id {id}; pass a plugin id such as com.snc.document_management"
        )));
    }
    let with = if demo_data { " with its demo data" } else { "" };
    ctx.confirm(&format!(
        "activate plugin {id}{with}, which cannot be undone"
    ))?;
    let client = ctx.client().await?;
    // An unknown id would be accepted and would then block the queue.
    let Some(found) = plugin_detail(client, id).await? else {
        return Err(Error::NotFound(format!(
            "there is no plugin `{id}`; find it with `sn appmanager search {id} --available --kind plugin` (applications are installed with `sn cicd app install`)"
        )));
    };
    let plugin = &found["plugin"];
    if flag(&plugin["isInstalled"]) {
        return Err(Error::invalid(format!(
            "plugin {id} is installed already (version {}); nothing was changed",
            plugin["version"].as_str().unwrap_or("unknown")
        )));
    }
    let request = Request::new(Method::GET, format!("{API}/plugin/activate"))
        .query("plugin_id", id)
        .query("load_demo_data", demo_data.to_string())
        .writes();
    let started = match client.send(request).await {
        Ok(response) => response.result()?,
        Err(err) if err.code() == "timeout" || matches!(err, Error::Network(_)) => {
            return Err(Error::Other {
                code: err.code(),
                message: format!(
                    "{err}; the activation may have started, so check with `sn appmanager show {id}` first"
                ),
                data: Value::Null,
            });
        }
        Err(err) => return Err(explain(err)),
    };
    let refused = matches!(started["status"].as_str(), Some("3" | "4"));
    if !refused && started.pointer("/links/progress/id").is_none() {
        return Err(Error::api(
            format!(
                "the instance named no operation, so it is unclear whether the activation started; check with `sn appmanager show {id}`"
            ),
            started,
        ));
    }
    cicd::finish(ctx, started, wait, Follow::Progress).await
}

fn app_summary(info: &Value) -> Value {
    json!({
        "kind": "app", "id": text(info, "scope"), "name": text(info, "name"),
        "scope": text(info, "scope"), "version": text(info, "version"),
        "latest_version": text(info, "latest_version"),
        "installed": flag(&info["isInstalled"]),
        "update_available": flag(&info["isInstalledAndUpdateAvailable"]),
        "vendor": text(info, "vendor"), "description": text(info, "short_description"),
        "sys_id": text(info, "sys_id"), "plugin_id": text(info, "sys_code"),
        "install_date": text(info, "install_date"), "demo_data": text(info, "demo_data"),
        "dependencies": dependencies(&info["dependencies"]),
        "indicators": info["indicators"],
    })
}

fn plugin_summary(plugin: &Value) -> Value {
    json!({
        "kind": "plugin", "id": text(plugin, "id"), "name": text(plugin, "name"),
        "scope": text(plugin, "scope"), "version": text(plugin, "version"),
        "latest_version": null, "installed": flag(&plugin["isInstalled"]),
        "update_available": flag(&plugin["update_available"]), "vendor": null,
        "description": text(plugin, "description"), "sys_id": null,
        "product": text(plugin, "lineage_name"),
        "has_demo_data": flag(&plugin["has_demo_data"]),
        "can_rollback": flag(&plugin["can_rollback"]), "indicators": plugin["indicators"],
    })
}

/// `id:version,id:version`; the version of a platform plugin is `sys`.
fn dependencies(value: &Value) -> Value {
    value
        .as_str()
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(|entry| match entry.rsplit_once(':') {
            Some((id, version)) => json!({ "id": id, "version": version }),
            None => json!({ "id": entry, "version": null }),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_never_routes_or_paths() {
        assert!(check_id("com.snc.incident").is_ok() && check_id("sn_flow-x").is_ok());
        for bad in ["", "..", "a/b", "Install", "activate", "a b", "x?y"] {
            assert!(check_id(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn dependencies_split_at_the_last_colon() {
        assert_eq!(
            dependencies(&json!("com.a:sys, com.b:1.2.0,c")),
            json!([
                { "id": "com.a", "version": "sys" }, { "id": "com.b", "version": "1.2.0" },
                { "id": "c", "version": null },
            ])
        );
        assert_eq!(dependencies(&Value::Null), json!([]));
    }
}

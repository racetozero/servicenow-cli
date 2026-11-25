//! Nouns about the platform: scope, updateset, property, script, choices,
//! instance.

use super::{shown, stored};
use crate::cmd::args::{dropped, show};
use crate::cmd::set::{Context, IN_PROGRESS};
use crate::cmd::{graphql, lookup};
use crate::ctx::Ctx;
use crate::output::{bold, cell, col, fields_text, flex};
use clap::{Args, ValueEnum};
use serde_json::{Map, Value, json};
use sn_core::client::{Method, Request};
use sn_core::table::{Walk, raw, text};
use sn_core::{Choice, Client, DisplayValue, Error, Result, TableQuery, query};
use std::fmt::Write;

pub async fn scope(ctx: &Ctx) -> Result<()> {
    let client = ctx.client().await?;
    let current = Context::read(client).await?;
    let params = TableQuery::default()
        .fields(&[
            "sys_id",
            "name",
            "scope",
            "version",
            "vendor",
            "vendor_prefix",
            "short_description",
            "active",
        ])
        .display(DisplayValue::False);
    let row = client
        .record("sys_scope", &current.scope_id, &params)
        .await?;
    ctx.out.item(json!({
        "sys_id": current.scope_id,
        "name": stored(&row, "name"),
        "scope": stored(&row, "scope"),
        "version": stored(&row, "version"),
        "vendor": stored(&row, "vendor"),
        "vendor_prefix": stored(&row, "vendor_prefix"),
        "description": stored(&row, "short_description"),
        "active": raw(&row, "active") == "true",
        "global": current.scope_id == "global",
    }))
}

#[derive(Args, Debug)]
pub struct UpdatesetArgs {
    /// Name or sys_id; your current update set by default.
    set: Option<String>,
    /// How many of the newest changes to list (0: none).
    #[arg(
        long,
        short = 'n',
        short_alias = 'l',
        default_value_t = 10,
        conflicts_with = "all"
    )]
    limit: u64,
    /// List all changes.
    #[arg(long)]
    all: bool,
}

const SET_FIELDS: &[&str] = &[
    "sys_id",
    "name",
    "state",
    "application",
    "description",
    "is_default",
    "parent",
    "sys_created_by",
    "sys_created_on",
    "sys_updated_on",
];

pub async fn update_set(ctx: &Ctx, args: UpdatesetArgs) -> Result<()> {
    let client = ctx.client().await?;
    let params = TableQuery::default()
        .fields(SET_FIELDS)
        .display(DisplayValue::All);
    let id = match &args.set {
        None => Context::read(client).await?.set_id,
        Some(target) => {
            let found = lookup::find(
                client,
                "sys_update_set",
                target.trim(),
                &["name"],
                &["sys_id", "name", "state", "application.name"],
                "",
            )
            .await?;
            let row = lookup::one(
                found,
                "set",
                Error::NotFound(format!(
                    "there is no update set `{target}` that you can read; pass its name or sys_id"
                )),
                format!("several update sets are named `{target}`; pass the sys_id of one"),
                |r| {
                    format!(
                        "{} [{}] {}",
                        raw(r, "name"),
                        raw(r, "application.name"),
                        raw(r, "state")
                    )
                },
            )?;
            raw(&row, "sys_id").to_owned()
        }
    };
    let limit = match args.all {
        true => u64::MAX,
        false => args.limit,
    };
    let (row, count, changes) = tokio::try_join!(
        client.record("sys_update_set", &id, &params),
        change_count(client, &id),
        changes(client, &id, limit),
    )?;
    let out = json!({
        "sys_id": id,
        "name": stored(&row, "name"),
        "state": stored(&row, "state"),
        "in_progress": raw(&row, "state") == IN_PROGRESS,
        "scope": { "sys_id": stored(&row, "application"), "name": shown(&row, "application") },
        "description": stored(&row, "description"),
        "is_default": raw(&row, "is_default") == "true",
        "parent": stored(&row, "parent"),
        "created_by": stored(&row, "sys_created_by"),
        "created_on": stored(&row, "sys_created_on"),
        "updated_on": stored(&row, "sys_updated_on"),
        "current": args.set.is_none(),
        "change_count": count,
        "changes": changes,
    });
    ctx.out.item_with(out, json!({}), |data| {
        let mut head = data.clone();
        head["scope"] = data["scope"]["name"].clone();
        if let Some(map) = head.as_object_mut() {
            map.remove("changes");
        }
        let mut out = fields_text(&head);
        for change in data["changes"].as_array().into_iter().flatten() {
            let _ = write!(
                out,
                "\n  {}  {} {}: {}",
                cell(change, "updated_on"),
                cell(change, "action"),
                cell(change, "type"),
                cell(change, "target")
            );
        }
        out
    })
}

/// Counted by group, so the answer proves what it counted.
async fn change_count(client: &Client, set: &str) -> Result<u64> {
    let request = Request::new(Method::GET, "/api/now/stats/sys_update_xml")
        .query("sysparm_count", "true")
        .query("sysparm_query", query::term("update_set", "=", set))
        .query("sysparm_group_by", "update_set");
    let groups = client.send(request).await?.result()?;
    let mut total = 0;
    for group in groups.as_array().into_iter().flatten() {
        if group["groupby_fields"][0]["value"] != set {
            return Err(dropped("update_set", "sys_update_xml"));
        }
        let count = &group["stats"]["count"];
        total += count
            .as_u64()
            .or_else(|| count.as_str()?.parse().ok())
            .unwrap_or(0);
    }
    Ok(total)
}

async fn changes(client: &Client, set: &str, limit: u64) -> Result<Vec<Value>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let filter = format!(
        "{}^ORDERBYDESCsys_updated_on",
        query::term("update_set", "=", set)
    );
    let params = TableQuery::new(filter)
        .fields(&[
            "sys_id",
            "name",
            "type",
            "target_name",
            "action",
            "sys_updated_on",
            "sys_updated_by",
            "update_set",
        ])
        .display(DisplayValue::False);
    let mut walk = Walk::new(client, "sys_update_xml", params, limit.min(500), true, None);
    let mut out = Vec::new();
    while let Some(rows) = walk.next().await? {
        for row in rows {
            if raw(&row, "update_set") != set {
                return Err(dropped("update_set", "sys_update_xml"));
            }
            out.push(json!({
                "type": stored(&row, "type"),
                "target": stored(&row, "target_name"),
                "action": stored(&row, "action"),
                "updated_on": stored(&row, "sys_updated_on"),
                "updated_by": stored(&row, "sys_updated_by"),
                "update_name": stored(&row, "name"),
                "sys_id": stored(&row, "sys_id"),
            }));
            if out.len() as u64 >= limit {
                return Ok(out);
            }
        }
    }
    Ok(out)
}

#[derive(Args, Debug)]
pub struct PropertyArgs {
    /// Name, for example glide.ui.session_timeout; end it with * to list a prefix.
    name: String,
    /// Only the property of this application scope (x_acme_app).
    #[arg(long)]
    scope: Option<String>,
}

/// Returned instead of a property that is not set.
const UNSET: &str = "\u{1}unset\u{1}";
const PREFIX_LIMIT: u64 = 200;

pub async fn property(ctx: &Ctx, args: PropertyArgs) -> Result<()> {
    let client = ctx.client().await?;
    let (name, prefix) = match args.name.trim().strip_suffix('*') {
        Some(prefix) => (prefix, true),
        None => (args.name.trim(), false),
    };
    if name.is_empty()
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    {
        return Err(Error::invalid(format!(
            "you passed the property `{}`; pass a name with letters, digits, dots, dashes and underscores, and * at the end to list a prefix",
            args.name
        )));
    }
    let operator = if prefix { "STARTSWITH" } else { "=" };
    let scope = args
        .scope
        .as_deref()
        .map(|s| query::term("sys_scope.scope", "=", s))
        .unwrap_or_default();
    let filter = query::all([
        query::term("name", operator, name).as_str(),
        &scope,
        "ORDERBYname",
    ]);
    let params = TableQuery::new(filter)
        .fields(&[
            "sys_id",
            "name",
            "value",
            "type",
            "description",
            "sys_scope",
            "is_private",
            "read_roles",
            "write_roles",
        ])
        .limit(if prefix { PREFIX_LIMIT } else { 5 })
        .display(DisplayValue::All);
    let rows = client.list("sys_properties", &params).await?.rows;
    let wanted = name.to_ascii_lowercase();
    let hit = |row: &Value| {
        let have = raw(row, "name").to_ascii_lowercase();
        if prefix {
            have.starts_with(&wanted)
        } else {
            have == wanted
        }
    };
    if !rows.iter().all(hit) {
        return Err(dropped("name", "sys_properties"));
    }
    if prefix {
        let rows: Vec<Value> = rows.iter().map(property_json).collect();
        let meta = match rows.len() as u64 >= PREFIX_LIMIT {
            true => json!({ "next": "only the first 200 are shown; pass a longer prefix" }),
            false => json!({}),
        };
        let cols = [
            col("NAME", "name"),
            col("SCOPE", "scope"),
            flex("VALUE", "value"),
        ];
        return ctx.out.list(rows, &cols, meta);
    }
    match rows.as_slice() {
        [row] => ctx.out.item(property_json(row)),
        // The platform answers some properties that have no record.
        [] if args.scope.is_none() => match runtime_property(client, name).await? {
            Some(value) => ctx
                .out
                .item(json!({ "name": name, "value": value, "source": "runtime" })),
            None => Err(Error::NotFound(format!(
                "there is no system property {name} that you can read; list similar ones with `sn get property '{name}*'`"
            ))),
        },
        [] => Err(Error::NotFound(format!(
            "there is no system property {name} in the scope {}; leave out --scope to search all scopes",
            args.scope.unwrap_or_default()
        ))),
        many => Err(Error::Input {
            arg: "scope".into(),
            message: format!("several scopes define {name}; pass --scope"),
            choices: many
                .iter()
                .map(|r| Choice::new(text(r, "sys_scope"), ""))
                .collect(),
        }),
    }
}

fn property_json(row: &Value) -> Value {
    json!({
        "name": stored(row, "name"),
        "value": raw(row, "value"),
        "type": stored(row, "type"),
        "description": stored(row, "description"),
        "scope": shown(row, "sys_scope"),
        "private": raw(row, "is_private") == "true",
        "read_roles": stored(row, "read_roles"),
        "write_roles": stored(row, "write_roles"),
        "sys_id": stored(row, "sys_id"),
        "source": "sys_properties",
    })
}

/// `gs.getProperty`, which only administrators get an answer from.
async fn runtime_property(client: &Client, name: &str) -> Result<Option<String>> {
    let document = "query ($n: String!, $d: String) { snDecisionTable { sysProperties { v: getSysPropertyValue(sys_property_name: $n, default_value: $d) } } }";
    let answer = graphql::execute(client, document, json!({ "n": name, "d": UNSET })).await?;
    Ok(answer["data"]["snDecisionTable"]["sysProperties"]["v"]
        .as_str()
        .filter(|value| *value != UNSET)
        .map(str::to_owned))
}

/// Output names and the properties behind them.
const INSTANCE: [(&str, &str); 6] = [
    ("release", "glide.buildname"),
    ("build_tag", "glide.buildtag"),
    ("build_date", "glide.builddate"),
    ("war", "glide.war"),
    ("node", "glide.cluster.node_name"),
    ("instance_name", "instance_name"),
];

pub async fn instance(ctx: &Ctx) -> Result<()> {
    let client = ctx.client().await?;
    let fields: String = INSTANCE
        .iter()
        .map(|(name, property)| {
            format!(
                "{name}: getSysPropertyValue(sys_property_name: \"{property}\", default_value: $d) "
            )
        })
        .collect();
    let document =
        format!("query ($d: String) {{ snDecisionTable {{ sysProperties {{ {fields}}} }} }}");
    let answer = graphql::execute(client, &document, json!({ "d": UNSET })).await?;
    let values = &answer["data"]["snDecisionTable"]["sysProperties"];
    let mut out = Map::new();
    out.insert("url".into(), client.base().into());
    for (name, _) in INSTANCE {
        let value = match values[name].as_str() {
            Some(text) if !text.is_empty() && text != UNSET => text.into(),
            _ => Value::Null,
        };
        out.insert(name.into(), value);
    }
    // glide-brazil-08-25-2026__patch0-08-26-2026_...zip
    let patch = values["war"]
        .as_str()
        .and_then(|war| war.split_once("__")?.1.split('-').next())
        .filter(|patch| patch.starts_with("patch"))
        .map_or(Value::Null, Value::from);
    out.insert("patch".into(), patch);
    ctx.out.item(out.into())
}

#[derive(Args, Debug)]
pub struct ScriptArgs {
    /// Name; a script include also by its API name (global.JSUtil).
    name: String,
    /// Only the source, as text.
    #[arg(long)]
    source: bool,
    /// Only this kind.
    #[arg(long = "type", short = 't', value_enum)]
    kind: Option<Kind>,
    /// Only scripts of this application scope.
    #[arg(long)]
    scope: Option<String>,
}

#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Script include.
    Include,
    /// Business rule.
    #[value(alias = "br")]
    BusinessRule,
    /// Client script.
    Client,
    /// UI action.
    #[value(alias = "action")]
    UiAction,
    /// UI script.
    UiScript,
    /// Scheduled script.
    Job,
    /// Fix script.
    Fix,
}

impl Kind {
    /// Output name, table, and the field with the table it runs on.
    const fn parts(self) -> (&'static str, &'static str, Option<&'static str>) {
        match self {
            Self::Include => ("include", "sys_script_include", None),
            Self::BusinessRule => ("business-rule", "sys_script", Some("collection")),
            Self::Client => ("client", "sys_script_client", Some("table")),
            Self::UiAction => ("ui-action", "sys_ui_action", Some("table")),
            Self::UiScript => ("ui-script", "sys_ui_script", None),
            Self::Job => ("job", "sysauto_script", None),
            Self::Fix => ("fix", "sys_script_fix", None),
        }
    }
}

pub async fn script(ctx: &Ctx, args: ScriptArgs) -> Result<()> {
    let name = args.name.trim();
    if name.is_empty() {
        return Err(Error::invalid(
            "you passed an empty script name; pass the name of a script",
        ));
    }
    let kinds = match args.kind {
        Some(kind) => vec![kind],
        None => Kind::value_variants().to_vec(),
    };
    let scope = args
        .scope
        .as_deref()
        .map(|s| query::term("sys_scope.scope", "=", s))
        .unwrap_or_default();
    let by_name = query::all([query::term("name", "=", name).as_str(), &scope]);
    // An include's API name is calculated and cannot be searched, so
    // `scope.Name` is searched as a name in a scope.
    let include = match name.rsplit_once('.') {
        Some((of, short)) if !of.is_empty() && !short.is_empty() => format!(
            "{by_name}^NQ{}",
            query::all([
                query::term("name", "=", short).as_str(),
                &query::term("sys_scope.scope", "=", of),
                &scope,
            ])
        ),
        _ => by_name.clone(),
    };
    // A variable that the document does not use is an error.
    let mut variables = Map::new();
    let mut fields = String::new();
    for kind in &kinds {
        let (_, table, runs_on) = kind.parts();
        let (variable, filter, extra) = match kind {
            Kind::Include => ("byApiName", &include, "api_name { value } ".to_owned()),
            _ => (
                "byName",
                &by_name,
                runs_on.map_or(String::new(), |f| format!("{f} {{ value }} ")),
            ),
        };
        variables.insert(variable.into(), filter.as_str().into());
        let _ = write!(
            fields,
            "{table}(queryConditions: ${variable}, pagination: {{ limit: 3 }}) {{ _results {{ sys_id {{ value }} name {{ value }} script {{ value }} active {{ value }} sys_scope {{ value displayValue }} sys_updated_on {{ value }} sys_updated_by {{ value }} {extra}}} }} "
        );
    }
    let declared: Vec<String> = variables
        .keys()
        .map(|name| format!("${name}: String!"))
        .collect();
    let document = format!(
        "query ({}) {{ GlideRecord_Query {{ {fields}}} }}",
        declared.join(", ")
    );
    let client = ctx.client().await?;
    let answer = graphql::execute(client, &document, variables.into()).await?;
    let mut found = Vec::new();
    for kind in &kinds {
        let (label, table, runs_on) = kind.parts();
        let rows = &answer["data"]["GlideRecord_Query"][table]["_results"];
        for row in rows.as_array().into_iter().flatten() {
            let field = |name: &str| row[name]["value"].clone();
            let same = |value: &Value| {
                value
                    .as_str()
                    .is_some_and(|have| have.eq_ignore_ascii_case(name))
            };
            if !same(&field("name")) && !same(&field("api_name")) {
                return Err(dropped("name", table));
            }
            let mut script = json!({
                "type": label,
                "table": table,
                "sys_id": field("sys_id"),
                "name": field("name"),
                "scope": row["sys_scope"]["displayValue"],
                "active": field("active"),
                "updated_on": field("sys_updated_on"),
                "updated_by": field("sys_updated_by"),
            });
            if *kind == Kind::Include {
                script["api_name"] = field("api_name");
            }
            if let Some(runs_on) = runs_on {
                script["runs_on"] = field(runs_on);
            }
            script["script"] = field("script");
            found.push(script);
        }
    }
    match found.as_slice() {
        [script] if args.source => ctx.out.text(script["script"].as_str().unwrap_or_default()),
        [script] => ctx.out.item_with(script.clone(), json!({}), |data| {
            let mut head = data.clone();
            let source = head
                .as_object_mut()
                .and_then(|map| map.remove("script"))
                .unwrap_or_default();
            format!(
                "{}\n\n{}\n{}",
                fields_text(&head),
                bold("Source", ctx.out.tty),
                source.as_str().unwrap_or_default()
            )
        }),
        [] => Err(Error::NotFound(format!(
            "there is no script named {name} that you can read; search the code with `sn codesearch`"
        ))),
        many => Err(Error::Input {
            arg: "type".into(),
            message: format!(
                "{} scripts are named {name}; pass --type or --scope, or read one with `sn get TABLE:SYS_ID`",
                many.len()
            ),
            choices: many
                .iter()
                .map(|s| {
                    let value = format!("{}:{}", cell(s, "table"), cell(s, "sys_id"));
                    let label = format!("{} in {}", cell(s, "type"), cell(s, "scope"));
                    Choice::new(value, label)
                })
                .collect(),
        }),
    }
}

pub async fn choices(ctx: &Ctx, column: &str, field: Option<&str>) -> Result<()> {
    let (table, field) = match (column.split_once('.'), field) {
        (Some((table, field)), None) => (table, field),
        (None, Some(field)) => (column, field),
        (Some(_), Some(_)) => {
            return Err(Error::invalid(format!(
                "you passed `{column}` and a second field; pass TABLE.FIELD or TABLE FIELD"
            )));
        }
        (None, None) => {
            return Err(Error::invalid(format!(
                "you passed `{column}` without a field; pass TABLE.FIELD, for example incident.state"
            )));
        }
    };
    query::check_name("table", table)?;
    query::check_name("field", field)?;
    let meta = ctx
        .client()
        .await?
        .get(format!("/api/now/ui/meta/{table}"))
        .await?;
    let choices = match &meta["columns"][field]["choices"] {
        Value::Array(choices) if !choices.is_empty() => choices.clone(),
        _ => {
            return Err(Error::NotFound(format!(
                "`{table}.{field}` has no choices; check the field with `sn schema columns {table}`"
            )));
        }
    };
    if ctx.out.raw {
        return show(ctx, choices.into());
    }
    ctx.out.list(
        choices,
        &[col("VALUE", "value"), flex("LABEL", "label")],
        json!({}),
    )
}

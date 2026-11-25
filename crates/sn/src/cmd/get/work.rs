//! Nouns about you and your work: me, mine, last, logs.

use super::{Shape, shown, stored};
use crate::cmd::args::dropped;
use crate::cmd::flow::debug::{parse_since, utc_timestamp};
use crate::ctx::Ctx;
use crate::output::{col, flex};
use clap::{Args, ValueEnum};
use serde_json::{Value, json};
use sn_core::refs::Record;
use sn_core::refs::caller;
use sn_core::table::{Walk, raw};
use sn_core::{Client, DisplayValue, Error, Result, TableQuery, now_unix, query};
use std::collections::BTreeSet;

/// Page size of walks over memberships.
const PAGE: u64 = 500;

pub async fn me(ctx: &Ctx) -> Result<()> {
    let client = ctx.client().await?;
    let me = caller(client).await?;
    let params = TableQuery::default()
        .fields(&[
            "sys_id",
            "user_name",
            "name",
            "email",
            "title",
            "department",
            "manager",
            "location",
            "time_zone",
            "active",
            "last_login_time",
        ])
        .display(DisplayValue::All);
    let (user, roles, groups) = tokio::try_join!(
        client.record("sys_user", &me.sys_id, &params),
        roles(client, &me.sys_id),
        groups(client, &me.sys_id),
    )?;
    let (roles, direct) = roles;
    let groups: Vec<Value> = groups
        .into_iter()
        .map(|(id, name)| json!({ "sys_id": id, "name": name }))
        .collect();
    let out = json!({
        "sys_id": me.sys_id,
        "user_name": stored(&user, "user_name"),
        "name": shown(&user, "name"),
        "email": stored(&user, "email"),
        "title": stored(&user, "title"),
        "department": shown(&user, "department"),
        "manager": shown(&user, "manager"),
        "location": shown(&user, "location"),
        "time_zone": stored(&user, "time_zone"),
        "active": raw(&user, "active") == "true",
        "last_login": stored(&user, "last_login_time"),
        "admin": roles.contains("admin"),
        "roles": roles,
        "direct_roles": direct,
        "groups": groups,
    });
    ctx.out.item_with(out, json!({}), |data| {
        let mut data = data.clone();
        // People need the roles they were given, not the hundreds inherited.
        if let Some(map) = data.as_object_mut() {
            let total = map
                .get("roles")
                .and_then(Value::as_array)
                .map_or(0, Vec::len);
            map.insert("roles".into(), format!("{total} (see --json)").into());
        }
        data["groups"] = data["groups"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|g| g["name"].clone())
            .collect();
        crate::output::fields_text(&data)
    })
}

/// All active roles of a user, and those granted directly.
async fn roles(client: &Client, user: &str) -> Result<(BTreeSet<String>, BTreeSet<String>)> {
    let filter = format!("{}^state=active", query::term("user", "=", user));
    let params = TableQuery::new(filter)
        .fields(&["user", "role.name", "inherited"])
        .display(DisplayValue::False);
    let mut walk = Walk::new(client, "sys_user_has_role", params, PAGE, true, None);
    let (mut all, mut direct) = (BTreeSet::new(), BTreeSet::new());
    while let Some(rows) = walk.next().await? {
        for row in rows {
            if raw(&row, "user") != user {
                return Err(dropped("user", "sys_user_has_role"));
            }
            let name = raw(&row, "role.name");
            if name.is_empty() {
                continue;
            }
            if raw(&row, "inherited") != "true" {
                direct.insert(name.to_owned());
            }
            all.insert(name.to_owned());
        }
    }
    Ok((all, direct))
}

/// The groups of a user: sys_id and name.
async fn groups(client: &Client, user: &str) -> Result<Vec<(String, String)>> {
    let params = TableQuery::new(query::term("user", "=", user))
        .fields(&["user", "group", "group.name"])
        .display(DisplayValue::False);
    let mut walk = Walk::new(client, "sys_user_grmember", params, PAGE, true, None);
    let mut out: Vec<(String, String)> = Vec::new();
    while let Some(rows) = walk.next().await? {
        for row in rows {
            if raw(&row, "user") != user {
                return Err(dropped("user", "sys_user_grmember"));
            }
            let id = raw(&row, "group");
            if !id.is_empty() && !out.iter().any(|(have, _)| have == id) {
                out.push((id.to_owned(), raw(&row, "group.name").to_owned()));
            }
        }
    }
    Ok(out)
}

#[derive(Args, Debug)]
pub struct MineArgs {
    /// Only tasks assigned to you.
    #[arg(long)]
    me_only: bool,
    /// Only this task table, for example incident.
    #[arg(long, default_value = "task")]
    table: String,
    /// Most tasks to return.
    #[arg(long, short = 'n', short_alias = 'l', default_value_t = 50)]
    limit: u64,
}

pub async fn mine(ctx: &Ctx, args: MineArgs) -> Result<()> {
    query::check_name("table", &args.table)?;
    let client = ctx.client().await?;
    let me = caller(client).await?.sys_id;
    let own = tasks(
        client,
        &args,
        query::term("assigned_to", "=", &me),
        |row: &Value| raw(row, "assigned_to") == me,
    );
    let of_groups = async {
        if args.me_only {
            return Ok(Vec::new());
        }
        let groups: Vec<String> = groups(client, &me)
            .await?
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        if groups.is_empty() {
            return Ok(Vec::new());
        }
        let filter = format!("assigned_toISEMPTY^assignment_groupIN{}", groups.join(","));
        tasks(client, &args, filter, |row| {
            raw(row, "assigned_to").is_empty()
                && groups.iter().any(|g| g == raw(row, "assignment_group"))
        })
        .await
    };
    let (mut rows, more) = tokio::try_join!(own, of_groups)?;
    rows.extend(more);
    // Raw times are UTC and sort as text.
    rows.sort_by(|a, b| b["updated_on"].as_str().cmp(&a["updated_on"].as_str()));
    rows.truncate(usize::try_from(args.limit).unwrap_or(usize::MAX));
    let cols = [
        col("NUMBER", "number"),
        col("PRIORITY", "priority"),
        col("STATE", "state"),
        col("ASSIGNED TO", "assigned_to"),
        col("GROUP", "assignment_group"),
        col("UPDATED", "updated_on"),
        flex("SHORT DESCRIPTION", "short_description"),
    ];
    ctx.out.list(rows, &cols, json!({}))
}

async fn tasks(
    client: &Client,
    args: &MineArgs,
    filter: String,
    belongs: impl Fn(&Value) -> bool,
) -> Result<Vec<Value>> {
    let params = TableQuery::new(format!("active=true^{filter}^ORDERBYDESCsys_updated_on"))
        .fields(&[
            "sys_id",
            "number",
            "sys_class_name",
            "short_description",
            "state",
            "priority",
            "active",
            "assigned_to",
            "assignment_group",
            "sys_updated_on",
        ])
        .limit(args.limit)
        .display(DisplayValue::All);
    let rows = client.list(&args.table, &params).await?.rows;
    rows.iter()
        .map(|row| {
            if raw(row, "active") != "true" || !belongs(row) {
                return Err(dropped("assigned_to or active", &args.table));
            }
            Ok(json!({
                "number": stored(row, "number"),
                "table": stored(row, "sys_class_name"),
                "short_description": stored(row, "short_description"),
                "state": shown(row, "state"),
                "priority": shown(row, "priority"),
                "assigned_to": shown(row, "assigned_to"),
                "assignment_group": shown(row, "assignment_group"),
                "updated_on": stored(row, "sys_updated_on"),
                "sys_id": stored(row, "sys_id"),
            }))
        })
        .collect()
}

#[derive(Args, Debug)]
pub struct LastArgs {
    /// Look in this table instead of all tasks, for example incident or sys_script.
    #[arg(long, default_value = "task")]
    table: String,
    /// Fields of the record, comma-separated.
    #[arg(long, short = 'f', value_delimiter = ',')]
    fields: Vec<String>,
}

pub async fn last(ctx: &Ctx, args: LastArgs) -> Result<()> {
    query::check_name("table", &args.table)?;
    let client = ctx.client().await?;
    let me = caller(client).await?.user_name;
    let filter = format!(
        "{}^ORDERBYDESCsys_updated_on",
        query::term("sys_updated_by", "=", &me)
    );
    let params = TableQuery::new(filter)
        .fields(&["sys_id", "sys_class_name", "sys_updated_by"])
        .limit(2)
        .display(DisplayValue::False);
    let rows = client.list(&args.table, &params).await?.rows;
    if rows
        .iter()
        .any(|r| !raw(r, "sys_updated_by").eq_ignore_ascii_case(&me))
    {
        return Err(dropped("sys_updated_by", &args.table));
    }
    let Some(row) = rows.first() else {
        return Err(Error::NotFound(format!(
            "there is no {} record that {me} updated last; pass another --table",
            args.table
        )));
    };
    let class = raw(row, "sys_class_name");
    let table = match query::check_name("table", class) {
        Ok(()) => class.to_owned(),
        Err(_) => args.table,
    };
    let record = Record {
        table,
        sys_id: raw(row, "sys_id").to_owned(),
    };
    let shape = Shape {
        fields: args.fields,
        ..Shape::default()
    };
    super::composite(ctx, &record, &shape).await
}

#[derive(Args, Debug)]
pub struct LogsArgs {
    /// How far back: a number and s, m, h or d, for example 30s or 2h.
    #[arg(long, default_value = "15m")]
    since: String,
    /// The least severe level to show.
    #[arg(long, value_enum, default_value_t = Level::Warning)]
    level: Level,
    /// Only entries of your own sessions.
    #[arg(long)]
    mine: bool,
    /// Only messages that contain this text.
    #[arg(long, short = 'g', value_name = "TEXT")]
    grep: Option<String>,
    /// Only sources that contain this text.
    #[arg(long, value_name = "TEXT")]
    source: Option<String>,
    /// Most entries to return.
    #[arg(long, short = 'n', short_alias = 'l', default_value_t = 100)]
    limit: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Level {
    Trace,
    Debug,
    Info,
    Warning,
    Error,
    Fatal,
}

/// Level names, from code -2 upwards.
const LEVELS: [&str; 6] = ["trace", "debug", "info", "warning", "error", "fatal"];

impl Level {
    const fn code(self) -> i64 {
        self as i64 - 2
    }
}

pub async fn logs(ctx: &Ctx, args: LogsArgs) -> Result<()> {
    let window = parse_since(&args.since)?;
    let (unit, count) = match window {
        w if w % 3600 == 0 => ("hour", w / 3600),
        w if w % 60 == 0 => ("minute", w / 60),
        w => ("second", w),
    };
    let client = ctx.client().await?;
    let user = match args.mine {
        true => Some(caller(client).await?.user_name),
        false => None,
    };
    let floor = args.level.code();
    let levels: Vec<String> = (floor..=3).map(|l| l.to_string()).collect();
    // The server works out the window, so no time crosses a time zone.
    let mut filter = format!(
        "sys_created_onRELATIVEGT@{unit}@ago@{count}^levelIN{}",
        levels.join(",")
    );
    for (field, operator, value) in [
        ("sys_created_by", "=", &user),
        ("source", "LIKE", &args.source),
        ("message", "LIKE", &args.grep),
    ] {
        if let Some(value) = value {
            filter.push('^');
            filter.push_str(&query::term(field, operator, value));
        }
    }
    filter.push_str("^ORDERBYDESCsys_created_on");
    let params = TableQuery::new(filter)
        .fields(&[
            "sys_id",
            "sys_created_on",
            "level",
            "source",
            "message",
            "sys_created_by",
        ])
        .limit(args.limit)
        .display(DisplayValue::False);
    let rows = client.list("syslog", &params).await?.rows;
    // A minute of slack for clocks that differ.
    let oldest = utc_timestamp(now_unix().saturating_sub(window.saturating_add(60)));
    let contains = |row: &Value, field: &str, part: &Option<String>| {
        part.as_ref().is_none_or(|part| {
            raw(row, field)
                .to_lowercase()
                .contains(&part.to_lowercase())
        })
    };
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        let level: i64 = raw(row, "level").parse().unwrap_or(i64::MIN);
        let wrong = if raw(row, "sys_created_on") < oldest.as_str() {
            Some("sys_created_on")
        } else if level < floor {
            Some("level")
        } else if user
            .as_ref()
            .is_some_and(|u| !raw(row, "sys_created_by").eq_ignore_ascii_case(u))
        {
            Some("sys_created_by")
        } else if !contains(row, "source", &args.source) {
            Some("source")
        } else if !contains(row, "message", &args.grep) {
            Some("message")
        } else {
            None
        };
        if let Some(field) = wrong {
            return Err(dropped(field, "syslog"));
        }
        let name = usize::try_from(level + 2)
            .ok()
            .and_then(|at| LEVELS.get(at))
            .unwrap_or(&"unknown");
        out.push(json!({
            "time": raw(row, "sys_created_on"),
            "level": name,
            "source": stored(row, "source"),
            "message": stored(row, "message"),
            "created_by": stored(row, "sys_created_by"),
            "sys_id": stored(row, "sys_id"),
        }));
    }
    let cols = [
        col("TIME (UTC)", "time"),
        col("LEVEL", "level"),
        col("SOURCE", "source"),
        col("BY", "created_by"),
        flex("MESSAGE", "message"),
    ];
    ctx.out.list(out, &cols, json!({}))
}

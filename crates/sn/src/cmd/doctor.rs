//! `ping` and `doctor`.
//!
//! `doctor` checks the setup (configuration, profile, credential, network,
//! identity) and, with `--need-*`, roles, plugins and properties through one
//! GraphQL document over scripted schemas. Only `pass` and `skipped` are
//! good. A schema that the instance lacks leaves its checks `unavailable`.
//!
//! The role check says yes to any name for administrators, so a role passes
//! only when `sys_user_role` has exactly one row of that name.

use super::graphql;
use crate::ctx::Ctx;
use crate::output::{bold, cell, dim, paint};
use clap::Args;
use serde_json::{Map, Value, json};
use sn_core::config::Resolved;
use sn_core::table::{raw, text};
use sn_core::uisession::{SESSION_PATH, SessionInfo};
use sn_core::{Client, DisplayValue, Error, Result, TableQuery, refs};
use std::fmt::Write;
use std::time::Instant;

/// What the property getter answers for a property that is unset or empty.
const UNSET: &str = "sn:unset:6b1f0e2c";
/// A plugin that is active on any instance that answers GraphQL.
const CANARY: &str = "com.glide.graphql";
/// The top-level fields of the document; each can be missing.
const ROOTS: &[&str] = &[
    "now",
    "GlideRecord_Query",
    "snWorkflowStudio",
    "snDecisionTable",
];

#[derive(Args, Debug)]
pub struct DoctorArgs {
    /// Check the files on this computer only.
    #[arg(long, conflicts_with_all = ["need_role", "need_plugin", "need_property"])]
    offline: bool,
    /// Role you must have (repeatable).
    #[arg(long, value_delimiter = ',', value_name = "ROLE")]
    need_role: Vec<String>,
    /// Plugin that must be active, for example com.snc.change_management (repeatable).
    #[arg(long, value_delimiter = ',', value_name = "ID")]
    need_plugin: Vec<String>,
    /// System property that must be set, or have this value (repeatable).
    #[arg(long, value_name = "NAME[=VALUE]")]
    need_property: Vec<String>,
}

/// One line of the report.
fn check(kind: &str, name: &str, status: &str, detail: impl Into<String>) -> Value {
    json!({ "kind": kind, "name": name, "status": status, "detail": detail.into() })
}

pub async fn run(ctx: &Ctx, args: DoctorArgs) -> Result<()> {
    let needs = Needs::parse(&args)?;
    let mut checks = Vec::new();
    let mut report = json!({ "ok": false });
    if let Some(resolved) = local(ctx, &mut checks) {
        report["profile"] = resolved.name.clone().into();
        report["instance"] = resolved.profile.instance.clone().into();
        if args.offline {
            report["offline"] = true.into();
            for name in ["network", "identity", "table_api"] {
                checks.push(check("setup", name, "skipped", "--offline"));
            }
        } else {
            remote(ctx, &needs, &mut checks, &mut report).await?;
        }
    }
    let bad: Vec<String> = checks
        .iter()
        .filter(|c| c["status"] != "pass" && c["status"] != "skipped")
        .map(|c| {
            format!(
                "{} {} ({})",
                cell(c, "kind"),
                cell(c, "name"),
                cell(c, "status")
            )
        })
        .collect();
    let total = checks.len();
    report["ok"] = bad.is_empty().into();
    report["checks"] = checks.into();
    if bad.is_empty() || !ctx.out.agent {
        let tty = ctx.out.tty;
        ctx.out
            .item_with(report.clone(), json!({}), |report| report_text(report, tty))?;
    }
    match bad.is_empty() {
        true => Ok(()),
        false => Err(Error::Other {
            code: "check_failed",
            message: format!(
                "{} of {total} checks did not pass: {}",
                bad.len(),
                bad.join(", ")
            ),
            data: report,
        }),
    }
}

fn report_text(report: &Value, tty: bool) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{} {}",
        bold(&cell(report, "profile"), tty),
        cell(report, "instance")
    );
    for check in report["checks"].as_array().into_iter().flatten() {
        let status = cell(check, "status");
        let mark = match status.as_str() {
            "pass" => paint("32", "ok     ", tty),
            "skipped" => dim("skipped", tty),
            "fail" => paint("31;1", "FAIL   ", tty),
            _ => paint("33", "unknown", tty),
        };
        let kind = match check["kind"] == "setup" {
            true => String::new(),
            false => format!("{} ", cell(check, "kind")),
        };
        let name = format!("{kind}{}", cell(check, "name"));
        let _ = writeln!(out, "  {mark}  {name:<16}  {}", cell(check, "detail"));
    }
    out
}

/// The checks that need no network; `None` stops the others.
fn local(ctx: &Ctx, checks: &mut Vec<Value>) -> Option<Resolved> {
    match ctx.store.load() {
        Ok(_) => checks.push(check(
            "setup",
            "configuration",
            "pass",
            ctx.store.config_path().display().to_string(),
        )),
        Err(err) => {
            checks.push(check("setup", "configuration", "fail", err.to_string()));
            return None;
        }
    }
    let resolved = match ctx.resolve() {
        Ok(resolved) => resolved,
        Err(err) => {
            checks.push(check("setup", "profile", "fail", err.to_string()));
            return None;
        }
    };
    let from = match resolved.ephemeral {
        true => " (from the environment)",
        false => "",
    };
    checks.push(check(
        "setup",
        "profile",
        "pass",
        format!("{} on {}{from}", resolved.name, resolved.profile.instance),
    ));
    let profile = &resolved.profile;
    let credential = super::profile::has_credential(profile, &resolved.secrets);
    checks.push(match credential {
        true => check("setup", "credential", "pass", profile.auth.name()),
        false => check(
            "setup",
            "credential",
            "fail",
            format!(
                "the profile has no stored {}; run `sn auth login --profile {}`",
                profile.auth.name(),
                resolved.name
            ),
        ),
    });
    let writes = match profile.read_only || ctx.globals.read_only {
        true => "read-only: writes are refused",
        false => "writes are allowed",
    };
    checks.push(check("setup", "write_safety", "pass", writes));
    credential.then_some(resolved)
}

async fn remote(
    ctx: &Ctx,
    needs: &Needs,
    checks: &mut Vec<Value>,
    report: &mut Value,
) -> Result<()> {
    let client = ctx.client().await?;
    let started = Instant::now();
    let identity = async {
        let me = refs::me(client).await;
        (me, started.elapsed())
    };
    let params = TableQuery::default().fields(&["sys_id"]).limit(1);
    let (identity, incidents, answer) = tokio::join!(
        identity,
        client.list("incident", &params),
        ask(client, needs)
    );
    let (me, elapsed) = identity;
    report["latency_ms"] = u64::try_from(elapsed.as_millis())
        .unwrap_or(u64::MAX)
        .into();
    match me {
        Ok(user) => {
            checks.push(check("setup", "network", "pass", client.host()));
            let name = match text(&user, "user_name") {
                "" => text(&user, "sys_id"),
                name => name,
            };
            checks.push(check("setup", "identity", "pass", name));
            report["user"] = json!({ "user_name": user["user_name"], "sys_id": user["sys_id"] });
        }
        Err(err) if matches!(err, Error::Network(_)) || err.code() == "timeout" => {
            checks.push(check("setup", "network", "fail", err.to_string()));
            for name in ["identity", "table_api"] {
                checks.push(check(
                    "setup",
                    name,
                    "unavailable",
                    "there is no connection",
                ));
            }
            needs.unavailable("there is no connection", checks);
            return Ok(());
        }
        Err(err) => {
            checks.push(check("setup", "network", "pass", client.host()));
            checks.push(check("setup", "identity", "fail", err.to_string()));
        }
    }
    checks.push(match incidents {
        Ok(_) => check(
            "setup",
            "table_api",
            "pass",
            "the incident table is readable",
        ),
        Err(err) => check("setup", "table_api", "fail", err.to_string()),
    });
    match answer {
        Ok(answer) => needs.judge(&answer, checks, report),
        // Without --need the document only adds facts.
        Err(_) if needs.is_empty() => {}
        Err(err @ Error::Api(_)) if err.status().is_none() => return Err(err),
        Err(err) => needs.unavailable(&err.to_string(), checks),
    }
    Ok(())
}

struct Property {
    name: String,
    expected: Option<String>,
}

struct Needs {
    roles: Vec<String>,
    plugins: Vec<String>,
    properties: Vec<Property>,
}

/// The data of the document, and why some of its fields are missing.
struct Answer {
    data: Value,
    missing: Vec<(&'static str, String)>,
}

impl Answer {
    /// A part of the data, or why there is none. Access rules answer `null`.
    fn section(&self, root: &str, path: &str) -> std::result::Result<&Value, String> {
        if let Some((_, why)) = self.missing.iter().find(|(r, _)| *r == root) {
            return Err(format!("this instance has no {root}: {why}"));
        }
        self.data
            .pointer(path)
            .filter(|v| !v.is_null())
            .ok_or_else(|| format!("your user may not read {root}"))
    }
}

impl Needs {
    fn parse(args: &DoctorArgs) -> Result<Self> {
        // Role names go into an encoded query.
        let identifiers = |list: &[String], flag: &str| -> Result<Vec<String>> {
            let mut out: Vec<String> = Vec::new();
            for item in list {
                let item = item.trim();
                let valid = !item.is_empty()
                    && item
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'));
                if !valid {
                    return Err(Error::invalid(format!(
                        "you passed {flag} {item}; use letters, digits, `_`, `.` and `-` only"
                    )));
                }
                if !out.iter().any(|o| o == item) {
                    out.push(item.to_owned());
                }
            }
            Ok(out)
        };
        let properties = args
            .need_property
            .iter()
            .map(|item| {
                let (name, expected) = match item.split_once('=') {
                    Some((name, value)) => (name.trim(), Some(value.to_owned())),
                    None => (item.trim(), None),
                };
                if name.is_empty()
                    || name.contains(char::is_whitespace)
                    || expected.as_deref() == Some("")
                {
                    return Err(Error::invalid(format!(
                        "you passed --need-property {item}; pass NAME, or NAME=VALUE with a value (the instance does not tell empty from unset)"
                    )));
                }
                Ok(Property {
                    name: name.to_owned(),
                    expected,
                })
            })
            .collect::<Result<_>>()?;
        Ok(Self {
            roles: identifiers(&args.need_role, "--need-role")?,
            plugins: identifiers(&args.need_plugin, "--need-plugin")?,
            properties,
        })
    }

    fn is_empty(&self) -> bool {
        self.roles.is_empty() && self.plugins.is_empty() && self.properties.is_empty()
    }

    fn unavailable(&self, why: &str, checks: &mut Vec<Value>) {
        let roles = self.roles.iter().map(|n| ("role", n));
        let plugins = self.plugins.iter().map(|n| ("plugin", n));
        let properties = self.properties.iter().map(|p| ("property", &p.name));
        for (kind, name) in roles.chain(plugins).chain(properties) {
            checks.push(check(kind, name, "unavailable", why));
        }
    }

    /// The document without the missing fields, and its variables. A
    /// variable that is declared and not used is an error.
    fn document(&self, missing: &[(&'static str, String)]) -> Option<(String, Value)> {
        let has = |root: &str| !missing.iter().any(|(r, _)| *r == root);
        let mut variables = Map::new();
        let mut declared: Vec<String> = Vec::new();
        let mut body = String::new();
        if has("now") {
            body.push_str("now { sessionUser { admin: getMatchingRoles(roleNames: [\"admin\"])");
            if !self.roles.is_empty() {
                declared.push("$roles: [String]!".into());
                variables.insert("roles".into(), json!(self.roles));
                body.push_str(" held: getMatchingRoles(roleNames: $roles)");
            }
            body.push_str(" } }\n");
        }
        if has("GlideRecord_Query") && !self.roles.is_empty() {
            body.push_str("GlideRecord_Query {");
            for (i, role) in self.roles.iter().enumerate() {
                declared.push(format!("$role{i}: String"));
                variables.insert(format!("role{i}"), format!("name={role}").into());
                let _ = write!(
                    body,
                    " role{i}: sys_user_role(queryConditions: $role{i}, pagination: {{limit: 2}}) {{ _rowCount }}"
                );
            }
            body.push_str(" }\n");
        }
        if has("snWorkflowStudio") && !self.plugins.is_empty() {
            let _ = write!(
                body,
                "snWorkflowStudio {{ workflowStudio {{ canary: isPluginInstalled(plugin_name: \"{CANARY}\")"
            );
            for (i, plugin) in self.plugins.iter().enumerate() {
                declared.push(format!("$plugin{i}: String!"));
                variables.insert(format!("plugin{i}"), plugin.clone().into());
                let _ = write!(
                    body,
                    " plugin{i}: isPluginInstalled(plugin_name: $plugin{i})"
                );
            }
            body.push_str(" } }\n");
        }
        if has("snDecisionTable") {
            declared.push("$unset: String".into());
            variables.insert("unset".into(), UNSET.into());
            body.push_str("snDecisionTable { sysProperties { build: getSysPropertyValue(sys_property_name: \"glide.buildtag\", default_value: $unset)");
            for (i, property) in self.properties.iter().enumerate() {
                declared.push(format!("$property{i}: String!"));
                variables.insert(format!("property{i}"), property.name.clone().into());
                let _ = write!(
                    body,
                    " property{i}: getSysPropertyValue(sys_property_name: $property{i}, default_value: $unset)"
                );
            }
            body.push_str(" } }\n");
        }
        if body.is_empty() {
            return None;
        }
        let head = match declared.is_empty() {
            true => "query".to_owned(),
            false => format!("query ({})", declared.join(", ")),
        };
        Some((format!("{head} {{\n{body}}}"), variables.into()))
    }

    fn judge(&self, answer: &Answer, checks: &mut Vec<Value>, report: &mut Value) {
        let session = answer.section("now", "/now/sessionUser");
        let admin = session
            .as_ref()
            .ok()
            .and_then(|s| s["admin"].as_array())
            .map(|a| a.iter().any(|r| r == "admin"));
        report["admin"] = admin.into();
        let records = match self.roles.is_empty() {
            true => Ok(&Value::Null),
            false => answer.section("GlideRecord_Query", "/GlideRecord_Query"),
        };
        for (i, role) in self.roles.iter().enumerate() {
            checks.push(role_check(i, role, &session, &records, admin));
        }
        let plugins = answer
            .section("snWorkflowStudio", "/snWorkflowStudio/workflowStudio")
            .and_then(|studio| match studio["canary"].as_bool() {
                Some(true) => Ok(studio),
                _ => Err(format!(
                    "the plugin check does not know {CANARY}, so its answers mean nothing"
                )),
            });
        for (i, plugin) in self.plugins.iter().enumerate() {
            let found = plugins
                .as_ref()
                .map(|studio| studio[format!("plugin{i}")].as_bool());
            checks.push(match found {
                Err(why) => check("plugin", plugin, "unavailable", why.clone()),
                Ok(Some(true)) => check("plugin", plugin, "pass", "active"),
                Ok(Some(false)) => check("plugin", plugin, "fail", "not installed, or not active"),
                Ok(None) => check("plugin", plugin, "unavailable", "the check gave no answer"),
            });
        }
        let properties = answer
            .section("snDecisionTable", "/snDecisionTable/sysProperties")
            .and_then(|p| match p["build"].is_string() {
                true => Ok(p),
                false => Err("your user may not read properties".to_owned()),
            });
        if let Ok(values) = &properties
            && values["build"] != UNSET
        {
            report["build_tag"] = values["build"].clone();
        }
        for (i, property) in self.properties.iter().enumerate() {
            let value = properties
                .as_ref()
                .map(|p| p[format!("property{i}")].as_str());
            let name = &property.name;
            let mut line = match value {
                Err(why) => check("property", name, "unavailable", why.clone()),
                // Password properties have no readable value.
                Ok(None) => check("property", name, "unavailable", "its value is not readable"),
                Ok(Some(UNSET)) => check("property", name, "fail", "not set, or empty"),
                Ok(Some(value)) => match &property.expected {
                    Some(expected) if expected != value => check(
                        "property",
                        name,
                        "fail",
                        format!("the value is `{value}`, not `{expected}`"),
                    ),
                    _ => check("property", name, "pass", value),
                },
            };
            if let Ok(Some(value)) = value
                && value != UNSET
            {
                line["value"] = value.into();
            }
            checks.push(line);
        }
        let available = |result: bool| json!({ "available": result });
        report["capabilities"] = json!({
            "roles": available(session.is_ok() && records.is_ok()),
            "plugins": available(self.plugins.is_empty() || plugins.is_ok()),
            "properties": available(properties.is_ok()),
        });
    }
}

type Section<'a> = std::result::Result<&'a Value, String>;

fn role_check(
    index: usize,
    role: &str,
    session: &Section<'_>,
    records: &Section<'_>,
    admin: Option<bool>,
) -> Value {
    let (session, records) = match (session, records) {
        (Ok(session), Ok(records)) => (session, records),
        (Err(why), _) | (_, Err(why)) => {
            return check("role", role, "unavailable", why.clone());
        }
    };
    let Some(held) = session["held"].as_array() else {
        return check("role", role, "unavailable", "the check gave no answer");
    };
    let held = held.iter().any(|r| r == role);
    match records[format!("role{index}")]["_rowCount"].as_u64() {
        None => check("role", role, "unavailable", "sys_user_role gave no count"),
        Some(0) => check("role", role, "fail", "this instance has no such role"),
        Some(1) if held && admin == Some(true) && role != "admin" => {
            check("role", role, "pass", "through admin")
        }
        Some(1) if held => check("role", role, "pass", "you have it"),
        Some(1) => check("role", role, "fail", "you do not have it"),
        // More than one means that the filter was ignored.
        Some(_) => check(
            "role",
            role,
            "unavailable",
            "the instance ignored the filter for the role",
        ),
    }
}

/// Sends the document, and again without the fields that the instance does
/// not have.
async fn ask(client: &Client, needs: &Needs) -> Result<Answer> {
    let mut missing: Vec<(&'static str, String)> = Vec::new();
    loop {
        let Some((document, variables)) = needs.document(&missing) else {
            return Ok(Answer {
                data: Value::Null,
                missing,
            });
        };
        let mut answer = graphql::ask(client, &document, variables).await?;
        let errors = graphql::errors(&answer);
        if errors.is_empty() {
            return Ok(Answer {
                data: answer.get_mut("data").map(Value::take).unwrap_or_default(),
                missing,
            });
        }
        for error in errors {
            let root = error_root(error).and_then(|r| ROOTS.iter().find(|known| **known == r));
            match root {
                Some(root) if !missing.iter().any(|(r, _)| r == root) => {
                    let why = error["message"].as_str().unwrap_or("no reason given");
                    missing.push((root, why.to_owned()));
                }
                // Already dropped in this round, or a real failure.
                Some(root) if document.contains(&format!("{root} {{")) => {}
                _ => return Err(graphql::refusal(errors)),
            }
        }
    }
}

/// The top-level field of an error: in its path, or after `@[` in the text.
fn error_root(error: &Value) -> Option<&str> {
    if let Some(root) = error["path"][0].as_str() {
        return Some(root);
    }
    let (_, rest) = error["message"].as_str()?.split_once("@[")?;
    rest.split([']', '/']).next()
}

pub async fn ping(ctx: &Ctx) -> Result<()> {
    let client = ctx.client().await?;
    let started = Instant::now();
    let session = async {
        let answer = client.get(SESSION_PATH).await.map(SessionInfo::read);
        (answer, started.elapsed())
    };
    let params = TableQuery::new("name=glide.buildname^ORname=glide.buildtag")
        .fields(&["name", "value"])
        .limit(2)
        .display(DisplayValue::False);
    let ((session, elapsed), build) = tokio::join!(session, client.list("sys_properties", &params));
    let build = build.map(|page| page.rows).unwrap_or_default();
    let property = |name: &str| {
        build
            .iter()
            .find(|row| raw(row, "name") == name)
            .map(|row| raw(row, "value").to_owned())
    };
    let mut data = json!({
        "ok": true, "profile": client.profile_name(), "instance": client.base(),
        "username": null,
        "latency_ms": u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
        "build_name": property("glide.buildname"), "build_tag": property("glide.buildtag"),
        "identity_source": null, "user_sys_id": null, "user_display_name": null,
        "admin": null, "can_impersonate": null, "impersonating": null, "original_user": null,
    });
    // The answer also has a session token, which is never shown.
    match session {
        Ok(SessionInfo {
            current_user: Some(user),
            original_user,
            can_impersonate,
            admin,
            ..
        }) => {
            data["impersonating"] = original_user.as_ref().map(|o| *o != user).into();
            data["original_user"] = original_user.into();
            data["username"] = user.into();
            data["identity_source"] = "session".into();
            data["admin"] = admin.into();
            data["can_impersonate"] = can_impersonate.into();
        }
        Err(err) if err.status() != Some(404) => return Err(err),
        // Older instances: another endpoint proves the credential.
        _ => match refs::me(client).await {
            Ok(user) => {
                data["username"] = user["user_name"].clone();
                data["user_sys_id"] = user["sys_id"].clone();
                data["user_display_name"] = user["name"].clone();
                data["identity_source"] = "sys_user".into();
            }
            Err(Error::NotFound(_)) => {
                data["username"] = client.username().await.into();
                data["identity_source"] = "profile".into();
            }
            Err(err) => return Err(err),
        },
    }
    let human = format!(
        "{} answered in {} ms: you are {}{}{}",
        client.host(),
        data["latency_ms"],
        cell(&data, "username"),
        match cell(&data, "original_user") {
            original if data["impersonating"] == true => format!(" (signed in as {original})"),
            _ => String::new(),
        },
        match cell(&data, "build_tag") {
            tag if tag.is_empty() => String::new(),
            tag => format!(", build {tag}"),
        }
    );
    ctx.out.done(data, &human)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn needs(roles: &[&str], plugins: &[&str], properties: &[&str]) -> Result<Needs> {
        let owned = |list: &[&str]| list.iter().map(|s| (*s).to_owned()).collect();
        Needs::parse(&DoctorArgs {
            offline: false,
            need_role: owned(roles),
            need_plugin: owned(plugins),
            need_property: owned(properties),
        })
    }

    fn judged(needs: &Needs, data: Value) -> Vec<(String, String)> {
        let mut checks = Vec::new();
        let answer = Answer {
            data,
            missing: Vec::new(),
        };
        needs.judge(&answer, &mut checks, &mut json!({}));
        checks
            .iter()
            .map(|c| (cell(c, "name"), cell(c, "status")))
            .collect()
    }

    #[test]
    fn names_that_would_change_the_query_are_refused() {
        assert!(needs(&["itil^active=true"], &[], &[]).is_err());
        assert!(needs(&[], &["a b"], &[]).is_err());
        assert!(needs(&[], &[], &["=x"]).is_err());
        assert!(needs(&[], &[], &["name="]).is_err());
        assert!(
            needs(&["itil", "itil"], &["com.a"], &["a.b=c=d"])
                .expect("needs")
                .roles
                .len()
                == 1
        );
    }

    #[test]
    fn administrators_do_not_pass_roles_that_do_not_exist() {
        let needs = needs(&["itil", "nonexistent_xyz", "many"], &[], &[]).expect("needs");
        let data = json!({
            "now": { "sessionUser": { "admin": ["admin"], "held": ["itil", "nonexistent_xyz", "many"] } },
            "GlideRecord_Query": { "role0": { "_rowCount": 1 }, "role1": { "_rowCount": 0 }, "role2": { "_rowCount": 981 } },
        });
        let status: Vec<String> = judged(&needs, data).into_iter().map(|(_, s)| s).collect();
        assert_eq!(status, ["pass", "fail", "unavailable"]);
    }

    #[test]
    fn properties_tell_unset_from_unreadable() {
        let needs = needs(&[], &[], &["a", "b=1", "c", "d=x"]).expect("needs");
        let data = json!({ "snDecisionTable": { "sysProperties": {
            "build": "tag", "property0": UNSET, "property1": "2", "property2": null, "property3": "x",
        } } });
        let status: Vec<String> = judged(&needs, data).into_iter().map(|(_, s)| s).collect();
        assert_eq!(status, ["fail", "fail", "unavailable", "pass"]);
    }

    #[test]
    fn errors_name_their_field() {
        assert_eq!(
            error_root(&json!({ "path": ["snDecisionTable", "x"] })),
            Some("snDecisionTable")
        );
        let message = "Validation error (FieldUndefined@[snWorkflowStudio/workflowStudio]) : Field";
        assert_eq!(
            error_root(&json!({ "message": message })),
            Some("snWorkflowStudio")
        );
        assert_eq!(error_root(&json!({ "message": "boom" })), None);
    }
}

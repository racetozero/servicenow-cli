//! Finding the record a person means: a number, `table:number`, a sys_id, or
//! a link copied from the web interface.

use crate::client::Client;
use crate::error::{Choice, Error, Result};
use crate::table::{DisplayValue, TableQuery, raw, text};
use crate::{is_sys_id, percent, query};
use serde_json::Value;

/// Number prefixes that need no lookup.
pub const PREFIXES: &[(&str, &str)] = &[
    ("CHG", "change_request"),
    ("CTASK", "change_task"),
    ("INC", "incident"),
    ("KB", "kb_knowledge"),
    ("PRB", "problem"),
    ("PTASK", "problem_task"),
    ("REQ", "sc_request"),
    ("RITM", "sc_req_item"),
    ("SCTASK", "sc_task"),
    ("SIR", "sn_si_incident"),
    ("CS", "sn_customerservice_case"),
    ("HRC", "sn_hr_core_case"),
    ("STRY", "rm_story"),
];

/// A record without its key looked up yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reference {
    /// `None` for a bare number with an unknown prefix.
    pub table: Option<String>,
    /// A sys_id or a number.
    pub id: String,
}

/// A record on the instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub table: String,
    pub sys_id: String,
}

/// The letters in front of a record number such as `INC0010001`.
pub fn prefix(number: &str) -> Option<String> {
    let letters = number.trim_end_matches(|c: char| c.is_ascii_digit());
    let digits = number.len() - letters.len();
    let valid = (2..=10).contains(&letters.len())
        && digits >= 3
        && letters.bytes().all(|b| b.is_ascii_alphabetic());
    valid.then(|| letters.to_ascii_uppercase())
}

/// Parses `table:id`, a number, or a form URL of the instance.
pub fn parse(client: &Client, text: &str) -> Result<Reference> {
    let token = text.trim();
    if token.starts_with("https://") || token.starts_with("http://") {
        return parse_url(client, token);
    }
    if let Some((table, id)) = token.split_once(':') {
        query::check_name("table", table)?;
        if id.is_empty() {
            return Err(Error::invalid(format!(
                "you passed `{token}` without a record after the colon; pass {table}:NUMBER or {table}:SYS_ID"
            )));
        }
        return Ok(Reference {
            table: Some(table.to_owned()),
            id: id.to_owned(),
        });
    }
    if let Some(prefix) = prefix(token) {
        let table = PREFIXES
            .iter()
            .find(|(p, _)| *p == prefix)
            .map(|(_, t)| (*t).to_owned());
        return Ok(Reference {
            table,
            id: token.to_owned(),
        });
    }
    Err(Error::invalid(format!(
        "you passed `{token}`, which does not name a record; pass a number (INC0010001), table:number, table:sys_id, or a link to the record"
    )))
}

fn parse_url(client: &Client, text: &str) -> Result<Reference> {
    let bad = |why: &str| {
        Error::invalid(format!(
            "you passed the link {text}, which {why}; copy the link of a record on {}",
            client.host()
        ))
    };
    let url = url::Url::parse(text).map_err(|_| bad("is not valid"))?;
    let base = url::Url::parse(client.base()).map_err(|_| bad("cannot be compared"))?;
    if url.origin() != base.origin() {
        return Err(bad("is on another host"));
    }
    // Classic pages sit in a frame: nav_to.do?uri=incident.do?sys_id=...
    let mut page = url.clone();
    if let Some((_, uri)) = url.query_pairs().find(|(n, _)| n == "uri") {
        page = base
            .join(uri.trim_start_matches('/'))
            .map_err(|_| bad("has a broken target"))?;
    } else if let Some(target) = url.path().split("/params/target/").nth(1) {
        page = base
            .join(&percent::decode(target))
            .map_err(|_| bad("has a broken target"))?;
    }
    let segments: Vec<&str> = page.path().split('/').filter(|s| !s.is_empty()).collect();
    if let Some(at) = segments.iter().position(|s| *s == "record")
        && let (Some(table), Some(id)) = (segments.get(at + 1), segments.get(at + 2))
    {
        return Ok(Reference {
            table: Some((*table).to_owned()),
            id: (*id).to_owned(),
        });
    }
    let table = segments
        .last()
        .and_then(|s| s.strip_suffix(".do"))
        .ok_or_else(|| bad("is not a record page"))?;
    let param = |name: &str| {
        page.query_pairs()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.into_owned())
    };
    let id = param("sys_id")
        .or_else(|| {
            param("sysparm_query")?
                .strip_prefix("number=")
                .map(str::to_owned)
        })
        .ok_or_else(|| bad("has no sys_id"))?;
    query::check_name("table", table)?;
    Ok(Reference {
        table: Some(table.to_owned()),
        id,
    })
}

/// The record of `table` and `id`, where `id` may be a number, or of one
/// combined reference when there is no `id`.
pub async fn record(client: &Client, table: &str, id: Option<&str>) -> Result<Record> {
    let reference = match id {
        None => parse(client, table)?,
        Some(id) if id.contains("://") => {
            let reference = parse(client, id)?;
            if reference.table.as_deref() != Some(table) {
                return Err(Error::invalid(format!(
                    "you passed table `{table}` and a link to a record of `{}`; pass the matching table",
                    reference.table.unwrap_or_default()
                )));
            }
            reference
        }
        Some(id) => {
            query::check_name("table", table)?;
            Reference {
                table: Some(table.to_owned()),
                id: id.to_owned(),
            }
        }
    };
    locate(client, reference).await
}

/// Looks up the sys_id, and the table of an unknown number prefix.
pub async fn locate(client: &Client, reference: Reference) -> Result<Record> {
    let table = match reference.table {
        Some(table) => table,
        None => table_of_prefix(client, &reference.id).await?,
    };
    if is_sys_id(&reference.id) {
        return Ok(Record {
            table,
            sys_id: reference.id.to_ascii_lowercase(),
        });
    }
    let params = TableQuery::new(query::term("number", "=", &reference.id))
        .fields(&["sys_id", "number"])
        .limit(2)
        .display(DisplayValue::False);
    let rows = client.list(&table, &params).await?.rows;
    // The instance drops a term whose field does not exist and answers with
    // any record, so the number is checked.
    let mut matching = rows
        .iter()
        .filter(|row| raw(row, "number").eq_ignore_ascii_case(&reference.id));
    match (matching.next(), matching.next()) {
        (Some(row), None) => Ok(Record {
            table,
            sys_id: raw(row, "sys_id").to_owned(),
        }),
        (Some(_), Some(_)) => Err(Error::Other {
            code: "conflict",
            message: format!(
                "more than one record of `{table}` has the number {}; pass {table}:SYS_ID",
                reference.id
            ),
            data: Value::Null,
        }),
        (None, _) if rows.is_empty() => Err(Error::NotFound(format!(
            "there is no record {} in `{table}` that you can read; check the number and your access",
            reference.id
        ))),
        (None, _) => Err(Error::invalid(format!(
            "`{table}` cannot be searched by number; pass {table}:SYS_ID instead of {table}:{}",
            reference.id
        ))),
    }
}

async fn table_of_prefix(client: &Client, number: &str) -> Result<String> {
    let prefix = prefix(number).unwrap_or_default();
    let params = TableQuery::new(query::term("prefix", "=", &prefix))
        .fields(&["prefix", "category"])
        .limit(10)
        .display(DisplayValue::All);
    let rows = client.list("sys_number", &params).await?.rows;
    let mut tables: Vec<String> = rows
        .iter()
        .filter(|r| raw(r, "prefix").eq_ignore_ascii_case(&prefix))
        .map(|r| raw(r, "category").to_owned())
        .filter(|t| !t.is_empty())
        .collect();
    tables.sort();
    tables.dedup();
    match tables.as_slice() {
        [table] => Ok(table.clone()),
        [] => Err(Error::invalid(format!(
            "the prefix {prefix} of {number} is not known on {}; pass table:{number}",
            client.host()
        ))),
        _ => Err(Error::Input {
            arg: "reference".into(),
            message: format!("several tables use the prefix {prefix}; pass table:{number}"),
            choices: tables
                .iter()
                .map(|t| Choice::new(format!("{t}:{number}"), t))
                .collect(),
        }),
    }
}

/// What a person can name instead of a sys_id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Person {
    User,
    Group,
}

/// Resolves `@me`, a sys_id, a user name, an e-mail address, or a full name
/// (users), or a group name (groups) to a sys_id.
pub async fn person(client: &Client, kind: Person, value: &str) -> Result<String> {
    let value = value.trim();
    if is_sys_id(value) {
        return Ok(value.to_ascii_lowercase());
    }
    if kind == Person::User && value.eq_ignore_ascii_case("@me") {
        let current = current_user(client).await?;
        return match current.get("user_sys_id").and_then(Value::as_str) {
            Some(id) if is_sys_id(id) => Ok(id.to_ascii_lowercase()),
            _ => Ok(text(&identity(client, current).await?, "sys_id").to_owned()),
        };
    }
    let (table, what, fields): (_, _, &[&str]) = match kind {
        Person::User => ("sys_user", "user", &["user_name", "email", "name"]),
        Person::Group => ("sys_user_group", "group", &["name"]),
    };
    let filter = fields
        .iter()
        .map(|f| query::term(f, "=", value))
        .collect::<Vec<_>>()
        .join("^OR");
    let mut wanted = vec!["sys_id"];
    wanted.extend(fields);
    let params = TableQuery::new(filter)
        .fields(&wanted)
        .limit(10)
        .display(DisplayValue::False);
    let rows = client.list(table, &params).await?.rows;
    let matches: Vec<&Value> = rows
        .iter()
        .filter(|row| {
            fields
                .iter()
                .any(|f| raw(row, f).eq_ignore_ascii_case(value))
        })
        .collect();
    match matches.as_slice() {
        [row] => Ok(raw(row, "sys_id").to_owned()),
        [] => Err(Error::NotFound(format!(
            "there is no {what} `{value}`; pass a {}, or a sys_id",
            if kind == Person::User {
                "user name, e-mail address, full name, @me"
            } else {
                "group name"
            }
        ))),
        many => Err(Error::Input {
            arg: what.into(),
            message: format!("several records match the {what} `{value}`; pass the sys_id of one"),
            choices: many
                .iter()
                .map(|row| {
                    let label: Vec<&str> = fields
                        .iter()
                        .map(|f| raw(row, f))
                        .filter(|t| !t.is_empty())
                        .collect();
                    Choice::new(raw(row, "sys_id"), label.join(", "))
                })
                .collect(),
        }),
    }
}

/// Who the instance says is calling; `null` where it has no such endpoint.
async fn current_user(client: &Client) -> Result<Value> {
    // This endpoint names the caller without a scripted query term, which
    // the instance could drop and answer with another user.
    match client.get("/api/now/ui/user/current_user").await {
        Err(Error::Api(failure)) if matches!(failure.status, Some(400 | 403 | 404)) => {
            Ok(Value::Null)
        }
        answer => answer,
    }
}

/// The signed-in user.
pub struct Caller {
    pub sys_id: String,
    pub user_name: String,
}

/// Who the instance says you are; never a guess.
pub async fn caller(client: &Client) -> Result<Caller> {
    let read = |user: &Value, id: &str| Caller {
        sys_id: text(user, id).to_owned(),
        user_name: text(user, "user_name").to_owned(),
    };
    let current = current_user(client).await?;
    let found = read(&current, "user_sys_id");
    if is_sys_id(&found.sys_id) && !found.user_name.is_empty() {
        return Ok(found);
    }
    Ok(read(&identity(client, current).await?, "sys_id"))
}

/// The signed-in user: `sys_id`, `user_name`, `name`, `email`.
pub async fn me(client: &Client) -> Result<Value> {
    identity(client, current_user(client).await?).await
}

async fn identity(client: &Client, current: Value) -> Result<Value> {
    let user_id = current
        .get("user_sys_id")
        .or_else(|| current.get("sys_id"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let fields = [
        "sys_id",
        "user_name",
        "name",
        "email",
        "title",
        "time_zone",
        "active",
    ];
    let params = TableQuery::default().fields(&fields);
    if is_sys_id(user_id) {
        return match client.record("sys_user", user_id, &params).await {
            Ok(record) => Ok(record),
            // The user may not read sys_user; the endpoint still named them.
            Err(Error::Api(_)) => Ok(serde_json::json!({
                "sys_id": user_id,
                "user_name": current.get("user_name"),
                "name": current.get("user_display_name"),
            })),
            Err(err) => Err(err),
        };
    }
    let params = TableQuery {
        query: "sys_id=javascript:gs.getUserID()".into(),
        limit: Some(2),
        ..params
    };
    let mut rows = client.list("sys_user", &params).await?.rows;
    match rows.len() {
        1 => Ok(rows.remove(0)),
        0 => Err(Error::NotFound("the instance did not say who you are, and you cannot read your sys_user record; ask an administrator for read access".into())),
        _ => Err(Error::api("the instance ignored the filter for the current user", Value::Null)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::Options;
    use crate::config::{Profile, Resolved, Secrets};

    fn client() -> Client {
        let profile = Profile {
            instance: "https://acme.service-now.com".into(),
            ..Profile::default()
        };
        let resolved = Resolved {
            name: "t".into(),
            profile,
            secrets: Secrets::default(),
            ephemeral: true,
        };
        Client::new(resolved, None, Options::default()).expect("client")
    }

    #[test]
    fn references_parse() {
        let c = client();
        let id = "a".repeat(32);
        let table = |text: &str| parse(&c, text).map(|r| (r.table.unwrap_or_default(), r.id));
        assert_eq!(
            table("INC0010001").expect("n"),
            ("incident".into(), "INC0010001".into())
        );
        assert_eq!(
            table("incident:INC1").expect("t"),
            ("incident".into(), "INC1".into())
        );
        assert_eq!(
            table("ABCD0001").expect("u"),
            (String::new(), "ABCD0001".into())
        );
        for url in [
            format!("https://acme.service-now.com/incident.do?sys_id={id}"),
            format!("https://acme.service-now.com/nav_to.do?uri=incident.do%3Fsys_id%3D{id}"),
            format!(
                "https://acme.service-now.com/now/nav/ui/classic/params/target/incident.do%3Fsys_id%3D{id}"
            ),
            format!("https://acme.service-now.com/now/sow/record/incident/{id}"),
        ] {
            assert_eq!(table(&url).expect(&url), ("incident".into(), id.clone()));
        }
        assert!(table("https://evil.com/incident.do?sys_id=1").is_err());
        assert!(table("hello world").is_err());
        assert!(table("incident:").is_err());
    }
}

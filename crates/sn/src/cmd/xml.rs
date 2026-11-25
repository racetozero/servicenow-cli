//! Records as XML: what "Export XML" and "Import XML" of the web interface
//! do. These pages are not REST endpoints:
//!
//! - The export (`TABLE.do?UNL`) refuses bearer tokens and API keys. A
//!   session opened with them works after it has loaded a page.
//! - It drops a query term whose column does not exist and then exports the
//!   whole table, so the columns are checked first.
//! - The import answers an empty 200 whatever happened.

use super::attachment;
use crate::ctx::Ctx;
use clap::Subcommand;
use serde_json::{Map, Value, json};
use sn_core::client::{Body, Method, Request};
use sn_core::config::AuthKind;
use sn_core::uisession::UiSession;
use sn_core::{Client, Error, Result, query};
use std::collections::HashMap;
use std::path::PathBuf;

const UPLOAD: &str = "/sys_upload.do";
/// Must not be empty; the file itself names its tables.
const TARGET: &str = "servicenow_xml_import";

#[derive(Subcommand, Debug)]
pub enum XmlCommand {
    /// Export the records that a query matches, with their journal and attachments.
    Export {
        /// Table. Records of a parent table keep their own class.
        table: String,
        /// Encoded query, for example number=INC0010001.
        query: String,
        /// Most records to export (the instance has its own limit, 10000 by default).
        #[arg(long, short = 'n', short_alias = 'l', value_parser = clap::value_parser!(u64).range(1..))]
        limit: Option<u64>,
        /// Write to this file instead of stdout.
        #[arg(long, value_name = "FILE")]
        out: Option<PathBuf>,
        /// Replace a file that exists.
        #[arg(long, requires = "out")]
        force: bool,
    },
    /// Import an export file. It overwrites and deletes records without business rules (administrators).
    Import {
        /// XML file, or - for stdin.
        file: String,
    },
}

pub async fn run(ctx: &Ctx, command: XmlCommand) -> Result<()> {
    match command {
        XmlCommand::Export {
            table,
            query,
            limit,
            out,
            force,
        } => export(ctx, &table, query.trim(), limit, out, force).await,
        XmlCommand::Import { file } => import(ctx, &file).await,
    }
}

async fn export(
    ctx: &Ctx,
    table: &str,
    filter: &str,
    limit: Option<u64>,
    out: Option<PathBuf>,
    force: bool,
) -> Result<()> {
    query::check_name("table", table)?;
    if filter.is_empty() {
        return Err(Error::invalid(
            "you passed an empty query, which would export the whole table; pass a query such as active=true",
        ));
    }
    if let Some(dest) = &out {
        attachment::check_destination(dest, force)?;
    }
    let client = ctx.client().await?;
    check_columns(client, table, filter).await?;
    let mut request = Request::new(Method::GET, format!("/{table}.do"))
        .query("UNL", "")
        .query("sysparm_query", filter)
        .query_opt("sysparm_record_count", limit.map(|n| n.to_string()));
    match client.auth_kind() {
        AuthKind::Oauth | AuthKind::Token | AuthKind::ApiKey => {
            let mut session = UiSession::mint(client).await?;
            session
                .send(client, Request::new(Method::GET, "/home.do"))
                .await?;
            request = session.apply(request);
        }
        AuthKind::Cookie | AuthKind::Session => {
            client.send(Request::new(Method::GET, "/home.do")).await?;
        }
        AuthKind::Basic | AuthKind::Certificate => {}
    }
    let mut response = attachment::open(ctx, request).await?;
    // A sign-in page is not an export, whatever its status.
    let is_page = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("text/html"));
    if is_page || response.status().as_u16() >= 300 {
        return Err(Error::Auth(format!(
            "{} answered the export with a web page; exporting XML needs the admin role and a session, so check your roles or sign in again",
            client.host()
        )));
    }
    if let Some(dest) = out {
        let bytes = attachment::save(ctx, &mut response, &dest, force).await?;
        return ctx.out.done(
            json!({ "path": dest, "bytes": bytes }),
            &format!(
                "Wrote {} ({})",
                dest.display(),
                attachment::size(&bytes.to_string())
            ),
        );
    }
    if ctx.out.agent {
        let mut data = Vec::new();
        attachment::copy(ctx, &mut response, &mut data).await?;
        return ctx.out.text(&String::from_utf8_lossy(&data));
    }
    attachment::copy(ctx, &mut response, &mut std::io::stdout().lock())
        .await
        .map(|_| ())
}

/// Each column of the query must exist, through references too.
async fn check_columns(client: &Client, table: &str, filter: &str) -> Result<()> {
    let mut known: HashMap<String, Map<String, Value>> = HashMap::new();
    for field in query_fields(filter) {
        let mut on = table.to_owned();
        let mut parts = field.split('.').peekable();
        while let Some(part) = parts.next() {
            let columns = columns(client, &mut known, &on).await?;
            let column = columns.get(part).ok_or_else(|| {
                Error::invalid(format!(
                    "you passed `{field}` in the query, but `{on}` has no column `{part}`; the instance would ignore it and export more than you asked for, so check the name with `sn schema columns {on}`"
                ))
            })?;
            if parts.peek().is_some() {
                let next = column["reference"].as_str().filter(|t| !t.is_empty());
                on = next
                    .ok_or_else(|| {
                        Error::invalid(format!(
                            "you passed `{field}` in the query, but `{on}.{part}` is not a reference, so it has no fields; end the name there"
                        ))
                    })?
                    .to_owned();
            }
        }
    }
    // A query without columns to check still needs a table that exists.
    columns(client, &mut known, table).await.map(|_| ())
}

async fn columns<'a>(
    client: &Client,
    known: &'a mut HashMap<String, Map<String, Value>>,
    table: &str,
) -> Result<&'a Map<String, Value>> {
    if !known.contains_key(table) {
        let mut meta = client.get(format!("/api/now/ui/meta/{table}")).await?;
        let columns = match meta.get_mut("columns").map(Value::take) {
            Some(Value::Object(columns)) if !columns.is_empty() => columns,
            _ => {
                return Err(Error::NotFound(format!(
                    "there is no table `{table}`; check the name with `sn schema tables`"
                )));
            }
        };
        known.insert(table.to_owned(), columns);
    }
    Ok(&known[table])
}

/// The columns that an encoded query names. Terms without one (text search,
/// EQ, variables, related lists) cannot be checked.
fn query_fields(filter: &str) -> Vec<&str> {
    // The terms of a related list are about another table.
    if filter.contains("RLQUERY") {
        return Vec::new();
    }
    let mut fields = Vec::new();
    for term in query::terms(filter) {
        let term = ["ORDERBYDESC", "ORDERBY", "GROUPBY", "NQ", "OR"]
            .iter()
            .find_map(|keyword| term.strip_prefix(keyword))
            .unwrap_or(term);
        let stop = term
            .find(|c: char| !(c.is_ascii_lowercase() || c.is_ascii_digit() || "_.".contains(c)))
            .unwrap_or(term.len());
        let field = &term[..stop];
        let checkable = field.starts_with(|c: char| c.is_ascii_lowercase())
            && !field.starts_with("variables.")
            && !field.starts_with("ref_");
        if checkable {
            fields.push(field);
        }
    }
    fields
}

async fn import(ctx: &Ctx, file: &str) -> Result<()> {
    let failed = |err: std::io::Error| {
        Error::invalid(format!(
            "cannot read {file} ({err}); pass an XML file that exists, or - for stdin"
        ))
    };
    let (name, data) = match file {
        "-" => {
            let mut data = Vec::new();
            std::io::Read::read_to_end(&mut std::io::stdin().lock(), &mut data).map_err(failed)?;
            ("unload.xml".to_owned(), data)
        }
        path => {
            let data = std::fs::read(path).map_err(failed)?;
            let name = std::path::Path::new(path)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned());
            (name.unwrap_or_else(|| "unload.xml".into()), data)
        }
    };
    if data.iter().all(u8::is_ascii_whitespace) {
        return Err(Error::invalid(format!(
            "{file} is empty; pass a file that `sn xml export` wrote"
        )));
    }
    ctx.confirm(&format!(
        "import {file} into the instance; records in it are overwritten or deleted, without business rules"
    ))?;
    let client = ctx.client().await?;
    let session = UiSession::open(client).await?;
    if session.user_token.is_empty() {
        return Err(Error::Auth(
            "the instance gave no session token, and an import needs one; sign in again with `sn auth login`".into(),
        ));
    }
    let bytes = data.len();
    let body = Body::Multipart {
        fields: vec![
            ("sysparm_ck".into(), session.user_token.clone()),
            ("sysparm_target".into(), TARGET.into()),
        ],
        file_field: "attachFile".into(),
        file_name: name.clone(),
        data: data.into(),
        content_type: "text/xml".into(),
    };
    let request = session.attach(client, Request::new(Method::POST, UPLOAD).body(body));
    if client.send(request).await?.status >= 300 {
        return Err(Error::Auth(format!(
            "{} answered the import of {file} with a redirect, so nothing was loaded; an import needs a session, so sign in again with `sn auth login`",
            client.host()
        )));
    }
    let data = json!({ "uploaded": name, "bytes": bytes, "confirmed": false });
    let text = format!(
        "Uploaded {name} ({})\nThe instance does not report what it loaded, and ignores files from users without the admin role; check the records",
        attachment::size(&bytes.to_string())
    );
    ctx.out.done(data, &text)
}

#[cfg(test)]
mod tests {
    use super::query_fields;

    #[test]
    fn the_columns_of_a_query() {
        assert_eq!(
            query_fields(
                "active=true^ORpriority=1^NQcaller_id.nameLIKEx^ORDERBYDESCsys_updated_on"
            ),
            ["active", "priority", "caller_id.name", "sys_updated_on"]
        );
        assert_eq!(
            query_fields("short_description=a^^zzz=1^state=2"),
            ["short_description", "state"]
        );
        assert!(query_fields("123TEXTQUERY321=x^EQ^variables.abc=1^ref_task.x=1").is_empty());
        assert!(query_fields("RLQUERYtask.x,>=1^active=true^ENDRLQUERY").is_empty());
        assert_eq!(query_fields("order=1"), ["order"]);
    }
}

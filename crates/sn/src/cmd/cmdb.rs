//! Configuration items and their relations (CMDB Instance and Meta APIs).

use super::args::{BodyArgs, PageArgs, RecordArgs, show};
use crate::ctx::Ctx;
use crate::output::{col, fields_text, flex};
use clap::{Args, Subcommand};
use serde_json::{Map, Value, json};
use sn_core::client::{Method, Request};
use sn_core::refs;
use sn_core::table::text;
use sn_core::{Error, Result, is_sys_id, query};
use std::fmt::Write;

const DEFAULT_LIMIT: u64 = 100;
/// What a change made by hand is. A discovery tool's name would let that
/// tool overwrite the record.
const DEFAULT_SOURCE: &str = "Manual Entry";

#[derive(Subcommand, Debug)]
pub enum CmdbCommand {
    /// List configuration items of a class.
    #[command(visible_alias = "ls")]
    List {
        /// Class, for example cmdb_ci_server.
        class: String,
        /// Encoded query.
        #[arg(long, short = 'q', alias = "sysparm-query")]
        query: Option<String>,
        #[command(flatten)]
        page: PageArgs,
    },
    /// Read a configuration item with its relations.
    Get {
        #[command(flatten)]
        item: RecordArgs,
    },
    /// Create a configuration item. A --data object with an `attributes` object goes out as it is, so it can hold relations.
    Create {
        /// Class, for example cmdb_ci_linux_server.
        class: String,
        #[command(flatten)]
        write: WriteArgs,
    },
    /// Change a configuration item.
    Update {
        #[command(flatten)]
        item: RecordArgs,
        #[command(flatten)]
        write: WriteArgs,
    },
    /// Attributes, identification rules and relation rules of a class.
    Meta {
        /// Class, for example cmdb_ci_linux_server.
        class: String,
    },
    /// Relations of a configuration item.
    #[command(subcommand)]
    Relation(RelationCommand),
}

#[derive(Args, Debug)]
pub struct WriteArgs {
    #[command(flatten)]
    body: BodyArgs,
    /// Discovery source to record (default: Manual Entry).
    #[arg(long, value_name = "NAME")]
    source: Option<String>,
}

#[derive(Subcommand, Debug)]
pub enum RelationCommand {
    /// Add relations: --data '{"outbound_relations":[{"type":ID,"target":ID}],"source":"..."}'.
    Add {
        #[command(flatten)]
        item: RecordArgs,
        #[command(flatten)]
        body: BodyArgs,
    },
    /// Delete a relation.
    #[command(visible_alias = "rm")]
    Delete {
        /// Class and sys_id, or class:sys_id, or a link; then the sys_id of the relation.
        #[arg(required = true, num_args = 2..=3, value_name = "CLASS SYS_ID RELATION")]
        words: Vec<String>,
    },
}

pub async fn run(ctx: &Ctx, command: CmdbCommand) -> Result<()> {
    match command {
        CmdbCommand::List { class, query, page } => list(ctx, &class, query, page).await,
        CmdbCommand::Get { item } => {
            let client = ctx.client().await?;
            let item = item.record(client).await?;
            let found = client
                .get(format!("{}/{}", path(&item.table)?, item.sys_id))
                .await?;
            if ctx.out.raw {
                return show(ctx, found);
            }
            ctx.out.item_with(found, json!({}), item_text)
        }
        CmdbCommand::Create { class, write } => {
            let body = envelope(write.body.required()?, write.source)?;
            let made = ctx.client().await?.post(path(&class)?, body).await?;
            written(ctx, "Created", &class, made)
        }
        CmdbCommand::Update { item, write } => {
            let body = envelope(write.body.required()?, write.source)?;
            let client = ctx.client().await?;
            let item = item.record(client).await?;
            let changed = client
                .patch(format!("{}/{}", path(&item.table)?, item.sys_id), body)
                .await?;
            written(ctx, "Updated", &item.table, changed)
        }
        CmdbCommand::Meta { class } => {
            query::check_name("class", &class)?;
            let meta = ctx
                .client()
                .await?
                .get(format!("/api/now/cmdb/meta/{class}"))
                .await?;
            show(ctx, meta)
        }
        CmdbCommand::Relation(RelationCommand::Add { item, body }) => {
            let body = body.required()?;
            let client = ctx.client().await?;
            let item = item.record(client).await?;
            let at = format!("{}/{}/relation", path(&item.table)?, item.sys_id);
            let made = client.post(at, body.into()).await?;
            let text = format!("Added relations to {} {}", item.table, item.sys_id);
            ctx.out.done(made, &text)
        }
        CmdbCommand::Relation(RelationCommand::Delete { words }) => {
            delete_relation(ctx, &words).await
        }
    }
}

fn path(class: &str) -> Result<String> {
    query::check_name("class", class)?;
    Ok(format!("/api/now/cmdb/instance/{class}"))
}

async fn list(ctx: &Ctx, class: &str, query: Option<String>, page: PageArgs) -> Result<()> {
    let request = Request::new(Method::GET, path(class)?).query_opt("sysparm_query", query);
    let response = ctx
        .client()
        .await?
        .send(page.apply(request, DEFAULT_LIMIT))
        .await?;
    let total = response.total();
    let rows = match response.result()? {
        Value::Array(rows) => rows,
        Value::Null => Vec::new(),
        other => vec![other],
    };
    let meta = page.meta(rows.len(), total, DEFAULT_LIMIT);
    ctx.out
        .list(rows, &[flex("NAME", "name"), col("SYS_ID", "sys_id")], meta)
}

fn item_text(item: &Value) -> String {
    let mut out = fields_text(&item["attributes"]);
    for (title, key, arrow) in [
        ("Outbound relations", "outbound_relations", "->"),
        ("Inbound relations", "inbound_relations", "<-"),
    ] {
        let relations = item[key].as_array().map_or(&[][..], Vec::as_slice);
        if relations.is_empty() {
            continue;
        }
        let _ = write!(out, "\n\n{title}");
        for relation in relations {
            let name = |of: &str| text(relation, of);
            let _ = write!(out, "\n  {arrow} {} ({})", name("target"), name("type"));
        }
    }
    out
}

/// One line for people; the item has its fields in `attributes`.
fn written(ctx: &Ctx, verb: &str, class: &str, item: Value) -> Result<()> {
    let attributes = &item["attributes"];
    let text = format!(
        "{verb} {class} {} ({})",
        text(attributes, "name"),
        text(attributes, "sys_id")
    );
    ctx.out.done(item, &text)
}

/// The body that the API takes: `{attributes, source}`. It answers a flat
/// body with a server error, and an attribute that is not text as well.
fn envelope(body: Map<String, Value>, source: Option<String>) -> Result<Value> {
    // Some classes have a column named `attributes`, but never an object.
    let mut out = match body.get("attributes").is_some_and(Value::is_object) {
        true => body,
        false if body.contains_key("source") => {
            return Err(Error::invalid(
                "you passed a field `source`, which this API reads as the discovery source and not as a column; pass --source NAME for the discovery source, or --data '{\"attributes\":{\"source\":\"...\"},\"source\":\"...\"}' for the column",
            ));
        }
        false => {
            let mut out = Map::new();
            out.insert("attributes".into(), body.into());
            out
        }
    };
    match (out.get("source"), source) {
        (Some(inside), Some(flag)) => {
            return Err(Error::invalid(format!(
                "you passed the discovery source twice: --source {flag}, and {inside} in --data; pass it once"
            )));
        }
        (Some(inside), None) if !inside.is_string() => {
            return Err(Error::invalid(format!(
                "you passed {inside} as the source in --data; pass text, such as \"{DEFAULT_SOURCE}\""
            )));
        }
        (Some(_), None) => {}
        (None, flag) => {
            let source = flag.unwrap_or_else(|| DEFAULT_SOURCE.into());
            out.insert("source".into(), source.into());
        }
    }
    for (name, value) in out
        .get_mut("attributes")
        .and_then(Value::as_object_mut)
        .into_iter()
        .flatten()
    {
        match value {
            Value::String(_) | Value::Null => {}
            Value::Number(_) | Value::Bool(_) => *value = value.to_string().into(),
            _ => {
                return Err(Error::invalid(format!(
                    "you passed a JSON object or list for the attribute `{name}`; this API takes text only, so pass the JSON as a quoted text if the column holds JSON"
                )));
            }
        }
    }
    Ok(out.into())
}

async fn delete_relation(ctx: &Ctx, words: &[String]) -> Result<()> {
    // `class:id RELATION` and `CLASS ID RELATION` differ in the first word.
    let combined = words[0].contains(':');
    let (class, id, relation) = match words {
        [item, relation] if combined => (item, None, relation),
        [class, id, relation] if !combined => (class, Some(id.as_str()), relation),
        _ => {
            return Err(Error::invalid(format!(
                "you passed `{}`; pass CLASS SYS_ID RELATION, or CLASS:SYS_ID RELATION",
                words.join(" ")
            )));
        }
    };
    if !is_sys_id(relation) {
        return Err(Error::invalid(format!(
            "you passed the relation `{relation}`; pass its sys_id, which `sn cmdb get` shows"
        )));
    }
    let client = ctx.client().await?;
    let item = refs::record(client, class, id).await?;
    ctx.confirm(&format!(
        "delete the relation {relation} of {} {}",
        item.table, item.sys_id
    ))?;
    client
        .delete(format!(
            "{}/{}/relation/{relation}",
            path(&item.table)?,
            item.sys_id
        ))
        .await?;
    ctx.out.done(
        json!({ "class": item.table, "sys_id": item.sys_id, "relation": relation, "deleted": true }),
        &format!("Deleted the relation {relation}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(value: Value) -> Map<String, Value> {
        value.as_object().cloned().expect("object")
    }

    #[test]
    fn fields_go_into_attributes_as_text() {
        let out = envelope(
            body(json!({ "name": "x", "cpu_count": 8, "virtual": true, "ip": null })),
            None,
        )
        .expect("envelope");
        assert_eq!(
            out,
            json!({ "attributes": { "name": "x", "cpu_count": "8", "virtual": "true", "ip": null }, "source": "Manual Entry" })
        );
        // `attributes` as text is a column.
        let out = envelope(body(json!({ "attributes": "a,b" })), Some("Tool".into()));
        assert_eq!(
            out.expect("envelope"),
            json!({ "attributes": { "attributes": "a,b" }, "source": "Tool" })
        );
    }

    #[test]
    fn an_envelope_keeps_its_shape() {
        let given = json!({ "attributes": { "n": 1 }, "source": "S", "outbound_relations": [{}] });
        assert_eq!(
            envelope(body(given), None).expect("envelope"),
            json!({ "attributes": { "n": "1" }, "source": "S", "outbound_relations": [{}] })
        );
    }

    #[test]
    fn ambiguous_and_structured_values_are_refused() {
        for (given, source) in [
            (json!({ "name": "x", "source": "S" }), None),
            (json!({ "attributes": {}, "source": "S" }), Some("T")),
            (json!({ "attributes": {}, "source": 1 }), None),
            (json!({ "name": ["x"] }), None),
            (json!({ "attributes": { "name": {} } }), None),
        ] {
            assert!(
                envelope(body(given.clone()), source.map(str::to_owned)).is_err(),
                "{given}"
            );
        }
    }
}

//! GraphQL: the passthrough command, and the calls other commands build on.
//!
//! GraphQL reports failure inside a successful response, sometimes next to
//! partial data.

use super::args::{pairs, parse_json};
use crate::ctx::{Ctx, read_input};
use clap::Args;
use serde_json::{Map, Value, json};
use sn_core::client::{Method, Request};
use sn_core::{Client, Error, Result};

const PATH: &str = "/api/now/graphql";

#[derive(Args, Debug)]
pub struct GraphqlArgs {
    /// GraphQL document, @file, or - for stdin.
    pub query: String,
    /// Variable name=value, sent as text (repeatable).
    #[arg(long = "var", value_name = "NAME=VALUE")]
    pub var: Vec<String>,
    /// All variables as a JSON object, @file, or - for stdin; --var wins.
    #[arg(long, value_name = "JSON")]
    pub variables: Option<String>,
    /// The operation to run when the document has several.
    #[arg(long, value_name = "NAME")]
    pub operation: Option<String>,
}

pub async fn run(ctx: &Ctx, args: GraphqlArgs) -> Result<()> {
    let document = read_input(&args.query)?;
    let mut variables = match &args.variables {
        None => Map::new(),
        Some(text) => match parse_json(&read_input(text)?, "--variables")? {
            Value::Object(map) => map,
            _ => {
                return Err(Error::invalid(
                    "--variables is JSON but not an object; pass an object such as '{\"limit\":5}'",
                ));
            }
        },
    };
    for (name, value) in pairs(&args.var, "--var")? {
        variables.insert(name, value.into());
    }
    let reads = !has_mutation(&document);
    let mut body = Map::new();
    body.insert("query".into(), document.into());
    if !variables.is_empty() {
        body.insert("variables".into(), variables.into());
    }
    if let Some(operation) = args.operation {
        body.insert("operationName".into(), operation.into());
    }
    let mut request = Request::new(Method::POST, PATH).json(body.into());
    if reads {
        request = request.reads();
    }
    let answer = ctx.client().await?.send(request).await?.json()?;
    let pretty = |data: &Value| serde_json::to_string_pretty(data).unwrap_or_default();
    if errors(&answer).is_empty() {
        let data = match (ctx.out.raw, answer.get("data")) {
            (false, Some(data)) => data.clone(),
            _ => answer,
        };
        return ctx.out.item_with(data, json!({}), pretty);
    }
    // Agents read one envelope, so theirs has the partial data in the error.
    if let Some(data) = answer.get("data").filter(|d| !d.is_null())
        && !ctx.out.agent
    {
        let data = if ctx.out.raw { &answer } else { data };
        ctx.out.item_with(data.clone(), json!({}), pretty)?;
    }
    Err(failure(&answer))
}

/// Runs a query and returns the whole answer, or the errors in it.
pub async fn execute(client: &Client, document: &str, variables: Value) -> Result<Value> {
    let answer = ask(client, document, variables).await?;
    match errors(&answer).is_empty() {
        true => Ok(answer),
        false => Err(failure(&answer)),
    }
}

/// Runs a query and returns the answer with its `errors`, if any.
pub async fn ask(client: &Client, document: &str, variables: Value) -> Result<Value> {
    send(client, document, variables, true).await
}

/// The same for any document; one that does not only read is guarded as a
/// write.
pub async fn send(client: &Client, document: &str, variables: Value, reads: bool) -> Result<Value> {
    let mut body = json!({ "query": document });
    if variables.as_object().is_some_and(|v| !v.is_empty()) {
        body["variables"] = variables;
    }
    let mut request = Request::new(Method::POST, PATH).json(body);
    if reads {
        request = request.reads();
    }
    client.send(request).await?.json()
}

pub fn errors(answer: &Value) -> &[Value] {
    answer["errors"].as_array().map_or(&[], Vec::as_slice)
}

pub fn failure(answer: &Value) -> Error {
    let errors = errors(answer);
    let message = errors
        .first()
        .and_then(|e| e["message"].as_str())
        .unwrap_or("the query failed");
    let more = match errors.len() {
        0 | 1 => String::new(),
        n => format!(" (and {} more)", n - 1),
    };
    let mut payload = json!({ "errors": errors });
    if let Some(data) = answer.get("data").filter(|d| !d.is_null()) {
        payload["data"] = data.clone();
    }
    Error::api(
        format!("the instance rejected the GraphQL document: {message}{more}"),
        payload,
    )
}

/// What the first error says, for documents that are not the caller's own.
pub fn refusal(errors: &[Value]) -> Error {
    let message = errors
        .first()
        .and_then(|e| e["message"].as_str())
        .unwrap_or("the GraphQL query failed without a reason");
    Error::api(message, errors.into())
}

/// True when the document was refused because the field does not exist.
pub fn undefined(answer: &Value, field: &str) -> bool {
    let needle = format!("Field '{field}'");
    errors(answer).iter().any(|e| {
        e["message"]
            .as_str()
            .is_some_and(|m| m.contains("FieldUndefined") && m.contains(&needle))
    })
}

/// True when the document was refused because the table has no query field.
/// An unknown table can also fail on its arguments, so the error's path
/// counts too.
pub fn unknown_table(answer: &Value, table: &str) -> bool {
    let path = format!("@[GlideRecord_Query/{table}])");
    undefined(answer, table)
        || errors(answer)
            .iter()
            .any(|e| e["message"].as_str().is_some_and(|m| m.contains(&path)))
}

pub fn unknown_table_error(answer: &Value, table: &str) -> Error {
    Error::api(
        format!(
            "you asked for table `{table}`, which GraphQL does not offer; check the name with `sn schema tables`, or read it with `sn table list {table}`"
        ),
        json!({ "errors": errors(answer) }),
    )
}

/// True when the document defines a mutation. Comments and text values are
/// skipped; a name that happens to be `mutation` counts too, which only
/// makes the read-only guard stricter.
fn has_mutation(document: &str) -> bool {
    let mut depth = 0_u32;
    let mut word = String::new();
    let mut rest = document;
    while let Some(c) = rest.chars().next() {
        if depth == 0 && (c.is_alphanumeric() || c == '_') {
            word.push(c);
        } else if std::mem::take(&mut word) == "mutation" {
            return true;
        }
        let skip = match c {
            '#' => rest.find('\n').unwrap_or(rest.len()),
            '"' if rest.starts_with("\"\"\"") => match rest[3..].find("\"\"\"") {
                Some(end) => end + 6,
                None => rest.len(),
            },
            '"' => {
                let mut escaped = false;
                let end = rest[1..].find(|c| {
                    let closes = c == '"' && !escaped;
                    escaped = c == '\\' && !escaped;
                    closes
                });
                end.map_or(rest.len(), |end| end + 2)
            }
            '{' => {
                depth += 1;
                1
            }
            '}' => {
                depth = depth.saturating_sub(1);
                1
            }
            other => other.len_utf8(),
        };
        rest = &rest[skip..];
    }
    word == "mutation"
}

#[cfg(test)]
mod tests {
    use super::has_mutation;

    #[test]
    fn only_documents_with_a_mutation_write() {
        for reads in [
            "{ a }",
            "query mutationLike { a }",
            "# mutation\nquery { mutation }",
            "query { a(text: \"} mutation {\") }",
            "query { a(text: \"\"\" } mutation \"\"\") }",
            "query { a(text: \"\\\"} mutation { \") }",
        ] {
            assert!(!has_mutation(reads), "{reads}");
        }
        for writes in [
            "mutation { a }",
            "mutation M($x: Int) { a }",
            "query Q { a }\nmutation{ b }",
            "fragment F on T { x } mutation N { a }",
        ] {
            assert!(has_mutation(writes), "{writes}");
        }
    }
}

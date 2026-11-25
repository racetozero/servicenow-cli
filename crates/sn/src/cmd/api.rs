//! Every documented REST operation as a command. The command tree comes from
//! the catalog: only the named API gets its operations, and only the named
//! operation its flags.

use super::args::{header, pairs, parse_json};
use crate::cli::GlobalArgs;
use crate::ctx::{Ctx, read_input};
use crate::help::{self, Effect};
use crate::output::{col, flex};
use clap::{Arg, ArgAction, ArgMatches, Args, Command, FromArgMatches};
use serde_json::{Map, Value, json};
use sn_core::catalog::{self, Operation, Param, Place};
use sn_core::client::{Body, Request, without_envelope};
use sn_core::{Error, Result};
use std::collections::HashSet;

/// The arguments are built at run time by [`tree`], so they stay unparsed.
#[derive(Debug)]
pub struct ApiArgs(pub ArgMatches);

impl FromArgMatches for ApiArgs {
    fn from_arg_matches(matches: &ArgMatches) -> Result<Self, clap::Error> {
        Ok(Self(matches.clone()))
    }

    fn update_from_arg_matches(&mut self, matches: &ArgMatches) -> Result<(), clap::Error> {
        self.0 = matches.clone();
        Ok(())
    }
}

impl Args for ApiArgs {
    fn augment_args(command: Command) -> Command {
        command
    }

    fn augment_args_for_update(command: Command) -> Command {
        command
    }
}

/// Adds the catalog to the `api` command. `argv` is everything after `api`.
pub fn tree(command: Command, argv: &[String]) -> Command {
    let words: Vec<&str> = argv
        .iter()
        .map(String::as_str)
        .filter(|a| !a.starts_with('-'))
        .collect();
    let named = words.first().copied().unwrap_or_default();
    let mut command = command
        .about("Every documented REST operation (141 APIs)")
        .subcommand_required(true)
        .arg_required_else_help(true)
        .subcommand_value_name("API|COMMAND")
        .subcommand(Command::new("list").about("List the APIs"))
        .subcommand(
            Command::new("search")
                .about("Find operations by words")
                .arg(
                    Arg::new("text")
                        .required(true)
                        .num_args(1..)
                        .help("Words, all of which must match"),
                )
                .arg(
                    Arg::new("method")
                        .long("method")
                        .help("Only this HTTP method"),
                )
                .arg(
                    Arg::new("limit")
                        .long("limit")
                        .short('n')
                        .value_parser(clap::value_parser!(usize))
                        .default_value("30")
                        .help("Most results"),
                ),
        )
        .subcommand(
            Command::new("describe")
                .about("Parameters and body of an operation")
                .arg(
                    Arg::new("api")
                        .required(true)
                        .help("API name from `api list`"),
                )
                .arg(
                    Arg::new("operation")
                        .required(true)
                        .help("Operation from `sn api API --help` or `api search`"),
                ),
        );
    for api in catalog::apis() {
        // Hidden from `api --help`: 141 lines say less than `api list`.
        let mut sub = Command::new(api.name).about(api.title.clone()).hide(true);
        if api.name == named {
            sub = sub
                .hide(false)
                .arg_required_else_help(true)
                .subcommand_required(true)
                .long_about(format!("{}\n\n{}\n{}", api.title, api.text, api.doc));
            for operation in api.operations() {
                let wanted = words.get(1) == Some(&operation.id.as_str());
                let about = format!(
                    "{} {}  {}",
                    operation.method, operation.path, operation.summary
                );
                let mut op = Command::new(operation.id.clone()).about(about);
                if wanted {
                    op = with_flags(op, api.name, &operation);
                }
                sub = sub.subcommand(op);
            }
        }
        command = command.subcommand(sub);
    }
    command
}

/// The flag names the program already uses.
fn taken() -> HashSet<String> {
    let mut taken: HashSet<String> = GlobalArgs::augment_args(Command::new("sn"))
        .get_arguments()
        .flat_map(|a| a.get_long_and_visible_aliases().unwrap_or_default())
        .map(str::to_owned)
        .collect();
    taken.extend(
        [
            "data", "field", "file", "out", "help", "version", "param", "header",
        ]
        .map(str::to_owned),
    );
    taken
}

/// Each parameter with its flag; two that collide get their place in front.
fn flags(operation: &Operation) -> Vec<(String, &Param)> {
    let mut taken = taken();
    operation
        .params
        .iter()
        .map(|param| {
            let mut flag = param.flag();
            if flag.is_empty() || !taken.insert(flag.clone()) {
                flag = format!("{}-{flag}", param.place.name());
                while !taken.insert(flag.clone()) {
                    flag.push('2');
                }
            }
            (flag, param)
        })
        .collect()
}

/// Operations that remove something ask first.
fn confirms(operation: &Operation) -> bool {
    operation.method == "DELETE"
        || operation.id.starts_with("delete")
        || operation.id.starts_with("remove")
}

fn with_flags(mut command: Command, api: &str, operation: &Operation) -> Command {
    let mut example = format!("sn api {api} {}", operation.id);
    for (flag, param) in flags(operation) {
        let kind = param.kind.as_deref().unwrap_or("String");
        let mut help = param.text.clone();
        if !param.fields.is_empty() {
            let names: Vec<&str> = param.fields.iter().map(|f| f.name.as_str()).collect();
            help = format!("{help} JSON with: {}.", names.join(", "));
        }
        // Body fields may come through --data, so clap does not require them.
        let required = param.place == Place::Path;
        if required {
            example.push_str(&format!(
                " --{flag} {}",
                flag.replace('-', "_").to_uppercase()
            ));
        }
        let mut arg = Arg::new(format!("p:{}:{}", param.place.name(), param.name))
            .long(flag.clone())
            .value_name(if param.fields.is_empty() {
                kind.to_uppercase().replace(' ', "_")
            } else {
                "JSON".into()
            })
            .help(help.trim().to_owned())
            .required(required)
            .allow_hyphen_values(true);
        let original = param.name.replace('_', "-").to_ascii_lowercase();
        if original.starts_with("sysparm-") {
            arg = arg.alias(original);
        }
        if param.place == Place::Path {
            arg = arg.help_heading("Path");
        } else if param.place == Place::Body {
            arg = arg.help_heading("Body");
        } else {
            arg = arg.help_heading("Query");
        }
        command = command.arg(arg);
    }
    let many = |name: &'static str, short: char, value: &'static str, help: &'static str| {
        Arg::new(name)
            .long(name)
            .short(short)
            .value_name(value)
            .help(help)
            .action(ArgAction::Append)
            .allow_hyphen_values(true)
    };
    if !matches!(operation.method.as_str(), "GET" | "DELETE") {
        example.push_str(" -D @body.json");
    }
    let effect = match (
        confirms(operation),
        operation.writes,
        operation.method.as_str(),
    ) {
        (true, ..) => Effect::Confirm,
        (_, false, "GET" | "HEAD") => Effect::Read,
        _ => Effect::Write,
    };
    let agent = format!("{example} --agent");
    command
        .after_long_help(help::footer(Some(effect), &[("", &example), ("", &agent)]))
        .arg(
            Arg::new("data")
                .long("data")
                .short('D')
                .value_name("JSON")
                .help("Whole body: JSON, @file, or - for stdin")
                .allow_hyphen_values(true),
        )
        .arg(many("field", 'F', "NAME=VALUE", "Body field (repeatable)"))
        .arg(many(
            "param",
            'P',
            "NAME=VALUE",
            "Query parameter that is not listed (repeatable)",
        ))
        .arg(many("header", 'H', "NAME: VALUE", "Header (repeatable)"))
        .arg(
            Arg::new("file")
                .long("file")
                .value_name("FILE")
                .help("Send this file as the body")
                .conflicts_with_all(["data", "field"]),
        )
        .arg(
            Arg::new("out")
                .long("out")
                .value_name("FILE")
                .help("Write the response to a file"),
        )
}

pub async fn run(ctx: &Ctx, matches: &ArgMatches) -> Result<()> {
    match matches.subcommand() {
        Some(("list", _)) => {
            let rows = catalog::apis()
                .iter()
                .map(|a| json!({ "api": a.name, "title": a.title, "operations": a.operations().len(), "text": a.text, "doc": a.doc }))
                .collect();
            ctx.out.list(
                rows,
                &[
                    col("API", "api"),
                    col("OPERATIONS", "operations"),
                    flex("TITLE", "title"),
                ],
                json!({}),
            )
        }
        Some(("search", m)) => {
            let words: Vec<&str> = m
                .get_many::<String>("text")
                .into_iter()
                .flatten()
                .map(String::as_str)
                .collect();
            let found = catalog::search(
                &words.join(" "),
                m.get_one::<String>("method").map(String::as_str),
            );
            let total = found.len();
            let rows = found
                .into_iter()
                .take(m.get_one("limit").copied().unwrap_or(30))
                .map(|(api, o)| json!({ "api": api.name, "operation": o.id, "method": o.method, "path": o.path, "summary": o.summary }))
                .collect();
            let cols = [
                col("API", "api"),
                col("OPERATION", "operation"),
                col("METHOD", "method"),
                flex("PATH", "path"),
            ];
            ctx.out.list(rows, &cols, json!({ "total": total }))
        }
        Some(("describe", m)) => {
            let text = |id: &str| m.get_one::<String>(id).cloned().unwrap_or_default();
            describe(ctx, &text("api"), &text("operation"))
        }
        Some((name, m)) => {
            let api = catalog::api(name)?;
            let Some((id, m)) = m.subcommand() else {
                return Err(api
                    .operation("")
                    .err()
                    .unwrap_or_else(|| Error::invalid("pass an operation")));
            };
            call(ctx, &api.operation(id)?, m).await
        }
        None => Err(Error::invalid(
            "pass an API, or search, list or describe; see `sn api --help`",
        )),
    }
}

fn describe(ctx: &Ctx, api: &str, id: &str) -> Result<()> {
    let page = catalog::api(api)?;
    let operation = page.operation(id)?;
    fn param_json(flag: Option<String>, p: &Param) -> Value {
        let mut out = json!({ "name": p.name, "in": p.place.name(), "type": p.kind, "required": p.required, "text": p.text });
        if let Some(flag) = flag {
            out["flag"] = format!("--{flag}").into();
        }
        if !p.fields.is_empty() {
            out["fields"] = p.fields.iter().map(|f| param_json(None, f)).collect();
        }
        out
    }
    let params: Vec<Value> = flags(&operation)
        .into_iter()
        .map(|(f, p)| param_json(Some(f), p))
        .collect();
    let data = json!({
        "api": page.name, "operation": operation.id, "method": operation.method, "path": operation.path,
        "summary": operation.summary, "content_type": operation.content, "params": params, "doc": page.doc,
    });
    ctx.out.item_with(data, json!({}), |data| {
        let mut lines = vec![
            format!("{} {}", operation.method, operation.path),
            operation.summary.clone(),
        ];
        let mut place = "";
        for p in data["params"].as_array().into_iter().flatten() {
            let at = p["in"].as_str().unwrap_or_default();
            if at != place {
                place = match at {
                    "path" => "path",
                    "query" => "query",
                    _ => "body",
                };
                lines.push(format!("\n{}", place.to_uppercase()));
            }
            let star = if p["required"] == true { "*" } else { "" };
            lines.push(format!(
                "  {}{star}  {}",
                p["flag"].as_str().unwrap_or_default(),
                p["text"].as_str().unwrap_or_default()
            ));
            for f in p["fields"].as_array().into_iter().flatten() {
                lines.push(format!(
                    "      {}  {}",
                    f["name"].as_str().unwrap_or_default(),
                    f["text"].as_str().unwrap_or_default()
                ));
            }
        }
        lines.push(format!("\n{}", page.doc));
        lines.join("\n")
    })
}

async fn call(ctx: &Ctx, operation: &Operation, matches: &ArgMatches) -> Result<()> {
    let mut path = operation.path.clone();
    let mut query = Vec::new();
    let mut body = Map::new();
    for param in &operation.params {
        let Some(value) =
            matches.get_one::<String>(&format!("p:{}:{}", param.place.name(), param.name))
        else {
            continue;
        };
        match param.place {
            Place::Path => {
                // One path segment, whatever the value contains.
                let mut url =
                    url::Url::parse("http://x/").map_err(|err| Error::Internal(err.to_string()))?;
                if let Ok(mut segments) = url.path_segments_mut() {
                    segments.clear().push(value);
                }
                path = path.replace(
                    &format!("{{{}}}", param.name),
                    url.path().trim_start_matches('/'),
                );
            }
            Place::Query => query.push((param.name.clone(), value.clone())),
            Place::Body => {
                body.insert(param.name.clone(), typed(param, value)?);
            }
        }
    }
    let list = |name: &str| -> Vec<String> {
        matches
            .get_many::<String>(name)
            .into_iter()
            .flatten()
            .cloned()
            .collect()
    };
    for (name, value) in pairs(&list("field"), "--field")? {
        body.insert(name, value.into());
    }
    query.extend(pairs(&list("param"), "--param")?);
    let method = super::raw::parse_method(&operation.method)?;
    if confirms(operation) {
        ctx.confirm(&format!("send {method} {path}"))?;
    }
    let mut request = Request::new(method, path).always_dry();
    if operation.writes {
        request = request.writes();
    }
    request.query = query;
    let mut content_type = operation.content.clone();
    for text in list("header") {
        let (name, value) = header(&text)?;
        match name.eq_ignore_ascii_case("content-type") {
            true => content_type = Some(value.to_owned()),
            false => request = request.header(name, value),
        }
    }
    if let Some(file) = matches.get_one::<String>("file") {
        request = request.body(file_body(operation, file, content_type, body)?);
    } else if let Some(data) = matches.get_one::<String>("data") {
        let text = read_input(data)?;
        request = match &content_type {
            Some(kind) if !kind.contains("json") && kind != "*/*" => request.body(Body::Bytes {
                data: text.into_bytes().into(),
                content_type: kind.clone(),
            }),
            _ => match parse_json(&text, "--data")? {
                Value::Object(mut whole) => {
                    whole.extend(body);
                    request.json(whole.into())
                }
                other => request.json(other),
            },
        };
        if let Some(kind) = content_type.filter(|k| k.contains("json") && k != "application/json") {
            request = request.header("content-type", kind);
        }
    } else if !body.is_empty() {
        request = request.json(body.into());
    }
    let response = ctx.client().await?.send(request).await?;
    if let Some(out) = matches.get_one::<String>("out") {
        std::fs::write(out, &response.body).map_err(|err| {
            Error::invalid(format!(
                "cannot write {out} ({err}); pass a path you can write"
            ))
        })?;
        return ctx.out.done(
            json!({ "path": out, "bytes": response.body.len() }),
            &format!("Wrote {} bytes to {out}", response.body.len()),
        );
    }
    let total = response.total();
    match response.json() {
        Ok(Value::Null) => ctx.out.done(json!({ "status": response.status }), "done"),
        Ok(whole) if ctx.out.raw => ctx.out.item(whole),
        Ok(whole) => match without_envelope(whole) {
            Value::Array(rows) if rows.iter().all(Value::is_object) => ctx.out.list(
                rows,
                &[],
                total.map_or_else(|| json!({}), |t| json!({ "total": t })),
            ),
            Value::Null => ctx.out.done(json!({ "status": response.status }), "done"),
            other => ctx.out.item(other),
        },
        Err(_) => ctx.out.text(&response.text()),
    }
}

/// A body value in the documented type: JSON for objects, arrays, numbers
/// and booleans, text otherwise.
fn typed(param: &Param, text: &str) -> Result<Value> {
    let kind = param
        .kind
        .as_deref()
        .unwrap_or("String")
        .to_ascii_lowercase();
    let structured = !param.fields.is_empty()
        || ["object", "array", "list"]
            .iter()
            .any(|k| kind.starts_with(k));
    if structured {
        return parse_json(&read_input(text)?, &format!("--{}", param.flag()));
    }
    let scalar = ["number", "integer", "long", "float", "double", "boolean"]
        .iter()
        .any(|k| kind.starts_with(k));
    if scalar {
        return Ok(serde_json::from_str(text).unwrap_or_else(|_| text.into()));
    }
    Ok(read_input(text)?.into())
}

fn file_body(
    operation: &Operation,
    file: &str,
    content_type: Option<String>,
    fields: Map<String, Value>,
) -> Result<Body> {
    let path = std::path::PathBuf::from(file);
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if content_type.as_deref() == Some("multipart/form-data") {
        let data = std::fs::read(&path).map_err(|err| {
            Error::invalid(format!(
                "cannot read {file} ({err}); pass a file that exists"
            ))
        })?;
        let file_field = operation
            .params
            .iter()
            .find(|p| p.place == Place::Body && p.name.to_ascii_lowercase().contains("file"))
            .map_or_else(|| "uploadFile".to_owned(), |p| p.name.clone());
        let fields = fields
            .into_iter()
            .map(|(n, v)| (n, crate::output::cell_text(&v)))
            .collect();
        return Ok(Body::Multipart {
            fields,
            file_field,
            file_name: name,
            data: data.into(),
            content_type: "application/octet-stream".into(),
        });
    }
    let content_type = content_type
        .filter(|c| c != "*/*")
        .unwrap_or_else(|| "application/octet-stream".into());
    Ok(Body::File { path, content_type })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every operation must build: no two flags alike, none that hides a
    /// global flag.
    #[test]
    fn every_operation_has_a_valid_command() {
        for api in catalog::apis() {
            for operation in api.operations() {
                GlobalArgs::augment_args(Command::new("sn"))
                    .subcommand(with_flags(
                        Command::new(operation.id.clone()),
                        api.name,
                        &operation,
                    ))
                    .debug_assert();
            }
        }
    }
}

//! The REST APIs that the instance publishes, from the endpoints of the REST
//! API Explorer (`/api/now/doc`). They are undocumented, so a 404 can mean
//! that the instance has none, or that an argument is wrong.
//! `cli::implied_verb` turns `apisearch TEXT` into a search.

use crate::ctx::Ctx;
use crate::output::{Col, Format, col, flex};
use clap::{Args, Subcommand, ValueEnum};
use serde_json::{Map, Value, json};
use sn_core::client::{Method, Request};
use sn_core::{Choice, Client, Error, Result};

const SEARCH_COLUMNS: &[Col] = &[
    col("METHOD", "method"),
    flex("ROUTE", "route"),
    col("API", "name"),
    col("VERSION", "version"),
];
const API_COLUMNS: &[Col] = &[
    flex("NAME", "name"),
    col("API", "api_name"),
    col("VERSIONS", "versions"),
    col("ENDPOINTS", "endpoints"),
];

#[derive(Subcommand, Debug)]
pub enum ApisearchCommand {
    /// Find endpoints by text (also without the word search: `apisearch TEXT`).
    Search(SearchArgs),
    /// Every namespace, or the APIs of one namespace.
    #[command(visible_alias = "ls")]
    List {
        /// Namespace, for example now.
        #[arg(long, short = 'n', value_name = "NAME")]
        namespace: Option<String>,
    },
    /// The OpenAPI 3 specification of one API.
    Spec(SpecArgs),
}

#[derive(Args, Debug)]
pub struct SearchArgs {
    /// Text in the namespace, name, route, or description.
    term: String,
    /// Only this namespace.
    #[arg(long, short = 'n', value_name = "NAME")]
    namespace: Option<String>,
    /// Only this HTTP method.
    #[arg(long, short = 'm', value_name = "METHOD")]
    method: Option<String>,
}

#[derive(Args, Debug)]
pub struct SpecArgs {
    /// API name from `apisearch list`, or a part that only one API has.
    name: String,
    /// Namespace of the API.
    #[arg(long, short = 'n', value_name = "NAME")]
    namespace: Option<String>,
    /// API version, for example v2 (default: the latest).
    #[arg(long, value_name = "VERSION")]
    version: Option<String>,
    /// Form of the specification; yaml is also chosen by --output yaml.
    #[arg(long = "as", value_enum, default_value_t = SpecForm::Json, value_name = "FORM")]
    form: SpecForm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum SpecForm {
    Json,
    Yaml,
}

pub async fn run(ctx: &Ctx, command: ApisearchCommand) -> Result<()> {
    match command {
        ApisearchCommand::Search(args) => search(ctx, args).await,
        ApisearchCommand::List { namespace } => list(ctx, namespace.as_deref()).await,
        ApisearchCommand::Spec(args) => spec(ctx, args).await,
    }
}

/// One answer of the doc endpoints. The specification is served only as
/// application/octet-stream, so any type is accepted.
fn doc(path: &str) -> Request {
    Request::new(Method::GET, format!("/api/now/doc{path}")).header("accept", "*/*")
}

/// The catalog, `{namespace: {api: {...}}}`, whole or of one namespace.
async fn catalog(client: &Client, namespace: Option<&str>) -> Result<Map<String, Value>> {
    let request = match namespace {
        Some(name) => doc("/services").query("namespace", name),
        None => doc(""),
    };
    let result = match client.send(request).await {
        Ok(response) => response.result()?,
        Err(err) => return Err(explain(client, err, "").await),
    };
    let Value::Object(catalog) = result else {
        return Err(Error::api(
            "the REST API catalog has an unknown shape",
            result,
        ));
    };
    match namespace {
        // An unknown namespace gets an empty catalog.
        Some(name) if catalog.is_empty() => Err(unknown_namespace(client, name).await),
        _ => Ok(catalog),
    }
}

async fn namespaces(client: &Client) -> Result<Vec<String>> {
    let result = client.send(doc("/namespaces")).await?.result()?;
    Ok(result
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|n| n.as_str().map(str::to_owned))
        .collect())
}

async fn unknown_namespace(client: &Client, name: &str) -> Error {
    let all = namespaces(client).await.unwrap_or_default();
    if all.iter().any(|n| n == name) {
        return Error::invalid(format!(
            "the namespace {name} has no REST API; `sn apisearch list` shows those that have"
        ));
    }
    let near = near_matches(name, &all);
    Error::Input {
        arg: "namespace".into(),
        message: format!(
            "you passed --namespace {name}, which this instance does not have{}; `sn apisearch list` shows them",
            did_you_mean(&near)
        ),
        choices: near.iter().map(|n| Choice::new(n, n)).collect(),
    }
}

/// A 404 means an old instance only when the cheapest endpoint is missing
/// too; else it is about the request, and the instance says why.
async fn explain(client: &Client, err: Error, hint: &str) -> Error {
    let Error::Api(mut failure) = err else {
        return err;
    };
    if failure.status == Some(404) {
        let missing = matches!(namespaces(client).await, Err(err) if err.status() == Some(404));
        match (missing, &failure.detail) {
            (true, _) => {
                failure.message = "this instance has no REST API catalog (/api/now/doc); it is too old, so use `sn api search`".into();
            }
            (false, Some(detail)) => failure.message = format!("{detail}{hint}"),
            (false, None) => failure.message.push_str(hint),
        }
    }
    Error::Api(failure)
}

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or_default()
}

/// Every version and endpoint of an API.
fn resources(api: &Value) -> impl Iterator<Item = (&str, &Value)> {
    api["versions"]
        .as_object()
        .into_iter()
        .flatten()
        .flat_map(|(version, v)| {
            let list = v["resources"].as_array().map_or(&[][..], Vec::as_slice);
            list.iter().map(move |r| (version.as_str(), r))
        })
}

fn apis(catalog: &Map<String, Value>) -> impl Iterator<Item = (&str, &str, &Value)> {
    catalog.iter().flat_map(|(namespace, apis)| {
        apis.as_object()
            .into_iter()
            .flatten()
            .map(move |(name, api)| (namespace.as_str(), name.as_str(), api))
    })
}

async fn list(ctx: &Ctx, namespace: Option<&str>) -> Result<()> {
    let client = ctx.client().await?;
    let catalog = catalog(client, namespace).await?;
    if ctx.out.raw {
        return ctx.out.item(json!({ "result": catalog }));
    }
    if namespace.is_none() {
        let rows = catalog
            .iter()
            .map(|(name, apis)| {
                let apis = apis.as_object();
                let endpoints: usize = apis
                    .into_iter()
                    .flatten()
                    .map(|(_, api)| resources(api).count())
                    .sum();
                json!({ "namespace": name, "apis": apis.map_or(0, Map::len), "endpoints": endpoints })
            })
            .collect();
        return ctx.out.list(rows, &[], json!({}));
    }
    let rows = apis(&catalog)
        .map(|(namespace, name, api)| {
            let versions: Vec<&String> = api["versions"]
                .as_object()
                .into_iter()
                .flat_map(Map::keys)
                .collect();
            json!({
                "namespace": namespace, "name": name, "api_name": text(api, "apiName"),
                "versions": versions, "endpoints": resources(api).count(),
                "scripted": api["scripted"] == true, "description": text(api, "description"),
            })
        })
        .collect();
    ctx.out.list(rows, API_COLUMNS, json!({}))
}

async fn search(ctx: &Ctx, args: SearchArgs) -> Result<()> {
    let client = ctx.client().await?;
    let catalog = catalog(client, args.namespace.as_deref()).await?;
    if ctx.out.raw {
        return ctx.out.item(json!({ "result": catalog }));
    }
    let term = args.term.to_lowercase();
    let has = |text: &str| text.to_lowercase().contains(&term);
    let mut rows = Vec::new();
    for (namespace, name, api) in apis(&catalog) {
        // A hit on the API itself means all its endpoints.
        let whole = has(namespace)
            || has(name)
            || has(text(api, "apiName"))
            || has(text(api, "description"));
        for (version, resource) in resources(api) {
            let method = text(resource, "httpMethod");
            let wanted = args
                .method
                .as_ref()
                .is_none_or(|m| m.eq_ignore_ascii_case(method));
            let route = text(resource, "route");
            let description = text(resource, "description");
            if wanted && (whole || has(route) || has(description)) {
                rows.push(json!({
                    "namespace": namespace, "name": name, "api_name": text(api, "apiName"),
                    "version": version, "method": method, "route": route,
                    "description": description,
                }));
            }
        }
    }
    ctx.out.list(rows, SEARCH_COLUMNS, json!({}))
}

async fn spec(ctx: &Ctx, args: SpecArgs) -> Result<()> {
    let client = ctx.client().await?;
    let catalog = catalog(client, args.namespace.as_deref()).await?;
    let (namespace, name) = pick(&catalog, &args.name)?;
    let yaml = args.form == SpecForm::Yaml || ctx.out.format == Format::Yaml;
    let request = doc("/oas_3")
        .query("namespace", namespace)
        .query("name", name)
        .query("format", if yaml { "yaml" } else { "json" })
        .query_opt("version", args.version);
    let response = match client.send(request).await {
        Ok(response) => response,
        Err(err) => {
            let hint = format!("; `sn apisearch list --namespace {namespace}` shows the versions");
            return Err(explain(client, err, &hint).await);
        }
    };
    match yaml {
        true => ctx.out.text(&response.text()),
        false if ctx.out.human() => ctx
            .out
            .text(&serde_json::to_string_pretty(&response.json()?)?),
        false => ctx.out.item(response.json()?),
    }
}

/// The API of this title, else of this `apiName`, else the only one with
/// this part.
fn pick<'a>(catalog: &'a Map<String, Value>, wanted: &str) -> Result<(&'a str, &'a str)> {
    let wanted_lower = wanted.to_lowercase();
    let (mut titles, mut names, mut parts) = (Vec::new(), Vec::new(), Vec::new());
    for (namespace, title, api) in apis(catalog) {
        let lower = title.to_lowercase();
        let api_name = text(api, "apiName").to_lowercase();
        if lower == wanted_lower {
            titles.push((namespace, title));
        } else if api_name == wanted_lower {
            names.push((namespace, title));
        } else if lower.contains(&wanted_lower) || api_name.contains(&wanted_lower) {
            parts.push((namespace, title));
        }
    }
    let matches = [titles, names, parts]
        .into_iter()
        .find(|m| !m.is_empty())
        .unwrap_or_default();
    match matches.as_slice() {
        [one] => Ok(*one),
        [] => {
            let all: Vec<String> = apis(catalog).map(|(_, t, _)| t.to_owned()).collect();
            let near = near_matches(wanted, &all);
            Err(Error::Input {
                arg: "name".into(),
                message: format!(
                    "there is no API `{wanted}` here{}; find it with `sn apisearch search TEXT`",
                    did_you_mean(&near)
                ),
                choices: near.iter().map(|n| Choice::new(n, n)).collect(),
            })
        }
        many => Err(Error::Input {
            arg: "name".into(),
            message: format!(
                "several APIs match `{wanted}`: {}; pass the whole name, and --namespace when namespaces share it",
                many.iter()
                    .map(|(ns, t)| format!("{ns}/{t}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            choices: many
                .iter()
                .map(|(ns, title)| Choice::new(*title, format!("--namespace {ns}")))
                .collect(),
        }),
    }
}

fn did_you_mean(near: &[String]) -> String {
    match near.is_empty() {
        true => String::new(),
        false => format!(" (did you mean {}?)", near.join(", ")),
    }
}

/// Up to three names like `wanted`: parts of it first, then typing mistakes
/// in the name or in one of its words.
fn near_matches(wanted: &str, names: &[String]) -> Vec<String> {
    let wanted = wanted.to_lowercase();
    let allowed = (1 + wanted.chars().count() / 4).min(3);
    let mut scored: Vec<(usize, &String)> = names
        .iter()
        .filter_map(|name| {
            let lower = name.to_lowercase();
            if lower.contains(&wanted) || wanted.contains(&lower) {
                return Some((0, name));
            }
            let distance = lower
                .split_whitespace()
                .chain([lower.as_str()])
                .map(|word| edit_distance(&wanted, word))
                .min()?;
            (distance <= allowed).then_some((distance, name))
        })
        .collect();
    scored.sort();
    scored.into_iter().take(3).map(|(_, n)| n.clone()).collect()
}

/// Levenshtein, with swapped neighbours as one mistake.
fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut rows = vec![vec![0; b.len() + 1]; a.len() + 1];
    for (i, row) in rows.iter_mut().enumerate() {
        row[0] = i;
    }
    rows[0] = (0..=b.len()).collect();
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            let mut best = (rows[i - 1][j] + 1)
                .min(rows[i][j - 1] + 1)
                .min(rows[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                best = best.min(rows[i - 2][j - 2] + 1);
            }
            rows[i][j] = best;
        }
    }
    rows[a.len()][b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_resolve_by_title_then_api_name_then_part() {
        let catalog = json!({
            "now": { "Table API": { "apiName": "now/table" }, "now/pa": { "apiName": "now/scores" } },
            "x_one": { "Shared": { "apiName": "x_one/pa" } },
            "x_two": { "Shared": { "apiName": "x_two/a" } },
        });
        let catalog = catalog.as_object().expect("catalog");
        assert_eq!(
            pick(catalog, "table api").expect("title"),
            ("now", "Table API")
        );
        assert_eq!(
            pick(catalog, "NOW/TABLE").expect("api name"),
            ("now", "Table API")
        );
        assert_eq!(pick(catalog, "able").expect("part"), ("now", "Table API"));
        let err = pick(catalog, "shared").expect_err("two");
        assert!(err.to_string().contains("x_one/Shared, x_two/Shared"));
        let err = pick(catalog, "tabel").expect_err("none").to_string();
        assert!(err.contains("did you mean Table API?"), "{err}");
    }

    #[test]
    fn swapped_letters_are_one_mistake() {
        assert_eq!(edit_distance("nwo", "now"), 1);
        assert_eq!(edit_distance("", "abc"), 3);
        assert!(near_matches("chg", &["sn_chg_rest".into(), "now".into()]) == ["sn_chg_rest"]);
    }
}

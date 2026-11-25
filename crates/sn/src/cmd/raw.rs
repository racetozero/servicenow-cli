//! Any REST path with the profile's sign-in.

use super::args::{BodyArgs, header, pairs, parse_json};
use crate::ctx::{Ctx, read_input};
use clap::Args;
use sn_core::client::{Body, Method, Request};
use sn_core::{Error, Result};

#[derive(Args, Debug)]
pub struct RawArgs {
    /// HTTP method: GET, POST, PUT, PATCH, DELETE, HEAD, OPTIONS.
    pub method: String,
    /// Path on the instance, for example /api/now/table/incident?sysparm_limit=1.
    pub path: String,
    /// Query parameter name=value (repeatable).
    #[arg(long = "query", short = 'q', value_name = "NAME=VALUE")]
    pub query: Vec<String>,
    /// Header "Name: value" (repeatable).
    #[arg(long = "header", short = 'H', value_name = "HEADER")]
    pub header: Vec<String>,
    #[command(flatten)]
    pub body: BodyArgs,
    /// Send this file (or - for stdin) as it is, with the Content-Type header you pass.
    #[arg(long, value_name = "FILE", conflicts_with_all = ["data", "field"])]
    pub body_file: Option<String>,
    /// Write the response body to a file.
    #[arg(long, value_name = "FILE")]
    pub out: Option<std::path::PathBuf>,
    /// Show the status and headers too.
    #[arg(long)]
    pub include: bool,
}

pub async fn run(ctx: &Ctx, args: RawArgs) -> Result<()> {
    let method = parse_method(&args.method)?;
    let mut request = Request::new(method.clone(), args.path.clone()).always_dry();
    for (name, value) in pairs(&args.query, "--query")? {
        request = request.query(name, value);
    }
    for text in &args.header {
        let (name, value) = header(text)?;
        request = request.header(name, value);
    }
    if let Some(file) = &args.body_file {
        let data = match file.as_str() {
            "-" => read_input("-")?.into_bytes(),
            path => std::fs::read(path).map_err(|err| {
                Error::invalid(format!(
                    "cannot read {path} ({err}); pass a file that exists"
                ))
            })?,
        };
        let content_type = request
            .headers
            .iter()
            .position(|(n, _)| n.eq_ignore_ascii_case("content-type"))
            .map(|at| request.headers.remove(at).1)
            .unwrap_or_else(|| "application/octet-stream".into());
        request = request.body(Body::Bytes {
            data: data.into(),
            content_type,
        });
    } else if let Some(data) = &args.body.data
        && args.body.field.is_empty()
    {
        request = request.json(parse_json(&read_input(data)?, "--data")?);
    } else if !args.body.is_empty() {
        request = request.json(args.body.object()?.into());
    }
    let client = ctx.client().await?;
    let response = client.send(request).await?;
    if let Some(path) = &args.out {
        std::fs::write(path, &response.body).map_err(|err| {
            Error::invalid(format!(
                "cannot write {} ({err}); pass a path you can write",
                path.display()
            ))
        })?;
        let data = serde_json::json!({ "path": path.display().to_string(), "bytes": response.body.len(), "status": response.status });
        return ctx.out.done(
            data,
            &format!("Wrote {} bytes to {}", response.body.len(), path.display()),
        );
    }
    let body = match response.json() {
        Ok(value) => value,
        // Not JSON: XML, CSV, text.
        Err(_) if !ctx.out.agent && !args.include => return ctx.out.text(&response.text()),
        Err(_) => response.text().into(),
    };
    if args.include {
        let headers: serde_json::Map<String, serde_json::Value> = response
            .headers
            .iter()
            .filter(|(n, _)| n.as_str() != "set-cookie")
            .map(|(n, v)| (n.to_string(), v.to_str().unwrap_or_default().into()))
            .collect();
        return ctx.out.item(
            serde_json::json!({ "status": response.status, "headers": headers, "body": body }),
        );
    }
    match ctx.out.human() {
        true => ctx.out.text(&serde_json::to_string_pretty(&body)?),
        false => ctx.out.item(body),
    }
}

pub fn parse_method(text: &str) -> Result<Method> {
    Ok(match text.to_ascii_uppercase().as_str() {
        "GET" => Method::GET,
        "POST" => Method::POST,
        "PUT" => Method::PUT,
        "PATCH" => Method::PATCH,
        "DELETE" => Method::DELETE,
        "HEAD" => Method::HEAD,
        "OPTIONS" => Method::OPTIONS,
        _ => {
            return Err(Error::invalid(format!(
                "you passed the method `{text}`; use GET, POST, PUT, PATCH, DELETE, HEAD or OPTIONS"
            )));
        }
    })
}

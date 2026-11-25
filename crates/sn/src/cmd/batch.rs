//! Several requests in one round trip (Batch API).

use crate::ctx::{Ctx, read_input};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use clap::Args;
use serde_json::{Value, json};
use sn_core::client::{Method, Request};
use sn_core::{Error, Result, now_unix};

#[derive(Args, Debug)]
pub struct BatchArgs {
    /// Requests: "GET /api/now/table/incident?sysparm_limit=1" (repeatable).
    pub requests: Vec<String>,
    /// JSON array of {id, method, url, headers, body}, @file, or - for stdin.
    #[arg(long, short = 'D', value_name = "JSON")]
    pub data: Option<String>,
    /// Run the requests one after the other, in order.
    #[arg(long)]
    pub ordered: bool,
}

pub async fn run(ctx: &Ctx, args: BatchArgs) -> Result<()> {
    let mut entries = Vec::new();
    for text in &args.requests {
        let (method, url) = text.trim().split_once(char::is_whitespace).ok_or_else(|| {
            Error::invalid(format!("you passed `{text}`; write each request as \"METHOD /path\", for example \"GET /api/now/table/incident?sysparm_limit=1\""))
        })?;
        entries.push(
            json!({ "method": super::raw::parse_method(method)?.as_str(), "url": url.trim() }),
        );
    }
    if let Some(data) = &args.data {
        match super::args::parse_json(&read_input(data)?, "--data")? {
            Value::Array(items) => entries.extend(items),
            _ => {
                return Err(Error::invalid(
                    "--data must be a JSON array of {method, url, headers, body}",
                ));
            }
        }
    }
    if entries.is_empty() {
        return Err(Error::Input {
            arg: "requests".into(),
            message: "there is no request; pass \"GET /api/...\" arguments or --data".into(),
            choices: Vec::new(),
        });
    }
    let mut only_reads = true;
    let requests = entries
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let method = super::raw::parse_method(entry["method"].as_str().unwrap_or("GET"))?;
            only_reads &= matches!(method, Method::GET | Method::HEAD);
            let url = entry["url"].as_str().filter(|u| u.starts_with('/')).ok_or_else(|| {
                Error::invalid(format!("request {} has no url that starts with /; add one, for example /api/now/table/incident", index + 1))
            })?;
            let mut headers = vec![json!({ "name": "Accept", "value": "application/json" })];
            if let Some(map) = entry["headers"].as_object() {
                headers.extend(map.iter().map(|(n, v)| json!({ "name": n, "value": v })));
            }
            let mut request = json!({
                "id": entry.get("id").cloned().unwrap_or_else(|| (index + 1).to_string().into()),
                "method": method.as_str(), "url": url, "headers": headers,
            });
            if let Some(body) = entry.get("body").filter(|b| !b.is_null()) {
                request["body"] = STANDARD.encode(body.to_string()).into();
                if let Some(list) = request["headers"].as_array_mut() {
                    list.push(json!({ "name": "Content-Type", "value": "application/json" }));
                }
            }
            Ok(request)
        })
        .collect::<Result<Vec<_>>>()?;
    let mut request = Request::new(Method::POST, "/api/now/v1/batch").json(json!({
        "batch_request_id": now_unix().to_string(),
        "enforce_order": args.ordered.to_string(),
        "rest_requests": requests,
    }));
    if only_reads {
        request = request.reads();
    }
    let answer = ctx.client().await?.send(request).await?.json()?;
    let mut rows = Vec::new();
    for served in answer["serviced_requests"].as_array().into_iter().flatten() {
        let body = served["body"]
            .as_str()
            .and_then(|b| STANDARD.decode(b).ok())
            .map(|b| {
                serde_json::from_slice(&b)
                    .unwrap_or_else(|_| String::from_utf8_lossy(&b).into_owned().into())
            })
            .unwrap_or(Value::Null);
        rows.push(json!({ "id": served["id"], "status": served["status_code"], "ms": served["execution_time"], "body": body }));
    }
    for id in answer["unserviced_requests"]
        .as_array()
        .into_iter()
        .flatten()
    {
        rows.push(
            json!({ "id": id, "status": null, "body": "not served: the batch ran out of time" }),
        );
    }
    rows.sort_by_key(|r| {
        r["id"]
            .as_str()
            .and_then(|i| i.parse::<u64>().ok())
            .unwrap_or(u64::MAX)
    });
    ctx.out.list(rows, &[], json!({}))
}

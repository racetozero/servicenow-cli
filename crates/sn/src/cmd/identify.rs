//! Identification and reconciliation of configuration items.

use super::args::{BodyArgs, show};
use crate::ctx::Ctx;
use clap::{Args, Subcommand};
use sn_core::Result;
use sn_core::client::{Method, Request};

#[derive(Subcommand, Debug)]
pub enum IdentifyCommand {
    /// Create or update configuration items: --data '{"items":[{"className":"cmdb_ci_server","values":{...}}]}'.
    CreateUpdate(PlainArgs),
    /// Show what would be created or updated; changes nothing.
    Query(PlainArgs),
    /// Create or update with options.
    CreateUpdateEnhanced(EnhancedArgs),
    /// Show what would change, with options; changes nothing.
    QueryEnhanced(EnhancedArgs),
}

#[derive(Args, Debug)]
pub struct PlainArgs {
    #[command(flatten)]
    body: BodyArgs,
    /// Discovery source that the data comes from.
    #[arg(long, value_name = "NAME", alias = "sysparm-data-source")]
    data_source: Option<String>,
}

#[derive(Args, Debug)]
pub struct EnhancedArgs {
    #[command(flatten)]
    plain: PlainArgs,
    /// Options such as partial_payloads:true,partial_commits:true.
    #[arg(long, value_name = "NAME:VALUE,...")]
    options: Option<String>,
}

pub async fn run(ctx: &Ctx, command: IdentifyCommand) -> Result<()> {
    let (path, args, options, reads) = match command {
        IdentifyCommand::CreateUpdate(args) => ("", args, None, false),
        IdentifyCommand::Query(args) => ("/query", args, None, true),
        IdentifyCommand::CreateUpdateEnhanced(args) => {
            ("/enhanced", args.plain, args.options, false)
        }
        IdentifyCommand::QueryEnhanced(args) => ("/queryEnhanced", args.plain, args.options, true),
    };
    let mut request = Request::new(Method::POST, format!("/api/now/identifyreconcile{path}"))
        .query_opt("sysparm_data_source", args.data_source)
        .query_opt("options", options)
        .json(args.body.required()?.into());
    if reads {
        request = request.reads();
    }
    let result = ctx.client().await?.send(request).await?.result()?;
    show(ctx, result)
}

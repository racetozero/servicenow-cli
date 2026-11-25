//! Opens a record or a list in the person's default browser.

use crate::browser;
use crate::ctx::Ctx;
use clap::Args;
use serde_json::json;
use sn_core::refs::Record;
use sn_core::{Error, Result, query, refs};

#[derive(Args, Debug)]
pub struct OpenArgs {
    /// Table (its list), or a record: a number, table:number, table:sys_id, or a link.
    #[arg(value_name = "TABLE|RECORD")]
    pub table: String,
    /// sys_id or number when the first argument is a table.
    #[arg(value_name = "SYS_ID|NUMBER", conflicts_with = "query")]
    pub id: Option<String>,
    /// Filter of the list, for example active=true^priority=1.
    #[arg(long, short = 'q', value_name = "QUERY", alias = "sysparm-query")]
    pub query: Option<String>,
    /// Print the address and open nothing.
    #[arg(long)]
    pub print_url: bool,
}

pub async fn run(ctx: &Ctx, args: OpenArgs) -> Result<()> {
    let names_record = args.id.is_some()
        || args.table.contains(':')
        || (args.query.is_none() && refs::prefix(&args.table).is_some());
    if names_record && args.query.is_some() {
        return Err(Error::invalid(
            "--query filters a list, and you named a record; pass a table with --query, or the record alone",
        ));
    }
    let client = ctx.client().await?;
    let target = if names_record {
        // Only a number needs the instance.
        let Record { table, sys_id } =
            refs::record(client, &args.table, args.id.as_deref()).await?;
        format!("/{table}.do?sys_id={sys_id}")
    } else {
        query::check_name("table", &args.table)?;
        match args.query.as_deref().filter(|q| !q.is_empty()) {
            Some(query) => format!(
                "/{}_list.do?sysparm_query={}",
                args.table,
                sn_core::percent::encode(query)
            ),
            None => format!("/{}_list.do", args.table),
        }
    };
    // nav_to.do decodes its uri once, the list its query once more.
    let url = format!(
        "{}/nav_to.do?uri={}",
        client.base(),
        sn_core::percent::encode(&target)
    );
    if args.print_url && !ctx.out.agent {
        return ctx.out.text(&url);
    }
    let opened = !args.print_url && !ctx.out.agent && browser::open(&url);
    if !opened && !args.print_url && !ctx.out.agent {
        ctx.out
            .note("no browser is available here; open this address:");
    }
    ctx.out.done(json!({ "opened": opened, "url": url }), &url)
}

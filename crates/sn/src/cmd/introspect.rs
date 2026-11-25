//! The command tree as JSON, for tools and agents.

use crate::cli::Cli;
use crate::ctx::Ctx;
use crate::help;
use clap::{Args, CommandFactory};
use serde_json::{Value, json};
use sn_core::{Error, Result};
use std::collections::HashMap;

#[derive(Args, Debug)]
pub struct IntrospectArgs {
    /// Only this command, for example `table list`.
    pub command: Vec<String>,
    /// Only names, summaries and effects.
    #[arg(long)]
    pub brief: bool,
}

pub fn run(ctx: &Ctx, args: IntrospectArgs) -> Result<()> {
    let root = Cli::command();
    let mut at = &root;
    let mut path = Vec::new();
    for word in &args.command {
        at = at.find_subcommand(word).ok_or_else(|| {
            let names: Vec<&str> = at.get_subcommands().map(clap::Command::get_name).collect();
            Error::invalid(format!(
                "there is no command `{word}` here; pass one of: {}",
                names.join(", ")
            ))
        })?;
        // The word can be an alias.
        path.push(at.get_name());
    }
    let path = path.join(" ");
    if args.brief {
        let mut rows = Vec::new();
        leaves(at, &path, &help::entries(), &mut rows);
        return ctx.out.list(
            rows,
            &[
                crate::output::col("COMMAND", "command"),
                crate::output::col("EFFECT", "effect"),
                crate::output::flex("SUMMARY", "summary"),
            ],
            json!({}),
        );
    }
    let data = describe(at, &path, &help::entries(), path.is_empty());
    match ctx.out.agent {
        true => ctx.out.item(data),
        false => ctx.out.text(&serde_json::to_string(&data)?),
    }
}

type Entries = HashMap<&'static str, help::Entry>;

fn leaves(command: &clap::Command, path: &str, entries: &Entries, rows: &mut Vec<Value>) {
    let mut any = false;
    for sub in command.get_subcommands().filter(|s| !s.is_hide_set()) {
        any = true;
        leaves(
            sub,
            format!("{path} {}", sub.get_name()).trim(),
            entries,
            rows,
        );
    }
    if !any {
        rows.push(json!({
            "command": path,
            "summary": command.get_about().map(ToString::to_string),
            "effect": effect(path, entries),
        }));
    }
}

fn effect(path: &str, entries: &Entries) -> Option<&'static str> {
    Some(entries.get(path)?.effect?.name())
}

fn describe(command: &clap::Command, path: &str, entries: &Entries, with_globals: bool) -> Value {
    let args: Vec<Value> = command
        .get_arguments()
        .filter(|a| !a.is_hide_set() && (with_globals || !a.is_global_set()))
        .map(|arg| {
            let values: Vec<String> = arg.get_possible_values().iter().map(|v| v.get_name().to_owned()).collect();
            let takes_value = arg.get_action().takes_values();
            let mut out = json!({
                "name": arg.get_id().as_str(),
                "flag": arg.get_long().map(|l| format!("--{l}")),
                "short": arg.get_short().map(|s| format!("-{s}")),
                "help": arg.get_help().map(ToString::to_string),
                "required": arg.is_required_set(),
                "takes_value": takes_value,
                "repeatable": matches!(arg.get_action(), clap::ArgAction::Append | clap::ArgAction::Count),
            });
            if takes_value && !values.is_empty() {
                out["values"] = values.into();
            }
            let defaults: Vec<String> = arg.get_default_values().iter().map(|v| v.to_string_lossy().into_owned()).collect();
            if takes_value && !defaults.is_empty() {
                out["default"] = defaults.join(",").into();
            }
            out
        })
        .collect();
    let commands: Vec<Value> = command
        .get_subcommands()
        .filter(|s| !s.is_hide_set())
        .map(|s| describe(s, format!("{path} {}", s.get_name()).trim(), entries, false))
        .collect();
    let aliases: Vec<&str> = command.get_visible_aliases().collect();
    let mut out = json!({ "name": command.get_name(), "summary": command.get_about().map(ToString::to_string), "args": args });
    if let Some(entry) = entries.get(path) {
        out["description"] = entry.description.as_str().into();
        out["effect"] = effect(path, entries).into();
        out["examples"] = entry.examples.iter().map(|(_, e)| *e).collect();
    }
    if !aliases.is_empty() {
        out["aliases"] = aliases.into();
    }
    if !commands.is_empty() {
        out["commands"] = commands.into();
    }
    out
}

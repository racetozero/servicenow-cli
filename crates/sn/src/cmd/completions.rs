//! Shell completion scripts.

use super::cache::Index;
use crate::ctx::Ctx;
use clap::{Args, Command, ValueEnum};
use clap_complete::engine::{ArgValueCompleter, CompletionCandidate};
use sn_core::config::Store;
use sn_core::{Error, Result};
use std::ffi::OsStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Shell {
    Bash,
    Elvish,
    Fish,
    #[value(alias = "pwsh")]
    Powershell,
    Zsh,
}

#[derive(Args, Debug)]
pub struct CompletionsArgs {
    /// Shell to print the script for.
    pub shell: Shell,
    /// Accepted for compatibility: completion always asks the CLI, table and column names included.
    #[arg(long, hide = true)]
    pub dynamic: bool,
}

pub fn run(ctx: &Ctx, args: CompletionsArgs) -> Result<()> {
    use clap_complete::env::{Bash, Elvish, EnvCompleter, Fish, Powershell, Zsh};
    let completer: &dyn EnvCompleter = match args.shell {
        Shell::Bash => &Bash,
        Shell::Elvish => &Elvish,
        Shell::Fish => &Fish,
        Shell::Powershell => &Powershell,
        Shell::Zsh => &Zsh,
    };
    let mut script = Vec::new();
    completer
        .write_registration("SN_COMPLETE", "sn", "sn", "sn", &mut script)
        .map_err(|err| Error::Internal(format!("cannot write the completion script: {err}")))?;
    ctx.out.text(&String::from_utf8_lossy(&script))
}

/// Adds table, column and profile names to the command tree. They come from
/// the offline copy (`sn cache refresh`); completion never uses the
/// network.
pub fn with_candidates(command: Command) -> Command {
    command
        .mut_args(|arg| match arg.get_id().as_str() {
            "table" => arg.add(ArgValueCompleter::new(tables)),
            "class" => arg.add(ArgValueCompleter::new(classes)),
            "fields" | "field" | "group_by" | "on_change" => {
                arg.add(ArgValueCompleter::new(columns))
            }
            "profile" => arg.add(ArgValueCompleter::new(profiles)),
            _ => arg,
        })
        .mut_subcommands(with_candidates)
}

/// The words being completed, after the `--` that the shell script adds.
pub fn words() -> Vec<String> {
    std::env::args().skip_while(|a| a != "--").skip(1).collect()
}

/// The offline copy of the selected profile's instance. Reads no credential.
fn index() -> Option<Index> {
    let store = Store::open().ok()?;
    let config = store.load().ok()?;
    let words = words();
    let flag = words
        .windows(2)
        .find(|w| w[0] == "-p" || w[0] == "--profile")
        .map(|w| w[1].clone());
    let env = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
    let name = flag
        .or_else(|| env("SN_PROFILE"))
        .or(config.default_profile);
    let instance = match env("SN_INSTANCE") {
        Some(text) => sn_core::config::normalize_instance(&text).ok()?,
        None => {
            let profile = match &name {
                Some(name) => config.profiles.get(name),
                None => config
                    .profiles
                    .values()
                    .next()
                    .filter(|_| config.profiles.len() == 1),
            };
            profile?.instance.clone()
        }
    };
    super::cache::load(&store, &instance).ok()?
}

fn table_candidates(current: &OsStr, only: &str) -> Vec<CompletionCandidate> {
    let (Some(index), Some(current)) = (index(), current.to_str()) else {
        return Vec::new();
    };
    index
        .table_names(current)
        .filter(|name| name.starts_with(only))
        .map(CompletionCandidate::new)
        .collect()
}

fn tables(current: &OsStr) -> Vec<CompletionCandidate> {
    table_candidates(current, "")
}

fn classes(current: &OsStr) -> Vec<CompletionCandidate> {
    table_candidates(current, "cmdb")
}

/// Columns of the first table on the line; `a,b,` completes the next one.
fn columns(current: &OsStr) -> Vec<CompletionCandidate> {
    let (Some(index), Some(current)) = (index(), current.to_str()) else {
        return Vec::new();
    };
    let (done, part) = match current.rfind(',') {
        Some(at) => current.split_at(at + 1),
        None => ("", current),
    };
    words()
        .iter()
        .filter(|w| !w.starts_with('-'))
        .filter_map(|w| index.columns(w.split(':').next().unwrap_or(w)))
        .find(|columns| !columns.is_empty())
        .unwrap_or_default()
        .into_iter()
        .filter(|c| c.starts_with(part))
        .map(|c| CompletionCandidate::new(format!("{done}{c}")))
        .collect()
}

fn profiles(current: &OsStr) -> Vec<CompletionCandidate> {
    let config = Store::open().and_then(|s| s.load()).unwrap_or_default();
    let current = current.to_string_lossy();
    config
        .profiles
        .into_iter()
        .filter(|(name, _)| name.starts_with(current.as_ref()))
        .map(|(name, p)| CompletionCandidate::new(name).help(Some(p.instance.into())))
        .collect()
}

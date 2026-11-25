//! The full-screen workspace: its start, and what it needs from outside the
//! full screen (sign-in, profiles, the browser).

use crate::ctx::Ctx;
use crate::{browser, cmd::auth};
use sn_core::client::{Debug, Options};
use sn_core::config::{self, ConfigFile};
use sn_core::{Client, Error, Result, query};
use sn_tui::{Exit, Hooks, Profile, Session};
use std::io::{IsTerminal, Write};
use std::path::Path;
use std::sync::Arc;

#[derive(clap::Args, Debug)]
pub struct TuiArgs {
    /// Table to start with (default: your work).
    pub table: Option<String>,
    /// Encoded query to start with, for example active=true^priority=1.
    #[arg(long, short = 'q', value_name = "QUERY", alias = "sysparm-query")]
    pub query: Option<String>,
    /// Rows to load at once.
    #[arg(long, default_value_t = 50, value_name = "N", value_parser = clap::value_parser!(u64).range(5..=200))]
    pub page_size: u64,
    /// View of the table; it decides the columns.
    #[arg(long, value_name = "NAME")]
    pub view: Option<String>,
}

pub async fn run(ctx: &Ctx, args: TuiArgs) -> Result<()> {
    if ctx.out.agent {
        return Err(Error::invalid(
            "you started the full-screen workspace in agent mode, which never opens one; use the plain commands instead, for example `sn incident mine --agent`",
        ));
    }
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return Err(Error::invalid(
            "you started the full-screen workspace without a terminal on stdin and stdout; run it in a terminal window, or use the plain commands, for example `sn table list incident`",
        ));
    }
    if std::env::var("TERM").is_ok_and(|term| term == "dumb") {
        return Err(Error::invalid(
            "TERM is `dumb`, and the workspace needs a terminal that can move the cursor; set TERM to your terminal, for example TERM=xterm-256color",
        ));
    }
    if let Some(table) = &args.table {
        query::check_name("table", table)?;
    }
    // A query without a table searches incidents, the table people mean.
    let table = match (&args.table, &args.query, &args.view) {
        (None, None, None) => None,
        (table, _, _) => Some(table.clone().unwrap_or_else(|| "incident".into())),
    };
    let pins_path = ctx.store.dir().join("tui.json");
    let mut profile = ctx.globals.profile.clone();
    let mut failed: Option<String> = None;
    let mut options = sn_tui::Options {
        table,
        query: args.query,
        view: args.view,
        page_size: args.page_size,
        no_color: ctx.globals.no_color,
        download_dir: std::env::current_dir().unwrap_or_else(|_| ".".into()),
        pins: read_pins(&pins_path),
        resume: None,
    };
    loop {
        let session = match session(ctx, profile.as_deref(), failed.take()) {
            Ok(session) => session,
            // At the first start this is an error of the command line. Later
            // the workspace holds unsent work, so it stays and says why.
            Err(err) if options.resume.is_none() => return Err(err),
            Err(err) => Session::signed_out(
                profile.as_deref().unwrap_or(config::DEFAULT_PROFILE),
                "",
                err.to_string(),
                profiles(ctx),
            ),
        };
        let hooks = Hooks {
            open_url: Arc::new(browser::open),
            save_pins: {
                let path = pins_path.clone();
                let store = ctx.store.clone();
                Arc::new(move |pins| {
                    let _lock = store.lock()?;
                    save_pins(&path, pins)
                })
            },
        };
        let resume = match sn_tui::run(session, options, hooks).await? {
            Exit::Quit => return Ok(()),
            Exit::SignIn(resume) => {
                if let Err(err) = sign_in(ctx, profile.as_deref()).await {
                    failed = Some(format!("The sign-in did not work: {err}"));
                }
                resume
            }
            Exit::SwitchProfile(name, resume) => {
                profile = Some(name);
                resume
            }
        };
        options = sn_tui::Options {
            resume: Some(resume),
            ..sn_tui::Options::default()
        };
    }
}

/// The saved profiles. Without a readable file there are none to switch to.
fn profiles(ctx: &Ctx) -> Vec<Profile> {
    ctx.store
        .load()
        .unwrap_or_default()
        .profiles
        .iter()
        .map(|(name, p)| Profile {
            name: name.clone(),
            instance: p.instance.clone(),
            read_only: p.read_only,
        })
        .collect()
}

/// The connection of a profile, or the reason why there is none yet.
fn session(ctx: &Ctx, profile: Option<&str>, failed: Option<String>) -> Result<Session> {
    let profiles = profiles(ctx);
    let mut resolved = match config::resolve(&ctx.store, profile) {
        Ok(resolved) => resolved,
        Err(Error::Auth(reason)) => {
            let reason = failed.unwrap_or(reason);
            return Ok(Session::signed_out(
                profile.unwrap_or(config::DEFAULT_PROFILE),
                "",
                reason,
                profiles,
            ));
        }
        Err(err) => return Err(err),
    };
    ctx.globals.network(&mut resolved.profile.network);
    let client = Client::new(
        resolved,
        Some(ctx.store.clone()),
        Options {
            // Request lines on stderr would break the screen.
            debug: Debug::Off,
            ..ctx.options()
        },
    )?;
    let session = Session::connected(client, profiles);
    // The old credential may still work, so it gets its chance.
    Ok(match failed {
        Some(reason) => session.with_notice(reason),
        None => session,
    })
}

/// Signs in on the normal screen. The terminal has been restored.
async fn sign_in(ctx: &Ctx, profile: Option<&str>) -> Result<()> {
    if let Ok(resolved) = config::resolve(&ctx.store, profile)
        && resolved.ephemeral
    {
        return Err(Error::invalid(
            "the credential comes from SN_* environment variables and the instance does not accept it; correct or unset them, then start the workspace again",
        ));
    }
    let file: ConfigFile = ctx.store.load()?;
    let mut globals = ctx.globals.clone();
    globals.profile = profile
        .map(str::to_owned)
        .or_else(|| file.default_profile.clone());
    let _ = writeln!(std::io::stderr());
    let result = auth::login(&Ctx::new(globals)?, auth::LoginArgs::defaults()?).await;
    if let Err(err) = &result {
        let _ = writeln!(std::io::stderr(), "\n{err}");
    }
    result
}

fn read_pins(path: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let value: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
    value["pins"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str())
        .filter(|t| query::check_name("table", t).is_ok())
        .map(str::to_owned)
        .collect()
}

fn save_pins(path: &Path, pins: &[String]) -> Result<()> {
    let text = serde_json::to_string_pretty(&serde_json::json!({ "pins": pins }))?;
    std::fs::write(path, text).map_err(|err| {
        Error::invalid(format!(
            "cannot write {} ({err}); check that you can write to the directory",
            path.display()
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pins_survive_and_bad_files_are_ignored() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("tui.json");
        assert!(read_pins(&path).is_empty());
        save_pins(&path, &["cmdb_ci".into(), "u_x".into()]).expect("save");
        assert_eq!(read_pins(&path), ["cmdb_ci", "u_x"]);
        std::fs::write(&path, r#"{"pins": ["a/../b", 3, "ok"]}"#).expect("write");
        assert_eq!(read_pins(&path), ["ok"]);
        std::fs::write(&path, "not json").expect("write");
        assert!(read_pins(&path).is_empty());
    }
}

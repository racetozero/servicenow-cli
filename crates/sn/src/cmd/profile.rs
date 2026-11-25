//! Profiles, the configuration file, and aliases.

use crate::ctx::Ctx;
use crate::output::{col, flex};
use clap::{Args, Subcommand};
use serde_json::{Value, json};
use sn_core::config::{AuthKind, ConfigFile, Grant, Profile, Secrets};
use sn_core::{Choice, Error, Result};

#[derive(Subcommand, Debug)]
pub enum ProfileCommand {
    /// Profiles and their instances.
    #[command(visible_alias = "ls")]
    List,
    /// Settings of one profile, without secrets.
    Show {
        /// Profile name (default: the selected profile).
        name: Option<String>,
    },
    /// Make a profile the default.
    Use {
        /// Profile name.
        name: Option<String>,
    },
    /// Sign in and save it under a name.
    Add {
        /// Profile name.
        name: String,
        #[command(flatten)]
        login: Box<super::auth::LoginArgs>,
    },
    /// Change settings of a profile.
    Set(SetArgs),
    /// Remove a profile and its credential.
    #[command(visible_alias = "rm")]
    Remove {
        /// Profile name.
        name: String,
    },
    /// Sign in again (the same as `auth login`).
    Login(Box<super::auth::LoginArgs>),
    /// Remove the stored credential (the same as `auth logout`).
    Logout,
    /// Sign-in state (the same as `auth status`).
    Status,
    /// New access token (the same as `auth refresh`).
    Refresh,
}

#[derive(Args, Debug)]
pub struct SetArgs {
    /// Profile name.
    name: String,
    /// Refuse writes (true or false).
    #[arg(long = "profile-read-only", value_name = "BOOL")]
    read_only: Option<bool>,
    /// Keep certificate checks off (true or false).
    #[arg(long = "profile-insecure", value_name = "BOOL")]
    insecure: Option<bool>,
    /// Hosts that bypass the proxy; empty removes the setting.
    #[arg(long, value_name = "HOSTS")]
    bypass_proxy: Option<String>,
    /// User name of the proxy; the password comes from stdin.
    #[arg(long, value_name = "NAME")]
    proxy_user: Option<String>,
}

#[derive(Subcommand, Debug)]
pub enum ConfigCommand {
    /// Where the configuration file is.
    Path,
    /// The configuration, without secrets.
    Show,
    /// Connect an instance step by step (the same as `init`).
    Init(Box<super::auth::LoginArgs>),
}

#[derive(Subcommand, Debug)]
pub enum AliasCommand {
    /// Aliases and what they run.
    #[command(visible_alias = "ls")]
    List,
    /// Add or replace an alias: alias set mine "incident mine --limit 5".
    Set {
        /// Alias name.
        name: String,
        /// Command without `sn`, quoted as one argument.
        command: String,
    },
    /// Remove an alias.
    #[command(visible_alias = "rm")]
    Delete {
        /// Alias name.
        name: String,
    },
}

pub async fn run(ctx: &Ctx, command: ProfileCommand) -> Result<()> {
    use super::auth::{self, AuthCommand};
    match command {
        ProfileCommand::List => list(ctx),
        ProfileCommand::Show { name } => show(ctx, name),
        ProfileCommand::Use { name } => switch(ctx, name),
        ProfileCommand::Add { name, login } => {
            if ctx.store.load()?.profiles.contains_key(&name)
                && login.instance.is_none()
                && !login.force
            {
                return Err(Error::invalid(format!(
                    "profile `{name}` exists; run `sn auth login --profile {name}` to sign in again, or pass --instance to replace it"
                )));
            }
            let mut globals = ctx.globals.clone();
            globals.profile = Some(name);
            auth::login(&Ctx::new(globals)?, *login).await
        }
        ProfileCommand::Set(args) => set(ctx, args),
        ProfileCommand::Remove { name } => remove(ctx, &name),
        ProfileCommand::Login(args) => auth::login(ctx, *args).await,
        ProfileCommand::Logout => auth::run(ctx, AuthCommand::Logout { local: false }).await,
        ProfileCommand::Status => auth::run(ctx, AuthCommand::Status { offline: false }).await,
        ProfileCommand::Refresh => auth::run(ctx, AuthCommand::Refresh).await,
    }
}

fn row(file: &ConfigFile, name: &str, profile: &Profile) -> Result<Value> {
    let mut row = json!({ "name": name, "default": file.default_profile.as_deref() == Some(name) });
    if let (Some(row), Value::Object(fields)) =
        (row.as_object_mut(), serde_json::to_value(profile)?)
    {
        row.extend(fields);
    }
    if let Some(proxy) = row["proxy"].as_str() {
        // A proxy URL can carry a password.
        row["proxy"] = match proxy.split_once('@') {
            Some((_, host)) => format!("[hidden]@{host}").into(),
            None => proxy.into(),
        };
    }
    Ok(row)
}

fn list(ctx: &Ctx) -> Result<()> {
    let file = ctx.store.load()?;
    let rows = file
        .profiles
        .iter()
        .map(|(n, p)| row(&file, n, p))
        .collect::<Result<Vec<_>>>()?;
    let cols = [
        col("NAME", "name"),
        col("DEFAULT", "default"),
        col("METHOD", "auth"),
        col("USER", "username"),
        flex("INSTANCE", "instance"),
    ];
    ctx.out.list(rows, &cols, json!({}))
}

/// The profile can sign in: with a stored secret, or with a method that
/// stores none.
pub fn has_credential(profile: &Profile, secrets: &Secrets) -> bool {
    *secrets != Secrets::default()
        || profile.token_command.is_some()
        || profile.auth == AuthKind::Certificate
        || profile
            .oauth
            .as_ref()
            .is_some_and(|o| o.grant == Grant::JwtBearer)
}

fn unknown(file: &ConfigFile, name: &str) -> Error {
    Error::Input {
        arg: "profile".into(),
        message: format!(
            "there is no profile `{name}`; pass one that exists, or create it with `sn auth login --profile {name}`"
        ),
        choices: super::auth::profile_choices(file),
    }
}

fn show(ctx: &Ctx, name: Option<String>) -> Result<()> {
    let file = ctx.store.load()?;
    let name = match name {
        Some(name) => name,
        None => ctx.resolve()?.name,
    };
    let profile = file
        .profiles
        .get(&name)
        .ok_or_else(|| unknown(&file, &name))?;
    let secrets = ctx.store.secrets(&name, profile)?;
    let mut data = row(&file, &name, profile)?;
    data["credential"] = has_credential(profile, &secrets).into();
    ctx.out.item(data)
}

pub fn switch(ctx: &Ctx, name: Option<String>) -> Result<()> {
    let name = match name.or_else(|| ctx.globals.profile.clone()) {
        Some(name) => name,
        None => {
            let choices: Vec<Choice> = super::auth::profile_choices(&ctx.store.load()?);
            ctx.need("name", "Which profile?", choices)?
        }
    };
    let _lock = ctx.store.lock()?;
    let mut file = ctx.store.load()?;
    if !file.profiles.contains_key(&name) {
        return Err(unknown(&file, &name));
    }
    file.default_profile = Some(name.clone());
    ctx.store.save(&file)?;
    ctx.out.done(
        json!({ "default_profile": name }),
        &format!("{name} is now the default profile."),
    )
}

fn set(ctx: &Ctx, args: SetArgs) -> Result<()> {
    let file = ctx.store.load()?;
    if !file.profiles.contains_key(&args.name) {
        return Err(unknown(&file, &args.name));
    }
    // Before the lock: the person may take a while to type it.
    let proxy_password = match &args.proxy_user {
        Some(_) => Some(crate::ctx::read_secret_stdin()?),
        None => None,
    };
    let _lock = ctx.store.lock()?;
    let mut file = ctx.store.load()?;
    let Some(profile) = file.profiles.get_mut(&args.name) else {
        return Err(unknown(&file, &args.name));
    };
    if let Some(value) = args.read_only {
        profile.read_only = value;
    }
    ctx.globals.network(&mut profile.network);
    if let Some(value) = args.insecure {
        profile.network.insecure = value;
    }
    if let Some(hosts) = args.bypass_proxy {
        profile.network.no_proxy = Some(hosts).filter(|h| !h.is_empty());
    }
    if ctx.globals.no_proxy {
        profile.network.proxy = None;
    }
    if let Some(user) = args.proxy_user {
        let mut secrets = ctx.store.secrets(&args.name, profile)?;
        secrets.proxy_user = Some(user);
        secrets.proxy_password = proxy_password;
        ctx.store.save_secrets(&args.name, profile, &secrets)?;
    }
    ctx.store.save(&file)?;
    let data = row(&file, &args.name, &file.profiles[&args.name])?;
    ctx.out.done(data, &format!("Saved profile {}.", args.name))
}

fn remove(ctx: &Ctx, name: &str) -> Result<()> {
    ctx.confirm(&format!("remove profile {name} and its credential"))?;
    let _lock = ctx.store.lock()?;
    let mut file = ctx.store.load()?;
    let Some(profile) = file.profiles.remove(name) else {
        return Err(unknown(&file, name));
    };
    ctx.store
        .save_secrets(name, &profile, &Secrets::default())?;
    if file.default_profile.as_deref() == Some(name) {
        file.default_profile = None;
    }
    ctx.store.save(&file)?;
    ctx.out.done(
        json!({ "profile": name, "removed": true }),
        &format!("Removed profile {name}."),
    )
}

pub async fn config(ctx: &Ctx, command: ConfigCommand) -> Result<()> {
    match command {
        ConfigCommand::Path => ctx.out.text(&ctx.store.config_path().display().to_string()),
        ConfigCommand::Show => {
            let file = ctx.store.load()?;
            let rows = file
                .profiles
                .iter()
                .map(|(n, p)| row(&file, n, p))
                .collect::<Result<Vec<_>>>()?;
            ctx.out.item(json!({
                "path": ctx.store.config_path().display().to_string(),
                "cache": ctx.store.cache_dir().display().to_string(),
                "default_profile": file.default_profile,
                "aliases": file.aliases,
                "profiles": rows,
            }))
        }
        ConfigCommand::Init(args) => super::auth::login(ctx, *args).await,
    }
}

pub fn alias(ctx: &Ctx, command: AliasCommand) -> Result<()> {
    match command {
        AliasCommand::List => {
            let file = ctx.store.load()?;
            let rows = file
                .aliases
                .iter()
                .map(|(n, c)| json!({ "name": n, "command": c }))
                .collect();
            ctx.out.list(
                rows,
                &[col("NAME", "name"), flex("COMMAND", "command")],
                json!({}),
            )
        }
        AliasCommand::Set { name, command } => {
            split_words(&command)?;
            if clap::Command::find_subcommand(
                &<crate::cli::Cli as clap::CommandFactory>::command(),
                &name,
            )
            .is_some()
            {
                return Err(Error::invalid(format!(
                    "`{name}` is a command already; choose another alias name"
                )));
            }
            let _lock = ctx.store.lock()?;
            let mut file = ctx.store.load()?;
            file.aliases.insert(name.clone(), command.clone());
            ctx.store.save(&file)?;
            ctx.out.done(
                json!({ "name": name, "command": command }),
                &format!("sn {name} now runs: sn {command}"),
            )
        }
        AliasCommand::Delete { name } => {
            let _lock = ctx.store.lock()?;
            let mut file = ctx.store.load()?;
            if file.aliases.remove(&name).is_none() {
                return Err(Error::NotFound(format!(
                    "there is no alias `{name}`; list them with `sn alias list`"
                )));
            }
            ctx.store.save(&file)?;
            ctx.out.done(
                json!({ "name": name, "removed": true }),
                &format!("Removed the alias {name}."),
            )
        }
    }
}

/// Splits a command line at spaces, with single and double quotes.
pub fn split_words(text: &str) -> Result<Vec<String>> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut quote = None;
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some('"') | None, '\\') => {
                word.extend(chars.next());
                in_word = true;
            }
            (None, '"' | '\'') => {
                quote = Some(c);
                in_word = true;
            }
            (None, c) if c.is_whitespace() => {
                if std::mem::take(&mut in_word) {
                    words.push(std::mem::take(&mut word));
                }
            }
            (_, c) => {
                word.push(c);
                in_word = true;
            }
        }
    }
    if quote.is_some() {
        return Err(Error::invalid(format!(
            "the command `{text}` has an open quote; close it"
        )));
    }
    if in_word {
        words.push(word);
    }
    Ok(words)
}

#[cfg(test)]
mod tests {
    use super::split_words;

    #[test]
    fn words_split_like_a_shell() {
        assert_eq!(
            split_words(r#"table list incident -q "a=1^b=x y" '' \"q"#).expect("split"),
            ["table", "list", "incident", "-q", "a=1^b=x y", "", "\"q"]
        );
        assert!(split_words("a 'b").is_err());
    }
}

//! Command-line parsing and dispatch.

use crate::cmd;
use crate::ctx::Ctx;
use crate::output::{self, Format};
use clap::error::ErrorKind;
use clap::{Args, CommandFactory, FromArgMatches, Parser, Subcommand};
use sn_core::config::{Network, Store, truthy};
use sn_core::{Error, Result};
use std::io::IsTerminal;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "sn",
    version,
    about = "ServiceNow from the terminal, for people and agents",
    long_about = "ServiceNow from the terminal, for people and agents.\n\n\
        Start:    sn auth login --instance acme\n\
        Look:     sn get INC0010001 · sn table list incident -q active=true\n\
        Work:     sn incident mine · sn change list · sn tui\n\
        Any API:  sn api search attachment · sn raw GET /api/now/table/incident\n\
        Agents:   add --agent (or set SN_AGENT=1) and read `sn skill`\n\
        Help:     sn COMMAND --help explains a command and shows examples\n\n\
        Output is a table in a terminal, JSON in a pipe, and one JSON envelope with --agent.\n\
        Exit codes: 0 ok, 1 API error, 2 sign-in or permission, 3 invalid input, 4 not found, 5 other, 130 interrupted.",
    subcommand_required = true,
    arg_required_else_help = true,
    disable_help_subcommand = true,
    // `--agent` twice (an alias or a wrapper adds one) is no mistake.
    args_override_self = true,
    max_term_width = 110
)]
pub struct Cli {
    #[command(flatten)]
    pub globals: GlobalArgs,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Args, Debug, Clone, Default)]
#[command(next_help_heading = "Global options")]
pub struct GlobalArgs {
    /// Profile to use (default: SN_PROFILE, then the default profile).
    #[arg(long, short = 'p', global = true, value_name = "NAME")]
    pub profile: Option<String>,
    /// Agent mode: one JSON envelope on stdout, errors too; never asks, never opens a full-screen view.
    #[arg(long, global = true, env = "SN_AGENT", value_parser = clap::builder::BoolishValueParser::new(), num_args = 0, default_missing_value = "true", default_value = "false")]
    pub agent: bool,
    /// Output format.
    #[arg(long, short = 'o', global = true, value_enum, default_value_t = Format::Auto, value_name = "FORMAT", visible_alias = "format")]
    pub output: Format,
    /// Same as --output json.
    #[arg(long, global = true)]
    pub json: bool,
    /// Keep only these fields, also as the columns (comma-separated; dots reach into objects).
    #[arg(
        long,
        global = true,
        value_delimiter = ',',
        value_name = "FIELDS",
        visible_alias = "columns"
    )]
    pub select: Vec<String>,
    /// Filter the output with a jq expression.
    #[arg(long, global = true, value_name = "EXPR")]
    pub jq: Option<String>,
    /// Keep API responses as the server sent them, the result envelope included.
    #[arg(long, global = true)]
    pub raw: bool,
    /// Allow a destructive action without asking.
    #[arg(long, short = 'y', global = true)]
    pub yes: bool,
    /// Show the write request instead of sending it (reads still run).
    #[arg(long, global = true)]
    pub dry_run: bool,
    /// Refuse every write in this run.
    #[arg(long, global = true, env = "SN_READ_ONLY", value_parser = clap::builder::BoolishValueParser::new(), num_args = 0, default_missing_value = "true", default_value = "false")]
    pub read_only: bool,
    /// Never ask; fail and name what is missing.
    #[arg(long, global = true, alias = "non-interactive", env = "SN_NO_INPUT", value_parser = clap::builder::BoolishValueParser::new(), num_args = 0, default_missing_value = "true", default_value = "false")]
    pub no_input: bool,
    /// Seconds to wait for each request (default 60).
    #[arg(long, global = true, value_name = "SECONDS")]
    pub timeout: Option<u64>,
    /// Show requests on stderr; twice adds headers and bodies, with secrets hidden.
    #[arg(long, short = 'v', global = true, action = clap::ArgAction::Count)]
    pub verbose: u8,
    /// The same as -v, or -vv with =full.
    #[arg(long, global = true, value_name = "full", num_args = 0..=1, require_equals = true, default_missing_value = "requests", value_parser = ["requests", "full"])]
    pub debug: Option<String>,
    /// Hide status messages.
    #[arg(long, global = true)]
    pub quiet: bool,
    /// No colors (also NO_COLOR).
    #[arg(long, global = true)]
    pub no_color: bool,
    /// Leave out the header row of tables, CSV and TSV.
    #[arg(long, global = true)]
    pub no_headers: bool,
    /// Do not shorten table cells.
    #[arg(long, global = true)]
    pub no_truncate: bool,
    /// Do not use a pager for long text.
    #[arg(long, global = true)]
    pub no_pager: bool,
    /// Proxy URL (http://, https://, socks5://), or none.
    #[arg(long, global = true, value_name = "URL")]
    pub proxy: Option<String>,
    /// Ignore every configured proxy.
    #[arg(long, global = true, conflicts_with = "proxy")]
    pub no_proxy: bool,
    /// Trust these PEM certificates too (the system store is always used).
    #[arg(long, global = true, value_name = "FILE")]
    pub ca_cert: Option<PathBuf>,
    /// TLS client certificate and key in one PEM file.
    #[arg(long, global = true, value_name = "FILE")]
    pub client_cert: Option<PathBuf>,
    /// Do not verify the server certificate.
    #[arg(long, global = true)]
    pub insecure: bool,
}

impl GlobalArgs {
    /// Puts the network flags over the profile's settings.
    pub fn network(&self, network: &mut Network) {
        if let Some(proxy) = &self.proxy {
            network.proxy = Some(proxy.clone());
        }
        if self.no_proxy {
            network.proxy = Some("none".into());
        }
        if let Some(path) = &self.ca_cert {
            network.ca_cert = Some(path.clone());
        }
        if let Some(path) = &self.client_cert {
            network.client_cert = Some(path.clone());
        }
        network.insecure |= self.insecure;
    }
}

// Parsed once per process.
#[allow(clippy::large_enum_variant)]
#[derive(Subcommand, Debug)]
pub enum Command {
    /// Sign in, check, and sign out.
    #[command(subcommand, display_order = 1)]
    Auth(cmd::auth::AuthCommand),
    /// Connect an instance step by step (the same as `auth login`).
    #[command(display_order = 1)]
    Init(cmd::auth::LoginArgs),
    /// Saved instances and credentials.
    #[command(subcommand, display_order = 1, visible_alias = "profiles")]
    Profile(cmd::profile::ProfileCommand),
    /// Configuration file, its location, and settings.
    #[command(subcommand, display_order = 90)]
    Config(cmd::profile::ConfigCommand),
    /// Open the full-screen workspace.
    #[command(display_order = 2)]
    Tui(cmd::tui::TuiArgs),

    /// Read a record by number, link or sys_id, or one piece of your context.
    #[command(display_order = 10)]
    Get(cmd::get::GetArgs),
    /// Records of any table.
    #[command(subcommand, display_order = 10, visible_alias = "tables")]
    Table(cmd::table::TableCommand),
    /// Read fields of related records in one request (GraphQL).
    #[command(display_order = 11)]
    Gr(cmd::gr::GrArgs),
    /// Count, sum and average on the server.
    #[command(display_order = 11, visible_alias = "stats")]
    Aggregate(cmd::aggregate::AggregateArgs),
    /// Run a GraphQL document.
    #[command(display_order = 11)]
    Graphql(cmd::graphql::GraphqlArgs),
    /// Comments and work notes of a record.
    #[command(display_order = 12)]
    Journal(cmd::journal::JournalArgs),
    /// Catalog variables of a record.
    #[command(subcommand, display_order = 12)]
    Variables(cmd::variables::VariablesCommand),
    /// Files on records.
    #[command(subcommand, display_order = 12, visible_alias = "attachments")]
    Attachment(cmd::attachment::AttachmentCommand),
    /// Stream record changes as they happen.
    #[command(display_order = 13)]
    Watch(cmd::watch::WatchArgs),
    /// Open a record or list in the browser.
    #[command(display_order = 13)]
    Open(cmd::open::OpenArgs),
    /// Export and import records as XML.
    #[command(subcommand, display_order = 14)]
    Xml(cmd::xml::XmlCommand),
    /// Load data through import sets.
    #[command(subcommand, display_order = 14)]
    Import(cmd::import::ImportCommand),
    /// Find the sys_id of a user, group, or record.
    #[command(subcommand, display_order = 14)]
    Resolve(cmd::resolve::ResolveCommand),

    /// Incidents: find, create, update, resolve.
    #[command(subcommand, display_order = 20, visible_alias = "incidents")]
    Incident(cmd::incident::IncidentCommand),
    /// Change requests, tasks, conflicts and approvals.
    #[command(subcommand, display_order = 20)]
    Change(cmd::change::ChangeCommand),
    /// Approvals that wait for you.
    #[command(subcommand, display_order = 20, visible_alias = "approvals")]
    Approval(cmd::approval::ApprovalCommand),
    /// Service Catalog: browse, order, track.
    #[command(subcommand, display_order = 21)]
    Catalog(cmd::catalog::CatalogCommand),
    /// Knowledge articles.
    #[command(subcommand, display_order = 21, visible_alias = "kb")]
    Knowledge(cmd::knowledge::KnowledgeCommand),
    /// Configuration items and their relations.
    #[command(subcommand, display_order = 22)]
    Cmdb(cmd::cmdb::CmdbCommand),
    /// Identification and reconciliation of configuration items.
    #[command(subcommand, display_order = 22)]
    Identify(cmd::identify::IdentifyCommand),
    /// Performance Analytics scorecards.
    #[command(subcommand, display_order = 23)]
    Scores(cmd::scores::ScoresCommand),

    /// Tables, columns and choices of the instance.
    #[command(subcommand, display_order = 30)]
    Schema(cmd::schema::SchemaCommand),
    /// Choice values of a field.
    #[command(display_order = 30)]
    Choices(cmd::schema::ChoicesArgs),
    /// Offline copy of tables and columns.
    #[command(subcommand, display_order = 30)]
    Cache(cmd::cache::CacheCommand),
    /// Switch the current application scope or update set.
    #[command(subcommand, display_order = 31)]
    Set(cmd::set::SetCommand),
    /// Flow Designer: find, read, and debug flows.
    #[command(subcommand, display_order = 32)]
    Flow(cmd::flow::FlowCommand),
    /// Decision tables.
    #[command(subcommand, display_order = 32)]
    Decision(cmd::decision::DecisionCommand),
    /// Playbooks of a record.
    #[command(subcommand, display_order = 32)]
    Playbook(cmd::playbook::PlaybookCommand),
    /// Run server-side JavaScript.
    #[command(subcommand, display_order = 33)]
    Script(cmd::script::ScriptCommand),
    /// Run server-side JavaScript (the same as `script run`).
    #[command(display_order = 33)]
    Exec(cmd::script::RunArgs),
    /// Search the instance's code.
    #[command(display_order = 33)]
    Codesearch(cmd::codesearch::CodesearchArgs),
    /// Applications and plugins.
    #[command(subcommand, display_order = 34)]
    Appmanager(cmd::appmanager::AppmanagerCommand),
    /// Pipelines: apps, update sets, tests.
    #[command(subcommand, display_order = 34)]
    Cicd(cmd::cicd::CicdCommand),
    /// Run the official now-sdk with this profile's sign-in.
    #[command(display_order = 35)]
    Sdk(cmd::sdk::SdkArgs),
    /// Run one command as another user.
    #[command(display_order = 35)]
    Impersonate(cmd::impersonate::ImpersonateArgs),

    /// Every documented REST operation.
    #[command(display_order = 40)]
    Api(cmd::api::ApiArgs),
    /// Call any REST path.
    #[command(display_order = 40)]
    Raw(cmd::raw::RawArgs),
    /// Find the REST endpoints that this instance publishes.
    #[command(subcommand, display_order = 41)]
    Apisearch(cmd::apisearch::ApisearchCommand),
    /// Send several requests as one.
    #[command(display_order = 41)]
    Batch(cmd::batch::BatchArgs),

    /// Check the connection and who you are.
    #[command(display_order = 80)]
    Ping,
    /// Check roles, plugins, properties and the setup.
    #[command(display_order = 80)]
    Doctor(cmd::doctor::DoctorArgs),
    /// Install the newest release.
    #[command(display_order = 91)]
    Update(cmd::update::UpdateArgs),
    /// Shortcuts for commands.
    #[command(subcommand, display_order = 92)]
    Alias(cmd::profile::AliasCommand),
    /// Print or install the instructions for AI agents.
    #[command(display_order = 92)]
    Skill(cmd::skill::SkillArgs),
    /// Print the command tree as JSON.
    #[command(display_order = 92)]
    Introspect(cmd::introspect::IntrospectArgs),
    /// Print a shell completion script.
    #[command(display_order = 92, visible_alias = "completion")]
    Completions(cmd::completions::CompletionsArgs),
}

/// Global flags that take a value; used to find the command word.
const VALUE_FLAGS: &[&str] = &[
    "--profile",
    "-p",
    "--output",
    "-o",
    "--format",
    "--select",
    "--columns",
    "--jq",
    "--timeout",
    "--proxy",
    "--ca-cert",
    "--client-cert",
];

/// The index of the first word that is not a global flag or its value.
pub fn command_index(argv: &[String]) -> Option<usize> {
    let mut skip = false;
    for (index, arg) in argv.iter().enumerate().skip(1) {
        if std::mem::take(&mut skip) {
            continue;
        }
        if arg.starts_with('-') && arg.len() > 1 {
            if arg == "--" {
                return None;
            }
            skip = VALUE_FLAGS.contains(&arg.as_str());
            continue;
        }
        return Some(index);
    }
    None
}

/// Replaces a user-defined command word with its alias.
fn expand_alias(root: &clap::Command, mut argv: Vec<String>) -> Result<Vec<String>> {
    let Some(at) = command_index(&argv) else {
        return Ok(argv);
    };
    if root.find_subcommand(&argv[at]).is_some() {
        return Ok(argv);
    }
    let Ok(config) = Store::open().and_then(|s| s.load()) else {
        return Ok(argv);
    };
    if let Some(text) = config.aliases.get(&argv[at]) {
        let words = cmd::profile::split_words(text)?;
        argv.splice(at..=at, words);
    }
    Ok(argv)
}

/// Adds the read verb to `table incident`, `cmdb cmdb_ci_server SYS_ID`,
/// `schema incident` and `apisearch TEXT`.
/// Writes are never implied, and a mistyped verb stays an error.
fn implied_verb(root: &clap::Command, mut argv: Vec<String>) -> Vec<String> {
    let Some(at) = command_index(&argv) else {
        return argv;
    };
    let Some(group) = root
        .find_subcommand(&argv[at])
        .filter(|c| matches!(c.get_name(), "table" | "cmdb" | "apisearch" | "schema"))
    else {
        return argv;
    };
    let Some(word) = argv.get(at + 1).filter(|w| !w.starts_with('-')) else {
        return argv;
    };
    let mistyped = |verb: &str| {
        verb.len().abs_diff(word.len()) <= 1
            && word.chars().filter(|c| verb.contains(*c)).count() + 1 >= verb.len()
    };
    // Any word can be a search text.
    let searches = group.get_name() == "apisearch";
    let known = |c: &clap::Command| {
        c.get_name_and_visible_aliases().contains(&word.as_str())
            || (!searches && mistyped(c.get_name()))
    };
    if word == "help" || group.get_subcommands().any(known) {
        return argv;
    }
    let names_record = word.contains(':')
        || argv
            .get(at + 2)
            .is_some_and(|id| sn_core::is_sys_id(id) || sn_core::refs::prefix(id).is_some());
    let verb = match group.get_name() {
        "apisearch" => "search",
        "schema" => "columns",
        _ if names_record => "get",
        _ => "list",
    };
    argv.insert(at + 1, verb.to_owned());
    argv
}

pub async fn run(argv: Vec<String>) -> Result<()> {
    let before_dashes = argv.iter().skip(1).take_while(|a| *a != "--");
    let agent = before_dashes.clone().any(|a| a == "--agent")
        || std::env::var("SN_AGENT").is_ok_and(|v| truthy(&v));
    output::set_agent(agent);
    if agent || before_dashes.clone().any(|a| a == "--no-color") {
        output::disable_color();
    }
    // Building the command tree costs about a millisecond: once is enough.
    let root = Cli::command();
    let argv = expand_alias(&root, argv)?;
    let argv = implied_verb(&root, argv);
    output::set_command(
        command_index(&argv)
            .map(|at| argv[at].clone())
            .unwrap_or_default(),
    );
    let (cli, name) = match parse(root, &argv) {
        Ok(parsed) => parsed,
        Err(err) => return usage(err, &argv),
    };
    output::set_command(name);
    let ctx = Ctx::new(cli.globals)?;
    let result = dispatch(&ctx, cli.command).await;
    if result.is_ok()
        && let Some(client) = ctx.built_client()
    {
        client.finish().await?;
    }
    result
}

/// Adds the API catalog when the command line uses it.
pub fn with_api(command: clap::Command, argv: &[String]) -> clap::Command {
    match command_index(argv).filter(|at| argv[*at] == "api") {
        Some(at) => command.mut_subcommand("api", |api| cmd::api::tree(api, &argv[at + 1..])),
        None => command,
    }
}

/// The parsed command line and the command's name, for example `table list`.
fn parse(command: clap::Command, argv: &[String]) -> Result<(Cli, String), clap::Error> {
    let matches = with_api(command, argv).try_get_matches_from(argv)?;
    let mut path = Vec::new();
    let mut at = &matches;
    while let Some((name, next)) = at.subcommand() {
        path.push(name);
        at = next;
    }
    Ok((Cli::from_arg_matches(&matches)?, path.join(" ")))
}

/// Help and version print as they are. Other parse errors follow the error
/// contract: an envelope for agents, JSON in a pipe, clap's text otherwise.
fn usage(mut err: clap::Error, argv: &[String]) -> Result<()> {
    if matches!(
        err.kind(),
        ErrorKind::DisplayHelp | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
    ) && let Err(full) =
        crate::help::enrich(with_api(Cli::command(), argv)).try_get_matches_from(argv)
    {
        err = full;
    }
    let wants_help = matches!(
        err.kind(),
        ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
    );
    if wants_help || (!output::agent_mode() && std::io::stderr().is_terminal()) {
        let _ = err.print();
        if wants_help {
            return Ok(());
        }
        std::process::exit(3);
    }
    let text = match err.kind() {
        ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => {
            let words: Vec<&str> = argv[1..]
                .iter()
                .map(String::as_str)
                .filter(|a| !a.starts_with('-'))
                .collect();
            let line = [&["sn"], &words[..]].concat().join(" ");
            format!(
                "you passed `{line}`, which needs a command or a value; run `{line} --help` for the list"
            )
        }
        _ => err.render().to_string(),
    };
    let lines: Vec<&str> = text
        .lines()
        .map(|l| l.trim().trim_prefix("error: "))
        .filter(|l| !l.is_empty())
        .collect();
    Err(Error::invalid(lines.join("; ")))
}

async fn dispatch(ctx: &Ctx, command: Command) -> Result<()> {
    match command {
        Command::Auth(c) => cmd::auth::run(ctx, c).await,
        Command::Init(c) => cmd::auth::login(ctx, c).await,
        Command::Profile(c) => cmd::profile::run(ctx, c).await,
        Command::Config(c) => cmd::profile::config(ctx, c).await,
        Command::Alias(c) => cmd::profile::alias(ctx, c),
        Command::Tui(c) => cmd::tui::run(ctx, c).await,
        Command::Get(c) => cmd::get::run(ctx, c).await,
        Command::Table(c) => cmd::table::run(ctx, c).await,
        Command::Gr(c) => cmd::gr::run(ctx, c).await,
        Command::Aggregate(c) => cmd::aggregate::run(ctx, c).await,
        Command::Graphql(c) => cmd::graphql::run(ctx, c).await,
        Command::Journal(c) => cmd::journal::run(ctx, c).await,
        Command::Variables(c) => cmd::variables::run(ctx, c).await,
        Command::Attachment(c) => cmd::attachment::run(ctx, c).await,
        Command::Watch(c) => cmd::watch::run(ctx, c).await,
        Command::Open(c) => cmd::open::run(ctx, c).await,
        Command::Xml(c) => cmd::xml::run(ctx, c).await,
        Command::Import(c) => cmd::import::run(ctx, c).await,
        Command::Resolve(c) => cmd::resolve::run(ctx, c).await,
        Command::Incident(c) => cmd::incident::run(ctx, c).await,
        Command::Change(c) => cmd::change::run(ctx, c).await,
        Command::Approval(c) => cmd::approval::run(ctx, c).await,
        Command::Catalog(c) => cmd::catalog::run(ctx, c).await,
        Command::Knowledge(c) => cmd::knowledge::run(ctx, c).await,
        Command::Cmdb(c) => cmd::cmdb::run(ctx, c).await,
        Command::Identify(c) => cmd::identify::run(ctx, c).await,
        Command::Scores(c) => cmd::scores::run(ctx, c).await,
        Command::Schema(c) => cmd::schema::run(ctx, c).await,
        Command::Choices(c) => cmd::schema::choices(ctx, c).await,
        Command::Cache(c) => cmd::cache::run(ctx, c).await,
        Command::Set(c) => cmd::set::run(ctx, c).await,
        Command::Flow(c) => cmd::flow::run(ctx, c).await,
        Command::Decision(c) => cmd::decision::run(ctx, c).await,
        Command::Playbook(c) => cmd::playbook::run(ctx, c).await,
        Command::Script(c) => cmd::script::run(ctx, c).await,
        Command::Exec(c) => cmd::script::exec(ctx, c).await,
        Command::Codesearch(c) => cmd::codesearch::run(ctx, c).await,
        Command::Appmanager(c) => cmd::appmanager::run(ctx, c).await,
        Command::Cicd(c) => cmd::cicd::run(ctx, c).await,
        Command::Sdk(c) => cmd::sdk::run(ctx, c).await,
        Command::Impersonate(c) => cmd::impersonate::run(ctx, c).await,
        Command::Api(c) => cmd::api::run(ctx, &c.0).await,
        Command::Raw(c) => cmd::raw::run(ctx, c).await,
        Command::Apisearch(c) => cmd::apisearch::run(ctx, c).await,
        Command::Batch(c) => cmd::batch::run(ctx, c).await,
        Command::Ping => cmd::doctor::ping(ctx).await,
        Command::Doctor(c) => cmd::doctor::run(ctx, c).await,
        Command::Update(c) => cmd::update::run(ctx, c).await,
        Command::Skill(c) => cmd::skill::run(ctx, c),
        Command::Introspect(c) => cmd::introspect::run(ctx, c),
        Command::Completions(c) => cmd::completions::run(ctx, c),
    }
}

/// The command tree for shell completion, with the API being completed.
pub fn completion_command() -> clap::Command {
    cmd::completions::with_candidates(with_api(Cli::command(), &cmd::completions::words()))
}

#[cfg(test)]
pub fn root() -> clap::Command {
    Cli::command()
}

/// The command that a line runs.
#[cfg(test)]
pub fn parse_name(argv: Vec<String>) -> Result<String, clap::Error> {
    let root = Cli::command();
    let argv = implied_verb(&root, argv);
    parse(root, &argv).map(|(_, name)| name)
}

/// Parses a command that runs inside this process (`impersonate`) and names
/// it. `None` means that help was printed.
pub fn parse_inner(argv: Vec<String>) -> Result<Option<(Cli, String)>> {
    // Building the command tree costs about a millisecond: once is enough.
    let root = Cli::command();
    let argv = expand_alias(&root, argv)?;
    let argv = implied_verb(&root, argv);
    match parse(root, &argv) {
        Ok(parsed) => Ok(Some(parsed)),
        Err(err) => usage(err, &argv).map(|()| None),
    }
}

/// Runs a parsed command in a prepared context. Boxed, because a command can
/// run another one.
pub fn run_inner<'a>(
    ctx: &'a Ctx,
    command: Command,
) -> std::pin::Pin<Box<dyn Future<Output = Result<()>> + 'a>> {
    Box::pin(dispatch(ctx, command))
}

//! The CI/CD API (`/api/sn_cicd`): applications, update sets, tests, scans,
//! plugins and source control. Most operations run in the background and
//! are followed through a progress record.

use super::args::{parse_json, show};
use super::lookup::one;
use crate::ctx::{Ctx, read_input};
use clap::{Args, Subcommand, ValueEnum};
use serde_json::{Value, json};
use sn_core::client::{Method, Request};
use sn_core::table::raw;
use sn_core::{Client, DisplayValue, Error, Result, TableQuery, query};
use std::time::{Duration, Instant};

const API: &str = "/api/sn_cicd";
const POLL: Duration = Duration::from_secs(2);

#[derive(Subcommand, Debug)]
pub enum CicdCommand {
    /// Applications of the application repository.
    #[command(subcommand)]
    App(AppCommand),
    /// Update sets.
    #[command(subcommand, visible_alias = "update-set")]
    Updateset(UpdateSetCommand),
    /// Test suites of the Automated Test Framework.
    #[command(subcommand, visible_alias = "testsuite")]
    Atf(SuiteCommand),
    /// Single tests of the Automated Test Framework.
    #[command(subcommand)]
    Test(TestCommand),
    /// Several packages in one installation.
    #[command(subcommand)]
    Batch(BatchCommand),
    /// Plugins.
    #[command(subcommand)]
    Plugin(PluginCommand),
    /// Instance scans.
    #[command(subcommand)]
    Scan(ScanCommand),
    /// Applications under source control.
    #[command(subcommand, name = "sc", visible_alias = "source-control")]
    SourceControl(SourceCommand),
    /// The state of an operation.
    Progress {
        /// Progress id that the operation returned.
        id: String,
        #[command(flatten)]
        wait: WaitArgs,
    },
}

#[derive(Args, Debug, Default, Clone)]
pub struct WaitArgs {
    /// Wait until the operation ends; a failed operation is an error.
    #[arg(long)]
    pub wait: bool,
    /// Give up waiting after this many seconds (0: never).
    #[arg(
        long,
        value_name = "SECONDS",
        default_value_t = 3600,
        requires = "wait",
        alias = "poll-timeout"
    )]
    pub wait_timeout: u64,
}

/// Which application: `--sys-id` or `--scope`, else the project here.
#[derive(Args, Debug)]
pub struct TargetArgs {
    /// sys_id of the application (default: now.config.json of the project here).
    #[arg(long, value_name = "SYS_ID", alias = "app-sys-id")]
    sys_id: Option<String>,
    /// Scope of the application, for example x_acme_app.
    #[arg(long, value_name = "SCOPE")]
    scope: Option<String>,
}

#[derive(Subcommand, Debug)]
pub enum AppCommand {
    /// Install an application from the repository.
    Install {
        #[command(flatten)]
        target: TargetArgs,
        /// Version to install (default: package.json of the project here, else the newest).
        #[arg(long, value_name = "VERSION", alias = "app-version")]
        version: Option<String>,
        /// Upgrade the base application when that is needed.
        #[arg(long)]
        auto_upgrade_base_app: bool,
        /// Version of the base application.
        #[arg(long, value_name = "VERSION")]
        base_app_version: Option<String>,
        #[command(flatten)]
        wait: WaitArgs,
    },
    /// Publish an application to the repository.
    Publish {
        #[command(flatten)]
        target: TargetArgs,
        /// Version to publish (default: package.json of the project here).
        #[arg(long, value_name = "VERSION", alias = "app-version")]
        version: Option<String>,
        /// Notes for this version.
        #[arg(long, value_name = "TEXT")]
        dev_notes: Option<String>,
        #[command(flatten)]
        wait: WaitArgs,
    },
    /// Go back to an earlier version.
    Rollback {
        #[command(flatten)]
        target: TargetArgs,
        /// Version the application has afterwards.
        #[arg(long, value_name = "VERSION", alias = "app-version")]
        version: String,
        #[command(flatten)]
        wait: WaitArgs,
    },
}

#[derive(Subcommand, Debug)]
pub enum UpdateSetCommand {
    /// Create an update set.
    Create {
        /// Name.
        #[arg(long, alias = "update-set-name")]
        name: String,
        /// Description.
        #[arg(long)]
        description: Option<String>,
        /// sys_id that the update set gets.
        #[arg(long, value_name = "SYS_ID")]
        sys_id: Option<String>,
        /// Application scope.
        #[arg(long)]
        scope: Option<String>,
    },
    /// Load an update set from another instance.
    Retrieve {
        /// sys_id of the update set there.
        #[arg(long, value_name = "SYS_ID")]
        update_set_id: String,
        /// sys_id of the remote instance record (sys_update_set_source).
        #[arg(long, value_name = "SYS_ID")]
        update_source_id: Option<String>,
        /// Instance id of the remote instance.
        #[arg(long, value_name = "ID")]
        update_source_instance_id: Option<String>,
        /// Preview it right away.
        #[arg(long)]
        auto_preview: bool,
        /// Remove the loaded copy when it was committed.
        #[arg(long)]
        cleanup_retrieved: bool,
        #[command(flatten)]
        wait: WaitArgs,
    },
    /// Preview a loaded update set.
    Preview(RemoteSetArgs),
    /// Commit a previewed update set.
    Commit(RemoteSetArgs),
    /// Commit several update sets in order.
    CommitMultiple {
        /// sys_ids of the loaded update sets, comma-separated.
        #[arg(long, value_delimiter = ',', required = true, value_name = "SYS_IDS")]
        ids: Vec<String>,
        #[command(flatten)]
        wait: WaitArgs,
    },
    /// Undo a committed update set.
    BackOut {
        /// sys_id of the update set.
        #[arg(long, value_name = "SYS_ID")]
        update_set_id: String,
        /// Also roll back the applications it installed.
        #[arg(long)]
        rollback_installs: bool,
        #[command(flatten)]
        wait: WaitArgs,
    },
}

#[derive(Args, Debug)]
pub struct RemoteSetArgs {
    /// sys_id of the loaded update set (sys_remote_update_set).
    remote_update_set_id: String,
    #[command(flatten)]
    wait: WaitArgs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Browser {
    Any,
    Chrome,
    Firefox,
    Edge,
    Ie,
    Safari,
}

/// A result id, as an argument or the flag of now-sdk.
#[derive(Args, Debug)]
pub struct ResultArgs {
    /// Result id (`links.results.id` of a run).
    #[arg(value_name = "RESULT", required_unless_present = "result_id")]
    result: Option<String>,
    #[arg(long, hide = true, conflicts_with = "result")]
    result_id: Option<String>,
}

impl ResultArgs {
    fn id(&self) -> Result<&str> {
        let id = self.result.as_deref().or(self.result_id.as_deref());
        segment("result id", id.unwrap_or_default())
    }
}

#[derive(Args, Debug)]
pub struct WatchProgressArgs {
    /// Progress id (`links.progress.id` of a run).
    #[arg(value_name = "PROGRESS", required_unless_present = "progress_id")]
    progress: Option<String>,
    #[arg(long, hide = true, conflicts_with = "progress")]
    progress_id: Option<String>,
    /// Give up waiting after this many seconds (0: never).
    #[arg(
        long,
        value_name = "SECONDS",
        default_value_t = 3600,
        alias = "poll-timeout"
    )]
    wait_timeout: u64,
}

impl WatchProgressArgs {
    fn started(&self) -> Result<Value> {
        let id = self.progress.as_deref().or(self.progress_id.as_deref());
        let id = segment("progress id", id.unwrap_or_default())?;
        Ok(json!({ "links": { "progress": { "id": id } } }))
    }

    fn wait(&self) -> WaitArgs {
        WaitArgs {
            wait: true,
            wait_timeout: self.wait_timeout,
        }
    }
}

#[derive(Subcommand, Debug)]
pub enum SuiteCommand {
    /// Run a test suite.
    Run(Box<SuiteRunArgs>),
    /// Wait for a run and show its result.
    Watch(WatchProgressArgs),
    /// The result of a run.
    #[command(visible_alias = "result")]
    Results(ResultArgs),
}

#[derive(Args, Debug)]
#[command(group = clap::ArgGroup::new("suite").required(true).multiple(true).args(["suite_id", "suite_name"]))]
pub struct SuiteRunArgs {
    /// sys_id of the suite.
    #[arg(long, value_name = "SYS_ID", alias = "test-suite-sys-id")]
    suite_id: Option<String>,
    /// Name of the suite.
    #[arg(long, value_name = "NAME", alias = "test-suite-name")]
    suite_name: Option<String>,
    /// Browser for tests of the interface.
    #[arg(long, value_enum)]
    browser_name: Option<Browser>,
    /// Start of the browser version.
    #[arg(long, value_name = "VERSION")]
    browser_version: Option<String>,
    /// Operating system.
    #[arg(long, value_name = "NAME")]
    os_name: Option<String>,
    /// Start of the operating system version.
    #[arg(long, value_name = "VERSION")]
    os_version: Option<String>,
    /// Use the Cloud Runner.
    #[arg(long)]
    run_in_cloud: bool,
    /// Run it as a performance test.
    #[arg(long, alias = "is-performance-run")]
    performance_run: bool,
    #[command(flatten)]
    wait: WaitArgs,
}

#[derive(Subcommand, Debug)]
pub enum TestCommand {
    /// Run one test.
    Run {
        /// sys_id of the test.
        #[arg(
            long,
            value_name = "SYS_ID",
            alias = "test-sys-id",
            required_unless_present = "test_name"
        )]
        test_id: Option<String>,
        /// Name of the test.
        #[arg(long, value_name = "NAME", conflicts_with = "test_id")]
        test_name: Option<String>,
        /// Use the Cloud Runner.
        #[arg(long)]
        run_in_cloud: bool,
        /// Record the logs of the node.
        #[arg(long)]
        capture_node_logs: bool,
        #[command(flatten)]
        wait: WaitArgs,
    },
    /// Wait for a run and show its result.
    Watch(WatchProgressArgs),
    /// The result of a run.
    #[command(visible_alias = "results")]
    Result(ResultArgs),
    /// The log of a run.
    Logs {
        #[command(flatten)]
        result: ResultArgs,
        /// Only lines that match this regular expression.
        #[arg(long, value_name = "REGEX")]
        pattern: Option<String>,
        /// Most lines.
        #[arg(long, short = 'n', default_value_t = 100, value_name = "N")]
        limit: u64,
    },
}

#[derive(Subcommand, Debug)]
pub enum BatchCommand {
    /// Install several applications and plugins.
    Install {
        /// Name of the installation.
        #[arg(long)]
        name: Option<String>,
        /// Notes.
        #[arg(long, value_name = "TEXT")]
        notes: Option<String>,
        /// Application SYS_ID@VERSION (repeatable).
        #[arg(long, value_name = "SYS_ID@VERSION")]
        app: Vec<String>,
        /// Plugin id (repeatable).
        #[arg(long, value_name = "ID")]
        plugin: Vec<String>,
        /// The whole request as JSON, @file, or - for stdin: {name, notes, packages: [...]}.
        #[arg(long, short = 'D', value_name = "JSON")]
        data: Option<String>,
        #[command(flatten)]
        wait: WaitArgs,
    },
    /// The result of a batch installation.
    Results(ResultArgs),
    /// Undo a batch installation.
    Rollback {
        /// Rollback id (`links.rollback.id` of the installation).
        id: String,
        #[command(flatten)]
        wait: WaitArgs,
    },
}

#[derive(Subcommand, Debug)]
pub enum PluginCommand {
    /// Activate a plugin; it cannot be deactivated.
    Activate {
        /// Plugin id, for example com.snc.change_management.
        id: String,
        #[command(flatten)]
        wait: WaitArgs,
    },
    /// Go back to the version before the last activation or upgrade.
    Rollback {
        /// Plugin id.
        id: String,
        #[command(flatten)]
        wait: WaitArgs,
    },
}

#[derive(Subcommand, Debug)]
pub enum ScanCommand {
    /// Scan the whole instance.
    Full {
        #[command(flatten)]
        wait: WaitArgs,
    },
    /// Scan one record.
    Point {
        /// Table of the record.
        #[arg(long, alias = "target-table")]
        table: String,
        /// sys_id of the record.
        #[arg(long, value_name = "SYS_ID", alias = "target-sys-id")]
        sys_id: String,
        #[command(flatten)]
        wait: WaitArgs,
    },
    /// Run a scan suite over applications or update sets.
    #[command(group = clap::ArgGroup::new("target").required(true).args(["apps", "update_sets"]))]
    Suite {
        /// sys_id of the suite (scan_check_suite).
        suite: String,
        /// sys_ids of applications, comma-separated.
        #[arg(long, value_delimiter = ',', value_name = "SYS_IDS")]
        apps: Vec<String>,
        /// sys_ids of update sets, comma-separated.
        #[arg(long, value_delimiter = ',', value_name = "SYS_IDS")]
        update_sets: Vec<String>,
        #[command(flatten)]
        wait: WaitArgs,
    },
    /// Run a saved combination (scan_combo).
    Combo {
        /// sys_id of the combination.
        combo: String,
        #[command(flatten)]
        wait: WaitArgs,
    },
    /// The result of a scan.
    Results {
        /// Progress id of the scan.
        progress: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum SourceCommand {
    /// Apply the newest commit of a branch to an application.
    Apply {
        #[command(flatten)]
        target: TargetArgs,
        /// Branch (default: the default branch).
        #[arg(long, value_name = "NAME", alias = "branch-name")]
        branch: Option<String>,
        /// Do not upgrade the base application.
        #[arg(long)]
        no_auto_upgrade_base_app: bool,
        #[command(flatten)]
        wait: WaitArgs,
    },
    /// Apply a stash that an earlier apply made.
    ApplyStash {
        /// Stash id (`links.stash.id` in the progress).
        stash: String,
        #[command(flatten)]
        wait: WaitArgs,
    },
    /// Import an application from a Git repository.
    Import {
        /// Address of the repository.
        #[arg(long, value_name = "URL")]
        repo_url: String,
        /// Branch.
        #[arg(long, value_name = "NAME", alias = "branch-name")]
        branch: String,
        /// sys_id of the credential for the repository.
        #[arg(long, value_name = "SYS_ID", alias = "credential-sys-id")]
        credential: Option<String>,
        /// sys_id of the MID server.
        #[arg(long, value_name = "SYS_ID", alias = "mid-server-sys-id")]
        mid_server: Option<String>,
        /// Do not upgrade the base application.
        #[arg(long)]
        no_auto_upgrade_base_app: bool,
        #[command(flatten)]
        wait: WaitArgs,
    },
}

/// The now-sdk project in this directory or above it.
pub(super) struct Project {
    pub scope_id: Option<String>,
    pub version: Option<String>,
}

impl Project {
    pub fn here() -> Option<Self> {
        let current = std::env::current_dir().ok()?;
        let read = |path: &std::path::Path| -> Option<Value> {
            serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
        };
        let text = |value: &Value| value.as_str().filter(|t| !t.is_empty()).map(str::to_owned);
        current.ancestors().find_map(|dir| {
            let config = read(&dir.join("now.config.json"))?;
            let package = read(&dir.join("package.json")).unwrap_or_default();
            Some(Self {
                scope_id: text(&config["scopeId"]),
                version: text(&package["version"]),
            })
        })
    }
}

impl TargetArgs {
    /// Adds the application to a request. The project's version goes with
    /// the project's application only.
    fn apply(
        &self,
        ctx: &Ctx,
        request: Request,
        version: Option<&str>,
        default_version: bool,
    ) -> Result<(Request, String)> {
        if self.sys_id.is_some() || self.scope.is_some() {
            let name = self.sys_id.as_ref().or(self.scope.as_ref());
            let request = request
                .query_opt("sys_id", self.sys_id.clone())
                .query_opt("scope", self.scope.clone())
                .query_opt("version", version);
            return Ok((request, name.cloned().unwrap_or_default()));
        }
        let project = Project::here();
        let Some(id) = project.as_ref().and_then(|p| p.scope_id.clone()) else {
            return Err(Error::Input {
                arg: "sys-id".into(),
                message: "the application is missing; pass --sys-id SYS_ID or --scope SCOPE, or run this in a now-sdk project (now.config.json)".into(),
                choices: Vec::new(),
            });
        };
        let version = version
            .map(str::to_owned)
            .or_else(|| project.and_then(|p| p.version).filter(|_| default_version));
        ctx.out.note(&format!(
            "using application {id}{} of the project here",
            version
                .as_ref()
                .map(|v| format!(" version {v}"))
                .unwrap_or_default()
        ));
        let request = request
            .query("sys_id", id.clone())
            .query_opt("version", version);
        Ok((request, id))
    }
}

/// An id that goes into the path.
fn segment<'a>(what: &str, id: &'a str) -> Result<&'a str> {
    let valid = !id.is_empty()
        && id.bytes().any(|b| b != b'.')
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    match valid {
        true => Ok(id),
        false => Err(Error::invalid(format!(
            "you passed the {what} `{id}`; pass an id with letters, digits, dots, dashes and underscores only"
        ))),
    }
}

fn post(path: &str) -> Request {
    Request::new(Method::POST, format!("{API}/{path}")).json(json!({}))
}

async fn get(ctx: &Ctx, path: &str) -> Result<Value> {
    ctx.client().await?.get(format!("{API}/{path}")).await
}

/// What the result of an operation is, beyond its progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Follow {
    Progress,
    Suite,
    Test,
    Batch,
    Scan,
}

pub async fn run(ctx: &Ctx, command: CicdCommand) -> Result<()> {
    match command {
        CicdCommand::App(command) => app(ctx, command).await,
        CicdCommand::Updateset(command) => update_set(ctx, command).await,
        CicdCommand::Atf(command) => suite(ctx, command).await,
        CicdCommand::Test(command) => test(ctx, command).await,
        CicdCommand::Batch(command) => batch(ctx, command).await,
        CicdCommand::Plugin(command) => plugin(ctx, command).await,
        CicdCommand::Scan(command) => scan(ctx, command).await,
        CicdCommand::SourceControl(command) => source(ctx, command).await,
        CicdCommand::Progress { id, wait } => {
            let path = format!("progress/{}", segment("progress id", &id)?);
            if wait.wait {
                let started = json!({ "links": { "progress": { "id": id } } });
                return finish(ctx, started, &wait, Follow::Progress).await;
            }
            show(ctx, get(ctx, &path).await?)
        }
    }
}

/// Sends the operation, and follows it with `--wait`.
async fn start(ctx: &Ctx, request: Request, wait: &WaitArgs, follow: Follow) -> Result<()> {
    let started = ctx.client().await?.send(request).await?.result()?;
    finish(ctx, started, wait, follow).await
}

/// The end of every background operation.
pub(super) async fn finish(
    ctx: &Ctx,
    started: Value,
    wait: &WaitArgs,
    follow: Follow,
) -> Result<()> {
    if let Some(err) = failure(&started) {
        return Err(err);
    }
    let id = started
        .pointer("/links/progress/id")
        .and_then(Value::as_str)
        .map(str::to_owned);
    if ctx.out.raw && !wait.wait {
        return ctx.out.item(json!({ "result": started }));
    }
    let Some(id) = id else {
        // Finished at once, or nothing to follow.
        return show(ctx, started);
    };
    if !wait.wait {
        let text = format!("Started; follow it with `sn cicd progress {id} --wait`");
        return ctx.out.done(started, &text);
    }
    let client = ctx.client().await?;
    let mut done = poll(ctx, client, &id, wait.wait_timeout).await?;
    let result_id = done
        .pointer("/links/results/id")
        .and_then(Value::as_str)
        .filter(|id| segment("result id", id).is_ok())
        .map(str::to_owned);
    let path = match (follow, result_id) {
        (Follow::Suite, Some(result)) => Some(format!("testsuite/results/{result}")),
        (Follow::Test, Some(result)) => Some(format!("tests/test/results/{result}")),
        (Follow::Batch, Some(result)) => Some(format!("app/batch/results/{result}")),
        (Follow::Scan, _) => Some(format!("instance_scan/result/{id}")),
        _ => None,
    };
    let tests = matches!(follow, Follow::Suite | Follow::Test);
    let unread = match path {
        Some(path) => match get(ctx, &path).await {
            Ok(results) => {
                done["results"] = results;
                None
            }
            Err(err) => Some(format!("the result could not be read ({err})")),
        },
        None => tests.then(|| "the instance named no result".to_owned()),
    };
    let problem = match unread {
        Some(why) if tests => Some(format!(
            "the run ended, but {why}, so it is not known whether the tests passed; look at `sn cicd progress {id}`, or run the tests again"
        )),
        Some(why) => {
            ctx.out.warn(&why);
            None
        }
        None if tests => tests_failed(&done["results"]),
        None => None,
    };
    if let Some(message) = problem {
        return Err(Error::Other {
            code: "check_failed",
            message,
            data: done,
        });
    }
    if ctx.out.raw {
        return ctx.out.item(json!({ "result": done }));
    }
    let message = |value: &Value| value.as_str().filter(|t| !t.is_empty()).map(str::to_owned);
    let text = message(&done["status_message"])
        .or_else(|| message(&done["status_detail"]))
        .unwrap_or_else(|| "The operation was successful".into());
    let text = match summary(&done["results"]) {
        Some(summary) => format!("{text}\n{summary}"),
        None => text,
    };
    ctx.out.done(done, &text)
}

/// Counts of a suite, or the output of a test.
fn summary(results: &Value) -> Option<String> {
    if let Some(status) = results["test_suite_status"].as_str() {
        let count = |name: &str| {
            results[format!("rolledup_test_{name}_count")]
                .as_u64()
                .unwrap_or(0)
        };
        return Some(format!(
            "suite {}: {status}, {} passed, {} failed, {} errors, {} skipped",
            results["test_suite_name"].as_str().unwrap_or_default(),
            count("success"),
            count("failure"),
            count("error"),
            count("skip")
        ));
    }
    let status = results["test_status"].as_str()?;
    let output = results["output"].as_str().unwrap_or_default();
    Some(format!("test: {status} {output}").trim_end().to_owned())
}

fn tests_failed(results: &Value) -> Option<String> {
    let status = ["test_suite_status", "test_status"]
        .iter()
        .find_map(|f| results[*f].as_str())
        .filter(|status| !status.is_empty());
    let Some(status) = status.map(str::to_ascii_lowercase) else {
        return Some("the run ended, but its result has no test status, so it is not known whether the tests passed; read it with `sn cicd atf results` or `sn cicd test result`".into());
    };
    let counted = ["failure", "error"].iter().any(|f| {
        results[format!("rolledup_test_{f}_count")]
            .as_u64()
            .is_some_and(|n| n > 0)
    });
    let failed = counted
        || matches!(
            status.as_str(),
            "failure" | "failed" | "error" | "canceled" | "cancelled"
        );
    failed.then(|| {
        format!(
            "the tests did not pass: {}",
            summary(results).unwrap_or(status)
        )
    })
}

/// An operation that failed or was cancelled.
fn failure(progress: &Value) -> Option<Error> {
    let status = match &progress["status"] {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    };
    let end = match status.as_str() {
        "3" => "failed",
        "4" => "was cancelled",
        _ => return None,
    };
    let reasons: Vec<&str> = ["status_message", "error", "status_detail"]
        .iter()
        .filter_map(|f| progress[*f].as_str())
        .filter(|t| !t.is_empty())
        .collect();
    let why = match reasons.is_empty() {
        true => "the instance gave no reason".to_owned(),
        false => reasons.join("; "),
    };
    Some(Error::api(
        format!("the operation {end}: {why}"),
        progress.clone(),
    ))
}

async fn poll(ctx: &Ctx, client: &Client, id: &str, timeout: u64) -> Result<Value> {
    let path = format!("{API}/progress/{}", segment("progress id", id)?);
    let started = Instant::now();
    let mut shown = String::new();
    loop {
        let progress = client.get(path.as_str()).await?;
        if let Some(err) = failure(&progress) {
            return Err(err);
        }
        if progress["status"] == "2" || progress["status"] == 2 {
            return Ok(progress);
        }
        // The percentage is a number in some answers and text in others.
        let percent = match &progress["percent_complete"] {
            Value::String(text) => text.clone(),
            Value::Null => "0".into(),
            other => other.to_string(),
        };
        let line = format!(
            "{percent}% {}",
            progress["status_message"].as_str().unwrap_or_default()
        );
        if line != shown {
            ctx.out.note(line.trim_end());
            shown = line;
        }
        if timeout > 0 && started.elapsed().as_secs() >= timeout {
            return Err(Error::Other {
                code: "timeout",
                message: format!(
                    "the operation still runs after {timeout} seconds; wait on with `sn cicd progress {id} --wait`"
                ),
                data: progress,
            });
        }
        tokio::time::sleep(POLL).await;
    }
}

async fn app(ctx: &Ctx, command: AppCommand) -> Result<()> {
    match command {
        AppCommand::Install {
            target,
            version,
            auto_upgrade_base_app,
            base_app_version,
            wait,
        } => {
            let request = post("app_repo/install");
            let (request, _) = target.apply(ctx, request, version.as_deref(), true)?;
            let request = request
                .query_opt(
                    "auto_upgrade_base_app",
                    auto_upgrade_base_app.then_some("true"),
                )
                .query_opt("base_app_version", base_app_version);
            start(ctx, request, &wait, Follow::Progress).await
        }
        AppCommand::Publish {
            target,
            version,
            dev_notes,
            wait,
        } => {
            let request = post("app_repo/publish");
            let (request, _) = target.apply(ctx, request, version.as_deref(), true)?;
            let request = request.query_opt("dev_notes", dev_notes);
            start(ctx, request, &wait, Follow::Progress).await
        }
        AppCommand::Rollback {
            target,
            version,
            wait,
        } => {
            let request = post("app_repo/rollback");
            let (request, name) = target.apply(ctx, request, Some(&version), false)?;
            ctx.confirm(&format!(
                "roll back application {name} to version {version}"
            ))?;
            start(ctx, request, &wait, Follow::Progress).await
        }
    }
}

async fn update_set(ctx: &Ctx, command: UpdateSetCommand) -> Result<()> {
    match command {
        UpdateSetCommand::Create {
            name,
            description,
            sys_id,
            scope,
        } => {
            let request = post("update_set/create")
                .query("update_set_name", name.clone())
                .query_opt("description", description)
                .query_opt("sys_id", sys_id)
                .query_opt("scope", scope);
            let created = ctx.client().await?.send(request).await?.result()?;
            if ctx.out.raw {
                return show(ctx, created);
            }
            let id = created["update_set_id"].as_str().unwrap_or_default();
            let text = format!("Created update set {name} {id}");
            ctx.out.done(created, text.trim_end())
        }
        UpdateSetCommand::Retrieve {
            update_set_id,
            update_source_id,
            update_source_instance_id,
            auto_preview,
            cleanup_retrieved,
            wait,
        } => {
            let request = post("update_set/retrieve")
                .query("update_set_id", update_set_id)
                .query_opt("update_source_id", update_source_id)
                .query_opt("update_source_instance_id", update_source_instance_id)
                .query_opt("auto_preview", auto_preview.then_some("true"))
                .query_opt("cleanup_retrieved", cleanup_retrieved.then_some("true"));
            start(ctx, request, &wait, Follow::Progress).await
        }
        UpdateSetCommand::Preview(args) => {
            let id = segment("update set", &args.remote_update_set_id)?;
            let request = post(&format!("update_set/preview/{id}"));
            start(ctx, request, &args.wait, Follow::Progress).await
        }
        UpdateSetCommand::Commit(args) => {
            let id = segment("update set", &args.remote_update_set_id)?;
            let request = post(&format!("update_set/commit/{id}"));
            start(ctx, request, &args.wait, Follow::Progress).await
        }
        UpdateSetCommand::CommitMultiple { ids, wait } => {
            let request =
                post("update_set/commitMultiple").query("remote_update_set_ids", ids.join(","));
            start(ctx, request, &wait, Follow::Progress).await
        }
        UpdateSetCommand::BackOut {
            update_set_id,
            rollback_installs,
            wait,
        } => {
            ctx.confirm(&format!("back out update set {update_set_id}"))?;
            let request = post("update_set/back_out")
                .query("update_set_id", update_set_id)
                .query_opt("rollback_installs", rollback_installs.then_some("true"));
            start(ctx, request, &wait, Follow::Progress).await
        }
    }
}

async fn suite(ctx: &Ctx, command: SuiteCommand) -> Result<()> {
    match command {
        SuiteCommand::Run(args) => {
            let browser = args
                .browser_name
                .and_then(|b| b.to_possible_value())
                .map(|v| v.get_name().to_owned());
            let request = post("testsuite/run")
                .query_opt("test_suite_sys_id", args.suite_id)
                .query_opt("test_suite_name", args.suite_name)
                .query_opt("browser_name", browser)
                .query_opt("browser_version", args.browser_version)
                .query_opt("os_name", args.os_name)
                .query_opt("os_version", args.os_version)
                .query_opt("run_in_cloud", args.run_in_cloud.then_some("true"))
                .query_opt("is_performance_run", args.performance_run.then_some("true"));
            start(ctx, request, &args.wait, Follow::Suite).await
        }
        SuiteCommand::Watch(args) => {
            finish(ctx, args.started()?, &args.wait(), Follow::Suite).await
        }
        SuiteCommand::Results(result) => {
            let path = format!("testsuite/results/{}", result.id()?);
            show(ctx, get(ctx, &path).await?)
        }
    }
}

async fn test(ctx: &Ctx, command: TestCommand) -> Result<()> {
    match command {
        TestCommand::Run {
            test_id,
            test_name,
            run_in_cloud,
            capture_node_logs,
            wait,
        } => {
            let id = match (test_id, test_name) {
                (Some(id), _) => id,
                (None, name) => find_test(ctx.client().await?, &name.unwrap_or_default()).await?,
            };
            let request = post("tests/run_test")
                .query("test_sys_id", id)
                .query_opt("run_in_cloud", run_in_cloud.then_some("true"))
                .query_opt("capture_node_logs", capture_node_logs.then_some("true"));
            start(ctx, request, &wait, Follow::Test).await
        }
        TestCommand::Watch(args) => finish(ctx, args.started()?, &args.wait(), Follow::Test).await,
        TestCommand::Result(result) => {
            let path = format!("tests/test/results/{}", result.id()?);
            show(ctx, get(ctx, &path).await?)
        }
        TestCommand::Logs {
            result,
            pattern,
            limit,
        } => {
            let path = format!("{API}/tests/test/results/{}/logs", result.id()?);
            let request = Request::new(Method::GET, path)
                .query_opt("sysparm_regex", pattern)
                .query("sysparm_limit", limit.to_string());
            show(ctx, ctx.client().await?.send(request).await?.result()?)
        }
    }
}

async fn find_test(client: &Client, name: &str) -> Result<String> {
    let params = TableQuery::new(query::term("name", "=", name))
        .fields(&["sys_id", "name"])
        .limit(10)
        .display(DisplayValue::False);
    let mut rows = client.list("sys_atf_test", &params).await?.rows;
    rows.retain(|row| raw(row, "name").eq_ignore_ascii_case(name));
    let test = one(
        rows,
        "test-id",
        Error::NotFound(format!(
            "there is no test `{name}` that you can read; find it with `sn table list sys_atf_test -q nameLIKE{name}`"
        )),
        format!("several tests have the name `{name}`; pass --test-id"),
        |row| raw(row, "name").to_owned(),
    )?;
    Ok(raw(&test, "sys_id").to_owned())
}

async fn batch(ctx: &Ctx, command: BatchCommand) -> Result<()> {
    match command {
        BatchCommand::Install {
            name,
            notes,
            app,
            plugin,
            data,
            wait,
        } => {
            let mut body = match data {
                Some(data) => parse_json(&read_input(&data)?, "--data")?,
                None => json!({}),
            };
            if !body.is_object() {
                return Err(Error::invalid(
                    "--data is not a JSON object; pass {\"name\": ..., \"packages\": [...]}",
                ));
            }
            let mut packages = body["packages"].as_array().cloned().unwrap_or_default();
            for text in &app {
                let Some((id, version)) = text.split_once('@') else {
                    return Err(Error::invalid(format!(
                        "you passed --app {text}; pass SYS_ID@VERSION, for example 4dd9686d1b9800103d374087bc4bcb3d@1.0.2"
                    )));
                };
                packages
                    .push(json!({ "id": id, "type": "application", "requested_version": version }));
            }
            packages.extend(
                plugin
                    .iter()
                    .map(|id| json!({ "id": id, "type": "plugin" })),
            );
            if let Some(name) = name {
                body["name"] = name.into();
            }
            if let Some(notes) = notes {
                body["notes"] = notes.into();
            }
            let missing = match (body["name"].as_str(), packages.is_empty()) {
                (None | Some(""), _) => "name",
                (_, true) => "app",
                _ => "",
            };
            if !missing.is_empty() {
                return Err(Error::Input {
                    arg: missing.into(),
                    message: "a batch needs --name and at least one --app SYS_ID@VERSION or --plugin ID, or --data with the whole request".into(),
                    choices: Vec::new(),
                });
            }
            body["packages"] = packages.into();
            let request = post("app/batch/install").json(body);
            start(ctx, request, &wait, Follow::Batch).await
        }
        BatchCommand::Results(result) => {
            let path = format!("app/batch/results/{}", result.id()?);
            show(ctx, get(ctx, &path).await?)
        }
        BatchCommand::Rollback { id, wait } => {
            let id = segment("rollback id", &id)?;
            ctx.confirm(&format!("roll back the batch installation {id}"))?;
            let request = post(&format!("app/batch/rollback/{id}"));
            start(ctx, request, &wait, Follow::Progress).await
        }
    }
}

async fn plugin(ctx: &Ctx, command: PluginCommand) -> Result<()> {
    let (id, verb, action, wait) = match &command {
        PluginCommand::Activate { id, wait } => {
            (id, "activate", "activate (it cannot be deactivated)", wait)
        }
        PluginCommand::Rollback { id, wait } => (id, "rollback", "roll back", wait),
    };
    let id = segment("plugin id", id)?;
    ctx.confirm(&format!("{action} plugin {id}"))?;
    let request = post(&format!("plugin/{id}/{verb}"));
    start(ctx, request, wait, Follow::Progress).await
}

async fn scan(ctx: &Ctx, command: ScanCommand) -> Result<()> {
    let (request, wait) = match command {
        ScanCommand::Full { wait } => (post("instance_scan/full_scan"), wait),
        ScanCommand::Point {
            table,
            sys_id,
            wait,
        } => {
            query::check_name("table", &table)?;
            let request = post("instance_scan/point_scan")
                .query("target_table", table)
                .query("target_sys_id", sys_id);
            (request, wait)
        }
        ScanCommand::Suite {
            suite,
            apps,
            update_sets,
            wait,
        } => {
            let suite = segment("suite", &suite)?;
            let (path, body) = match apps.is_empty() {
                true => ("update_sets", json!({ "update_set_sys_ids": update_sets })),
                false => ("scoped_apps", json!({ "app_scope_sys_ids": apps })),
            };
            let request = post(&format!("instance_scan/suite_scan/{suite}/{path}")).json(body);
            (request, wait)
        }
        ScanCommand::Combo { combo, wait } => {
            let combo = segment("combination", &combo)?;
            (
                post(&format!("instance_scan/suite_scan/combo/{combo}")),
                wait,
            )
        }
        ScanCommand::Results { progress } => {
            let path = format!(
                "instance_scan/result/{}",
                segment("progress id", &progress)?
            );
            return show(ctx, get(ctx, &path).await?);
        }
    };
    start(ctx, request, &wait, Follow::Scan).await
}

async fn source(ctx: &Ctx, command: SourceCommand) -> Result<()> {
    let (request, wait) = match command {
        SourceCommand::Apply {
            target,
            branch,
            no_auto_upgrade_base_app,
            wait,
        } => {
            let (request, _) = target.apply(ctx, post("sc/apply_changes"), None, false)?;
            let request = request.query_opt("branch_name", branch).query_opt(
                "auto_upgrade_base_app",
                no_auto_upgrade_base_app.then_some("false"),
            );
            (request, wait)
        }
        SourceCommand::ApplyStash { stash, wait } => {
            let stash = segment("stash id", &stash)?;
            (post(&format!("sc/apply_stash/{stash}")), wait)
        }
        SourceCommand::Import {
            repo_url,
            branch,
            credential,
            mid_server,
            no_auto_upgrade_base_app,
            wait,
        } => {
            let request = post("sc/import")
                .query("repo_url", repo_url)
                .query("branch_name", branch)
                .query_opt("credential_sys_id", credential)
                .query_opt("mid_server_sys_id", mid_server)
                .query_opt(
                    "auto_upgrade_base_app",
                    no_auto_upgrade_base_app.then_some("false"),
                );
            (request, wait)
        }
    };
    start(ctx, request, &wait, Follow::Progress).await
}

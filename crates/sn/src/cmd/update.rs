//! `sn update` and the update notice.
//!
//! In a terminal, a release build checks GitHub for a newer release at most
//! once every 20 hours, in the background while the command runs, and
//! prints one line after the command's output. `sn update` runs the
//! release's own installer into a temporary directory (it downloads the
//! archive and verifies its SHA-256 checksum) and then replaces the running
//! executable.

use crate::cli::command_index;
use crate::ctx::Ctx;
use anyhow::{Context as _, Result};
use clap::Args;
use semver::Version;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

const REPOSITORY: &str = "racetozero/servicenow-cli";
const LATEST_RELEASE_URL: &str =
    "https://api.github.com/repos/racetozero/servicenow-cli/releases/latest";
const RELEASES_URL: &str = "https://github.com/racetozero/servicenow-cli/releases";
const RAW_REPOSITORY_URL: &str = "https://raw.githubusercontent.com/racetozero/servicenow-cli";
const HOMEBREW_UPGRADE_COMMAND: &str = "brew upgrade racetozero/tap/sn";
const CACHE_MAX_AGE_SECS: u64 = 20 * 60 * 60;
const CACHE_FILE: &str = "update-check.json";
/// Longest wait for the background check after a command finished.
const FINISH_WAIT: Duration = Duration::from_millis(800);

#[derive(Debug, Deserialize)]
struct GitHubRelease {
    tag_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct VersionCache {
    latest_version: Version,
    /// Seconds since 1970.
    last_checked_at: u64,
}

#[derive(Args, Debug)]
pub struct UpdateArgs {
    /// Only say whether a newer release exists.
    #[arg(long)]
    check: bool,
}

pub async fn run(ctx: &Ctx, args: UpdateArgs) -> sn_core::Result<()> {
    let outcome = update(args.check, ctx)
        .await
        .map_err(|err| sn_core::Error::Internal(format!("{err:#}")))?;
    let (current, latest) = (&outcome.current, &outcome.latest);
    let text = match (outcome.updated, latest > current) {
        (true, _) => format!("Updated sn from v{current} to v{latest}."),
        (false, true) => notice(current, latest),
        (false, false) => format!("sn v{current} is up to date."),
    };
    let data = json!({
        "version": current.to_string(), "latest": latest.to_string(),
        "update_available": latest > current && !outcome.updated, "updated": outcome.updated,
    });
    ctx.out.done(data, &text)
}

struct Outcome {
    current: Version,
    latest: Version,
    updated: bool,
}

async fn update(check: bool, ctx: &Ctx) -> Result<Outcome> {
    if cfg!(debug_assertions) {
        anyhow::bail!(
            "`sn update` is not available in debug builds; install a release (see the README) to use it"
        );
    }
    let client = http_client()?;
    let current = current_version()?;
    let latest = fetch_latest_version(&client, LATEST_RELEASE_URL).await?;
    if check || latest <= current {
        return Ok(Outcome {
            current,
            latest,
            updated: false,
        });
    }
    if let Some(template) = custom_update_command() {
        run_custom_update(&template, &current, &latest).await?;
    } else if installed_by_homebrew() {
        anyhow::bail!(
            "`sn update` cannot replace a copy that Homebrew installed; run `{HOMEBREW_UPGRADE_COMMAND}`, or set SN_SELF_UPDATE_COMMAND to that command"
        );
    } else {
        ctx.out
            .note(&format!("Updating sn from v{current} to v{latest}..."));
        let installer = download_installer(&client, &latest).await?;
        let staging = tempfile::tempdir().context("cannot create the update staging directory")?;
        let staged = run_staged_installer(&installer, &latest, staging.path()).await?;
        self_replace::self_replace(&staged).context("cannot replace the sn executable")?;
    }
    Ok(Outcome {
        current,
        latest,
        updated: true,
    })
}

/// `SN_SELF_UPDATE_COMMAND`, split on spaces: the update command of
/// another package manager, for example `cargo install sn --force`.
fn custom_update_command() -> Option<Vec<String>> {
    std::env::var("SN_SELF_UPDATE_COMMAND")
        .ok()
        .map(|command| command.split_whitespace().map(str::to_owned).collect())
        .filter(|command: &Vec<String>| !command.is_empty())
}

/// Fills `{packageName}`, `{version}`, and `{installSpec}` for the release.
fn update_command_argv(template: &[String], latest: &Version) -> Vec<String> {
    template
        .iter()
        .map(|argument| {
            argument
                .replace("{installSpec}", &format!("sn@{latest}"))
                .replace("{packageName}", "sn")
                .replace("{version}", &latest.to_string())
        })
        .collect()
}

async fn run_custom_update(template: &[String], current: &Version, latest: &Version) -> Result<()> {
    let argv = update_command_argv(template, latest);
    let shown = argv.join(" ");
    eprintln!("Updating sn from v{current} to v{latest} with `{shown}`...");
    let status = tokio::process::Command::new(&argv[0])
        .args(&argv[1..])
        .status()
        .await
        .with_context(|| {
            format!(
                "cannot start `{shown}` from SN_SELF_UPDATE_COMMAND; check that `{}` is installed and on PATH",
                argv[0]
            )
        })?;
    if !status.success() {
        anyhow::bail!(
            "`{shown}` failed ({status}); fix SN_SELF_UPDATE_COMMAND, or remove it to use the sn installer"
        );
    }
    Ok(())
}

/// A pending update check, started before the command runs.
pub struct Check {
    /// A newer release found by an earlier check.
    latest: Option<Version>,
    /// The background refresh of a stale cache.
    refresh: Option<tokio::task::JoinHandle<()>>,
}

/// Starts the update check when this run may show a notice (release build,
/// stderr is a terminal, no CI, no agent, not `update`, `completions` or `tui`).
pub fn start(argv: &[String]) -> Option<Check> {
    if cfg!(debug_assertions) {
        return None;
    }
    let terminal = std::io::IsTerminal::is_terminal(&std::io::stderr());
    if !eligible(argv, terminal, |name| std::env::var(name).ok()) {
        return None;
    }
    let path = cache_path()?;
    let cache = read_cache(&path).ok();
    let current = current_version().ok()?;
    let latest = newer_cached_version(cache.as_ref(), &current);
    let refresh = needs_refresh(cache.as_ref(), sn_core::now_unix()).then(|| {
        let known = latest.clone().unwrap_or(current);
        tokio::spawn(async move {
            let _ = refresh_cache(&path, known, LATEST_RELEASE_URL).await;
        })
    });
    Some(Check { latest, refresh })
}

/// Lets a running refresh finish (briefly) and prints the notice.
pub async fn finish(check: Option<Check>) {
    let Some(check) = check else {
        return;
    };
    if let Some(refresh) = check.refresh {
        let _ = tokio::time::timeout(FINISH_WAIT, refresh).await;
    }
    if crate::output::agent_mode() {
        return;
    }
    if let (Some(latest), Ok(current)) = (check.latest, current_version()) {
        eprintln!("{}", crate::output::dim(&notice(&current, &latest), true));
    }
}

fn eligible(argv: &[String], stderr_terminal: bool, var: impl Fn(&str) -> Option<String>) -> bool {
    let set = |name: &str| var(name).is_some_and(|v| sn_core::config::truthy(&v));
    if !stderr_terminal || set("CI") || set("SN_NO_UPDATE_NOTIFIER") || set("SN_AGENT") {
        return false;
    }
    if argv.iter().any(|a| a == "--agent") {
        return false;
    }
    !matches!(
        command_index(argv).map(|at| argv[at].as_str()),
        None | Some("update" | "completions" | "completion" | "tui")
    )
}

/// The one-line notice for a newer release.
fn notice(current: &Version, latest: &Version) -> String {
    let command = if installed_by_homebrew() && custom_update_command().is_none() {
        HOMEBREW_UPGRADE_COMMAND
    } else {
        "sn update"
    };
    format!("Update available: sn v{current} -> v{latest}. Run `{command}`.")
}

/// True when the resolved executable is `.../Cellar/sn/<version>/bin/sn`.
fn installed_by_homebrew() -> bool {
    std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .is_ok_and(|path| {
            path.ancestors()
                .nth(3)
                .is_some_and(|rack| rack.ends_with("Cellar/sn"))
        })
}

fn current_version() -> Result<Version> {
    Version::parse(env!("CARGO_PKG_VERSION")).context("cannot parse the sn version")
}

fn parse_release_tag(tag: &str) -> Result<Version> {
    let version = tag.trim_prefix('v');
    Version::parse(version)
        .with_context(|| format!("the latest GitHub release has an invalid tag `{tag}`"))
}

fn http_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .user_agent(concat!("sn/", env!("CARGO_PKG_VERSION")))
        .build()
        .context("cannot build the update HTTP client")
}

async fn fetch_latest_version(client: &reqwest::Client, url: &str) -> Result<Version> {
    let release = client
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .await
        .context("cannot check the latest sn release on GitHub")?
        .error_for_status()
        .context("GitHub did not return the latest sn release")?
        .json::<GitHubRelease>()
        .await
        .context("cannot read the latest sn release from GitHub")?;
    parse_release_tag(&release.tag_name)
}

async fn download_installer(
    client: &reqwest::Client,
    version: &Version,
) -> Result<tempfile::TempPath> {
    let url = installer_url(version);
    let bytes = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("cannot download the sn installer from {url}"))?
        .error_for_status()
        .with_context(|| format!("the sn installer is not available at {url}"))?
        .bytes()
        .await
        .context("cannot read the sn installer")?;
    let mut file = tempfile::Builder::new()
        .prefix("sn-update-")
        .suffix(if cfg!(windows) { ".ps1" } else { ".sh" })
        .tempfile()
        .context("cannot create a temporary installer file")?;
    file.write_all(&bytes)
        .and_then(|()| file.flush())
        .context("cannot write the temporary sn installer")?;
    Ok(file.into_temp_path())
}

/// The installer of exactly this release (not of `main`).
fn installer_url(version: &Version) -> String {
    let name = if cfg!(windows) {
        "install.ps1"
    } else {
        "install.sh"
    };
    format!("{RAW_REPOSITORY_URL}/v{version}/{name}")
}

async fn run_staged_installer(
    installer: &Path,
    version: &Version,
    staging: &Path,
) -> Result<PathBuf> {
    let mut command = installer_command(installer)?;
    command
        .current_dir(staging)
        .env("SN_VERSION", version.to_string())
        .env("SN_INSTALL_DIR", staging)
        .env("SN_REPOSITORY", REPOSITORY)
        .env("SN_RELEASES_URL", RELEASES_URL)
        .env_remove("SN_TARGET");
    let output = command
        .output()
        .await
        .context("cannot start the sn release installer")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        let detail = if stderr.is_empty() { stdout } else { stderr };
        anyhow::bail!("the sn installer failed ({}): {detail}", output.status);
    }
    let staged = staging.join(if cfg!(windows) { "sn.exe" } else { "sn" });
    if !staged.is_file() {
        anyhow::bail!("the sn installer did not create {}", staged.display());
    }
    Ok(staged)
}

#[cfg(windows)]
fn installer_command(installer: &Path) -> Result<tokio::process::Command> {
    let system_root = std::env::var_os("SystemRoot").context("SystemRoot is not set")?;
    let powershell = PathBuf::from(system_root)
        .join("System32")
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("powershell.exe");
    if !powershell.is_file() {
        anyhow::bail!("PowerShell was not found at {}", powershell.display());
    }
    let mut command = tokio::process::Command::new(powershell);
    command
        .arg("-NoProfile")
        .arg("-ExecutionPolicy")
        .arg("Bypass")
        .arg("-File")
        .arg(installer);
    Ok(command)
}

#[cfg(not(windows))]
fn installer_command(installer: &Path) -> Result<tokio::process::Command> {
    let mut command = tokio::process::Command::new("/bin/sh");
    command.arg(installer);
    Ok(command)
}

/// `update-check.json` next to the config file.
fn cache_path() -> Option<PathBuf> {
    sn_core::config::Store::open()
        .ok()
        .map(|store| store.dir().join(CACHE_FILE))
}

fn read_cache(path: &Path) -> Result<VersionCache> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("cannot read the update cache {}", path.display()))?;
    serde_json::from_str(&text)
        .with_context(|| format!("cannot parse the update cache {}", path.display()))
}

fn needs_refresh(cache: Option<&VersionCache>, now: u64) -> bool {
    cache.is_none_or(|cache| cache.last_checked_at + CACHE_MAX_AGE_SECS < now)
}

fn newer_cached_version(cache: Option<&VersionCache>, current: &Version) -> Option<Version> {
    cache
        .filter(|cache| cache.latest_version > *current)
        .map(|cache| cache.latest_version.clone())
}

/// Records the check before it runs: without GitHub, the next commands must
/// not wait for it again.
async fn refresh_cache(path: &Path, known: Version, url: &str) -> Result<()> {
    let mut cache = VersionCache {
        latest_version: known,
        last_checked_at: sn_core::now_unix(),
    };
    write_cache(path, &cache).await?;
    cache.latest_version = fetch_latest_version(&http_client()?, url).await?;
    write_cache(path, &cache).await
}

async fn write_cache(path: &Path, cache: &VersionCache) -> Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    let json = serde_json::to_vec_pretty(cache).context("cannot encode the update cache")?;
    tokio::fs::write(path, json)
        .await
        .with_context(|| format!("cannot write the update cache {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_update_command_gets_the_release_placeholders() {
        let template: Vec<String> = [
            "mise",
            "use",
            "-g",
            "cargo:{packageName}@{version}",
            "{installSpec}",
        ]
        .map(String::from)
        .into();
        assert_eq!(
            update_command_argv(&template, &Version::new(1, 2, 3)),
            ["mise", "use", "-g", "cargo:sn@1.2.3", "sn@1.2.3"]
        );
    }

    #[test]
    fn parses_release_tags_as_semantic_versions() {
        assert_eq!(parse_release_tag("v1.2.3").unwrap(), Version::new(1, 2, 3));
        assert_eq!(parse_release_tag("1.2.3").unwrap(), Version::new(1, 2, 3));
        assert!(parse_release_tag("release-1.2.3").is_err());
    }

    #[test]
    fn refreshes_only_missing_or_old_cache_data() {
        let now = 1_000_000;
        let fresh = VersionCache {
            latest_version: Version::new(1, 2, 3),
            last_checked_at: now - 19 * 3600,
        };
        let old = VersionCache {
            last_checked_at: now - 21 * 3600,
            ..fresh.clone()
        };
        assert!(!needs_refresh(Some(&fresh), now));
        assert!(needs_refresh(Some(&old), now));
        assert!(needs_refresh(None, now));
    }

    #[tokio::test]
    async fn a_failed_check_is_not_repeated_by_the_next_command() {
        let server = wiremock::MockServer::start().await;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("sn").join(CACHE_FILE);
        let known = Version::new(1, 2, 3);
        assert!(
            refresh_cache(&path, known.clone(), &server.uri())
                .await
                .is_err()
        );
        let cache = read_cache(&path).unwrap();
        assert!(!needs_refresh(Some(&cache), sn_core::now_unix()));
        assert_eq!(cache.latest_version, known);
    }

    #[test]
    fn returns_only_a_newer_cached_version() {
        let current = Version::new(1, 2, 3);
        let mut cache = VersionCache {
            latest_version: Version::new(1, 2, 4),
            last_checked_at: 0,
        };
        assert_eq!(
            newer_cached_version(Some(&cache), &current),
            Some(Version::new(1, 2, 4))
        );
        cache.latest_version = current.clone();
        assert_eq!(newer_cached_version(Some(&cache), &current), None);
        cache.latest_version = Version::new(1, 2, 2);
        assert_eq!(newer_cached_version(Some(&cache), &current), None);
    }

    #[test]
    fn gives_a_direct_update_instruction() {
        assert_eq!(
            notice(&Version::new(1, 2, 3), &Version::new(1, 3, 0)),
            "Update available: sn v1.2.3 -> v1.3.0. Run `sn update`."
        );
    }

    #[test]
    fn checks_only_in_a_terminal_without_ci_agents_or_update_commands() {
        let argv = |args: &[&str]| -> Vec<String> {
            std::iter::once("sn")
                .chain(args.iter().copied())
                .map(String::from)
                .collect()
        };
        let none = |_: &str| None;
        assert!(eligible(&argv(&["table", "list", "incident"]), true, none));
        assert!(!eligible(
            &argv(&["table", "list", "incident"]),
            false,
            none
        ));
        assert!(!eligible(&argv(&["ping", "--agent"]), true, none));
        for command in ["update", "completions", "tui"] {
            assert!(!eligible(&argv(&[command]), true, none), "{command}");
        }
        assert!(!eligible(&argv(&["-p", "work", "tui"]), true, none));
        assert!(!eligible(&argv(&[]), true, none));
        let ci = |name: &str| (name == "CI").then(|| "true".to_owned());
        assert!(!eligible(&argv(&["ping"]), true, ci));
        let agent_off = |name: &str| (name == "SN_AGENT").then(|| "0".to_owned());
        assert!(eligible(&argv(&["ping"]), true, agent_off));
    }

    #[cfg(unix)]
    #[test]
    fn homebrew_notice_uses_the_resolved_install_path() {
        const EXPECTED_COMMAND: &str = "SN_TEST_UPDATE_COMMAND";
        if let Ok(command) = std::env::var(EXPECTED_COMMAND) {
            assert_eq!(
                notice(&Version::new(1, 2, 3), &Version::new(1, 3, 0)),
                format!("Update available: sn v1.2.3 -> v1.3.0. Run `{command}`.")
            );
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let homebrew = directory.path().join("Cellar/sn/1.2.3/bin/sn");
        let manual = directory.path().join("Cellar/projects/bin/sn");
        for binary in [&homebrew, &manual] {
            std::fs::create_dir_all(binary.parent().unwrap()).unwrap();
            std::fs::copy(std::env::current_exe().unwrap(), binary).unwrap();
        }
        let link = directory.path().join("sn");
        std::os::unix::fs::symlink(&homebrew, &link).unwrap();
        for (binary, command) in [
            (&homebrew, HOMEBREW_UPGRADE_COMMAND),
            (&link, HOMEBREW_UPGRADE_COMMAND),
            (&manual, "sn update"),
        ] {
            let output = std::process::Command::new(binary)
                .args([
                    "--exact",
                    "cmd::update::tests::homebrew_notice_uses_the_resolved_install_path",
                ])
                .env(EXPECTED_COMMAND, command)
                .env_remove("SN_SELF_UPDATE_COMMAND")
                .output()
                .unwrap();
            assert!(
                output.status.success()
                    && String::from_utf8_lossy(&output.stdout).contains("1 passed"),
                "{}: {}{}",
                binary.display(),
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    #[test]
    fn pins_the_installer_to_the_release_tag() {
        let url = installer_url(&Version::new(1, 2, 3));
        assert!(
            url.starts_with("https://raw.githubusercontent.com/racetozero/servicenow-cli/v1.2.3/"),
            "{url}"
        );
        assert!(url.ends_with(if cfg!(windows) {
            "install.ps1"
        } else {
            "install.sh"
        }));
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn runs_the_installer_in_a_staging_directory() {
        let mut installer = tempfile::Builder::new().suffix(".sh").tempfile().unwrap();
        installer
            .write_all(b"#!/bin/sh\nset -eu\nprintf '%s' \"$SN_VERSION\" > \"$SN_INSTALL_DIR/sn\"\nchmod 0755 \"$SN_INSTALL_DIR/sn\"\n")
            .unwrap();
        installer.flush().unwrap();
        let staging = tempfile::tempdir().unwrap();
        let binary = run_staged_installer(installer.path(), &Version::new(1, 2, 3), staging.path())
            .await
            .unwrap();
        assert_eq!(std::fs::read_to_string(binary).unwrap(), "1.2.3");
    }
}

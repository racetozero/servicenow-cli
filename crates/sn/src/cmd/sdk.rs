//! The official now-sdk with this profile's sign-in.

use crate::ctx::Ctx;
use clap::Args;
use sn_core::uisession::UiSession;
use sn_core::{Client, Error, Result};

#[derive(Args, Debug)]
#[command(disable_help_flag = true)]
pub struct SdkArgs {
    /// now-sdk command and flags, for example: build, install, transform --table sys_script.
    #[arg(
        trailing_var_arg = true,
        allow_hyphen_values = true,
        value_name = "ARGS"
    )]
    pub args: Vec<String>,
}

/// The variables that give now-sdk a ready session.
pub async fn session_env(client: &Client) -> Result<Vec<(&'static str, String)>> {
    let mut env = vec![
        ("SN_SDK_INSTANCE_URL", client.base().to_owned()),
        ("SN_SDK_NODE_ENV", "SN_SDK_CI_INSTALL".to_owned()),
    ];
    if let Some(token) = client.bearer(false).await? {
        env.push(("SN_SDK_SESSION_BEARER_TOKEN", token));
    }
    let session = UiSession::mint(client).await?;
    env.push(("SN_SDK_SESSION_TOKEN", session.user_token.clone()));
    env.push(("SN_SDK_SESSION_COOKIE", session.cookie_header()));
    Ok(env)
}

pub async fn run(ctx: &Ctx, args: SdkArgs) -> Result<()> {
    // An installed now-sdk, then bun, then npm's runner.
    let runners: [(&str, &[&str]); 3] = [
        ("now-sdk", &[]),
        ("bunx", &["--package", "@servicenow/sdk", "now-sdk"]),
        ("npx", &["--yes", "--package", "@servicenow/sdk", "now-sdk"]),
    ];
    let found = runners.iter().find(|(program, _)| on_path(program)).ok_or_else(|| {
        Error::invalid("now-sdk, bunx and npx are not on PATH; install Bun (https://bun.sh), or `bun add -g @servicenow/sdk`")
    })?;
    let offline = [
        "build",
        "clean",
        "pack",
        "explain",
        "--help",
        "-h",
        "--version",
    ];
    let needs_session = args
        .args
        .first()
        .is_some_and(|first| !offline.contains(&first.as_str()));
    let env = match needs_session {
        true => session_env(ctx.client().await?).await?,
        false => Vec::new(),
    };
    if ctx.out.agent {
        // One envelope on stdout: the tool's own output goes into it.
        let output = tokio::process::Command::new(found.0)
            .args(found.1)
            .args(&args.args)
            .envs(env)
            .stdin(std::process::Stdio::null())
            .output()
            .await
            .map_err(|err| Error::Internal(format!("cannot start {} ({err})", found.0)))?;
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        let data = serde_json::json!({ "exit_code": output.status.code(), "stdout": stdout, "stderr": stderr });
        return match output.status.success() {
            true => ctx.out.item(data),
            false => Err(Error::Other {
                code: "api",
                message: format!("now-sdk failed ({}); see data.stderr", output.status),
                data,
            }),
        };
    }
    let status = tokio::process::Command::new(found.0)
        .args(found.1)
        .args(&args.args)
        .envs(env)
        .status()
        .await
        .map_err(|err| Error::Internal(format!("cannot start {} ({err})", found.0)))?;
    match status.success() {
        true => Ok(()),
        false => Err(Error::api(
            format!("now-sdk failed ({status}); its messages are above"),
            serde_json::Value::Null,
        )),
    }
}

fn on_path(program: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| {
        let file = dir.join(program);
        file.is_file()
            || (cfg!(windows)
                && ["exe", "cmd"]
                    .iter()
                    .any(|e| file.with_extension(e).is_file()))
    })
}

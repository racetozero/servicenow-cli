//! `sn`: ServiceNow from the terminal, for people and agents.
#![forbid(unsafe_code)]

mod amb;
mod browser;
mod cli;
mod cmd;
mod ctx;
mod help;
mod output;
mod prompt;

use sn_core::Error;
use std::io::{IsTerminal, Write};
use std::process::ExitCode;

/// Parsing large answers allocates a lot, and the allocator of musl (the
/// Linux builds) is slow.
#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// Runs on a thread with an 8 MiB stack: Windows gives the main thread
/// 1 MiB, and building the command tree in debug builds needs more.
fn main() -> ExitCode {
    std::thread::Builder::new()
        .name("sn".into())
        .stack_size(8 << 20)
        .spawn(|| {
            // One worker is enough for spawned tasks and starts faster than
            // one per core.
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .enable_all()
                .build()
                .expect("cannot start the tokio runtime")
                .block_on(run())
        })
        .expect("cannot start the main thread")
        .join()
        .unwrap_or_else(|_| {
            // The panic message is on stderr; agents still get an envelope.
            report(Err(Error::Internal(
                "a defect in sn stopped the command; what it already sent to the instance stays done, so check there before you repeat a write, and report the message on stderr at https://github.com/racetozero/servicenow-cli/issues".into(),
            )))
        })
}

async fn run() -> ExitCode {
    let args: Vec<String> = std::env::args_os()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    // Answers shell completion requests and exits.
    clap_complete::CompleteEnv::with_factory(cli::completion_command)
        .var("SN_COMPLETE")
        .complete();
    let check = cmd::update::start(&args);
    let result = tokio::select! {
        result = cli::run(args) => result,
        _ = ctx::interrupted() => {
            let _ = writeln!(std::io::stderr(), "interrupted");
            return ExitCode::from(130);
        }
    };
    let code = report(result);
    cmd::update::finish(check).await;
    code
}

/// Prints an error (an envelope on stdout in agent mode, one line of text on
/// a terminal, JSON on stderr otherwise) and gives the exit code.
fn report(result: sn_core::Result<()>) -> ExitCode {
    let Err(err) = result else {
        return ExitCode::SUCCESS;
    };
    if let Error::DryRun(request) = &err {
        output::dry_run(request);
        return ExitCode::SUCCESS;
    }
    let error = err.to_json();
    if output::agent_mode() {
        output::print_line(
            &serde_json::json!({ "ok": false, "command": output::command_name(), "error": error })
                .to_string(),
        );
    } else if std::io::stderr().is_terminal() {
        let mut text = format!("{} {err}", output::paint("31;1", "error:", true));
        if let Error::Api(failure) = &err
            && let Some(detail) = &failure.detail
        {
            text.push_str(&format!("\n  {detail}"));
        }
        if let Error::Input { arg, choices, .. } = &err
            && !choices.is_empty()
        {
            text.push_str(&format!("\nvalues for {arg}:"));
            for choice in choices {
                match choice.label.is_empty() || choice.label == choice.value {
                    true => text.push_str(&format!("\n  {}", choice.value)),
                    false => text.push_str(&format!("\n  {}  ({})", choice.value, choice.label)),
                }
            }
        }
        let _ = writeln!(std::io::stderr(), "{text}");
    } else {
        let _ = writeln!(
            std::io::stderr(),
            "{}",
            serde_json::json!({ "error": error })
        );
    }
    ExitCode::from(err.exit_code())
}

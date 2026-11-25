//! One command as another user.
//!
//! Impersonation belongs to a session on the server, and a credential on a
//! request ends it. So this opens a private session with the profile's
//! credential, switches it, and runs the command on its cookies alone. The
//! cookies stay in this process. The session is switched back and signed out
//! however the command ends: success, error, panic, or Ctrl-C.
//!
//! The instance answers the switch with success even when it refuses it, so
//! the session is asked who it is before the command runs.

use crate::cli::{self, Command, GlobalArgs};
use crate::cmd::flow::FlowCommand;
use crate::ctx::{Ctx, HandlesInterrupt};
use crate::output;
use clap::Args;
use futures_util::FutureExt;
use serde_json::{Value, json};
use sn_core::client::{Method, Request};
use sn_core::config::AuthKind;
use sn_core::uisession::{SESSION_PATH, SessionInfo, UiSession};
use sn_core::{Client, DisplayValue, Error, Result, TableQuery, is_sys_id, query, table};
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;

/// The longest wait for the instance to end the session.
const END_WAIT: Duration = Duration::from_secs(30);

#[derive(Args, Debug)]
pub struct ImpersonateArgs {
    /// User to act as: a user name or sys_id. Your user needs the admin or impersonator role.
    pub user: String,
    /// The command, after --. Connection options go before --.
    #[arg(last = true, required = true, value_name = "COMMAND")]
    pub command: Vec<String>,
}

pub async fn run(ctx: &Ctx, args: ImpersonateArgs) -> Result<()> {
    // Everything that can be wrong with the arguments comes before the
    // session exists.
    let Some((inner, name)) = prepare(&ctx.globals, args.command)? else {
        return Ok(());
    };
    let user = args.user.trim();
    if user.is_empty() || user.contains(char::is_control) {
        return Err(Error::invalid(
            "the user to act as is empty; pass a user name or a sys_id",
        ));
    }
    // The options of the command decide: it can ask for a dry run itself.
    let inner_ctx = Ctx::new(inner.globals)?;
    let client = ctx.client().await?;
    if matches!(client.auth_kind(), AuthKind::Cookie | AuthKind::Session) {
        return Err(Error::invalid(format!(
            "profile `{}` signs in with one shared session, and impersonating would switch that session everywhere; use a profile with another method, for example `sn auth login --method sso`",
            client.profile_name()
        )));
    }
    let opened = client.send(Request::new(Method::GET, SESSION_PATH)).await?;
    let state = SessionInfo::read(opened.json()?);
    current(&state)?;
    let mut session = UiSession::opened_by(&opened)?;
    session.user_token = state.token.clone().unwrap_or_default();
    let mut hop = Hop {
        client: Arc::new(client.for_session(&session, inner_ctx.options()).await),
        original: None,
    };
    // From here on the session exists, so every way out ends it.
    let _handles = HandlesInterrupt::start();
    let outcome = {
        let work = switch_and_run(inner_ctx, &mut hop, &state, user, inner.command, &name);
        tokio::select! {
            outcome = AssertUnwindSafe(work).catch_unwind() => Some(outcome),
            _ = tokio::signal::ctrl_c() => None,
        }
    };
    let unconfirmed = |why: &str| {
        Error::api(
            format!(
                "{why} before the instance confirmed that the impersonation of `{user}` ended; no other program has its session, which ends when it is idle"
            ),
            Value::Null,
        )
    };
    let ended = tokio::select! {
        ended = tokio::time::timeout(END_WAIT, hop.end()) => ended.unwrap_or_else(|_| {
            Err(unconfirmed(&format!("{} seconds passed", END_WAIT.as_secs())))
        }),
        _ = tokio::signal::ctrl_c() => Err(unconfirmed("you pressed Ctrl-C")),
    };
    let Some(outcome) = outcome else {
        match ended {
            Ok(()) => eprintln!("interrupted; the impersonation was ended"),
            Err(err) => ctx.out.warn(&err.to_string()),
        }
        std::process::exit(130);
    };
    match (outcome, ended) {
        // The command's envelope is on stdout already.
        (Ok(Ok(())), Err(end)) if ctx.out.agent => {
            ctx.out.warn(&end.to_string());
            std::process::exit(1);
        }
        (Ok(Ok(())), ended) => ended,
        (Ok(Err(err)), ended) => {
            if let Err(end) = ended {
                ctx.out.warn(&end.to_string());
            }
            Err(err)
        }
        (Err(panic), _) => std::panic::resume_unwind(panic),
    }
}

async fn switch_and_run(
    ctx: Ctx,
    hop: &mut Hop,
    state: &SessionInfo,
    user: &str,
    command: Command,
    name: &str,
) -> Result<()> {
    if state.can_impersonate == Some(false) {
        return Err(Error::Other {
            code: "permission",
            message: format!(
                "`{}` may not impersonate other users on {}; ask for the impersonator role",
                current(state)?,
                hop.client.host()
            ),
            data: Value::Null,
        });
    }
    let field = match is_sys_id(user) {
        true => "sys_id",
        false => "user_name",
    };
    let (original, target) = tokio::try_join!(
        find_user(&hop.client, "user_name", current(state)?),
        find_user(&hop.client, field, user),
    )?;
    hop.original = Some(original.clone());
    hop.become_user(&target.sys_id).await?;
    let after = SessionInfo::read(
        hop.client
            .send(Request::new(Method::GET, SESSION_PATH))
            .await?
            .json()?,
    );
    let switched = current(&after)?.eq_ignore_ascii_case(&target.user_name)
        && after
            .original_user
            .as_deref()
            .is_some_and(|o| o.eq_ignore_ascii_case(&original.user_name));
    if !switched {
        return Err(Error::api(
            format!(
                "the instance accepted the request but did not switch to `{}`: the session is still `{}`; check that your user may impersonate this user",
                target.user_name,
                current(&after)?
            ),
            Value::Null,
        ));
    }
    if after.token != state.token {
        let mut session = hop.client.ui_session().await;
        session.user_token = after.token.unwrap_or_default();
        hop.client = Arc::new(hop.client.for_session(&session, ctx.options()).await);
    }
    ctx.out.note(&format!(
        "acting as {} (signed in as {})",
        target.user_name, original.user_name
    ));
    output::set_command(name.to_owned());
    let ctx = ctx.with_client(Arc::clone(&hop.client));
    cli::run_inner(&ctx, command).await
}

/// Parses the command and gives it this run's connection.
fn prepare(outer: &GlobalArgs, mut command: Vec<String>) -> Result<Option<(cli::Cli, String)>> {
    if command.first().is_none_or(|word| word != "sn") {
        command.insert(0, "sn".into());
    }
    if command.len() == 1 {
        return Err(Error::invalid(
            "there is no command after --; pass one, for example `sn impersonate abel.tuter -- table list incident`",
        ));
    }
    let Some((mut inner, name)) = cli::parse_inner(command)? else {
        return Ok(None);
    };
    if let Some(why) = refusal(&inner.command) {
        return Err(Error::invalid(format!(
            "you asked to run `{name}` as another user, but {why}; run it without `impersonate`"
        )));
    }
    let globals = &mut inner.globals;
    let connection = [
        ("--profile", globals.profile.is_some()),
        ("--proxy", globals.proxy.is_some()),
        ("--no-proxy", globals.no_proxy),
        ("--ca-cert", globals.ca_cert.is_some()),
        ("--client-cert", globals.client_cert.is_some()),
        ("--insecure", globals.insecure),
        ("--timeout", globals.timeout.is_some()),
        ("--agent", globals.agent && !output::agent_mode()),
    ];
    if let Some((flag, _)) = connection.iter().find(|(_, set)| *set) {
        return Err(Error::invalid(format!(
            "you passed {flag} after --; put it before --, for example `sn impersonate USER {flag} ... -- COMMAND`"
        )));
    }
    globals.profile.clone_from(&outer.profile);
    globals.proxy.clone_from(&outer.proxy);
    globals.no_proxy = outer.no_proxy;
    globals.ca_cert.clone_from(&outer.ca_cert);
    globals.client_cert.clone_from(&outer.client_cert);
    globals.insecure = outer.insecure;
    globals.timeout = outer.timeout;
    // Output options work on both sides.
    if globals.output == output::Format::Auto {
        globals.output = outer.output;
    }
    if globals.select.is_empty() {
        globals.select.clone_from(&outer.select);
    }
    if globals.jq.is_none() {
        globals.jq.clone_from(&outer.jq);
    }
    if globals.debug.is_none() {
        globals.debug.clone_from(&outer.debug);
    }
    globals.verbose = globals.verbose.max(outer.verbose);
    globals.json |= outer.json;
    globals.raw |= outer.raw;
    globals.yes |= outer.yes;
    globals.dry_run |= outer.dry_run;
    globals.read_only |= outer.read_only;
    globals.no_input |= outer.no_input;
    globals.quiet |= outer.quiet;
    globals.no_headers |= outer.no_headers;
    globals.no_truncate |= outer.no_truncate;
    globals.no_pager |= outer.no_pager;
    Ok(Some((inner, name)))
}

/// Why a command cannot run as someone else.
fn refusal(command: &Command) -> Option<&'static str> {
    match command {
        Command::Impersonate(_) => Some("impersonations do not nest"),
        Command::Auth(_) | Command::Init(_) | Command::Profile(_) | Command::Sdk(_) => {
            Some("it works with the profile's own sign-in")
        }
        Command::Config(_)
        | Command::Alias(_)
        | Command::Skill(_)
        | Command::Introspect(_)
        | Command::Completions(_)
        | Command::Update(_) => Some("it does not work with records of the instance"),
        Command::Cache(_) => {
            Some("the offline copy is shared and must hold what the profile itself can see")
        }
        Command::Watch(_) | Command::Flow(FlowCommand::Tail(_)) => {
            Some("a watcher needs new sessions all the time, and the impersonated one is single")
        }
        Command::Script(_) | Command::Exec(_) => {
            Some("background scripts need a session of their own")
        }
        Command::Open(_) => Some("your browser has its own session"),
        Command::Tui(_) => Some("the full-screen workspace uses its own session"),
        _ => None,
    }
}

#[derive(Debug, Clone)]
struct User {
    sys_id: String,
    user_name: String,
}

async fn find_user(client: &Client, field: &str, value: &str) -> Result<User> {
    let params = TableQuery::new(query::term(field, "=", value))
        .fields(&["sys_id", "user_name"])
        .limit(2)
        .display(DisplayValue::False);
    let rows = client.list("sys_user", &params).await?.rows;
    match rows.as_slice() {
        [] => Err(Error::NotFound(format!(
            "there is no user `{value}` that you can read; pass a user name or a sys_id"
        ))),
        [row]
            if table::raw(row, field).eq_ignore_ascii_case(value)
                && is_sys_id(table::raw(row, "sys_id"))
                && !table::raw(row, "user_name").is_empty() =>
        {
            Ok(User {
                sys_id: table::raw(row, "sys_id").to_owned(),
                user_name: table::raw(row, "user_name").to_owned(),
            })
        }
        // Both fields are unique: anything else was not filtered.
        _ => Err(Error::api(
            format!(
                "the instance ignored the filter for the user `{value}`, so nobody was impersonated"
            ),
            Value::Null,
        )),
    }
}

/// Whose session it is.
fn current(info: &SessionInfo) -> Result<&str> {
    info.current_user.as_deref().ok_or_else(|| {
        Error::api(
            "the instance did not say whose session this is, so impersonation is not possible",
            Value::Null,
        )
    })
}

/// The private session.
struct Hop {
    client: Arc<Client>,
    /// Set just before the switch: from then on ending includes going back.
    original: Option<User>,
}

impl Hop {
    /// It changes only the session, so read-only profiles and dry runs may
    /// do it.
    async fn become_user(&self, sys_id: &str) -> Result<()> {
        let path = format!("/api/now/ui/impersonate/{sys_id}");
        let request = Request::new(Method::POST, path).json(json!({})).reads();
        self.client.send(request).await.map(|_| ())
    }

    /// Goes back, signs out, and checks that the session is gone.
    async fn end(&self) -> Result<()> {
        // The sign-out can delete them from the client.
        let cookies = self.client.ui_session().await.cookie_header();
        let reverted = match &self.original {
            Some(user) => self.become_user(&user.sys_id).await,
            None => Ok(()),
        };
        let signed_out = self
            .client
            .send(Request::new(Method::GET, "/logout.do"))
            .await;
        let check = Request::new(Method::GET, SESSION_PATH)
            .no_auth()
            .header("cookie", cookies);
        let body = match self.client.send(check).await {
            Err(Error::Auth(_)) => return Ok(()),
            Err(Error::Api(failure)) if matches!(failure.status, Some(401 | 403)) => return Ok(()),
            Err(err) => return Err(reverted.err().or(signed_out.err()).unwrap_or(err)),
            Ok(answer) if answer.status >= 300 => return Ok(()),
            Ok(answer) => answer.json()?,
        };
        let current = SessionInfo::read(body).current_user.unwrap_or_default();
        let back = self
            .original
            .as_ref()
            .is_none_or(|user| user.user_name.eq_ignore_ascii_case(&current));
        match back {
            // Still open, but the person's own again, and nobody has it.
            true => Ok(()),
            false => Err(Error::api(
                format!(
                    "the impersonated session could not be ended (it is still `{current}`); no other program has its cookies, and it ends when it is idle"
                ),
                Value::Null,
            )),
        }
    }
}

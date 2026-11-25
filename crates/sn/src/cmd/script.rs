//! Server-side JavaScript through *Scripts - Background* (`sys.scripts.do`).
//!
//! The processor needs a session of the web interface and answers with HTML.
//! "Script completed" is printed for failures too; the output is in `<PRE>`,
//! one entry per `<BR/>`, HTML-escaped. Script output has the prefix
//! `*** Script: ` in global and `SCOPE: ` in an application scope.

use super::cicd;
use super::lookup::one;
use crate::ctx::{Ctx, read_input};
use clap::{Args, Subcommand};
use serde_json::{Value, json};
use sn_core::client::{Method, Request};
use sn_core::table::raw;
use sn_core::uisession::UiSession;
use sn_core::{Client, DisplayValue, Error, Result, TableQuery, is_sys_id, query};
use std::time::Duration;

const PATH: &str = "/sys.scripts.do";
const GLOBAL: &str = "global";
/// Scripts run longer than other requests.
const TIMEOUT: Duration = Duration::from_secs(300);
/// The instance keeps only the last lines of a run.
const MAX_LINES: usize = 50_001;

#[derive(Subcommand, Debug)]
pub enum ScriptCommand {
    /// Run JavaScript on the server as your user.
    Run(RunArgs),
}

#[derive(Args, Debug)]
pub struct RunArgs {
    /// JavaScript, @file, or - for stdin.
    script: Option<String>,
    /// File with the script (- for stdin).
    #[arg(long, short = 'f', value_name = "FILE", conflicts_with = "script")]
    file: Option<String>,
    /// Application scope: scope name, name, or sys_id (default: the scope of now.config.json here, else global).
    #[arg(long, value_name = "SCOPE")]
    scope: Option<String>,
    /// Record the changes so that they can be rolled back (the default).
    #[arg(long, alias = "record-for-rollback", overrides_with = "no_rollback")]
    rollback: bool,
    /// Do not record the changes (faster).
    #[arg(long, alias = "no-record-for-rollback")]
    no_rollback: bool,
    /// Why you run it; kept as a comment in the script's history.
    #[arg(long, value_name = "TEXT")]
    reason: Option<String>,
    /// Also the session log of the run, with stack traces.
    #[arg(long)]
    server_logs: bool,
}

pub async fn run(ctx: &Ctx, command: ScriptCommand) -> Result<()> {
    match command {
        ScriptCommand::Run(args) => exec(ctx, args).await,
    }
}

pub async fn exec(ctx: &Ctx, args: RunArgs) -> Result<()> {
    let wanted = args
        .scope
        .or_else(|| cicd::Project::here()?.scope_id)
        .unwrap_or_else(|| GLOBAL.to_owned());
    if wanted.trim().is_empty() {
        return Err(Error::invalid(
            "--scope is empty; pass a scope such as global or x_acme_app",
        ));
    }
    let client = ctx.client().await?;
    let mut script = super::args::text_or_file(
        args.script
            .as_deref()
            .map(read_input)
            .transpose()?
            .as_deref(),
        args.file.as_deref(),
        "script",
    )?;
    ctx.confirm(&format!(
        "run a script in scope {wanted} on {} with all rights of your user",
        client.host()
    ))?;
    if let Some(reason) = &args.reason {
        let reason = reason.split_whitespace().collect::<Vec<_>>().join(" ");
        script.push_str(&format!("\n// Reason: {reason}\n"));
    }
    let scope = scope(client, wanted.trim());
    let (session, (scope, scope_id)) = tokio::try_join!(UiSession::open(client), scope)?;
    let mut form = vec![
        ("script".to_owned(), script),
        ("sysparm_ck".to_owned(), session.user_token.clone()),
        ("runscript".to_owned(), "Run script".to_owned()),
        // The transaction quota of the instance applies, as in the form.
        ("quota_managed_transaction".to_owned(), "on".to_owned()),
        ("sys_scope".to_owned(), scope_id),
    ];
    if !args.no_rollback {
        form.push(("record_for_rollback".to_owned(), "on".to_owned()));
    }
    let mut request = session.attach(client, Request::new(Method::POST, PATH).form(form));
    request = request.header("accept", "*/*");
    if ctx.globals.timeout.is_none() {
        request = request.timeout(TIMEOUT);
    }
    let body = send(client, request, &scope).await?;
    let page = judge(&body, &scope)?;
    let rollback = match &page.history_id {
        Some(id) if !args.no_rollback => rollback_context(ctx, client, id).await,
        _ => None,
    };
    if page.lines >= MAX_LINES {
        ctx.out.warn(
            "the instance keeps only the last 50001 lines of a run; earlier output is missing",
        );
    }
    if page
        .log
        .iter()
        .any(|l| l.contains("ReadOnlyRoleAccessHandler"))
    {
        ctx.out.warn("your user has the role snc_read_only, so the instance ran the script in a sandbox and kept no history");
    }
    let mut data = json!({
        "ok": page.error.is_none(), "scope": page.scope, "output": page.output,
        "messages": page.messages, "error": page.error, "elapsed_ms": page.elapsed_ms,
        "history_id": page.history_id, "rollback_context": rollback,
    });
    if args.server_logs {
        data["log"] = page.log.clone().into();
    }
    // Agents get one envelope: the result, or the error that carries it.
    let failed = page.error.is_some() || page.scope != scope;
    if !failed || !ctx.out.agent {
        let text = page.output.join("\n");
        ctx.out.item_with(data.clone(), json!({}), |_| text)?;
    }
    if ctx.out.wants_text() {
        for line in page.log.iter().filter(|_| args.server_logs) {
            ctx.out.note(line);
        }
        let mut note = format!("ran in scope {}", page.scope);
        if let Some(id) = &page.history_id {
            note.push_str(&format!("; history {id}"));
        }
        if let Some(id) = &rollback {
            note.push_str(&format!("; rollback context {id}"));
        }
        ctx.out.note(&crate::output::dim(&note, true));
    }
    if let Some(error) = &page.error {
        let line = match error["line"].as_u64() {
            Some(line) => format!(" (line {line})"),
            None => String::new(),
        };
        return Err(Error::Other {
            code: "script_error",
            message: format!(
                "the script has a {} error{line}: {}",
                error["type"].as_str().unwrap_or_default(),
                error["message"].as_str().unwrap_or_default()
            ),
            data,
        });
    }
    if page.scope != scope {
        return Err(Error::api(
            format!(
                "you asked for scope {scope}, but the instance ran the script in {}",
                page.scope
            ),
            data,
        ));
    }
    Ok(())
}

/// The scope's name and the sys_id that the form needs. The instance runs
/// an unknown scope in global, so it must exist.
async fn scope(client: &Client, wanted: &str) -> Result<(String, String)> {
    if wanted.eq_ignore_ascii_case(GLOBAL) {
        return Ok((GLOBAL.to_owned(), GLOBAL.to_owned()));
    }
    let filter = match is_sys_id(wanted) {
        true => query::term("sys_id", "=", wanted),
        false => format!(
            "{}^OR{}",
            query::term("scope", "=", wanted),
            query::term("name", "=", wanted)
        ),
    };
    let params = TableQuery::new(filter)
        .fields(&["sys_id", "scope", "name"])
        .limit(5)
        .display(DisplayValue::False);
    let mut rows = client.list("sys_scope", &params).await?.rows;
    rows.retain(|row| {
        ["sys_id", "scope", "name"]
            .iter()
            .any(|f| raw(row, f).eq_ignore_ascii_case(wanted))
    });
    let none = Error::NotFound(format!(
        "there is no application scope `{wanted}` on {}, so the script did not run; pass a scope name, application name or sys_id",
        client.host()
    ));
    let several = format!("several applications match the scope `{wanted}`; pass a sys_id");
    let row = one(rows, "scope", none, several, |row| {
        format!("{} ({})", raw(row, "scope"), raw(row, "name"))
    })?;
    Ok((
        raw(&row, "scope").to_owned(),
        raw(&row, "sys_id").to_owned(),
    ))
}

async fn send(client: &Client, request: Request, scope: &str) -> Result<String> {
    let refused = || {
        Error::Other {
        code: "permission",
        message: "the instance does not let your user run background scripts; the admin role is needed, and the script did not run".into(),
        data: Value::Null,
    }
    };
    let response = match client.open(request).await {
        Ok(response) => response,
        Err(err) if matches!(err.status(), Some(401 | 403)) => return Err(refused()),
        Err(err) if err.code() == "timeout" || matches!(err, Error::Network(_)) => {
            return Err(Error::Other {
                code: err.code(),
                message: format!(
                    "{err}; the script may still run on the instance, so look at sys_script_execution_history before you run it again"
                ),
                data: Value::Null,
            });
        }
        Err(err) => return Err(err),
    };
    // A refusal is a redirect to a page for people.
    if response.status().as_u16() >= 300 || response.url().path() != PATH {
        return Err(refused());
    }
    let body = response.text().await?;
    match body.trim() {
        "" => Err(Error::Auth(
            "the instance did not accept the session, and the script did not run; sign in again with `sn auth login`".into(),
        )),
        text if text.eq_ignore_ascii_case("not authorized") => Err(Error::Other {
            code: "permission",
            message: match scope {
                GLOBAL => "the instance rejected the session token, and the script did not run; sign in again with `sn auth login`".to_owned(),
                _ => format!(
                    "the instance refuses scripts in scope {scope} (it does for applications from the store), and the script did not run; use another --scope"
                ),
            },
            data: Value::Null,
        }),
        _ => Ok(body),
    }
}

/// Empty when the script changed nothing.
async fn rollback_context(ctx: &Ctx, client: &Client, history: &str) -> Option<String> {
    let params = TableQuery::default()
        .fields(&["rollback_context"])
        .display(DisplayValue::False);
    match client
        .record("sys_script_execution_history", history, &params)
        .await
    {
        Ok(row) => Some(raw(&row, "rollback_context").trim().to_owned()).filter(|t| !t.is_empty()),
        // The run itself worked.
        Err(err) => {
            ctx.out
                .warn(&format!("the rollback context is not known ({err})"));
            None
        }
    }
}

/// One run, read from the result page.
#[derive(Debug, Default, PartialEq)]
struct Page {
    scope: String,
    elapsed_ms: Option<u64>,
    history_id: Option<String>,
    output: Vec<String>,
    /// Other entries before an error: slow business rules, SQL debugging.
    messages: Vec<String>,
    /// Every entry that is not output.
    log: Vec<String>,
    error: Option<Value>,
    lines: usize,
}

fn judge(body: &str, scope: &str) -> Result<Page> {
    parse(body).ok_or_else(|| {
        Error::api(
            format!(
                "the instance answered with a page this command does not know, so it is unclear whether the script ran in {scope}; look at sys_script_execution_history"
            ),
            body.chars().take(500).collect::<String>().into(),
        )
    })
}

fn parse(body: &str) -> Option<Page> {
    let after = |mark: &str| body.split_once(mark).map(|(_, rest)| rest);
    let scope = after("Script completed in scope ")?
        .split(':')
        .next()?
        .trim()
        .to_owned();
    let history_id: Option<String> = after("sys_script_execution_history.do?sys_id=")
        .map(|rest| rest.chars().take_while(char::is_ascii_hexdigit).collect())
        .filter(|id: &String| !id.is_empty());
    let elapsed_ms = body
        .trim_start()
        .strip_prefix('[')
        .and_then(|rest| parse_elapsed(rest.split(']').next()?));
    let start = body.find("<PRE>")? + "<PRE>".len();
    let text = body.get(start..body.rfind("</PRE>")?)?;
    let prefix = match scope.as_str() {
        GLOBAL => "*** Script: ".to_owned(),
        other => format!("{other}: "),
    };
    let mut page = Page {
        scope,
        elapsed_ms,
        history_id,
        ..Page::default()
    };
    let mut error: Option<(&str, Vec<String>)> = None;
    for entry in text.split("<BR/>").filter(|e| !e.is_empty()) {
        let entry = unescape(entry);
        page.lines += 1;
        if let Some((_, lines)) = &mut error {
            // Execution stopped; the rest is detail.
            lines.push(entry.clone());
            page.log.push(entry);
        } else if let Some(line) = entry.strip_prefix(&prefix) {
            page.output.push(line.to_owned());
        } else if entry.starts_with("Script compilation error:") {
            page.log.push(entry.clone());
            error = Some(("compilation", vec![entry]));
        } else if entry.starts_with("Script execution error:") {
            page.log.push(entry.clone());
            error = Some(("execution", vec![entry]));
        } else {
            page.log.push(entry.clone());
            page.messages.push(entry);
        }
    }
    page.error = error.map(|(kind, lines)| {
        let detail = lines.join("\n");
        json!({
            "type": kind,
            "message": description(&lines[0]),
            "line": error_line(&detail),
            "detail": detail,
        })
    });
    Some(page)
}

/// `..., Error Description: TEXT, Script ES Level: ...`, else the first line.
fn description(entry: &str) -> &str {
    match entry.split_once("Error Description: ") {
        Some((_, rest)) => rest.split(", Script ES Level:").next().unwrap_or(rest),
        None => entry.lines().next().unwrap_or(entry),
    }
    .trim()
}

/// `(null.null.script; line 3)` or `Line(3)`.
fn error_line(detail: &str) -> Option<u64> {
    ["; line ", "Line("].iter().find_map(|mark| {
        let (_, rest) = detail.split_once(mark)?;
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        digits.parse().ok()
    })
}

/// `H:MM:SS.mmm` in milliseconds.
fn parse_elapsed(text: &str) -> Option<u64> {
    let (clock, ms) = text.trim().split_once('.')?;
    let mut seconds = 0;
    for part in clock.split(':') {
        seconds = seconds * 60 + part.parse::<u64>().ok()?;
    }
    Some(seconds * 1000 + ms.parse::<u64>().ok()?)
}

fn unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let decoded = rest
            .find(';')
            .filter(|end| *end <= 10)
            .and_then(|end| Some((super::knowledge::entity(&rest[1..end])?, end + 1)));
        match decoded {
            Some((character, length)) => {
                out.push(character);
                rest = &rest[length..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cut from a page of a live instance.
    fn page(scope: &str, pre: &str) -> String {
        format!(
            "[0:00:01.951] <HTML><BODY>Script completed in scope {scope}: script<HR/>Script execution history <A target='blank' HREF='sys_script_execution_history.do?sys_id=48169e6a2fe303107efd1d707fa4e3f4'>available here</A><HR/><PRE>{pre}</PRE><HR/></BODY></HTML>"
        )
    }

    #[test]
    fn output_is_told_from_messages_by_the_scope_prefix() {
        let run = parse(&page(
            "global",
            "*** Script: {&quot;n&quot;:3}<BR/>Slow business rule &#39;X&#x27; &amp;lt;<BR/>",
        ))
        .expect("page");
        assert_eq!(run.output, ["{\"n\":3}"]);
        assert_eq!(run.messages, ["Slow business rule 'X' &lt;"]);
        assert_eq!(run.elapsed_ms, Some(1951));
        assert_eq!(
            run.history_id.as_deref(),
            Some("48169e6a2fe303107efd1d707fa4e3f4")
        );
        let run = parse(&page("x_acme", "x_acme: hi<BR/>*** Script: other<BR/>")).expect("page");
        assert_eq!((run.output, run.messages.len()), (vec!["hi".into()], 1));
    }

    #[test]
    fn errors_keep_their_description_line_and_detail() {
        let run = parse(&page(
            "global",
            "*** Script: before<BR/>Script execution error: Script Identifier: null.null.script, Error Description: Error: boom (null.null.script; line 2), Script ES Level: 0<BR/>Root cause<BR/>",
        ))
        .expect("page");
        let error = run.error.expect("error");
        assert_eq!(error["message"], "Error: boom (null.null.script; line 2)");
        assert_eq!((error["line"].as_u64(), run.output.len()), (Some(2), 1));
        assert!(
            error["detail"]
                .as_str()
                .expect("detail")
                .ends_with("Root cause")
        );
        assert_eq!(parse("<html>login</html>"), None);
    }
}

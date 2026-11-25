//! What every command gets: output, the client, and the rules for asking.

use crate::cli::GlobalArgs;
use crate::output::{self, Out};
use crate::prompt;
use sn_core::client::{Debug, Options};
use sn_core::config::{self, Resolved, Store};
use sn_core::{Choice, Client, Error, Result};
use std::io::{IsTerminal, Read};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::sync::OnceCell;

pub struct Ctx {
    pub out: Out,
    pub globals: GlobalArgs,
    pub store: Store,
    client: OnceCell<Arc<Client>>,
}

impl Ctx {
    pub fn new(globals: GlobalArgs) -> Result<Self> {
        let out = Out {
            agent: output::agent_mode(),
            format: if globals.json {
                output::Format::Json
            } else {
                globals.output
            },
            select: globals.select.clone(),
            jq: globals.jq.clone().map(output::Jq::new),
            raw: globals.raw,
            tty: std::io::stdout().is_terminal(),
            no_headers: globals.no_headers,
            no_truncate: globals.no_truncate,
            quiet: globals.quiet,
            pager: !globals.no_pager && !output::agent_mode(),
        };
        Ok(Self {
            out,
            globals,
            store: Store::open()?,
            client: OnceCell::new(),
        })
    }

    /// The HTTP client of the selected profile, built on first use.
    pub async fn client(&self) -> Result<&Client> {
        self.client
            .get_or_try_init(|| async {
                Client::new(self.resolve()?, Some(self.store.clone()), self.options()).map(Arc::new)
            })
            .await
            .map(|client| &**client)
    }

    /// The same context with a client that is ready (`impersonate`).
    pub fn with_client(self, client: Arc<Client>) -> Self {
        Self {
            client: OnceCell::new_with(Some(client)),
            ..self
        }
    }

    /// The client, when a command already built it.
    pub fn built_client(&self) -> Option<&Client> {
        self.client.get().map(|client| &**client)
    }

    pub fn options(&self) -> Options {
        let g = &self.globals;
        let debug = match (g.verbose, g.debug.as_deref()) {
            (0, None) => Debug::Off,
            (_, Some("full")) | (2.., _) => Debug::Full,
            _ => Debug::Requests,
        };
        Options {
            dry_run: g.dry_run,
            read_only: g.read_only,
            debug,
            timeout: g.timeout.map(Duration::from_secs),
        }
    }

    /// The selected profile with the network flags applied.
    pub fn resolve(&self) -> Result<Resolved> {
        let mut resolved = config::resolve(&self.store, self.globals.profile.as_deref())?;
        self.globals.network(&mut resolved.profile.network);
        Ok(resolved)
    }

    /// Questions are allowed: a terminal, not an agent, not `--no-input`.
    pub fn interactive(&self) -> bool {
        !self.out.agent
            && !self.globals.no_input
            && std::io::stdin().is_terminal()
            && std::io::stderr().is_terminal()
    }

    /// Guards a destructive action, for example "delete incident INC1".
    pub fn confirm(&self, action: &str) -> Result<()> {
        if self.globals.yes || self.globals.dry_run {
            return Ok(());
        }
        if !self.interactive() {
            return Err(Error::ConfirmationRequired(format!(
                "this will {action}; run the command again with --yes to allow it"
            )));
        }
        match prompt::confirm(&format!("This will {action}. Continue?"))? {
            true => Ok(()),
            false => Err(Error::invalid("you answered no; nothing was changed")),
        }
    }

    /// A value that a flag left out: asked on a terminal, an error that
    /// names the flag otherwise.
    pub fn need(&self, arg: &str, question: &str, choices: Vec<Choice>) -> Result<String> {
        if !self.interactive() {
            let shown: Vec<&str> = choices.iter().map(|c| c.value.as_str()).collect();
            let hint = match shown.is_empty() {
                true => String::new(),
                false => format!(", one of: {}", shown.join(", ")),
            };
            return Err(Error::Input {
                arg: arg.to_owned(),
                message: format!("`{arg}` is missing ({question}); pass it{hint}"),
                choices,
            });
        }
        if choices.is_empty() {
            return prompt::text(question);
        }
        let labels: Vec<String> = choices
            .iter()
            .map(|c| {
                if c.label.is_empty() {
                    c.value.clone()
                } else {
                    c.label.clone()
                }
            })
            .collect();
        let at = prompt::choose(question, &labels)?;
        Ok(choices[at].value.clone())
    }
}

/// `@file`, `-`/`@-` for stdin, or the text itself.
pub fn read_input(text: &str) -> Result<String> {
    let path = match text {
        "-" | "@-" => {
            let mut content = String::new();
            std::io::stdin()
                .read_to_string(&mut content)
                .map_err(|err| {
                    Error::invalid(format!(
                        "cannot read stdin ({err}); pipe UTF-8 text into the command"
                    ))
                })?;
            return Ok(content);
        }
        _ => match text.strip_prefix('@') {
            Some(path) => path,
            None => return Ok(text.to_owned()),
        },
    };
    std::fs::read_to_string(path).map_err(|err| {
        Error::invalid(format!(
            "cannot read {path} ({err}); pass a UTF-8 text file that exists"
        ))
    })
}

/// A secret from stdin, without its line break.
pub fn read_secret_stdin() -> Result<String> {
    let secret = read_input("-")?.trim().to_owned();
    match secret.is_empty() {
        true => Err(Error::invalid(
            "stdin was empty; pipe the secret into the command, for example `sn ... < secret.txt`",
        )),
        false => Ok(secret),
    }
}

static FINISHING: AtomicUsize = AtomicUsize::new(0);

/// While this exists, Ctrl-C does not end the process: the command listens
/// for it and cleans up (ends an impersonation, leaves a channel).
pub struct HandlesInterrupt(());

impl HandlesInterrupt {
    pub fn start() -> Self {
        FINISHING.fetch_add(1, Ordering::SeqCst);
        Self(())
    }
}

impl Drop for HandlesInterrupt {
    fn drop(&mut self) {
        FINISHING.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Resolves at a Ctrl-C that no command handles itself.
pub async fn interrupted() {
    loop {
        if tokio::signal::ctrl_c().await.is_err() {
            return std::future::pending().await;
        }
        if FINISHING.load(Ordering::SeqCst) == 0 {
            return;
        }
    }
}

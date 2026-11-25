//! Signing in and out.
//!
//! The browser sign-in is the OAuth authorization code grant with PKCE in
//! the person's own default browser, so single sign-on works as it does for
//! any other page. The link is always printed. The answer comes back through
//! a callback on this computer, or through a code the person pastes.

use crate::browser;
use crate::ctx::{Ctx, read_secret_stdin};
use crate::prompt;
use clap::{Args, Subcommand, ValueEnum};
use serde_json::{Value, json};
use sn_core::client::{Method, Request};
use sn_core::config::{self, AuthKind, Grant, OAuthConfig, Profile, Resolved, Secrets, StoreKind};
use sn_core::oauth::{self, Callback, Pending};
use sn_core::uisession::UiSession;
use sn_core::{Choice, Client, Error, Result, now_unix, refs};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Subcommand, Debug)]
pub enum AuthCommand {
    /// Sign in to an instance and save the profile.
    Login(Box<LoginArgs>),
    /// Remove the stored credential of a profile.
    Logout {
        /// Keep the token valid on the instance (do not revoke it).
        #[arg(long)]
        local: bool,
    },
    /// Show how a profile signs in and whether it works.
    #[command(visible_alias = "whoami")]
    Status {
        /// Do not contact the instance.
        #[arg(long)]
        offline: bool,
    },
    /// Get a new access token now.
    Refresh,
    /// Print the live credential for other tools.
    Token {
        /// How to print it.
        #[arg(long = "as", value_enum, default_value_t = TokenFormat::Headers, value_name = "FORM")]
        form: TokenFormat,
    },
    /// Make a profile the default.
    Switch {
        /// Profile name.
        name: Option<String>,
    },
    /// Create the OAuth client that lets everyone sign in without pasting (administrators).
    Register(RegisterArgs),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum TokenFormat {
    /// HTTP headers, one per line.
    Headers,
    /// The bearer token alone.
    Bearer,
    /// Shell variables for now-sdk and scripts.
    Env,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum MethodArg {
    /// Browser sign-in with your company login (OAuth with PKCE).
    #[value(alias = "oauth", alias = "browser", alias = "authorization_code")]
    Sso,
    /// User name and password on every request.
    Basic,
    /// User name, password and MFA code, for a session.
    Session,
    /// OAuth password grant.
    Password,
    /// OAuth client credentials (service account).
    #[value(alias = "client_credentials")]
    ClientCredentials,
    /// OAuth with a signed JWT (service account, no shared secret).
    #[value(alias = "jwt_bearer", alias = "jwt-bearer")]
    Jwt,
    /// Inbound REST API key.
    #[value(alias = "apikey")]
    ApiKey,
    /// A bearer token you have, or a command that prints one.
    #[value(alias = "bearer")]
    Token,
    /// A copied browser session (cookie header).
    Cookie,
    /// A TLS client certificate (--client-cert).
    #[value(alias = "mtls")]
    Certificate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum StoreArg {
    /// A file only you can read.
    File,
    /// The credential store of the operating system.
    Keychain,
}

#[derive(Args, Debug)]
pub struct LoginArgs {
    /// Instance name, host, or URL: acme, acme.service-now.com.
    #[arg(long, short = 'i', value_name = "NAME")]
    pub instance: Option<String>,
    /// How to sign in (default: sso).
    #[arg(long, short = 'm', value_enum, visible_alias = "auth")]
    pub method: Option<MethodArg>,
    /// User name (basic, session, password).
    #[arg(long, short = 'u', value_name = "NAME")]
    pub username: Option<String>,
    /// Read the secret of the method from stdin: password, token, API key, client secret, or cookie.
    #[arg(long, aliases = ["password-stdin", "token-stdin", "api-key-stdin", "client-secret-stdin", "cookie-stdin"])]
    pub secret_stdin: bool,
    /// MFA code (session).
    #[arg(long, value_name = "CODE")]
    pub mfa_code: Option<String>,
    /// OAuth client id. Without it, sso uses the client of the ServiceNow SDK and you paste a code.
    #[arg(long, env = "SN_CLIENT_ID", value_name = "ID")]
    pub client_id: Option<String>,
    /// File with the OAuth client secret (or SN_CLIENT_SECRET).
    #[arg(long, value_name = "FILE")]
    pub client_secret_file: Option<PathBuf>,
    /// Callback port on this computer (default 8400); the client's redirect URL must have it. 0 takes a free port.
    #[arg(long, value_name = "PORT")]
    pub port: Option<u16>,
    /// The whole redirect URL, when it is not http://localhost:PORT/callback.
    #[arg(long, value_name = "URL")]
    pub redirect_uri: Option<String>,
    /// Sign in without PKCE, for instances that refuse it.
    #[arg(long)]
    pub no_pkce: bool,
    /// Accepted for compatibility: the OAuth grant, now a method.
    #[arg(long, hide = true, value_enum, conflicts_with = "method")]
    pub grant: Option<MethodArg>,
    /// Accepted for compatibility: the same as --store file.
    #[arg(long, hide = true, conflicts_with = "store")]
    pub insecure_storage: bool,
    /// Accepted for compatibility: a sign-in always replaces the profile.
    #[arg(long, hide = true)]
    pub force: bool,
    /// OAuth scope to ask for.
    #[arg(long, value_name = "SCOPE")]
    pub scope: Option<String>,
    /// Print the link and do not open a browser.
    #[arg(long)]
    pub no_browser: bool,
    /// Wait for the browser callback even without a terminal.
    #[arg(long)]
    pub wait: bool,
    /// Seconds to wait for the browser.
    #[arg(long, default_value_t = 300, value_name = "SECONDS")]
    pub wait_timeout: u64,
    /// Finish a sign-in that was started without a terminal: the code, or the address the browser ended on.
    #[arg(long, value_name = "CODE|URL")]
    pub code: Option<String>,
    /// Shell command that prints a bearer token (token).
    #[arg(long, value_name = "COMMAND")]
    pub token_command: Option<String>,
    /// Anti-forgery token that belongs to the cookie (g_ck), needed for writes.
    #[arg(long, value_name = "TOKEN")]
    pub user_token: Option<String>,
    /// Private key for the JWT grant (PEM).
    #[arg(long, value_name = "FILE")]
    pub jwt_key_file: Option<PathBuf>,
    /// User the JWT grant acts as.
    #[arg(long, value_name = "USER")]
    pub jwt_subject: Option<String>,
    /// Key id of the instance's JWT verifier map.
    #[arg(long, value_name = "KID")]
    pub jwt_kid: Option<String>,
    /// JWT algorithm: RS256, RS384, RS512, ES256 or ES384.
    #[arg(long, value_name = "ALG", value_parser = ["RS256", "RS384", "RS512", "ES256", "ES384"])]
    pub jwt_alg: Option<String>,
    /// Make the profile refuse writes.
    #[arg(long = "profile-read-only")]
    pub profile_read_only: bool,
    /// Where to keep the secret.
    #[arg(long, value_enum)]
    pub store: Option<StoreArg>,
    /// Save without contacting the instance.
    #[arg(long)]
    pub no_verify: bool,
    /// Make this the default profile.
    #[arg(long)]
    pub set_default: bool,
}

#[derive(Args, Debug)]
pub struct RegisterArgs {
    /// Name of the OAuth client on the instance.
    #[arg(long, default_value = "sn CLI", value_name = "NAME")]
    pub name: String,
    /// Callback ports to allow.
    #[arg(
        long,
        value_delimiter = ',',
        default_value = "8400",
        value_name = "PORTS"
    )]
    pub ports: Vec<u16>,
    /// Only print the settings, to enter them by hand.
    #[arg(long)]
    pub print: bool,
}

pub async fn run(ctx: &Ctx, command: AuthCommand) -> Result<()> {
    match command {
        AuthCommand::Login(args) => login(ctx, *args).await,
        AuthCommand::Logout { local } => logout(ctx, local).await,
        AuthCommand::Status { offline } => status(ctx, offline).await,
        AuthCommand::Refresh => refresh(ctx).await,
        AuthCommand::Token { form } => token(ctx, form).await,
        AuthCommand::Switch { name } => super::profile::switch(ctx, name),
        AuthCommand::Register(args) => register(ctx, args).await,
    }
}

/// A sign-in between the link and the code, kept for `--code`.
#[derive(serde::Serialize, serde::Deserialize)]
struct SavedLogin {
    name: String,
    profile: Profile,
    secrets: Secrets,
    set_default: bool,
    pending: Pending,
}

/// What the browser sign-in gave back.
enum Outcome {
    Code(String),
    /// No terminal to wait in: the link went out, `--code` finishes.
    Deferred,
}

pub async fn login(ctx: &Ctx, args: LoginArgs) -> Result<()> {
    if let Some(code) = &args.code {
        return finish_saved(ctx, &args, code).await;
    }
    let file = ctx.store.load()?;
    let name = ctx
        .globals
        .profile
        .clone()
        .or_else(|| std::env::var("SN_PROFILE").ok().filter(|v| !v.is_empty()))
        .or_else(|| {
            file.default_profile
                .clone()
                .filter(|_| args.instance.is_none())
        })
        .or_else(|| {
            // The profile of the same instance, so a second sign-in renews it.
            let wanted = config::normalize_instance(args.instance.as_deref()?).ok()?;
            let found = file.profiles.iter().find(|(_, p)| p.instance == wanted)?;
            Some(found.0.clone())
        });
    let existing = name.as_ref().and_then(|n| file.profiles.get(n)).cloned();
    let instance = match (&args.instance, &existing) {
        (Some(text), _) => config::normalize_instance(text)?,
        (None, Some(profile)) => profile.instance.clone(),
        (None, None) => match std::env::var("SN_INSTANCE") {
            Ok(text) if !text.is_empty() => config::normalize_instance(&text)?,
            _ => config::normalize_instance(&ctx.need(
                "instance",
                "Instance (for example acme, or https://acme.service-now.com)",
                Vec::new(),
            )?)?,
        },
    };
    let name = name.unwrap_or_else(|| match file.profiles.is_empty() {
        true => config::DEFAULT_PROFILE.to_owned(),
        // A second instance gets its own profile, named after it.
        false => instance
            .split("://")
            .nth(1)
            .and_then(|h| h.split(['.', ':']).next())
            .unwrap_or(config::DEFAULT_PROFILE)
            .to_owned(),
    });
    let method = choose_method(ctx, &args, existing.as_ref())?;
    let same_instance = existing.as_ref().filter(|p| p.instance == instance);
    let mut profile = Profile {
        instance: instance.clone(),
        username: args
            .username
            .clone()
            .or_else(|| same_instance.and_then(|p| p.username.clone())),
        read_only: args.profile_read_only || same_instance.is_some_and(|p| p.read_only),
        store: args
            .store
            .or(args.insecure_storage.then_some(StoreArg::File))
            .map(|s| match s {
                StoreArg::File => StoreKind::File,
                StoreArg::Keychain => StoreKind::Keychain,
            })
            .or_else(|| existing.as_ref().and_then(|p| p.store)),
        network: existing
            .as_ref()
            .map(|p| p.network.clone())
            .unwrap_or_default(),
        ..Profile::default()
    };
    ctx.globals.network(&mut profile.network);
    let mut secrets = Secrets::default();
    let stored_oauth = same_instance.and_then(|p| p.oauth.clone());
    prepare(ctx, &args, method, stored_oauth, &mut profile, &mut secrets).await?;
    let set_default = args.set_default || file.default_profile.is_none();

    if method == MethodArg::Sso {
        let config = profile.oauth.clone().unwrap_or_default();
        let callback = match oauth::loopback_port(&config.redirect_uri) {
            Some(port) if waits(ctx, &args) => Some(Callback::bind(port).await?),
            _ => None,
        };
        let mut config = config;
        if let Some(callback) = callback.as_ref().filter(|_| args.port == Some(0)) {
            config.redirect_uri = format!(
                "http://localhost:{}{}",
                callback.port(),
                oauth::CALLBACK_PATH
            );
        }
        let pending = Pending::new(&instance, &config)?;
        let client = draft_client(ctx, &name, &profile, &secrets)?;
        probe(&client, &config, &pending).await?;
        match browser_sign_in(ctx, &args, &pending, callback).await? {
            Outcome::Code(code) => {
                let tokens = oauth::exchange_code(
                    &client,
                    &config,
                    secrets.client_secret.as_deref(),
                    &pending,
                    &code,
                )
                .await?;
                tokens.store(&mut secrets);
            }
            Outcome::Deferred => {
                let url = pending.url.clone();
                let opened = !args.no_browser && !ctx.out.agent && browser::open(&url);
                save_pending(
                    ctx,
                    &SavedLogin {
                        name: name.clone(),
                        profile,
                        secrets,
                        set_default,
                        pending,
                    },
                )?;
                let next = format!("sn auth login --profile {name} --code 'CODE or ADDRESS'");
                let data = json!({
                    "status": "authorization_required", "profile": name, "instance": instance,
                    "url": url, "browser_opened": opened, "next": next,
                });
                return ctx.out.done(data, &format!(
                    "Open this link and sign in:\n\n  {url}\n\nThen finish with the code, or the address of the page the browser ended on:\n\n  {next}"
                ));
            }
        }
    }
    finish(ctx, &args, &name, profile, secrets, set_default).await
}

/// True when the command may block until the browser answers.
fn waits(ctx: &Ctx, args: &LoginArgs) -> bool {
    args.wait || ctx.interactive()
}

fn choose_method(ctx: &Ctx, args: &LoginArgs, existing: Option<&Profile>) -> Result<MethodArg> {
    if let Some(method) = args.method.or(args.grant) {
        return Ok(method);
    }
    // Flags that only fit one method choose it.
    if args.token_command.is_some() {
        return Ok(MethodArg::Token);
    }
    if args.jwt_key_file.is_some() {
        return Ok(MethodArg::Jwt);
    }
    if args.mfa_code.is_some() {
        return Ok(MethodArg::Session);
    }
    if let Some(profile) = existing.filter(|_| args.instance.is_none()) {
        return Ok(
            match (profile.auth, profile.oauth.as_ref().map(|o| o.grant)) {
                (AuthKind::Oauth, Some(Grant::ClientCredentials)) => MethodArg::ClientCredentials,
                (AuthKind::Oauth, Some(Grant::Password)) => MethodArg::Password,
                (AuthKind::Oauth, Some(Grant::JwtBearer)) => MethodArg::Jwt,
                (AuthKind::Oauth, _) => MethodArg::Sso,
                (AuthKind::Basic, _) => MethodArg::Basic,
                (AuthKind::Session, _) => MethodArg::Session,
                (AuthKind::ApiKey, _) => MethodArg::ApiKey,
                (AuthKind::Token, _) => MethodArg::Token,
                (AuthKind::Cookie, _) => MethodArg::Cookie,
                (AuthKind::Certificate, _) => MethodArg::Certificate,
            },
        );
    }
    if args.client_id.is_some() || !ctx.interactive() {
        return Ok(if args.username.is_some() {
            MethodArg::Basic
        } else {
            MethodArg::Sso
        });
    }
    let methods = [
        (
            MethodArg::Sso,
            "Browser: your company sign-in (SSO) or ServiceNow login",
        ),
        (MethodArg::Basic, "User name and password"),
        (MethodArg::Session, "User name, password and MFA code"),
        (MethodArg::ApiKey, "API key"),
        (
            MethodArg::Token,
            "Bearer token, or a command that prints one",
        ),
        (
            MethodArg::ClientCredentials,
            "OAuth client credentials (service account)",
        ),
        (MethodArg::Jwt, "OAuth signed JWT (service account)"),
        (MethodArg::Password, "OAuth password grant"),
        (MethodArg::Cookie, "Copied browser session (cookie)"),
        (MethodArg::Certificate, "TLS client certificate"),
    ];
    let labels: Vec<String> = methods
        .iter()
        .map(|(_, label)| (*label).to_owned())
        .collect();
    Ok(methods[prompt::choose("How do you sign in?", &labels)?].0)
}

/// Fills the profile and the secrets that the method needs.
async fn prepare(
    ctx: &Ctx,
    args: &LoginArgs,
    method: MethodArg,
    stored: Option<OAuthConfig>,
    profile: &mut Profile,
    secrets: &mut Secrets,
) -> Result<()> {
    let secret = |what: &str| -> Result<String> {
        if args.secret_stdin {
            return read_secret_stdin();
        }
        if !ctx.interactive() {
            return Err(Error::Input {
                arg: "secret-stdin".into(),
                message: format!(
                    "the {what} is missing; pipe it in and pass --secret-stdin (never put it on the command line)"
                ),
                choices: Vec::new(),
            });
        }
        let text = prompt::secret(what)?;
        match text.trim().is_empty() {
            true => Err(Error::invalid(format!("the {what} is empty; enter it"))),
            false => Ok(text.trim().to_owned()),
        }
    };
    let user = |profile: &mut Profile| -> Result<String> {
        if profile.username.is_none() {
            profile.username = Some(ctx.need("username", "User name", Vec::new())?);
        }
        Ok(profile.username.clone().unwrap_or_default())
    };
    let client_secret = || -> Result<Option<String>> {
        if let Some(path) = &args.client_secret_file {
            let text = std::fs::read_to_string(path).map_err(|err| {
                Error::invalid(format!(
                    "cannot read {} ({err}); pass a file with the client secret",
                    path.display()
                ))
            })?;
            return Ok(Some(text.trim().to_owned()));
        }
        Ok(std::env::var("SN_CLIENT_SECRET")
            .ok()
            .filter(|s| !s.is_empty()))
    };
    let client_id = || -> Result<String> {
        match args
            .client_id
            .clone()
            .or_else(|| stored.as_ref().map(|o| o.client_id.clone()))
        {
            Some(id) if id != oauth::SDK_CLIENT_ID => Ok(id),
            _ => ctx.need(
                "client-id",
                "OAuth client id (System OAuth > Application Registry)",
                Vec::new(),
            ),
        }
    };
    match method {
        MethodArg::Sso => {
            profile.auth = AuthKind::Oauth;
            profile.username = None;
            let client_id = args
                .client_id
                .clone()
                .or_else(|| stored.as_ref().map(|o| o.client_id.clone()))
                .unwrap_or_else(|| oauth::SDK_CLIENT_ID.to_owned());
            let same = stored.filter(|o| o.client_id == client_id);
            let redirect_uri = if client_id == oauth::SDK_CLIENT_ID {
                oauth::SDK_REDIRECT.to_owned()
            } else if let Some(uri) = &args.redirect_uri {
                uri.clone()
            } else if let Some(port) = args.port {
                format!("http://localhost:{port}{}", oauth::CALLBACK_PATH)
            } else {
                same.as_ref()
                    .map(|o| o.redirect_uri.clone())
                    .unwrap_or_else(|| {
                        format!(
                            "http://localhost:{}{}",
                            oauth::DEFAULT_PORT,
                            oauth::CALLBACK_PATH
                        )
                    })
            };
            secrets.client_secret = client_secret()?;
            profile.oauth = Some(OAuthConfig {
                client_id,
                redirect_uri,
                no_pkce: args.no_pkce || same.as_ref().is_some_and(|o| o.no_pkce),
                scope: args.scope.clone().or_else(|| same.and_then(|o| o.scope)),
                ..OAuthConfig::default()
            });
        }
        MethodArg::Basic => {
            profile.auth = AuthKind::Basic;
            user(profile)?;
            secrets.password = Some(secret("Password")?);
        }
        MethodArg::Session => {
            profile.auth = AuthKind::Session;
            let user = user(profile)?;
            let password = secret("Password")?;
            let client = draft_client(ctx, "login", profile, secrets)?;
            let mut given = args.mfa_code.clone();
            let session = UiSession::login(&client, &user, &password, async || match given.take() {
                Some(code) => Ok(code),
                None if ctx.interactive() => prompt::secret("MFA code"),
                None => Err(Error::Input {
                    arg: "mfa-code".into(),
                    message: "the account needs an MFA code; pass --mfa-code with the current six digits".into(),
                    choices: Vec::new(),
                }),
            })
            .await?;
            secrets.cookie = Some(session.cookie_header());
            secrets.user_token = Some(session.user_token);
        }
        MethodArg::ApiKey => {
            profile.auth = AuthKind::ApiKey;
            secrets.api_key = Some(secret("API key")?);
        }
        MethodArg::Token => {
            profile.auth = AuthKind::Token;
            profile.token_command = args.token_command.clone();
            if profile.token_command.is_none() {
                secrets.token = Some(secret("Bearer token")?);
            }
        }
        MethodArg::Cookie => {
            profile.auth = AuthKind::Cookie;
            secrets.cookie = Some(secret(
                "Cookie header (from the browser's developer tools)",
            )?);
            secrets.user_token = args.user_token.clone();
        }
        MethodArg::Certificate => {
            profile.auth = AuthKind::Certificate;
            if profile.network.client_cert.is_none() {
                return Err(Error::Input {
                    arg: "client-cert".into(),
                    message: "the certificate is missing; pass --client-cert FILE (certificate and key in one PEM file)".into(),
                    choices: Vec::new(),
                });
            }
        }
        MethodArg::Password | MethodArg::ClientCredentials | MethodArg::Jwt => {
            profile.auth = AuthKind::Oauth;
            let mut oauth = OAuthConfig {
                client_id: client_id()?,
                scope: args.scope.clone(),
                ..OAuthConfig::default()
            };
            secrets.client_secret = client_secret()?;
            match method {
                MethodArg::Password => {
                    oauth.grant = Grant::Password;
                    user(profile)?;
                    secrets.password = Some(secret("Password")?);
                }
                MethodArg::ClientCredentials => {
                    oauth.grant = Grant::ClientCredentials;
                    if secrets.client_secret.is_none() {
                        secrets.client_secret = Some(secret("Client secret")?);
                    }
                }
                _ => {
                    oauth.grant = Grant::JwtBearer;
                    if secrets.client_secret.is_none() && args.secret_stdin {
                        secrets.client_secret = Some(secret("Client secret")?);
                    }
                    let absolute =
                        |p: &PathBuf| std::path::absolute(p).unwrap_or_else(|_| p.clone());
                    oauth.jwt_key_file = args.jwt_key_file.as_ref().map(absolute);
                    oauth.jwt_subject = Some(match &args.jwt_subject {
                        Some(subject) => subject.clone(),
                        None => ctx.need("jwt-subject", "User the CLI acts as", Vec::new())?,
                    });
                    oauth.jwt_kid = args.jwt_kid.clone();
                    oauth.jwt_alg = args.jwt_alg.clone();
                    sn_core::jwt::check_key(&oauth)?;
                }
            }
            profile.oauth = Some(oauth);
        }
    }
    Ok(())
}

fn draft_client(ctx: &Ctx, name: &str, profile: &Profile, secrets: &Secrets) -> Result<Client> {
    let resolved = Resolved {
        name: name.to_owned(),
        profile: profile.clone(),
        secrets: secrets.clone(),
        ephemeral: true,
    };
    let mut options = ctx.options();
    options.dry_run = false;
    Client::new(resolved, None, options)
}

/// Asks the instance whether it knows the client before a browser opens.
async fn probe(client: &Client, config: &OAuthConfig, pending: &Pending) -> Result<()> {
    let request = Request::new(Method::GET, pending.url.clone())
        .no_auth()
        .header("accept", "text/html")
        .timeout(Duration::from_secs(20));
    match client.open(request).await {
        Ok(_) => Ok(()),
        // The sign-in page itself says what is wrong in other cases.
        Err(Error::Api(failure)) if !matches!(failure.status, Some(400 | 401)) => Ok(()),
        Err(Error::Api(_)) if config.client_id == oauth::SDK_CLIENT_ID => {
            Err(Error::Auth(format!(
                "{} does not accept the ServiceNow SDK client, so browser sign-in needs an OAuth client of its own; ask an administrator to run `sn auth register`, then pass --client-id, or use another --method (basic, api-key, token)",
                client.host()
            )))
        }
        Err(Error::Api(_)) => Err(Error::Auth(format!(
            "{} does not accept the OAuth client {} with the redirect {}; check that the client is active and its redirect URL is exactly that, or pass --port / --redirect-uri",
            client.host(),
            config.client_id,
            config.redirect_uri
        ))),
        Err(Error::Auth(_)) => Ok(()),
        Err(err) => Err(err),
    }
}

async fn browser_sign_in(
    ctx: &Ctx,
    args: &LoginArgs,
    pending: &Pending,
    callback: Option<Callback>,
) -> Result<Outcome> {
    if !waits(ctx, args) {
        return Ok(Outcome::Deferred);
    }
    let can_paste = ctx.interactive();
    if callback.is_none() && !can_paste {
        // --wait has nothing to wait for: the SDK client never calls back.
        return Ok(Outcome::Deferred);
    }
    let opened = !args.no_browser && browser::open(&pending.url);
    let how = match opened {
        true => "Your browser opens for sign-in. If it does not, open this link:",
        false => "Open this link in a browser on any computer and sign in:",
    };
    eprintln!("{how}\n\n  {}\n", pending.url);
    let question = match (callback.is_some(), can_paste) {
        (true, true) => {
            "Waiting for the browser. If it shows an error page at the end, paste its address here:"
        }
        (true, false) => "Waiting for the browser.",
        (false, _) => "Copy the code that the browser shows at the end and paste it here:",
    };
    eprintln!("{question}");
    let arrived = async {
        match &callback {
            Some(callback) => callback.wait(pending).await,
            None => std::future::pending().await,
        }
    };
    let pasted = async {
        if !can_paste {
            return std::future::pending().await;
        }
        loop {
            match read_line().await {
                Ok(line) if !line.is_empty() => match pending.parse_pasted(&line) {
                    Ok(code) => return Ok(code),
                    Err(Error::Invalid(why)) => eprintln!("{why}"),
                    Err(err) => return Err(err),
                },
                // End of input: only the callback can answer now.
                _ if callback.is_some() => return std::future::pending().await,
                _ => {
                    return Err(Error::invalid(
                        "the input ended before a code was pasted; run the command again",
                    ));
                }
            }
        }
    };
    let limit = Duration::from_secs(args.wait_timeout);
    tokio::select! {
        code = arrived => code.map(Outcome::Code),
        code = pasted => code.map(Outcome::Code),
        () = tokio::time::sleep(limit) => Err(Error::Other {
            code: "timeout",
            message: format!("no sign-in arrived in {} seconds; run the command again, or raise --wait-timeout", args.wait_timeout),
            data: Value::Null,
        }),
    }
}

/// A line from the terminal, empty at the end of the input. It is read on a
/// plain thread: an unfinished read must not keep the process alive.
async fn read_line() -> std::io::Result<String> {
    let (send, receive) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        // After the callback arrived nobody waits for the line, and a read
        // would take the keys of the workspace that signs in.
        #[cfg(unix)]
        {
            use rustix::event::{PollFd, PollFlags, Timespec, poll};
            let pause = Timespec {
                tv_sec: 0,
                tv_nsec: 100_000_000,
            };
            let stdin = std::io::stdin();
            loop {
                let ready = poll(&mut [PollFd::new(&stdin, PollFlags::IN)], Some(&pause));
                if send.is_closed() {
                    return;
                }
                if !matches!(ready, Ok(0) | Err(rustix::io::Errno::INTR)) {
                    break;
                }
            }
        }
        let mut line = String::new();
        let _ = send.send(std::io::stdin().read_line(&mut line).map(|_| line));
    });
    receive.await.map_err(std::io::Error::other)?
}

fn pending_path(ctx: &Ctx) -> PathBuf {
    ctx.store.dir().join("pending-login.json")
}

fn save_pending(ctx: &Ctx, saved: &SavedLogin) -> Result<()> {
    let _lock = ctx.store.lock()?;
    let path = pending_path(ctx);
    let failed = |err: std::io::Error| {
        Error::Internal(format!(
            "cannot write {} ({err}); check the directory's permissions",
            path.display()
        ))
    };
    // 0600: it holds the PKCE verifier.
    let mut file = tempfile::NamedTempFile::new_in(ctx.store.dir()).map_err(failed)?;
    std::io::Write::write_all(&mut file, &serde_json::to_vec(saved)?).map_err(failed)?;
    file.persist(&path).map_err(|err| failed(err.error))?;
    Ok(())
}

async fn finish_saved(ctx: &Ctx, args: &LoginArgs, code: &str) -> Result<()> {
    let path = pending_path(ctx);
    let start = "start one with `sn auth login --instance NAME`";
    let text = std::fs::read(&path)
        .map_err(|_| Error::invalid(format!("no sign-in is waiting for a code; {start}")))?;
    let saved: SavedLogin = serde_json::from_slice(&text)
        .map_err(|_| Error::invalid(format!("the waiting sign-in is not readable; {start}")))?;
    if now_unix().saturating_sub(saved.pending.created_at) > 3600 {
        let _ = std::fs::remove_file(&path);
        return Err(Error::invalid(format!(
            "the waiting sign-in is older than an hour; {start}"
        )));
    }
    if let Some(name) = ctx.globals.profile.as_ref().filter(|n| **n != saved.name) {
        return Err(Error::invalid(format!(
            "you passed --profile {name}, but the sign-in that waits for a code is of profile `{}`; pass --profile {}, or start a new one with `sn auth login --profile {name}`",
            saved.name, saved.name
        )));
    }
    let code = saved.pending.parse_pasted(code)?;
    let SavedLogin {
        name,
        profile,
        mut secrets,
        set_default,
        pending,
    } = saved;
    let config = profile.oauth.clone().unwrap_or_default();
    let client = draft_client(ctx, &name, &profile, &secrets)?;
    let tokens = oauth::exchange_code(
        &client,
        &config,
        secrets.client_secret.as_deref(),
        &pending,
        &code,
    )
    .await?;
    tokens.store(&mut secrets);
    let _ = std::fs::remove_file(&path);
    finish(ctx, args, &name, profile, secrets, set_default).await
}

/// Verifies the credential, then saves the profile.
async fn finish(
    ctx: &Ctx,
    args: &LoginArgs,
    name: &str,
    mut profile: Profile,
    mut secrets: Secrets,
    set_default: bool,
) -> Result<()> {
    let mut user = Value::Null;
    if !args.no_verify {
        let client = draft_client(ctx, name, &profile, &secrets)?;
        if let AuthKind::Oauth = profile.auth
            && secrets.access_token.is_none()
        {
            // Headless grants: the first token proves the settings.
            client.bearer(false).await?;
        }
        user = match refs::me(&client).await {
            Ok(user) => user,
            Err(err) if matches!(err.code(), "auth" | "network" | "timeout") => return Err(err),
            // Signed in, but the user may not read about itself.
            Err(_) => Value::Null,
        };
        secrets = client.into_secrets();
    }
    let user_name = user["user_name"].as_str().unwrap_or_default().to_owned();
    if profile.username.is_none() && !user_name.is_empty() {
        profile.username = Some(user_name.clone());
    }
    if ctx.globals.dry_run {
        return ctx.out.done(
            json!({ "dry_run": true, "profile": name, "instance": profile.instance, "user": user_name }),
            "The sign-in works. Nothing was saved (--dry-run).",
        );
    }
    let _lock = ctx.store.lock()?;
    let mut file = ctx.store.load()?;
    let stored_in = ctx.store.save_secrets(name, &profile, &secrets)?;
    if profile.store == Some(StoreKind::Keychain) && stored_in == StoreKind::File {
        ctx.out.warn("the credential store of the operating system did not take the secret; it is in a file only you can read");
    }
    let auth = profile.auth.name();
    let instance = profile.instance.clone();
    let sdk_client = profile
        .oauth
        .as_ref()
        .is_some_and(|o| o.client_id == oauth::SDK_CLIENT_ID);
    file.profiles.insert(name.to_owned(), profile);
    if set_default {
        file.default_profile = Some(name.to_owned());
    }
    let is_default = file.default_profile.as_deref() == Some(name);
    ctx.store.save(&file)?;
    // A sign-in of this profile that still waits for a code is over.
    let path = pending_path(ctx);
    let waiting = std::fs::read(&path).ok();
    if waiting
        .and_then(|text| serde_json::from_slice::<SavedLogin>(&text).ok())
        .is_some_and(|saved| saved.name == name)
    {
        let _ = std::fs::remove_file(&path);
    }
    let data = json!({
        "status": "signed_in", "profile": name, "instance": instance, "method": auth, "user": user,
        "default": is_default, "verified": !args.no_verify, "stored_in": stored_in,
    });
    let who = match (
        user["name"].as_str().unwrap_or_default(),
        user_name.as_str(),
    ) {
        ("", "") => String::new(),
        ("", user) => format!(" as {user}"),
        (full, user) => format!(" as {full} ({user})"),
    };
    let mut text = format!(
        "Signed in to {instance}{who}. Profile: {name}{}.",
        if is_default { " (default)" } else { "" }
    );
    if !is_default {
        text.push_str(&format!(
            "\nUse it with --profile {name}, or make it the default: sn auth switch {name}"
        ));
    }
    if sdk_client {
        text.push_str("\nTip: `sn auth register` (administrator) creates an OAuth client, so that nobody has to paste a code.");
    }
    ctx.out.done(data, &text)
}

async fn logout(ctx: &Ctx, local: bool) -> Result<()> {
    let resolved = ctx.resolve()?;
    if resolved.ephemeral {
        return Err(Error::invalid(
            "the credential comes from SN_* variables; unset them to sign out",
        ));
    }
    let mut revoked = false;
    if !local && resolved.profile.auth == AuthKind::Oauth {
        let client = draft_client(ctx, &resolved.name, &resolved.profile, &resolved.secrets)?;
        for token in [
            &resolved.secrets.refresh_token,
            &resolved.secrets.access_token,
        ]
        .into_iter()
        .flatten()
        {
            revoked |= oauth::revoke(&client, token).await;
        }
    }
    let _lock = ctx.store.lock()?;
    ctx.store
        .save_secrets(&resolved.name, &resolved.profile, &Secrets::default())?;
    ctx.out.done(
        json!({ "profile": resolved.name, "signed_out": true, "revoked": revoked }),
        &format!(
            "Signed out of profile {}. Its settings are kept.",
            resolved.name
        ),
    )
}

async fn status(ctx: &Ctx, offline: bool) -> Result<()> {
    let resolved = ctx.resolve()?;
    let secrets = &resolved.secrets;
    let mut data = json!({
        "profile": resolved.name,
        "instance": resolved.profile.instance,
        "method": resolved.profile.auth.name(),
        "username": resolved.profile.username,
        "read_only": resolved.profile.read_only,
        "from_environment": resolved.ephemeral,
        "credential": super::profile::has_credential(&resolved.profile, secrets),
        "stored_in": resolved.profile.store_kind(),
    });
    if let Some(oauth) = &resolved.profile.oauth {
        data["client_id"] = oauth.client_id.clone().into();
        data["grant"] = serde_json::to_value(oauth.grant)?;
        data["refresh_token"] = secrets.refresh_token.is_some().into();
    }
    if let Some(at) = secrets.expires_at {
        data["token_expires_in"] = (at as i64 - now_unix() as i64).into();
    }
    if !offline {
        let user = refs::me(ctx.client().await?).await?;
        data["signed_in"] = true.into();
        data["user"] = user;
    }
    ctx.out.item(data)
}

async fn refresh(ctx: &Ctx) -> Result<()> {
    let client = ctx.client().await?;
    if client.bearer(true).await?.is_none() {
        return Err(Error::invalid(format!(
            "profile `{}` signs in with {}, which has no token to refresh; run `sn auth login` to replace the credential",
            client.profile_name(),
            client.auth_kind().name()
        )));
    }
    ctx.out.done(
        json!({ "profile": client.profile_name(), "refreshed": true }),
        &format!("Profile {} has a new access token.", client.profile_name()),
    )
}

async fn token(ctx: &Ctx, form: TokenFormat) -> Result<()> {
    let client = ctx.client().await?;
    let headers = client.auth_headers().await?;
    let get = |name: &str| {
        headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    };
    let bearer = get("authorization").and_then(|v| v.strip_prefix("Bearer "));
    let text = match form {
        TokenFormat::Headers => headers
            .iter()
            .map(|(n, v)| format!("{n}: {v}"))
            .collect::<Vec<_>>()
            .join("\n"),
        TokenFormat::Bearer => bearer
            .ok_or_else(|| {
                Error::invalid(format!(
                    "profile `{}` signs in with {}, which has no bearer token; use --as headers",
                    client.profile_name(),
                    client.auth_kind().name()
                ))
            })?
            .to_owned(),
        TokenFormat::Env => super::sdk::session_env(client)
            .await?
            .iter()
            .map(|(n, v)| format!("export {n}='{}'", v.replace('\'', "'\\''")))
            .collect::<Vec<_>>()
            .join("\n"),
    };
    ctx.out.text(&text)
}

async fn register(ctx: &Ctx, args: RegisterArgs) -> Result<()> {
    let urls: Vec<String> = args
        .ports
        .iter()
        .flat_map(|p| {
            ["localhost", "127.0.0.1"]
                .map(|host| format!("http://{host}:{p}{}", oauth::CALLBACK_PATH))
        })
        .collect();
    let redirect = urls.join(",");
    let steps = format!(
        "On the instance, open System OAuth > Application Registry > New > Create an OAuth API endpoint for external clients:\n  Name          {}\n  Redirect URL  {redirect}\n  Public client checked\n  PKCE required checked\nSave it, copy the Client ID, and give it to your users:\n  sn auth login --instance NAME --client-id CLIENT_ID",
        args.name
    );
    if args.print {
        return ctx.out.done(json!({ "name": args.name, "redirect_url": redirect, "public_client": true, "steps": steps }), &steps);
    }
    let client = ctx.client().await?;
    ctx.confirm(&format!(
        "create the OAuth client \"{}\" on {}",
        args.name,
        client.host()
    ))?;
    let body = json!({
        "name": args.name,
        "type": "client",
        "active": "true",
        "public_client": "true",
        "redirect_url": redirect,
        "default_grant_type": "authorization_code",
        "comments": "Browser sign-in of the sn command-line tool (authorization code with PKCE).",
    });
    let body = body.as_object().cloned().unwrap_or_default();
    let params = sn_core::TableQuery::default()
        .fields(&[
            "sys_id",
            "name",
            "client_id",
            "redirect_url",
            "public_client",
        ])
        .display(sn_core::DisplayValue::False);
    let created = client.create("oauth_entity", body, &params).await.map_err(|err| match err.code() {
        "permission" | "not_found" => Error::Auth(format!(
            "your user may not create OAuth clients (role oauth_admin or admin); give an administrator these steps.\n{steps}"
        )),
        _ => err,
    })?;
    let id = created["client_id"].as_str().unwrap_or_default().to_owned();
    if created["public_client"].as_str() != Some("true") {
        ctx.out.warn("the instance did not mark the client as public; check \"Public client\" on its form, or sign-in will ask for a client secret");
    }
    let host = client.host();
    ctx.out.done(
        created,
        &format!("Created the OAuth client. Your users sign in with:\n  sn auth login --instance {host} --client-id {id}"),
    )
}

/// The choice list of `auth switch`.
pub fn profile_choices(file: &config::ConfigFile) -> Vec<Choice> {
    file.profiles
        .iter()
        .map(|(name, p)| Choice::new(name, format!("{name}  {}", p.instance)))
        .collect()
}

impl LoginArgs {
    /// `auth login` without a flag, for other commands that sign in.
    pub fn defaults() -> Result<Self> {
        let command = <Self as Args>::augment_args(clap::Command::new("login"));
        let matches = command
            .try_get_matches_from(["login"])
            .map_err(|err| Error::Internal(err.to_string()))?;
        <Self as clap::FromArgMatches>::from_arg_matches(&matches)
            .map_err(|err| Error::Internal(err.to_string()))
    }
}

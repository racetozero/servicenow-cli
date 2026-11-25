//! OAuth 2.0: PKCE, the loopback callback, and the token endpoint.

use crate::client::Client;
use crate::config::{OAuthConfig, Secrets};
use crate::error::{ApiFailure, Error, Result};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use url::Url;

/// The public client of the ServiceNow SDK, which instances ship. Its only
/// registered redirect is [`SDK_REDIRECT`].
pub const SDK_CLIENT_ID: &str = "543e5655f77746a28228c6009a599dfb";
/// A page on the instance that shows the code for the user to copy.
pub const SDK_REDIRECT: &str = "/sdk-oauth.do";
pub const DEFAULT_PORT: u16 = 8400;
pub const CALLBACK_PATH: &str = "/callback";
const AUTH_PATH: &str = "/oauth_auth.do";
const TOKEN_PATH: &str = "/oauth_token.do";
const REVOKE_PATH: &str = "/oauth_revoke.do";

/// What the token endpoint returned.
pub struct Tokens {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: Option<u64>,
}

impl Tokens {
    pub fn store(self, secrets: &mut Secrets) {
        secrets.access_token = Some(self.access_token);
        // The instance may keep the old refresh token valid and send none.
        if self.refresh_token.is_some() {
            secrets.refresh_token = self.refresh_token;
        }
        secrets.expires_at = self.expires_at;
    }
}

pub fn random_urlsafe(bytes: usize) -> Result<String> {
    let mut buffer = vec![0u8; bytes];
    getrandom::fill(&mut buffer)
        .map_err(|err| Error::Internal(format!("the system gave no random numbers ({err})")))?;
    Ok(URL_SAFE_NO_PAD.encode(buffer))
}

/// The S256 code challenge of a PKCE verifier.
pub fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// One sign-in between opening the browser and exchanging the code.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Pending {
    pub instance: String,
    pub client_id: String,
    pub redirect_uri: String,
    pub state: String,
    pub verifier: String,
    pub url: String,
    pub created_at: u64,
}

impl Pending {
    pub fn new(instance: &str, config: &OAuthConfig) -> Result<Self> {
        let state = random_urlsafe(24)?;
        let verifier = random_urlsafe(32)?;
        let mut url = Url::parse(&format!("{instance}{AUTH_PATH}")).map_err(|err| {
            Error::invalid(format!(
                "`{instance}{AUTH_PATH}` is not a URL ({err}); check the instance"
            ))
        })?;
        {
            let mut query = url.query_pairs_mut();
            query
                .append_pair("response_type", "code")
                .append_pair("client_id", &config.client_id)
                .append_pair("redirect_uri", &config.redirect_uri)
                .append_pair("state", &state);
            if !config.no_pkce {
                query
                    .append_pair("code_challenge", &challenge(&verifier))
                    .append_pair("code_challenge_method", "S256");
            }
            if let Some(scope) = &config.scope {
                query.append_pair("scope", scope);
            }
        }
        Ok(Self {
            instance: instance.to_owned(),
            client_id: config.client_id.clone(),
            redirect_uri: config.redirect_uri.clone(),
            state,
            verifier,
            url: url.into(),
            created_at: crate::now_unix(),
        })
    }

    /// The code in what the user pasted: the bare code, the redirect URL, or
    /// its query string.
    pub fn parse_pasted(&self, pasted: &str) -> Result<String> {
        let text = pasted.trim();
        let query = if let Ok(url) = Url::parse(text) {
            url.query().unwrap_or_default().to_owned()
        } else if text.contains("code=") {
            text.trim_start_matches(['?', '&']).to_owned()
        } else if !text.is_empty() && !text.contains(char::is_whitespace) {
            return Ok(text.to_owned());
        } else {
            return Err(Error::invalid(
                "what you pasted is not a sign-in code; paste the code that the browser shows, or the whole address of the page it ended on",
            ));
        };
        self.parse_query(&query)
    }

    fn parse_query(&self, query: &str) -> Result<String> {
        let (mut code, mut state, mut error, mut description) = (None, None, None, None);
        for (name, value) in url::form_urlencoded::parse(query.as_bytes()) {
            match name.as_ref() {
                "code" => code = Some(value.into_owned()),
                "state" => state = Some(value.into_owned()),
                "error" => error = Some(value.into_owned()),
                "error_description" => description = Some(value.into_owned()),
                _ => {}
            }
        }
        if let Some(error) = error {
            let description = description.map(|d| format!(" ({d})")).unwrap_or_default();
            return Err(Error::Auth(format!(
                "the instance refused the sign-in: {error}{description}; check that the OAuth client is active and allows {}",
                self.redirect_uri
            )));
        }
        if state.is_some_and(|s| s != self.state) {
            return Err(Error::Auth(
                "the answer belongs to another sign-in (its state does not match); use the newest link and try again".into(),
            ));
        }
        code.filter(|c| !c.is_empty()).ok_or_else(|| {
            Error::Auth(
                "the answer has no code; sign in again and paste the address of the last page"
                    .into(),
            )
        })
    }

    /// Reads the code out of a callback request line.
    fn parse_request_line(&self, line: &str) -> Inbound {
        let target = line.split_whitespace().nth(1).unwrap_or_default();
        let (path, query) = target.split_once('?').unwrap_or((target, ""));
        let expected = Url::parse(&self.redirect_uri).ok();
        if expected.is_none_or(|url| url.path() != path) {
            return Inbound::OtherPath;
        }
        // A callback always carries our state; a page that guesses does not.
        let ours = url::form_urlencoded::parse(query.as_bytes())
            .any(|(n, v)| n == "state" && v == self.state);
        match ours {
            true => Inbound::Callback(self.parse_query(query)),
            false => Inbound::WrongState,
        }
    }
}

/// One request that reached the callback listener.
enum Inbound {
    OtherPath,
    /// The callback path without the state of this sign-in: not ours.
    WrongState,
    Callback(Result<String>),
}

/// The port of a redirect URI that points at this computer.
pub fn loopback_port(redirect_uri: &str) -> Option<u16> {
    let url = Url::parse(redirect_uri).ok()?;
    let local = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    (url.scheme() == "http" && local).then(|| url.port_or_known_default())?
}

/// Listens for the callback on IPv4 and, when available, IPv6 loopback:
/// browsers resolve `localhost` to either.
pub struct Callback {
    v4: TcpListener,
    v6: Option<TcpListener>,
}

impl Callback {
    pub async fn bind(port: u16) -> Result<Self> {
        let v4 = TcpListener::bind(("127.0.0.1", port)).await.map_err(|err| {
            Error::invalid(format!(
                "cannot listen on 127.0.0.1:{port} for the sign-in callback ({err}); close the program that uses the port, or pass --port N with a port that the OAuth client's redirect URL also has"
            ))
        })?;
        let port = v4.local_addr().map(|a| a.port()).unwrap_or(port);
        let v6 = TcpListener::bind(("::1", port)).await.ok();
        Ok(Self { v4, v6 })
    }

    pub fn port(&self) -> u16 {
        self.v4.local_addr().map(|a| a.port()).unwrap_or_default()
    }

    /// Serves until the sign-in of `pending` arrives.
    pub async fn wait(&self, pending: &Pending) -> Result<String> {
        loop {
            let v6 = async {
                match &self.v6 {
                    Some(listener) => listener.accept().await,
                    None => std::future::pending().await,
                }
            };
            let accepted = tokio::select! {
                accepted = self.v4.accept() => accepted,
                accepted = v6 => accepted,
            };
            let Ok((socket, _)) = accepted else { continue };
            // A browser opens spare connections that never send a request.
            let served = tokio::time::timeout(Duration::from_secs(5), serve(socket, pending)).await;
            if let Ok(Some(result)) = served {
                return result;
            }
        }
    }
}

async fn serve(mut socket: tokio::net::TcpStream, pending: &Pending) -> Option<Result<String>> {
    let mut buffer = vec![0u8; 16 * 1024];
    let mut read = 0;
    while read < buffer.len() && !buffer[..read].windows(4).any(|w| w == b"\r\n\r\n") {
        match socket.read(&mut buffer[read..]).await {
            Ok(0) | Err(_) => break,
            Ok(n) => read += n,
        }
    }
    let head = String::from_utf8_lossy(&buffer[..read]);
    let line = head.lines().next().unwrap_or_default();
    let inbound = pending.parse_request_line(line);
    let (status, title, text) = match &inbound {
        Inbound::OtherPath => ("404 Not Found", "Not found", String::new()),
        Inbound::WrongState => (
            "400 Bad Request",
            "This is not the current sign-in",
            "Use the newest link from the terminal.".to_owned(),
        ),
        Inbound::Callback(Ok(_)) => (
            "200 OK",
            "You are signed in",
            "You can close this tab and return to the terminal.".to_owned(),
        ),
        Inbound::Callback(Err(err)) => ("400 Bad Request", "Sign-in did not work", err.to_string()),
    };
    let text = text.replace('&', "&amp;").replace('<', "&lt;");
    let page = format!(
        "<!doctype html><meta charset=utf-8><meta name=viewport content=\"width=device-width\"><title>sn</title>\
         <style>body{{font:16px/1.5 system-ui,sans-serif;display:grid;place-items:center;min-height:90vh;margin:0;color-scheme:light dark}}main{{max-width:30rem;padding:2rem}}</style>\
         <main><h1>{title}</h1><p>{text}</p></main>"
    );
    let answer = format!(
        "HTTP/1.1 {status}\r\ncontent-type: text/html; charset=utf-8\r\ncontent-length: {}\r\ncache-control: no-store\r\nreferrer-policy: no-referrer\r\nconnection: close\r\n\r\n{page}",
        page.len()
    );
    let _ = socket.write_all(answer.as_bytes()).await;
    let _ = socket.shutdown().await;
    match inbound {
        Inbound::Callback(result) => Some(result),
        Inbound::OtherPath | Inbound::WrongState => None,
    }
}

async fn token_request(
    client: &Client,
    config: &OAuthConfig,
    secret: Option<&str>,
    fields: &[(&str, &str)],
) -> Result<Tokens> {
    let mut form: Vec<(String, String)> = fields
        .iter()
        .map(|(n, v)| ((*n).to_owned(), (*v).to_owned()))
        .collect();
    form.push(("client_id".into(), config.client_id.clone()));
    if let Some(secret) = secret {
        form.push(("client_secret".into(), secret.to_owned()));
    }
    if let Some(scope) = config
        .scope
        .as_deref()
        .filter(|_| fields[0].1 != "refresh_token")
    {
        form.push(("scope".into(), scope.to_owned()));
    }
    let response = client.post_form_plain(TOKEN_PATH, &form, "").await?;
    let body: Value = serde_json::from_slice(&response.body).unwrap_or(Value::Null);
    if response.status >= 400 || body.get("access_token").is_none() {
        let error = body["error"]
            .as_str()
            .unwrap_or("no access token in the answer");
        let description = body["error_description"].as_str().unwrap_or("");
        let hint = match (error, description) {
            ("invalid_client", _) => {
                "the client id or secret is wrong, or the client is not active on this instance"
            }
            ("invalid_grant", _) => {
                "the code or token is used, expired, or belongs to another client; sign in again"
            }
            (_, "access_denied") => "the instance does not allow this grant for the client or user",
            _ => "check the OAuth client in System OAuth > Application Registry",
        };
        return Err(Error::Api(Box::new(ApiFailure {
            status: Some(if response.status >= 400 {
                response.status
            } else {
                401
            }),
            message: format!(
                "the instance refused the token request: {error} {description}; {hint}"
            ),
            payload: body,
            ..ApiFailure::default()
        })));
    }
    Ok(parse_tokens(&body))
}

fn parse_tokens(body: &Value) -> Tokens {
    let number = |v: &Value| v.as_u64().or_else(|| v.as_str()?.trim().parse().ok());
    let text = |names: &[&str]| {
        names
            .iter()
            .find_map(|n| body.get(*n)?.as_str())
            .map(|t| t.trim().to_owned())
            .filter(|t| !t.is_empty())
    };
    let absolute = ["expires_at", "expires_on", "expiresAt"]
        .iter()
        .find_map(|n| body.get(*n).and_then(number))
        // Some tools state milliseconds.
        .map(|at| if at > 100_000_000_000 { at / 1000 } else { at });
    Tokens {
        access_token: text(&["access_token", "accessToken", "token"]).unwrap_or_default(),
        refresh_token: text(&["refresh_token"]),
        expires_at: absolute.or_else(|| {
            let seconds = body.get("expires_in").and_then(number)?;
            Some(crate::now_unix().saturating_add(seconds))
        }),
    }
}

pub async fn exchange_code(
    client: &Client,
    config: &OAuthConfig,
    secret: Option<&str>,
    pending: &Pending,
    code: &str,
) -> Result<Tokens> {
    let mut fields = vec![
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", pending.redirect_uri.as_str()),
    ];
    if !config.no_pkce {
        fields.push(("code_verifier", pending.verifier.as_str()));
    }
    token_request(client, config, secret, &fields).await
}

pub async fn refresh(
    client: &Client,
    config: &OAuthConfig,
    secret: Option<&str>,
    token: &str,
) -> Result<Tokens> {
    token_request(
        client,
        config,
        secret,
        &[("grant_type", "refresh_token"), ("refresh_token", token)],
    )
    .await
}

pub async fn client_credentials(
    client: &Client,
    config: &OAuthConfig,
    secret: Option<&str>,
) -> Result<Tokens> {
    token_request(
        client,
        config,
        secret,
        &[("grant_type", "client_credentials")],
    )
    .await
}

pub async fn password(
    client: &Client,
    config: &OAuthConfig,
    secret: Option<&str>,
    user: &str,
    password: &str,
) -> Result<Tokens> {
    let fields = [
        ("grant_type", "password"),
        ("username", user),
        ("password", password),
    ];
    token_request(client, config, secret, &fields).await
}

pub async fn jwt_bearer(
    client: &Client,
    config: &OAuthConfig,
    secret: Option<&str>,
) -> Result<Tokens> {
    let assertion = crate::jwt::assertion(config)?;
    let fields = [
        ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
        ("assertion", assertion.as_str()),
    ];
    token_request(client, config, secret, &fields).await
}

/// Asks the instance to forget a token. Best effort.
pub async fn revoke(client: &Client, token: &str) -> bool {
    let request = crate::client::Request::new(crate::client::Method::GET, REVOKE_PATH)
        .query("token", token)
        .no_auth();
    client.send(request).await.is_ok_and(|r| r.status < 300)
}

/// Runs the profile's token command. Its output is the bare token, or JSON
/// with the token and optionally its expiry. The output is never shown.
pub async fn run_token_command(command: &str, profile: &str, instance: &str) -> Result<Tokens> {
    let (shell, flag) = if cfg!(windows) {
        ("cmd", "/C")
    } else {
        ("sh", "-c")
    };
    let failed = |why: String| {
        Error::Auth(format!(
            "the token command of profile `{profile}` {why}; run it yourself to check that it prints a token"
        ))
    };
    let child = tokio::process::Command::new(shell)
        .arg(flag)
        .arg(command)
        .env("SN_PROFILE", profile)
        .env("SN_INSTANCE", instance)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output();
    let output = tokio::time::timeout(Duration::from_secs(60), child)
        .await
        .map_err(|_| failed("did not finish in 60 seconds".into()))?
        .map_err(|err| failed(format!("did not start ({err})")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let tail = stderr.trim().lines().last().unwrap_or_default();
        return Err(failed(format!("failed ({}): {tail}", output.status)));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let text = text.trim();
    let tokens = match serde_json::from_str::<Value>(text) {
        Ok(Value::Object(map)) => parse_tokens(&Value::Object(map)),
        _ => Tokens {
            access_token: text.to_owned(),
            refresh_token: None,
            expires_at: None,
        },
    };
    if tokens.access_token.is_empty() || tokens.access_token.contains(char::is_whitespace) {
        return Err(failed(
            "printed no token (expected one line, or JSON with access_token)".into(),
        ));
    }
    Ok(tokens)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending() -> Pending {
        let config = OAuthConfig {
            client_id: "cid".into(),
            redirect_uri: "http://localhost:8400/callback".into(),
            ..OAuthConfig::default()
        };
        Pending::new("https://acme.service-now.com", &config).expect("pending")
    }

    #[test]
    fn challenge_matches_rfc_7636() {
        assert_eq!(
            challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn pasted_codes_urls_and_errors() {
        let p = pending();
        assert_eq!(p.parse_pasted(" abc-1 \n").expect("bare"), "abc-1");
        let url = format!(
            "http://localhost:8400/callback?code=xy%2Bz&state={}",
            p.state
        );
        assert_eq!(p.parse_pasted(&url).expect("url"), "xy+z");
        assert!(
            p.parse_pasted("http://localhost:8400/callback?code=x&state=evil")
                .is_err()
        );
        assert!(
            p.parse_pasted("http://localhost:8400/callback?error=access_denied")
                .is_err()
        );
        assert!(p.parse_pasted("").is_err());
    }

    #[test]
    fn callback_lines_need_path_and_state() {
        let p = pending();
        assert!(matches!(
            p.parse_request_line("GET /favicon.ico HTTP/1.1"),
            Inbound::OtherPath
        ));
        let good = format!("GET /callback?code=1&state={} HTTP/1.1", p.state);
        assert!(matches!(p.parse_request_line(&good), Inbound::Callback(Ok(code)) if code == "1"));
        assert!(matches!(
            p.parse_request_line("GET /callback?code=1 HTTP/1.1"),
            Inbound::WrongState
        ));
    }

    #[test]
    fn loopback_redirects() {
        assert_eq!(loopback_port("http://localhost:8400/callback"), Some(8400));
        assert_eq!(loopback_port("http://127.0.0.1:9/x"), Some(9));
        assert_eq!(loopback_port("/sdk-oauth.do"), None);
        assert_eq!(loopback_port("https://localhost:1/x"), None);
    }

    #[test]
    fn token_command_output_states_expiry() {
        let tokens =
            parse_tokens(&serde_json::json!({ "accessToken": "t", "expires_on": "4102444800" }));
        assert_eq!(
            (tokens.access_token.as_str(), tokens.expires_at),
            ("t", Some(4_102_444_800))
        );
        let tokens =
            parse_tokens(&serde_json::json!({ "token": "t", "expiresAt": 4_102_444_800_000_u64 }));
        assert_eq!(tokens.expires_at, Some(4_102_444_800));
    }
}

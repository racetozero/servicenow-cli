//! The HTTP client: authentication, retry, read-only guard and dry run.

use crate::config::{AuthKind, Grant, Network, OAuthConfig, Profile, Resolved, Secrets, Store};
use crate::error::{ApiFailure, Error, Result};
use crate::oauth;
use crate::uisession::UiSession;
use bytes::Bytes;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::Mutex;

pub use reqwest::Method;

const USER_AGENT: &str = concat!("sn/", env!("CARGO_PKG_VERSION"));
const TIMEOUT: Duration = Duration::from_secs(60);
/// A token is renewed this many seconds before it expires.
const EXPIRY_MARGIN: u64 = 60;
const MAX_RETRIES: u32 = 3;
const MAX_WAIT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Default)]
pub enum Body {
    #[default]
    None,
    Json(Value),
    Form(Vec<(String, String)>),
    Bytes {
        data: Bytes,
        content_type: String,
    },
    /// Streamed from disk, so a retry can open it again.
    File {
        path: PathBuf,
        content_type: String,
    },
    /// `multipart/form-data`: text fields, then one file.
    Multipart {
        fields: Vec<(String, String)>,
        file_field: String,
        file_name: String,
        data: Bytes,
        content_type: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum AuthMode {
    #[default]
    Profile,
    None,
}

/// One HTTP request.
#[derive(Debug, Clone)]
pub struct Request {
    pub method: Method,
    /// Path and optional query relative to the instance, or an absolute URL
    /// on the instance.
    pub path: String,
    pub query: Vec<(String, String)>,
    pub headers: Vec<(String, String)>,
    pub body: Body,
    pub timeout: Option<Duration>,
    reads: bool,
    writes: bool,
    always_dry: bool,
    auth: AuthMode,
}

impl Request {
    pub fn new(method: Method, path: impl Into<String>) -> Self {
        Self {
            method,
            path: path.into(),
            query: Vec::new(),
            headers: Vec::new(),
            body: Body::None,
            timeout: None,
            reads: false,
            writes: false,
            always_dry: false,
            auth: AuthMode::Profile,
        }
    }

    pub fn query(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.query.push((name.into(), value.into()));
        self
    }

    /// Adds the parameter when there is a value.
    pub fn query_opt(self, name: &str, value: Option<impl Into<String>>) -> Self {
        match value {
            Some(value) => self.query(name, value),
            None => self,
        }
    }

    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    pub fn json(mut self, body: Value) -> Self {
        self.body = Body::Json(body);
        self
    }

    pub fn form(mut self, fields: Vec<(String, String)>) -> Self {
        self.body = Body::Form(fields);
        self
    }

    pub fn body(mut self, body: Body) -> Self {
        self.body = body;
        self
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Marks a POST that only reads (a query sent as a body).
    pub fn reads(mut self) -> Self {
        self.reads = true;
        self
    }

    /// Marks a GET that changes something.
    pub fn writes(mut self) -> Self {
        self.writes = true;
        self
    }

    /// With `--dry-run`, shows this request even when it only reads.
    pub fn always_dry(mut self) -> Self {
        self.always_dry = true;
        self
    }

    /// Sends no credential of the profile.
    pub fn no_auth(mut self) -> Self {
        self.auth = AuthMode::None;
        self
    }

    fn is_read(&self) -> bool {
        let reading = matches!(self.method, Method::GET | Method::HEAD | Method::OPTIONS);
        self.reads || (reading && !self.writes)
    }
}

#[derive(Debug, Clone)]
pub struct Response {
    pub status: u16,
    pub headers: HeaderMap,
    pub body: Bytes,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// The parsed body; an empty body is `null`.
    pub fn json(&self) -> Result<Value> {
        if self.body.iter().all(u8::is_ascii_whitespace) {
            return Ok(Value::Null);
        }
        serde_json::from_slice(&self.body).map_err(|_| not_json(&self.body))
    }

    /// The body without ServiceNow's `{"result": ...}` envelope.
    pub fn result(&self) -> Result<Value> {
        self.json().map(without_envelope)
    }

    /// `X-Total-Count`, the number of matching records.
    pub fn total(&self) -> Option<u64> {
        self.header("x-total-count")?.parse().ok()
    }

    /// All `Set-Cookie` pairs.
    pub fn cookies(&self) -> Vec<(String, String)> {
        set_cookies(&self.headers)
    }
}

/// An answer without its `{"result": ...}` envelope, when it has one.
pub fn without_envelope(answer: Value) -> Value {
    match answer {
        Value::Object(mut map) if map.contains_key("result") => {
            map.remove("result").unwrap_or(Value::Null)
        }
        other => other,
    }
}

fn set_cookies(headers: &HeaderMap) -> Vec<(String, String)> {
    headers
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .filter_map(|v| v.split(';').next()?.split_once('='))
        .map(|(name, value)| (name.trim().to_owned(), value.trim().to_owned()))
        .collect()
}

/// `a=1; b=2` as pairs.
pub(crate) fn parse_cookie_header(text: &str) -> BTreeMap<String, String> {
    text.trim()
        .trim_prefix("Cookie:")
        .split(';')
        .filter_map(|pair| pair.split_once('='))
        .map(|(name, value)| (name.trim().to_owned(), value.trim().to_owned()))
        .collect()
}

pub(crate) fn cookie_header(cookies: &BTreeMap<String, String>) -> String {
    cookies
        .iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>()
        .join("; ")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum Debug {
    #[default]
    Off,
    /// Method, URL, status.
    Requests,
    /// Headers and bodies too, secrets masked.
    Full,
}

/// Per-run settings.
#[derive(Debug, Clone, Default)]
pub struct Options {
    pub dry_run: bool,
    pub read_only: bool,
    pub debug: Debug,
    pub timeout: Option<Duration>,
}

struct AuthState {
    profile: Profile,
    secrets: Secrets,
    cookies: BTreeMap<String, String>,
    /// Cookies the server rotated, to write back after success.
    cookies_changed: bool,
    /// A token from `token_command` that states no expiry lives one run.
    run_token: Option<String>,
}

pub struct Client {
    http: reqwest::Client,
    /// Follows no redirect; sign-in forms answer with one.
    plain: std::sync::OnceLock<reqwest::Client>,
    base: String,
    name: String,
    store: Option<Store>,
    auth_kind: AuthKind,
    state: Mutex<AuthState>,
    /// What `plain` is built from: the secrets are only the proxy's.
    network: (Network, Secrets),
    options: Options,
}

impl Client {
    /// `store` is where renewed tokens go; `None` keeps them in memory.
    pub fn new(resolved: Resolved, store: Option<Store>, options: Options) -> Result<Self> {
        let timeout = options.timeout.unwrap_or(TIMEOUT);
        let http = build_http(&resolved.profile.network, &resolved.secrets, timeout, true)?;
        let cookies = resolved
            .secrets
            .cookie
            .as_deref()
            .map(parse_cookie_header)
            .unwrap_or_default();
        let mut options = options;
        options.read_only |= resolved.profile.read_only;
        Ok(Self {
            http,
            plain: std::sync::OnceLock::new(),
            base: resolved.profile.instance.clone(),
            name: resolved.name,
            store: store.filter(|_| !resolved.ephemeral),
            auth_kind: resolved.profile.auth,
            network: (
                resolved.profile.network.clone(),
                Secrets {
                    proxy_user: resolved.secrets.proxy_user.clone(),
                    proxy_password: resolved.secrets.proxy_password.clone(),
                    ..Secrets::default()
                },
            ),
            state: Mutex::new(AuthState {
                profile: resolved.profile,
                secrets: resolved.secrets,
                cookies,
                cookies_changed: false,
                run_token: None,
            }),
            options,
        })
    }

    /// A client of one session of the web interface. It shares this client's
    /// connections, and nothing of the session is written to the profile.
    pub async fn for_session(&self, session: &UiSession, options: Options) -> Self {
        let cookie = session.cookie_header();
        Self {
            http: self.http.clone(),
            plain: std::sync::OnceLock::new(),
            base: self.base.clone(),
            name: self.name.clone(),
            store: None,
            auth_kind: AuthKind::Cookie,
            network: self.network.clone(),
            state: Mutex::new(AuthState {
                profile: Profile {
                    instance: self.base.clone(),
                    auth: AuthKind::Cookie,
                    username: self.username().await,
                    ..Profile::default()
                },
                secrets: Secrets {
                    cookie: Some(cookie),
                    user_token: Some(session.user_token.clone()),
                    ..Secrets::default()
                },
                cookies: session.cookies.clone(),
                cookies_changed: false,
                run_token: None,
            }),
            options: Options {
                read_only: options.read_only || self.options.read_only,
                ..options
            },
        }
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    /// The instance host, for example `acme.service-now.com`.
    pub fn host(&self) -> &str {
        self.base.split("://").nth(1).unwrap_or(&self.base)
    }

    pub fn profile_name(&self) -> &str {
        &self.name
    }

    pub fn auth_kind(&self) -> AuthKind {
        self.auth_kind
    }

    pub fn is_read_only(&self) -> bool {
        self.options.read_only
    }

    pub fn is_dry_run(&self) -> bool {
        self.options.dry_run
    }

    pub fn debug(&self) -> Debug {
        self.options.debug
    }

    /// The profile's network settings and proxy login, for clients of other
    /// protocols.
    pub fn network(&self) -> (&Network, &Secrets) {
        (&self.network.0, &self.network.1)
    }

    pub async fn username(&self) -> Option<String> {
        self.state.lock().await.profile.username.clone()
    }

    pub async fn get(&self, path: impl Into<String>) -> Result<Value> {
        self.send(Request::new(Method::GET, path)).await?.result()
    }

    pub async fn post(&self, path: impl Into<String>, body: Value) -> Result<Value> {
        self.send(Request::new(Method::POST, path).json(body))
            .await?
            .result()
    }

    pub async fn patch(&self, path: impl Into<String>, body: Value) -> Result<Value> {
        self.send(Request::new(Method::PATCH, path).json(body))
            .await?
            .result()
    }

    pub async fn put(&self, path: impl Into<String>, body: Value) -> Result<Value> {
        self.send(Request::new(Method::PUT, path).json(body))
            .await?
            .result()
    }

    pub async fn delete(&self, path: impl Into<String>) -> Result<Value> {
        self.send(Request::new(Method::DELETE, path))
            .await?
            .result()
    }

    /// Posts a form without the profile's credential (token endpoint,
    /// sign-in forms) and returns the response whatever its status. `cookies`
    /// carry a session of the web interface; they can be empty.
    pub(crate) async fn post_form_plain(
        &self,
        path: &str,
        fields: &[(String, String)],
        cookies: &str,
    ) -> Result<Response> {
        let url = format!("{}{path}", self.base);
        if self.options.debug >= Debug::Requests {
            eprintln!("> POST {path}");
        }
        let http = match self.plain.get() {
            Some(http) => http,
            None => {
                let timeout = self.options.timeout.unwrap_or(TIMEOUT);
                let http = build_http(&self.network.0, &self.network.1, timeout, false)?;
                self.plain.get_or_init(|| http)
            }
        };
        let mut builder = http
            .post(&url)
            .header("accept", "application/json")
            .form(fields);
        if !cookies.is_empty() {
            builder = builder.header("cookie", sensitive(cookies)?);
        }
        let response = builder.send().await?;
        let status = response.status().as_u16();
        if self.options.debug >= Debug::Requests {
            eprintln!("< {status}");
        }
        let headers = response.headers().clone();
        Ok(Response {
            status,
            headers,
            body: response.bytes().await?,
        })
    }

    /// Sends a request and reads the whole body. Any status of 400 or more
    /// is an error.
    pub async fn send(&self, request: Request) -> Result<Response> {
        let response = self.open(request).await?;
        let status = response.status().as_u16();
        let headers = response.headers().clone();
        let body = response.bytes().await?;
        if self.options.debug >= Debug::Full {
            eprintln!("< {}", mask_body(&String::from_utf8_lossy(&body)));
        }
        Ok(Response {
            status,
            headers,
            body,
        })
    }

    /// Sends a request and returns the open response, for streaming.
    pub async fn open(&self, request: Request) -> Result<reqwest::Response> {
        let url = self.url(&request)?;
        if self.options.dry_run && (request.always_dry || !request.is_read()) {
            return Err(Error::DryRun(describe(&request, &url)));
        }
        if self.options.read_only && !request.is_read() {
            return Err(Error::ReadOnly(format!(
                "{} {} writes, and profile `{}` is read-only; use another --profile, or sign in again without --read-only",
                request.method, request.path, self.name
            )));
        }
        let mut attempt = 0;
        let mut rejected: Option<String> = None;
        loop {
            let http = self.build(&request, &url, rejected.as_deref()).await?;
            let bearer = http
                .headers()
                .get("authorization")
                .and_then(|v| v.to_str().ok()?.strip_prefix("Bearer "))
                .map(str::to_owned);
            if self.options.debug >= Debug::Requests {
                eprintln!("> {} {}", request.method, mask_url(&url));
            }
            let sent = self.http.execute(http).await;
            let response = match sent {
                Ok(response) => response,
                // A request that never reached the server is safe to repeat.
                Err(err)
                    if attempt < MAX_RETRIES
                        && (err.is_connect() || err.is_request())
                        && !err.is_timeout()
                        && request.is_read() =>
                {
                    attempt += 1;
                    tokio::time::sleep(backoff(attempt)).await;
                    continue;
                }
                Err(err) => return Err(err.into()),
            };
            let status = response.status().as_u16();
            if self.options.debug >= Debug::Requests {
                eprintln!("< {status}");
            }
            if self.options.debug >= Debug::Full {
                for (name, value) in response.headers() {
                    eprintln!(
                        "<   {name}: {}",
                        mask_field(name.as_str(), value.to_str().unwrap_or(""))
                    );
                }
            }
            self.adopt_cookies(&request, response.headers(), status)
                .await;
            let retry = match status {
                // The instance did not process a rate-limited request.
                429 => true,
                502..=504 => request.is_read(),
                _ => false,
            };
            if retry && attempt < MAX_RETRIES {
                attempt += 1;
                let wait = response
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse().ok())
                    .map_or_else(|| backoff(attempt), Duration::from_secs);
                if wait <= MAX_WAIT {
                    tokio::time::sleep(wait).await;
                    continue;
                }
            }
            if status == 401
                && rejected.is_none()
                && bearer.is_some()
                && request.auth == AuthMode::Profile
                && self.renewable().await
            {
                rejected = bearer;
                continue;
            }
            if status < 300 {
                return Ok(response);
            }
            if status < 400 {
                // Redirects inside the instance are followed; this one
                // leaves it, or goes to a sign-in page.
                let to = response
                    .headers()
                    .get("location")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .to_owned();
                if request.auth == AuthMode::None {
                    return Ok(response);
                }
                return Err(Error::Auth(format!(
                    "{} answered with a redirect to a sign-in page ({}); your session is not accepted, so run `sn auth login --profile {}`",
                    self.host(),
                    to.split('?').next().unwrap_or(""),
                    self.name
                )));
            }
            let headers = response.headers().clone();
            let body = response.bytes().await?;
            return Err(self.failure(&request, status, &headers, &body));
        }
    }

    fn failure(&self, request: &Request, status: u16, headers: &HeaderMap, body: &[u8]) -> Error {
        let payload: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
        let error = payload
            .get("error")
            .or_else(|| payload.pointer("/result/error"));
        let text = |value: Option<&Value>| {
            value
                .and_then(|v| match v {
                    Value::String(s) => Some(s.clone()),
                    Value::Null => None,
                    other => Some(other.to_string()),
                })
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
        };
        let message = match error {
            Some(Value::Object(map)) => text(map.get("message")),
            other => text(other),
        }
        .or_else(|| text(payload.get("error_description")))
        .or_else(|| text(payload.get("message")))
        .or_else(|| text(payload.pointer("/result/message")))
        .unwrap_or_else(|| {
            if looks_like_html(body) {
                "the instance answered with a web page, not with data".to_owned()
            } else {
                let body = String::from_utf8_lossy(body);
                let body = body.trim();
                match body.is_empty() {
                    true => "the instance gave no reason".to_owned(),
                    false => body.chars().take(300).collect(),
                }
            }
        });
        let detail = error
            .and_then(|e| text(e.get("detail")))
            .or_else(|| text(payload.get("error_description")))
            .filter(|d| *d != message);
        let transaction_id = headers
            .get("x-transaction-id")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let hint = match status {
            401 => format!(
                "; the instance rejected the credential of profile `{}`, so run `sn auth login --profile {}`",
                self.name, self.name
            ),
            403 => "; your user needs a role or an access rule (ACL) for this".to_owned(),
            _ => String::new(),
        };
        Error::Api(Box::new(ApiFailure {
            status: Some(status),
            message: format!(
                "{} {} failed with HTTP {status}: {message}{hint}",
                request.method, request.path
            ),
            detail,
            payload,
            transaction_id,
        }))
    }

    fn url(&self, request: &Request) -> Result<url::Url> {
        let text = if request.path.starts_with("http://") || request.path.starts_with("https://") {
            request.path.clone()
        } else {
            format!("{}/{}", self.base, request.path.trim_start_matches('/'))
        };
        let mut url = url::Url::parse(&text).map_err(|err| {
            Error::invalid(format!(
                "you passed the path `{}`, which is not valid ({err}); pass a path such as /api/now/table/incident",
                request.path
            ))
        })?;
        let origin = url::Url::parse(&self.base).ok().map(|u| u.origin());
        if origin.as_ref() != Some(&url.origin()) {
            return Err(Error::invalid(format!(
                "you passed {}, which is not on {}; requests only go to the instance of the profile",
                url.origin().ascii_serialization(),
                self.base
            )));
        }
        if !request.query.is_empty() {
            let mut pairs = url.query_pairs_mut();
            for (name, value) in &request.query {
                pairs.append_pair(name, value);
            }
        }
        Ok(url)
    }

    async fn build(
        &self,
        request: &Request,
        url: &url::Url,
        rejected: Option<&str>,
    ) -> Result<reqwest::Request> {
        let mut builder = self.http.request(request.method.clone(), url.clone());
        let mut headers = HeaderMap::new();
        for (name, value) in &request.headers {
            let name = HeaderName::try_from(name.as_str()).map_err(|_| {
                Error::invalid(format!("you passed the header name `{name}`, which is not valid; use letters, digits and dashes"))
            })?;
            let value = HeaderValue::try_from(value.as_str()).map_err(|_| {
                Error::invalid(format!("the value of header {name} has characters that HTTP does not allow; use printable ASCII"))
            })?;
            headers.insert(name, value);
        }
        if !headers.contains_key("accept") {
            headers.insert("accept", HeaderValue::from_static("application/json"));
        }
        builder = builder.headers(headers);
        if let Some(timeout) = request.timeout {
            builder = builder.timeout(timeout);
        }
        builder = match &request.body {
            Body::None => builder,
            Body::Json(value) => builder.json(value),
            Body::Form(fields) => builder.form(fields),
            Body::Bytes { data, content_type } => builder
                .header("content-type", content_type)
                .body(data.clone()),
            Body::File { path, content_type } => {
                let file = tokio::fs::File::open(path).await.map_err(|err| {
                    Error::invalid(format!(
                        "cannot read {} ({err}); pass a file you can read",
                        path.display()
                    ))
                })?;
                let size = file.metadata().await.map(|m| m.len()).unwrap_or_default();
                builder
                    .header("content-type", content_type)
                    .header("content-length", size)
                    .body(reqwest::Body::from(file))
            }
            Body::Multipart {
                fields,
                file_field,
                file_name,
                data,
                content_type,
            } => {
                let mut form = reqwest::multipart::Form::new();
                for (name, value) in fields {
                    form = form.text(name.clone(), value.clone());
                }
                let part = reqwest::multipart::Part::stream(data.clone())
                    .file_name(file_name.clone())
                    .mime_str(content_type)
                    .map_err(|_| {
                        Error::invalid(format!(
                            "`{content_type}` is not a content type; use a value such as text/plain"
                        ))
                    })?;
                builder.multipart(form.part(file_field.clone(), part))
            }
        };
        if request.auth == AuthMode::Profile {
            builder = self.authorize(builder, rejected).await?;
        }
        Ok(builder.build()?)
    }

    /// `rejected` is a bearer token that the instance did not accept.
    async fn authorize(
        &self,
        builder: reqwest::RequestBuilder,
        rejected: Option<&str>,
    ) -> Result<reqwest::RequestBuilder> {
        let missing = |what: &str| {
            Error::Auth(format!(
                "profile `{}` has no stored {what}; run `sn auth login --profile {}`",
                self.name, self.name
            ))
        };
        let mut state = self.state.lock().await;
        Ok(match self.auth_kind {
            AuthKind::Basic => {
                let user = state
                    .profile
                    .username
                    .clone()
                    .ok_or_else(|| missing("user name"))?;
                let password = state
                    .secrets
                    .password
                    .clone()
                    .ok_or_else(|| missing("password"))?;
                builder.basic_auth(user, Some(password))
            }
            AuthKind::ApiKey => {
                let key = state
                    .secrets
                    .api_key
                    .as_deref()
                    .ok_or_else(|| missing("API key"))?;
                builder.header("x-sn-apikey", sensitive(key)?)
            }
            AuthKind::Token => {
                let token = self.external_token(&mut state, rejected).await?;
                builder.header("authorization", sensitive(&format!("Bearer {token}"))?)
            }
            AuthKind::Oauth => {
                let token = self.oauth_token(&mut state, rejected).await?;
                builder.header("authorization", sensitive(&format!("Bearer {token}"))?)
            }
            AuthKind::Cookie | AuthKind::Session => {
                if state.cookies.is_empty() {
                    return Err(missing("session"));
                }
                let mut builder =
                    builder.header("cookie", sensitive(&cookie_header(&state.cookies))?);
                if let Some(token) = state
                    .secrets
                    .user_token
                    .as_deref()
                    .filter(|t| !t.is_empty())
                {
                    builder = builder.header("x-usertoken", sensitive(token)?);
                }
                builder
            }
            AuthKind::Certificate => builder,
        })
    }

    /// True when a 401 can be answered with a new token.
    async fn renewable(&self) -> bool {
        let state = self.state.lock().await;
        match self.auth_kind {
            AuthKind::Oauth => {
                let grant = state
                    .profile
                    .oauth
                    .as_ref()
                    .map(|o| o.grant)
                    .unwrap_or_default();
                grant != Grant::AuthorizationCode || state.secrets.refresh_token.is_some()
            }
            AuthKind::Token => state.profile.token_command.is_some(),
            _ => false,
        }
    }

    /// The bearer token of an OAuth or token profile, renewed when needed.
    /// `None` when the profile has no token, or `renew` asks for a new one
    /// and it has only a fixed one.
    pub async fn bearer(&self, renew: bool) -> Result<Option<String>> {
        let mut state = self.state.lock().await;
        let current = state
            .run_token
            .clone()
            .or_else(|| state.secrets.access_token.clone())
            .filter(|_| renew);
        let rejected = current.as_deref();
        match self.auth_kind {
            AuthKind::Oauth => self.oauth_token(&mut state, rejected).await.map(Some),
            AuthKind::Token if renew && state.profile.token_command.is_none() => Ok(None),
            AuthKind::Token => self.external_token(&mut state, rejected).await.map(Some),
            _ => Ok(None),
        }
    }

    /// The session of a cookie profile.
    pub async fn ui_session(&self) -> UiSession {
        let state = self.state.lock().await;
        UiSession {
            cookies: state.cookies.clone(),
            user_token: state.secrets.user_token.clone().unwrap_or_default(),
        }
    }

    /// The profile's secrets with everything this client renewed.
    pub fn into_secrets(self) -> Secrets {
        let mut state = self.state.into_inner();
        if !state.cookies.is_empty() {
            state.secrets.cookie = Some(cookie_header(&state.cookies));
        }
        state.secrets
    }

    /// Credential headers for tools that make their own requests.
    pub async fn auth_headers(&self) -> Result<Vec<(String, String)>> {
        let builder = self.http.get(&self.base);
        let request = self.authorize(builder, None).await?.build()?;
        Ok(request
            .headers()
            .iter()
            .filter_map(|(n, v)| Some((n.as_str().to_owned(), v.to_str().ok()?.to_owned())))
            .collect())
    }

    async fn external_token(
        &self,
        state: &mut AuthState,
        rejected: Option<&str>,
    ) -> Result<String> {
        let Some(command) = state.profile.token_command.clone() else {
            return state.secrets.token.clone().ok_or_else(|| {
                Error::Auth(format!(
                    "profile `{}` has no stored token; run `sn auth login --profile {} --method token`",
                    self.name, self.name
                ))
            });
        };
        let run_token = state.run_token.clone();
        let known = run_token.filter(|t| rejected != Some(t.as_str()));
        if let Some(token) = known.or_else(|| usable(&state.secrets, rejected)) {
            return Ok(token);
        }
        let tokens = oauth::run_token_command(&command, &self.name, &self.base).await?;
        state.run_token = Some(tokens.access_token.clone());
        // Only a stated lifetime is worth caching.
        if tokens.expires_at.is_some() {
            state.secrets.access_token = Some(tokens.access_token.clone());
            state.secrets.expires_at = tokens.expires_at;
            if let Some(store) = &self.store {
                let _lock = store.lock_async().await?;
                store.save_secrets(&self.name, &state.profile, &state.secrets)?;
            }
        }
        Ok(tokens.access_token)
    }

    async fn oauth_token(&self, state: &mut AuthState, rejected: Option<&str>) -> Result<String> {
        if let Some(token) = usable(&state.secrets, rejected) {
            return Ok(token);
        }
        let config = state.profile.oauth.clone().ok_or_else(|| {
            Error::Auth(format!(
                "profile `{}` has no OAuth settings; run `sn auth login --profile {}`",
                self.name, self.name
            ))
        })?;
        // Refresh tokens rotate: two processes must not use the same one.
        let mut lock = None;
        if let Some(store) = &self.store {
            lock = Some(store.lock_async().await?);
            state.secrets = store.secrets(&self.name, &state.profile)?;
            if let Some(token) = usable(&state.secrets, rejected) {
                return Ok(token);
            }
        }
        let tokens = self.renew(&config, state).await?;
        let access = tokens.access_token.clone();
        tokens.store(&mut state.secrets);
        if let Some(store) = &self.store {
            store.save_secrets(&self.name, &state.profile, &state.secrets)?;
        }
        drop(lock);
        Ok(access)
    }

    async fn renew(&self, config: &OAuthConfig, state: &AuthState) -> Result<oauth::Tokens> {
        let secrets = &state.secrets;
        match config.grant {
            Grant::AuthorizationCode => {
                let again = || {
                    Error::Auth(format!(
                        "the sign-in of profile `{}` expired; run `sn auth login --profile {}`",
                        self.name, self.name
                    ))
                };
                let refresh = secrets.refresh_token.as_deref().ok_or_else(again)?;
                oauth::refresh(self, config, secrets.client_secret.as_deref(), refresh)
                    .await
                    .map_err(|err| match err {
                        Error::Api(failure)
                            if failure.status.is_some_and(|s| s < 500 && s != 429) =>
                        {
                            again()
                        }
                        other => other,
                    })
            }
            Grant::ClientCredentials => {
                oauth::client_credentials(self, config, secrets.client_secret.as_deref()).await
            }
            Grant::Password => {
                if let Some(refresh) = secrets.refresh_token.as_deref()
                    && let Ok(tokens) =
                        oauth::refresh(self, config, secrets.client_secret.as_deref(), refresh)
                            .await
                {
                    return Ok(tokens);
                }
                let user = state.profile.username.as_deref().unwrap_or_default();
                let password = secrets.password.as_deref().unwrap_or_default();
                oauth::password(
                    self,
                    config,
                    secrets.client_secret.as_deref(),
                    user,
                    password,
                )
                .await
            }
            Grant::JwtBearer => {
                oauth::jwt_bearer(self, config, secrets.client_secret.as_deref()).await
            }
        }
    }

    async fn adopt_cookies(&self, request: &Request, headers: &HeaderMap, status: u16) {
        let session = matches!(self.auth_kind, AuthKind::Cookie | AuthKind::Session);
        if !session || request.auth == AuthMode::None || status >= 300 {
            return;
        }
        let mut state = self.state.lock().await;
        for (name, value) in set_cookies(headers) {
            // An empty value is how the instance deletes a cookie.
            let before = match value.is_empty() {
                true => state.cookies.remove(&name).is_some(),
                false => state.cookies.insert(name, value.clone()).as_ref() != Some(&value),
            };
            state.cookies_changed |= before;
        }
    }

    /// Writes rotated session cookies back. Call it after the command
    /// succeeded.
    pub async fn finish(&self) -> Result<()> {
        let mut state = self.state.lock().await;
        let changed = std::mem::take(&mut state.cookies_changed);
        let Some(store) = self.store.as_ref().filter(|_| changed) else {
            return Ok(());
        };
        let _lock = store.lock_async().await?;
        let mut stored = store.secrets(&self.name, &state.profile)?;
        // A sign-in or a logout meanwhile is newer than these cookies.
        if stored.cookie == state.secrets.cookie {
            stored.cookie = Some(cookie_header(&state.cookies));
            store.save_secrets(&self.name, &state.profile, &stored)?;
            state.secrets = stored;
        }
        Ok(())
    }
}

/// The access token, when it is valid for now and not the rejected one.
fn usable(secrets: &Secrets, rejected: Option<&str>) -> Option<String> {
    let valid = secrets
        .expires_at
        .is_none_or(|at| at > crate::now_unix() + EXPIRY_MARGIN);
    let token = secrets.access_token.as_deref()?;
    (valid && rejected != Some(token)).then(|| token.to_owned())
}

fn sensitive(text: &str) -> Result<HeaderValue> {
    let mut value = HeaderValue::try_from(text.trim()).map_err(|_| {
        Error::invalid("the stored credential has characters that HTTP does not allow; sign in again with `sn auth login`")
    })?;
    value.set_sensitive(true);
    Ok(value)
}

fn backoff(attempt: u32) -> Duration {
    Duration::from_millis(250 << attempt.min(6))
}

fn describe(request: &Request, url: &url::Url) -> Value {
    let mut out = Map::new();
    out.insert("method".into(), request.method.as_str().into());
    out.insert("url".into(), url.as_str().into());
    if !request.headers.is_empty() {
        let headers = request
            .headers
            .iter()
            .map(|(n, v)| (n.clone(), mask_field(n, v).into()))
            .collect();
        out.insert("headers".into(), Value::Object(headers));
    }
    match &request.body {
        Body::None => {}
        Body::Json(value) => {
            out.insert("body".into(), value.clone());
        }
        Body::Form(fields) => {
            let fields: Map<String, Value> = fields
                .iter()
                .map(|(n, v)| (n.clone(), mask_field(n, v).into()))
                .collect();
            out.insert("form".into(), fields.into());
        }
        Body::Bytes { data, content_type } => {
            out.insert(
                "body".into(),
                json!({ "content_type": content_type, "bytes": data.len() }),
            );
        }
        Body::File { path, content_type } => {
            out.insert(
                "body".into(),
                json!({ "content_type": content_type, "file": path.display().to_string() }),
            );
        }
        Body::Multipart {
            fields,
            file_name,
            data,
            ..
        } => {
            let fields: Map<String, Value> = fields
                .iter()
                .map(|(n, v)| (n.clone(), mask_field(n, v).into()))
                .collect();
            out.insert(
                "body".into(),
                json!({ "fields": fields, "file": file_name, "bytes": data.len() }),
            );
        }
    }
    Value::Object(out)
}

/// Names that carry secrets in headers, forms and JSON.
fn is_secret(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    [
        "authorization",
        "cookie",
        "token",
        "secret",
        "password",
        "apikey",
        "api_key",
        "assertion",
        "sysparm_ck",
        "code_verifier",
    ]
    .iter()
    .any(|word| name.contains(word))
}

fn mask_field(name: &str, value: &str) -> String {
    match is_secret(name) {
        true => "[hidden]".to_owned(),
        false => value.to_owned(),
    }
}

fn mask_url(url: &url::Url) -> String {
    let mut shown = url.clone();
    if url.query_pairs().any(|(name, _)| is_secret(&name)) {
        shown.query_pairs_mut().clear().extend_pairs(
            url.query_pairs()
                .map(|(n, v)| (n.clone().into_owned(), mask_field(&n, &v))),
        );
    }
    shown.to_string()
}

fn mask_body(text: &str) -> String {
    fn walk(value: &mut Value) {
        match value {
            Value::Object(map) => {
                for (name, value) in map.iter_mut() {
                    if is_secret(name) && value.is_string() {
                        *value = "[hidden]".into();
                    } else {
                        walk(value);
                    }
                }
            }
            Value::Array(items) => items.iter_mut().for_each(walk),
            _ => {}
        }
    }
    match serde_json::from_str::<Value>(text) {
        Ok(mut value) => {
            walk(&mut value);
            value.to_string()
        }
        Err(_) => format!("({} bytes, not JSON)", text.len()),
    }
}

fn looks_like_html(body: &[u8]) -> bool {
    let start = String::from_utf8_lossy(&body[..body.len().min(512)])
        .trim_start()
        .to_ascii_lowercase();
    start.starts_with("<!doctype") || start.starts_with("<html")
}

fn not_json(body: &[u8]) -> Error {
    let text = String::from_utf8_lossy(body);
    if text.to_ascii_lowercase().contains("hibernat") {
        return Error::Network(
            "the developer instance is hibernating; wake it at https://developer.servicenow.com and try again".into(),
        );
    }
    if looks_like_html(body) {
        return Error::Auth(
            "the instance answered with a web page, not with data; the session is not accepted or the path does not exist, so check the path, then run `sn auth login`".into(),
        );
    }
    let start: String = text.chars().take(120).collect();
    Error::Internal(format!(
        "the instance answered with data that is not JSON: {start}"
    ))
}

/// Builds the HTTP client: OS certificate store, proxy, client certificate.
fn build_http(
    network: &Network,
    secrets: &Secrets,
    timeout: Duration,
    follow: bool,
) -> Result<reqwest::Client> {
    let read = |path: &PathBuf, what: &str| {
        std::fs::read(path).map_err(|err| {
            Error::invalid(format!(
                "cannot read the {what} {} ({err}); pass a PEM file you can read",
                path.display()
            ))
        })
    };
    let mut builder = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(timeout)
        .connect_timeout(Duration::from_secs(15))
        // Credentials must not follow a redirect to another host, and a
        // redirect to a sign-in page must be visible.
        .redirect(reqwest::redirect::Policy::custom(move |attempt| {
            let same = attempt
                .previous()
                .last()
                .is_some_and(|from| from.origin() == attempt.url().origin());
            let path = attempt.url().path();
            let sign_in = path.contains("login") || path.contains("auth") || path.contains("saml");
            if follow && same && !sign_in && attempt.previous().len() < 5 {
                attempt.follow()
            } else {
                attempt.stop()
            }
        }));
    if let Some(path) = &network.ca_cert {
        let certificates = reqwest::Certificate::from_pem_bundle(&read(path, "CA certificate")?)
            .map_err(|err| {
                Error::invalid(format!(
                    "{} is not a PEM certificate bundle ({err}); export the certificate as PEM",
                    path.display()
                ))
            })?;
        for certificate in certificates {
            builder = builder.add_root_certificate(certificate);
        }
    }
    if let Some(path) = &network.client_cert {
        let identity = reqwest::Identity::from_pem(&read(path, "client certificate")?).map_err(|err| {
            Error::invalid(format!(
                "{} is not a client certificate with its private key ({err}); put both in one PEM file",
                path.display()
            ))
        })?;
        builder = builder.identity(identity);
    }
    if network.insecure {
        builder = builder.danger_accept_invalid_certs(true);
    }
    match network.proxy.as_deref() {
        Some("") | Some("none") => builder = builder.no_proxy(),
        Some(proxy) => {
            let mut proxy = reqwest::Proxy::all(proxy).map_err(|err| {
                Error::invalid(format!("`{proxy}` is not a proxy URL ({err}); use http://host:port or socks5://host:port"))
            })?;
            if let Some(user) = &secrets.proxy_user {
                proxy = proxy.basic_auth(user, secrets.proxy_password.as_deref().unwrap_or(""));
            }
            if let Some(list) = &network.no_proxy {
                proxy = proxy.no_proxy(reqwest::NoProxy::from_string(list));
            }
            builder = builder.proxy(proxy);
        }
        None => {}
    }
    builder.build().map_err(Error::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_are_masked() {
        let url = url::Url::parse("https://x.com/a?token=abc&b=1").expect("url");
        assert_eq!(mask_url(&url), "https://x.com/a?token=%5Bhidden%5D&b=1");
        assert_eq!(
            mask_body(r#"{"access_token":"a","n":{"password":"b","x":1}}"#),
            r#"{"access_token":"[hidden]","n":{"password":"[hidden]","x":1}}"#
        );
        assert_eq!(mask_body("secret text"), "(11 bytes, not JSON)");
    }

    #[test]
    fn cookies_parse_and_print() {
        let cookies = parse_cookie_header("Cookie: JSESSIONID=a; glide_user_route=b=c");
        assert_eq!(
            cookie_header(&cookies),
            "JSESSIONID=a; glide_user_route=b=c"
        );
    }
}

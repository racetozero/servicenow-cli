//! ServiceNow's message bus (AMB): the Bayeux protocol over a websocket, the
//! transport behind record watchers. It is not documented; the behaviour here
//! is the one the MIT-licensed `sn` CLI measured on a live instance.
//!
//! - `/amb` reads only session cookies. Any other credential first opens a
//!   session over HTTP.
//! - The upgrade needs an `Origin` header, else it answers 403.
//! - The upgrade and the handshake succeed without a valid session too, and
//!   then no event ever arrives. Only `ext["glide.session.status"]` tells.
//! - Events flow only while a `/meta/connect` is open, so each answer to one
//!   is followed by the next.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use futures_util::{SinkExt, StreamExt};
use rustls::ClientConfig;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls_platform_verifier::Verifier;
use serde_json::{Value, json};
use sn_core::client::{Debug, Method, Request};
use sn_core::config::{Network, Secrets};
use sn_core::uisession::UiSession;
use sn_core::{Client, Error, Result, percent};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::{self, Message};
use tokio_tungstenite::{Connector, MaybeTlsStream, WebSocketStream};

const LOGGED_IN: &str = "session.logged.in";

/// The channel of every change to the records that match `filter`.
pub fn record_channel(table: &str, filter: &str) -> String {
    // Standard base64, but with dashes for padding.
    let encoded = STANDARD.encode(filter).replace('=', "-");
    format!("/rw/default/{table}/{encoded}")
}

/// A cookie header for a session of this profile.
pub async fn session_cookies(client: &Client) -> Result<String> {
    match UiSession::open(client).await {
        Ok(session) => Ok(session.cookie_header()),
        // Older instances do not have the session endpoint; any signed-in
        // request opens a session too.
        Err(err) if err.status() == Some(404) => {
            let request = Request::new(Method::GET, "/api/now/table/sys_user")
                .query("sysparm_limit", "1")
                .query("sysparm_fields", "sys_id");
            Ok(UiSession::opened_by(&client.send(request).await?)?.cookie_header())
        }
        Err(err) => Err(err),
    }
}

/// How to reach `/amb` of an instance. One value opens any number of sockets.
pub struct Transport {
    url: String,
    origin: String,
    host: String,
    port: u16,
    tls: Option<Arc<ClientConfig>>,
    proxy: Option<Proxy>,
    timeout: Duration,
    debug: Debug,
}

impl Transport {
    /// Takes the certificate and proxy settings of the profile: the socket
    /// does not go through the HTTP client.
    pub fn new(client: &Client, timeout: Duration) -> Result<Self> {
        let base = url::Url::parse(client.base()).map_err(|err| {
            Error::invalid(format!(
                "the instance address {} is not valid ({err}); set it with `sn auth login --instance NAME`",
                client.base()
            ))
        })?;
        let host = base
            .host_str()
            .unwrap_or_default()
            .trim_matches(['[', ']'])
            .to_owned();
        let secure = base.scheme() == "https";
        let (network, secrets) = client.network();
        Ok(Self {
            url: client.base().replacen("http", "ws", 1) + "/amb",
            origin: base.origin().ascii_serialization(),
            port: base.port_or_known_default().unwrap_or(443),
            tls: match secure {
                true => Some(Arc::new(tls_config(network)?)),
                false => None,
            },
            proxy: Proxy::select(network, secrets, &host, secure, &|name| {
                std::env::var(name).ok().filter(|v| !v.trim().is_empty())
            })?,
            host,
            timeout,
            debug: client.debug(),
        })
    }

    /// A session that is subscribed to `channel`.
    pub async fn connect(&self, cookies: &str, channel: &str) -> Result<Amb> {
        let timed_out = |_| {
            Error::Network(format!(
                "{} did not answer on the message bus within {} seconds; raise the limit with --timeout SECONDS",
                self.host,
                self.timeout.as_secs()
            ))
        };
        let opened = async {
            let mut amb = self.open(cookies).await?;
            amb.subscribe(channel).await?;
            Ok(amb)
        };
        tokio::time::timeout(self.timeout, opened)
            .await
            .map_err(timed_out)?
    }

    async fn open(&self, cookies: &str) -> Result<Amb> {
        if self.debug >= Debug::Requests {
            eprintln!("> WS {}", self.url);
        }
        let bad_header = |_| {
            Error::Auth("the session cookie has characters that HTTP does not allow; sign in again with `sn auth login`".into())
        };
        let mut request = self
            .url
            .as_str()
            .into_client_request()
            .map_err(|err| self.failed(err))?;
        let headers = request.headers_mut();
        let mut cookie: tungstenite::http::HeaderValue = cookies.parse().map_err(bad_header)?;
        cookie.set_sensitive(true);
        headers.insert("cookie", cookie);
        headers.insert("origin", self.origin.parse().map_err(bad_header)?);
        headers.insert(
            "user-agent",
            tungstenite::http::HeaderValue::from_static(concat!("sn/", env!("CARGO_PKG_VERSION"))),
        );
        let stream = match &self.proxy {
            Some(proxy) => proxy.tunnel(&self.host, self.port).await?,
            None => TcpStream::connect((self.host.as_str(), self.port))
                .await
                .map_err(|err| {
                    Error::Network(format!(
                        "cannot connect to {}:{} ({err}); check the instance name, your network, and --proxy",
                        self.host, self.port
                    ))
                })?,
        };
        let connector = match &self.tls {
            Some(config) => Connector::Rustls(Arc::clone(config)),
            None => Connector::Plain,
        };
        let (socket, _) =
            tokio_tungstenite::client_async_tls_with_config(request, stream, None, Some(connector))
                .await
                .map_err(|err| self.failed(err))?;
        let mut amb = Amb {
            socket,
            client_id: String::new(),
            next_id: 0,
            rearm: false,
            dying: None,
            pending: Vec::new(),
            debug: self.debug,
        };
        amb.handshake().await?;
        Ok(amb)
    }

    fn failed(&self, err: tungstenite::Error) -> Error {
        match err {
            tungstenite::Error::Http(response) => {
                let status = response.status().as_u16();
                let text = format!("{} refused the websocket with HTTP {status}", self.host);
                match status {
                    401 | 403 => Error::Auth(format!(
                        "{text}; check that your user can sign in, and run `sn auth login`"
                    )),
                    _ => Error::Network(format!(
                        "{text}; the message bus may be off, or a proxy may block websockets"
                    )),
                }
            }
            err => {
                let text = err.to_string();
                let hint = match text.contains("certificate") || text.contains("UnknownIssuer") {
                    true => {
                        "; the server certificate is not trusted: install its root in the operating system certificate store, or pass --ca-cert FILE"
                    }
                    false => "",
                };
                Error::Network(format!(
                    "the websocket to {} failed: {text}{hint}",
                    self.host
                ))
            }
        }
    }
}

/// One websocket session. It does not reconnect: that needs a new session
/// cookie, so the watcher does it.
pub struct Amb {
    socket: WebSocketStream<MaybeTlsStream<TcpStream>>,
    client_id: String,
    next_id: u64,
    /// A `/meta/connect` was answered and the next one is due.
    rearm: bool,
    /// The session ended; events of the same frame are delivered first.
    dying: Option<String>,
    pending: Vec<Value>,
    debug: Debug,
}

impl Amb {
    async fn handshake(&mut self) -> Result<()> {
        let reply = self
            .exchange(
                "/meta/handshake",
                json!({
                    "channel": "/meta/handshake",
                    "version": "1.0",
                    "minimumVersion": "1.0",
                    "supportedConnectionTypes": ["websocket", "long-polling"],
                    "advice": { "timeout": 60_000, "interval": 0 },
                    "ext": { "supportsSubscribeCommandFlow": true },
                }),
            )
            .await?;
        // An invalid session also gets `successful` and a client id.
        if let Some(status) = session_status(&reply).filter(|s| *s != LOGGED_IN) {
            return Err(Error::Auth(format!(
                "the instance rejected the session of this profile for the message bus ({status}); run `sn auth login`"
            )));
        }
        let client_id = reply["clientId"].as_str().unwrap_or_default();
        if reply["successful"] != true || client_id.is_empty() {
            return Err(Error::Network(format!(
                "the message bus refused the handshake: {}",
                reply["error"].as_str().unwrap_or("no reason given")
            )));
        }
        self.client_id = client_id.to_owned();
        // The first connect is answered at once and arms the next one.
        let connect = json!({
            "channel": "/meta/connect",
            "connectionType": "websocket",
            "advice": { "timeout": 0 },
            "clientId": self.client_id,
        });
        self.exchange("/meta/connect", connect).await?;
        self.arm().await
    }

    async fn subscribe(&mut self, channel: &str) -> Result<()> {
        let message = json!({
            "channel": "/meta/subscribe",
            "subscription": channel,
            "clientId": self.client_id,
        });
        let reply = self.exchange("/meta/subscribe", message).await?;
        if reply["successful"] == true {
            return Ok(());
        }
        Err(Error::api(
            format!(
                "the message bus refused to watch {channel}: {}; check the table name and the encoded query",
                reply["error"].as_str().unwrap_or("no reason given")
            ),
            reply.clone(),
        ))
    }

    /// Waits for events. The list can be empty. Dropping the future loses
    /// nothing.
    pub async fn poll(&mut self) -> Result<Vec<Value>> {
        if self.pending.is_empty() {
            if let Some(reason) = self.dying.take() {
                return Err(Error::Network(reason));
            }
            self.arm().await?;
            for message in self.read().await? {
                self.sort(message);
            }
            if self.pending.is_empty()
                && let Some(reason) = self.dying.take()
            {
                return Err(Error::Network(reason));
            }
        }
        Ok(std::mem::take(&mut self.pending))
    }

    /// Leaves the bus, as well as possible.
    pub async fn disconnect(mut self) {
        let goodbye = async {
            let message = json!({ "channel": "/meta/disconnect", "clientId": self.client_id });
            let _ = self.send(message).await;
            let _ = self.socket.close(None).await;
        };
        let _ = tokio::time::timeout(Duration::from_secs(2), goodbye).await;
    }

    /// Sends a message and waits for its answer.
    async fn exchange(&mut self, channel: &str, message: Value) -> Result<Value> {
        self.arm().await?;
        self.send(message).await?;
        loop {
            let mut reply = None;
            for message in self.read().await? {
                if reply.is_none() && message["channel"] == channel {
                    reply = Some(message.clone());
                }
                self.sort(message);
            }
            // A session that ended meanwhile fails later with an error that
            // blames the table name.
            if let Some(reason) = self.dying.take() {
                return Err(Error::Network(reason));
            }
            if let Some(reply) = reply {
                return Ok(reply);
            }
            self.arm().await?;
        }
    }

    fn sort(&mut self, mut message: Value) {
        let channel = message["channel"].as_str().unwrap_or_default();
        match channel {
            "/meta/connect" => match ended(&message) {
                Some(reason) => self.dying = Some(reason.to_owned()),
                None => self.rearm = true,
            },
            "" => {}
            other if other.starts_with("/meta/") => {}
            _ => {
                if let Some(data) = message.get_mut("data") {
                    self.pending.push(data.take());
                }
            }
        }
    }

    async fn arm(&mut self) -> Result<()> {
        if self.rearm {
            let message = json!({
                "channel": "/meta/connect",
                "connectionType": "websocket",
                "clientId": self.client_id,
            });
            self.send(message).await?;
            // After the send: a dropped future must not lose the connect.
            self.rearm = false;
        }
        Ok(())
    }

    async fn send(&mut self, mut message: Value) -> Result<()> {
        self.next_id += 1;
        message["id"] = self.next_id.to_string().into();
        let text = Value::Array(vec![message]).to_string();
        if self.debug >= Debug::Full {
            eprintln!("> {text}");
        }
        self.socket.send(Message::text(text)).await.map_err(lost)
    }

    /// The messages of one frame.
    async fn read(&mut self) -> Result<Vec<Value>> {
        loop {
            match self.socket.next().await {
                Some(Ok(Message::Text(text))) => {
                    if self.debug >= Debug::Full {
                        eprintln!("< {text}");
                    }
                    let parsed: Value = serde_json::from_str(&text).map_err(|err| {
                        Error::Network(format!("the message bus sent invalid JSON ({err})"))
                    })?;
                    return Ok(match parsed {
                        Value::Array(messages) => messages,
                        single => vec![single],
                    });
                }
                Some(Ok(Message::Close(_))) | None => {
                    return Err(Error::Network(
                        "the instance closed the message bus connection".into(),
                    ));
                }
                Some(Ok(_)) => {}
                Some(Err(err)) => return Err(lost(err)),
            }
        }
    }
}

fn lost(err: tungstenite::Error) -> Error {
    Error::Network(format!("the message bus connection was lost ({err})"))
}

fn session_status(message: &Value) -> Option<&str> {
    message["ext"]["glide.session.status"].as_str()
}

/// Why an answer to `/meta/connect` means that the session is over. The
/// instance ends a watcher's session after a minute or two; such a session
/// still answers `successful: true` and delivers nothing.
fn ended(message: &Value) -> Option<&'static str> {
    if session_status(message).is_some_and(|s| s != LOGGED_IN) {
        return Some("the instance ended the message bus session");
    }
    (message["advice"]["reconnect"] == "handshake")
        .then_some("the message bus session expired and needs a new handshake")
}

fn pem_error(path: &Path, what: &str, err: impl std::fmt::Display) -> Error {
    Error::invalid(format!(
        "{} is not a usable {what} ({err}); pass a PEM file",
        path.display()
    ))
}

fn certificates(path: &Path, what: &str) -> Result<Vec<CertificateDer<'static>>> {
    let list = CertificateDer::pem_file_iter(path)
        .map_err(|err| pem_error(path, what, err))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|err| pem_error(path, what, err))?;
    match list.is_empty() {
        true => Err(pem_error(path, what, "it has no certificate")),
        false => Ok(list),
    }
}

/// The trust of the HTTP client: the operating system, plus `--ca-cert`.
fn tls_config(network: &Network) -> Result<ClientConfig> {
    let failed = |err: rustls::Error| Error::Internal(format!("cannot set up TLS ({err})"));
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let verifier: Arc<dyn ServerCertVerifier> = match (network.insecure, &network.ca_cert) {
        (true, _) => Arc::new(Unverified(Arc::clone(&provider))),
        (false, Some(path)) => Arc::new(
            Verifier::new_with_extra_roots(
                certificates(path, "CA certificate")?,
                Arc::clone(&provider),
            )
            .map_err(|err| pem_error(path, "CA certificate", err))?,
        ),
        (false, None) => Arc::new(Verifier::new(Arc::clone(&provider)).map_err(failed)?),
    };
    let builder = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(failed)?
        .dangerous()
        .with_custom_certificate_verifier(verifier);
    match &network.client_cert {
        None => Ok(builder.with_no_client_auth()),
        Some(path) => {
            let what = "client certificate";
            let key =
                PrivateKeyDer::from_pem_file(path).map_err(|err| pem_error(path, what, err))?;
            builder
                .with_client_auth_cert(certificates(path, what)?, key)
                .map_err(|err| pem_error(path, what, err))
        }
    }
}

/// `--insecure`: accepts any certificate, and still checks that the server
/// has its key, as the HTTP client does.
#[derive(Debug)]
struct Unverified(Arc<CryptoProvider>);

impl ServerCertVerifier for Unverified {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signed: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        let algorithms = &self.0.signature_verification_algorithms;
        rustls::crypto::verify_tls12_signature(message, cert, signed, algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signed: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        let algorithms = &self.0.signature_verification_algorithms;
        rustls::crypto::verify_tls13_signature(message, cert, signed, algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

/// An HTTP proxy that tunnels with CONNECT.
#[derive(Debug, PartialEq, Eq)]
struct Proxy {
    host: String,
    port: u16,
    /// The `Proxy-Authorization` value.
    authorization: Option<String>,
}

impl Proxy {
    /// The proxy that the profile or the environment names for `host`.
    fn select(
        network: &Network,
        secrets: &Secrets,
        host: &str,
        secure: bool,
        env: &dyn Fn(&str) -> Option<String>,
    ) -> Result<Option<Self>> {
        let names = match secure {
            true => ["HTTPS_PROXY", "https_proxy", "ALL_PROXY", "all_proxy"],
            false => ["HTTP_PROXY", "http_proxy", "ALL_PROXY", "all_proxy"],
        };
        let text = match network.proxy.as_deref() {
            Some("" | "none") => None,
            Some(proxy) => Some(proxy.to_owned()),
            None => names.iter().find_map(|name| env(name)),
        };
        let Some(text) = text else { return Ok(None) };
        let exceptions = network
            .no_proxy
            .clone()
            .or_else(|| env("NO_PROXY"))
            .or_else(|| env("no_proxy"))
            .unwrap_or_default();
        if exceptions.split(',').any(|entry| direct(host, entry)) {
            return Ok(None);
        }
        let parsed = match text.contains("://") {
            true => url::Url::parse(&text),
            false => url::Url::parse(&format!("http://{text}")),
        };
        let usable = parsed
            .ok()
            .filter(|u| u.scheme() == "http" && u.host_str().is_some());
        let Some(url) = usable else {
            return Err(Error::invalid(
                "this command opens a websocket, which goes only through http:// proxies, and another proxy is configured; pass --proxy http://HOST:PORT, or --no-proxy when the instance is reachable directly",
            ));
        };
        let login = match (url.username(), &secrets.proxy_user) {
            ("", None) => None,
            ("", Some(user)) => Some((
                user.clone(),
                secrets.proxy_password.clone().unwrap_or_default(),
            )),
            (user, _) => Some((
                percent::decode(user),
                percent::decode(url.password().unwrap_or_default()),
            )),
        };
        Ok(Some(Self {
            host: url
                .host_str()
                .unwrap_or_default()
                .trim_matches(['[', ']'])
                .to_owned(),
            port: url.port_or_known_default().unwrap_or(80),
            authorization: login.map(|(user, password)| {
                format!("Basic {}", STANDARD.encode(format!("{user}:{password}")))
            }),
        }))
    }

    async fn tunnel(&self, host: &str, port: u16) -> Result<TcpStream> {
        let failed = |why: String| {
            Error::Network(format!(
                "the proxy {}:{} did not connect to {host}:{port} ({why}); check --proxy and its credentials",
                self.host, self.port
            ))
        };
        let mut stream = TcpStream::connect((self.host.as_str(), self.port))
            .await
            .map_err(|err| failed(err.to_string()))?;
        let target = match host.contains(':') {
            true => format!("[{host}]:{port}"),
            false => format!("{host}:{port}"),
        };
        let mut request = format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n");
        if let Some(authorization) = &self.authorization {
            request.push_str(&format!("Proxy-Authorization: {authorization}\r\n"));
        }
        request.push_str("\r\n");
        stream
            .write_all(request.as_bytes())
            .await
            .map_err(|err| failed(err.to_string()))?;
        // Byte by byte: what follows the headers belongs to the tunnel.
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            let byte = stream.read_u8().await;
            head.push(byte.map_err(|err| failed(err.to_string()))?);
            if head.len() > 16 << 10 {
                return Err(failed("its answer is too long".into()));
            }
        }
        let head = String::from_utf8_lossy(&head);
        let line = head.lines().next().unwrap_or_default();
        match line.split(' ').nth(1) {
            Some(status) if status.starts_with('2') => Ok(stream),
            _ => Err(failed(format!("it answered `{line}`"))),
        }
    }
}

/// `NO_PROXY` entries: a host, a domain with or without a leading dot, or `*`.
fn direct(host: &str, entry: &str) -> bool {
    let entry = entry.trim().trim_prefix("*.").trim_start_matches('.');
    let entry = match entry.rsplit_once(':') {
        Some((name, port)) if port.bytes().all(|b| b.is_ascii_digit()) && !name.contains(':') => {
            name
        }
        _ => entry,
    };
    let host = host.to_ascii_lowercase();
    let entry = entry.to_ascii_lowercase();
    entry == "*" || (!entry.is_empty() && (host == entry || host.ends_with(&format!(".{entry}"))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channels_encode_the_filter_with_dashes_for_padding() {
        let channel = |filter: &str| record_channel("incident", filter);
        assert_eq!(
            channel("sys_id=1c741bd70b2322007518478d83673af3"),
            "/rw/default/incident/c3lzX2lkPTFjNzQxYmQ3MGIyMzIyMDA3NTE4NDc4ZDgzNjczYWYz"
        );
        assert!(channel("ab").ends_with("YWI-"));
        assert!(channel("a").ends_with("YQ--"));
        assert!(
            channel(">>>???").ends_with("Pj4+Pz8/"),
            "the alphabet is the standard one"
        );
    }

    #[test]
    fn a_session_ends_only_on_an_explicit_verdict() {
        assert_eq!(ended(&json!({ "successful": true })), None);
        assert_eq!(
            ended(&json!({ "ext": { "glide.session.status": LOGGED_IN } })),
            None
        );
        assert!(
            ended(&json!({ "successful": true, "ext": { "glide.session.status": "session.invalidated" } }))
                .is_some()
        );
        assert!(ended(&json!({ "advice": { "reconnect": "handshake" } })).is_some());
        assert_eq!(ended(&json!({ "advice": { "reconnect": "retry" } })), None);
    }

    fn select(network: &Network, env: &[(&str, &str)]) -> Result<Option<Proxy>> {
        Proxy::select(
            network,
            &Secrets::default(),
            "acme.service-now.com",
            true,
            &|name| {
                env.iter()
                    .find(|(n, _)| *n == name)
                    .map(|(_, v)| (*v).to_owned())
            },
        )
    }

    #[test]
    fn proxies_come_from_the_profile_then_the_environment() {
        let plain = Network::default();
        assert_eq!(select(&plain, &[]).expect("none"), None);
        let proxy = select(&plain, &[("HTTPS_PROXY", "http://u:p%40@proxy:3128")])
            .expect("proxy")
            .expect("some");
        assert_eq!((proxy.host.as_str(), proxy.port), ("proxy", 3128));
        assert_eq!(
            proxy.authorization,
            Some(format!("Basic {}", STANDARD.encode("u:p@")))
        );
        let env = [
            ("HTTPS_PROXY", "proxy:3128"),
            ("NO_PROXY", "localhost,.service-now.com"),
        ];
        assert_eq!(select(&plain, &env).expect("exception"), None);
        let off = Network {
            proxy: Some("none".into()),
            ..Network::default()
        };
        assert_eq!(select(&off, &env[..1]).expect("off"), None);
        let socks = Network {
            proxy: Some("socks5://proxy:1080".into()),
            ..Network::default()
        };
        let err = select(&socks, &[]).expect_err("socks").to_string();
        assert!(err.contains("--proxy http://HOST:PORT"), "{err}");
    }

    #[test]
    fn tls_trusts_the_system_and_rejects_files_without_certificates() {
        assert!(tls_config(&Network::default()).is_ok());
        let file = tempfile::NamedTempFile::new().expect("file");
        std::fs::write(file.path(), "not a certificate").expect("write");
        let network = Network {
            ca_cert: Some(file.path().to_owned()),
            ..Network::default()
        };
        let err = tls_config(&network).expect_err("bad file").to_string();
        assert!(err.contains("CA certificate"), "{err}");
    }
}

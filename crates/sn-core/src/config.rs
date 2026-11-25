//! Profiles (`config.toml`) and their secrets (`credentials.toml`, or the
//! credential store of the operating system).

use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;
use url::Url;

pub const DEFAULT_PROFILE: &str = "default";
/// The service name in the operating system credential store.
const KEYCHAIN_SERVICE: &str = "sn";
const LOCK_POLL: Duration = Duration::from_millis(50);

/// How a profile authenticates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum AuthKind {
    /// OAuth 2.0: browser sign-in (SSO), password, client credentials, JWT.
    Oauth,
    #[default]
    Basic,
    /// Form login that mints a session: local accounts with MFA.
    Session,
    /// Inbound REST API key (`x-sn-apikey`).
    ApiKey,
    /// A bearer token issued elsewhere, stored or printed by a command.
    Token,
    /// A copied browser session: cookie header plus user token.
    Cookie,
    /// Only a TLS client certificate.
    Certificate,
}

impl AuthKind {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Oauth => "oauth",
            Self::Basic => "basic",
            Self::Session => "session",
            Self::ApiKey => "api-key",
            Self::Token => "token",
            Self::Cookie => "cookie",
            Self::Certificate => "certificate",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Grant {
    #[default]
    AuthorizationCode,
    ClientCredentials,
    Password,
    JwtBearer,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OAuthConfig {
    pub client_id: String,
    pub grant: Grant,
    pub redirect_uri: String,
    /// Sign in without PKCE, for instances that refuse it.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub no_pkce: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jwt_key_file: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jwt_subject: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jwt_kid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jwt_alg: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StoreKind {
    File,
    Keychain,
}

impl Default for StoreKind {
    /// Credential Manager on Windows. A 0600 file elsewhere: macOS asks for
    /// permission again after every update of an unsigned binary, and a
    /// locked Linux keyring would block agents.
    fn default() -> Self {
        if cfg!(windows) {
            Self::Keychain
        } else {
            Self::File
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Profile {
    /// Base URL, for example `https://acme.service-now.com`.
    pub instance: String,
    pub auth: AuthKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    /// Refuse every write.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub read_only: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oauth: Option<OAuthConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub store: Option<StoreKind>,
    #[serde(flatten)]
    pub network: Network,
}

impl Profile {
    pub fn store_kind(&self) -> StoreKind {
        self.store.unwrap_or_default()
    }
}

/// Proxy and TLS settings.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Network {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub no_proxy: Option<String>,
    /// Extra trusted root certificates (PEM).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ca_cert: Option<PathBuf>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub insecure: bool,
    /// Client certificate and key in one PEM file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_cert: Option<PathBuf>,
}

/// Secrets of one profile. Never printed.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Secrets {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// A static bearer token (`AuthKind::Token`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub access_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    /// Seconds since 1970 when `access_token` expires.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cookie: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy_user: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy_password: Option<String>,
}

impl std::fmt::Debug for Secrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secrets(hidden)")
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ConfigFile {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_profile: Option<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub aliases: BTreeMap<String, String>,
    pub profiles: BTreeMap<String, Profile>,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct CredentialsFile {
    profiles: BTreeMap<String, Secrets>,
}

/// The configuration directory.
#[derive(Debug, Clone)]
pub struct Store {
    dir: PathBuf,
}

/// Holds the configuration lock until dropped.
pub struct Lock(#[allow(dead_code)] File);

impl Store {
    /// `SN_CONFIG_DIR`, or the platform's configuration directory.
    pub fn open() -> Result<Self> {
        if let Some(dir) = std::env::var_os("SN_CONFIG_DIR").filter(|d| !d.is_empty()) {
            return Ok(Self { dir: dir.into() });
        }
        // `~/.config` on macOS too: it is where people look for CLI settings.
        let base = if cfg!(windows) {
            dirs::config_dir()
        } else {
            std::env::var_os("XDG_CONFIG_HOME")
                .filter(|d| !d.is_empty())
                .map(PathBuf::from)
                .or_else(|| dirs::home_dir().map(|home| home.join(".config")))
        };
        let base = base.ok_or_else(|| {
            Error::invalid(
                "cannot find the home directory; set HOME, or set SN_CONFIG_DIR to a directory for the configuration",
            )
        })?;
        Ok(Self {
            dir: base.join("sn"),
        })
    }

    pub fn at(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn config_path(&self) -> PathBuf {
        self.dir.join("config.toml")
    }

    fn credentials_path(&self) -> PathBuf {
        self.dir.join("credentials.toml")
    }

    /// Where cached metadata lives: `SN_CACHE_DIR`, or `cache` in
    /// the configuration directory.
    pub fn cache_dir(&self) -> PathBuf {
        std::env::var_os("SN_CACHE_DIR")
            .filter(|d| !d.is_empty())
            .map_or_else(|| self.dir.join("cache"), PathBuf::from)
    }

    /// The lock, unless another process holds it.
    fn try_lock(&self) -> Result<Option<Lock>> {
        self.create_dir()?;
        let path = self.dir.join(".lock");
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(|err| file_error("open", &path, &err))?;
        match file.try_lock() {
            Ok(()) => Ok(Some(Lock(file))),
            Err(std::fs::TryLockError::WouldBlock) => Ok(None),
            Err(std::fs::TryLockError::Error(err)) => Err(file_error("lock", &path, &err)),
        }
    }

    fn locked(&self) -> Error {
        Error::Internal(format!(
            "another sn command holds {} for too long; wait for it to finish and try again",
            self.dir.join(".lock").display()
        ))
    }

    /// Serializes read-modify-write cycles between processes.
    pub fn lock(&self) -> Result<Lock> {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(lock) = self.try_lock()? {
                return Ok(lock);
            }
            if std::time::Instant::now() >= deadline {
                return Err(self.locked());
            }
            std::thread::sleep(LOCK_POLL);
        }
    }

    /// Waits longer than [`Store::lock`]: the holder can be renewing a token.
    pub(crate) async fn lock_async(&self) -> Result<Lock> {
        let wait = async {
            loop {
                match self.try_lock().transpose() {
                    Some(lock) => return lock,
                    None => tokio::time::sleep(LOCK_POLL).await,
                }
            }
        };
        tokio::time::timeout(Duration::from_secs(90), wait)
            .await
            .unwrap_or_else(|_| Err(self.locked()))
    }

    pub fn load(&self) -> Result<ConfigFile> {
        read_toml(&self.config_path())
    }

    pub fn save(&self, config: &ConfigFile) -> Result<()> {
        self.create_dir()?;
        write_toml(&self.config_path(), config)
    }

    pub fn secrets(&self, name: &str, profile: &Profile) -> Result<Secrets> {
        if profile.store_kind() == StoreKind::Keychain
            && let Some(text) = keychain_get(name)
        {
            return serde_json::from_str(&text).map_err(|err| {
                Error::invalid(format!(
                    "the stored credential of profile `{name}` is not readable ({err}); run `sn auth login --profile {name}` to replace it"
                ))
            });
        }
        let mut file: CredentialsFile = read_toml(&self.credentials_path())?;
        Ok(file.profiles.remove(name).unwrap_or_default())
    }

    /// Writes the secrets. A credential store that refuses (too large, not
    /// available) falls back to the 0600 file; the result tells which.
    pub fn save_secrets(
        &self,
        name: &str,
        profile: &Profile,
        secrets: &Secrets,
    ) -> Result<StoreKind> {
        let kind = profile.store_kind();
        let empty = *secrets == Secrets::default();
        let in_keychain = kind == StoreKind::Keychain
            && !empty
            && keychain_set(name, &serde_json::to_string(secrets)?);
        if empty && kind == StoreKind::Keychain {
            keychain_delete(name);
        }
        let path = self.credentials_path();
        let mut file: CredentialsFile = read_toml(&path)?;
        let changed = if in_keychain || empty {
            file.profiles.remove(name).is_some()
        } else {
            file.profiles.insert(name.to_owned(), secrets.clone());
            true
        };
        if changed {
            self.create_dir()?;
            write_toml(&path, &file)?;
        }
        Ok(if in_keychain {
            StoreKind::Keychain
        } else {
            StoreKind::File
        })
    }

    fn create_dir(&self) -> Result<()> {
        std::fs::create_dir_all(&self.dir).map_err(|err| file_error("create", &self.dir, &err))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&self.dir, std::fs::Permissions::from_mode(0o700));
        }
        Ok(())
    }
}

/// The profile a command runs with, after flags and environment.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub name: String,
    pub profile: Profile,
    pub secrets: Secrets,
    /// The credential came from the environment: refreshed tokens and
    /// cookies are not written anywhere.
    pub ephemeral: bool,
}

/// Picks the profile: the flag, `SN_PROFILE`, the default profile,
/// the only profile. `SN_INSTANCE` and the credential variables
/// build one without a configuration file.
pub fn resolve(store: &Store, flag: Option<&str>) -> Result<Resolved> {
    let env = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
    let config = store.load()?;
    let wanted = flag.map(str::to_owned).or_else(|| env("SN_PROFILE"));
    let name = wanted
        .clone()
        .or_else(|| config.default_profile.clone())
        .or_else(|| (config.profiles.len() == 1).then(|| config.profiles.keys().next().cloned())?)
        .unwrap_or_else(|| DEFAULT_PROFILE.to_owned());
    let stored = config.profiles.get(&name).cloned();
    if stored.is_none() && wanted.is_some() && env("SN_INSTANCE").is_none() {
        let names: Vec<String> = config.profiles.keys().cloned().collect();
        return Err(Error::Input {
            arg: "profile".into(),
            message: format!(
                "you selected profile `{name}`, which does not exist; pass one of: {}, or create it with `sn auth login --profile {name} --instance NAME`",
                if names.is_empty() {
                    "(no profiles yet)".to_owned()
                } else {
                    names.join(", ")
                }
            ),
            choices: names.iter().map(|n| crate::Choice::new(n, n)).collect(),
        });
    }
    let instance = env("SN_INSTANCE")
        .map(|v| normalize_instance(&v))
        .transpose()?;
    let credential = env_credential(&env);
    let mut resolved = match (stored, instance, credential) {
        // A credential from the environment replaces the stored one.
        (stored, instance, Some((auth, username, secrets, oauth))) => {
            let base = instance
                .or_else(|| stored.as_ref().map(|p| p.instance.clone()))
                .ok_or_else(|| {
                    Error::invalid(
                        "a SN_* credential is set but no instance; set SN_INSTANCE to your instance name, for example acme",
                    )
                })?;
            let stored = stored.unwrap_or_default();
            Resolved {
                name,
                profile: Profile {
                    instance: base,
                    auth,
                    username,
                    oauth,
                    ..stored
                },
                secrets,
                ephemeral: true,
            }
        }
        (Some(profile), instance, None) => {
            // A stored credential only ever goes to its own instance.
            if let Some(instance) = instance.filter(|i| *i != profile.instance) {
                return Err(Error::invalid(format!(
                    "SN_INSTANCE is {instance}, but profile `{name}` belongs to {}; unset SN_INSTANCE, choose another --profile, or set SN_USERNAME and SN_PASSWORD (or SN_TOKEN) too",
                    profile.instance
                )));
            }
            let secrets = store.secrets(&name, &profile)?;
            Resolved {
                name,
                profile,
                secrets,
                ephemeral: false,
            }
        }
        (None, instance, None) => {
            let target = instance.unwrap_or_else(|| "NAME".to_owned());
            return Err(Error::Auth(format!(
                "you are not signed in to ServiceNow; run `sn auth login --instance {target}`, or set SN_INSTANCE with SN_USERNAME and SN_PASSWORD, SN_TOKEN, or SN_API_KEY"
            )));
        }
    };
    if let Some(flag) = env("SN_READ_ONLY") {
        resolved.profile.read_only = truthy(&flag);
    }
    let network = &mut resolved.profile.network;
    if let Some(proxy) = env("SN_PROXY") {
        network.proxy = Some(proxy);
    }
    if let Some(list) = env("SN_NO_PROXY") {
        network.no_proxy = Some(list);
    }
    if let Some(path) = env("SN_CA_CERT") {
        network.ca_cert = Some(path.into());
    }
    if let Some(path) = env("SN_CLIENT_CERT") {
        network.client_cert = Some(path.into());
    }
    if env("SN_INSECURE").is_some_and(|v| truthy(&v)) {
        network.insecure = true;
    }
    Ok(resolved)
}

type EnvCredential = (AuthKind, Option<String>, Secrets, Option<OAuthConfig>);

fn env_credential(env: &dyn Fn(&str) -> Option<String>) -> Option<EnvCredential> {
    if let Some(token) = env("SN_TOKEN") {
        let secrets = Secrets {
            token: Some(token),
            ..Secrets::default()
        };
        return Some((AuthKind::Token, None, secrets, None));
    }
    if let Some(key) = env("SN_API_KEY") {
        let secrets = Secrets {
            api_key: Some(key),
            ..Secrets::default()
        };
        return Some((AuthKind::ApiKey, None, secrets, None));
    }
    if let Some(cookie) = env("SN_COOKIE") {
        let secrets = Secrets {
            cookie: Some(cookie),
            user_token: env("SN_USER_TOKEN"),
            ..Secrets::default()
        };
        return Some((AuthKind::Cookie, None, secrets, None));
    }
    if let (Some(id), Some(secret)) = (env("SN_CLIENT_ID"), env("SN_CLIENT_SECRET")) {
        let secrets = Secrets {
            client_secret: Some(secret),
            password: env("SN_PASSWORD"),
            ..Secrets::default()
        };
        let grant = match secrets.password {
            Some(_) => Grant::Password,
            None => Grant::ClientCredentials,
        };
        let oauth = OAuthConfig {
            client_id: id,
            grant,
            ..OAuthConfig::default()
        };
        return Some((AuthKind::Oauth, env("SN_USERNAME"), secrets, Some(oauth)));
    }
    let (user, password) = (env("SN_USERNAME")?, env("SN_PASSWORD")?);
    let secrets = Secrets {
        password: Some(password),
        ..Secrets::default()
    };
    Some((AuthKind::Basic, Some(user), secrets, None))
}

pub fn truthy(text: &str) -> bool {
    !matches!(
        text.trim().to_ascii_lowercase().as_str(),
        "" | "0" | "false" | "no" | "off"
    )
}

/// `acme`, `acme.service-now.com` or a URL, as `https://host[:port]`.
/// Plain HTTP is only accepted for this computer (tests, tunnels).
pub fn normalize_instance(text: &str) -> Result<String> {
    let text = text.trim().trim_end_matches('/');
    let invalid = |why: &str| {
        Error::invalid(format!(
            "you passed the instance `{text}`, which {why}; pass a name (acme), a host (acme.service-now.com), or a URL (https://acme.service-now.com)"
        ))
    };
    if text.is_empty() {
        return Err(invalid("is empty"));
    }
    let full = if text.contains("://") {
        text.to_owned()
    } else if text.contains('.') || text.contains(':') || text == "localhost" {
        format!("https://{text}")
    } else {
        format!("https://{text}.service-now.com")
    };
    let url = Url::parse(&full).map_err(|err| invalid(&format!("is not a host or URL ({err})")))?;
    let host = url.host_str().ok_or_else(|| invalid("has no host"))?;
    let local = matches!(host, "localhost" | "127.0.0.1" | "[::1]");
    match url.scheme() {
        "https" => {}
        "http" if local => {}
        "http" => {
            return Err(invalid(
                "uses http, which would send your credentials in clear text",
            ));
        }
        _ => return Err(invalid("does not use https")),
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(invalid("contains a user name or password"));
    }
    let port = url.port().map(|p| format!(":{p}")).unwrap_or_default();
    Ok(format!("{}://{host}{port}", url.scheme()))
}

fn file_error(what: &str, path: &Path, err: &std::io::Error) -> Error {
    Error::Internal(format!(
        "cannot {what} {} ({err}); check its permissions, or set SN_CONFIG_DIR to a directory you can write",
        path.display()
    ))
}

fn read_toml<T: Default + serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(T::default()),
        Err(err) => return Err(file_error("read", path, &err)),
    };
    toml::from_str(&text).map_err(|err| {
        Error::invalid(format!(
            "{} is not valid ({}); fix the file, or move it away and sign in again",
            path.display(),
            err.message()
        ))
    })
}

/// Writes a new 0600 file and renames it into place.
fn write_toml(path: &Path, value: &impl Serialize) -> Result<()> {
    let text = toml::to_string_pretty(value)
        .map_err(|err| Error::Internal(format!("cannot write {}: {err}", path.display())))?;
    let dir = path.parent().unwrap_or(Path::new("."));
    // tempfile creates the file with mode 0600.
    let mut file =
        tempfile::NamedTempFile::new_in(dir).map_err(|err| file_error("write", path, &err))?;
    file.write_all(text.as_bytes())
        .and_then(|()| file.as_file().sync_all())
        .map_err(|err| file_error("write", path, &err))?;
    file.persist(path)
        .map_err(|err| file_error("write", path, &err.error))?;
    Ok(())
}

/// Runs on its own thread: the Linux backend starts a runtime, which a
/// thread of our own runtime may not do.
fn keychain<T: Send + 'static>(
    name: &str,
    action: impl FnOnce(keyring::Entry) -> Option<T> + Send + 'static,
) -> Option<T> {
    let name = name.to_owned();
    std::thread::spawn(move || action(keyring::Entry::new(KEYCHAIN_SERVICE, &name).ok()?))
        .join()
        .ok()
        .flatten()
}

fn keychain_get(name: &str) -> Option<String> {
    keychain(name, |entry| entry.get_password().ok())
}

fn keychain_set(name: &str, text: &str) -> bool {
    let text = text.to_owned();
    keychain(name, move |entry| entry.set_password(&text).ok()).is_some()
}

fn keychain_delete(name: &str) {
    keychain(name, |entry| entry.delete_credential().ok());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instances_normalize_and_reject_clear_text() {
        for (input, expected) in [
            ("acme", "https://acme.service-now.com"),
            ("acme.service-now.com/", "https://acme.service-now.com"),
            (
                "https://acme.example.com:8443/nav_to.do",
                "https://acme.example.com:8443",
            ),
            ("http://127.0.0.1:9000", "http://127.0.0.1:9000"),
        ] {
            assert_eq!(normalize_instance(input).expect(input), expected);
        }
        for input in [
            "",
            "http://acme.service-now.com",
            "https://u:p@acme.com",
            "ftp://x.com",
        ] {
            assert!(normalize_instance(input).is_err(), "{input}");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_client_waits_for_a_command_that_renews_a_token() {
        let dir = tempfile::tempdir().expect("dir");
        let store = Store::at(dir.path());
        let held = store.lock().expect("lock");
        let release = async {
            tokio::time::sleep(Duration::from_secs(60)).await;
            drop(held);
        };
        let (locked, ()) = tokio::join!(store.lock_async(), release);
        locked.expect("after the other command");
    }

    #[test]
    fn profiles_round_trip_and_files_are_private() {
        let dir = tempfile::tempdir().expect("dir");
        let store = Store::at(dir.path().join("sn"));
        let mut config = ConfigFile::default();
        let profile = Profile {
            instance: "https://acme.service-now.com".into(),
            auth: AuthKind::Oauth,
            oauth: Some(OAuthConfig {
                client_id: "abc".into(),
                ..OAuthConfig::default()
            }),
            store: Some(StoreKind::File),
            network: Network {
                proxy: Some("http://p:1".into()),
                ..Network::default()
            },
            ..Profile::default()
        };
        config.profiles.insert("work".into(), profile.clone());
        store.save(&config).expect("save");
        assert_eq!(store.load().expect("load"), config);
        let secrets = Secrets {
            password: Some("pw".into()),
            ..Secrets::default()
        };
        store
            .save_secrets("work", &profile, &secrets)
            .expect("secrets");
        assert!(store.secrets("work", &profile).expect("read") == secrets);
        assert_eq!(format!("{secrets:?}"), "Secrets(hidden)");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(store.credentials_path())
                .expect("meta")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        store
            .save_secrets("work", &profile, &Secrets::default())
            .expect("clear");
        assert!(store.secrets("work", &profile).expect("read") == Secrets::default());
    }
}

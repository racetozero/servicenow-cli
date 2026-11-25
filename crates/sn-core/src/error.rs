//! One error type with a stable code per failure class.

use serde_json::{Value, json};

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// A value the caller can pass to fix an [`Error::Input`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    pub value: String,
    pub label: String,
}

impl Choice {
    pub fn new(value: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            label: label.into(),
        }
    }
}

/// What the server said about a failed request.
#[derive(Debug, Clone, Default)]
pub struct ApiFailure {
    /// HTTP status; `None` when the failure came inside a successful response.
    pub status: Option<u16>,
    pub message: String,
    pub detail: Option<String>,
    /// The instance's own error payload.
    pub payload: Value,
    pub transaction_id: Option<String>,
}

impl ApiFailure {
    /// The instance has no such API; it answers this with 400, not 404.
    pub fn missing_route(&self) -> bool {
        self.status == Some(400) && self.message.contains("does not represent any resource")
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The input is wrong: flag, argument, body, or configuration.
    #[error("{0}")]
    Invalid(String),
    /// A value is missing or ambiguous; `choices` lists what fits.
    #[error("{message}")]
    Input {
        arg: String,
        message: String,
        choices: Vec<Choice>,
    },
    /// A destructive command ran without `--yes`.
    #[error("{0}")]
    ConfirmationRequired(String),
    /// The profile refuses writes.
    #[error("{0}")]
    ReadOnly(String),
    /// No credential, or the server rejected it (401).
    #[error("{0}")]
    Auth(String),
    #[error("{}", .0.message)]
    Api(Box<ApiFailure>),
    /// The server, or a lookup, found nothing.
    #[error("{0}")]
    NotFound(String),
    /// The request was not sent because of `--dry-run`.
    #[error("dry run")]
    DryRun(Value),
    #[error("{0}")]
    Network(String),
    #[error("{0}")]
    Internal(String),
    /// A failure that carries data for the caller (for example the cart an
    /// order was left in). `code` is one of [`CODES`].
    #[error("{message}")]
    Other {
        code: &'static str,
        message: String,
        data: Value,
    },
}

/// Every value [`Error::code`] returns.
pub const CODES: &[&str] = &[
    "invalid",
    "input_required",
    "confirmation_required",
    "read_only",
    "auth",
    "permission",
    "api",
    "conflict",
    "rate_limited",
    "not_found",
    "not_ordered",
    "check_failed",
    "script_error",
    "timeout",
    "dry_run",
    "network",
    "internal",
];

impl Error {
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::Invalid(message.into())
    }

    /// A failure the instance reported inside a successful response.
    pub fn api(message: impl Into<String>, payload: Value) -> Self {
        Self::Api(Box::new(ApiFailure {
            message: message.into(),
            payload,
            ..ApiFailure::default()
        }))
    }

    pub fn status(&self) -> Option<u16> {
        match self {
            Self::Api(failure) => failure.status,
            _ => None,
        }
    }

    pub fn code(&self) -> &'static str {
        match self {
            Self::Invalid(_) => "invalid",
            Self::Input { .. } => "input_required",
            Self::ConfirmationRequired(_) => "confirmation_required",
            Self::ReadOnly(_) => "read_only",
            Self::Auth(_) => "auth",
            Self::Api(failure) => match failure.status {
                Some(401) => "auth",
                Some(403) => "permission",
                Some(404) => "not_found",
                Some(409 | 412) => "conflict",
                Some(429) => "rate_limited",
                _ => "api",
            },
            Self::NotFound(_) => "not_found",
            Self::DryRun(_) => "dry_run",
            Self::Network(_) => "network",
            Self::Internal(_) => "internal",
            Self::Other { code, .. } => code,
        }
    }

    /// The process exit code: 0 ok, 1 API, 2 auth, 3 invalid input, 4 not
    /// found, 5 other.
    pub fn exit_code(&self) -> u8 {
        match self.code() {
            "dry_run" => 0,
            "api" | "conflict" | "rate_limited" | "not_ordered" | "check_failed"
            | "script_error" => 1,
            "auth" | "permission" => 2,
            "invalid" | "input_required" | "confirmation_required" | "read_only" => 3,
            "not_found" => 4,
            _ => 5,
        }
    }

    /// The error as the agent envelope and stderr JSON carry it.
    pub fn to_json(&self) -> Value {
        let mut out = json!({ "code": self.code(), "message": self.to_string() });
        match self {
            Self::Input { arg, choices, .. } => {
                let choices: Vec<Value> = choices
                    .iter()
                    .map(|c| json!({ "value": c.value, "label": c.label }))
                    .collect();
                out["input"] = json!({ "arg": arg, "choices": choices });
            }
            Self::Api(failure) => {
                if let Some(status) = failure.status {
                    out["status"] = status.into();
                }
                if let Some(detail) = &failure.detail {
                    out["detail"] = detail.clone().into();
                }
                if let Some(id) = &failure.transaction_id {
                    out["transaction_id"] = id.clone().into();
                }
                if !failure.payload.is_null() {
                    out["payload"] = failure.payload.clone();
                }
            }
            Self::DryRun(request) => out["request"] = request.clone(),
            Self::Other { data, .. } if !data.is_null() => out["data"] = data.clone(),
            _ => {}
        }
        out
    }
}

impl From<serde_json::Error> for Error {
    fn from(err: serde_json::Error) -> Self {
        Self::Internal(format!("cannot convert JSON: {err}"))
    }
}

impl From<reqwest::Error> for Error {
    fn from(err: reqwest::Error) -> Self {
        // The innermost cause says what went wrong ("certificate not trusted").
        let mut text = err.to_string();
        let mut source = std::error::Error::source(&err);
        while let Some(inner) = source {
            let inner_text = inner.to_string();
            if !text.contains(&inner_text) {
                text = format!("{text}: {inner_text}");
            }
            source = inner.source();
        }
        if err.is_timeout() {
            return Self::Other {
                code: "timeout",
                message: format!(
                    "the request timed out ({text}); raise the limit with --timeout SECONDS"
                ),
                data: Value::Null,
            };
        }
        let hint = if text.contains("certificate") || text.contains("UnknownIssuer") {
            "; the server certificate is not trusted: install its root in the operating system certificate store, or pass --ca-cert FILE"
        } else if err.is_connect() {
            "; check the instance name, your network, and --proxy"
        } else {
            ""
        };
        Self::Network(format!("{text}{hint}"))
    }
}

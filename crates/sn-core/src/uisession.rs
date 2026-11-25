//! Sessions of the web interface: signing in through the login form (local
//! accounts, MFA) and opening a session from a profile for the endpoints
//! that only accept cookies.

use crate::client::{Client, Method, Request, Response, cookie_header, without_envelope};
use crate::config::AuthKind;
use crate::error::{Error, Result};
use serde_json::Value;
use std::collections::BTreeMap;

/// Answers with the caller's session token and sets the session cookies.
pub const SESSION_PATH: &str = "/api/now/sg/impersonation/session";

/// Whose session it is, as [`SESSION_PATH`] answers. Never show the token.
#[derive(Clone, Default)]
pub struct SessionInfo {
    pub current_user: Option<String>,
    /// Who impersonates `current_user`.
    pub original_user: Option<String>,
    pub can_impersonate: Option<bool>,
    pub admin: Option<bool>,
    pub token: Option<String>,
}

impl SessionInfo {
    pub fn read(answer: Value) -> Self {
        let body = without_envelope(answer);
        let text = |name: &str| {
            let text = body[name].as_str()?.trim();
            (!text.is_empty()).then(|| text.to_owned())
        };
        Self {
            current_user: text("CurrentUser"),
            original_user: text("OriginalUser"),
            can_impersonate: body["CanImpersonate"].as_bool(),
            admin: body["admin"].as_bool(),
            token: text("SessionToken"),
        }
    }
}

/// Cookies of one session plus its anti-forgery token (`g_ck`).
#[derive(Clone, Default)]
pub struct UiSession {
    pub cookies: BTreeMap<String, String>,
    pub user_token: String,
}

impl UiSession {
    /// Opens a session with the profile's credential.
    pub async fn mint(client: &Client) -> Result<Self> {
        let response = client.send(Request::new(Method::GET, SESSION_PATH)).await?;
        let mut session = Self::opened_by(&response)?;
        session.user_token = SessionInfo::read(response.json()?)
            .token
            .unwrap_or_default();
        Ok(session)
    }

    /// The session whose cookies a response set.
    pub fn opened_by(response: &Response) -> Result<Self> {
        let mut session = Self::default();
        session.absorb(response);
        if !session.cookies.contains_key("JSESSIONID") {
            return Err(Error::Auth(
                "the instance opened no session for this credential, and this command needs one; sign in with another method (`sn auth login`)".into(),
            ));
        }
        Ok(session)
    }

    /// The session of a cookie profile, or a new one for any other profile.
    pub async fn open(client: &Client) -> Result<Self> {
        if !matches!(client.auth_kind(), AuthKind::Cookie | AuthKind::Session) {
            return Self::mint(client).await;
        }
        let response = client.send(Request::new(Method::GET, SESSION_PATH)).await?;
        let mut session = client.ui_session().await;
        if let Some(token) = SessionInfo::read(response.json()?).token {
            session.user_token = token;
        }
        Ok(session)
    }

    /// Adds this session to a request and keeps the profile's credential:
    /// pages of the web interface accept the cookies of a new session alone
    /// only after it has loaded a page.
    pub fn attach(&self, client: &Client, request: Request) -> Request {
        match client.auth_kind() {
            AuthKind::Cookie | AuthKind::Session => self.apply(request),
            _ => request
                .header("cookie", self.cookie_header())
                .header("x-usertoken", self.user_token.clone()),
        }
    }

    pub fn cookie_header(&self) -> String {
        cookie_header(&self.cookies)
    }

    /// Takes over cookies that a response set or deleted.
    pub fn absorb(&mut self, response: &Response) {
        for (name, value) in response.cookies() {
            match value.is_empty() {
                true => self.cookies.remove(&name),
                false => self.cookies.insert(name, value),
            };
        }
    }

    /// Adds this session to a request, in place of the profile's credential.
    pub fn apply(&self, request: Request) -> Request {
        let request = request.no_auth().header("cookie", self.cookie_header());
        match self.user_token.is_empty() {
            true => request,
            false => request.header("x-usertoken", self.user_token.clone()),
        }
    }

    pub async fn send(&mut self, client: &Client, request: Request) -> Result<Response> {
        let response = client.send(self.apply(request)).await?;
        self.absorb(&response);
        Ok(response)
    }

    /// Signs in through the login form. `mfa` is asked for a six-digit code
    /// when the account needs one.
    pub async fn login(
        client: &Client,
        user: &str,
        password: &str,
        mut mfa: impl AsyncFnMut() -> Result<String>,
    ) -> Result<Self> {
        let mut session = Self::default();
        let form = vec![
            ("sysparm_type".to_owned(), "login".to_owned()),
            ("ni.nolog.user_password".to_owned(), "true".to_owned()),
            ("user_name".to_owned(), user.to_owned()),
            ("user_password".to_owned(), password.to_owned()),
        ];
        let response = client
            .post_form_plain("/angular.do?sysparm_type=view_form.login", &form, "")
            .await?;
        session.absorb(&response);
        let body: Value = serde_json::from_slice(&response.body).unwrap_or(Value::Null);
        match body["status"].as_str() {
            Some("success") => {}
            Some("mfa_code_required") => session.verify_mfa(client, &mut mfa).await?,
            _ => {
                let why = body["message"]
                    .as_str()
                    .unwrap_or("the login form did not accept them");
                return Err(Error::Auth(format!(
                    "the instance rejected the user name or password: {why}; check both, or use `sn auth login --method sso` when your company signs in through SSO"
                )));
            }
        }
        // The instance may answer 401 here and still send the token.
        let response = client
            .post_form_plain(
                "/angular.do?sysparm_type=get_user",
                &[],
                &session.cookie_header(),
            )
            .await?;
        session.absorb(&response);
        session.user_token = response
            .header("x-usertoken-response")
            .unwrap_or_default()
            .to_owned();
        Ok(session)
    }

    async fn verify_mfa(
        &mut self,
        client: &Client,
        mfa: &mut impl AsyncFnMut() -> Result<String>,
    ) -> Result<()> {
        for _ in 0..3 {
            let page = client
                .send(
                    Request::new(Method::GET, "/validate_multifactor_auth_code.do")
                        .no_auth()
                        .header("cookie", self.cookie_header()),
                )
                .await?;
            self.absorb(&page);
            let html = page.text();
            let (Some(ck), Some(nonce)) = (
                hidden_field(&html, "sysparm_ck"),
                hidden_field(&html, "sysparm_process_nonce"),
            ) else {
                return Err(Error::Auth(
                    "the instance asked for an MFA code but its form is not readable; sign in with `sn auth login --method sso`".into(),
                ));
            };
            let code = mfa().await?;
            let fields = [
                ("sysparm_ck", ck.as_str()),
                ("sysparm_process_nonce", nonce.as_str()),
                ("sys_action", "sysverb_validate_mfa_code"),
                ("sys_mfa_check_remembered_browser", "false"),
                ("bfp", ""),
                ("bfp_hash", ""),
                ("sys_web_authentication_successful", "false"),
                ("sys_web_authentication_response", ""),
                ("sys_web_authn_registration_successful", "false"),
                ("sys_web_authn_registration_skipped", "false"),
                ("sys_mfa_factor_validate", "TOTP"),
                ("sys_show_works_with_sn_enabled", "true"),
                ("mfa_factor", "mfa_TOTP_div"),
                ("txtResponse", code.trim()),
                ("txtResponse_email", ""),
                ("txtResponse_sms", ""),
            ];
            let fields: Vec<(String, String)> = fields
                .iter()
                .map(|(n, v)| ((*n).to_owned(), (*v).to_owned()))
                .collect();
            let response = client
                .post_form_plain("/validate_mfa_code.do", &fields, &self.cookie_header())
                .await?;
            self.absorb(&response);
            // Success is a redirect; a wrong code answers 200 and `invalid`.
            if response.status == 302 && response.header("x-mfa_token") != Some("invalid") {
                return Ok(());
            }
        }
        Err(Error::Auth("the instance rejected the MFA code three times; check the clock of this computer and sign in again".into()))
    }
}

/// The value of a hidden input, whatever the order of its attributes.
fn hidden_field(html: &str, name: &str) -> Option<String> {
    html.split('<')
        .filter(|tag| tag.starts_with("input"))
        .find_map(|tag| {
            let attribute = |wanted: &str| {
                let at = tag.find(&format!("{wanted}="))? + wanted.len() + 1;
                let quote = tag[at..]
                    .chars()
                    .next()
                    .filter(|c| matches!(c, '"' | '\''))?;
                let rest = &tag[at + 1..];
                Some(&rest[..rest.find(quote)?])
            };
            (attribute("name") == Some(name)).then(|| attribute("value").map(str::to_owned))?
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidden_fields_and_tokens() {
        let html =
            r#"<form><input value='v1' name='sysparm_ck' type="hidden"><input name="x" value="y">"#;
        assert_eq!(hidden_field(html, "sysparm_ck").as_deref(), Some("v1"));
        assert_eq!(hidden_field(html, "missing"), None);
        let body = serde_json::json!({ "result": { "a": 1, "SessionToken": "tok" } });
        assert_eq!(SessionInfo::read(body).token.as_deref(), Some("tok"));
    }
}

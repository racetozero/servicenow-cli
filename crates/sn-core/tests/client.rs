//! Retries, renewed credentials, and what a long-lived client writes back.

use serde_json::json;
use sn_core::client::Options;
use sn_core::config::{AuthKind, Grant, OAuthConfig, Profile, Resolved, Secrets, Store};
use sn_core::{Client, Error, refs};
use std::time::Duration;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const INCIDENTS: &str = "/api/now/table/incident";

struct Setup {
    server: MockServer,
    store: Store,
    resolved: Resolved,
    _dir: tempfile::TempDir,
}

impl Setup {
    /// A saved profile of a mock instance.
    async fn new(profile: Profile, secrets: Secrets) -> Self {
        let server = MockServer::start().await;
        let dir = tempfile::tempdir().expect("dir");
        let store = Store::at(dir.path());
        let profile = Profile {
            instance: server.uri(),
            store: Some(sn_core::config::StoreKind::File),
            ..profile
        };
        let resolved = Resolved {
            name: "work".into(),
            profile,
            secrets,
            ephemeral: false,
        };
        let setup = Self {
            server,
            store,
            resolved,
            _dir: dir,
        };
        setup.save(&setup.resolved.secrets);
        setup
    }

    fn save(&self, secrets: &Secrets) {
        self.store
            .save_secrets("work", &self.resolved.profile, secrets)
            .expect("save");
    }

    fn saved(&self) -> Secrets {
        self.store
            .secrets("work", &self.resolved.profile)
            .expect("saved")
    }

    fn client(&self) -> Client {
        self.client_with(Options::default())
    }

    fn client_with(&self, options: Options) -> Client {
        Client::new(self.resolved.clone(), Some(self.store.clone()), options).expect("client")
    }

    async fn on(&self, verb: &str, at: &str, answer: ResponseTemplate, times: u64) {
        Mock::given(method(verb))
            .and(path(at))
            .respond_with(answer)
            .expect(times)
            .mount(&self.server)
            .await;
    }

    async fn on_token(&self, token: &str, answer: ResponseTemplate) {
        Mock::given(header("authorization", format!("Bearer {token}").as_str()))
            .respond_with(answer)
            .mount(&self.server)
            .await;
    }
}

fn oauth() -> (Profile, Secrets) {
    let profile = Profile {
        auth: AuthKind::Oauth,
        oauth: Some(OAuthConfig {
            client_id: "id".into(),
            grant: Grant::AuthorizationCode,
            ..OAuthConfig::default()
        }),
        ..Profile::default()
    };
    let secrets = Secrets {
        access_token: Some("old".into()),
        refresh_token: Some("r1".into()),
        ..Secrets::default()
    };
    (profile, secrets)
}

fn rows() -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({ "result": [] }))
}

fn new_token() -> ResponseTemplate {
    ResponseTemplate::new(200)
        .set_body_json(json!({ "access_token": "new", "refresh_token": "r2", "expires_in": 1800 }))
}

#[tokio::test]
async fn a_request_that_timed_out_is_not_sent_again() {
    let (profile, secrets) = oauth();
    let setup = Setup::new(profile, secrets).await;
    setup
        .on(
            "GET",
            INCIDENTS,
            rows().set_delay(Duration::from_secs(5)),
            1,
        )
        .await;
    let client = setup.client_with(Options {
        timeout: Some(Duration::from_secs(1)),
        ..Options::default()
    });
    let err = client.get(INCIDENTS).await.expect_err("timeout");
    assert_eq!(err.code(), "timeout", "{err}");
}

#[tokio::test]
async fn requests_with_one_rejected_token_renew_it_once() {
    let (profile, secrets) = oauth();
    let setup = Setup::new(profile, secrets).await;
    setup.on("POST", "/oauth_token.do", new_token(), 1).await;
    setup.on_token("old", ResponseTemplate::new(401)).await;
    setup.on_token("new", rows()).await;
    let client = setup.client();
    let (a, b, c) = tokio::join!(
        client.get(INCIDENTS),
        client.get(INCIDENTS),
        client.get(INCIDENTS)
    );
    for answer in [a, b, c] {
        answer.expect("renewed");
    }
    assert_eq!(setup.saved().refresh_token.as_deref(), Some("r2"));
}

#[tokio::test]
async fn a_retry_after_a_renewal_keeps_the_new_token() {
    let (profile, secrets) = oauth();
    let setup = Setup::new(profile, secrets).await;
    setup.on("POST", "/oauth_token.do", new_token(), 1).await;
    setup.on_token("old", ResponseTemplate::new(401)).await;
    Mock::given(header("authorization", "Bearer new"))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(1)
        .mount(&setup.server)
        .await;
    setup.on_token("new", rows()).await;
    setup.client().get(INCIDENTS).await.expect("retried");
}

#[tokio::test]
async fn a_token_endpoint_that_is_down_is_not_an_expired_sign_in() {
    for (status, code) in [(503, "api"), (429, "rate_limited"), (400, "auth")] {
        let (profile, mut secrets) = oauth();
        secrets.expires_at = Some(1);
        let setup = Setup::new(profile, secrets).await;
        let answer = ResponseTemplate::new(status).set_body_json(json!({ "error": "x" }));
        setup.on("POST", "/oauth_token.do", answer, 1).await;
        let err = setup.client().get(INCIDENTS).await.expect_err("refused");
        assert_eq!(err.code(), code, "{status}: {err}");
    }
}

#[tokio::test]
async fn a_renewal_does_not_undo_a_logout() {
    let (profile, secrets) = oauth();
    let setup = Setup::new(profile, secrets).await;
    setup.on("POST", "/oauth_token.do", new_token(), 0).await;
    setup.on_token("old", ResponseTemplate::new(401)).await;
    let client = setup.client();
    setup.save(&Secrets::default());
    let err = client.get(INCIDENTS).await.expect_err("signed out");
    assert!(matches!(err, Error::Auth(_)), "{err}");
    assert!(setup.saved() == Secrets::default());
}

#[tokio::test]
async fn rotated_cookies_replace_only_the_session_they_come_from() {
    let session = |cookie: &str| Secrets {
        cookie: Some(cookie.into()),
        user_token: Some(format!("token of {cookie}")),
        ..Secrets::default()
    };
    for (meanwhile, kept) in [
        (None, session("a=2")),
        (Some(session("b=1")), session("b=1")),
        (Some(Secrets::default()), Secrets::default()),
    ] {
        let profile = Profile {
            auth: AuthKind::Session,
            ..Profile::default()
        };
        let setup = Setup::new(profile, session("a=1")).await;
        let answer = rows().insert_header("set-cookie", "a=2; Path=/");
        setup.on("GET", INCIDENTS, answer, 1).await;
        let client = setup.client();
        client.get(INCIDENTS).await.expect("rows");
        if let Some(secrets) = &meanwhile {
            setup.save(secrets);
        }
        client.finish().await.expect("finish");
        assert_eq!(setup.saved().cookie, kept.cookie);
        if meanwhile.is_some() {
            assert!(setup.saved() == kept);
        }
    }
}

#[tokio::test]
async fn a_static_token_cannot_be_renewed() {
    let profile = Profile {
        auth: AuthKind::Token,
        ..Profile::default()
    };
    let secrets = Secrets {
        token: Some("t".into()),
        ..Secrets::default()
    };
    let setup = Setup::new(profile, secrets).await;
    let client = setup.client();
    assert_eq!(client.bearer(true).await.expect("no error"), None);
    assert_eq!(
        client.bearer(false).await.expect("token").as_deref(),
        Some("t")
    );
}

#[tokio::test]
async fn naming_yourself_takes_one_request() {
    const ME: &str = "6816f79cc0a8016401c5a33be04be441";
    let (profile, secrets) = oauth();
    let setup = Setup::new(profile, secrets).await;
    let answer =
        ResponseTemplate::new(200).set_body_json(json!({ "result": { "user_sys_id": ME } }));
    setup
        .on("GET", "/api/now/ui/user/current_user", answer, 1)
        .await;
    let me = refs::person(&setup.client(), refs::Person::User, "@me").await;
    assert_eq!(me.expect("me"), ME);
}

#[tokio::test]
async fn a_renewal_outlives_the_client() {
    let (profile, secrets) = oauth();
    let setup = Setup::new(profile, secrets).await;
    setup.on("POST", "/oauth_token.do", new_token(), 1).await;
    setup.on_token("old", ResponseTemplate::new(401)).await;
    setup.on_token("new", rows()).await;
    let client = Client::new(setup.resolved.clone(), None, Options::default()).expect("client");
    client.get(INCIDENTS).await.expect("renewed");
    let secrets = client.into_secrets();
    assert_eq!(secrets.access_token.as_deref(), Some("new"));
    assert_eq!(secrets.refresh_token.as_deref(), Some("r2"));
}

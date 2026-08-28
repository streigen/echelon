//! Unit suite for [`crate::commands::auth`].
//!
//! The password flows run end to end against a mock homeserver: the command
//! authenticates, persists the session and the account, builds the real
//! per-account SQLite store under a temp directory, and swaps the handler into
//! the client state.
//!
//! The OAuth flows are covered only as far as their argument guards. Completing
//! one calls `open::that`, which launches the machine's browser, and then blocks
//! on a redirect that a test has no way to deliver.

use super::*;
use crate::test_support::*;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, ResponseTemplate};

/// The account the mock homeserver signs everyone in as.
///
/// Fixed by the prebuilt login response rather than chosen here, which is the
/// point of reading the identity off the response instead of off what the user
/// typed: the two need not match.
const USER_ID: &str = "@cheeky_monkey:matrix.org";

/// The device the mock homeserver issues, likewise fixed by that response.
const DEVICE_ID: &str = "GHTYAJCE";

/// Whether a session is currently held in the state.
///
/// # Arguments
/// * `state` - The state to inspect.
async fn is_signed_in(state: &ClientState) -> bool {
    state.read().await.is_some()
}

/// The user id of the session held in the state.
///
/// # Arguments
/// * `state` - The state to inspect.
async fn signed_in_as(state: &ClientState) -> Option<String> {
    state
        .read()
        .await
        .as_ref()
        .and_then(|handler| handler.get_client().user_id().map(ToString::to_string))
}

/// Mock the endpoints a successful password sign-in needs.
///
/// Sync is mocked because the command starts the sync loop before returning; a
/// loop hitting an unmocked endpoint would spin and log rather than fail, but
/// answering it keeps the test output clean.
///
/// # Arguments
/// * `session` - The session whose server to mount on.
async fn mock_sign_in(session: &MockSession) {
    session.server.mock_versions().ok().named("versions").mount().await;
    session.server.mock_login().ok().mount().await;
    session.server.mock_sync().ok_and_run(&session.client, |_| {}).await;
}

/// Mount a `/register` endpoint returning `response` for each call in turn.
///
/// Hand-rolled because the mock server has no prebuilt registration endpoint,
/// and the challenge-response case needs two different answers in sequence.
///
/// # Arguments
/// * `session` - The session whose server to mount on.
/// * `responses` - The responses to give, in order.
async fn mock_register(session: &MockSession, responses: Vec<ResponseTemplate>) {
    for response in responses {
        Mock::given(method("POST"))
            .and(path("/_matrix/client/v3/register"))
            .respond_with(response)
            .up_to_n_times(1)
            .mount(&session.server)
            .await;
    }
}

/// The body a homeserver returns once an account exists and is signed in.
///
/// Matches the identity the login mock reports, so both flows land on the same
/// account and the assertions can share one constant.
fn registered_body() -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "user_id": USER_ID,
        "device_id": DEVICE_ID,
        "access_token": "abc123",
    }))
}

/// The 401 a homeserver returns to ask for a registration token.
fn needs_token_body() -> ResponseTemplate {
    ResponseTemplate::new(401).set_body_json(json!({
        "session": "uiaa-session",
        "flows": [{"stages": ["m.login.registration_token"]}],
        "params": {},
    }))
}

mod guards {
    use super::*;

    #[tokio::test]
    async fn login_requires_a_username_and_password() {
        for (username, password) in [("", "pw"), ("alice", ""), ("  ", "pw"), ("alice", "  ")] {
            let outcome = login(
                username.to_owned(),
                password.to_owned(),
                "https://example.org".to_owned(),
                empty_state(),
            )
            .await
            .expect_err("incomplete credentials should be refused");

            assert_eq!(outcome, "username and password are required");
        }
    }

    #[tokio::test]
    async fn register_requires_a_username_and_password() {
        let outcome = register(
            String::new(),
            String::new(),
            "https://example.org".to_owned(),
            None,
            empty_state(),
        )
        .await
        .expect_err("incomplete credentials should be refused");

        assert_eq!(outcome, "username and password are required");
    }

    #[tokio::test]
    async fn restore_session_requires_a_user_id() {
        for user_id in ["", "   "] {
            let outcome = restore_session(user_id.to_owned(), None, empty_state())
                .await
                .expect_err("a blank user id should be refused");

            assert_eq!(outcome, "user id is required");
        }
    }

    #[tokio::test]
    async fn the_oauth_flows_require_a_homeserver() {
        for homeserver in ["", "   "] {
            assert_eq!(
                oauth_login(homeserver.to_owned(), empty_state())
                    .await
                    .expect_err("a blank homeserver should be refused"),
                "homeserver is required"
            );
            assert_eq!(
                oauth_register(homeserver.to_owned(), empty_state())
                    .await
                    .expect_err("a blank homeserver should be refused"),
                "homeserver is required"
            );
        }
    }

    #[tokio::test]
    async fn every_command_needs_a_session_to_work_through() {
        // Even signing in runs through the existing handler, since that is what
        // owns the store the new session will be written to.
        assert_eq!(
            login(
                "alice".to_owned(),
                "hunter2".to_owned(),
                "https://example.org".to_owned(),
                empty_state()
            )
            .await
            .expect_err("there is no handler"),
            "No active client session"
        );
        assert_eq!(
            oauth_login("https://example.org".to_owned(), empty_state())
                .await
                .expect_err("there is no handler"),
            "No active client session"
        );
        assert_eq!(
            logout(empty_state())
                .await
                .expect_err("there is no handler"),
            "No active client session"
        );
        assert_eq!(
            restore_session(USER_ID.to_owned(), None, empty_state())
                .await
                .expect_err("there is no handler"),
            "No active client session"
        );
        assert_eq!(
            list_accounts(empty_state())
                .await
                .expect_err("there is no handler"),
            "No active client session"
        );
    }
}

mod login {
    use super::*;

    #[tokio::test]
    async fn signs_in_and_swaps_the_handler() {
        let session = mock_session("auth-login").await;
        mock_sign_in(&session).await;

        let outcome = super::super::login(
            "alice".to_owned(),
            "hunter2".to_owned(),
            session.server.uri(),
            session.state.clone(),
        )
        .await
        .expect("signing in should succeed");

        assert_eq!(outcome, "logged in");
        assert_eq!(signed_in_as(&session.state).await.as_deref(), Some(USER_ID));
    }

    #[tokio::test]
    async fn records_the_account_and_its_homeserver() {
        let session = mock_session("auth-login-account").await;
        mock_sign_in(&session).await;

        super::super::login(
            "alice".to_owned(),
            "hunter2".to_owned(),
            session.server.uri(),
            session.state.clone(),
        )
        .await
        .expect("signing in should succeed");

        let account = session
            .app_state
            .echelon_store
            .get_account(USER_ID)
            .expect("reading the store should succeed")
            .expect("the account was just added");
        assert_eq!(account.homeserver.as_deref(), Some(session.server.uri().as_str()));
    }

    #[tokio::test]
    async fn stores_the_session_for_a_later_restore() {
        let session = mock_session("auth-login-secrets").await;
        mock_sign_in(&session).await;

        super::super::login(
            "alice".to_owned(),
            "hunter2".to_owned(),
            session.server.uri(),
            session.state.clone(),
        )
        .await
        .expect("signing in should succeed");

        let stored = session
            .app_state
            .secret_service
            .get_session(USER_ID)
            .expect("reading secrets should succeed")
            .expect("the session was just stored");
        assert_eq!(stored.device_id, DEVICE_ID);
        assert!(!stored.access_token.is_empty());
    }

    #[tokio::test]
    async fn reports_a_rejected_password_and_keeps_the_old_handler() {
        let session = mock_session("auth-login-rejected").await;
        session.server.mock_versions().ok().mount().await;
        Mock::given(method("POST"))
            .and(path("/_matrix/client/v3/login"))
            .respond_with(ResponseTemplate::new(403).set_body_json(json!({
                "errcode": "M_FORBIDDEN",
                "error": "Invalid password",
            })))
            .mount(&session.server)
            .await;

        let outcome = super::super::login(
            "alice".to_owned(),
            "wrong".to_owned(),
            session.server.uri(),
            session.state.clone(),
        )
        .await
        .expect_err("the password was rejected");

        assert!(outcome.starts_with("Login failed: "), "got: {outcome}");
        // A failed sign-in must not sign the user out of what they had.
        assert!(is_signed_in(&session.state).await);
    }
}

mod register {
    use super::*;

    #[tokio::test]
    async fn creates_an_account_and_signs_in() {
        let session = mock_session("auth-register").await;
        session.server.mock_versions().ok().mount().await;
        session.server.mock_sync().ok_and_run(&session.client, |_| {}).await;
        mock_register(&session, vec![registered_body()]).await;

        let outcome = super::super::register(
            "alice".to_owned(),
            "hunter2".to_owned(),
            session.server.uri(),
            None,
            session.state.clone(),
        )
        .await
        .expect("registration should succeed");

        assert_eq!(outcome, "registered");
        assert_eq!(signed_in_as(&session.state).await.as_deref(), Some(USER_ID));
    }

    #[tokio::test]
    async fn replays_the_request_with_the_servers_challenge_session() {
        // A homeserver that gates registration answers the first attempt with a
        // 401 naming a UIAA session. The token has to go back with that session
        // attached, or the server treats it as a fresh unauthenticated attempt.
        let session = mock_session("auth-register-uiaa").await;
        session.server.mock_versions().ok().mount().await;
        session.server.mock_sync().ok_and_run(&session.client, |_| {}).await;
        mock_register(&session, vec![needs_token_body(), registered_body()]).await;

        let outcome = super::super::register(
            "alice".to_owned(),
            "hunter2".to_owned(),
            session.server.uri(),
            Some("secret-token".to_owned()),
            session.state.clone(),
        )
        .await
        .expect("registration should succeed after the challenge");

        assert_eq!(outcome, "registered");
    }

    #[tokio::test]
    async fn says_a_token_is_needed_when_the_server_asks_and_none_was_given() {
        let session = mock_session("auth-register-no-token").await;
        session.server.mock_versions().ok().mount().await;
        mock_register(&session, vec![needs_token_body()]).await;

        let outcome = super::super::register(
            "alice".to_owned(),
            "hunter2".to_owned(),
            session.server.uri(),
            None,
            session.state.clone(),
        )
        .await
        .expect_err("the server wants a token that was not supplied");

        assert!(
            outcome.contains("Registration token required"),
            "got: {outcome}"
        );
    }

    #[tokio::test]
    async fn reports_a_refused_registration() {
        let session = mock_session("auth-register-refused").await;
        session.server.mock_versions().ok().mount().await;
        mock_register(
            &session,
            vec![ResponseTemplate::new(400).set_body_json(json!({
                "errcode": "M_USER_IN_USE",
                "error": "That username is taken",
            }))],
        )
        .await;

        let outcome = super::super::register(
            "alice".to_owned(),
            "hunter2".to_owned(),
            session.server.uri(),
            None,
            session.state.clone(),
        )
        .await
        .expect_err("the username is taken");

        assert!(outcome.starts_with("Registration failed"), "got: {outcome}");
    }
}

mod logout {
    use super::*;

    /// Sign in through the mock server, leaving a session to be logged out of.
    ///
    /// # Arguments
    /// * `name` - A label unique to the calling test.
    async fn signed_in(name: &str) -> MockSession {
        let session = mock_session(name).await;
        mock_sign_in(&session).await;
        super::super::login(
            "alice".to_owned(),
            "hunter2".to_owned(),
            session.server.uri(),
            session.state.clone(),
        )
        .await
        .expect("signing in should succeed");
        session
    }

    #[tokio::test]
    async fn clears_the_client_state() {
        let session = signed_in("auth-logout").await;
        session.server.mock_logout().ok().mount().await;

        let outcome = super::super::logout(session.state.clone())
            .await
            .expect("logging out should succeed");

        assert_eq!(outcome, "logged out");
        assert!(!is_signed_in(&session.state).await);
    }

    #[tokio::test]
    async fn forgets_the_account_and_its_secrets() {
        let session = signed_in("auth-logout-cleanup").await;
        session.server.mock_logout().ok().mount().await;

        super::super::logout(session.state.clone())
            .await
            .expect("logging out should succeed");

        assert!(
            session
                .app_state
                .echelon_store
                .get_account(USER_ID)
                .expect("reading the store should succeed")
                .is_none()
        );
        assert!(
            session
                .app_state
                .secret_service
                .get_session(USER_ID)
                .expect("reading secrets should succeed")
                .is_none()
        );
    }

    #[tokio::test]
    async fn finishes_even_when_the_homeserver_refuses_to_revoke() {
        // A server that is unreachable or refuses the revocation must not strand
        // the user in a session the app will not let them leave. The local
        // cleanup happens either way.
        let session = signed_in("auth-logout-server-error").await;
        session.server.mock_logout().error500().mount().await;

        let outcome = super::super::logout(session.state.clone())
            .await
            .expect("local logout should succeed regardless");

        assert_eq!(outcome, "logged out");
        assert!(!is_signed_in(&session.state).await);
        assert!(
            session
                .app_state
                .secret_service
                .get_session(USER_ID)
                .expect("reading secrets should succeed")
                .is_none()
        );
    }
}

mod restore_session {
    use super::*;

    #[tokio::test]
    async fn restores_a_session_that_was_stored_earlier() {
        let session = mock_session("auth-restore").await;
        mock_sign_in(&session).await;
        super::super::login(
            "alice".to_owned(),
            "hunter2".to_owned(),
            session.server.uri(),
            session.state.clone(),
        )
        .await
        .expect("signing in should succeed");

        let outcome = super::super::restore_session(
            USER_ID.to_owned(),
            Some(session.server.uri()),
            session.state.clone(),
        )
        .await
        .expect("restoring should succeed");

        assert_eq!(outcome, "session restored");
        assert_eq!(signed_in_as(&session.state).await.as_deref(), Some(USER_ID));
    }

    #[tokio::test]
    async fn refuses_a_user_with_nothing_stored() {
        let session = mock_session("auth-restore-unknown").await;
        session.server.mock_versions().ok().mount().await;

        let outcome = super::super::restore_session(
            "@nobody:localhost".to_owned(),
            Some(session.server.uri()),
            session.state.clone(),
        )
        .await
        .expect_err("nothing was ever stored for this user");

        assert!(
            outcome.starts_with("Session restoration failed"),
            "got: {outcome}"
        );
    }
}

mod list_accounts {
    use super::*;

    #[tokio::test]
    async fn reports_an_empty_store() {
        let session = mock_session("auth-list-empty").await;

        let listing = super::super::list_accounts(session.state.clone())
            .await
            .expect("listing should succeed");

        assert_eq!(listing, "Last account: none\nAccounts count: 0");
    }

    #[tokio::test]
    async fn names_each_account_and_its_homeserver() {
        let session = mock_session("auth-list-some").await;
        session
            .app_state
            .echelon_store
            .add_account("@alice:example.org", "https://example.org")
            .expect("adding should succeed");

        let listing = super::super::list_accounts(session.state.clone())
            .await
            .expect("listing should succeed");

        assert_eq!(
            listing,
            concat!(
                "Last account: @alice:example.org\n",
                "Accounts count: 1\n",
                " - @alice:example.org (homeserver: https://example.org)"
            )
        );
    }

    #[tokio::test]
    async fn marks_an_account_with_no_recorded_homeserver() {
        // A legacy snapshot holds bare user ids, so the homeserver has to be
        // rediscovered. The listing has to say so rather than showing a blank.
        let session = mock_session("auth-list-legacy").await;
        crate::storage::store::test_seed::seed_raw_accounts(
            &session.app_state.echelon_store,
            json!({"last": null, "accounts": ["@alice:example.org"]}),
        );

        let listing = super::super::list_accounts(session.state.clone())
            .await
            .expect("listing should succeed");

        assert_eq!(
            listing,
            concat!(
                "Last account: none\n",
                "Accounts count: 1\n",
                " - @alice:example.org (homeserver: <unknown/discover>)"
            )
        );
    }
}

//! Unit suite for [`crate::commands::debug`].
//!
//! Every case runs against an empty client state, so a command that gets past
//! its own argument handling stops at the session guard instead of reaching the
//! network. What is under test here is the dispatcher: name lookup, positional
//! argument handling, and the parsing it does before delegating.

use super::*;
use std::sync::Arc;
use tokio::sync::RwLock;

/// A client state with no session in it.
fn no_session() -> ClientState {
    Arc::new(RwLock::new(None))
}

/// The error every command returns once its arguments are accepted but there is
/// nobody logged in.
const NO_SESSION: &str = "No active client session";

/// Dispatch `command` with `args` against an empty client state.
///
/// # Arguments
/// * `command` - The command name to dispatch.
/// * `args` - Positional arguments, as the debug console would supply them.
async fn run(command: &str, args: &[&str]) -> Result<String, String> {
    let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
    dispatch(command, &args, no_session()).await
}

/// The error `command` produced, or a panic if it somehow succeeded.
///
/// # Arguments
/// * `command` - The command name to dispatch.
/// * `args` - Positional arguments, as the debug console would supply them.
async fn error(command: &str, args: &[&str]) -> String {
    run(command, args)
        .await
        .expect_err("no command can succeed without a session")
}

mod name_lookup {
    use super::*;

    #[tokio::test]
    async fn rejects_a_name_it_does_not_know() {
        assert_eq!(
            error("frobnicate", &[]).await,
            "unknown command 'frobnicate'"
        );
    }

    #[tokio::test]
    async fn rejects_an_empty_name() {
        assert_eq!(error("", &[]).await, "unknown command ''");
    }

    #[tokio::test]
    async fn every_advertised_command_is_wired_up() {
        // The console builds its UI from `COMMANDS`, so a name listed there but
        // missing from the dispatcher would offer the user a button that only
        // ever answers "unknown command".
        for spec in COMMANDS {
            let outcome = error(spec.name, &[]).await;

            assert!(
                !outcome.starts_with("unknown command"),
                "{} is advertised but not dispatched",
                spec.name
            );
        }
    }

    #[test]
    fn no_command_is_advertised_twice() {
        let mut names: Vec<&str> = COMMANDS.iter().map(|spec| spec.name).collect();
        names.sort_unstable();
        let count = names.len();
        names.dedup();

        assert_eq!(names.len(), count, "COMMANDS contains a duplicate name");
    }
}

mod missing_arguments {
    use super::*;

    #[tokio::test]
    async fn a_command_called_with_nothing_validates_rather_than_panicking() {
        // The console can send fewer arguments than a command reads; the missing
        // ones become empty strings, which the commands then reject on their own
        // terms.
        assert_eq!(
            error("login", &[]).await,
            "username and password are required"
        );
        assert_eq!(
            error("register", &[]).await,
            "username and password are required"
        );
        assert_eq!(error("oauth_login", &[]).await, "homeserver is required");
        assert_eq!(error("oauth_register", &[]).await, "homeserver is required");
        assert_eq!(error("restore_session", &[]).await, "user id is required");
    }

    #[tokio::test]
    async fn a_command_taking_no_arguments_ignores_any_it_is_given() {
        assert_eq!(error("logout", &["ignored", "extra"]).await, NO_SESSION);
        assert_eq!(error("list_accounts", &["ignored"]).await, NO_SESSION);
    }

    #[tokio::test]
    async fn a_blank_required_argument_is_treated_as_missing() {
        assert_eq!(
            error("login", &["  ", "  ", "https://example.org"]).await,
            "username and password are required"
        );
    }

    #[tokio::test]
    async fn complete_arguments_get_past_validation_to_the_session_guard() {
        assert_eq!(
            error("login", &["alice", "hunter2", "https://example.org"]).await,
            NO_SESSION
        );
        assert_eq!(
            error("restore_session", &["@alice:example.org"]).await,
            NO_SESSION
        );
    }
}

mod reset_account {
    use super::*;

    #[tokio::test]
    async fn accepts_both_reset_types() {
        for reset_type in ["IdentityReset", "KeyBackupReset"] {
            assert_eq!(
                error("reset_account", &[reset_type]).await,
                NO_SESSION,
                "for {reset_type}"
            );
        }
    }

    #[tokio::test]
    async fn names_the_valid_types_when_given_something_else() {
        assert_eq!(
            error("reset_account", &["identityreset"]).await,
            "unknown reset type 'identityreset', expected IdentityReset or KeyBackupReset"
        );
    }

    #[tokio::test]
    async fn rejects_a_missing_type_rather_than_defaulting() {
        assert_eq!(
            error("reset_account", &[]).await,
            "unknown reset type '', expected IdentityReset or KeyBackupReset"
        );
    }

    #[tokio::test]
    async fn parses_the_type_before_checking_for_a_session() {
        // Ordering matters for the console: a typo should be reported as a typo,
        // not masked by a session error the user cannot act on.
        let outcome = error("reset_account", &["Nonsense"]).await;

        assert!(outcome.starts_with("unknown reset type"), "got: {outcome}");
    }
}

mod get_messages {
    use super::*;

    #[tokio::test]
    async fn rejects_an_unparseable_room_id() {
        let outcome = error("get_messages", &["not-a-room"]).await;

        assert!(
            outcome.starts_with("invalid room_id 'not-a-room'"),
            "got: {outcome}"
        );
    }

    #[tokio::test]
    async fn rejects_a_missing_room_id() {
        let outcome = error("get_messages", &[]).await;

        assert!(outcome.starts_with("invalid room_id ''"), "got: {outcome}");
    }

    #[tokio::test]
    async fn accepts_a_valid_room_id() {
        assert_eq!(
            error("get_messages", &["!room:example.org"]).await,
            NO_SESSION
        );
    }

    #[tokio::test]
    async fn tolerates_an_unparseable_limit() {
        // The limit falls back to its default rather than failing the command,
        // so a typo in the console costs a page size, not the request.
        assert_eq!(
            error("get_messages", &["!room:example.org", "", "not-a-number"]).await,
            NO_SESSION
        );
    }
}

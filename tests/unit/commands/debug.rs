//! Unit suite for [`crate::commands::debug`].
//!
//! Most cases run against an empty client state, so a command that gets past its
//! own argument handling stops at the session guard instead of reaching the
//! network. What is under test there is the dispatcher: name lookup, positional
//! argument handling, and the parsing it does before delegating.
//!
//! The [`rendering`] cases are the other half of the dispatcher's job. Three
//! commands return structured values that it has to flatten into the single
//! string the console prints, and that flattening is the only place the console
//! output is decided.

use super::*;
use crate::test_support::*;
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

mod rendering {
    use super::*;
    use matrix_sdk_test::JoinedRoomBuilder;
    use serde_json::json;

    /// The room the message cases work in.
    const ROOM: &str = "!room:localhost";

    /// The account the fixture messages are sent by.
    const SENDER: &str = "@alice:example.org";

    /// A fixed timestamp, so the rendered output is comparable in full.
    const TIMESTAMP: u64 = 1_700_000_000_000;

    /// Dispatch `command` against a session and return what the console would
    /// print.
    ///
    /// # Arguments
    /// * `session` - The session to dispatch through.
    /// * `command` - The command name.
    /// * `args` - Positional arguments.
    async fn render(session: &MockSession, command: &str, args: &[&str]) -> String {
        let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
        dispatch(command, &args, session.state.clone())
            .await
            .expect("the command should succeed against the mock server")
    }

    /// Sync a set of rooms into the session.
    ///
    /// # Arguments
    /// * `session` - The session to sync into.
    /// * `rooms` - The rooms to report as joined.
    async fn sync(session: &MockSession, rooms: Vec<JoinedRoomBuilder>) {
        session
            .server
            .mock_sync()
            .ok_and_run(&session.client, move |builder| {
                for room in rooms {
                    builder.add_joined_room(room);
                }
            })
            .await;
    }

    /// A joined room, by id.
    ///
    /// # Arguments
    /// * `room_id` - The room's id.
    fn room(room_id: &str) -> JoinedRoomBuilder {
        JoinedRoomBuilder::new(room_id.try_into().expect("a valid room id"))
    }

    mod dm_rooms {
        use super::*;

        #[tokio::test]
        async fn counts_the_rooms_and_lists_their_ids() {
            let session = mock_session("debug-render-dms").await;
            sync(&session, vec![room("!aaa:localhost")]).await;

            assert_eq!(
                render(&session, "get_dm_rooms", &[]).await,
                "1 dm rooms\n!aaa:localhost"
            );
        }

        #[tokio::test]
        async fn an_account_with_no_dms_renders_a_count_of_zero() {
            // The console has to say something. An empty string would read as a
            // command that silently did nothing.
            let session = mock_session("debug-render-no-dms").await;
            sync(&session, Vec::new()).await;

            assert_eq!(render(&session, "get_dm_rooms", &[]).await, "0 dm rooms\n");
        }
    }

    mod space_hierarchy {
        use super::*;

        #[tokio::test]
        async fn names_each_space_with_its_child_count() {
            let session = mock_session("debug-render-spaces").await;
            sync(
                &session,
                vec![
                    room("!space:localhost")
                        .add_state_event(space_create_event())
                        .add_state_event(space_child_event("!child:localhost", &["localhost"])),
                    room("!child:localhost"),
                ],
            )
            .await;

            assert_eq!(
                render(&session, "get_space_hierarchy", &[]).await,
                "1 spaces\n!space:localhost (1 children)"
            );
        }

        #[tokio::test]
        async fn an_account_with_no_spaces_renders_a_count_of_zero() {
            let session = mock_session("debug-render-no-spaces").await;
            sync(&session, Vec::new()).await;

            assert_eq!(
                render(&session, "get_space_hierarchy", &[]).await,
                "0 spaces\n"
            );
        }
    }

    mod messages {
        use super::*;

        /// A text message event.
        ///
        /// # Arguments
        /// * `event_id` - The event's id.
        /// * `body` - The message text.
        fn text(
            event_id: &str,
            body: &str,
        ) -> ruma::serde::Raw<ruma::events::AnySyncTimelineEvent> {
            raw_event(json!({
                "type": "m.room.message",
                "event_id": event_id,
                "sender": SENDER,
                "origin_server_ts": TIMESTAMP,
                "content": {"msgtype": "m.text", "body": body},
            }))
        }

        /// A message that replaces `target`.
        ///
        /// # Arguments
        /// * `event_id` - The edit's own event id.
        /// * `target` - The event being replaced.
        /// * `body` - The replacement text.
        fn edit(
            event_id: &str,
            target: &str,
            body: &str,
        ) -> ruma::serde::Raw<ruma::events::AnySyncTimelineEvent> {
            raw_event(json!({
                "type": "m.room.message",
                "event_id": event_id,
                "sender": SENDER,
                "origin_server_ts": TIMESTAMP,
                "content": {
                    "msgtype": "m.text",
                    "body": format!("* {body}"),
                    "m.new_content": {"msgtype": "m.text", "body": body},
                    "m.relates_to": {"rel_type": "m.replace", "event_id": target},
                },
            }))
        }

        /// A redaction of `target`.
        ///
        /// # Arguments
        /// * `event_id` - The redaction's own event id.
        /// * `target` - The event being redacted.
        fn redaction(
            event_id: &str,
            target: &str,
        ) -> ruma::serde::Raw<ruma::events::AnySyncTimelineEvent> {
            raw_event(json!({
                "type": "m.room.redaction",
                "event_id": event_id,
                "sender": SENDER,
                "origin_server_ts": TIMESTAMP,
                "redacts": target,
                "content": {},
            }))
        }

        /// Sync the test room holding `timeline`.
        ///
        /// # Arguments
        /// * `session` - The session to sync into.
        /// * `timeline` - Timeline events, oldest first.
        async fn sync_messages(
            session: &MockSession,
            timeline: Vec<ruma::serde::Raw<ruma::events::AnySyncTimelineEvent>>,
        ) {
            let mut builder = room(ROOM);
            for event in timeline {
                builder = builder.add_timeline_event(event);
            }
            sync(session, vec![builder]).await;
        }

        #[tokio::test]
        async fn renders_a_message_with_its_timestamp_and_sender() {
            let session = mock_session("debug-render-message").await;
            sync_messages(&session, vec![text("$one", "hello")]).await;

            assert_eq!(
                render(&session, "get_messages", &[ROOM, "", "10"]).await,
                format!(
                    "1 effects (next_token: Some(\"$one\"))\n\
                     [{TIMESTAMP}] {SENDER}: hello"
                )
            );
        }

        #[tokio::test]
        async fn renders_an_edit_and_a_redaction_alongside_a_message() {
            // Every effect is printed rather than only the rows a user would
            // see, which is the point of the console: a page that folds down to
            // one row should show why it did.
            let session = mock_session("debug-render-effects").await;
            sync_messages(
                &session,
                vec![
                    text("$one", "hello"),
                    edit("$two", "$one", "hello, edited"),
                    // Targets an event outside the room, so the message above
                    // survives to anchor the page.
                    redaction("$three", "$absent"),
                ],
            )
            .await;

            assert_eq!(
                render(&session, "get_messages", &[ROOM, "", "10"]).await,
                format!(
                    "3 effects (next_token: Some(\"$one\"))\n\
                     [{TIMESTAMP}] {SENDER}: hello\n\
                     [edit of $one] hello, edited\n\
                     [redacts $absent]"
                )
            );
        }

        #[tokio::test]
        async fn a_room_with_no_history_renders_a_count_of_zero() {
            let session = mock_session("debug-render-no-messages").await;
            sync_messages(&session, Vec::new()).await;
            session
                .server
                .mock_room_messages()
                .ok(matrix_sdk::test_utils::mocks::RoomMessagesResponseTemplate::default())
                .mount()
                .await;

            assert_eq!(
                render(&session, "get_messages", &[ROOM, "", "10"]).await,
                "0 effects (next_token: None)\n"
            );
        }
    }
}

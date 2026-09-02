//! Unit suite for [`crate::commands::messages`].
//!
//! Pagination is the interesting part: a page is answered from the local event
//! cache where it can be, and from the homeserver where it cannot, and the token
//! it hands back decides whether the UI keeps scrolling or stops.

use super::*;
use crate::test_support::*;
use matrix_sdk::test_utils::mocks::RoomMessagesResponseTemplate;
use matrix_sdk_test::event_factory::EventFactory;
use matrix_sdk_test::{InvitedRoomBuilder, JoinedRoomBuilder, LeftRoomBuilder};
use ruma::{RoomId, UserId, event_id, room_id, user_id};
use serde_json::json;

/// The room every case works in.
fn the_room() -> &'static RoomId {
    room_id!("!room:localhost")
}

/// The account sending the messages under test.
fn alice() -> &'static UserId {
    user_id!("@alice:example.org")
}

/// An event factory bound to the test room and sender.
fn events() -> EventFactory {
    EventFactory::new().room(the_room()).sender(alice())
}

/// `count` text messages, oldest first, with predictable ids and bodies.
///
/// # Arguments
/// * `count` - How many messages to build.
fn message_batch(count: usize) -> Vec<ruma::serde::Raw<ruma::events::AnySyncTimelineEvent>> {
    (0..count)
        .map(|n| {
            events()
                .text_msg(format!("message {n}"))
                .event_id(&owned_event_id(n))
                .into_raw_sync()
        })
        .collect()
}

/// The event id of the `n`th message in a batch.
///
/// # Arguments
/// * `n` - The message's index.
fn owned_event_id(n: usize) -> ruma::OwnedEventId {
    ruma::EventId::parse(format!("$message{n}")).expect("a valid event id")
}

/// Sync a joined room holding `timeline`, plus any extra state events.
///
/// The sync carries no `prev_batch` token, so the event cache treats what it
/// receives as the whole of the room's history and will never paginate.
///
/// # Arguments
/// * `session` - The session to sync into.
/// * `timeline` - Timeline events, oldest first.
/// * `state` - State events to include, such as room membership.
async fn sync_room(
    session: &MockSession,
    timeline: Vec<ruma::serde::Raw<ruma::events::AnySyncTimelineEvent>>,
    state: Vec<ruma::serde::Raw<ruma::events::AnySyncStateEvent>>,
) {
    sync_room_inner(session, timeline, state, false).await;
}

/// Sync a joined room that has history older than what the sync carried.
///
/// The `prev_batch` token is what tells the event cache there is more to fetch;
/// without one it reports the room as fully loaded and no request is ever made,
/// however short the cache is.
///
/// # Arguments
/// * `session` - The session to sync into.
/// * `timeline` - Timeline events, oldest first.
async fn sync_room_with_older_history(
    session: &MockSession,
    timeline: Vec<ruma::serde::Raw<ruma::events::AnySyncTimelineEvent>>,
) {
    sync_room_inner(session, timeline, Vec::new(), true).await;
}

/// Shared body of the two sync helpers.
///
/// # Arguments
/// * `session` - The session to sync into.
/// * `timeline` - Timeline events, oldest first.
/// * `state` - State events to include.
/// * `more_history` - Whether to advertise older history with a `prev_batch`.
async fn sync_room_inner(
    session: &MockSession,
    timeline: Vec<ruma::serde::Raw<ruma::events::AnySyncTimelineEvent>>,
    state: Vec<ruma::serde::Raw<ruma::events::AnySyncStateEvent>>,
    more_history: bool,
) {
    let mut builder = JoinedRoomBuilder::new(the_room());
    for event in timeline {
        builder = builder.add_timeline_event(event);
    }
    for event in state {
        builder = builder.add_state_event(event);
    }
    if more_history {
        builder = builder.set_timeline_prev_batch("prev-batch-token");
    }

    session
        .server
        .mock_sync()
        .ok_and_run(&session.client, move |response| {
            response.add_joined_room(builder);
        })
        .await;
}

/// The bodies of the new messages in a page, in display order.
///
/// # Arguments
/// * `page` - The page to read.
fn bodies(page: &MessagePage) -> Vec<String> {
    page.effects
        .iter()
        .filter_map(|effect| match effect {
            EventEffect::New(message) => Some(message.body.clone()),
            _ => None,
        })
        .collect()
}

mod send_message {
    use super::*;

    /// Send `body` into the test room with a fixed transaction id.
    ///
    /// # Arguments
    /// * `session` - The session to send through.
    /// * `body` - The message text.
    async fn send(session: &MockSession, body: &str) -> Result<ruma::OwnedEventId, String> {
        super::super::send_message(
            session.state.clone(),
            the_room().to_owned(),
            body.to_owned(),
            "txn-1".into(),
        )
        .await
    }

    #[tokio::test]
    async fn refuses_without_a_session() {
        let outcome = super::super::send_message(
            empty_state(),
            the_room().to_owned(),
            "hello".to_owned(),
            "txn-1".into(),
        )
        .await
        .expect_err("there is nobody signed in");

        assert_eq!(outcome, "No active client session");
    }

    #[tokio::test]
    async fn refuses_an_empty_message() {
        let session = mock_session("send-empty").await;
        sync_room(&session, Vec::new(), Vec::new()).await;

        for body in ["", "   ", "\n\t"] {
            assert_eq!(
                send(&session, body)
                    .await
                    .expect_err("an empty message should be refused"),
                "message content is required",
                "for {body:?}"
            );
        }
    }

    #[tokio::test]
    async fn checks_the_body_before_looking_for_the_room() {
        // An empty composer should say so, not complain about a room the user
        // can plainly see.
        let session = mock_session("send-empty-first").await;

        let outcome = super::super::send_message(
            session.state.clone(),
            room_id!("!nowhere:localhost").to_owned(),
            String::new(),
            "txn-1".into(),
        )
        .await
        .expect_err("an empty message should be refused");

        assert_eq!(outcome, "message content is required");
    }

    #[tokio::test]
    async fn reports_an_unknown_room() {
        let session = mock_session("send-unknown-room").await;

        let outcome = super::super::send_message(
            session.state.clone(),
            room_id!("!nowhere:localhost").to_owned(),
            "hello".to_owned(),
            "txn-1".into(),
        )
        .await
        .expect_err("the room was never joined");

        assert_eq!(outcome, "Room !nowhere:localhost not found");
    }

    #[tokio::test]
    async fn sends_into_a_joined_room() {
        let session = mock_session("send-ok").await;
        sync_room(&session, Vec::new(), Vec::new()).await;
        session.server.mock_room_state_encryption().plain().mount().await;
        session
            .server
            .mock_room_send()
            .ok(event_id!("$sent"))
            .expect(1)
            .mount()
            .await;

        let sent = send(&session, "hello").await.expect("sending should succeed");

        assert_eq!(sent, "$sent");
    }

    #[tokio::test]
    async fn sends_the_transaction_id_it_was_given() {
        // The optimistic row the UI drew is keyed by this id, so a send that
        // used a different one would leave that row stuck as pending forever.
        let session = mock_session("send-txn").await;
        sync_room(&session, Vec::new(), Vec::new()).await;
        session.server.mock_room_state_encryption().plain().mount().await;
        session
            .server
            .mock_room_send()
            .ok(event_id!("$sent"))
            .mount()
            .await;

        send(&session, "hello").await.expect("sending should succeed");

        let requests = session
            .server
            .received_requests()
            .await
            .expect("the server records requests");
        assert!(
            requests
                .iter()
                .any(|request| request.url.path().ends_with("/txn-1")),
            "no send request carried the transaction id"
        );
    }

    #[tokio::test]
    async fn reports_a_server_refusal() {
        let session = mock_session("send-error").await;
        sync_room(&session, Vec::new(), Vec::new()).await;
        session.server.mock_room_state_encryption().plain().mount().await;
        session.server.mock_room_send().error500().mount().await;

        let outcome = send(&session, "hello")
            .await
            .expect_err("the server refused the send");

        assert!(
            outcome.starts_with("Failed to send message to room !room:localhost"),
            "unexpected error: {outcome}"
        );
    }

    #[tokio::test]
    async fn refuses_a_room_the_account_was_only_invited_to() {
        // The client knows this room, so the "not found" check passes and the
        // send would otherwise go out and be refused by the homeserver. Saying
        // so here names a reason the user can act on, and costs no request.
        let session = mock_session("send-invited").await;
        session
            .server
            .mock_sync()
            .ok_and_run(&session.client, |builder| {
                builder.add_invited_room(InvitedRoomBuilder::new(the_room()));
            })
            .await;
        session
            .server
            .mock_room_send()
            .ok(event_id!("$sent"))
            .expect(0)
            .mount()
            .await;

        let outcome = send(&session, "hello")
            .await
            .expect_err("the account has not joined this room");

        assert_eq!(outcome, "Not joined to room !room:localhost");
        session.server.verify_and_reset().await;
    }

    #[tokio::test]
    async fn refuses_a_room_the_account_has_left() {
        let session = mock_session("send-left").await;
        session
            .server
            .mock_sync()
            .ok_and_run(&session.client, |builder| {
                builder.add_left_room(LeftRoomBuilder::new(the_room()));
            })
            .await;

        let outcome = send(&session, "hello")
            .await
            .expect_err("the account is no longer in this room");

        assert_eq!(outcome, "Not joined to room !room:localhost");
    }
}

mod own_display_name {
    use super::*;

    /// The name this account shows under in the test room.
    ///
    /// # Arguments
    /// * `session` - The session to ask.
    async fn name(session: &MockSession) -> String {
        super::super::own_display_name(session.state.clone(), the_room().to_owned())
            .await
            .expect("the room exists")
    }

    /// A joined-membership state event for `user_id` with an optional name.
    ///
    /// # Arguments
    /// * `user_id` - The member.
    /// * `display_name` - The name they chose, if any.
    fn member(
        user_id: &str,
        display_name: Option<&str>,
    ) -> ruma::serde::Raw<ruma::events::AnySyncStateEvent> {
        let content = match display_name {
            Some(name) => json!({"membership": "join", "displayname": name}),
            None => json!({"membership": "join"}),
        };
        state_event("m.room.member", user_id, content)
    }

    #[tokio::test]
    async fn refuses_without_a_session() {
        let outcome = super::super::own_display_name(empty_state(), the_room().to_owned())
            .await
            .expect_err("there is nobody signed in");

        assert_eq!(outcome, "No active client session");
    }

    #[tokio::test]
    async fn reports_an_unknown_room() {
        let session = mock_session("name-unknown-room").await;

        let outcome =
            super::super::own_display_name(session.state.clone(), the_room().to_owned())
                .await
                .expect_err("the room was never joined");

        assert_eq!(outcome, "Room !room:localhost not found");
    }

    #[tokio::test]
    async fn uses_the_name_the_account_chose() {
        let session = mock_session("name-chosen").await;
        sync_room(
            &session,
            Vec::new(),
            vec![member("@example:localhost", Some("Example User"))],
        )
        .await;

        assert_eq!(name(&session).await, "Example User");
    }

    #[tokio::test]
    async fn falls_back_to_the_localpart_when_no_name_is_set() {
        // A member event without a `displayname` resolves to the localpart, not
        // to the full user id. The full-id fallback in `display_name` is for a
        // member the client has no event for at all, which is a different case.
        let session = mock_session("name-unset").await;
        sync_room(&session, Vec::new(), vec![member("@example:localhost", None)]).await;

        assert_eq!(name(&session).await, "example");
    }

    #[tokio::test]
    async fn falls_back_to_the_user_id_for_a_member_it_has_no_event_for() {
        let session = mock_session("name-no-member-event").await;
        sync_room(&session, Vec::new(), Vec::new()).await;

        assert_eq!(name(&session).await, "@example:localhost");
    }

    #[tokio::test]
    async fn disambiguates_a_name_two_members_share() {
        // Two people called the same thing have to be told apart, or a message
        // from either looks like a message from the other.
        let session = mock_session("name-ambiguous").await;
        sync_room(
            &session,
            Vec::new(),
            vec![
                member("@example:localhost", Some("Alex")),
                member("@other:localhost", Some("Alex")),
            ],
        )
        .await;

        assert_eq!(name(&session).await, "Alex (@example:localhost)");
    }
}

mod pagination {
    use super::*;

    /// The error from a page that is expected to fail.
    ///
    /// `MessagePage` holds classified events and is not `Debug`, so the `Result`
    /// helper that would report an unexpected success is unavailable.
    ///
    /// # Arguments
    /// * `result` - The outcome to unwrap.
    fn expect_error(result: Result<MessagePage, String>) -> String {
        match result {
            Ok(page) => panic!(
                "expected the page to fail, got one with {} effects",
                page.effects.len()
            ),
            Err(error) => error,
        }
    }

    /// Fetch one page from the test room.
    ///
    /// # Arguments
    /// * `session` - The session to read through.
    /// * `from` - The pagination token, if continuing a scroll.
    /// * `limit` - How many events the page should hold.
    async fn page(
        session: &MockSession,
        from: Option<&str>,
        limit: u32,
    ) -> Result<MessagePage, String> {
        get_messages_from_room_paginated(
            session.state.clone(),
            the_room().to_owned(),
            from.map(ToOwned::to_owned),
            limit,
        )
        .await
    }

    #[tokio::test]
    async fn refuses_without_a_session() {
        let outcome = expect_error(
            get_messages_from_room_paginated(empty_state(), the_room().to_owned(), None, 10).await,
        );

        assert_eq!(outcome, "No active client session");
    }

    #[tokio::test]
    async fn reports_an_unknown_room() {
        let session = mock_session("page-unknown-room").await;

        let outcome = expect_error(page(&session, None, 10).await);

        assert_eq!(outcome, "Room !room:localhost not found");
    }

    #[tokio::test]
    async fn rejects_an_unparseable_token() {
        let session = mock_session("page-bad-token").await;
        sync_room(&session, message_batch(3), Vec::new()).await;

        let outcome = expect_error(page(&session, Some("not-an-event-id"), 10).await);

        assert!(
            outcome.starts_with("invalid from event id 'not-an-event-id'"),
            "unexpected error: {outcome}"
        );
    }

    #[tokio::test]
    async fn answers_a_full_page_without_asking_the_server() {
        // The cache already holds enough, so the scroll must not cost a request.
        let session = mock_session("page-from-cache").await;
        sync_room(&session, message_batch(5), Vec::new()).await;
        session
            .server
            .mock_room_messages()
            .ok(RoomMessagesResponseTemplate::default())
            .expect(0)
            .mount()
            .await;

        let page = page(&session, None, 3).await.expect("the page should build");

        assert_eq!(bodies(&page), ["message 2", "message 3", "message 4"]);
        session.server.verify_and_reset().await;
    }

    #[tokio::test]
    async fn anchors_the_next_page_on_the_oldest_message_it_returned() {
        let session = mock_session("page-token").await;
        sync_room(&session, message_batch(5), Vec::new()).await;

        let page = page(&session, None, 3).await.expect("the page should build");

        assert_eq!(page.next_token.as_deref(), Some("$message2"));
    }

    #[tokio::test]
    async fn continues_from_a_token_into_the_events_before_it() {
        let session = mock_session("page-anchored").await;
        sync_room(&session, message_batch(5), Vec::new()).await;

        let page = page(&session, Some("$message3"), 2)
            .await
            .expect("the page should build");

        assert_eq!(bodies(&page), ["message 1", "message 2"]);
    }

    #[tokio::test]
    async fn a_cache_shorter_than_the_page_is_still_answered_locally() {
        // Reaching the front of what is loaded is enough to answer, even though
        // the page came back shorter than asked for. The token it hands back is
        // what carries the scroll onward, so nothing is lost by not fetching
        // here.
        let session = mock_session("page-short-cache").await;
        sync_room_with_older_history(&session, message_batch(1)).await;
        session
            .server
            .mock_room_messages()
            .ok(RoomMessagesResponseTemplate::default())
            .expect(0)
            .mount()
            .await;

        let page = page(&session, None, 10).await.expect("the page should build");

        assert_eq!(bodies(&page), ["message 0"]);
        assert_eq!(page.next_token.as_deref(), Some("$message0"));
        session.server.verify_and_reset().await;
    }

    #[tokio::test]
    async fn asks_the_server_when_the_cache_is_empty() {
        let session = mock_session("page-from-server").await;
        sync_room_with_older_history(&session, Vec::new()).await;
        session
            .server
            .mock_room_messages()
            .ok(RoomMessagesResponseTemplate::default().events(vec![
                events()
                    .text_msg("older")
                    .event_id(event_id!("$older"))
                    .into_raw_timeline(),
            ]))
            .expect(1)
            .mount()
            .await;

        let page = page(&session, None, 10).await.expect("the page should build");

        assert!(bodies(&page).contains(&"older".to_owned()));
        session.server.verify_and_reset().await;
    }

    #[tokio::test]
    async fn stops_the_scroll_at_the_start_of_the_room() {
        // A response with no continuation token means there is nothing older, so
        // the UI has to be told to stop asking.
        let session = mock_session("page-start").await;
        sync_room_with_older_history(&session, Vec::new()).await;
        session
            .server
            .mock_room_messages()
            .ok(RoomMessagesResponseTemplate::default())
            .mount()
            .await;

        let page = page(&session, None, 10).await.expect("the page should build");

        assert_eq!(page.next_token, None);
    }

    #[tokio::test]
    async fn a_window_holding_no_messages_does_not_end_the_scroll() {
        // Redactions are events but not rows. A window made only of them folds
        // to nothing displayable, and reporting that as an empty page would
        // strand the scroll short of the room's actual beginning.
        let session = mock_session("page-redactions").await;
        let mut timeline = vec![
            events()
                .text_msg("oldest")
                .event_id(&owned_event_id(0))
                .into_raw_sync(),
        ];
        for n in 1..4 {
            timeline.push(
                events()
                    .redaction(&owned_event_id(0))
                    .event_id(&owned_event_id(n))
                    .into_raw_sync(),
            );
        }
        sync_room_with_older_history(&session, timeline).await;
        session
            .server
            .mock_room_messages()
            .ok(RoomMessagesResponseTemplate::default())
            .expect(1)
            .mount()
            .await;

        let _page = page(&session, None, 3).await.expect("the page should build");

        session.server.verify_and_reset().await;
    }

    #[tokio::test]
    async fn resolves_the_display_name_of_every_sender() {
        let session = mock_session("page-names").await;
        sync_room(
            &session,
            message_batch(2),
            vec![state_event(
                "m.room.member",
                alice().as_str(),
                json!({"membership": "join", "displayname": "Alice"}),
            )],
        )
        .await;

        let page = page(&session, None, 2).await.expect("the page should build");

        assert_eq!(
            page.display_names.get(alice()).map(String::as_str),
            Some("Alice")
        );
    }

    #[tokio::test]
    async fn a_limit_of_zero_is_not_fatal() {
        let session = mock_session("page-zero").await;
        sync_room(&session, message_batch(3), Vec::new()).await;
        session
            .server
            .mock_room_messages()
            .ok(RoomMessagesResponseTemplate::default())
            .mount()
            .await;

        let page = page(&session, None, 0).await.expect("the page should build");

        assert!(bodies(&page).is_empty());
    }

    #[tokio::test]
    async fn reopening_the_same_room_keeps_its_subscription() {
        // Opening a room takes the event cache subscription over from whichever
        // room held it. Releasing and retaking it for the room that already has
        // it would unload the scrollback the user is looking at, so the second
        // open reads the cache without touching the slot.
        let session = mock_session("page-reopen").await;
        sync_room(&session, message_batch(5), Vec::new()).await;

        let first = page(&session, None, 3).await.expect("the page should build");
        let second = page(&session, None, 3).await.expect("the page should build");

        assert_eq!(bodies(&first), ["message 2", "message 3", "message 4"]);
        assert_eq!(bodies(&second), bodies(&first));
        assert_eq!(second.next_token, first.next_token);
    }

    #[tokio::test]
    async fn a_fetched_page_with_no_messages_is_anchored_on_a_raw_event() {
        // A fetch can come back holding nothing the list shows: a run of
        // membership changes, or reactions. Reporting no token for that reads to
        // the caller as the start of the room, so the scroll would dead-end
        // short of the room's actual beginning. The raw event id keeps it going
        // and resolves as an anchor next time, since the event is in the cache
        // whether or not it renders.
        let session = mock_session("page-raw-anchor").await;
        sync_room_with_older_history(&session, Vec::new()).await;
        session
            .server
            .mock_room_messages()
            .ok(RoomMessagesResponseTemplate::default()
                .end_token("older-still")
                .events(
                    // Newest first, as a backwards page arrives, and enough of
                    // them to satisfy the limit in one round so the pagination
                    // stops short of the room's start.
                    (0..3)
                        .map(|n| {
                            events()
                                .reaction(event_id!("$absent"), "👍")
                                .event_id(&ruma::EventId::parse(format!("$reaction{n}")).unwrap())
                                .into_raw_timeline()
                        })
                        .collect::<Vec<_>>(),
                ))
            .mount()
            .await;

        let page = page(&session, None, 3).await.expect("the page should build");

        assert!(bodies(&page).is_empty());
        assert_eq!(page.next_token.as_deref(), Some("$reaction2"));
    }
}

mod oldest_event_id {
    use super::*;

    /// A batch as `run_backwards_until` returns one: newest first.
    ///
    /// # Arguments
    /// * `ids` - Event ids, newest first.
    fn newest_first(ids: &[&str]) -> Vec<TimelineEvent> {
        ids.iter()
            .map(|id| {
                timeline_event(json!({
                    "type": "m.room.message",
                    "event_id": id,
                    "sender": "@alice:example.org",
                    "origin_server_ts": 1_700_000_000_000u64,
                    "content": {"msgtype": "m.text", "body": "hello"},
                }))
            })
            .collect()
    }

    #[test]
    fn takes_the_last_event_of_a_newest_first_batch() {
        // Reading this from the front would anchor the next page on the newest
        // event of this one, which asks the server for what was just fetched.
        assert_eq!(
            super::super::oldest_event_id(&newest_first(&["$new", "$mid", "$old"])),
            Some(event_id!("$old").to_owned())
        );
    }

    #[test]
    fn a_single_event_is_its_own_oldest() {
        assert_eq!(
            super::super::oldest_event_id(&newest_first(&["$only"])),
            Some(event_id!("$only").to_owned())
        );
    }

    #[test]
    fn an_empty_batch_has_no_oldest_event() {
        assert_eq!(super::super::oldest_event_id(&[]), None);
    }

    #[test]
    fn skips_past_an_event_that_carries_no_id() {
        // An event the server sent without an `event_id` cannot anchor
        // anything, so the search has to keep walking rather than give up.
        let mut batch = newest_first(&["$new"]);
        batch.push(timeline_event(json!({
            "type": "m.room.message",
            "sender": "@alice:example.org",
            "origin_server_ts": 1_700_000_000_000u64,
            "content": {"msgtype": "m.text", "body": "no id"},
        })));

        assert_eq!(
            super::super::oldest_event_id(&batch),
            Some(event_id!("$new").to_owned())
        );
    }
}

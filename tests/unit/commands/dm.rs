//! Unit suite for [`crate::commands::dm`].
//!
//! The room list comes from two places that have to be combined: rooms the
//! account marked as direct in its `m.direct` account data, and joined rooms
//! that belong to no space. These cases arrange both through a mock homeserver.

use super::*;
use crate::test_support::*;
use matrix_sdk_test::JoinedRoomBuilder;
use serde_json::json;

/// The ids of the rooms a call returned, in order.
///
/// # Arguments
/// * `rooms` - The rooms to name.
fn ids(rooms: &[Room]) -> Vec<String> {
    rooms
        .iter()
        .map(|room| room.room_id().to_string())
        .collect()
}

/// Sync a set of rooms, plus optional `m.direct` account data naming some of
/// them.
///
/// # Arguments
/// * `session` - The session to sync into.
/// * `rooms` - The joined rooms to report.
/// * `direct` - Room ids to list under `m.direct`, or `None` to send no such
///   account data at all.
async fn sync(session: &MockSession, rooms: Vec<JoinedRoomBuilder>, direct: Option<Vec<&str>>) {
    let direct = direct.map(|ids| {
        json!({
            "type": "m.direct",
            "content": {"@bob:example.org": ids},
        })
    });

    session
        .server
        .mock_sync()
        .ok_and_run(&session.client, move |builder| {
            for room in rooms {
                builder.add_joined_room(room);
            }
            if let Some(direct) = direct {
                builder.add_custom_global_account_data(direct);
            }
        })
        .await;
}

/// A plain joined room that belongs to no space.
///
/// # Arguments
/// * `room_id` - The room's id.
fn orphan(room_id: &str) -> JoinedRoomBuilder {
    JoinedRoomBuilder::new(room_id.try_into().expect("a valid room id"))
}

/// A joined room that is itself a space.
///
/// # Arguments
/// * `room_id` - The room's id.
fn space(room_id: &str) -> JoinedRoomBuilder {
    orphan(room_id).add_state_event(space_create_event())
}

/// A joined room that names a parent space.
///
/// # Arguments
/// * `room_id` - The room's id.
/// * `parent_id` - The space the room claims as its parent.
fn child_of(room_id: &str, parent_id: &str) -> JoinedRoomBuilder {
    orphan(room_id).add_state_event(space_parent_event(parent_id))
}

#[tokio::test]
async fn refuses_without_a_session() {
    let outcome = get_dm_rooms(empty_state())
        .await
        .expect_err("there is nobody signed in");

    assert_eq!(outcome, "No active client session");
}

#[tokio::test]
async fn returns_nothing_when_the_account_has_no_rooms() {
    let session = mock_session("dm-empty").await;
    sync(&session, Vec::new(), None).await;

    let rooms = get_dm_rooms(session.state.clone())
        .await
        .expect("listing should succeed");

    assert!(ids(&rooms).is_empty());
}

#[tokio::test]
async fn includes_a_room_named_by_the_direct_account_data() {
    let session = mock_session("dm-direct").await;
    sync(
        &session,
        vec![orphan("!direct:example.org")],
        Some(vec!["!direct:example.org"]),
    )
    .await;

    let rooms = get_dm_rooms(session.state.clone())
        .await
        .expect("listing should succeed");

    assert!(ids(&rooms).contains(&"!direct:example.org".to_owned()));
}

#[tokio::test]
async fn skips_a_direct_room_the_client_does_not_know() {
    // `m.direct` can name a room the account has since left, which the client
    // has no record of. That is not an error; it just is not listed.
    let session = mock_session("dm-direct-unknown").await;
    sync(
        &session,
        vec![orphan("!known:example.org")],
        Some(vec!["!known:example.org", "!vanished:example.org"]),
    )
    .await;

    let rooms = get_dm_rooms(session.state.clone())
        .await
        .expect("listing should succeed");

    assert!(!ids(&rooms).contains(&"!vanished:example.org".to_owned()));
}

#[tokio::test]
async fn includes_an_orphaned_room_with_no_direct_account_data_at_all() {
    // A brand new account has no `m.direct` event. Its rooms still have to be
    // listed rather than the whole call failing.
    let session = mock_session("dm-no-account-data").await;
    sync(&session, vec![orphan("!orphan:example.org")], None).await;

    let rooms = get_dm_rooms(session.state.clone())
        .await
        .expect("listing should succeed");

    assert_eq!(ids(&rooms), ["!orphan:example.org"]);
}

#[tokio::test]
async fn excludes_a_room_that_belongs_to_a_space() {
    // Rooms inside a space are reached through the space's own tab, so listing
    // them here as well would show them twice in the UI.
    let session = mock_session("dm-child").await;
    sync(
        &session,
        vec![
            space("!space:example.org"),
            child_of("!child:example.org", "!space:example.org"),
        ],
        None,
    )
    .await;

    let rooms = get_dm_rooms(session.state.clone())
        .await
        .expect("listing should succeed");

    assert!(!ids(&rooms).contains(&"!child:example.org".to_owned()));
}

#[tokio::test]
async fn excludes_a_space_itself() {
    let session = mock_session("dm-space").await;
    sync(&session, vec![space("!space:example.org")], None).await;

    let rooms = get_dm_rooms(session.state.clone())
        .await
        .expect("listing should succeed");

    assert!(!ids(&rooms).contains(&"!space:example.org".to_owned()));
}

#[tokio::test]
async fn lists_an_orphan_alongside_a_space_and_its_child() {
    let session = mock_session("dm-mixed").await;
    sync(
        &session,
        vec![
            space("!space:example.org"),
            child_of("!child:example.org", "!space:example.org"),
            orphan("!orphan:example.org"),
        ],
        None,
    )
    .await;

    let rooms = get_dm_rooms(session.state.clone())
        .await
        .expect("listing should succeed");

    assert_eq!(ids(&rooms), ["!orphan:example.org"]);
}

#[tokio::test]
async fn a_direct_room_with_no_parent_space_is_listed_twice() {
    // Pins current behaviour rather than blessing it. A 1:1 DM is both named by
    // `m.direct` and, having no parent space, an orphan, so it is appended by
    // both halves of the call. The two lists are concatenated with no
    // de-duplication, which is the common case for a DM rather than a corner
    // one.
    let session = mock_session("dm-duplicate").await;
    sync(
        &session,
        vec![orphan("!direct:example.org")],
        Some(vec!["!direct:example.org"]),
    )
    .await;

    let rooms = get_dm_rooms(session.state.clone())
        .await
        .expect("listing should succeed");

    assert_eq!(
        ids(&rooms),
        ["!direct:example.org", "!direct:example.org"],
        "de-duplicating this is a behaviour change, not a test fix"
    );
}

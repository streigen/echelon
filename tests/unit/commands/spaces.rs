//! Unit suite for [`crate::commands::spaces`].
//!
//! The hierarchy is built from `m.space.child` edges between joined rooms. What
//! matters is which edges count, which rooms end up as roots, and that a graph
//! shaped like a diamond or a cycle terminates.

use super::*;
use crate::test_support::*;
use matrix_sdk_test::JoinedRoomBuilder;

/// A space tree flattened into something comparable.
///
/// Children are sorted by room id, since the order state events come back in is
/// not part of what the hierarchy promises.
#[derive(Debug, PartialEq, Eq)]
struct Node(String, Vec<Node>);

/// Flatten a [`SpaceRoom`] for comparison.
///
/// # Arguments
/// * `node` - The node to flatten.
fn shape(node: &SpaceRoom) -> Node {
    let mut children: Vec<Node> = node.children.iter().map(shape).collect();
    children.sort_by(|a, b| a.0.cmp(&b.0));
    Node(node.room.room_id().to_string(), children)
}

/// Flatten a whole hierarchy.
///
/// # Arguments
/// * `hierarchy` - The roots to flatten.
fn shapes(hierarchy: &[SpaceRoom]) -> Vec<Node> {
    hierarchy.iter().map(shape).collect()
}

/// A node with no children, named by room id.
///
/// # Arguments
/// * `room_id` - The room the node stands for.
fn leaf(room_id: &str) -> Node {
    Node(room_id.to_owned(), Vec::new())
}

/// Sync a set of joined rooms into the session.
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

/// A joined room that is not a space.
///
/// # Arguments
/// * `room_id` - The room's id.
fn room(room_id: &str) -> JoinedRoomBuilder {
    JoinedRoomBuilder::new(room_id.try_into().expect("a valid room id"))
}

/// A space with an `m.space.child` edge to each of `children`.
///
/// # Arguments
/// * `room_id` - The space's id.
/// * `children` - Rooms the space points at, each reachable via `example.org`.
fn space(room_id: &str, children: &[&str]) -> JoinedRoomBuilder {
    let mut builder = room(room_id).add_state_event(space_create_event());
    for child in children {
        builder = builder.add_state_event(space_child_event(child, &["example.org"]));
    }
    builder
}

#[tokio::test]
async fn refuses_without_a_session() {
    let outcome = get_space_hierarchy(empty_state())
        .await
        .expect_err("there is nobody signed in");

    assert_eq!(outcome, "No active client session");
}

#[tokio::test]
async fn returns_nothing_when_the_account_has_no_spaces() {
    let session = mock_session("spaces-none").await;
    sync(&session, vec![room("!plain:example.org")]).await;

    let hierarchy = get_space_hierarchy(session.state.clone())
        .await
        .expect("building the hierarchy should succeed");

    assert!(hierarchy.is_empty());
}

#[tokio::test]
async fn a_space_with_no_children_is_a_root_on_its_own() {
    let session = mock_session("spaces-empty-space").await;
    sync(&session, vec![space("!space:example.org", &[])]).await;

    let hierarchy = get_space_hierarchy(session.state.clone())
        .await
        .expect("building the hierarchy should succeed");

    assert_eq!(shapes(&hierarchy), [leaf("!space:example.org")]);
}

#[tokio::test]
async fn a_space_carries_its_child_room() {
    let session = mock_session("spaces-child").await;
    sync(
        &session,
        vec![
            space("!space:example.org", &["!child:example.org"]),
            room("!child:example.org"),
        ],
    )
    .await;

    let hierarchy = get_space_hierarchy(session.state.clone())
        .await
        .expect("building the hierarchy should succeed");

    assert_eq!(
        shapes(&hierarchy),
        [Node(
            "!space:example.org".to_owned(),
            vec![leaf("!child:example.org")]
        )]
    );
}

#[tokio::test]
async fn an_edge_naming_no_server_is_not_a_child() {
    // An `m.space.child` with an empty `via` is how the spec says a child link
    // is removed, so honouring one would resurrect a room the user detached.
    let session = mock_session("spaces-no-via").await;
    sync(
        &session,
        vec![
            room("!space:example.org")
                .add_state_event(space_create_event())
                .add_state_event(space_child_event("!child:example.org", &[])),
            room("!child:example.org"),
        ],
    )
    .await;

    let hierarchy = get_space_hierarchy(session.state.clone())
        .await
        .expect("building the hierarchy should succeed");

    assert_eq!(shapes(&hierarchy), [leaf("!space:example.org")]);
}

#[tokio::test]
async fn an_edge_to_a_room_the_account_has_not_joined_is_not_a_child() {
    // A space can point at rooms the user cannot see. Those are not theirs to
    // list.
    let session = mock_session("spaces-unjoined-child").await;
    sync(
        &session,
        vec![space("!space:example.org", &["!elsewhere:example.org"])],
    )
    .await;

    let hierarchy = get_space_hierarchy(session.state.clone())
        .await
        .expect("building the hierarchy should succeed");

    assert_eq!(shapes(&hierarchy), [leaf("!space:example.org")]);
}

#[tokio::test]
async fn a_subspace_nests_rather_than_becoming_a_second_root() {
    let session = mock_session("spaces-nested").await;
    sync(
        &session,
        vec![
            space("!aaa-top:example.org", &["!bbb-mid:example.org"]),
            space("!bbb-mid:example.org", &["!ccc-leaf:example.org"]),
            room("!ccc-leaf:example.org"),
        ],
    )
    .await;

    let hierarchy = get_space_hierarchy(session.state.clone())
        .await
        .expect("building the hierarchy should succeed");

    assert_eq!(
        shapes(&hierarchy),
        [Node(
            "!aaa-top:example.org".to_owned(),
            vec![Node(
                "!bbb-mid:example.org".to_owned(),
                vec![leaf("!ccc-leaf:example.org")]
            )]
        )]
    );
}

#[tokio::test]
async fn roots_come_back_in_a_stable_order() {
    // The space tabs are built from this list, so an order that shuffled between
    // calls would move the tabs under the user's cursor.
    let session = mock_session("spaces-order").await;
    sync(
        &session,
        vec![
            space("!ccc:example.org", &[]),
            space("!aaa:example.org", &[]),
            space("!bbb:example.org", &[]),
        ],
    )
    .await;

    let first = get_space_hierarchy(session.state.clone())
        .await
        .expect("building the hierarchy should succeed");
    let second = get_space_hierarchy(session.state.clone())
        .await
        .expect("building the hierarchy should succeed");

    assert_eq!(
        shapes(&first),
        [
            leaf("!aaa:example.org"),
            leaf("!bbb:example.org"),
            leaf("!ccc:example.org")
        ]
    );
    assert_eq!(shapes(&first), shapes(&second));
}

#[tokio::test]
async fn a_room_reachable_through_two_spaces_appears_under_both() {
    // Spaces are a graph, not a tree, and a room legitimately belongs to more
    // than one. Emitting it once would hide it from whichever space lost the
    // race.
    let session = mock_session("spaces-diamond").await;
    sync(
        &session,
        vec![
            space(
                "!aaa-top:example.org",
                &["!bbb-left:example.org", "!ccc-right:example.org"],
            ),
            space("!bbb-left:example.org", &["!ddd-shared:example.org"]),
            space("!ccc-right:example.org", &["!ddd-shared:example.org"]),
            room("!ddd-shared:example.org"),
        ],
    )
    .await;

    let hierarchy = get_space_hierarchy(session.state.clone())
        .await
        .expect("building the hierarchy should succeed");

    assert_eq!(
        shapes(&hierarchy),
        [Node(
            "!aaa-top:example.org".to_owned(),
            vec![
                Node(
                    "!bbb-left:example.org".to_owned(),
                    vec![leaf("!ddd-shared:example.org")]
                ),
                Node(
                    "!ccc-right:example.org".to_owned(),
                    vec![leaf("!ddd-shared:example.org")]
                ),
            ]
        )]
    );
}

#[tokio::test]
async fn a_cycle_below_a_root_terminates() {
    // B and C point at each other. Walking that without a guard would recurse
    // until the stack ran out.
    let session = mock_session("spaces-cycle").await;
    sync(
        &session,
        vec![
            space("!aaa-top:example.org", &["!bbb:example.org"]),
            space("!bbb:example.org", &["!ccc:example.org"]),
            space("!ccc:example.org", &["!bbb:example.org"]),
        ],
    )
    .await;

    let hierarchy = get_space_hierarchy(session.state.clone())
        .await
        .expect("building the hierarchy should succeed");

    assert_eq!(
        shapes(&hierarchy),
        [Node(
            "!aaa-top:example.org".to_owned(),
            vec![Node(
                "!bbb:example.org".to_owned(),
                vec![leaf("!ccc:example.org")]
            )]
        )]
    );
}

#[tokio::test]
async fn a_pair_of_spaces_pointing_at_each_other_disappears_entirely() {
    // Pins current behaviour rather than blessing it. A root is a space that is
    // nobody's child, so when every space in a cycle is somebody's child there
    // are no roots at all and the whole group vanishes from the tab bar. The
    // user is still joined to both.
    let session = mock_session("spaces-closed-cycle").await;
    sync(
        &session,
        vec![
            space("!aaa:example.org", &["!bbb:example.org"]),
            space("!bbb:example.org", &["!aaa:example.org"]),
        ],
    )
    .await;

    let hierarchy = get_space_hierarchy(session.state.clone())
        .await
        .expect("building the hierarchy should succeed");

    assert!(
        hierarchy.is_empty(),
        "giving these a root is a behaviour change, not a test fix"
    );
}

mod labels {
    use super::*;

    /// Sync `builder` and hand back the room it describes.
    ///
    /// # Arguments
    /// * `session` - The session to sync into.
    /// * `room_id` - The room's id.
    /// * `builder` - The room to sync.
    async fn joined(session: &MockSession, room_id: &str, builder: JoinedRoomBuilder) -> Room {
        sync(session, vec![builder]).await;
        session
            .client
            .get_room(room_id.try_into().expect("a valid room id"))
            .expect("the room was just synced")
    }

    #[tokio::test]
    async fn uses_the_name_the_room_was_given() {
        let session = mock_session("spaces-label-named").await;
        let room = joined(
            &session,
            "!space:example.org",
            space("!space:example.org", &[]).add_state_event(state_event(
                "m.room.name",
                "",
                serde_json::json!({"name": "Engineering"}),
            )),
        )
        .await;

        assert_eq!(room_label(&room), "Engineering");
    }

    #[tokio::test]
    async fn falls_back_to_the_room_id_when_the_room_is_unnamed() {
        // A space with no `m.room.name` is legal. Labelling it with an empty
        // string would make two of them indistinguishable in the log this feeds.
        let session = mock_session("spaces-label-unnamed").await;
        let room = joined(
            &session,
            "!space:example.org",
            space("!space:example.org", &[]),
        )
        .await;

        assert_eq!(room_label(&room), "!space:example.org");
    }
}

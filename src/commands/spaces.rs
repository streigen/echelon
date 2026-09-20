use std::collections::{BTreeMap, HashMap, HashSet};

use matrix_sdk::Room;
use matrix_sdk::deserialized_responses::SyncOrStrippedState;
use ruma::events::SyncStateEvent;
use ruma::events::space::child::SpaceChildEventContent;
use ruma::{OwnedRoomId, OwnedServerName};
use tracing::debug;

use crate::ClientState;
use crate::rooms::room_types::SpaceRoom;

pub async fn get_space_hierarchy(client_state: ClientState) -> Result<Vec<SpaceRoom>, String> {
    let client = super::get_active_client(&client_state).await?;
    let all_joined_rooms = client.joined_rooms();

    // Use BTreeMap to maintain stable ordering of space tabs.
    let room_map: BTreeMap<OwnedRoomId, Room> = all_joined_rooms
        .into_iter()
        .map(|r| (r.room_id().to_owned(), r))
        .collect();

    let mut parent_to_children: HashMap<OwnedRoomId, Vec<OwnedRoomId>> = HashMap::new();
    let mut all_children: HashSet<OwnedRoomId> = HashSet::new();

    for (room_id, room) in &room_map {
        if !room.is_space() {
            continue;
        }

        if let Ok(child_events) = room
            .get_state_events_static::<SpaceChildEventContent>()
            .await
        {
            for raw_evt in child_events {
                if let Ok(evt) = raw_evt.deserialize() {
                    let child_room_id = evt.state_key();
                    let via: &[OwnedServerName] = match &evt {
                        SyncOrStrippedState::Sync(SyncStateEvent::Original(orig)) => {
                            &orig.content.via
                        }
                        SyncOrStrippedState::Stripped(stripped) => {
                            stripped.content.via.as_deref().unwrap_or_default()
                        }
                        _ => &[],
                    };
                    if !via.is_empty() && room_map.contains_key(child_room_id) {
                        parent_to_children
                            .entry(room_id.clone())
                            .or_default()
                            .push(child_room_id.clone());
                        all_children.insert(child_room_id.clone());
                    }
                }
            }
        }
    }

    let mut roots = Vec::new();
    for (room_id, room) in &room_map {
        if room.is_space() && !all_children.contains(room_id) {
            roots.push(room_id.clone());
        }
    }

    let mut hierarchy = Vec::new();
    let mut path = HashSet::new();

    for root_id in roots {
        if let Some(node) = build_tree(&root_id, &room_map, &parent_to_children, &mut path) {
            hierarchy.push(node);
        }
    }

    for k in &hierarchy {
        debug!(
            "Space: {:?} has children {:?}",
            k.room.name(),
            k.children
                .iter()
                .map(|r| room_label(&r.room))
                .collect::<Vec<String>>()
        );
    }

    Ok(hierarchy)
}

/// Return a room's display name, falling back to its ID if unset.
fn room_label(room: &Room) -> String {
    room.name().unwrap_or_else(|| room.room_id().to_string())
}

/// `path` holds only the current root-to-here chain, not every room seen so far. A
/// room reachable through two different parents (a legitimate DAG shape for spaces)
/// is thus emitted under each parent instead of being dropped after its first
/// occurrence; this check only guards against real cycles (a room nested under
/// itself). Backtracks by removing `current_id` before returning, so the same set
/// is reused across sibling and root traversals with no cloning.
fn build_tree(
    current_id: &OwnedRoomId,
    room_map: &BTreeMap<OwnedRoomId, Room>,
    parent_to_children: &HashMap<OwnedRoomId, Vec<OwnedRoomId>>,
    path: &mut HashSet<OwnedRoomId>,
) -> Option<SpaceRoom> {
    if !path.insert(current_id.clone()) {
        return None;
    }

    let room = room_map.get(current_id)?.clone();
    let mut children_nodes = Vec::new();

    if let Some(children_ids) = parent_to_children.get(current_id) {
        for child_id in children_ids {
            if let Some(child_node) = build_tree(child_id, room_map, parent_to_children, path) {
                children_nodes.push(child_node);
            }
        }
    }

    path.remove(current_id);

    Some(SpaceRoom {
        room,
        children: children_nodes,
    })
}

#[cfg(test)]
#[path = "../../tests/unit/commands/spaces.rs"]
mod tests;

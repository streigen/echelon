use std::collections::HashMap;

use futures_util::future::join_all;
use ruma::api::client::space::get_hierarchy;
use ruma::OwnedRoomId;
use tauri::State;
use tracing::debug;

use crate::rooms::room_types::{RawRoom, SpaceRoom};
use crate::spaces::raw_space::RawSpace;
use crate::ClientState;

/// Get the joined spaces. This is for when the frontend only requires spaces and not their full
/// hierarchies. This can be used as a quick placeholder while full trees are loading.
///
/// # Arguments
/// * `state` - The client state containing the Matrix client to fetch spaces from.
///
/// ### Returns
/// A list of `SpaceRoom` objects representing joined spaces. For full hierarchies,
/// prefer [`get_all_spaces_with_trees`] or [`get_space_tree`].
#[tauri::command]
pub async fn get_spaces(state: State<'_, ClientState>) -> Result<Vec<SpaceRoom>, String> {
    let result = super::with_active_client(&state, |client_handler| {
        client_handler
            .get_client()
            .joined_space_rooms()
            .into_iter()
            .map(|room| {
                let room_id = room.room_id().to_string();
                let name = room.name();
                let topic = room.topic();
                let avatar_url = room.avatar_url().map(|m| m.to_string());
                SpaceRoom {
                    base: RawRoom {
                        id: room_id,
                        name,
                        topic,
                        avatar_url,
                        is_space: room.is_space(),
                    },
                    // Root-level in this lightweight query: parent chain is resolved in tree APIs.
                    parent_spaces: Vec::new(),
                }
            })
            .collect::<Vec<SpaceRoom>>()
    })
    .await?;
    Ok(result)
}

/// This is relatively expensive because it fetches the full hierarchy for each joined space,
/// but it is useful for initial app load where the whole tree is needed in one call.
///
/// # Arguments
/// * `state` - The client state containing the Matrix client to fetch hierarchies from.
///
/// ### Returns
/// A map of `space_id -> RawSpace`, where each `RawSpace` contains basic space info
/// and all known descendants.
#[tauri::command]
pub async fn get_all_spaces_with_trees(
    state: State<'_, ClientState>,
) -> Result<HashMap<String, RawSpace>, String> {
    // Get joined spaces first.
    let joined_spaces =
        super::with_active_client(&state, |client_handler| client_handler.get_client().joined_space_rooms())
            .await?;

    // Collect all futures so we can execute hierarchy fetches concurrently.
    let tasks = joined_spaces.into_iter().map(|space| {
        let space_id = space.room_id().to_string();
        let state_clone = state.clone();
        async move {
            // Use get_space_tree for each root space; skip only the failing space.
            match get_space_tree(space_id.clone(), state_clone).await {
                Ok(tree) => Some(RawSpace {
                    raw_room: RawRoom {
                        id: space_id,
                        name: space.name(),
                        topic: space.topic(),
                        avatar_url: space.avatar_url().map(|m| m.to_string()),
                        is_space: space.is_space(),
                    },
                    rooms: tree,
                }),
                Err(e) => {
                    debug!("Failed to get tree for space {}: {}", space_id, e);
                    None
                }
            }
        }
    });
    let results = join_all(tasks).await;

    let mut root_map: HashMap<String, RawSpace> = HashMap::new();
    for res in results {
        match res {
            Some(raw_space) => {
                root_map.insert(raw_space.raw_room.id.clone(), raw_space);
            }
            None => continue,
        }
    }

    Ok(root_map)
}

/// Fetches the entire hierarchy of rooms under a given space, including parent-child
/// relationships. This powers the tree view in the UI.
///
/// # Arguments
/// * `space_id` - The ID of the space to fetch the hierarchy for.
/// * `state` - The client state containing the Matrix client to fetch from.
///
/// ### Returns
/// A list of `SpaceRoom` objects under the given space with parent-space chains.
#[tauri::command]
pub async fn get_space_tree(
    space_id: String,
    state: State<'_, ClientState>,
) -> Result<Vec<SpaceRoom>, String> {
    // Get the client.
    let state_r = state.0.read().await;
    let Some(client_handler) = state_r.as_ref() else {
        return Err("No active client session".to_string());
    };
    let client = client_handler.get_client();

    // Parse the input room id and fetch it.
    let space_room_id = OwnedRoomId::try_from(space_id).map_err(|e| e.to_string())?;
    let room = client.get_room(&*space_room_id);

    // Basic validation to ensure this is a real space room.
    let Some(room) = room else {
        return Err("Space not found".to_string());
    };
    if !room.is_space() {
        return Err("Given space ID does not correspond to a space room".to_string());
    }

    // Request full hierarchy for the target space.
    let request = get_hierarchy::v1::Request::new(space_room_id.clone());
    let response: get_hierarchy::v1::Response =
        client.send(request).await.map_err(|e| e.to_string())?;

    debug!("Space hierarchy returned {} rooms", response.rooms.len());

    // Track rooms plus child->parent and id->name lookup maps.
    let mut raw_rooms: Vec<RawRoom> = Vec::new();
    let mut child_to_parent: HashMap<String, String> = HashMap::new();
    let mut id_to_name: HashMap<String, String> = HashMap::new();

    for room_summary in response.rooms.iter().skip(1) {
        let room_id = room_summary.summary.room_id.to_string();
        let name = room_summary.summary.name.clone();
        let is_space = client
            .get_room(&*room_summary.summary.room_id)
            .map(|r| r.is_space())
            .unwrap_or(false);
        let topic = room_summary.summary.topic.clone();
        let avatar_url = room_summary.summary.avatar_url.as_ref().map(|u| u.to_string());

        id_to_name.insert(
            room_id.clone(),
            name.clone().unwrap_or_else(|| "Unnamed".to_string()),
        );

        for child_state in &room_summary.children_state {
            if let Ok(deserialized) = child_state.deserialize() {
                let child_id = deserialized.state_key.to_string();
                // This room is the parent of child_id.
                child_to_parent.insert(child_id.clone(), room_id.clone());
            }
        }

        debug!("  Child: {:?} ({})", name, room_id);

        raw_rooms.push(RawRoom {
            id: room_id,
            name,
            topic,
            avatar_url,
            is_space,
        });
    }

    // Build the parent-space chain for a room by walking child->parent links up to root.
    let build_parent_path = |start_id: &str| -> Vec<String> {
        let mut path: Vec<String> = Vec::new();
        let mut current = start_id.to_string();
        let mut visited = std::collections::HashSet::new();
        while let Some(parent_id) = child_to_parent.get(&current) {
            if visited.contains(parent_id) {
                break;
            }
            visited.insert(parent_id.clone());
            path.push(
                id_to_name
                    .get(parent_id)
                    .cloned()
                    .unwrap_or_else(|| parent_id.clone()),
            );
            current = parent_id.clone();
        }
        path.reverse();
        path
    };

    let mut rooms: Vec<SpaceRoom> = Vec::new();
    for raw in raw_rooms {
        let parent_spaces = build_parent_path(&raw.id);

        rooms.push(SpaceRoom {
            base: RawRoom {
                id: raw.id,
                name: raw.name,
                topic: raw.topic,
                avatar_url: raw.avatar_url,
                is_space: raw.is_space,
            },
            parent_spaces,
        });
    }

    Ok(rooms)
}



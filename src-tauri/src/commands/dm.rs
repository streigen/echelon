use std::collections::HashMap;

use futures_util::future::join_all;
use matrix_sdk::{Client, RoomMemberships};
use ruma::events::direct::OwnedDirectUserIdentifier;
use ruma::events::{AnyGlobalAccountDataEvent, GlobalAccountDataEventType, StateEventType};
use ruma::OwnedRoomId;
use tauri::State;
use tracing::error;

use crate::rooms::room_types::{DmRoom, RawRoom};
use crate::ClientState;

/// Get the DM rooms, including both explicit 1:1 rooms (`m.direct`) and inferred group DMs.
///
/// Group DMs are inferred via [`get_orphaned_rooms`] as a fallback for rooms that are
/// non-space and not linked to a parent space.
///
/// # Arguments
/// * `state` - The client state containing the Matrix client to fetch rooms from.
#[tauri::command]
pub async fn get_dm_rooms(state: State<'_, ClientState>) -> Result<Vec<DmRoom>, String> {
    // Get the client.
    let state_r = state.0.read().await;
    let Some(client_handler) = state_r.as_ref() else {
        return Err("No active client session".to_string());
    };
    let client = client_handler.get_client();
    // Final DM room list to return.
    let mut dm_rooms: Vec<DmRoom> = Vec::new();

    // Rooms with `m.direct` account data are canonical 1:1 DMs.
    let direct_rooms = client
        .state_store()
        .get_account_data_event(GlobalAccountDataEventType::Direct)
        .await
        .map_err(|e| e.to_string())?;

    if let Some(direct_rooms) = direct_rooms {
        if let Ok(deserialized) = direct_rooms.deserialize() {
            match deserialized {
                AnyGlobalAccountDataEvent::Direct(direct_data) => {
                    let mut dm_room_user_map: HashMap<OwnedRoomId, Vec<OwnedDirectUserIdentifier>> =
                        HashMap::new();
                    for (user_id, room_ids) in direct_data.content {
                        for room_id in room_ids {
                            // Map each room to related DM users for member rendering.
                            dm_room_user_map
                                .entry(room_id)
                                .or_insert_with(Vec::new)
                                .push(user_id.clone());
                        }
                    }
                    for (room_id, user_ids) in dm_room_user_map {
                        if let Some(room) = client.get_room(&room_id) {
                            let name = room.name();
                            let topic = room.topic();
                            let avatar_url = room.avatar_url().map(|u| u.to_string());
                            dm_rooms.push(DmRoom {
                                base: RawRoom {
                                    id: room_id.to_string(),
                                    name,
                                    topic,
                                    avatar_url,
                                    is_space: false,
                                },
                                members: user_ids.into_iter().map(|u| u.to_string()).collect(),
                            });
                        }
                    }
                }
                _ => error!("Unexpected account data event type, how"),
            }
        } else {
            error!("Failed to deserialize direct rooms data")
        }
    } else {
        error!("No direct message rooms found")
    }

    let mut other_rooms = get_orphaned_rooms(client).await?;
    dm_rooms.append(&mut other_rooms);

    Ok(dm_rooms)
}

/// Fetches joined rooms that are not spaces and do not have `m.space.parent` events.
///
/// This catches likely group DMs and legacy rooms that might not be represented in
/// `m.direct` account data.
///
/// # Arguments
/// * `client` - The Matrix client used for room/state lookups.
async fn get_orphaned_rooms(client: &Client) -> Result<Vec<DmRoom>, String> {
    let non_space_rooms = client.joined_rooms().into_iter().filter(|room| !room.is_space());

    let mut room_futures = Vec::new();
    for room in non_space_rooms {
        room_futures.push(async move {
            let has_parent_space = room
                .get_state_events(StateEventType::SpaceParent)
                .await
                .map(|events| !events.is_empty())
                .unwrap_or(false);

            if has_parent_space {
                return None;
            }

            let room_id = room.room_id().to_string();
            let name = room.name();
            let topic = room.topic();
            let avatar_url = room.avatar_url().map(|u| u.to_string());
            let members = room
                .members(RoomMemberships::all())
                .await
                .unwrap_or_default()
                .iter()
                .filter_map(|u| u.display_name().map(|n| n.to_string()))
                .collect();
            Some(DmRoom {
                base: RawRoom {
                    id: room_id,
                    name,
                    topic,
                    avatar_url,
                    is_space: false,
                },
                members,
            })
        });
    }

    let other_rooms = join_all(room_futures)
        .await
        .into_iter()
        .flatten()
        .collect();

    Ok(other_rooms)
}



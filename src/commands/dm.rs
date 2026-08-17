use std::collections::HashSet;

use futures_util::future::join_all;
use matrix_sdk::{Client, Room};
use ruma::OwnedRoomId;
use ruma::events::{AnyGlobalAccountDataEvent, GlobalAccountDataEventType, StateEventType};
use tracing::error;

use crate::ClientState;

/// Get the DM rooms, including both explicit 1:1 rooms (`m.direct`) and inferred group DMs.
///
/// Group DMs are inferred via [`get_orphaned_rooms`] as a fallback for rooms that are
/// non-space and not linked to a parent space.
///
/// # Arguments
/// * `state` - The client state containing the Matrix client to fetch rooms from.
pub async fn get_dm_rooms(state: ClientState) -> Result<Vec<Room>, String> {
    // Get the client.
    let state_r = state.read().await;
    let Some(client_handler) = state_r.as_ref() else {
        return Err("No active client session".to_string());
    };
    let client = client_handler.get_client();
    // Final DM room list to return.
    let mut dm_rooms: Vec<Room> = Vec::new();

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
                    // `m.direct` maps each user to the rooms they are a DM with, so one
                    // room shows up once per user it is a DM with. Only the distinct
                    // rooms are wanted here, and the set is what collapses them.
                    let dm_room_ids: HashSet<OwnedRoomId> = direct_data
                        .content
                        .into_iter()
                        .flat_map(|(_user_id, room_ids)| room_ids)
                        .collect();
                    for room_id in dm_room_ids {
                        if let Some(room) = client.get_room(&room_id) {
                            dm_rooms.push(room)
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
async fn get_orphaned_rooms(client: &Client) -> Result<Vec<Room>, String> {
    let non_space_rooms = client
        .joined_rooms()
        .into_iter()
        .filter(|room| !room.is_space());

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
            Some(room)
        });
    }

    let other_rooms = join_all(room_futures).await.into_iter().flatten().collect();

    Ok(other_rooms)
}

use std::collections::{HashMap, HashSet};

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

    let room_map: HashMap<OwnedRoomId, Room> = all_joined_rooms
        .into_iter()
        .map(|r| (r.room_id().to_owned(), r))
        .collect();

    let mut parent_to_children: HashMap<OwnedRoomId, Vec<OwnedRoomId>> = HashMap::new();
    let mut all_children: HashSet<OwnedRoomId> = HashSet::new();

    //room_map.iter().filter(|room_id, r| r.is_space()); // figure this out later i cba rn
    // parent -> children map
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
                            &stripped.content.via.as_deref().unwrap_or_default()
                        }
                        _ => &[],
                    };
                    debug!("via: {:?}", via);
                }
            }
        }
    }
    Ok(Vec::new())
    //Ok(result)
}

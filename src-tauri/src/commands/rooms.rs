use tauri::State;

use crate::rooms::room_types::RawRoom;
use crate::ClientState;

/// Get all joined rooms, including spaces and non-spaces.
///
/// This used to be used to build hierarchy trees in the frontend, but
/// [`get_space_tree`] and [`get_all_spaces_with_trees`] now perform that work
/// in the backend. It is kept temporarily for compatibility.
#[tauri::command]
#[deprecated(
    note = "I don't see why this needs to exist anymore, get_all_spaces_with_trees should cover all the same use cases and more. This function will be removed soon after i discuss w/ others"
)]
pub async fn get_rooms(state: State<'_, ClientState>) -> Result<Vec<RawRoom>, String> {
    let result = super::with_active_client(&state, |client_handler| {
        let rooms = client_handler.get_client().joined_rooms();
        let mut room_infos = Vec::new();
        for room in rooms {
            let room_id = room.room_id().to_string();
            let name = room.name();
            let topic = room.topic();
            let avatar_url = room.avatar_url().map(|m| m.to_string());

            room_infos.push(RawRoom {
                id: room_id,
                name,
                topic,
                avatar_url,
                is_space: room.is_space(),
            })
        }
        room_infos
    })
    .await?;
    Ok(result)
}



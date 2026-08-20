use crate::ClientState;
use crate::client::active_room::ActiveRoomSlot;
use matrix_sdk::Client;

pub mod account;
pub mod auth;
pub mod debug;
pub mod dm;
pub mod media;
pub mod messages;
pub mod spaces;

pub(crate) async fn get_active_client(state: &ClientState) -> Result<Client, String> {
    let state_r = state.read().await;

    let Some(client_handler) = state_r.as_ref() else {
        return Err("No active client session".to_string());
    };

    let client = client_handler.get_client().clone();

    Ok(client)
}

/// The active client along with its active-room subscription slot, read under a
/// single lock.
///
/// Both are cloned out and the guard released before returning. A caller that
/// held the guard instead would be holding it across the subscribe, and so
/// across a network call, blocking the login and logout that need to write it.
pub(crate) async fn get_active_client_and_room(
    state: &ClientState,
) -> Result<(Client, ActiveRoomSlot), String> {
    let state_r = state.read().await;

    let Some(client_handler) = state_r.as_ref() else {
        return Err("No active client session".to_string());
    };

    Ok((
        client_handler.get_client().clone(),
        client_handler.active_room_slot(),
    ))
}

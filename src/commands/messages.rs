use matrix_sdk::room::MessagesOptions;
use ruma::OwnedRoomId;
use tracing::debug;

use crate::ClientState;
use crate::rooms::messages::{MessageStore, StoredMessage};

/// One page of resolved, oldest-first messages plus the token to fetch
/// the next (older) page.
pub struct PaginatedMessages {
    pub messages: Vec<StoredMessage>,
    pub next_token: Option<String>,
}

/// Fetch one page of historical messages for a room, backward from the
/// given pagination token (or from the end of the timeline if `from` is
/// `None`), folding edits/redactions into their targets and deduping by
/// event id.
pub async fn get_messages_from_room_paginated(
    client_state: ClientState,
    room_id: OwnedRoomId,
    from: Option<String>,
    limit: u32,
) -> Result<PaginatedMessages, String> {
    let client = super::get_active_client(&client_state).await?;

    let room = client
        .get_room(&room_id)
        .ok_or_else(|| format!("Room {room_id} not found"))?;

    let mut options = MessagesOptions::backward();
    options.from = from;
    options.limit = limit.into();

    let response = room
        .messages(options)
        .await
        .map_err(|e| format!("Failed to fetch messages: {e}"))?;

    debug!(
        "Fetched {} raw events for room {}",
        response.chunk.len(),
        room_id
    );

    let mut store = MessageStore::with_capacity(limit as usize);
    for event in &response.chunk {
        store.apply_raw(event);
    }

    // `messages()` returns newest-first for backward pagination; store
    // fills in that same order, so reverse to get oldest-first for
    // display/append into the UI's message list.
    let mut messages = store.into_messages();
    messages.reverse();

    Ok(PaginatedMessages {
        messages,
        next_token: response.end,
    })
}

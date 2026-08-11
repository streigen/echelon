use ruma::{EventId, OwnedEventId, OwnedRoomId};
use tracing::debug;

use crate::ClientState;
use crate::rooms::messages::{MessageStore, StoredMessage};

/// One page of resolved, oldest-first messages plus the event id to pass
/// back as `from` to load the next (older) page.
pub struct PaginatedMessages {
    pub messages: Vec<StoredMessage>,
    pub next_token: Option<String>,
}

/// Fetch one page of messages for a room, folding edits/redactions into
/// their targets and deduping by event id.
///
/// `from` is the event id of the oldest message the caller has already
/// shown (typically the last `PaginatedMessages::next_token`), or `None` for
/// the page with the latest messages.
///
/// Backward pagination (`RoomEventCache::pagination`) is cache-aware
/// it reads from the the sqlite store before hitting network, making the
/// repeat visits fast.
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

    let anchor: Option<OwnedEventId> = from
        .map(|s| EventId::parse(&s).map_err(|e| format!("invalid from event id '{s}': {e}")))
        .transpose()?;

    let (cache, _drop_handles) = room
        .event_cache()
        .await
        .map_err(|e| format!("Event cache unavailable for room {room_id}: {e}"))?;

    let cached = cache
        .events()
        .await
        .map_err(|e| format!("Failed to read local event cache: {e}"))?;

    // Cached events are oldest-first. `window_end` is the index of the edge
    let window_end = match &anchor {
        None => Some(cached.len()),
        Some(anchor_id) => cached
            .iter()
            .position(|e| e.event_id().as_ref() == Some(anchor_id)),
    };

    if let Some(window_end) = window_end {
        let start = window_end.saturating_sub(limit as usize);
        let slice = &cached[start..window_end];

        // Enough already-loaded events to answer this page, or we've hit the front of what's loaded
        if slice.len() >= limit as usize || start == 0 {
            if let Some(messages) = messages_from_slice(slice) {
                debug!(
                    "Served {} messages for room {} from the local event cache (no network fetch)",
                    messages.len(),
                    room_id
                );
                let next_token = messages.first().map(|m| m.event_id.to_string());
                return Ok(PaginatedMessages {
                    messages,
                    next_token,
                });
            }
        }
    }

    // We haven't hit the limit yet. continue reading events, hitting the store (and then the messages endpoint if needed) and
    // persist in cache for the next time.
    let outcome = cache
        .pagination()
        .run_backwards_until(limit as u16)
        .await
        .map_err(|e| format!("Failed to fetch messages: {e}"))?;

    debug!(
        "read {} events for room {} (reached_start: {})",
        outcome.events.len(),
        room_id,
        outcome.reached_start
    );

    // `run_backwards_until` returns events newest-first but we need to feed the store
    // oldest-first so `into_messages()` comes out in display order.
    let mut store = MessageStore::with_capacity(outcome.events.len());
    for event in outcome.events.iter().rev() {
        store.apply_raw(event);
    }

    let messages = store.into_messages();
    let next_token = (!outcome.reached_start)
        .then(|| messages.first().map(|m| m.event_id.to_string()))
        .flatten();

    Ok(PaginatedMessages {
        messages,
        next_token,
    })
}

/// Resolves a slice of already-loaded, oldest-first cache events into
/// display-ready messages. Returns `None` if the slice folds down to
/// nothing displayable (e.g. it's all redactions/edits of events outside
/// the slice).
fn messages_from_slice(
    slice: &[matrix_sdk::deserialized_responses::TimelineEvent],
) -> Option<Vec<StoredMessage>> {
    let mut store = MessageStore::with_capacity(slice.len());
    for event in slice {
        store.apply_raw(event);
    }
    let messages = store.into_messages();
    (!messages.is_empty()).then_some(messages)
}

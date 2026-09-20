use std::collections::HashMap;

use matrix_sdk::deserialized_responses::TimelineEvent;
use matrix_sdk::{Room, RoomState};
use ruma::events::room::message::RoomMessageEventContent;
use ruma::{EventId, OwnedEventId, OwnedRoomId, OwnedTransactionId, OwnedUserId};
use tracing::{debug, trace};

use crate::ClientState;
use crate::client::active_room::subscribe_active_room;
use crate::rooms::members;
use crate::rooms::messages::{EventEffect, effects_of};

/// One page of classified events and optional next pagination token.
pub struct MessagePage {
    pub effects: Vec<EventEffect>,
    pub next_token: Option<String>,
    pub display_names: HashMap<OwnedUserId, String>,
}

/// Fetch one page of messages for a room.
///
/// # Arguments
/// * `client_state` - The client state containing the Matrix client.
/// * `room_id` - The room ID to fetch messages for.
/// * `from` - Optional pagination token for fetching older messages.
/// * `limit` - Maximum number of events to fetch.
pub async fn get_messages_from_room_paginated(
    client_state: ClientState,
    room_id: OwnedRoomId,
    from: Option<String>,
    limit: u32,
) -> Result<MessagePage, String> {
    let (client, active_room) = super::get_active_client_and_room(&client_state).await?;

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

    // Opening a room takes the subscription over from whichever room held it.
    // Paging further back inside the room that already holds it must leave it
    // where it is, since dropping it would unload the scrollback being read.
    let cached = match &anchor {
        None => subscribe_active_room(&active_room, &room_id, &cache).await?,
        Some(_) => cache
            .events()
            .await
            .map_err(|e| format!("Failed to read local event cache: {e}"))?,
    };

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
            let effects = effects_of(slice);
            // A window that folds down to no messages at all (all redactions,
            // or edits of events outside it) cannot answer the page, so it
            // falls through to the fetch below rather than reporting an empty
            // one and stranding the caller.
            if let Some(oldest) = oldest_message_id(&effects) {
                debug!(
                    "Served {} effects for room {} from the local event cache (no network fetch)",
                    effects.len(),
                    room_id
                );
                let next_token = Some(oldest.to_string());
                let display_names = resolve_display_names(&room, &effects).await;
                return Ok(MessagePage {
                    effects,
                    next_token,
                    display_names,
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

    // `run_backwards_until` returns events newest-first, classified oldest-first
    // so the effects apply in display order.
    let effects = effects_of(outcome.events.iter().rev());

    // Anchored on the oldest message when the page has one, and on the oldest
    // raw event when it does not.
    //
    // A page can fetch nothing the list shows: a run of membership changes, or
    // reactions, or redactions of events older than the page. Reporting `None`
    // for those reads to the caller as the start of the room, so the scroll
    // stops asking and the scrollback dead-ends short of its actual beginning.
    // The raw event id keeps the token non-empty, which is what tells the UI
    // there is more, and it resolves as an anchor next time because the event
    // is in the cache whether or not it renders.
    let next_token = (!outcome.reached_start)
        .then(|| {
            oldest_message_id(&effects)
                .map(ToString::to_string)
                // Newest-first, so the oldest event of the page is the last.
                .or_else(|| oldest_event_id(&outcome.events).map(|id| id.to_string()))
        })
        .flatten();

    let display_names = resolve_display_names(&room, &effects).await;

    Ok(MessagePage {
        effects,
        next_token,
        display_names,
    })
}

/// Event ID of the oldest event in a batch.
fn oldest_event_id(newest_first: &[TimelineEvent]) -> Option<OwnedEventId> {
    newest_first.iter().rev().find_map(TimelineEvent::event_id)
}

/// Event ID of the oldest message in a page.
fn oldest_message_id(effects: &[EventEffect]) -> Option<&EventId> {
    effects.iter().find_map(|effect| match effect {
        EventEffect::New(message) => Some(&*message.event_id),
        _ => None,
    })
}

/// Get the display name this account has in a room.
///
/// # Arguments
/// * `client_state` - The client state containing the Matrix client.
/// * `room_id` - The room ID to query.
pub async fn own_display_name(
    client_state: ClientState,
    room_id: OwnedRoomId,
) -> Result<String, String> {
    let client = super::get_active_client(&client_state).await?;

    let room = client
        .get_room(&room_id)
        .ok_or_else(|| format!("Room {room_id} not found"))?;

    Ok(members::own_display_name(&room).await)
}

/// Send a plain text message to a room.
///
/// # Arguments
/// * `client_state` - The client state containing the Matrix client.
/// * `room_id` - The target room ID.
/// * `body` - The text content of the message.
/// * `txn_id` - Transaction ID for this send.
pub async fn send_message(
    client_state: ClientState,
    room_id: OwnedRoomId,
    body: String,
    txn_id: OwnedTransactionId,
) -> Result<OwnedEventId, String> {
    trace!("Sending message to room: {}", room_id);

    if body.trim().is_empty() {
        return Err("message content is required".to_string());
    }

    let client = super::get_active_client(&client_state).await?;

    let room = client
        .get_room(&room_id)
        .ok_or_else(|| format!("Room {room_id} not found"))?;

    // Sending into a room that was only previewed or has already been left fails at the homeserver
    // with a permission error, so it's rejected here where the reason can be stated plainly.
    if room.state() != RoomState::Joined {
        return Err(format!("Not joined to room {room_id}"));
    }

    let response = room
        .send(RoomMessageEventContent::text_plain(body))
        .with_transaction_id(txn_id)
        .await
        .map_err(|e| format!("Failed to send message to room {room_id}: {e}"))?;

    let event_id = response.response.event_id;
    debug!("Sent message {} to room {}", event_id, room_id);

    Ok(event_id)
}

/// Resolve the display name of every sender in a page.
///
/// Only [`EventEffect::New`] carries a sender; an edit or a redaction is
/// labelled by the row it lands on, which already has its name.
///
/// # Arguments
/// * `room` - The room the messages were sent in.
/// * `effects` - The page whose senders to resolve.
async fn resolve_display_names(
    room: &Room,
    effects: &[EventEffect],
) -> HashMap<OwnedUserId, String> {
    members::display_names(
        room,
        effects.iter().filter_map(|effect| match effect {
            EventEffect::New(message) => Some(&*message.sender),
            _ => None,
        }),
    )
    .await
}

#[cfg(test)]
#[path = "../../tests/unit/commands/messages.rs"]
mod tests;

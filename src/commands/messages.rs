use std::collections::HashMap;

use matrix_sdk::deserialized_responses::TimelineEvent;
use matrix_sdk::event_cache::{RoomEventCache, RoomEventCacheSubscriber};
use matrix_sdk::{Room, RoomState};
use ruma::events::room::message::RoomMessageEventContent;
use ruma::{EventId, OwnedEventId, OwnedRoomId, OwnedTransactionId, OwnedUserId, RoomId};
use tokio::sync::Mutex;
use tracing::{debug, trace};

use crate::ClientState;
use crate::rooms::members;
use crate::rooms::messages::{EventEffect, effects_of};

/// Event cache subscription for the currently active room.
static ACTIVE_ROOM_SUBSCRIPTION: Mutex<Option<ActiveRoomSubscription>> = Mutex::const_new(None);

struct ActiveRoomSubscription {
    room_id: OwnedRoomId,
    _subscriber: RoomEventCacheSubscriber,
}

/// Take over the active-room subscription for `room_id`, releasing the previous
/// room's so it shrinks, and return the events already loaded for the new one.
///
/// Subscribing hands back the current events anyway, so this stands in for the
/// [`RoomEventCache::events`] read the caller would otherwise do rather than
/// adding a second copy of the same list.
///
/// # Arguments
/// * `room_id` - The room being opened.
/// * `cache` - That room's event cache.
async fn subscribe_active_room(
    room_id: &RoomId,
    cache: &RoomEventCache,
) -> Result<Vec<TimelineEvent>, String> {
    let mut active = ACTIVE_ROOM_SUBSCRIPTION.lock().await;

    // Reopening the room that already holds the subscription. Releasing and
    // retaking it would shrink the very room the user is about to read.
    if active
        .as_ref()
        .is_some_and(|held| &*held.room_id == room_id)
    {
        return cache
            .events()
            .await
            .map_err(|e| format!("Failed to read local event cache: {e}"));
    }

    // Released before the new one is taken, so the room being left shrinks even
    // if subscribing to the room being opened fails.
    *active = None;

    let (events, subscriber) = cache
        .subscribe()
        .await
        .map_err(|e| format!("Failed to subscribe to the event cache: {e}"))?;
    *active = Some(ActiveRoomSubscription {
        room_id: room_id.to_owned(),
        _subscriber: subscriber,
    });
    Ok(events)
}

/// One page of classified, oldest-first events plus the event id to pass back
/// as `from` to load the next (older) page.
///
/// The page carries effects rather than finished rows, so the caller folds them
/// into the message model itself. Edits and redactions whose target is not in
/// this page survive as effects and settle whenever the page holding their
/// target arrives, which a page of finished rows could not express.
pub struct MessagePage {
    /// Oldest-first, in the order they must be applied.
    pub effects: Vec<EventEffect>,
    pub next_token: Option<String>,
    /// Display name for each sender in `effects`.
    pub display_names: HashMap<OwnedUserId, String>,
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
) -> Result<MessagePage, String> {
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

    // Opening a room takes the subscription over from whichever room held it.
    // Paging further back inside the room that already holds it must leave it
    // where it is, since dropping it would unload the scrollback being read.
    let cached = match &anchor {
        None => subscribe_active_room(&room_id, &cache).await?,
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

    let next_token = (!outcome.reached_start)
        .then(|| oldest_message_id(&effects).map(ToString::to_string))
        .flatten();

    let display_names = resolve_display_names(&room, &effects).await;

    Ok(MessagePage {
        effects,
        next_token,
        display_names,
    })
}

/// Event id of the oldest message in a page, which is the anchor the next
/// (older) page is fetched from. `None` when the page carries no message of its
/// own, only changes to messages elsewhere.
fn oldest_message_id(effects: &[EventEffect]) -> Option<&EventId> {
    effects.iter().find_map(|effect| match effect {
        EventEffect::New(message) => Some(&*message.event_id),
        _ => None,
    })
}

/// The display name this account has in a room.
///
/// Read from the room's stored member state, so it costs no network round trip. The UI resolves it
/// when a channel is opened and holds onto it, which is what lets a message being sent be labelled
/// at the moment it is typed, before there is an echoed event with a sender to resolve.
///
/// # Arguments
/// * `client_state` - The client state containing the Matrix client to read through.
/// * `room_id` - The room whose member state carries the name.
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
/// The event is handed to the SDK's send queue, so it is retried across reconnects and encrypted
/// first if the room is. The returned event id is the one the homeserver assigned, which is what the
/// live sync handler will echo back for this message.
///
/// # Arguments
/// * `client_state` - The client state containing the Matrix client to send with.
/// * `room_id` - The room to send the message to.
/// * `body` - The message text. Sent as `m.text`.
/// * `txn_id` - Caller-chosen transaction id for this send. The homeserver puts it back in the
///   echoed event's `unsigned.transaction_id`, but only for the device that sent it, which is how
///   the UI recognises its own message and settles the row it is already showing for it. Must never
///   be reused, since the homeserver treats a repeat as a retry of the same send.
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

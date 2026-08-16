use std::collections::HashMap;

use matrix_sdk::Room;
use matrix_sdk::deserialized_responses::TimelineEvent;
use matrix_sdk::event_cache::{RoomEventCache, RoomEventCacheSubscriber};
use ruma::{EventId, OwnedEventId, OwnedRoomId, OwnedUserId, RoomId, UserId};
use tokio::sync::Mutex;
use tracing::debug;

use crate::ClientState;
use crate::rooms::members;
use crate::rooms::messages::{MessageStore, StoredMessage};

/// The event cache subscription belonging to the room the user has open.
///
/// A [`RoomEventCacheSubscriber`] is how the SDK is told that a room is being
/// looked at. Dropping the last one held for a room notifies the SDK's
/// auto-shrink task, which unloads every chunk of that room's in-memory
/// timeline but the last one. The persisted copy is left alone, so scrollback
/// still comes back without a network round trip.
///
/// Nothing else in the client ever takes one, so without this the count never
/// leaves zero, the notification is never sent, and the in-memory timeline of
/// every room opened this session stays live for the rest of the process.
static ACTIVE_ROOM_SUBSCRIPTION: Mutex<Option<ActiveRoomSubscription>> = Mutex::const_new(None);

struct ActiveRoomSubscription {
    room_id: OwnedRoomId,
    /// Never read from. It is held for its `Drop`, which is what asks the SDK
    /// to shrink the room once it stops being the open one. The updates it
    /// buffers in the meantime are bounded by the broadcast channel's own
    /// capacity, so an unread subscriber cannot grow without limit.
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

/// One page of resolved, oldest-first messages plus the event id to pass
/// back as `from` to load the next (older) page.
pub struct PaginatedMessages {
    pub messages: Vec<StoredMessage>,
    pub next_token: Option<String>,
    /// Display name for each sender in `messages`, keyed by user id. Resolved here rather than in
    /// the UI layer, since names live in the room's member state and reading it is async.
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
            if let Some(messages) = messages_from_slice(slice) {
                debug!(
                    "Served {} messages for room {} from the local event cache (no network fetch)",
                    messages.len(),
                    room_id
                );
                let next_token = messages.first().map(|m| m.event_id.to_string());
                let display_names = resolve_display_names(&room, &messages).await;
                return Ok(PaginatedMessages {
                    messages,
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

    let display_names = resolve_display_names(&room, &messages).await;

    Ok(PaginatedMessages {
        messages,
        next_token,
        display_names,
    })
}

/// Resolve the display name of every sender in a page of messages.
///
/// Senders whose id does not parse are skipped, which leaves the UI falling back to the raw string
/// it already holds.
///
/// # Arguments
/// * `room` - The room the messages were sent in.
/// * `messages` - The page whose senders to resolve.
async fn resolve_display_names(
    room: &Room,
    messages: &[StoredMessage],
) -> HashMap<OwnedUserId, String> {
    // Collected rather than passed lazily, so no borrow of `messages` is held across the await and
    // the returned future stays `Send`.
    let senders: Vec<OwnedUserId> = messages
        .iter()
        .filter_map(|m| UserId::parse(m.sender.as_ref()).ok())
        .collect();
    members::display_names(room, senders).await
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

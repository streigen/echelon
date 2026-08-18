use std::sync::Arc;

use matrix_sdk::deserialized_responses::TimelineEvent;
use matrix_sdk::event_cache::{RoomEventCache, RoomEventCacheSubscriber};
use ruma::{OwnedRoomId, RoomId};
use tokio::sync::Mutex;

/// Where a [`ClientHandler`] keeps the open room's subscription.
///
/// Behind an `Arc` so a caller can take it out of the handler and drop the
/// `ClientState` read guard before locking it. Held as a bare field it would
/// have to stay borrowed across the subscribe below, which blocks login and
/// logout, both of which write that lock, for the length of a network call.
///
/// [`ClientHandler`]: super::ClientHandler
pub(crate) type ActiveRoomSlot = Arc<Mutex<Option<ActiveRoomSubscription>>>;

/// A live subscription to one room's event cache.
///
/// Keeping this subscribed is what stops the room's loaded events from being
/// unloaded under the scrollback the user is reading. Only the open room holds
/// one, so every other room is free to shrink.
pub(crate) struct ActiveRoomSubscription {
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
/// * `slot` - The client's subscription slot, from `ClientHandler::active_room_slot`.
/// * `room_id` - The room being opened.
/// * `cache` - That room's event cache.
pub(crate) async fn subscribe_active_room(
    slot: &Mutex<Option<ActiveRoomSubscription>>,
    room_id: &RoomId,
    cache: &RoomEventCache,
) -> Result<Vec<TimelineEvent>, String> {
    let mut active = slot.lock().await;

    // Reopening the room that already holds the subscription. Releasing and
    // retaking it would shrink the very room the user is about to read.
    //
    // Comparing ids alone is enough because the slot belongs to one client: a
    // subscription found here was taken against the same event cache being
    // asked about. A room id is global to Matrix rather than per account, so
    // against a process-wide slot this test would confuse the same room seen
    // through two different accounts.
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

use std::sync::RwLock;

use ruma::{OwnedRoomId, RoomId};

pub(crate) mod members;
pub(crate) mod messages;
pub(crate) mod room_types;

/// The room the user has open, mirrored out of `UiState.active-room-id`.
///
/// The UI holds the real answer, but a Slint global can only be read on the UI
/// thread, and the sync loop's event handler runs on a tokio worker. Without a
/// copy it can reach, that handler has to do its work first and ask afterwards,
/// from inside `invoke_from_event_loop`, by which point the work is already paid
/// for. See [`crate::events::client_events::ClientEvents::on_message`].
///
/// Written only from the UI thread, alongside the property it mirrors, so the two
/// never disagree for longer than one read.
static ACTIVE_ROOM: RwLock<Option<OwnedRoomId>> = RwLock::new(None);

/// Point [`ACTIVE_ROOM`] at the room being opened. `None` while no channel is
/// open, which is what the process starts out as.
///
/// Must be called from the UI thread, in step with `UiState.active-room-id`.
///
/// # Arguments
/// * `room_id` - The room now open, or `None` to clear.
pub(crate) fn set_active_room(room_id: Option<OwnedRoomId>) {
    // A poisoned lock means a thread panicked while holding it. The value behind
    // it is one `Option`, which is never left half written, so the recovered
    // guard is as good as an unpoisoned one and is worth more than taking the
    // sync loop down in sympathy.
    let mut active = ACTIVE_ROOM.write().unwrap_or_else(|e| e.into_inner());
    *active = room_id;
}

/// Whether `room_id` is the room the user has open.
///
/// Safe to call from any thread. A caller racing a channel switch can read the
/// room being left, which is the same answer the UI thread would have given it a
/// moment earlier.
///
/// # Arguments
/// * `room_id` - The room to test.
pub(crate) fn is_active_room(room_id: &RoomId) -> bool {
    let active = ACTIVE_ROOM.read().unwrap_or_else(|e| e.into_inner());
    active.as_deref() == Some(room_id)
}

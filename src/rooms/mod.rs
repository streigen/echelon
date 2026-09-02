use std::sync::RwLock;

use ruma::{OwnedRoomId, RoomId};

pub(crate) mod members;
pub(crate) mod messages;
pub(crate) mod room_types;

/// Currently open room ID, mirrored for background workers.
static ACTIVE_ROOM: RwLock<Option<OwnedRoomId>> = RwLock::new(None);

/// Update [`ACTIVE_ROOM`] with the currently open room ID.
pub(crate) fn set_active_room(room_id: Option<OwnedRoomId>) {
    let mut active = ACTIVE_ROOM.write().unwrap_or_else(|e| e.into_inner());
    *active = room_id;
}

/// Check if `room_id` is the currently open room.
pub(crate) fn is_active_room(room_id: &RoomId) -> bool {
    let active = ACTIVE_ROOM.read().unwrap_or_else(|e| e.into_inner());
    active.as_deref() == Some(room_id)
}

#[cfg(test)]
#[path = "../../tests/unit/rooms/mod.rs"]
mod tests;

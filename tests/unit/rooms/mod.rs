//! Unit suite for [`crate::rooms`].
//!
//! The open room is held in a process-wide static so background workers can ask
//! about it without a handle on the UI. That makes these cases share one piece
//! of state, so each takes [`GUARD`] for the whole of its run and hands the
//! static back the way it found it.

use super::*;
use std::sync::{Mutex, MutexGuard};

/// Serializes access to `ACTIVE_ROOM`.
///
/// The static is shared by the whole test binary, so two cases running at once
/// would each see the other's writes.
static GUARD: Mutex<()> = Mutex::new(());

/// Take [`GUARD`] and clear the active room, leaving the static as a freshly
/// started process would have it.
///
/// Recovers from a poisoned guard because a case that panicked mid-test has
/// nothing to teach the next one; the state it left behind is cleared here
/// anyway.
fn exclusive() -> MutexGuard<'static, ()> {
    let guard = GUARD.lock().unwrap_or_else(|e| e.into_inner());
    set_active_room(None);
    guard
}

/// A room id, parsed.
///
/// # Arguments
/// * `id` - The room id in `!local:server` form.
fn room(id: &str) -> OwnedRoomId {
    RoomId::parse(id).expect("a valid room id")
}

#[test]
fn no_room_is_active_before_one_is_opened() {
    let _guard = exclusive();

    assert!(!is_active_room(&room("!room:example.org")));
}

#[test]
fn the_room_that_was_set_is_the_active_one() {
    let _guard = exclusive();
    set_active_room(Some(room("!open:example.org")));

    assert!(is_active_room(&room("!open:example.org")));
}

#[test]
fn another_room_is_not_the_active_one() {
    // This is the check that decides whether a live event is folded into the
    // view. Answering yes for the wrong room would append somebody else's
    // message to the open one.
    let _guard = exclusive();
    set_active_room(Some(room("!open:example.org")));

    assert!(!is_active_room(&room("!other:example.org")));
}

#[test]
fn opening_a_second_room_replaces_the_first() {
    let _guard = exclusive();
    set_active_room(Some(room("!first:example.org")));
    set_active_room(Some(room("!second:example.org")));

    assert!(!is_active_room(&room("!first:example.org")));
    assert!(is_active_room(&room("!second:example.org")));
}

#[test]
fn closing_the_room_leaves_nothing_active() {
    let _guard = exclusive();
    set_active_room(Some(room("!open:example.org")));
    set_active_room(None);

    assert!(!is_active_room(&room("!open:example.org")));
}

#[test]
fn a_panic_while_the_lock_was_held_does_not_wedge_the_static() {
    // Both accessors recover from poisoning rather than unwrapping. A panic
    // anywhere near this lock would otherwise take every later live event with
    // it, so the room would stop updating until the app was restarted.
    let _guard = exclusive();

    std::thread::spawn(|| {
        let _held = ACTIVE_ROOM.write().expect("the lock is not yet poisoned");
        panic!("poisoning the lock");
    })
    .join()
    .expect_err("the thread panicked on purpose");
    assert!(
        ACTIVE_ROOM.is_poisoned(),
        "the panic should have poisoned it"
    );

    set_active_room(Some(room("!after:example.org")));

    assert!(is_active_room(&room("!after:example.org")));

    ACTIVE_ROOM.clear_poison();
    set_active_room(None);
}

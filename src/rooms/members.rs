use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use matrix_sdk::Room;
use ruma::UserId;
use tracing::warn;

/// The name a member goes by in a room, or `None` when the room's state has no member event for
/// them. Reads only what is already in the store, so it never hits the network.
///
/// A member who set no display name falls back to the localpart of their user id, and a name shared
/// with another member of the room is suffixed with the user id, as the spec requires, so two people
/// both calling themselves "alice" stay apart.
///
/// # Arguments
/// * `room` - The room whose member state carries the name.
/// * `user_id` - The member to resolve.
async fn stored_name(room: &Room, user_id: &UserId) -> Option<String> {
    match room.get_member_no_sync(user_id).await {
        Ok(Some(member)) if member.name_ambiguous() => {
            Some(format!("{} ({})", member.name(), member.user_id()))
        }
        Ok(Some(member)) => Some(member.name().to_owned()),
        Ok(None) => None,
        Err(e) => {
            warn!("Failed to resolve display name for {user_id}: {e}");
            None
        }
    }
}

/// The name to show for a single room member, falling back to their user id when the room has no
/// member event for them. That fallback is what the UI showed for every sender before names were
/// resolved at all, so it is never worse than before.
///
/// Nothing is fetched here. Lazy loading means the server bundles a sender's member event with the
/// events they sent, so a member who is missing from a live message's room state is one this client
/// has genuinely never seen.
///
/// # Arguments
/// * `room` - The room the message was sent in.
/// * `user_id` - The sender to resolve.
pub async fn display_name(room: &Room, user_id: &UserId) -> String {
    stored_name(room, user_id)
        .await
        .unwrap_or_else(|| user_id.to_string())
}

/// The name this account goes by in `room`.
///
/// Resolved exactly like any other member's, so a message labels itself the same way the copy the
/// server echoes back will be labelled once it arrives.
///
/// # Arguments
/// * `room` - The room to read our own member event from.
pub async fn own_display_name(room: &Room) -> String {
    display_name(room, room.own_user_id()).await
}

/// Resolve the display names of every distinct sender in a page of messages, keyed by the same
/// interned sender string the messages themselves carry.
///
/// Keyed by `Arc<str>` rather than by `OwnedUserId` so that looking a sender up costs a hash of a
/// string the caller already holds. A map keyed by the typed id would make every row parse and
/// allocate an id purely to probe it, and ruma's owned ids are `Box<str>` unless the
/// `ruma_identifiers_storage` cfg says otherwise, so each of those is a real allocation. The typed
/// id is still what the store is asked with, but that happens once per distinct sender here, and it
/// borrows out of the key rather than allocating.
///
/// Backfilled pages can reach further back than the member state the server bundled with them, so
/// anything still unresolved after reading the store is worth one member request for the room. That
/// request is skipped entirely when the store already answered for every sender, which is the
/// common case and matters in a room with a large membership.
///
/// # Arguments
/// * `room` - The room the messages were sent in.
/// * `senders` - The senders to resolve, duplicates included. A sender whose id does not parse is
///   left out, which leaves the caller falling back to the raw string it already has.
pub async fn display_names<I>(room: &Room, senders: I) -> HashMap<Arc<str>, String>
where
    I: IntoIterator<Item = Arc<str>>,
{
    let senders: HashSet<Arc<str>> = senders.into_iter().collect();
    let mut names: HashMap<Arc<str>, String> = HashMap::with_capacity(senders.len());
    let mut missing: Vec<Arc<str>> = Vec::new();

    for sender in senders {
        let Ok(user_id) = <&UserId>::try_from(sender.as_ref()) else {
            continue;
        };
        match stored_name(room, user_id).await {
            Some(name) => {
                names.insert(sender, name);
            }
            None => missing.push(sender),
        }
    }

    if missing.is_empty() {
        return names;
    }

    // A failed sync is not fatal, since the unresolved senders fall back to their user ids.
    if let Err(e) = room.sync_members().await {
        warn!("Failed to sync members for room {}: {e}", room.room_id());
    }

    for sender in missing {
        // Reparsed rather than carried through the vec above, since a borrow taken from `sender`
        // cannot be stored alongside the `sender` it points into. It parsed once already, so this
        // cannot fail.
        let Ok(user_id) = <&UserId>::try_from(sender.as_ref()) else {
            continue;
        };
        let name = display_name(room, user_id).await;
        names.insert(sender, name);
    }
    names
}

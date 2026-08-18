use std::collections::{HashMap, HashSet};

use matrix_sdk::Room;
use ruma::{OwnedUserId, UserId};
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

/// The display name to show for a room member, falling back to their user ID.
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

/// Resolve the display names of senders in a page of messages.
///
/// Senders are borrowed rather than owned, and deduped before anything is
/// resolved, so a page of fifty messages from five people allocates five owned
/// ids for the map keys instead of one per message. `OwnedUserId` is a
/// `Box<str>` unless ruma is built with `ruma_identifiers_storage = "Arc"`, so
/// cloning one is an allocation and worth not doing per row.
///
/// # Arguments
/// * `room` - The room the messages were sent in.
/// * `senders` - The senders to resolve, in any order and with repeats.
pub async fn display_names<'a, I>(room: &Room, senders: I) -> HashMap<OwnedUserId, String>
where
    I: IntoIterator<Item = &'a UserId>,
{
    let senders: HashSet<&UserId> = senders.into_iter().collect();
    let mut names: HashMap<OwnedUserId, String> = HashMap::with_capacity(senders.len());
    let mut missing: Vec<&UserId> = Vec::new();

    for sender in senders {
        match stored_name(room, sender).await {
            Some(name) => {
                names.insert(sender.to_owned(), name);
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
        let name = display_name(room, sender).await;
        names.insert(sender.to_owned(), name);
    }
    names
}

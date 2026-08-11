use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use matrix_sdk::deserialized_responses::TimelineEvent;
use ruma::events::room::message::{MessageType, Relation, SyncRoomMessageEvent};
use ruma::events::room::redaction::SyncRoomRedactionEvent;
use ruma::events::{AnySyncMessageLikeEvent, AnySyncTimelineEvent};
use ruma::{OwnedEventId, OwnedUserId, UserId};
use tracing::warn;

/// A message as displayed in the UI, after edits/redactions have been
/// folded into the original event.
#[derive(Debug, Clone)]
pub struct StoredMessage {
    pub event_id: OwnedEventId,
    /// Interned per-room via [`MessageStore::intern_sender`] — repeated
    /// senders share one allocation instead of each message carrying its
    /// own copy of the same user id.
    pub sender: Arc<str>,
    pub body: String,
    pub origin_server_ts: u64,
    pub edited: bool,
    pub redacted: bool,
}

/// Accumulates timeline events into a deduped, edit/redaction-resolved
/// set of messages. Fed by both paginated backfill and the live sync
/// event handler, so both paths share exactly one code path for
/// dedup/edit/redaction handling.
#[derive(Default)]
pub struct MessageStore {
    /// Insertion-ordered messages, keyed by event id for O(1) lookup.
    messages: Vec<StoredMessage>,
    index: HashMap<OwnedEventId, usize>,

    /// Edits that arrived before we'd seen their target event yet
    /// (common during backward pagination, where newer events —
    /// including edits — are processed before the originals they
    /// target).
    pending_edits: HashMap<OwnedEventId, String>, // event_id -> new_body
    /// Redactions that arrived before their target event.
    pending_redactions: HashSet<OwnedEventId>,

    /// One shared allocation per distinct sender seen so far.
    sender_cache: HashMap<OwnedUserId, Arc<str>>,
}

impl MessageStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            messages: Vec::with_capacity(capacity),
            index: HashMap::with_capacity(capacity),
            ..Self::default()
        }
    }

    pub fn messages(&self) -> &[StoredMessage] {
        &self.messages
    }

    /// Consumes the store, returning the accumulated messages without
    /// cloning them.
    pub fn into_messages(self) -> Vec<StoredMessage> {
        self.messages
    }

    /// Feed a single raw timeline event into the store. Safe to call
    /// with duplicate events (from overlapping pagination/sync) and in
    /// any order.
    pub fn apply_raw(&mut self, event: &TimelineEvent) {
        let raw = event.raw();
        let Ok(deserialized) = raw.deserialize() else {
            warn!("Failed to deserialize timeline event, skipping");
            return;
        };
        self.apply(deserialized);
    }

    pub fn apply(&mut self, event: AnySyncTimelineEvent) {
        let AnySyncTimelineEvent::MessageLike(message_like) = event else {
            // State events (topic changes, membership, etc.) aren't
            // rendered as chat messages.
            return;
        };

        match message_like {
            AnySyncMessageLikeEvent::RoomMessage(room_message) => {
                self.apply_room_message(room_message)
            }
            AnySyncMessageLikeEvent::RoomRedaction(redaction) => {
                self.apply_redaction(redaction)
            }
            // TODO: reactions, stickers, polls, etc. — not rendered yet.
            _ => {}
        }
    }

    fn apply_room_message(&mut self, event: SyncRoomMessageEvent) {
        let SyncRoomMessageEvent::Original(original) = event else {
            // Already-redacted-at-sync-time events carry no body to show.
            return;
        };

        // Match by value (not by reference) so the edit's body can move
        // straight into `apply_edit` instead of being cloned out of a
        // borrow.
        if let Some(Relation::Replacement(replacement)) = original.content.relates_to {
            let new_body = body_of(replacement.new_content.msgtype);
            self.apply_edit(replacement.event_id, new_body);
            return;
        }

        if self.index.contains_key(&original.event_id) {
            // Already seen (pagination/sync overlap) — skip.
            return;
        }

        let sender = self.intern_sender(&original.sender);
        let event_id = original.event_id;
        let stored = StoredMessage {
            event_id: event_id.clone(),
            sender,
            body: body_of(original.content.msgtype),
            origin_server_ts: original.origin_server_ts.0.into(),
            edited: false,
            redacted: false,
        };

        let idx = self.messages.len();
        self.messages.push(stored);
        self.index.insert(event_id.clone(), idx);

        // Apply anything that arrived out of order, targeting this event.
        if let Some(new_body) = self.pending_edits.remove(&event_id) {
            self.messages[idx].body = new_body;
            self.messages[idx].edited = true;
        }
        if self.pending_redactions.remove(&event_id) {
            self.messages[idx].body.clear();
            self.messages[idx].redacted = true;
        }
    }

    fn apply_edit(&mut self, target: OwnedEventId, new_body: String) {
        if let Some(&idx) = self.index.get(&target) {
            self.messages[idx].body = new_body;
            self.messages[idx].edited = true;
        } else {
            self.pending_edits.insert(target, new_body);
        }
    }

    fn apply_redaction(&mut self, event: SyncRoomRedactionEvent) {
        // `redacts` moved from a top-level field to `content.redacts` in
        // room version 11; check both since we don't know the room's
        // version here.
        let target = match &event {
            SyncRoomRedactionEvent::Original(original) => original
                .redacts
                .clone()
                .or_else(|| original.content.redacts.clone()),
            SyncRoomRedactionEvent::Redacted(_) => None,
        };
        let Some(target) = target else {
            return;
        };

        if let Some(&idx) = self.index.get(&target) {
            self.messages[idx].body.clear();
            self.messages[idx].redacted = true;
        } else {
            self.pending_redactions.insert(target);
        }
    }

    /// Returns a shared handle to `sender`'s interned id, allocating one
    /// only the first time this sender is seen.
    fn intern_sender(&mut self, sender: &UserId) -> Arc<str> {
        if let Some(existing) = self.sender_cache.get(sender) {
            return existing.clone();
        }
        let interned: Arc<str> = Arc::from(sender.as_str());
        self.sender_cache.insert(sender.to_owned(), interned.clone());
        interned
    }
}

fn body_of(msgtype: MessageType) -> String {
    match msgtype {
        MessageType::Text(text) => text.body,
        MessageType::Notice(notice) => notice.body,
        MessageType::Emote(emote) => emote.body,
        other => other.body().to_string(),
    }
}

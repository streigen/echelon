use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use matrix_sdk::deserialized_responses::TimelineEvent;
use ruma::events::room::MediaSource;
use ruma::events::room::message::{MessageType, Relation, SyncRoomMessageEvent};
use ruma::events::room::redaction::SyncRoomRedactionEvent;
use ruma::events::sticker::{StickerEventContent, SyncStickerEvent};
use ruma::events::{AnySyncMessageLikeEvent, AnySyncTimelineEvent};
use ruma::{OwnedEventId, OwnedUserId, UserId};
use tracing::warn;

/// What kind of media an [`Attachment`] holds. This drives how the UI
/// renders it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachmentKind {
    Image,
    Video,
    Audio,
    File,
    Sticker,
}

impl AttachmentKind {
    /// Whether this kind renders as a raster preview, which decides if the
    /// fetch and patch path in `lib.rs` applies to it. This is the one place
    /// a new kind has to be classified. Everything else is kind agnostic.
    /// `Video` belongs here once there is a play affordance over the still.
    pub fn has_preview(self) -> bool {
        matches!(self, Self::Image | Self::Sticker)
    }
}

/// A media attachment, carrying just enough to fetch and render it later.
/// One shape covers every kind, so a new kind needs no new field here.
#[derive(Debug, Clone)]
pub struct Attachment {
    pub kind: AttachmentKind,
    /// The full-resolution file.
    pub source: MediaSource,
    /// Sender-provided thumbnail. This is preferred for chat display, and it
    /// is the only small option for encrypted media, since the homeserver
    /// cannot thumbnail content it cannot decrypt.
    pub thumbnail_source: Option<MediaSource>,
    /// Declared content type. Kinds without a renderer use it to label
    /// themselves, and a future file row can use it to pick an icon.
    pub mimetype: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

thread_local! {
    /// Attachments by event id. The UI model cannot carry a `MediaSource`,
    /// so a click or a scroll into view has only the event id and an index
    /// to go on. This turns those back into something fetchable. It is a
    /// `thread_local` because every access happens inside an
    /// `invoke_from_event_loop` closure. Entries are written only for the
    /// room on screen, and are never evicted.
    static ATTACHMENT_CACHE: std::cell::RefCell<HashMap<String, Vec<Attachment>>> =
        std::cell::RefCell::new(HashMap::new());
}

/// Remember an event's attachments so they can be fetched later.
pub fn cache_attachments(event_id: &str, attachments: &[Attachment]) {
    if attachments.is_empty() {
        return;
    }
    ATTACHMENT_CACHE.with(|cache| {
        cache
            .borrow_mut()
            .insert(event_id.to_string(), attachments.to_vec());
    });
}

/// Look up an attachment cached by [`cache_attachments`].
pub fn get_cached_attachment(event_id: &str, index: usize) -> Option<Attachment> {
    ATTACHMENT_CACHE.with(|cache| cache.borrow().get(event_id)?.get(index).cloned())
}

/// A message as displayed in the UI, after edits/redactions have been
/// folded into the original event.
#[derive(Debug, Clone)]
pub struct StoredMessage {
    pub event_id: OwnedEventId,
    /// Interned per-room via [`MessageStore::intern_sender`]. repeated
    /// senders share one allocation instead of each message carrying its
    /// own copy of the same user id.
    pub sender: Arc<str>,
    pub body: String,
    pub origin_server_ts: u64,
    pub edited: bool,
    pub redacted: bool,
    /// Empty for text-only messages.
    pub attachments: Vec<Attachment>,
}

/// Accumulates timeline events into a deduped, edit/redaction-resolved
/// set of messages. Fed by both paginated backfill and the live sync
/// event handler, so both paths share exactly one code path for
/// dedup/edit/redaction handling.
#[derive(Default)]
pub struct MessageStore {
    /// Insertion-ordered messages, keyed by event id.
    messages: Vec<StoredMessage>,
    index: HashMap<OwnedEventId, usize>,

    /// Edits that arrived before their target event
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
            AnySyncMessageLikeEvent::RoomRedaction(redaction) => self.apply_redaction(redaction),
            AnySyncMessageLikeEvent::Sticker(sticker) => self.apply_sticker(sticker),
            // TODO: deal with reactions and stuff later.
            _ => {}
        }
    }

    /// `m.sticker` is its own event type rather than an `m.room.message`
    /// msgtype, so it needs its own entry point. It still lands in the same
    /// [`StoredMessage`] shape as everything else, carrying a single
    /// [`AttachmentKind::Sticker`] attachment.
    fn apply_sticker(&mut self, event: SyncStickerEvent) {
        let SyncStickerEvent::Original(original) = event else {
            return;
        };
        let sender = self.intern_sender(&original.sender);
        let attachment = attachment_of_sticker(&original.content);
        self.push(StoredMessage {
            event_id: original.event_id,
            sender,
            body: original.content.body,
            origin_server_ts: original.origin_server_ts.0.into(),
            edited: false,
            redacted: false,
            attachments: vec![attachment],
        });
    }

    /// Append a message and settle any edit or redaction that arrived ahead
    /// of it. Duplicates are ignored, since pagination and sync overlap.
    fn push(&mut self, message: StoredMessage) {
        let event_id = message.event_id.clone();
        if self.index.contains_key(&event_id) {
            return;
        }

        let idx = self.messages.len();
        self.messages.push(message);
        self.index.insert(event_id.clone(), idx);

        if let Some(new_body) = self.pending_edits.remove(&event_id) {
            self.messages[idx].body = new_body;
            self.messages[idx].edited = true;
        }
        if self.pending_redactions.remove(&event_id) {
            self.messages[idx].body.clear();
            self.messages[idx].attachments.clear();
            self.messages[idx].redacted = true;
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
            let new_body = body_of(&replacement.new_content.msgtype);
            self.apply_edit(replacement.event_id, new_body);
            return;
        }

        let sender = self.intern_sender(&original.sender);
        self.push(StoredMessage {
            event_id: original.event_id,
            sender,
            body: body_of(&original.content.msgtype),
            origin_server_ts: original.origin_server_ts.0.into(),
            edited: false,
            redacted: false,
            attachments: attachment_of(&original.content.msgtype)
                .into_iter()
                .collect(),
        });
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
            self.messages[idx].attachments.clear();
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
        self.sender_cache
            .insert(sender.to_owned(), interned.clone());
        interned
    }
}

fn body_of(msgtype: &MessageType) -> String {
    match msgtype {
        MessageType::Text(text) => text.body.clone(),
        MessageType::Notice(notice) => notice.body.clone(),
        MessageType::Emote(emote) => emote.body.clone(),
        other => other.body().to_string(),
    }
}

/// Pull the media attachment out of an `m.room.message` msgtype, if it has
/// one. Stickers are their own timeline event and come in through
/// [`attachment_of_sticker`] instead. This is `pub(crate)` so the live sync
/// handler and the paginated fetch path share a single extractor.
pub(crate) fn attachment_of(msgtype: &MessageType) -> Option<Attachment> {
    // Each msgtype has a distinct ruma info type and there is no common
    // trait over them, so the fields are read structurally instead.
    macro_rules! visual {
        ($kind:expr, $m:expr) => {{
            let info = $m.info.as_deref();
            (
                $kind,
                &$m.source,
                info.and_then(|i| i.thumbnail_source.clone()),
                info.and_then(|i| i.mimetype.clone()),
                info.and_then(|i| i.width),
                info.and_then(|i| i.height),
            )
        }};
    }

    let (kind, source, thumbnail_source, mimetype, width, height) = match msgtype {
        MessageType::Image(m) => visual!(AttachmentKind::Image, m),
        MessageType::Video(m) => visual!(AttachmentKind::Video, m),
        MessageType::Audio(m) => {
            let mimetype = m.info.as_deref().and_then(|i| i.mimetype.clone());
            (AttachmentKind::Audio, &m.source, None, mimetype, None, None)
        }
        MessageType::File(m) => {
            let info = m.info.as_deref();
            (
                AttachmentKind::File,
                &m.source,
                info.and_then(|i| i.thumbnail_source.clone()),
                info.and_then(|i| i.mimetype.clone()),
                None,
                None,
            )
        }
        _ => return None,
    };

    Some(Attachment {
        kind,
        source: source.clone(),
        thumbnail_source,
        mimetype,
        width: width.map(uint_to_u32),
        height: height.map(uint_to_u32),
    })
}

/// The same, for the standalone `m.sticker` event. A sticker is an image
/// whose info block is mandatory rather than optional.
fn attachment_of_sticker(content: &StickerEventContent) -> Attachment {
    Attachment {
        kind: AttachmentKind::Sticker,
        source: content.source.clone().into(),
        thumbnail_source: content.info.thumbnail_source.clone(),
        mimetype: content.info.mimetype.clone(),
        width: content.info.width.map(uint_to_u32),
        height: content.info.height.map(uint_to_u32),
    }
}

/// Convert a `ruma::UInt` to a `u32`, saturating rather than failing. These
/// values are only display hints for sizing.
fn uint_to_u32(value: ruma::UInt) -> u32 {
    u32::try_from(u64::from(value)).unwrap_or(u32::MAX)
}

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use matrix_sdk::deserialized_responses::TimelineEvent;
use ruma::events::room::MediaSource;
use ruma::events::room::message::{MessageType, Relation, SyncRoomMessageEvent};
use ruma::events::room::redaction::SyncRoomRedactionEvent;
use ruma::events::sticker::{StickerEventContent, SyncStickerEvent};
use ruma::events::{AnySyncMessageLikeEvent, AnySyncTimelineEvent};
use ruma::{EventId, OwnedEventId, OwnedRoomId, OwnedUserId, RoomId, UserId};
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

    /// Whether to offer "save to disk" for this kind. Everything but a sticker is a file the sender
    /// chose to send and the user may want to keep. A sticker is one image out of a pack (MSC2545),
    /// has no name of its own, and is added by its pack rather than saved on its own.
    pub fn is_savable(self) -> bool {
        !matches!(self, Self::Sticker)
    }
}

/// Largest attachment that will be pulled down and decoded for an inline preview.
///
/// Past this the row shows a file card with a download button instead, so the bytes
/// only move if the user asks for them. A preview is fetched because a row scrolled
/// into view, which is nobody's decision, and this is the size past which that stops
/// being a reasonable thing to do uninvited.
///
/// This is not a memory bound, and must not be read as one. The decision is made from
/// the size the *sender* declared, and a sender that understates it is believed right
/// up until the bytes are already in memory. What actually bounds a decode is
/// `MAX_DECODE_ALLOC` and the downscale, both applied to the real data; what bounds
/// the transfer is nothing.
pub const MAX_PREVIEW_BYTES: u64 = 8 * 1024 * 1024;

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
    /// Sender-declared file name, which labels the row for kinds with no preview and seeds the save
    /// dialog. Sender-controlled, so it is only ever a suggestion.
    pub filename: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// Sender-declared size of the full file, in bytes.
    pub size: Option<u64>,
    /// Sender-declared size of `thumbnail_source`, in bytes.
    pub thumbnail_size: Option<u64>,
}

impl Attachment {
    /// Bytes an inline preview of this attachment would pull down, when that is
    /// knowable before asking for it.
    ///
    /// `None` means the cost cannot be predicted, which covers the case where the
    /// homeserver scales the image for us: unencrypted media with no sender
    /// thumbnail is fetched through the thumbnail endpoint, so the original's size
    /// says nothing about what crosses the wire. Mirrors the source selection in
    /// [`crate::commands::media::fetch_image`], and has to keep mirroring it.
    fn preview_bytes(&self) -> Option<u64> {
        match (&self.thumbnail_source, &self.source) {
            // A sender thumbnail is fetched as-is, whatever the original weighs.
            (Some(_), _) => self.thumbnail_size,
            // Scaled by the homeserver on the way out.
            (None, MediaSource::Plain(_)) => None,
            // The server cannot thumbnail what it cannot decrypt, so this is the
            // whole file.
            (None, MediaSource::Encrypted(_)) => self.size,
        }
    }

    /// Whether to render this attachment inline. False for a kind that has no raster
    /// to show, and for one whose preview would cost more than [`MAX_PREVIEW_BYTES`]
    /// to fetch. Those fall back to a file card, which offers the download the user
    /// can ask for deliberately.
    pub fn previewable(&self) -> bool {
        self.kind.has_preview()
            && self
                .preview_bytes()
                .is_none_or(|bytes| bytes <= MAX_PREVIEW_BYTES)
    }
}

/// Upper bound on how many events' attachments stay resolvable at once.
/// Leaving a room drops its entries outright, so in practice this is only
/// reached by scrolling a long way back inside one room. A page is 50
/// messages, so it covers many pages of scrollback even when every message
/// carries media.
const ATTACHMENT_CACHE_CAPACITY: usize = 2048;

/// Size the cache is trimmed back to once it overflows. Evicting a batch
/// keeps the victim search off the common insert path, at the cost of the
/// cache holding a little less than its cap most of the time.
const ATTACHMENT_CACHE_TRIM_TO: usize = ATTACHMENT_CACHE_CAPACITY * 3 / 4;

struct CachedAttachment {
    attachment: Attachment,
    /// Tick of the most recent insert or lookup, used to pick eviction
    /// victims. Every access bumps the clock, so these are unique.
    last_used: u64,
}

/// Attachments by room, then by event id. The UI model cannot carry a
/// `MediaSource`, so a click or a scroll into view has only a room id and an
/// event id to go on. This turns those back into something fetchable.
///
/// Grouping by room is what lets [`clear_room_attachments`] drop a room's
/// entries without touching anything else. The LRU cap is the backstop for
/// the one case that grouping does not cover: a single room scrolled back
/// far enough to accumulate more entries than it will ever show at once.
#[derive(Default)]
struct AttachmentCache {
    rooms: HashMap<OwnedRoomId, HashMap<OwnedEventId, CachedAttachment>>,
    /// Monotonic counter standing in for a clock. Ordering is all that
    /// matters here, and a counter cannot go backwards the way a wall clock
    /// can.
    tick: u64,
    /// Entry count across every room, kept alongside so the common insert
    /// does not have to walk the rooms to know whether it overflowed.
    len: usize,
}

impl AttachmentCache {
    fn insert(&mut self, room_id: &RoomId, event_id: &EventId, attachment: &Attachment) {
        self.tick += 1;
        let entry = CachedAttachment {
            attachment: attachment.clone(),
            last_used: self.tick,
        };
        let room = self.rooms.entry(room_id.to_owned()).or_default();
        if room.insert(event_id.to_owned(), entry).is_none() {
            self.len += 1;
        }
        if self.len > ATTACHMENT_CACHE_CAPACITY {
            self.evict_oldest();
        }
    }

    fn get(&mut self, room_id: &RoomId, event_id: &EventId) -> Option<Attachment> {
        self.tick += 1;
        let entry = self.rooms.get_mut(room_id)?.get_mut(event_id)?;
        // Touching on read is what keeps the rows currently on screen, which
        // are the ones the preview window looks up, out of the victim set.
        entry.last_used = self.tick;
        Some(entry.attachment.clone())
    }

    fn clear_room(&mut self, room_id: &RoomId) {
        if let Some(room) = self.rooms.remove(room_id) {
            self.len -= room.len();
        }
    }

    /// Drop the least recently used entries until only
    /// [`ATTACHMENT_CACHE_TRIM_TO`] remain.
    fn evict_oldest(&mut self) {
        let victims = self.len - ATTACHMENT_CACHE_TRIM_TO;
        let mut used: Vec<u64> = self
            .rooms
            .values()
            .flat_map(|room| room.values().map(|entry| entry.last_used))
            .collect();
        // Only the boundary value is needed, not a full ordering, so this
        // partitions in linear time instead of sorting. Ticks are unique, so
        // everything below the boundary is exactly the victim set.
        let (_, &mut cutoff, _) = used.select_nth_unstable(victims);

        self.rooms.retain(|_, room| {
            room.retain(|_, entry| entry.last_used >= cutoff);
            !room.is_empty()
        });
        self.len = self.rooms.values().map(HashMap::len).sum();
    }
}

thread_local! {
    /// It is a `thread_local` because every access happens inside an
    /// `invoke_from_event_loop` closure, on the UI thread.
    static ATTACHMENT_CACHE: std::cell::RefCell<AttachmentCache> =
        std::cell::RefCell::new(AttachmentCache::default());
}

/// Remember an event's attachment so it can be fetched later.
pub fn cache_attachment(room_id: &RoomId, event_id: &EventId, attachment: &Attachment) {
    ATTACHMENT_CACHE.with(|cache| cache.borrow_mut().insert(room_id, event_id, attachment));
}

/// Look up an attachment cached by [`cache_attachment`].
pub fn get_cached_attachment(room_id: &RoomId, event_id: &EventId) -> Option<Attachment> {
    ATTACHMENT_CACHE.with(|cache| cache.borrow_mut().get(room_id, event_id))
}

/// Forget everything cached for a room. Called when a room is left, and
/// again when one is opened, since opening refetches the first page and
/// rebuilds these entries from scratch anyway. Anything still on screen for
/// that room is on its way out with it.
pub fn clear_room_attachments(room_id: &RoomId) {
    ATTACHMENT_CACHE.with(|cache| cache.borrow_mut().clear_room(room_id));
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
    /// `None` for text-only messages. An `m.room.message` carries exactly one
    /// msgtype, so an event never has more than one attachment; several files
    /// are several events.
    pub attachment: Option<Attachment>,
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
            attachment: Some(attachment),
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
            self.messages[idx].attachment = None;
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
            attachment: attachment_of(&original.content.msgtype),
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
            self.messages[idx].attachment = None;
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
                $m.filename().to_owned(),
                info.and_then(|i| i.width),
                info.and_then(|i| i.height),
                info.and_then(|i| i.size),
                info.and_then(|i| i.thumbnail_info.as_ref()?.size),
            )
        }};
    }

    let (kind, source, thumbnail_source, mimetype, filename, width, height, size, thumbnail_size) =
        match msgtype {
            MessageType::Image(m) => visual!(AttachmentKind::Image, m),
            MessageType::Video(m) => visual!(AttachmentKind::Video, m),
            MessageType::Audio(m) => {
                let info = m.info.as_deref();
                (
                    AttachmentKind::Audio,
                    &m.source,
                    None,
                    info.and_then(|i| i.mimetype.clone()),
                    m.filename().to_owned(),
                    None,
                    None,
                    info.and_then(|i| i.size),
                    None,
                )
            }
            MessageType::File(m) => {
                let info = m.info.as_deref();
                (
                    AttachmentKind::File,
                    &m.source,
                    info.and_then(|i| i.thumbnail_source.clone()),
                    info.and_then(|i| i.mimetype.clone()),
                    m.filename().to_owned(),
                    None,
                    None,
                    info.and_then(|i| i.size),
                    info.and_then(|i| i.thumbnail_info.as_ref()?.size),
                )
            }
            _ => return None,
        };

    Some(Attachment {
        kind,
        source: source.clone(),
        thumbnail_source,
        mimetype,
        filename,
        width: width.map(uint_to_u32),
        height: height.map(uint_to_u32),
        size: size.map(u64::from),
        thumbnail_size: thumbnail_size.map(u64::from),
    })
}

/// The same, for the standalone `m.sticker` event. A sticker is an image whose info block is
/// mandatory rather than optional.
///
/// The file name is left empty because `m.sticker` defines `body` as a description of the image and
/// has no `filename` field to fall back on. Nothing needs one, since stickers are not savable.
fn attachment_of_sticker(content: &StickerEventContent) -> Attachment {
    Attachment {
        kind: AttachmentKind::Sticker,
        source: content.source.clone().into(),
        thumbnail_source: content.info.thumbnail_source.clone(),
        mimetype: content.info.mimetype.clone(),
        filename: String::new(),
        width: content.info.width.map(uint_to_u32),
        height: content.info.height.map(uint_to_u32),
        size: content.info.size.map(u64::from),
        thumbnail_size: content
            .info
            .thumbnail_info
            .as_ref()
            .and_then(|info| info.size)
            .map(u64::from),
    }
}

/// Convert a `ruma::UInt` to a `u32`, saturating rather than failing. These
/// values are only display hints for sizing.
fn uint_to_u32(value: ruma::UInt) -> u32 {
    u32::try_from(u64::from(value)).unwrap_or(u32::MAX)
}



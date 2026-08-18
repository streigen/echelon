use std::collections::HashMap;

use matrix_sdk::deserialized_responses::TimelineEvent;
use ruma::events::room::MediaSource;
use ruma::events::room::message::{MessageType, Relation, SyncRoomMessageEvent};
use ruma::events::room::redaction::SyncRoomRedactionEvent;
use ruma::events::sticker::{StickerEventContent, SyncStickerEvent};
use ruma::events::{AnySyncMessageLikeEvent, AnySyncTimelineEvent};
use ruma::{EventId, OwnedEventId, OwnedRoomId, OwnedUserId, RoomId};
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

/// Maximum attachment size (8MB) downloaded and decoded for inline preview.
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

/// In-memory cache mapping (room_id, event_id) to media attachments.
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

/// Clear all cached attachments for a given room.
pub fn clear_room_attachments(room_id: &RoomId) {
    ATTACHMENT_CACHE.with(|cache| cache.borrow_mut().clear_room(room_id));
}

/// A message to add to the list, from an event that creates one rather than
/// changing one already there.
#[derive(Debug, Clone)]
pub struct NewMessage {
    pub event_id: OwnedEventId,
    pub sender: OwnedUserId,
    pub body: String,
    pub origin_server_ts: u64,
    /// `None` for text-only messages. An `m.room.message` carries exactly one
    /// msgtype, so an event never has more than one attachment; several files
    /// are several events.
    pub attachment: Option<Attachment>,
}

/// What one timeline event means for the message list.
///
/// Pure data: no container, no state, and nothing about where the list lives.
/// Both the paginated backfill and the live sync handler classify through
/// [`effect_of`], so there is exactly one definition of what an edit or a
/// redaction is, and each applies the answer to the message model itself
/// rather than to a copy of it.
#[derive(Debug, Clone)]
pub enum EventEffect {
    /// A message that was not in the list before.
    New(NewMessage),
    /// Replace the body of the message `target` stands for. The target may not
    /// be there, since an edit is free to arrive before the event it edits.
    Edit {
        target: OwnedEventId,
        new_body: String,
    },
    /// Blank the message `target` stands for. Absent for the same reason as on
    /// [`EventEffect::Edit`].
    Redact { target: OwnedEventId },
    /// Nothing the message list shows: a state event, a reaction, an event
    /// already redacted when it synced, or one that failed to deserialize.
    Ignore,
}

/// Classify a raw event out of the event cache, deserializing it first.
///
/// An event that will not deserialize is skipped rather than failing the page
/// it arrived in, since one unreadable event should not cost the rest.
pub fn effect_of_raw(event: &TimelineEvent) -> EventEffect {
    let Ok(deserialized) = event.raw().deserialize() else {
        warn!("Failed to deserialize timeline event, skipping");
        return EventEffect::Ignore;
    };
    effect_of(deserialized)
}

/// Classify a batch of raw events, dropping the ones the message list does not
/// show. Oldest-first in, oldest-first out, so the result applies in display
/// order.
pub fn effects_of<'a>(events: impl IntoIterator<Item = &'a TimelineEvent>) -> Vec<EventEffect> {
    events
        .into_iter()
        .map(effect_of_raw)
        .filter(|effect| !matches!(effect, EventEffect::Ignore))
        .collect()
}

/// Classify a deserialized timeline event. See [`EventEffect`].
pub fn effect_of(event: AnySyncTimelineEvent) -> EventEffect {
    let AnySyncTimelineEvent::MessageLike(message_like) = event else {
        // State events (topic changes, membership, etc.) aren't rendered as
        // chat messages.
        return EventEffect::Ignore;
    };

    match message_like {
        AnySyncMessageLikeEvent::RoomMessage(room_message) => effect_of_room_message(room_message),
        AnySyncMessageLikeEvent::RoomRedaction(redaction) => effect_of_redaction(redaction),
        AnySyncMessageLikeEvent::Sticker(sticker) => effect_of_sticker(sticker),
        // TODO: deal with reactions and stuff later.
        _ => EventEffect::Ignore,
    }
}

/// Classify an `m.room.message` on its own, for the live sync handler, which is
/// registered per event type and so already has the concrete type in hand.
/// [`effect_of`] routes here too, so the two paths cannot disagree.
pub fn effect_of_room_message(event: SyncRoomMessageEvent) -> EventEffect {
    let SyncRoomMessageEvent::Original(original) = event else {
        // Already-redacted-at-sync-time events carry no body to show.
        return EventEffect::Ignore;
    };

    // Matched by value (not by reference) so the edit's body can move straight
    // out instead of being cloned out of a borrow.
    if let Some(Relation::Replacement(replacement)) = original.content.relates_to {
        return EventEffect::Edit {
            target: replacement.event_id,
            new_body: body_of(&replacement.new_content.msgtype),
        };
    }

    EventEffect::New(NewMessage {
        event_id: original.event_id,
        sender: original.sender,
        body: body_of(&original.content.msgtype),
        origin_server_ts: original.origin_server_ts.0.into(),
        attachment: attachment_of(&original.content.msgtype),
    })
}

/// `m.sticker` is its own event type rather than an `m.room.message` msgtype,
/// so it needs its own arm. It still lands in the same [`NewMessage`] shape as
/// everything else, carrying a single [`AttachmentKind::Sticker`] attachment.
fn effect_of_sticker(event: SyncStickerEvent) -> EventEffect {
    let SyncStickerEvent::Original(original) = event else {
        return EventEffect::Ignore;
    };
    let attachment = attachment_of_sticker(&original.content);
    EventEffect::New(NewMessage {
        event_id: original.event_id,
        sender: original.sender,
        body: original.content.body,
        origin_server_ts: original.origin_server_ts.0.into(),
        attachment: Some(attachment),
    })
}

/// Classify an `m.room.redaction` on its own. See [`effect_of_room_message`].
pub fn effect_of_redaction(event: SyncRoomRedactionEvent) -> EventEffect {
    let SyncRoomRedactionEvent::Original(original) = event else {
        return EventEffect::Ignore;
    };
    // `redacts` moved from a top-level field to `content.redacts` in room
    // version 11; check both since we don't know the room's version here.
    match original.redacts.or(original.content.redacts) {
        Some(target) => EventEffect::Redact { target },
        None => EventEffect::Ignore,
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
/// [`attachment_of_sticker`] instead.
///
/// Private because every path now reaches it through [`effect_of`], rather than
/// each extracting attachments for itself.
fn attachment_of(msgtype: &MessageType) -> Option<Attachment> {
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



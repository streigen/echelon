use std::collections::HashMap;

use matrix_sdk::deserialized_responses::TimelineEvent;
use ruma::events::room::MediaSource;
use ruma::events::room::message::{MessageType, Relation, SyncRoomMessageEvent};
use ruma::events::room::redaction::SyncRoomRedactionEvent;
use ruma::events::sticker::{StickerEventContent, SyncStickerEvent};
use ruma::events::{AnySyncMessageLikeEvent, AnySyncTimelineEvent};
use ruma::{EventId, OwnedEventId, OwnedRoomId, OwnedUserId, RoomId};
use tracing::warn;

/// Kind of media held by an [`Attachment`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachmentKind {
    Image,
    Video,
    Audio,
    File,
    Sticker,
}

impl AttachmentKind {
    /// Whether this kind renders inline as a raster preview.
    pub fn has_preview(self) -> bool {
        matches!(self, Self::Image | Self::Sticker)
    }

    /// Whether this attachment kind can be saved to disk.
    pub fn is_savable(self) -> bool {
        !matches!(self, Self::Sticker)
    }
}

/// Maximum attachment size (8MB) downloaded and decoded for inline preview.
pub const MAX_PREVIEW_BYTES: u64 = 8 * 1024 * 1024;

/// Media attachment details required for fetching and rendering.
#[derive(Debug, Clone)]
pub struct Attachment {
    pub kind: AttachmentKind,
    /// Full-resolution file source.
    pub source: MediaSource,
    /// Sender-provided thumbnail source.
    pub thumbnail_source: Option<MediaSource>,
    /// Declared MIME content type.
    pub mimetype: Option<String>,
    /// Sender-declared file name.
    pub filename: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// Sender-declared full file size in bytes.
    pub size: Option<u64>,
    /// Sender-declared thumbnail size in bytes.
    pub thumbnail_size: Option<u64>,
}

/// Target resolution for fetching an attachment.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ImageSize {
    /// Inline display resolution.
    Display,
    /// Original full resolution.
    Full,
}

/// Selected media source and its declared byte size.
pub struct SourceChoice<'a> {
    pub source: &'a MediaSource,
    /// Bytes this source will pull down, if known ahead of time.
    pub declared_bytes: Option<u64>,
    /// Whether the homeserver must scale this image on fetch.
    pub server_scaled: bool,
}

impl Attachment {
    /// Pick the source that answers a request at `size`.
    pub fn source_for(&self, size: ImageSize) -> SourceChoice<'_> {
        match (size, &self.thumbnail_source) {
            (ImageSize::Display, Some(thumbnail)) => SourceChoice {
                source: thumbnail,
                declared_bytes: self.thumbnail_size,
                server_scaled: false,
            },
            (ImageSize::Display, None) if matches!(self.source, MediaSource::Plain(_)) => {
                SourceChoice {
                    source: &self.source,
                    declared_bytes: None,
                    server_scaled: true,
                }
            }
            _ => SourceChoice {
                source: &self.source,
                declared_bytes: self.size,
                server_scaled: false,
            },
        }
    }

    /// Whether to render this attachment inline based on preview capability and size limit.
    pub fn previewable(&self) -> bool {
        self.kind.has_preview()
            && self
                .source_for(ImageSize::Display)
                .declared_bytes
                .is_none_or(|bytes| bytes <= MAX_PREVIEW_BYTES)
    }
}

/// Maximum capacity for cached attachments.
const ATTACHMENT_CACHE_CAPACITY: usize = 2048;

/// Target capacity when trimming the cache.
const ATTACHMENT_CACHE_TRIM_TO: usize = ATTACHMENT_CACHE_CAPACITY * 3 / 4;

struct CachedAttachment {
    attachment: Attachment,
    /// Monotonic tick of last access, used for eviction.
    last_used: u64,
}

/// In-memory cache mapping (room_id, event_id) to media attachments.
#[derive(Default)]
struct AttachmentCache {
    rooms: HashMap<OwnedRoomId, HashMap<OwnedEventId, CachedAttachment>>,
    tick: u64,
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
        entry.last_used = self.tick;
        Some(entry.attachment.clone())
    }

    fn remove(&mut self, room_id: &RoomId, event_id: &EventId) {
        let Some(room) = self.rooms.get_mut(room_id) else {
            return;
        };
        if room.remove(event_id).is_some() {
            self.len -= 1;
        }
    }

    fn clear_room(&mut self, room_id: &RoomId) {
        if let Some(room) = self.rooms.remove(room_id) {
            self.len -= room.len();
        }
    }

    /// Drop the least recently used entries until [`ATTACHMENT_CACHE_TRIM_TO`] remain.
    fn evict_oldest(&mut self) {
        let victims = self.len - ATTACHMENT_CACHE_TRIM_TO;
        let mut used: Vec<u64> = self
            .rooms
            .values()
            .flat_map(|room| room.values().map(|entry| entry.last_used))
            .collect();
        let (_, &mut cutoff, _) = used.select_nth_unstable(victims);

        self.rooms.retain(|_, room| {
            room.retain(|_, entry| entry.last_used >= cutoff);
            !room.is_empty()
        });
        self.len = self.rooms.values().map(HashMap::len).sum();
    }
}

thread_local! {
    static ATTACHMENT_CACHE: std::cell::RefCell<AttachmentCache> =
        std::cell::RefCell::new(AttachmentCache::default());
}

/// Remember an event's attachment so it can be fetched later.
pub fn cache_attachment(room_id: &RoomId, event_id: &EventId, attachment: &Attachment) {
    ATTACHMENT_CACHE.with(|cache| cache.borrow_mut().insert(room_id, event_id, attachment));
}

/// Remove an event's attachment from the cache.
pub fn uncache_attachment(room_id: &RoomId, event_id: &EventId) {
    ATTACHMENT_CACHE.with(|cache| cache.borrow_mut().remove(room_id, event_id));
}

/// Look up an attachment cached by [`cache_attachment`].
pub fn get_cached_attachment(room_id: &RoomId, event_id: &EventId) -> Option<Attachment> {
    ATTACHMENT_CACHE.with(|cache| cache.borrow_mut().get(room_id, event_id))
}

/// Clear all cached attachments for a given room.
pub fn clear_room_attachments(room_id: &RoomId) {
    ATTACHMENT_CACHE.with(|cache| cache.borrow_mut().clear_room(room_id));
}

/// A message to add to the timeline.
#[derive(Debug, Clone)]
pub struct NewMessage {
    pub event_id: OwnedEventId,
    pub sender: OwnedUserId,
    pub body: String,
    pub origin_server_ts: u64,
    /// Optional media attachment.
    pub attachment: Option<Attachment>,
}

/// Timeline event classification for message list updates.
#[derive(Debug, Clone)]
pub enum EventEffect {
    /// A new message.
    New(NewMessage),
    /// Replace the body and attachment of a target message.
    Edit {
        target: OwnedEventId,
        new_body: String,
        new_attachment: Option<Attachment>,
    },
    /// Redact a target message.
    Redact { target: OwnedEventId },
    /// Ignored event (state event, reaction, deserialization failure, etc.).
    Ignore,
}

/// Classify a raw timeline event, deserializing it first.
pub fn effect_of_raw(event: &TimelineEvent) -> EventEffect {
    let Ok(deserialized) = event.raw().deserialize() else {
        warn!("Failed to deserialize timeline event, skipping");
        return EventEffect::Ignore;
    };
    effect_of(deserialized)
}

/// Classify a batch of raw timeline events, dropping ignored events.
pub fn effects_of<'a>(events: impl IntoIterator<Item = &'a TimelineEvent>) -> Vec<EventEffect> {
    events
        .into_iter()
        .map(effect_of_raw)
        .filter(|effect| !matches!(effect, EventEffect::Ignore))
        .collect()
}

/// Classify a deserialized timeline event into an [`EventEffect`].
pub fn effect_of(event: AnySyncTimelineEvent) -> EventEffect {
    let AnySyncTimelineEvent::MessageLike(message_like) = event else {
        return EventEffect::Ignore;
    };

    match message_like {
        AnySyncMessageLikeEvent::RoomMessage(room_message) => effect_of_room_message(room_message),
        AnySyncMessageLikeEvent::RoomRedaction(redaction) => effect_of_redaction(redaction),
        AnySyncMessageLikeEvent::Sticker(sticker) => effect_of_sticker(sticker),
        _ => EventEffect::Ignore,
    }
}

/// Classify an `m.room.message` event.
pub fn effect_of_room_message(event: SyncRoomMessageEvent) -> EventEffect {
    let SyncRoomMessageEvent::Original(original) = event else {
        return EventEffect::Ignore;
    };

    if let Some(Relation::Replacement(replacement)) = original.content.relates_to {
        return EventEffect::Edit {
            target: replacement.event_id,
            new_body: body_of(&replacement.new_content.msgtype),
            new_attachment: attachment_of(&replacement.new_content.msgtype),
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

/// Classify an `m.sticker` event.
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

/// Classify an `m.room.redaction` event.
pub fn effect_of_redaction(event: SyncRoomRedactionEvent) -> EventEffect {
    let SyncRoomRedactionEvent::Original(original) = event else {
        return EventEffect::Ignore;
    };
    // `redacts` moved to `content.redacts` in room version 11; check both.
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

/// Extract media attachment from an `m.room.message` msgtype, if present.
fn attachment_of(msgtype: &MessageType) -> Option<Attachment> {
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

/// Extract media attachment from an `m.sticker` event content.
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

/// Convert a `ruma::UInt` to a `u32`, saturating at `u32::MAX`.
fn uint_to_u32(value: ruma::UInt) -> u32 {
    u32::try_from(u64::from(value)).unwrap_or(u32::MAX)
}

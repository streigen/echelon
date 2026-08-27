//! Unit suite for [`crate::rooms::messages`].
//!
//! Compiled as an inner module of that file, so the private classifiers and the
//! attachment cache are in scope through `super::*`.

use super::*;
use crate::test_support::*;
use ruma::events::room::message::SyncRoomMessageEvent;
use ruma::events::room::redaction::SyncRoomRedactionEvent;
use ruma::events::sticker::SyncStickerEvent;
use serde_json::{Value, json};

/// A minimal `m.room.message` event carrying `content`.
///
/// # Arguments
/// * `content` - The event's content object.
fn message_event(content: Value) -> Value {
    json!({
        "type": "m.room.message",
        "event_id": "$message",
        "sender": "@alice:example.org",
        "origin_server_ts": 1_700_000_000_000u64,
        "content": content,
    })
}

/// The [`NewMessage`] a classified event produced, or a panic naming what it
/// produced instead.
///
/// # Arguments
/// * `effect` - The classification to unwrap.
fn expect_new(effect: EventEffect) -> NewMessage {
    match effect {
        EventEffect::New(message) => message,
        other => panic!("expected a new message, got {other:?}"),
    }
}

mod room_messages {
    use super::*;

    /// Classify an `m.room.message` from its content object.
    fn classify(content: Value) -> EventEffect {
        let event: SyncRoomMessageEvent = serde_json::from_value(message_event(content))
            .expect("the fixture should deserialize");
        effect_of_room_message(event)
    }

    #[test]
    fn a_text_message_becomes_a_new_row() {
        let message = expect_new(classify(json!({"msgtype": "m.text", "body": "hello"})));

        assert_eq!(message.body, "hello");
        assert_eq!(message.sender, "@alice:example.org");
        assert_eq!(message.event_id, "$message");
        assert_eq!(message.origin_server_ts, 1_700_000_000_000);
        assert!(message.attachment.is_none());
    }

    #[test]
    fn a_notice_carries_its_body() {
        let message = expect_new(classify(json!({"msgtype": "m.notice", "body": "heads up"})));

        assert_eq!(message.body, "heads up");
    }

    #[test]
    fn an_emote_carries_its_body() {
        let message = expect_new(classify(json!({"msgtype": "m.emote", "body": "waves"})));

        assert_eq!(message.body, "waves");
    }

    #[test]
    fn a_redacted_message_is_ignored() {
        // The row it left behind is redacted by its own redaction event, so the
        // tombstone the server sends in its place must not add a second row.
        let mut event = message_event(json!({"msgtype": "m.text", "body": "gone"}));
        event["unsigned"] = json!({
            "redacted_because": {
                "type": "m.room.redaction",
                "event_id": "$redaction",
                "sender": "@alice:example.org",
                "origin_server_ts": 1_700_000_000_001u64,
                "content": {},
            }
        });
        let event: SyncRoomMessageEvent =
            serde_json::from_value(event).expect("the fixture should deserialize");

        assert!(matches!(effect_of_room_message(event), EventEffect::Ignore));
    }

    #[test]
    fn a_replacement_becomes_an_edit_of_its_target() {
        let effect = classify(json!({
            "msgtype": "m.text",
            "body": "* corrected",
            "m.new_content": {"msgtype": "m.text", "body": "corrected"},
            "m.relates_to": {"rel_type": "m.replace", "event_id": "$original"},
        }));

        match effect {
            EventEffect::Edit {
                target,
                new_body,
                new_attachment,
            } => {
                assert_eq!(target, "$original");
                // The replacement's own body carries the "* " fallback for
                // clients that cannot apply edits; the new content is the text
                // the row should actually end up showing.
                assert_eq!(new_body, "corrected");
                assert!(new_attachment.is_none());
            }
            other => panic!("expected an edit, got {other:?}"),
        }
    }

    #[test]
    fn a_reply_is_a_new_row_rather_than_an_edit() {
        // A reply also relates to another event, but it adds a row instead of
        // changing one.
        let effect = classify(json!({
            "msgtype": "m.text",
            "body": "agreed",
            "m.relates_to": {"m.in_reply_to": {"event_id": "$original"}},
        }));

        assert_eq!(expect_new(effect).body, "agreed");
    }
}

mod attachments {
    use super::*;

    /// The attachment extracted from an `m.room.message` content object.
    fn attachment_from(content: Value) -> Option<Attachment> {
        let event: SyncRoomMessageEvent = serde_json::from_value(message_event(content))
            .expect("the fixture should deserialize");
        expect_new(effect_of_room_message(event)).attachment
    }

    #[test]
    fn an_image_carries_every_declared_field() {
        let attachment = attachment_from(json!({
            "msgtype": "m.image",
            "body": "cat.png",
            "url": "mxc://example.org/full",
            "info": {
                "w": 800,
                "h": 600,
                "size": 4096,
                "mimetype": "image/png",
                "thumbnail_url": "mxc://example.org/thumb",
                "thumbnail_info": {"size": 512, "mimetype": "image/png"},
            },
        }))
        .expect("an image message has an attachment");

        assert_eq!(attachment.kind, AttachmentKind::Image);
        assert_eq!(attachment.filename, "cat.png");
        assert_eq!(attachment.mimetype.as_deref(), Some("image/png"));
        assert_eq!(attachment.width, Some(800));
        assert_eq!(attachment.height, Some(600));
        assert_eq!(attachment.size, Some(4096));
        assert_eq!(attachment.thumbnail_size, Some(512));
        assert!(attachment.thumbnail_source.is_some());
    }

    #[test]
    fn an_image_without_an_info_block_still_yields_an_attachment() {
        let attachment = attachment_from(json!({
            "msgtype": "m.image",
            "body": "cat.png",
            "url": "mxc://example.org/full",
        }))
        .expect("an image message has an attachment");

        assert_eq!(attachment.kind, AttachmentKind::Image);
        assert_eq!(attachment.width, None);
        assert_eq!(attachment.size, None);
        assert!(attachment.thumbnail_source.is_none());
    }

    #[test]
    fn a_video_keeps_its_dimensions() {
        let attachment = attachment_from(json!({
            "msgtype": "m.video",
            "body": "clip.mp4",
            "url": "mxc://example.org/video",
            "info": {"w": 1920, "h": 1080, "size": 90000, "mimetype": "video/mp4"},
        }))
        .expect("a video message has an attachment");

        assert_eq!(attachment.kind, AttachmentKind::Video);
        assert_eq!(attachment.width, Some(1920));
        assert_eq!(attachment.height, Some(1080));
    }

    #[test]
    fn audio_has_neither_dimensions_nor_a_thumbnail() {
        // Audio info carries no width, height or thumbnail, so those stay unset
        // rather than being invented.
        let attachment = attachment_from(json!({
            "msgtype": "m.audio",
            "body": "song.mp3",
            "url": "mxc://example.org/audio",
            "info": {"size": 5000, "mimetype": "audio/mpeg", "duration": 30000},
        }))
        .expect("an audio message has an attachment");

        assert_eq!(attachment.kind, AttachmentKind::Audio);
        assert_eq!(attachment.width, None);
        assert_eq!(attachment.height, None);
        assert!(attachment.thumbnail_source.is_none());
        assert_eq!(attachment.size, Some(5000));
    }

    #[test]
    fn a_file_can_carry_a_thumbnail() {
        let attachment = attachment_from(json!({
            "msgtype": "m.file",
            "body": "report.pdf",
            "url": "mxc://example.org/file",
            "info": {
                "size": 20000,
                "mimetype": "application/pdf",
                "thumbnail_url": "mxc://example.org/thumb",
                "thumbnail_info": {"size": 256},
            },
        }))
        .expect("a file message has an attachment");

        assert_eq!(attachment.kind, AttachmentKind::File);
        assert_eq!(attachment.thumbnail_size, Some(256));
        assert!(attachment.thumbnail_source.is_some());
    }

    #[test]
    fn a_text_message_has_no_attachment() {
        assert!(attachment_from(json!({"msgtype": "m.text", "body": "hello"})).is_none());
    }
}

mod redactions {
    use super::*;

    /// Classify an `m.room.redaction` from a full event object.
    fn classify(event: Value) -> EventEffect {
        let event: SyncRoomRedactionEvent =
            serde_json::from_value(event).expect("the fixture should deserialize");
        effect_of_redaction(event)
    }

    /// A redaction event with the given extra fields merged in.
    fn redaction(redacts: Option<&str>, in_content: bool) -> Value {
        let mut event = json!({
            "type": "m.room.redaction",
            "event_id": "$redaction",
            "sender": "@alice:example.org",
            "origin_server_ts": 1_700_000_000_000u64,
            "content": {},
        });
        if let Some(target) = redacts {
            if in_content {
                event["content"]["redacts"] = json!(target);
            } else {
                event["redacts"] = json!(target);
            }
        }
        event
    }

    #[test]
    fn reads_the_target_from_the_top_level_field() {
        // Where it lives in room versions up to 10.
        match classify(redaction(Some("$target"), false)) {
            EventEffect::Redact { target } => assert_eq!(target, "$target"),
            other => panic!("expected a redaction, got {other:?}"),
        }
    }

    #[test]
    fn reads_the_target_from_the_content_field() {
        // Where it moved in room version 11.
        match classify(redaction(Some("$target"), true)) {
            EventEffect::Redact { target } => assert_eq!(target, "$target"),
            other => panic!("expected a redaction, got {other:?}"),
        }
    }

    #[test]
    fn a_redaction_naming_no_target_never_reaches_the_classifier() {
        // The classifier has an arm for a redaction that names nothing, but
        // ruma refuses such an event outright rather than handing one over with
        // both fields empty. The arm is unreachable defence, not a live path.
        let error = serde_json::from_value::<SyncRoomRedactionEvent>(redaction(None, false))
            .expect_err("a redaction naming nothing is not a valid event");

        assert!(
            error.to_string().contains("missing field `redacts`"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn an_already_redacted_redaction_is_ignored() {
        let mut event = redaction(Some("$target"), false);
        event["unsigned"] = json!({
            "redacted_because": {
                "type": "m.room.redaction",
                "event_id": "$second",
                "sender": "@alice:example.org",
                "origin_server_ts": 1_700_000_000_001u64,
                "content": {},
            }
        });

        assert!(matches!(classify(event), EventEffect::Ignore));
    }
}

mod stickers {
    use super::*;

    #[test]
    fn a_sticker_becomes_a_new_row_with_an_attachment() {
        let event: SyncStickerEvent = serde_json::from_value(json!({
            "type": "m.sticker",
            "event_id": "$sticker",
            "sender": "@alice:example.org",
            "origin_server_ts": 1_700_000_000_000u64,
            "content": {
                "body": "waving cat",
                "url": "mxc://example.org/sticker",
                "info": {"w": 128, "h": 128, "size": 2048, "mimetype": "image/png"},
            },
        }))
        .expect("the fixture should deserialize");

        let message = expect_new(effect_of_sticker(event));
        let attachment = message.attachment.expect("a sticker always has media");

        assert_eq!(message.body, "waving cat");
        assert_eq!(attachment.kind, AttachmentKind::Sticker);
        assert_eq!(attachment.width, Some(128));
        assert_eq!(attachment.height, Some(128));
        // Stickers declare no filename, which is what sends them down the
        // kind-name fallback when one is saved.
        assert!(attachment.filename.is_empty());
    }
}

mod classification {
    use super::*;

    #[test]
    fn a_state_event_is_ignored() {
        let effect = effect_of_raw(&timeline_event(json!({
            "type": "m.room.member",
            "event_id": "$member",
            "sender": "@alice:example.org",
            "origin_server_ts": 1_700_000_000_000u64,
            "state_key": "@alice:example.org",
            "content": {"membership": "join"},
        })));

        assert!(matches!(effect, EventEffect::Ignore));
    }

    #[test]
    fn a_reaction_is_ignored() {
        let effect = effect_of_raw(&timeline_event(json!({
            "type": "m.reaction",
            "event_id": "$reaction",
            "sender": "@alice:example.org",
            "origin_server_ts": 1_700_000_000_000u64,
            "content": {
                "m.relates_to": {
                    "rel_type": "m.annotation",
                    "event_id": "$target",
                    "key": "👍",
                },
            },
        })));

        assert!(matches!(effect, EventEffect::Ignore));
    }

    #[test]
    fn an_undeserializable_event_is_ignored_rather_than_fatal() {
        // One malformed event in a page must not cost the whole page.
        let effect = effect_of_raw(&timeline_event(json!({"nonsense": true})));

        assert!(matches!(effect, EventEffect::Ignore));
    }

    #[test]
    fn a_batch_drops_the_ignored_events() {
        let events = vec![
            timeline_event(message_event(json!({"msgtype": "m.text", "body": "first"}))),
            timeline_event(json!({
                "type": "m.reaction",
                "event_id": "$reaction",
                "sender": "@alice:example.org",
                "origin_server_ts": 1_700_000_000_001u64,
                "content": {
                    "m.relates_to": {
                        "rel_type": "m.annotation",
                        "event_id": "$message",
                        "key": "👍",
                    },
                },
            })),
        ];

        let effects = effects_of(&events);

        assert_eq!(effects.len(), 1);
        assert_eq!(expect_new(effects[0].clone()).body, "first");
    }

    #[test]
    fn a_batch_keeps_its_order() {
        let events: Vec<_> = ["first", "second", "third"]
            .into_iter()
            .map(|body| timeline_event(message_event(json!({"msgtype": "m.text", "body": body}))))
            .collect();

        let bodies: Vec<String> = effects_of(&events)
            .into_iter()
            .map(|effect| expect_new(effect).body)
            .collect();

        assert_eq!(bodies, ["first", "second", "third"]);
    }
}

mod uint_conversion {
    use super::*;

    #[test]
    fn converts_a_value_that_fits() {
        assert_eq!(uint_to_u32(ruma::UInt::from(640u32)), 640);
    }

    #[test]
    fn saturates_rather_than_wrapping() {
        // `UInt` reaches 2^53-1, well past what a pixel count is stored in.
        assert_eq!(uint_to_u32(ruma::UInt::MAX), u32::MAX);
    }
}

mod attachment_cache {
    use super::*;
    use ruma::{EventId, RoomId};

    /// A room id unique to one test.
    ///
    /// The cache is thread-local and libtest may run several tests on one
    /// thread, so tests that shared a room id could see each other's entries.
    fn room(name: &str) -> ruma::OwnedRoomId {
        RoomId::parse(format!("!{name}:example.org")).expect("a valid room id")
    }

    /// An event id built from `n`.
    fn event(n: usize) -> ruma::OwnedEventId {
        EventId::parse(format!("$event{n}")).expect("a valid event id")
    }

    #[test]
    fn remembers_what_was_cached() {
        let room = room("remembers");
        let attachment = image_attachment();

        cache_attachment(&room, &event(0), &attachment);
        let found = get_cached_attachment(&room, &event(0));

        assert_eq!(
            found.expect("the entry was just cached").kind,
            attachment.kind
        );
    }

    #[test]
    fn reports_nothing_for_an_event_never_cached() {
        assert!(get_cached_attachment(&room("never"), &event(0)).is_none());
    }

    #[test]
    fn forgets_an_uncached_event() {
        let room = room("forgets");
        cache_attachment(&room, &event(0), &image_attachment());

        uncache_attachment(&room, &event(0));

        assert!(get_cached_attachment(&room, &event(0)).is_none());
    }

    #[test]
    fn clearing_a_room_leaves_other_rooms_alone() {
        let cleared = room("cleared");
        let kept = room("kept");
        let attachment = image_attachment();
        cache_attachment(&cleared, &event(0), &attachment);
        cache_attachment(&kept, &event(0), &attachment);

        clear_room_attachments(&cleared);

        assert!(get_cached_attachment(&cleared, &event(0)).is_none());
        assert!(get_cached_attachment(&kept, &event(0)).is_some());
    }

    #[test]
    fn recaching_an_event_replaces_rather_than_accumulates() {
        let room = room("recached");
        let second = Attachment {
            kind: AttachmentKind::Video,
            ..image_attachment()
        };

        cache_attachment(&room, &event(0), &image_attachment());
        cache_attachment(&room, &event(0), &second);

        assert_eq!(
            get_cached_attachment(&room, &event(0))
                .expect("the entry is cached")
                .kind,
            AttachmentKind::Video
        );
    }

    #[test]
    fn evicts_down_to_the_trim_target_once_full() {
        let room = room("evicting");
        let attachment = image_attachment();
        for n in 0..=ATTACHMENT_CACHE_CAPACITY {
            cache_attachment(&room, &event(n), &attachment);
        }

        // Overshooting the capacity by one drops the oldest quarter rather than
        // a single entry, so the scan this costs is paid once per few hundred
        // inserts instead of on every one.
        let survivors = (0..=ATTACHMENT_CACHE_CAPACITY)
            .filter(|n| get_cached_attachment(&room, &event(*n)).is_some())
            .count();

        assert_eq!(survivors, ATTACHMENT_CACHE_TRIM_TO);
        assert!(get_cached_attachment(&room, &event(0)).is_none());
        assert!(get_cached_attachment(&room, &event(ATTACHMENT_CACHE_CAPACITY)).is_some());
    }

    #[test]
    fn eviction_spares_a_recently_read_entry() {
        let room = room("recency");
        let attachment = image_attachment();
        for n in 0..ATTACHMENT_CACHE_CAPACITY {
            cache_attachment(&room, &event(n), &attachment);
        }

        // Reading the oldest entry makes it the newest, so the row a user is
        // still looking at is not the one dropped to make room.
        assert!(get_cached_attachment(&room, &event(0)).is_some());
        cache_attachment(&room, &event(ATTACHMENT_CACHE_CAPACITY), &attachment);

        assert!(get_cached_attachment(&room, &event(0)).is_some());
        assert!(get_cached_attachment(&room, &event(1)).is_none());
    }
}

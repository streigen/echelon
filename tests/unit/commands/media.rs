//! Unit suite for [`crate::commands::media`].
//!
//! Compiled as an inner module of that file, so private helpers are in scope
//! through `super::*`.

use super::*;
use crate::rooms::messages::AttachmentKind;
use crate::test_support::*;
use ruma::events::room::MediaSource;

mod extension_split {
    use super::*;

    #[test]
    fn finds_a_plain_extension() {
        assert_eq!(extension_split("photo.jpg"), Some(5));
    }

    #[test]
    fn takes_the_last_dot_of_a_double_extension() {
        // "archive.tar.gz" is one file named "archive.tar" of type gz, not a
        // file named "archive" of type "tar.gz".
        assert_eq!(extension_split("archive.tar.gz"), Some(11));
        assert_eq!(extension_split("a.b.c"), Some(3));
    }

    #[test]
    fn a_leading_dot_names_a_hidden_file_not_an_extension() {
        assert_eq!(extension_split(".bashrc"), None);
        assert_eq!(extension_split("."), None);
    }

    #[test]
    fn a_name_without_a_dot_has_no_extension() {
        assert_eq!(extension_split("noext"), None);
        assert_eq!(extension_split(""), None);
    }

    #[test]
    fn splits_on_a_character_boundary_after_a_multibyte_character() {
        // 'é' is two bytes, so the dot sits at byte 5 of a four-character stem.
        // Slicing at a byte offset counted in characters would panic here.
        let name = "café.jpg";
        let split = extension_split(name).expect("the name has an extension");

        assert_eq!(split, 5);
        assert_eq!(&name[..split], "café");
        assert_eq!(&name[split..], ".jpg");
    }
}

mod extension_for {
    use super::*;

    #[test]
    fn maps_known_types() {
        assert_eq!(extension_for("image/jpeg"), Some("jpg"));
        assert_eq!(extension_for("video/quicktime"), Some("mov"));
        assert_eq!(extension_for("application/pdf"), Some("pdf"));
    }

    #[test]
    fn accepts_either_spelling_of_an_aliased_type() {
        assert_eq!(extension_for("audio/wav"), Some("wav"));
        assert_eq!(extension_for("audio/x-wav"), Some("wav"));
        assert_eq!(extension_for("audio/flac"), Some("flac"));
        assert_eq!(extension_for("audio/x-flac"), Some("flac"));
    }

    #[test]
    fn ignores_case() {
        assert_eq!(extension_for("IMAGE/JPEG"), Some("jpg"));
        assert_eq!(extension_for("Image/Png"), Some("png"));
    }

    #[test]
    fn ignores_parameters_after_the_type() {
        assert_eq!(extension_for("text/plain; charset=utf-8"), Some("txt"));
    }

    #[test]
    fn trims_surrounding_whitespace() {
        assert_eq!(extension_for("  image/png  "), Some("png"));
    }

    #[test]
    fn an_unlisted_type_gets_no_extension_rather_than_a_wrong_one() {
        assert_eq!(extension_for("application/octet-stream"), None);
        assert_eq!(extension_for(""), None);
    }
}

mod suggested_filename {
    use super::*;

    /// An attachment of `kind` whose sender declared `filename` and `mimetype`.
    fn named(kind: AttachmentKind, filename: &str, mimetype: Option<&str>) -> Attachment {
        Attachment {
            kind,
            filename: filename.to_owned(),
            mimetype: mimetype.map(ToOwned::to_owned),
            ..image_attachment()
        }
    }

    #[test]
    fn keeps_a_usable_declared_name() {
        let attachment = named(AttachmentKind::Image, "photo.jpg", None);

        assert_eq!(suggested_filename(&attachment), "photo.jpg");
    }

    #[test]
    fn strips_directory_components_from_a_traversing_name() {
        // The name is sender-controlled, so it must not be able to walk out of
        // whatever directory it is about to be written into.
        let attachment = named(AttachmentKind::Image, "../../etc/passwd", None);

        assert_eq!(suggested_filename(&attachment), "passwd");
    }

    #[test]
    fn strips_backslash_separated_components_too() {
        let attachment = named(AttachmentKind::Image, "dir\\file.png", None);

        assert_eq!(suggested_filename(&attachment), "file.png");
    }

    #[test]
    fn falls_back_to_the_kinds_own_word_for_a_relative_directory_name() {
        for declared in [".", ".."] {
            let attachment = named(AttachmentKind::Image, declared, None);

            assert_eq!(suggested_filename(&attachment), "image", "for {declared:?}");
        }
    }

    #[test]
    fn falls_back_to_the_kinds_own_word_for_a_blank_name() {
        for declared in ["", "   "] {
            let attachment = named(AttachmentKind::Video, declared, None);

            assert_eq!(suggested_filename(&attachment), "video", "for {declared:?}");
        }
    }

    #[test]
    fn names_a_file_kind_attachment() {
        let attachment = named(AttachmentKind::File, "", None);

        assert_eq!(suggested_filename(&attachment), "attachment");
    }

    #[test]
    fn names_a_sticker_attachment() {
        // Stickers carry no filename and are not in the kind table, so they land
        // on the catch-all rather than on a word of their own.
        let attachment = named(AttachmentKind::Sticker, "", None);

        assert_eq!(suggested_filename(&attachment), "attachment");
    }

    #[test]
    fn appends_the_declared_types_extension_when_the_name_has_none() {
        let attachment = named(AttachmentKind::Image, "", Some("image/png"));

        assert_eq!(suggested_filename(&attachment), "image.png");
    }

    #[test]
    fn appends_an_extension_to_a_declared_name_that_lacks_one() {
        let attachment = named(AttachmentKind::File, "report", Some("application/pdf"));

        assert_eq!(suggested_filename(&attachment), "report.pdf");
    }

    #[test]
    fn leaves_a_name_that_already_has_an_extension_alone() {
        // The declared type disagrees with the name; the name wins.
        let attachment = named(AttachmentKind::Image, "photo.jpg", Some("image/png"));

        assert_eq!(suggested_filename(&attachment), "photo.jpg");
    }

    #[test]
    fn leaves_a_name_extensionless_when_the_type_is_unknown() {
        let attachment = named(
            AttachmentKind::File,
            "report",
            Some("application/octet-stream"),
        );

        assert_eq!(suggested_filename(&attachment), "report");
    }
}

mod source_for {
    use super::*;

    #[test]
    fn a_display_request_prefers_the_senders_thumbnail() {
        let attachment = Attachment {
            thumbnail_source: Some(plain_source("mxc://example.org/thumb")),
            thumbnail_size: Some(1024),
            size: Some(9_000_000),
            ..image_attachment()
        };

        let choice = attachment.source_for(ImageSize::Display);

        assert_eq!(choice.declared_bytes, Some(1024));
        assert!(!choice.server_scaled);
        assert!(matches!(choice.source, MediaSource::Plain(uri) if uri == "mxc://example.org/thumb"));
    }

    #[test]
    fn a_display_request_without_a_thumbnail_asks_the_homeserver_to_scale() {
        let attachment = Attachment {
            thumbnail_source: None,
            size: Some(9_000_000),
            ..image_attachment()
        };

        let choice = attachment.source_for(ImageSize::Display);

        assert!(choice.server_scaled);
        // The scaled result's size is not knowable ahead of the fetch, so the
        // declared full size must not be reported as the transfer size.
        assert_eq!(choice.declared_bytes, None);
    }

    #[test]
    fn an_encrypted_display_request_falls_back_to_the_full_file() {
        // The homeserver cannot scale what it cannot read, so an encrypted
        // attachment with no sender thumbnail is fetched whole and checked
        // against the preview limit.
        let attachment = Attachment {
            source: encrypted_source("mxc://example.org/enc"),
            thumbnail_source: None,
            size: Some(9_000_000),
            ..image_attachment()
        };

        let choice = attachment.source_for(ImageSize::Display);

        assert!(!choice.server_scaled);
        assert_eq!(choice.declared_bytes, Some(9_000_000));
    }

    #[test]
    fn a_full_request_ignores_the_thumbnail() {
        let attachment = Attachment {
            thumbnail_source: Some(plain_source("mxc://example.org/thumb")),
            thumbnail_size: Some(1024),
            size: Some(9_000_000),
            ..image_attachment()
        };

        let choice = attachment.source_for(ImageSize::Full);

        assert_eq!(choice.declared_bytes, Some(9_000_000));
        assert!(!choice.server_scaled);
        assert!(matches!(choice.source, MediaSource::Plain(uri) if uri == "mxc://example.org/full"));
    }
}

mod previewable {
    use super::*;

    #[test]
    fn an_image_within_the_limit_previews() {
        let attachment = Attachment {
            thumbnail_source: Some(plain_source("mxc://example.org/thumb")),
            thumbnail_size: Some(MAX_PREVIEW_BYTES),
            ..image_attachment()
        };

        assert!(attachment.previewable());
    }

    #[test]
    fn an_image_over_the_limit_does_not_preview() {
        let attachment = Attachment {
            thumbnail_source: Some(plain_source("mxc://example.org/thumb")),
            thumbnail_size: Some(MAX_PREVIEW_BYTES + 1),
            ..image_attachment()
        };

        assert!(!attachment.previewable());
    }

    #[test]
    fn an_image_of_unknown_size_previews() {
        // A server-scaled fetch declares no size, and refusing those would mean
        // never previewing an attachment whose sender supplied no thumbnail.
        let attachment = Attachment {
            thumbnail_source: None,
            size: Some(u64::MAX),
            ..image_attachment()
        };

        assert_eq!(attachment.source_for(ImageSize::Display).declared_bytes, None);
        assert!(attachment.previewable());
    }

    #[test]
    fn a_sticker_previews() {
        let attachment = Attachment {
            kind: AttachmentKind::Sticker,
            ..image_attachment()
        };

        assert!(attachment.previewable());
    }

    #[test]
    fn a_non_visual_kind_never_previews() {
        for kind in [
            AttachmentKind::Video,
            AttachmentKind::Audio,
            AttachmentKind::File,
        ] {
            let attachment = Attachment {
                kind,
                ..image_attachment()
            };

            assert!(!attachment.previewable(), "for {kind:?}");
        }
    }
}

mod attachment_kind {
    use super::*;

    #[test]
    fn only_images_and_stickers_render_inline() {
        assert!(AttachmentKind::Image.has_preview());
        assert!(AttachmentKind::Sticker.has_preview());
        assert!(!AttachmentKind::Video.has_preview());
        assert!(!AttachmentKind::Audio.has_preview());
        assert!(!AttachmentKind::File.has_preview());
    }

    #[test]
    fn everything_but_a_sticker_can_be_saved() {
        assert!(!AttachmentKind::Sticker.is_savable());
        assert!(AttachmentKind::Image.is_savable());
        assert!(AttachmentKind::Video.is_savable());
        assert!(AttachmentKind::Audio.is_savable());
        assert!(AttachmentKind::File.is_savable());
    }
}

mod over_limit {
    use super::*;

    #[test]
    fn names_both_the_size_and_the_limit() {
        assert_eq!(
            over_limit(9_000_000, MAX_PREVIEW_BYTES),
            "Attachment is 9000000 bytes, over the 8388608 byte limit"
        );
    }
}

mod decode_image {
    use super::*;

    #[test]
    fn decodes_a_small_rgba_image_at_its_own_size() {
        let decoded = decode_image(rgba_png(2, 2), 640).expect("a 2x2 PNG should decode");

        assert_eq!((decoded.width(), decoded.height()), (2, 2));
    }

    #[test]
    fn decodes_an_image_that_is_not_already_rgba() {
        // RGB8 cannot be read straight into the output buffer, so this takes the
        // conversion path rather than the fast one.
        let decoded = decode_image(rgb_png(2, 2), 640).expect("a 2x2 PNG should decode");

        assert_eq!((decoded.width(), decoded.height()), (2, 2));
    }

    #[test]
    fn does_not_upscale_an_image_smaller_than_the_limit() {
        let decoded = decode_image(rgba_png(10, 10), 640).expect("a 10x10 PNG should decode");

        assert_eq!((decoded.width(), decoded.height()), (10, 10));
    }

    #[test]
    fn downscales_an_oversized_image_preserving_its_aspect_ratio() {
        let decoded = decode_image(rgba_png(1000, 10), 640).expect("a 1000x10 PNG should decode");

        assert_eq!(decoded.width(), 640);
        // 10 * (640/1000) = 6.4, and the scale only ever shrinks.
        assert_eq!(decoded.height(), 6);
    }

    /// The error from a decode that is expected to fail.
    ///
    /// `DecodedImage` holds a pixel buffer and is not `Debug`, so the `Result`
    /// helpers that would report the unexpected success are unavailable here.
    ///
    /// # Arguments
    /// * `bytes` - The data to attempt to decode.
    fn decode_error(bytes: Vec<u8>) -> String {
        match decode_image(bytes, 640) {
            Ok(image) => panic!(
                "expected the decode to fail, got a {}x{} image",
                image.width(),
                image.height()
            ),
            Err(error) => error,
        }
    }

    #[test]
    fn rejects_data_that_is_not_an_image() {
        let error = decode_error(b"not an image at all".to_vec());

        // Not the "Unrecognized image data" message, despite that being the one
        // written for this case: `with_guessed_format` reports a failure to read
        // from the reader, not a failure to recognize what was read, and reading
        // from a `Cursor` over bytes already in memory cannot fail. Unknown data
        // is caught one step later, when the decoder is built.
        assert!(
            error.starts_with("Failed to read image header"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn rejects_empty_data() {
        assert!(decode_image(Vec::new(), 640).is_err());
    }

    #[test]
    fn rejects_a_truncated_image() {
        let mut bytes = rgba_png(64, 64);
        bytes.truncate(bytes.len() / 2);

        assert!(decode_image(bytes, 640).is_err());
    }

    #[test]
    fn refuses_a_declared_size_beyond_the_decode_ceiling() {
        // A PNG header can claim any size it likes; the decoder must refuse to
        // allocate for one before reading the pixels, rather than being talked
        // into a multi-gigabyte buffer by a sender.
        let bytes = png_header_claiming(MAX_DECODE_EDGE + 1, MAX_DECODE_EDGE + 1);

        assert!(decode_image(bytes, 640).is_err());
    }

    /// A PNG whose `IHDR` declares `width` x `height` without carrying the pixels
    /// for it.
    ///
    /// # Arguments
    /// * `width` - The width to declare.
    /// * `height` - The height to declare.
    fn png_header_claiming(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = rgba_png(1, 1);
        // IHDR is the first chunk: 8 bytes of signature, then the chunk's 4-byte
        // length and 4-byte type, so its width and height sit at bytes 16 and 20.
        bytes[16..20].copy_from_slice(&width.to_be_bytes());
        bytes[20..24].copy_from_slice(&height.to_be_bytes());
        bytes
    }
}

mod write_new_file {
    use super::*;

    #[test]
    fn writes_the_requested_name_into_an_empty_directory() {
        let dir = tempfile::tempdir().expect("a temp dir");

        let written =
            write_new_file(dir.path(), "photo.jpg", b"contents").expect("the write should succeed");

        assert_eq!(written, dir.path().join("photo.jpg"));
        assert_eq!(std::fs::read(&written).expect("the file exists"), b"contents");
    }

    #[test]
    fn suffixes_before_the_extension_so_the_file_stays_openable() {
        let dir = tempfile::tempdir().expect("a temp dir");
        std::fs::write(dir.path().join("photo.jpg"), b"first").expect("seeding the collision");

        let written =
            write_new_file(dir.path(), "photo.jpg", b"second").expect("the write should succeed");

        assert_eq!(written, dir.path().join("photo (1).jpg"));
    }

    #[test]
    fn never_overwrites_a_file_already_on_disk() {
        let dir = tempfile::tempdir().expect("a temp dir");
        std::fs::write(dir.path().join("photo.jpg"), b"first").expect("seeding the collision");

        write_new_file(dir.path(), "photo.jpg", b"second").expect("the write should succeed");

        assert_eq!(
            std::fs::read(dir.path().join("photo.jpg")).expect("the original still exists"),
            b"first"
        );
    }

    #[test]
    fn keeps_counting_past_the_first_free_name() {
        let dir = tempfile::tempdir().expect("a temp dir");
        std::fs::write(dir.path().join("photo.jpg"), b"first").expect("seeding");
        std::fs::write(dir.path().join("photo (1).jpg"), b"second").expect("seeding");

        let written =
            write_new_file(dir.path(), "photo.jpg", b"third").expect("the write should succeed");

        assert_eq!(written, dir.path().join("photo (2).jpg"));
    }

    #[test]
    fn suffixes_a_name_that_has_no_extension() {
        let dir = tempfile::tempdir().expect("a temp dir");
        std::fs::write(dir.path().join("photo"), b"first").expect("seeding the collision");

        let written =
            write_new_file(dir.path(), "photo", b"second").expect("the write should succeed");

        assert_eq!(written, dir.path().join("photo (1)"));
    }

    #[test]
    fn gives_up_after_the_attempt_limit() {
        let dir = tempfile::tempdir().expect("a temp dir");
        std::fs::write(dir.path().join("photo.jpg"), b"taken").expect("seeding");
        for n in 1..MAX_NAME_ATTEMPTS {
            std::fs::write(dir.path().join(format!("photo ({n}).jpg")), b"taken").expect("seeding");
        }

        let error = write_new_file(dir.path(), "photo.jpg", b"one too many")
            .expect_err("every candidate name is taken");

        assert!(
            error.starts_with("Could not find a free name for 'photo.jpg'"),
            "unexpected error: {error}"
        );
    }
}

#[cfg(target_os = "linux")]
mod parse_xdg_download_dir {
    use super::*;
    use std::path::{Path, PathBuf};

    /// The home directory the parsed values are resolved against.
    fn home() -> &'static Path {
        Path::new("/home/tester")
    }

    #[test]
    fn expands_a_home_relative_value() {
        let parsed = parse_xdg_download_dir(r#"XDG_DOWNLOAD_DIR="$HOME/Downloads""#, home());

        assert_eq!(parsed, Some(PathBuf::from("/home/tester/Downloads")));
    }

    #[test]
    fn expands_a_localized_home_relative_value() {
        // The whole reason for reading this file: on Linux the directory really
        // is created under its translated name.
        let parsed = parse_xdg_download_dir(r#"XDG_DOWNLOAD_DIR="$HOME/Téléchargements""#, home());

        assert_eq!(parsed, Some(PathBuf::from("/home/tester/Téléchargements")));
    }

    #[test]
    fn takes_an_absolute_value_verbatim() {
        let parsed = parse_xdg_download_dir(r#"XDG_DOWNLOAD_DIR="/data/downloads""#, home());

        assert_eq!(parsed, Some(PathBuf::from("/data/downloads")));
    }

    #[test]
    fn accepts_an_unquoted_value() {
        let parsed = parse_xdg_download_dir("XDG_DOWNLOAD_DIR=/data/downloads", home());

        assert_eq!(parsed, Some(PathBuf::from("/data/downloads")));
    }

    #[test]
    fn skips_comments_and_the_other_entries_that_share_the_file() {
        let contents = concat!(
            "# This file is written by xdg-user-dirs-update\n",
            "XDG_DESKTOP_DIR=\"$HOME/Desktop\"\n",
            "XDG_DOWNLOAD_DIR=\"$HOME/Downloads\"\n",
            "XDG_MUSIC_DIR=\"$HOME/Music\"\n",
        );

        let parsed = parse_xdg_download_dir(contents, home());

        assert_eq!(parsed, Some(PathBuf::from("/home/tester/Downloads")));
    }

    #[test]
    fn tolerates_leading_whitespace() {
        let parsed = parse_xdg_download_dir("   XDG_DOWNLOAD_DIR=\"$HOME/Downloads\"", home());

        assert_eq!(parsed, Some(PathBuf::from("/home/tester/Downloads")));
    }

    #[test]
    fn takes_the_first_of_two_entries() {
        let contents = concat!(
            "XDG_DOWNLOAD_DIR=\"$HOME/First\"\n",
            "XDG_DOWNLOAD_DIR=\"$HOME/Second\"\n",
        );

        let parsed = parse_xdg_download_dir(contents, home());

        assert_eq!(parsed, Some(PathBuf::from("/home/tester/First")));
    }

    #[test]
    fn does_not_match_a_key_padded_around_the_equals_sign() {
        // Pins the current behaviour rather than blessing it: `xdg-user-dirs`
        // never writes this form, so it is not worth handling, but a reader
        // should not assume it works.
        let parsed = parse_xdg_download_dir(r#"XDG_DOWNLOAD_DIR = "$HOME/Downloads""#, home());

        assert_eq!(parsed, None);
    }

    #[test]
    fn finds_nothing_in_a_file_without_the_entry() {
        let parsed = parse_xdg_download_dir("XDG_MUSIC_DIR=\"$HOME/Music\"\n", home());

        assert_eq!(parsed, None);
    }

    #[test]
    fn finds_nothing_in_an_empty_file() {
        assert_eq!(parse_xdg_download_dir("", home()), None);
    }
}

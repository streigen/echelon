//! Fixtures shared by the unit suites under `tests/unit`.
//!
//! Wired into the crate by a `#[cfg(test)] #[path]` module in `src/lib.rs`, so
//! every suite reaches it as `crate::test_support`. Suites are compiled as inner
//! modules of the code they cover, which is what lets them see private items.
#![allow(dead_code)]

use std::io::Cursor;
use std::sync::Once;

use image::{DynamicImage, ImageFormat, RgbImage, RgbaImage};
use matrix_sdk::deserialized_responses::TimelineEvent;
use ruma::OwnedMxcUri;
use ruma::events::AnySyncTimelineEvent;
use ruma::events::room::{
    EncryptedFile, EncryptedFileHashes, MediaSource, V2EncryptedFileInfo,
};
use ruma::serde::Raw;
use serde_json::Value;
use tempfile::TempDir;

use crate::rooms::messages::{Attachment, AttachmentKind};
use crate::storage::keyring_client::KeyringClient;
use crate::storage::secret::SecretService;
use crate::storage::store::EchelonStore;

/// Point `keyring-core` at its in-memory store, once per test process.
///
/// Every suite that touches stored secrets must call this before building
/// anything that reads the keyring. Without it the tests would create entries in
/// whatever credential store the machine running them happens to have, which
/// means prompting on some desktops and failing outright on a headless runner.
///
/// The mock store is process-wide and keeps no persistence, so tests isolate
/// themselves by using distinct service names rather than by clearing it.
pub fn install_mock_keyring() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        keyring_core::set_default_store(
            keyring_core::mock::Store::new().expect("the mock store should build"),
        );
    });
}

/// An [`EchelonStore`] over a fresh directory, keyed under `name`.
///
/// The returned [`TempDir`] owns the snapshot file, so a test must hold it for
/// as long as it uses the store.
///
/// # Arguments
/// * `name` - A label unique to the calling test, keeping its keyring entry and
///   its snapshot apart from every other test's.
pub fn temp_store(name: &str) -> (TempDir, EchelonStore) {
    install_mock_keyring();
    let dir = tempfile::tempdir().expect("a temp dir");
    let store = EchelonStore::new(
        KeyringClient::new(format!("echelon-test-{name}")),
        format!("account-{name}"),
        dir.path().to_path_buf(),
    );
    (dir, store)
}

/// A second [`EchelonStore`] over an existing directory, as a later run of the
/// app would open it.
///
/// # Arguments
/// * `dir` - The directory a previous store wrote its snapshot into.
/// * `name` - The same label that store was built with.
pub fn reopen_store(dir: &TempDir, name: &str) -> EchelonStore {
    EchelonStore::new(
        KeyringClient::new(format!("echelon-test-{name}")),
        format!("account-{name}"),
        dir.path().to_path_buf(),
    )
}

/// A [`SecretService`] over a fresh directory.
///
/// # Arguments
/// * `name` - A label unique to the calling test.
pub fn temp_secret_service(name: &str) -> (TempDir, SecretService) {
    install_mock_keyring();
    let dir = tempfile::tempdir().expect("a temp dir");
    let secrets = SecretService::new(
        KeyringClient::new(format!("echelon-test-{name}")),
        dir.path().to_path_buf(),
    );
    (dir, secrets)
}

/// A second [`SecretService`] over an existing directory.
///
/// # Arguments
/// * `dir` - The directory a previous service wrote its snapshots into.
/// * `name` - The same label that service was built with.
pub fn reopen_secret_service(dir: &TempDir, name: &str) -> SecretService {
    SecretService::new(
        KeyringClient::new(format!("echelon-test-{name}")),
        dir.path().to_path_buf(),
    )
}

/// An unencrypted media source pointing at `uri`.
///
/// # Arguments
/// * `uri` - The `mxc://` URI the source refers to.
pub fn plain_source(uri: &str) -> MediaSource {
    MediaSource::Plain(OwnedMxcUri::from(uri))
}

/// An encrypted media source pointing at `uri`.
///
/// The key, IV and hash are fixed zero bytes. Nothing under test decrypts
/// anything; what matters is only that the variant is `Encrypted`, since that is
/// what decides whether the homeserver can be asked to scale the image.
///
/// # Arguments
/// * `uri` - The `mxc://` URI the source refers to.
pub fn encrypted_source(uri: &str) -> MediaSource {
    MediaSource::Encrypted(Box::new(EncryptedFile::new(
        OwnedMxcUri::from(uri),
        V2EncryptedFileInfo::encode([0u8; 32], [0u8; 16]).into(),
        EncryptedFileHashes::with_sha256([0u8; 32]),
    )))
}

/// An image attachment with every optional field unset.
///
/// Meant to be overridden field by field with struct update syntax, so each test
/// states only the fields it actually depends on:
///
/// ```ignore
/// Attachment { size: Some(10), ..image_attachment() }
/// ```
pub fn image_attachment() -> Attachment {
    Attachment {
        kind: AttachmentKind::Image,
        source: plain_source("mxc://example.org/full"),
        thumbnail_source: None,
        mimetype: None,
        filename: String::new(),
        width: None,
        height: None,
        size: None,
        thumbnail_size: None,
    }
}

/// PNG bytes for an opaque white RGBA image.
///
/// RGBA8 is the colour type [`decode_image`](crate::commands::media::decode_image)
/// can read straight into its output buffer, so this is the fixture for that path.
///
/// # Arguments
/// * `width` - Image width in pixels.
/// * `height` - Image height in pixels.
pub fn rgba_png(width: u32, height: u32) -> Vec<u8> {
    encode_png(DynamicImage::ImageRgba8(RgbaImage::from_pixel(
        width,
        height,
        image::Rgba([255, 255, 255, 255]),
    )))
}

/// PNG bytes for an opaque white RGB image.
///
/// RGB8 forces the conversion path, since the decoder's output has to be widened
/// to RGBA before it can be handed to Slint.
///
/// # Arguments
/// * `width` - Image width in pixels.
/// * `height` - Image height in pixels.
pub fn rgb_png(width: u32, height: u32) -> Vec<u8> {
    encode_png(DynamicImage::ImageRgb8(RgbImage::from_pixel(
        width,
        height,
        image::Rgb([255, 255, 255]),
    )))
}

/// Encode an image to PNG bytes.
fn encode_png(image: DynamicImage) -> Vec<u8> {
    let mut bytes = Vec::new();
    image
        .write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
        .expect("encoding a PNG fixture should succeed");
    bytes
}

/// Wrap raw event JSON as an unencrypted timeline event.
///
/// # Arguments
/// * `json` - The event as it would arrive from a homeserver.
pub fn timeline_event(json: Value) -> TimelineEvent {
    TimelineEvent::from_plaintext(raw_event(json))
}

/// Parse raw event JSON without deserializing it into a typed event.
///
/// # Arguments
/// * `json` - The event as it would arrive from a homeserver.
pub fn raw_event(json: Value) -> Raw<AnySyncTimelineEvent> {
    Raw::new(&json)
        .expect("event fixture should serialize")
        .cast_unchecked()
}

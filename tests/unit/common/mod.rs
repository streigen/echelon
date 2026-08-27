//! Fixtures shared by the unit suites under `tests/unit`.
//!
//! Wired into the crate by a `#[cfg(test)] #[path]` module in `src/lib.rs`, so
//! every suite reaches it as `crate::test_support`. Suites are compiled as inner
//! modules of the code they cover, which is what lets them see private items.
#![allow(dead_code)]

use std::io::Cursor;
use std::sync::{Arc, Once};

use image::{DynamicImage, ImageFormat, RgbImage, RgbaImage};
use matrix_sdk::Client;
use matrix_sdk::deserialized_responses::TimelineEvent;
use matrix_sdk::test_utils::mocks::MatrixMockServer;
use ruma::OwnedMxcUri;
use ruma::events::AnySyncTimelineEvent;
use ruma::events::room::{
    EncryptedFile, EncryptedFileHashes, MediaSource, V2EncryptedFileInfo,
};
use ruma::serde::Raw;
use serde_json::Value;
use tempfile::TempDir;
use tokio::sync::RwLock;

use crate::ClientState;
use crate::app_state::AppState;
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

/// An [`AppState`] whose store and secrets live under `dir`.
///
/// Neither of them touches the keyring until something is read or written, so
/// building this is just paths and a hash.
///
/// # Arguments
/// * `dir` - The directory to keep snapshots and account databases in.
/// * `name` - A label unique to the calling test.
pub fn temp_app_state(dir: &std::path::Path, name: &str) -> Arc<AppState> {
    install_mock_keyring();
    Arc::new(AppState {
        secret_service: SecretService::new(
            KeyringClient::new(format!("echelon-test-{name}")),
            dir.to_path_buf(),
        ),
        echelon_store: EchelonStore::new(
            KeyringClient::new(format!("echelon-test-{name}")),
            format!("account-{name}"),
            dir.to_path_buf(),
        ),
        data_dir: dir.to_path_buf(),
    })
}

/// A client state with nobody signed in.
///
/// Every command checks this first, so it is what the guard cases run against.
pub fn empty_state() -> ClientState {
    Arc::new(RwLock::new(None))
}

/// A signed-in session against a mock homeserver.
///
/// Holds the temp directory so the account database and snapshots outlive the
/// test that built it.
pub struct MockSession {
    /// The homeserver the client talks to. Endpoints are mocked on this.
    pub server: MatrixMockServer,
    /// The client the handler owns, for arranging rooms and state directly.
    pub client: Client,
    /// The state the commands are called with.
    pub state: ClientState,
    /// The store and secrets the handler reads through.
    pub app_state: Arc<AppState>,
    _dir: TempDir,
}

/// Stand up a mock homeserver with a signed-in client wired into a client state.
///
/// The session is the default one `MockClientBuilder` provides, so the account
/// is `@example:localhost` on device `DEVICEID`. The event cache is subscribed
/// here because the real client does it at build time, and the pagination
/// commands depend on it.
///
/// # Arguments
/// * `name` - A label unique to the calling test, keeping its keyring entry and
///   its on-disk state apart from every other test's.
pub async fn mock_session(name: &str) -> MockSession {
    let dir = tempfile::tempdir().expect("a temp dir");
    let server = MatrixMockServer::new().await;
    let client = server.client_builder().build().await;
    client
        .event_cache()
        .subscribe()
        .expect("subscribing to the event cache should succeed");

    let app_state = temp_app_state(dir.path(), name);
    let handler = crate::client::test_handler::handler_from_parts(
        client.clone(),
        app_state.clone(),
        slint::Weak::default(),
    );

    MockSession {
        server,
        client,
        state: Arc::new(RwLock::new(Some(handler))),
        app_state,
        _dir: dir,
    }
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

/// Parse raw state event JSON for a sync response.
///
/// # Arguments
/// * `event_type` - The event's `type`.
/// * `state_key` - The event's state key.
/// * `content` - The event's content object.
pub fn state_event(
    event_type: &str,
    state_key: &str,
    content: Value,
) -> Raw<ruma::events::AnySyncStateEvent> {
    Raw::new(&serde_json::json!({
        "type": event_type,
        "state_key": state_key,
        "event_id": format!("${event_type}-{state_key}"),
        "sender": "@example:localhost",
        "origin_server_ts": 1_700_000_000_000u64,
        "content": content,
    }))
    .expect("state event fixture should serialize")
    .cast_unchecked()
}

/// The `m.room.create` event that marks a room as a space.
///
/// A room is a space by virtue of its create event, so a test room only counts
/// as one if it is synced carrying this.
pub fn space_create_event() -> Raw<ruma::events::AnySyncStateEvent> {
    state_event(
        "m.room.create",
        "",
        serde_json::json!({
            "creator": "@example:localhost",
            "room_version": "9",
            "type": "m.space",
        }),
    )
}

/// An `m.space.child` event pointing a space at `child_room_id`.
///
/// # Arguments
/// * `child_room_id` - The room the edge points to.
/// * `via` - Servers through which the child can be reached. An edge with none
///   is not a valid child link.
pub fn space_child_event(child_room_id: &str, via: &[&str]) -> Raw<ruma::events::AnySyncStateEvent> {
    state_event("m.space.child", child_room_id, serde_json::json!({"via": via}))
}

/// An `m.space.parent` event pointing a room at `parent_room_id`.
///
/// # Arguments
/// * `parent_room_id` - The space the room claims as its parent.
pub fn space_parent_event(parent_room_id: &str) -> Raw<ruma::events::AnySyncStateEvent> {
    state_event(
        "m.space.parent",
        parent_room_id,
        serde_json::json!({"via": ["example.org"]}),
    )
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

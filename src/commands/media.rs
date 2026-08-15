use std::io::Cursor;

use matrix_sdk::Client;
use matrix_sdk::media::{MediaFormat, MediaRequestParameters, MediaThumbnailSettings};
use ruma::UInt;
use ruma::api::client::media::get_content_thumbnail::v3::Method;
use ruma::events::room::MediaSource;

use crate::rooms::messages::Attachment;

/// Longest edge kept for an in-chat attachment. This is also the size asked
/// of the homeserver's thumbnail endpoint. It is twice the 320px attachment
/// width used in message-row.slint, so images stay sharp on HiDPI screens.
const DISPLAY_MAX_EDGE: u32 = 640;

/// Longest edge kept for the lightbox, roughly a 4K screen's short edge.
/// Past this the extra pixels are invisible but the buffer is not.
const FULL_MAX_EDGE: u32 = 2560;

/// Decoder ceilings applied to every image. The bytes come from an untrusted
/// homeserver, and without these a small crafted file can ask the decoder for
/// gigabytes. This is known as a decompression bomb.
const MAX_DECODE_ALLOC: u64 = 192 * 1024 * 1024;
const MAX_DECODE_EDGE: u32 = 16384;

/// Which resolution of an attachment to fetch.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ImageSize {
    /// The cheapest version that still looks right inline in the chat log.
    Display,
    /// The original file, used for click to enlarge.
    Full,
}

/// A decoded and downscaled image in the one shape that can cross threads.
/// `slint::Image` is not `Send`, but the `SharedPixelBuffer` behind it is.
/// Decoding happens on a blocking worker, and [`DecodedImage::into_image`]
/// wraps that same buffer on the UI thread without copying it again.
pub struct DecodedImage(slint::SharedPixelBuffer<slint::Rgba8Pixel>);

impl DecodedImage {
    pub fn width(&self) -> u32 {
        self.0.width()
    }

    pub fn height(&self) -> u32 {
        self.0.height()
    }

    /// Must be called on the UI thread, since `slint::Image` is not `Send`.
    pub fn into_image(self) -> slint::Image {
        slint::Image::from_rgba8(self.0)
    }
}

/// Download and decode an image attachment at the requested size.
///
/// The fetch goes through `client.media()`, which is backed by the local
/// media cache, so repeat views do not hit the network again.
///
/// [`ImageSize::Display`] has three strategies, in order of preference:
/// 1. Use a sender-provided `thumbnail_source`, which is already small.
/// 2. For unencrypted media, ask the homeserver to scale it, so only the
///    smaller image crosses the wire.
/// 3. For encrypted media with no attached thumbnail, fetch the full file.
///    The server cannot thumbnail content it cannot decrypt.
pub async fn fetch_image(
    client: &Client,
    attachment: &Attachment,
    size: ImageSize,
) -> Result<DecodedImage, String> {
    let (source, format) = match (size, &attachment.thumbnail_source) {
        (ImageSize::Display, Some(thumbnail)) => (thumbnail, MediaFormat::File),
        (ImageSize::Display, None) if matches!(attachment.source, MediaSource::Plain(_)) => (
            &attachment.source,
            MediaFormat::Thumbnail(MediaThumbnailSettings::with_method(
                Method::Scale,
                UInt::from(DISPLAY_MAX_EDGE),
                UInt::from(DISPLAY_MAX_EDGE),
            )),
        ),
        _ => (&attachment.source, MediaFormat::File),
    };
    let request = MediaRequestParameters {
        source: source.clone(),
        format,
    };

    let bytes = client
        .media()
        .get_media_content(&request, true)
        .await
        .map_err(|e| format!("Failed to download image: {e}"))?;

    let max_edge = match size {
        ImageSize::Display => DISPLAY_MAX_EDGE,
        ImageSize::Full => FULL_MAX_EDGE,
    };
    // Decoding is CPU bound and can take tens of milliseconds on a large
    // photo. On the async worker threads it would stall the sync loop.
    tokio::task::spawn_blocking(move || decode_image(bytes, max_edge))
        .await
        .map_err(|e| format!("Image decode task failed: {e}"))?
}

/// Download an attachment's original bytes, undecoded. Used for saving to disk, where the file
/// must land byte for byte as the sender uploaded it, so there is no thumbnailing and no kind
/// restriction.
///
/// # Arguments
/// * `client` - The Matrix client to download through.
/// * `attachment` - The attachment whose original file to fetch.
pub async fn fetch_file(client: &Client, attachment: &Attachment) -> Result<Vec<u8>, String> {
    let request = MediaRequestParameters {
        source: attachment.source.clone(),
        format: MediaFormat::File,
    };
    client
        .media()
        .get_media_content(&request, true)
        .await
        .map_err(|e| format!("Failed to download attachment: {e}"))
}

/// Ask the user where to save an attachment, then download it there. Returns the path written, or
/// `None` if the user dismissed the dialog.
///
/// The dialog is shown before the download starts, so a cancel costs no transfer and the click is
/// answered immediately.
///
/// # Arguments
/// * `client` - The Matrix client to download through.
/// * `attachment` - The attachment to save.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub async fn save_attachment(
    client: &Client,
    attachment: &Attachment,
) -> Result<Option<std::path::PathBuf>, String> {
    // The suggested name is sender-controlled, but only the picker decides the real path.
    let mut dialog = rfd::AsyncFileDialog::new()
        .set_title("Save attachment")
        .set_file_name(suggested_filename(attachment))
        .set_can_create_directories(true);

    // Left unset when there is no downloads directory, so the dialog uses its own default.
    if let Some(directory) = downloads_dir() {
        dialog = dialog.set_directory(directory);
    }

    let Some(file) = dialog.save_file().await else {
        return Ok(None);
    };

    let bytes = fetch_file(client, attachment).await?;
    file.write(&bytes)
        .await
        .map_err(|e| format!("Failed to write file: {e}"))?;
    Ok(Some(file.path().to_path_buf()))
}

/// The user's downloads directory, used as the save dialog's starting point. Every branch is a
/// guess, so the path is only returned once it has been confirmed to exist.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn downloads_dir() -> Option<std::path::PathBuf> {
    // Windows has no `HOME`; the equivalent there is `USERPROFILE`.
    let home = std::path::PathBuf::from(
        std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?,
    );

    // macOS and Windows localize only the folder's display name, so on disk it really is
    // "Downloads". This misses a Windows user who relocated theirs, which needs
    // `SHGetKnownFolderPath` to answer properly.
    #[cfg(not(target_os = "linux"))]
    let directory = home.join("Downloads");

    // Linux is the opposite: the directory is created under a localized name, so it is looked up.
    #[cfg(target_os = "linux")]
    let directory = xdg_download_dir(&home).unwrap_or_else(|| home.join("Downloads"));

    directory.is_dir().then_some(directory)
}

/// Read `XDG_DOWNLOAD_DIR` out of the user-dirs config, which is a file written by `xdg-user-dirs`
/// rather than an environment variable. On a non-English desktop the directory is created as
/// `~/Téléchargements`, `~/Descargas` and so on, which a hardcoded `~/Downloads` would never find.
///
/// Returns `None` when the config is missing, which means `xdg-user-dirs` never ran and there is no
/// localized directory to find.
///
/// # Arguments
/// * `home` - The user's home directory, used to expand the file's `$HOME` prefix.
#[cfg(target_os = "linux")]
fn xdg_download_dir(home: &std::path::Path) -> Option<std::path::PathBuf> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| home.join(".config"));
    let contents = std::fs::read_to_string(config.join("user-dirs.dirs")).ok()?;

    for line in contents.lines() {
        let Some(value) = line.trim().strip_prefix("XDG_DOWNLOAD_DIR=") else {
            continue;
        };
        let value = value.trim().trim_matches('"');

        // Paths under the home directory are written as "$HOME/...", the only expansion used here.
        return Some(match value.strip_prefix("$HOME") {
            Some(rest) => home.join(rest.trim_start_matches('/')),
            None => std::path::PathBuf::from(value),
        });
    }
    None
}

/// Save an attachment on mobile, where there is no desktop-style file picker. The file goes to the
/// app's own downloads directory, since Android's Storage Access Framework needs JNI that does not
/// exist here yet.
///
/// TODO: replace with an `ACTION_CREATE_DOCUMENT` intent on Android so the user picks the
/// destination, matching the desktop behaviour.
///
/// # Arguments
/// * `client` - The Matrix client to download through.
/// * `attachment` - The attachment to save.
#[cfg(any(target_os = "android", target_os = "ios"))]
pub async fn save_attachment(
    client: &Client,
    attachment: &Attachment,
) -> Result<Option<std::path::PathBuf>, String> {
    let dir = crate::app_state::app_data_dir(crate::APP_ID).join("downloads");
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("Failed to create {}: {e}", dir.display()))?;

    // `suggested_filename` returns a single path component, so it cannot escape `dir`.
    let path = dir.join(suggested_filename(attachment));
    let bytes = fetch_file(client, attachment).await?;
    std::fs::write(&path, &bytes).map_err(|e| format!("Failed to write file: {e}"))?;
    Ok(Some(path))
}

/// The name to save an attachment under, or to seed the save dialog with. Falls back to the kind's
/// own word when the sender declared nothing usable.
///
/// # Arguments
/// * `attachment` - The attachment being saved.
fn suggested_filename(attachment: &Attachment) -> String {
    // Keep only the last component, so a sender-supplied "../x" cannot walk out of a directory.
    let declared = attachment
        .filename
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default()
        .trim();
    if !declared.is_empty() && declared != "." && declared != ".." {
        return declared.to_owned();
    }
    match attachment.kind {
        crate::rooms::messages::AttachmentKind::Image => "image",
        crate::rooms::messages::AttachmentKind::Video => "video",
        crate::rooms::messages::AttachmentKind::Audio => "audio",
        _ => "attachment",
    }
    .to_owned()
}

/// Decode image bytes into RGBA8, downscaled so no edge exceeds `max_edge`.
/// The buffer lives as long as the message row does, so a 12MP original
/// would otherwise sit in memory at around 48 MB to fill a 320px box.
/// Slint has no in-memory raster decoder, so this uses the `image` crate.
fn decode_image(bytes: Vec<u8>, max_edge: u32) -> Result<DecodedImage, String> {
    let mut reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| format!("Unrecognized image data: {e}"))?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_DECODE_EDGE);
    limits.max_image_height = Some(MAX_DECODE_EDGE);
    limits.max_alloc = Some(MAX_DECODE_ALLOC);
    reader.limits(limits);

    let decoded = reader
        .decode()
        .map_err(|e| format!("Failed to decode image: {e}"))?;
    let decoded = if decoded.width() > max_edge || decoded.height() > max_edge {
        // Preserves the aspect ratio, and only ever shrinks given the guard.
        decoded.thumbnail(max_edge, max_edge)
    } else {
        decoded
    };

    // `into_rgba8` does nothing when the source was already RGBA8.
    let rgba = decoded.into_rgba8();
    let (width, height) = rgba.dimensions();
    Ok(DecodedImage(slint::SharedPixelBuffer::clone_from_slice(
        rgba.as_raw(),
        width,
        height,
    )))
}

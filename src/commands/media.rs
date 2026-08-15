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

/// Download an attachment's original bytes, undecoded. This is the path for
/// saving to disk, where the file has to land byte for byte as the sender
/// uploaded it — no thumbnailing, no re-encoding, and no kind restriction.
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

/// Ask the user where to put an attachment, then download it there. Returns
/// the path written, or `None` if the user dismissed the dialog.
///
/// The dialog comes first so a cancel costs no download, and so the user gets
/// an immediate response to their click rather than one that arrives whenever
/// the file finishes transferring.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub async fn save_attachment(
    client: &Client,
    attachment: &Attachment,
) -> Result<Option<std::path::PathBuf>, String> {
    // The sender picked this name, so it is untrusted. It is safe as a
    // suggestion because the file picker is what decides the real path, and
    // the user confirms it. Nothing here writes to it directly.
    let mut dialog = rfd::AsyncFileDialog::new()
        .set_title("Save attachment")
        .set_file_name(suggested_filename(attachment))
        .set_can_create_directories(true);
    // Start where a browser would. Left unset when there is no downloads
    // directory to point at, so the dialog falls back to its own default
    // rather than to a path that does not exist.
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

/// The user's downloads directory, if it can be worked out and actually
/// exists. This is the save dialog's starting point, so the common case —
/// keep the file, where files are kept — is one click.
///
/// Every branch here is a guess, which is why the result is only returned
/// once it has been confirmed to exist. A wrong guess means no starting
/// directory, and the dialog falls back to its own default.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn downloads_dir() -> Option<std::path::PathBuf> {
    // Windows has no `HOME`; the equivalent there is `USERPROFILE`.
    let home = std::path::PathBuf::from(
        std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?,
    );

    // macOS and Windows both localize only the folder's *display* name, so
    // the directory entry really is called "Downloads" on both. The one case
    // this misses is a Windows user who relocated theirs, which needs
    // `SHGetKnownFolderPath` to answer properly.
    #[cfg(not(target_os = "linux"))]
    let directory = home.join("Downloads");
    // Linux is the opposite: the directory itself is created under a
    // localized name, so it has to be looked up rather than assumed.
    #[cfg(target_os = "linux")]
    let directory = xdg_download_dir(&home).unwrap_or_else(|| home.join("Downloads"));

    directory.is_dir().then_some(directory)
}

/// Read `XDG_DOWNLOAD_DIR` out of the user-dirs config. It is not an
/// environment variable — `xdg-user-dirs` writes it to a file — and reading
/// it is what makes a localized or relocated downloads folder work. On a
/// non-English desktop it is created as `~/Téléchargements`, `~/Descargas`
/// and so on, and a hardcoded `~/Downloads` finds nothing at all.
///
/// Returning `None` means the config is missing, which means `xdg-user-dirs`
/// never ran, which means there is no localized directory to have found.
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
        // Paths under the home directory are written as "$HOME/...", the one
        // expansion this file uses.
        return Some(match value.strip_prefix("$HOME") {
            Some(rest) => home.join(rest.trim_start_matches('/')),
            None => std::path::PathBuf::from(value),
        });
    }
    None
}

/// Mobile has no desktop-style file picker, and wiring up Android's Storage
/// Access Framework needs JNI that does not exist here yet. Until it does,
/// the file goes to the app's own downloads directory under a sanitized name.
///
/// TODO: replace with an `ACTION_CREATE_DOCUMENT` intent on Android so the
/// user picks the destination, matching the desktop behaviour.
#[cfg(any(target_os = "android", target_os = "ios"))]
pub async fn save_attachment(
    client: &Client,
    attachment: &Attachment,
) -> Result<Option<std::path::PathBuf>, String> {
    let dir = crate::app_state::app_data_dir(crate::APP_ID).join("downloads");
    std::fs::create_dir_all(&dir).map_err(|e| format!("Failed to create {}: {e}", dir.display()))?;

    // `suggested_filename` has already reduced this to a single component, so
    // it cannot escape `dir`.
    let path = dir.join(suggested_filename(attachment));
    let bytes = fetch_file(client, attachment).await?;
    std::fs::write(&path, &bytes).map_err(|e| format!("Failed to write file: {e}"))?;
    Ok(Some(path))
}

/// The name to save under, or to seed the save dialog with. Falls back to the
/// kind's own word when the sender declared nothing usable, since a dialog
/// prefilled with an empty name is worse than one prefilled with a wrong one.
fn suggested_filename(attachment: &Attachment) -> String {
    // Only the last component is a file name; a sender-supplied "../x" would
    // otherwise open the dialog in the parent directory.
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

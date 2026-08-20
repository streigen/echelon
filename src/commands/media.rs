use std::io::Cursor;

use image::{ColorType, ImageDecoder};
use matrix_sdk::Client;
use matrix_sdk::media::{MediaFormat, MediaRequestParameters, MediaThumbnailSettings};
use ruma::UInt;
use ruma::api::client::media::get_content_thumbnail::v3::Method;
use crate::rooms::messages::{Attachment, ImageSize, MAX_PREVIEW_BYTES};

/// Maximum edge resolution for in-chat image thumbnails.
const DISPLAY_MAX_EDGE: u32 = 640;

/// Maximum edge resolution for full-size lightbox images.
const FULL_MAX_EDGE: u32 = 2560;

/// Security decoding ceilings to prevent image decompression bomb attacks.
const MAX_DECODE_ALLOC: u64 = 64 * 1024 * 1024;
const MAX_DECODE_EDGE: u32 = 16384;

/// Maximum bytes pulled down for a full resolution image.
const MAX_FULL_BYTES: u64 = 64 * 1024 * 1024;

/// Maximum allowed concurrent image decodes.
const DECODE_PERMITS: usize = 3;

static DECODE_LIMIT: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(DECODE_PERMITS);

/// A decoded image buffer that can cross threads.
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
/// # Arguments
/// * `client` - Matrix client for fetching content.
/// * `attachment` - Image attachment to fetch.
/// * `size` - Target resolution (`Display` or `Full`).
pub async fn fetch_image(
    client: &Client,
    attachment: &Attachment,
    size: ImageSize,
) -> Result<DecodedImage, String> {
    let choice = attachment.source_for(size);
    let format = if choice.server_scaled {
        MediaFormat::Thumbnail(MediaThumbnailSettings::with_method(
            Method::Scale,
            UInt::from(DISPLAY_MAX_EDGE),
            UInt::from(DISPLAY_MAX_EDGE),
        ))
    } else {
        MediaFormat::File
    };
    let request = MediaRequestParameters {
        source: choice.source.clone(),
        format,
    };

    let byte_limit = match size {
        ImageSize::Display => MAX_PREVIEW_BYTES,
        ImageSize::Full => MAX_FULL_BYTES,
    };

    if let Some(declared) = choice.declared_bytes
        && declared > byte_limit
    {
        return Err(over_limit(declared, byte_limit));
    }

    let bytes = client
        .media()
        .get_media_content(&request, true)
        .await
        .map_err(|e| format!("Failed to download image: {e}"))?;

    if bytes.len() as u64 > byte_limit {
        return Err(over_limit(bytes.len() as u64, byte_limit));
    }

    let max_edge = match size {
        ImageSize::Display => DISPLAY_MAX_EDGE,
        ImageSize::Full => FULL_MAX_EDGE,
    };

    let _permit = DECODE_LIMIT
        .acquire()
        .await
        .map_err(|e| format!("Image decode limiter closed: {e}"))?;

    tokio::task::spawn_blocking(move || decode_image(bytes, max_edge))
        .await
        .map_err(|e| format!("Image decode task failed: {e}"))?
}

/// The error for an attachment too big to fetch at the requested size.
fn over_limit(bytes: u64, limit: u64) -> String {
    format!("Attachment is {bytes} bytes, over the {limit} byte limit")
}

/// Download an attachment's original bytes, raw. Used for saving to disk, where the file
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

/// Find the user's downloads directory, returning `None` if missing.
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

/// Read `XDG_DOWNLOAD_DIR` from `user-dirs.dirs` for Linux desktop setups.
///
/// # Arguments
/// * `home` - The user's home directory.
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
    let name = suggested_filename(attachment);
    let bytes = fetch_file(client, attachment).await?;
    write_new_file(&dir, &name, &bytes).map(Some)
}

/// How many suffixed names to try before giving up on finding a free one.
#[cfg(any(target_os = "android", target_os = "ios"))]
const MAX_NAME_ATTEMPTS: u32 = 100;

/// Write `bytes` into `dir` under `name`, or under the first free variant of it.
///
/// `create_new` is what makes this safe rather than a check followed by a write: the file is created
/// only if it did not already exist, so nothing can appear in between the two. This path has no
/// picker to vet the name, and the name came from the sender, so a file already on disk must never
/// be replaced by one arriving over the network.
///
/// # Arguments
/// * `dir` - The directory to write into.
/// * `name` - A single path component, from [`suggested_filename`].
/// * `bytes` - The contents to write.
#[cfg(any(target_os = "android", target_os = "ios"))]
fn write_new_file(
    dir: &std::path::Path,
    name: &str,
    bytes: &[u8],
) -> Result<std::path::PathBuf, String> {
    use std::io::Write;

    // Suffixed before the extension, so a second "photo.jpg" becomes "photo (1).jpg" and stays
    // openable rather than becoming "photo.jpg (1)".
    let (stem, extension) = match extension_split(name) {
        Some(split) => (&name[..split], &name[split..]),
        None => (name, ""),
    };

    for attempt in 0..MAX_NAME_ATTEMPTS {
        let candidate = match attempt {
            0 => dir.join(name),
            n => dir.join(format!("{stem} ({n}){extension}")),
        };
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(mut file) => {
                return file
                    .write_all(bytes)
                    .map(|()| candidate.clone())
                    .map_err(|e| format!("Failed to write {}: {e}", candidate.display()));
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("Failed to create {}: {e}", candidate.display())),
        }
    }

    Err(format!(
        "Could not find a free name for '{name}' after {MAX_NAME_ATTEMPTS} tries"
    ))
}

/// The name to save an attachment under, or to seed the save dialog with. Falls back to the kind's
/// own word when the sender declared nothing usable, and to the declared mime type for an extension
/// when the name carries none.
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

    let name = if !declared.is_empty() && declared != "." && declared != ".." {
        declared
    } else {
        match attachment.kind {
            crate::rooms::messages::AttachmentKind::Image => "image",
            crate::rooms::messages::AttachmentKind::Video => "video",
            crate::rooms::messages::AttachmentKind::Audio => "audio",
            _ => "attachment",
        }
    };

    // A file with no extension has no handler on the systems that pick one by it, so a name that
    // came without one is given the type's own. The kind fallbacks above never carry one either.
    if extension_split(name).is_some() {
        return name.to_owned();
    }
    match attachment.mimetype.as_deref().and_then(extension_for) {
        Some(extension) => format!("{name}.{extension}"),
        None => name.to_owned(),
    }
}

/// Byte index of the dot starting `name`'s extension, or `None` if it has none.
///
/// A leading dot names a hidden file rather than an extension, so `.bashrc` has none. Indices come
/// from `char_indices`, so a name that starts with a multi-byte character cannot split mid-character
/// the way slicing from a fixed offset would.
fn extension_split(name: &str) -> Option<usize> {
    let mut chars = name.char_indices();
    chars.next();
    chars.filter(|(_, c)| *c == '.').map(|(i, _)| i).next_back()
}

/// Extension for a declared mime type, without the dot.
///
/// A short table rather than a mime database: these are the types a Matrix client actually receives,
/// and anything unlisted keeps no extension rather than being given a wrong one.
fn extension_for(mimetype: &str) -> Option<&'static str> {
    // Parameters such as "; charset=utf-8" are not part of the type itself.
    let base = mimetype.split(';').next().unwrap_or_default().trim();
    let normalized = base.to_ascii_lowercase();
    Some(match normalized.as_str() {
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/avif" => "avif",
        "image/heic" => "heic",
        "image/bmp" => "bmp",
        "image/tiff" => "tiff",
        "image/svg+xml" => "svg",
        "video/mp4" => "mp4",
        "video/webm" => "webm",
        "video/quicktime" => "mov",
        "video/x-matroska" => "mkv",
        "audio/mpeg" => "mp3",
        "audio/ogg" => "ogg",
        "audio/opus" => "opus",
        "audio/wav" | "audio/x-wav" => "wav",
        "audio/flac" | "audio/x-flac" => "flac",
        "audio/mp4" => "m4a",
        "audio/aac" => "aac",
        "application/pdf" => "pdf",
        "application/zip" => "zip",
        "text/plain" => "txt",
        _ => return None,
    })
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

    let decoder = reader
        .into_decoder()
        .map_err(|e| format!("Failed to read image header: {e}"))?;
    let (width, height) = decoder.dimensions();

    // An image that is already RGBA8 and already small enough is decoded straight
    // into the buffer Slint will render from, so the pixels are written once and
    // never copied. Every other case has to go through `DynamicImage`, either to
    // convert the colour or to downscale, and pays one copy into the shared buffer
    // at the end. That copy is bounded by `max_edge`, so it is the small one.
    if width <= max_edge && height <= max_edge && decoder.color_type() == ColorType::Rgba8 {
        let mut buffer = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(width, height);
        decoder
            .read_image(buffer.make_mut_bytes())
            .map_err(|e| format!("Failed to decode image: {e}"))?;
        return Ok(DecodedImage(buffer));
    }

    let decoded = image::DynamicImage::from_decoder(decoder)
        .map_err(|e| format!("Failed to decode image: {e}"))?;
    let decoded = if decoded.width() > max_edge || decoded.height() > max_edge {
        // Preserves the aspect ratio, and only ever shrinks given the guard.
        let scaled = decoded.thumbnail(max_edge, max_edge);
        drop(decoded);
        scaled
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

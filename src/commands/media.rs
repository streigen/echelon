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

//! Fast image decoder using optimized libraries.
//!
//! Uses `zune-image` for fast decoding with automatic RGBA conversion,
//! with fallback to `image` crate for unsupported formats.
//! On Android, a platform-native decoder (BitmapFactory via JNI) can be
//! registered at init time via `register_platform_decoder()`.

use std::sync::Arc;
use std::sync::OnceLock;

use shared::{
    error::{EngineError, ErrorCode},
    protocol::io_cmd::{AhbImage, NormalizedImage},
};

use crate::ktx2;

/// External RGBA decoder hook (legacy). Platform code calls
/// `register_platform_decoder()` at init time; kept for compatibility
/// and as the fallback when the AHB hook fails or isn't registered.
static PLATFORM_DECODER: OnceLock<fn(&[u8]) -> Result<NormalizedImage, EngineError>> =
    OnceLock::new();

/// Zero-staging AHB decoder hook. Android's Rust/Skia decoder writes directly
/// into an API-26 `AHardwareBuffer`; the renderer imports it via
/// `eglCreateImageKHR` without materializing a tightly-packed RGBA `Vec`.
///
/// When this hook is registered, allowed, and succeeds,
/// [`decode_image_to_any`] returns the AHB variant; callers who need RGBA use
/// [`decode_image_fast`] directly. The hook may fail on a per-image basis (e.g. decoder
/// OOM, AHB alloc refused by driver); on failure we transparently
/// retry via [`PLATFORM_DECODER`] so the caller sees either a valid
/// image or a single error, never a silent downgrade.
static PLATFORM_AHB_DECODER: OnceLock<fn(&[u8]) -> Result<AhbImage, EngineError>> = OnceLock::new();

type AhbDecoder = fn(&[u8]) -> Result<AhbImage, EngineError>;

#[inline]
fn select_ahb_decoder(allow_ahb: bool, decoder: Option<AhbDecoder>) -> Option<AhbDecoder> {
    allow_ahb.then_some(decoder).flatten()
}

pub fn register_platform_decoder(f: fn(&[u8]) -> Result<NormalizedImage, EngineError>) {
    if PLATFORM_DECODER.set(f).is_ok() {
        tracing::info!("platform image decoder registered");
    } else {
        tracing::warn!("platform image decoder already registered, ignoring");
    }
}

pub fn register_platform_ahb_decoder(f: fn(&[u8]) -> Result<AhbImage, EngineError>) {
    if PLATFORM_AHB_DECODER.set(f).is_ok() {
        tracing::info!("platform AHB image decoder registered");
    } else {
        tracing::warn!("platform AHB image decoder already registered, ignoring");
    }
}

/// Describes a compressed texture detected from file magic bytes.
///
/// When a file is identified as a KTX2 container holding ETC2 or ASTC data,
/// the caller should skip RGBA decoding and instead pass the raw compressed
/// data directly to the GPU via `glCompressedTexImage2D`.
#[derive(Debug, Clone)]
pub struct CompressedImageInfo {
    pub width: u32,
    pub height: u32,
    /// Vulkan format code as parsed from the KTX2 header.
    pub vk_format: ktx2::VkFormat,
    /// Byte offset of the compressed level 0 data within the original buffer.
    pub data_offset: usize,
    /// Byte length of the compressed level 0 data.
    pub data_len: usize,
}

/// Detect whether `data` is a KTX2 container holding a compressed GPU texture.
///
/// Returns `Some(CompressedImageInfo)` if the file is a valid KTX2 with a
/// recognized compressed format (ETC2 or ASTC). Returns `None` for regular
/// images (PNG, JPEG, etc.) that should go through normal RGBA decoding.
///
/// This function does **no allocation** -- it only inspects header bytes.
pub fn detect_compressed_format(data: &[u8]) -> Option<CompressedImageInfo> {
    if !ktx2::is_ktx2(data) {
        return None;
    }

    let ktx2_file = ktx2::parse_ktx2(data).ok()?;

    // Only return info for formats we can upload to GL.
    match ktx2_file.header.format {
        ktx2::VkFormat::Etc2R8G8B8UnormBlock
        | ktx2::VkFormat::Etc2R8G8B8A8UnormBlock
        | ktx2::VkFormat::Astc4x4UnormBlock
        | ktx2::VkFormat::Astc6x6UnormBlock
        | ktx2::VkFormat::Astc8x8UnormBlock => {}
        ktx2::VkFormat::Unknown(_) => return None,
    }

    // Compute the offset of the data slice within the original buffer.
    let data_ptr = ktx2_file.data.as_ptr() as usize;
    let base_ptr = data.as_ptr() as usize;
    let data_offset = data_ptr - base_ptr;

    Some(CompressedImageInfo {
        width: ktx2_file.header.width,
        height: ktx2_file.header.height,
        vk_format: ktx2_file.header.format,
        data_offset,
        data_len: ktx2_file.data.len(),
    })
}

/// Estimate the decoded RGBA byte size from image file headers without full
/// decoding.  Returns `width * height * 4` on success, or a conservative
/// fallback (16 MB = 2048x2048x4) if the header cannot be parsed.
///
/// This is cheap (~microseconds) and used by the IO byte budget to reserve
/// the right amount before committing to a full decode.
#[allow(dead_code)]
pub fn estimate_decoded_size(data: &[u8]) -> usize {
    const FALLBACK: usize = 2048 * 2048 * 4; // 16 MB
    const MAX_ESTIMATE: usize = 256 * 1024 * 1024; // 256 MB cap

    if let Some((w, h)) = probe_image_dimensions(data) {
        (w as usize)
            .checked_mul(h as usize)
            .and_then(|pixels| pixels.checked_mul(4))
            .map(|bytes| bytes.min(MAX_ESTIMATE))
            .unwrap_or(FALLBACK)
    } else {
        FALLBACK
    }
}

/// Hard upper bound on decoded image dimensions, enforced at every decode entry
/// point ([`decode_image_fast`], [`decode_image_to_any`]) and the KTX2
/// compressed path. Single source of truth lives in `shared` so `io` and
/// `graphics` cannot drift apart.
pub use shared::protocol::io_cmd::MAX_IMAGE_PIXELS;

/// Reject an image whose pixel count exceeds [`MAX_IMAGE_PIXELS`]. Cheap;
/// used both as a pre-decode header guard and a post-decode sanity check, and by
/// the KTX2 compressed-variant path (which never goes through the RGBA decoders)
/// in `image_ops`.
pub(crate) fn enforce_pixel_cap(width: u32, height: u32) -> Result<(), EngineError> {
    let px = (width as u64).saturating_mul(height as u64);
    if px > MAX_IMAGE_PIXELS {
        return Err(
            EngineError::new(ErrorCode::OutOfMemory).with_detail(format!(
                "image {width}x{height} ({px} px) exceeds MAX_IMAGE_PIXELS ({} px); \
             refusing decode to avoid OOM",
                MAX_IMAGE_PIXELS
            )),
        );
    }
    Ok(())
}

/// Best-effort, allocation-free image-dimension probe for pre-decode resource
/// limits. Callers must still validate the decoder's returned dimensions
/// because unknown formats and malformed headers deliberately return `None`.
pub fn probe_image_dimensions(data: &[u8]) -> Option<(u32, u32)> {
    // KTX2: compressed texture container -- width/height in header.
    if ktx2::is_ktx2(data) {
        if let Ok(f) = ktx2::parse_ktx2(data) {
            return Some((f.header.width, f.header.height));
        }
    }

    // PNG: bytes 16..24 contain width (u32 BE) and height (u32 BE) in IHDR.
    if data.len() >= 24 && &data[0..8] == b"\x89PNG\r\n\x1a\n" {
        let w = u32::from_be_bytes([data[16], data[17], data[18], data[19]]);
        let h = u32::from_be_bytes([data[20], data[21], data[22], data[23]]);
        return Some((w, h));
    }

    // JPEG: scan for SOF0/SOF2 marker (0xFF 0xC0 or 0xFF 0xC2).
    if data.len() >= 2 && data[0] == 0xFF && data[1] == 0xD8 {
        let mut i = 2;
        while i + 9 < data.len() {
            if data[i] != 0xFF {
                i += 1;
                continue;
            }
            let marker = data[i + 1];
            // Any Start-Of-Frame marker carries the dimensions: SOF0..SOF15 =
            // 0xC0..=0xCF, excluding DHT(0xC4), JPG(0xC8), DAC(0xCC). Only
            // probing SOF0/SOF2 missed progressive/arithmetic/lossless JPEGs,
            // leaving their (possibly huge) dimensions for the post-decode cap.
            let is_sof = (0xC0..=0xCF).contains(&marker)
                && marker != 0xC4
                && marker != 0xC8
                && marker != 0xCC;
            if is_sof {
                // SOF: length(2) + precision(1) + height(2) + width(2)
                let h = u16::from_be_bytes([data[i + 5], data[i + 6]]) as u32;
                let w = u16::from_be_bytes([data[i + 7], data[i + 8]]) as u32;
                return Some((w, h));
            }
            // Skip segment: length is at data[i+2..i+4]
            if i + 3 < data.len() {
                let seg_len = u16::from_be_bytes([data[i + 2], data[i + 3]]) as usize;
                i += 2 + seg_len;
            } else {
                break;
            }
        }
    }

    // BMP: bytes 18..26 contain width (i32 LE) and height (i32 LE; negative =
    // top-down). These are untrusted header fields, so use `unsigned_abs` rather
    // than `i32::abs` — the latter panics on `i32::MIN` in an overflow-checked
    // build. Reject non-positive width / zero height (invalid) so we don't feed
    // a bogus 0 into the pixel cap; the decoder will surface a clean error.
    if data.len() >= 26 && &data[0..2] == b"BM" {
        let w = i32::from_le_bytes([data[18], data[19], data[20], data[21]]);
        let h = i32::from_le_bytes([data[22], data[23], data[24], data[25]]);
        if w > 0 && h != 0 {
            return Some((w as u32, h.unsigned_abs()));
        }
        return None;
    }

    // WebP: "RIFF" + size + "WEBP" + a chunk fourcc at offset 12. All three
    // frame kinds (lossy VP8, lossless VP8L, extended VP8X) carry dimensions;
    // parsing all of them keeps the pre-decode guard effective for the common
    // lossy/extended files, not just VP8L.
    if data.len() >= 30 && &data[0..4] == b"RIFF" && &data[8..12] == b"WEBP" {
        match &data[12..16] {
            // VP8L (lossless): 0x2F signature at offset 20, then width-1 in bits
            // [0..14] and height-1 in bits [14..28] of the following 4 bytes.
            b"VP8L" => {
                let b = u32::from_le_bytes([data[21], data[22], data[23], data[24]]);
                let w = (b & 0x3FFF) + 1;
                let h = ((b >> 14) & 0x3FFF) + 1;
                return Some((w, h));
            }
            // VP8 (lossy): 3-byte frame tag, start code 0x9D 0x01 0x2A at 23..26,
            // then 14-bit width at 26..28 and 14-bit height at 28..30.
            b"VP8 " => {
                if data[23] == 0x9D && data[24] == 0x01 && data[25] == 0x2A {
                    let w = (u16::from_le_bytes([data[26], data[27]]) & 0x3FFF) as u32;
                    let h = (u16::from_le_bytes([data[28], data[29]]) & 0x3FFF) as u32;
                    return Some((w, h));
                }
            }
            // VP8X (extended): 1 flag byte + 3 reserved at 20..24, then canvas
            // width-1 (24-bit LE) at 24..27 and height-1 (24-bit LE) at 27..30.
            b"VP8X" => {
                let w = (u32::from_le_bytes([data[24], data[25], data[26], 0]) & 0x00FF_FFFF) + 1;
                let h = (u32::from_le_bytes([data[27], data[28], data[29], 0]) & 0x00FF_FFFF) + 1;
                return Some((w, h));
            }
            _ => {}
        }
    }

    // GIF: "GIF87a" / "GIF89a" + logical screen width/height (LE u16) at 6..10.
    if data.len() >= 10 && (&data[0..6] == b"GIF87a" || &data[0..6] == b"GIF89a") {
        let w = u16::from_le_bytes([data[6], data[7]]) as u32;
        let h = u16::from_le_bytes([data[8], data[9]]) as u32;
        return Some((w, h));
    }

    None
}

/// Decode image data to RGBA format using the fastest available decoder.
///
/// # Arguments
/// * `data` - Raw image file bytes
/// * `_path_hint` - Optional path for extension-based format detection (unused, magic bytes preferred)
///
/// # Returns
/// `NormalizedImage` with RGBA8 pixel data
///
/// # Supported formats / compatibility matrix
///
/// | Format              | Animation | Behaviour                                    |
/// |---------------------|-----------|----------------------------------------------|
/// | JPEG                | N/A       | Full support, EXIF orientation auto-applied  |
/// | PNG                 | N/A       | Full support                                 |
/// | APNG (animated PNG) | No        | **First frame only** (zune/image limitation) |
/// | GIF (animated)      | No        | **First frame only**                         |
/// | WebP (lossy/lossless) | N/A     | Full support                                 |
/// | WebP (animated)     | No        | **First frame only**                         |
/// | BMP / TIFF          | N/A       | Depends on `image` crate fallback            |
///
/// Animated formats decode to a single still image. A multi-frame
/// pipeline (frame timing, disposal handling, per-frame cache) would
/// need to live above this function; at the moment `Image.src` treats
/// every source as a static bitmap. Games that need animation should
/// render frames themselves from a sprite atlas.
pub fn decode_image_fast(
    data: &[u8],
    _path_hint: Option<&str>,
) -> Result<NormalizedImage, EngineError> {
    // Sniff EXIF orientation *before* we hand bytes off to a decoder
    // that might either drop the metadata (zune) or interpret it
    // differently (platform BitmapFactory). We apply rotation/flip
    // ourselves so every decode path produces pixels that match the
    // way a camera-shot JPEG is meant to be displayed.
    let orientation = detect_jpeg_exif_orientation(data).unwrap_or(1);

    // Image-bomb guard #1 (pre-decode): reject a header that declares more than
    // MAX_IMAGE_PIXELS *before* any decoder allocates. Covers the primary attack
    // vector (PNG/JPEG/BMP/WebP/KTX2 with a tiny compressed body but huge
    // declared dimensions). Formats whose header we can't parse fall through to
    // guard #2 below.
    if let Some((w, h)) = probe_image_dimensions(data) {
        enforce_pixel_cap(w, h)?;
    }

    // Priority: Rust-native decoders first (zero JNI, zero Java Heap),
    // platform decoder (BitmapFactory) as last resort.
    //
    // Image-bomb guard #2 (post-decode): re-check the *actual* decoded
    // dimensions so a format the header probe couldn't read (or a decoder that
    // ignores our estimate) still can't propagate a multi-GiB buffer downstream.
    #[cfg(feature = "rust-image-decode")]
    {
        match decode_with_zune(data) {
            Ok(img) => {
                enforce_pixel_cap(img.width, img.height)?;
                return Ok(apply_exif_orientation(img, orientation));
            }
            Err(_zune_err) => {
                // zune failed; try image crate before falling back to platform.
                match decode_with_image_crate(data) {
                    Ok(img) => {
                        enforce_pixel_cap(img.width, img.height)?;
                        return Ok(apply_exif_orientation(img, orientation));
                    }
                    Err(_img_err) => {
                        tracing::debug!(
                            "Rust decoders failed (zune: {_zune_err}, image: {_img_err}), trying platform"
                        );
                    }
                }
            }
        }
    }

    // Platform decoder fallback (e.g., Android BitmapFactory via JNI).
    if let Some(decoder) = PLATFORM_DECODER.get() {
        let img = decoder(data)?;
        enforce_pixel_cap(img.width, img.height)?;
        return Ok(apply_exif_orientation(img, orientation));
    }

    Err(EngineError::new(ErrorCode::ImageReadError)
        .with_detail("no image decoder available (all decoders failed or not registered)"))
}

/// Parse just enough of a JPEG to extract the EXIF `Orientation` tag
/// (TIFF tag 0x0112). Returns `Some(1..=8)` on success.
///
/// We don't bring in a full EXIF crate because we only care about one
/// tag and the rest of the metadata (GPS, maker notes, …) has no
/// privacy- or correctness-impacting role in a game runtime.
fn detect_jpeg_exif_orientation(data: &[u8]) -> Option<u8> {
    if data.len() < 4 || data[0] != 0xFF || data[1] != 0xD8 {
        return None; // not JPEG
    }
    let mut i = 2;
    while i + 4 < data.len() {
        if data[i] != 0xFF {
            return None;
        }
        // Skip fill bytes (FF FF ... sequences).
        let mut marker_i = i + 1;
        while marker_i < data.len() && data[marker_i] == 0xFF {
            marker_i += 1;
        }
        let marker = *data.get(marker_i)?;
        i = marker_i + 1;
        // SOS (0xDA) marks start of compressed data; EXIF is always before it.
        if marker == 0xDA || marker == 0xD9 {
            return None;
        }
        if i + 2 > data.len() {
            return None;
        }
        let seg_len = ((data[i] as usize) << 8) | (data[i + 1] as usize);
        if seg_len < 2 || i + seg_len > data.len() {
            return None;
        }
        let seg = &data[i + 2..i + seg_len];
        i += seg_len;
        // Look for APP1 with "Exif\0\0" prefix.
        if marker == 0xE1 && seg.len() > 6 && &seg[0..6] == b"Exif\0\0" {
            let tiff = &seg[6..];
            if tiff.len() < 8 {
                return None;
            }
            let (little, magic_ok) = match &tiff[0..2] {
                b"II" => (true, tiff[2] == 0x2A && tiff[3] == 0x00),
                b"MM" => (false, tiff[2] == 0x00 && tiff[3] == 0x2A),
                _ => return None,
            };
            if !magic_ok {
                return None;
            }
            let ifd_offset = if little {
                u32::from_le_bytes([tiff[4], tiff[5], tiff[6], tiff[7]]) as usize
            } else {
                u32::from_be_bytes([tiff[4], tiff[5], tiff[6], tiff[7]]) as usize
            };
            if ifd_offset + 2 > tiff.len() {
                return None;
            }
            let entry_count = if little {
                u16::from_le_bytes([tiff[ifd_offset], tiff[ifd_offset + 1]])
            } else {
                u16::from_be_bytes([tiff[ifd_offset], tiff[ifd_offset + 1]])
            } as usize;
            let entries_start = ifd_offset + 2;
            for e in 0..entry_count {
                let off = entries_start + e * 12;
                if off + 12 > tiff.len() {
                    break;
                }
                let tag = if little {
                    u16::from_le_bytes([tiff[off], tiff[off + 1]])
                } else {
                    u16::from_be_bytes([tiff[off], tiff[off + 1]])
                };
                if tag == 0x0112 {
                    // Orientation is SHORT (type=3) in bytes 8..10.
                    let val = if little {
                        u16::from_le_bytes([tiff[off + 8], tiff[off + 9]])
                    } else {
                        u16::from_be_bytes([tiff[off + 8], tiff[off + 9]])
                    };
                    if (1..=8).contains(&val) {
                        return Some(val as u8);
                    }
                    return None;
                }
            }
            return None;
        }
    }
    None
}

/// Apply an EXIF `Orientation` value to an already-decoded RGBA8
/// image, producing a visually-correct buffer. Values 1..8 follow the
/// TIFF spec:
///
/// | value | meaning                       |
/// |-------|-------------------------------|
/// | 1     | Normal                        |
/// | 2     | Flip horizontal               |
/// | 3     | Rotate 180                    |
/// | 4     | Flip vertical                 |
/// | 5     | Transpose (flip along TL-BR)  |
/// | 6     | Rotate 90 CW                  |
/// | 7     | Transverse (flip along TR-BL) |
/// | 8     | Rotate 90 CCW                 |
fn apply_exif_orientation(img: NormalizedImage, orientation: u8) -> NormalizedImage {
    if orientation <= 1 || orientation > 8 {
        return img;
    }
    let NormalizedImage {
        width,
        height,
        rgba,
    } = img;
    let w = width as usize;
    let h = height as usize;
    let src: &[u8] = rgba.as_ref();
    if src.len() != w * h * 4 {
        return NormalizedImage {
            width,
            height,
            rgba,
        }; // malformed; leave untouched
    }
    let mut dst = vec![0u8; src.len()];

    // Helper to write pixel (x, y) of the output buffer from
    // source position (sx, sy). `out_w` is the output width.
    let mut put = |out_w: usize, x: usize, y: usize, sx: usize, sy: usize| {
        let si = (sy * w + sx) * 4;
        let di = (y * out_w + x) * 4;
        dst[di..di + 4].copy_from_slice(&src[si..si + 4]);
    };

    let (new_w, new_h) = match orientation {
        5 | 6 | 7 | 8 => (height, width), // 90 CW / 90 CCW / transverse / transpose
        _ => (width, height),
    };
    let new_w_usize = new_w as usize;

    for y in 0..h {
        for x in 0..w {
            let (nx, ny) = match orientation {
                2 => (w - 1 - x, y),
                3 => (w - 1 - x, h - 1 - y),
                4 => (x, h - 1 - y),
                5 => (y, x),                 // transpose
                6 => (h - 1 - y, x),         // rotate 90 CW
                7 => (h - 1 - y, w - 1 - x), // transverse
                8 => (y, w - 1 - x),         // rotate 90 CCW
                _ => (x, y),
            };
            put(new_w_usize, nx, ny, x, y);
        }
    }

    NormalizedImage {
        width: new_w,
        height: new_h,
        rgba: Arc::new(dst),
    }
}

/// Decode to the best available representation. When `allow_ahb` is true,
/// prefers the registered AHB zero-staging path (Android API 26+ via
/// [`register_platform_ahb_decoder`]); otherwise falls through to
/// [`decode_image_fast`] and wraps the result as
/// [`shared::protocol::io_cmd::DecodedImage::Rgba`].
///
/// Callers that know they'll need CPU-side RGBA (e.g. `getImageData`,
/// resize, crop) can still call [`decode_image_fast`] directly to
/// skip the AHB-to-RGBA downgrade round-trip; everyone else should
/// prefer this entry so fast GPU-side uploads happen automatically
/// when supported.
pub fn decode_image_to_any(
    data: &[u8],
    path_hint: Option<&str>,
    allow_ahb: bool,
) -> Result<shared::protocol::io_cmd::DecodedImage, EngineError> {
    use shared::protocol::io_cmd::DecodedImage;

    // Image-bomb guard (pre-decode): same header check as decode_image_fast, so
    // the AHB path is capped too. The RGBA fallback below re-checks via
    // decode_image_fast.
    if let Some((w, h)) = probe_image_dimensions(data) {
        enforce_pixel_cap(w, h)?;
    }

    if let Some(ahb_decoder) = select_ahb_decoder(allow_ahb, PLATFORM_AHB_DECODER.get().copied()) {
        match ahb_decoder(data) {
            Ok(ahb) => {
                enforce_pixel_cap(ahb.width, ahb.height)?;
                return Ok(DecodedImage::HardwareBuffer(ahb));
            }
            Err(e) => {
                // AHB decode failed. Bump the total counter and the
                // per-reason bucket so operators can see at a glance
                // *why* the zero-copy path didn't engage (format,
                // hardware, size, etc.) without re-running traces.
                let reason = classify_ahb_fallback(&e);
                shared::stats::io_metrics_global().record_ahb_fallback(reason);
                tracing::debug!("AHB decode failed ({reason:?}), falling back to RGBA: {e:?}");
            }
        }
    }

    decode_image_fast(data, path_hint).map(DecodedImage::Rgba)
}

/// Best-effort classifier mapping an AHB decode error into a
/// [`shared::stats::AhbFallbackReason`]. The AHB decoder is platform-
/// specific and doesn't expose a typed error taxonomy, so we pattern-
/// match on the error's detail / msg / code fields.
fn classify_ahb_fallback(e: &EngineError) -> shared::stats::AhbFallbackReason {
    use shared::stats::AhbFallbackReason::*;
    let detail = e.detail.as_deref().unwrap_or("").to_ascii_lowercase();
    let msg = e.msg.as_ref().to_ascii_lowercase();
    let haystack = format!("{detail} {msg}");

    // Order matters: "too large" wins over generic "decoder" because
    // the native AHB path surfaces size rejections as decoder errors.
    if haystack.contains("too large")
        || haystack.contains("too big")
        || haystack.contains("exceeds")
        || haystack.contains("size limit")
        || haystack.contains("max dimension")
    {
        return TooLarge;
    }
    if haystack.contains("unsupported format")
        || haystack.contains("unsupported codec")
        || haystack.contains("no decoder")
        || haystack.contains("unknown mime")
    {
        return UnsupportedFormat;
    }
    if haystack.contains("hardwarebuffer")
        || haystack.contains("ahardwarebuffer")
        || haystack.contains("ahb_alloc")
        || haystack.contains("no hardware buffer")
        || haystack.contains("api level")
        || haystack.contains("unsupported device")
    {
        return HardwareBufferUnavailable;
    }
    if haystack.contains("decoder")
        || haystack.contains("imagedecoder")
        || haystack.contains("corrupt")
        || haystack.contains("malformed")
    {
        return DecoderRejected;
    }
    Unknown
}

#[cfg(test)]
mod ahb_selection_tests {
    use super::*;

    fn available_decoder(_: &[u8]) -> Result<AhbImage, EngineError> {
        panic!("selection test must not invoke the decoder")
    }

    #[test]
    fn renderer_capability_is_required_even_when_decoder_is_registered() {
        assert!(select_ahb_decoder(false, Some(available_decoder)).is_none());
        assert!(select_ahb_decoder(true, None).is_none());
        assert!(select_ahb_decoder(true, Some(available_decoder)).is_some());
    }
}

/// Decode using zune-image (fast, SIMD-optimized).
#[cfg(feature = "rust-image-decode")]
fn decode_with_zune(data: &[u8]) -> Result<NormalizedImage, EngineError> {
    use std::io::Cursor;
    use zune_core::colorspace::ColorSpace;
    use zune_core::options::DecoderOptions;
    use zune_image::image::Image;

    // Configure decoder options
    let options = DecoderOptions::new_fast();

    // Wrap data in a Cursor to provide Seek trait
    let cursor = Cursor::new(data);

    // Auto-detect format and decode
    let mut image = Image::read(cursor, options).map_err(|e| {
        EngineError::new(ErrorCode::ImageReadError)
            .with_detail(format!("zune decode error: {:?}", e))
    })?;

    // Convert to RGBA colorspace
    image.convert_color(ColorSpace::RGBA).map_err(|e| {
        EngineError::new(ErrorCode::ImageReadError)
            .with_detail(format!("zune color convert error: {:?}", e))
    })?;

    // Get dimensions
    let (width, height) = image.dimensions();

    // Flatten to RGBA bytes
    let rgba = image.flatten_to_u8().pop().ok_or_else(|| {
        EngineError::new(ErrorCode::ImageReadError).with_detail("zune: no image data")
    })?;

    // Verify we got RGBA (4 bytes per pixel)
    let expected_len = (width * height * 4) as usize;
    if rgba.len() != expected_len {
        return Err(
            EngineError::new(ErrorCode::ImageReadError).with_detail(format!(
                "zune: expected {} bytes, got {}",
                expected_len,
                rgba.len()
            )),
        );
    }

    Ok(NormalizedImage {
        width: width as u32,
        height: height as u32,
        rgba: Arc::new(rgba),
    })
}

/// Decode image using the `image` crate (fallback).
#[cfg(feature = "rust-image-decode")]
fn decode_with_image_crate(data: &[u8]) -> Result<NormalizedImage, EngineError> {
    use image::GenericImageView;
    use std::io::Cursor;

    // Bound the decoder's allocation to the pixel cap BEFORE decoding, so a
    // format the header probe can't read (TIFF, uncommon JPEG SOF, animated
    // containers, ...) can't allocate a multi-GiB buffer that only the
    // post-decode check would catch. `max_alloc` caps total decode allocation;
    // MAX_IMAGE_PIXELS * 4 is the RGBA byte ceiling.
    let mut limits = image::Limits::no_limits();
    limits.max_alloc = Some(MAX_IMAGE_PIXELS.saturating_mul(4));

    let mut reader = image::ImageReader::new(Cursor::new(data))
        .with_guessed_format()
        .map_err(|e| {
            EngineError::new(ErrorCode::ImageReadError)
                .with_detail(format!("image crate format detection error: {}", e))
        })?;
    reader.limits(limits);
    let img = reader.decode().map_err(|e| {
        EngineError::new(ErrorCode::ImageReadError)
            .with_detail(format!("image crate decode error: {}", e))
    })?;

    let (width, height) = img.dimensions();
    // `max_alloc` above bounds the decoder's *native* output (e.g. 1 byte/px
    // for L8 grayscale), but `into_rgba8()` is a separate allocation that
    // expands sub-RGBA formats by up to 4x. Enforce the pixel cap before that
    // expansion so a low-bpp giant (grayscale TIFF, uncommon SOF, ...) that
    // squeaked under `max_alloc` can't balloon past the RGBA ceiling here,
    // before the caller's post-decode check ever runs.
    enforce_pixel_cap(width, height)?;
    let rgba = img.into_rgba8().into_raw();

    Ok(NormalizedImage {
        width,
        height,
        rgba: Arc::new(rgba),
    })
}

/// Resize a decoded image to fit within `target_w x target_h`, preserving
/// aspect ratio. Uses bilinear filtering via the `image` crate.
///
/// If the image is already within the target dimensions, returns it unchanged.
/// Upper bound on a cropped RGBA buffer (128 MiB). Matches the soft
/// image byte budget; callers get a structured error instead of an
/// OOM abort when a game passes in a pathological `(sw, sh)`.
pub const MAX_SUBRECT_BYTES: usize = 128 * 1024 * 1024;

/// Crop `img` to the pixel rectangle `(sx, sy, sw, sh)`, with
/// WHATWG ImageBitmap out-of-bounds semantics: regions that fall
/// outside the source image are filled with transparent black
/// (`rgba(0, 0, 0, 0)`).
///
/// Returns a fresh `NormalizedImage` sized `sw x sh`.  When the
/// clipped region covers the whole source and has identical
/// dimensions, the input is returned unchanged (no allocation).
///
/// # Errors
/// * `InvalidArgument` when `sw > i32::MAX`, `sh > i32::MAX`, or
///   `sw * sh * 4` would overflow `usize`. The `i32::MAX` guard
///   prevents a `u32 -> i32` negative wrap downstream.
/// * `OutOfMemory` when the required buffer exceeds
///   [`MAX_SUBRECT_BYTES`].
///
/// Unlike [`resize_image`] this does not depend on the optional
/// `rust-image-decode` feature — crop is a pure CPU operation on
/// the RGBA buffer we always hold.
pub fn crop_image(
    img: NormalizedImage,
    sx: i32,
    sy: i32,
    sw: u32,
    sh: u32,
) -> Result<NormalizedImage, EngineError> {
    // Guard against u32 -> i32 wrap: later math casts `sw as i32`,
    // which silently produces a negative number for sw > i32::MAX.
    if sw > i32::MAX as u32 || sh > i32::MAX as u32 {
        return Err(EngineError::new(ErrorCode::InvalidArgument)
            .with_detail(format!("crop: size {}x{} exceeds i32::MAX", sw, sh)));
    }

    // Fast path: no-op crop.
    if sx == 0 && sy == 0 && sw == img.width && sh == img.height {
        return Ok(img);
    }

    // Checked allocation size. `sw`, `sh` are at most i32::MAX here,
    // so these multiplications only overflow usize on 32-bit targets.
    let pixels = (sw as usize).checked_mul(sh as usize).ok_or_else(|| {
        EngineError::new(ErrorCode::InvalidArgument)
            .with_detail(format!("crop: {}*{} pixel count overflows usize", sw, sh))
    })?;
    let bytes = pixels.checked_mul(4).ok_or_else(|| {
        EngineError::new(ErrorCode::InvalidArgument)
            .with_detail(format!("crop: {} pixels * 4 bytes overflows usize", pixels))
    })?;
    if bytes > MAX_SUBRECT_BYTES {
        return Err(
            EngineError::new(ErrorCode::OutOfMemory).with_detail(format!(
                "crop: {}x{} ({} bytes) exceeds MAX_SUBRECT_BYTES={}",
                sw, sh, bytes, MAX_SUBRECT_BYTES
            )),
        );
    }

    let out_w = sw as i32;
    let out_h = sh as i32;
    let mut out = vec![0u8; bytes];

    // Compute intersection of destination rect with source image.
    let src_w = img.width as i32;
    let src_h = img.height as i32;
    let intersect_x0 = sx.max(0);
    let intersect_y0 = sy.max(0);
    // Use saturating arithmetic for sx+out_w so very large (but valid)
    // sx values don't wrap past i32::MAX before `.min(src_w)` clamps.
    let intersect_x1 = sx.saturating_add(out_w).min(src_w);
    let intersect_y1 = sy.saturating_add(out_h).min(src_h);
    if intersect_x1 > intersect_x0 && intersect_y1 > intersect_y0 {
        let row_bytes_src = (src_w as usize) * 4;
        let row_bytes_dst = (out_w as usize) * 4;
        for y in intersect_y0..intersect_y1 {
            let src_row_start = (y as usize) * row_bytes_src + (intersect_x0 as usize) * 4;
            let src_row_end = (y as usize) * row_bytes_src + (intersect_x1 as usize) * 4;
            let dst_y = (y - sy) as usize;
            let dst_x = (intersect_x0 - sx) as usize;
            let dst_start = dst_y * row_bytes_dst + dst_x * 4;
            let copy_len = src_row_end - src_row_start;
            out[dst_start..dst_start + copy_len]
                .copy_from_slice(&img.rgba[src_row_start..src_row_end]);
        }
    }

    Ok(NormalizedImage {
        width: sw,
        height: sh,
        rgba: std::sync::Arc::new(out),
    })
}

#[cfg(feature = "rust-image-decode")]
fn resize_image_supported(img: NormalizedImage, target_w: u32, target_h: u32) -> NormalizedImage {
    if img.width <= target_w && img.height <= target_h {
        return img;
    }
    let scale = f64::min(
        target_w as f64 / img.width as f64,
        target_h as f64 / img.height as f64,
    );
    let new_w = ((img.width as f64 * scale).round() as u32).max(1);
    let new_h = ((img.height as f64 * scale).round() as u32).max(1);
    let w = img.width;
    let h = img.height;
    let raw: Vec<u8> = Arc::try_unwrap(img.rgba).unwrap_or_else(|arc| (*arc).clone());
    let required = (w as usize)
        .checked_mul(h as usize)
        .and_then(|px| px.checked_mul(4));
    if required.is_none_or(|needed| raw.len() < needed) {
        tracing::warn!(
            "resize_image: buffer holds {} bytes, {}x{} needs {:?} — skipping resize",
            raw.len(),
            w,
            h,
            required
        );
        return NormalizedImage {
            width: w,
            height: h,
            rgba: Arc::new(raw),
        };
    }
    let src = match image::RgbaImage::from_raw(w, h, raw) {
        Some(s) => s,
        None => {
            tracing::warn!("resize_image: from_raw rejected a checked {w}x{h} buffer");
            return NormalizedImage {
                width: 0,
                height: 0,
                rgba: Arc::new(Vec::new()),
            };
        }
    };
    let resized =
        image::imageops::resize(&src, new_w, new_h, image::imageops::FilterType::Triangle);
    NormalizedImage {
        width: new_w,
        height: new_h,
        rgba: Arc::new(resized.into_raw()),
    }
}

/// Resize capability is independent from the decoder entry points: callers
/// that request a target size must receive either an exact result or an error.
#[cfg(feature = "rust-image-decode")]
pub fn try_resize_image(
    img: NormalizedImage,
    target_w: u32,
    target_h: u32,
) -> Result<NormalizedImage, EngineError> {
    Ok(resize_image_supported(img, target_w, target_h))
}

/// Report whether exact CPU image resizing is available in this build.
pub const fn resize_capable() -> bool {
    cfg!(feature = "rust-image-decode")
}

/// When the Rust image feature is absent, do not silently ignore a target size.
#[cfg(not(feature = "rust-image-decode"))]
pub fn try_resize_image(
    _img: NormalizedImage,
    _target_w: u32,
    _target_h: u32,
) -> Result<NormalizedImage, EngineError> {
    Err(EngineError::new(ErrorCode::Unsupported)
        .with_msg("CPU image resize requires the rust-image-decode feature"))
}

#[cfg(test)]
// Pixel indices here are written out in full as `(row * width + col) * bpp`,
// including the terms that multiply by zero or one. Reducing `(0 * 4 + 2) * 4`
// to `8` would satisfy the lint and lose the thing the line is for: which pixel
// the assertion is about. The formula is the documentation.
#[allow(clippy::erasing_op, clippy::identity_op)]
mod crop_tests {
    use super::*;
    use std::sync::Arc;

    fn checkerboard(w: u32, h: u32) -> NormalizedImage {
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                let on = (x + y) % 2 == 0;
                rgba[i] = if on { 255 } else { 0 };
                rgba[i + 1] = if on { 255 } else { 0 };
                rgba[i + 2] = if on { 255 } else { 0 };
                rgba[i + 3] = 255;
            }
        }
        NormalizedImage {
            width: w,
            height: h,
            rgba: Arc::new(rgba),
        }
    }

    #[test]
    fn crop_identity_returns_input() {
        let img = checkerboard(4, 4);
        let out = crop_image(img.clone(), 0, 0, 4, 4).expect("identity crop");
        assert_eq!(out.width, 4);
        assert_eq!(out.height, 4);
        assert_eq!(&*out.rgba, &*img.rgba);
    }

    #[test]
    fn crop_center_region_extracts_pixels_correctly() {
        // Source is a 4x4 checkerboard starting "on" at (0,0).
        // Extract the 2x2 from (1,1) — should start "on" at (1,1)
        // because (x+y) = 2, even.
        let img = checkerboard(4, 4);
        let out = crop_image(img, 1, 1, 2, 2).expect("center crop");
        assert_eq!(out.width, 2);
        assert_eq!(out.height, 2);
        // Top-left pixel of output = source(1,1): on.
        assert_eq!(out.rgba[0], 255);
        // (0,1) of output = source(1,2): off.
        let row1_off = (1 * 2 * 4) as usize;
        assert_eq!(out.rgba[row1_off + 0], 0);
    }

    #[test]
    fn crop_entirely_out_of_bounds_returns_transparent_black() {
        let img = checkerboard(4, 4);
        let out = crop_image(img, 10, 10, 3, 3).expect("oob crop");
        assert_eq!(out.width, 3);
        assert_eq!(out.height, 3);
        assert!(out.rgba.iter().all(|&b| b == 0));
    }

    #[test]
    fn crop_partially_out_of_bounds_fills_missing_with_transparent_black() {
        // Source 4x4, extract 6x6 starting at (-1, -1).  The 5x5 region
        // from (0,0) to (4,4) should be the source checker; the rim
        // is zeroed.
        let img = checkerboard(4, 4);
        let out = crop_image(img, -1, -1, 6, 6).expect("partial oob crop");
        assert_eq!(out.width, 6);
        assert_eq!(out.height, 6);
        // Top-left corner (output pixel (0,0) maps to source(-1,-1)
        // which is out of bounds) should be zero.
        assert_eq!(&out.rgba[0..4], &[0, 0, 0, 0]);
        // Output pixel (1,1) maps to source(0,0): on, so RGBA =
        // (255,255,255,255).
        let idx = (1 * 6 + 1) * 4;
        assert_eq!(&out.rgba[idx..idx + 4], &[255, 255, 255, 255]);
    }

    #[test]
    fn crop_rejects_size_above_i32_max() {
        // `sw as i32` would wrap to a negative value downstream; we
        // must reject before that.  Use 1-pixel tall so the pixel
        // count itself doesn't overflow first.
        let img = checkerboard(4, 4);
        let err = crop_image(img, 0, 0, (i32::MAX as u32) + 1, 1)
            .expect_err("i32::MAX+1 must be rejected");
        assert_eq!(err.code, ErrorCode::InvalidArgument);
    }

    #[test]
    fn crop_rejects_pixel_count_overflow() {
        // On 64-bit targets usize is 64-bit so `sw * sh` only overflows
        // above ~4 billion × ~4 billion; we already reject those via
        // the i32::MAX guard.  On 32-bit targets usize is 32-bit and
        // `(0x1_0000 * 0x1_0000)` overflows. Either way the caller
        // must see an error, not a panic.
        let img = checkerboard(4, 4);
        // 65536 x 65536 x 4 = 16 GiB; exceeds MAX_SUBRECT_BYTES on
        // every target, so this also validates the size-cap path.
        let err = crop_image(img, 0, 0, 65_536, 65_536).expect_err("huge crop must be rejected");
        assert!(
            matches!(
                err.code,
                ErrorCode::InvalidArgument | ErrorCode::OutOfMemory
            ),
            "unexpected code: {:?}",
            err.code
        );
    }

    #[test]
    fn crop_rejects_above_max_subrect_bytes() {
        // 8192 x 8192 x 4 = 256 MiB > MAX_SUBRECT_BYTES (128 MiB).
        let img = checkerboard(4, 4);
        let err =
            crop_image(img, 0, 0, 8192, 8192).expect_err("> MAX_SUBRECT_BYTES must be rejected");
        assert_eq!(err.code, ErrorCode::OutOfMemory);
    }

    #[test]
    fn crop_at_max_subrect_bytes_succeeds() {
        // Largest allowed output: 128 MiB / 4 = 32 Mpx.
        // 8192 x 4096 x 4 = 128 MiB exactly. The source is tiny so
        // everything is out-of-bounds (transparent black), but the
        // allocation must succeed.
        let img = checkerboard(4, 4);
        let out = crop_image(img, 100, 100, 8192, 4096).expect("exact cap ok");
        assert_eq!(out.width, 8192);
        assert_eq!(out.height, 4096);
    }

    // ---- boundary-extension edges (P2-1) ----------------------------

    #[test]
    fn crop_extending_past_right_edge_copies_intersection_only() {
        // Source 4x4, request (sx=2, sy=0, sw=4, sh=2) — i.e. 4
        // columns wide starting at x=2.  Only x=[2,4) intersects
        // the source; the last 2 columns must be transparent
        // black, not wrapped or garbage.
        let img = checkerboard(4, 4);
        let out = crop_image(img, 2, 0, 4, 2).expect("rightward crop");
        assert_eq!(out.width, 4);
        assert_eq!(out.height, 2);
        // Output (2, 0) = source (4, 0) = OOB = zero.
        let idx = (0 * 4 + 2) * 4;
        assert_eq!(&out.rgba[idx..idx + 4], &[0, 0, 0, 0]);
        // Output (0, 0) = source (2, 0) = on (checker).
        assert_eq!(&out.rgba[0..4], &[255, 255, 255, 255]);
    }

    #[test]
    fn crop_extending_past_bottom_edge_copies_intersection_only() {
        // Source 4x4, request (sx=0, sy=3, sw=2, sh=4) — only the
        // last row of source overlaps.  Remaining 3 rows must be
        // transparent black (alpha=0), which is distinct from the
        // checker's "off" pixels (alpha=255 opaque black).
        let img = checkerboard(4, 4);
        let out = crop_image(img, 0, 3, 2, 4).expect("downward crop");
        assert_eq!(out.width, 2);
        assert_eq!(out.height, 4);
        // Output (0, 0) = source (0, 3) — checker "off" (x+y odd):
        // RGB=0 but alpha=255.  Proves the copy took the real row.
        assert_eq!(&out.rgba[0..4], &[0, 0, 0, 255]);
        // Output (1, 0) = source (1, 3) — checker "on" (x+y even).
        assert_eq!(&out.rgba[4..8], &[255, 255, 255, 255]);
        // Output (0, 1) = source (0, 4) = OOB.  Must be fully
        // transparent black; alpha=0 here distinguishes it from
        // the "off" pixels above.
        let idx = (1 * 2 + 0) * 4;
        assert_eq!(&out.rgba[idx..idx + 4], &[0, 0, 0, 0]);
        // All of rows 1..4 are OOB — every byte must be zero.
        for y in 1..4 {
            for x in 0..2 {
                let i = ((y * 2 + x) * 4) as usize;
                assert_eq!(
                    &out.rgba[i..i + 4],
                    &[0, 0, 0, 0],
                    "OOB pixel at ({}, {}) not transparent",
                    x,
                    y
                );
            }
        }
    }

    #[test]
    fn crop_one_pixel_intersection_at_max_sx() {
        // sx = i32::MAX - 1 is a pathological-but-legal JS input;
        // saturating_add must keep us from wrapping to negative
        // before the src-bounds clamp.  Result is fully OOB.
        let img = checkerboard(4, 4);
        let out = crop_image(img, i32::MAX - 1, 0, 2, 2).expect("no panic on near-max sx");
        assert_eq!(out.width, 2);
        assert_eq!(out.height, 2);
        assert!(out.rgba.iter().all(|&b| b == 0));
    }
}

#[cfg(test)]
mod pixel_cap_tests {
    use super::*;

    #[test]
    fn accepts_exactly_at_cap() {
        // 64 Mpx fits in u32; a 1-tall strip of MAX_IMAGE_PIXELS width sits
        // exactly at the ceiling and must be accepted.
        let n = MAX_IMAGE_PIXELS as u32;
        assert!(enforce_pixel_cap(n, 1).is_ok());
    }

    #[test]
    fn rejects_one_over_cap() {
        let n = MAX_IMAGE_PIXELS as u32;
        let err = enforce_pixel_cap(n + 1, 1).expect_err("one pixel over cap must reject");
        assert_eq!(err.code, ErrorCode::OutOfMemory);
    }

    #[test]
    fn rejects_max_u32_dimensions_without_panic() {
        // (u32::MAX as u64)^2 still fits in u64, so saturating_mul returns the
        // exact (astronomical) product; the guard must reject it cleanly with
        // no overflow panic.
        let err = enforce_pixel_cap(u32::MAX, u32::MAX).expect_err("huge dims must reject");
        assert_eq!(err.code, ErrorCode::OutOfMemory);
    }
}

#[cfg(test)]
mod resize_capability_tests {
    use super::*;
    use std::sync::Arc;

    fn solid(w: u32, h: u32) -> NormalizedImage {
        NormalizedImage {
            width: w,
            height: h,
            rgba: Arc::new(vec![128u8; (w * h * 4) as usize]),
        }
    }

    /// The new `try_resize_image` must FAIL (return Err) when the feature is off
    /// rather than silently returning the full-size image.
    /// RED: this test does not compile until `try_resize_image` is added below.
    #[cfg(not(feature = "rust-image-decode"))]
    #[test]
    fn try_resize_without_feature_returns_unsupported() {
        let img = solid(100, 100);
        let result = try_resize_image(img, 50, 50);
        assert!(
            result.is_err(),
            "resize without rust-image-decode must be Err, not silent full-size Ok"
        );
        let e = result.unwrap_err();
        assert_eq!(e.code, ErrorCode::Unsupported);
    }

    /// With `rust-image-decode` the resize must produce the correct dimensions.
    #[cfg(feature = "rust-image-decode")]
    #[test]
    fn try_resize_with_feature_honours_target_size() {
        let img = solid(100, 100);
        let out = try_resize_image(img, 50, 50).expect("resize with feature must succeed");
        assert_eq!(out.width, 50);
        assert_eq!(out.height, 50);
    }
}

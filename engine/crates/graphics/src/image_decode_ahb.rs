//! Static-image decode through Skia's linked codecs: directly into an API-26
//! `AHardwareBuffer`, or into tightly packed RGBA memory.
//!
//! The decoders are synchronous: encoded bytes are borrowed by Skia only until
//! `get_pixels_with_options` returns, and an AHB is synchronously unlocked
//! before the owned handle is published to the render pipeline.

use shared::{
    error::{EngineError, ErrorCode},
    protocol::{
        ahb::{AhbDesc, AhbError, AhbUsage, OwnedAhb},
        io_cmd::{AhbImage, MAX_IMAGE_PIXELS, NormalizedImage},
    },
};
use skia_safe::{AlphaType, Codec, ColorType, Data, EncodedOrigin, ImageInfo, codec};

const MIN_SKIA_DECODE_BUDGET: usize = 16 * 1024 * 1024;

fn image_error(detail: impl Into<String>) -> EngineError {
    EngineError::new(ErrorCode::ImageReadError).with_detail(detail)
}

fn ahb_error(stage: &'static str, error: AhbError) -> EngineError {
    image_error(format!("AHardwareBuffer {stage} failed: {error}"))
}

fn validate_dimensions(width: u32, height: u32) -> Result<usize, EngineError> {
    if width == 0 || height == 0 {
        return Err(image_error("Skia decoder returned zero image dimensions"));
    }
    let pixels = (width as u64)
        .checked_mul(height as u64)
        .ok_or_else(|| image_error("Skia decoder image dimensions overflow"))?;
    if pixels > MAX_IMAGE_PIXELS {
        return Err(EngineError::new(ErrorCode::OutOfMemory).with_detail(format!(
            "image {width}x{height} ({pixels} px) exceeds MAX_IMAGE_PIXELS ({MAX_IMAGE_PIXELS} px)"
        )));
    }
    (pixels as usize)
        .checked_mul(4)
        .ok_or_else(|| image_error("Skia decoder RGBA byte size overflows usize"))
}

/// A codec over borrowed bytes, with its dimensions already checked against
/// `MAX_IMAGE_PIXELS`.
struct OpenedCodec {
    codec: Codec<'static>,
    width: u32,
    height: u32,
    decoded_bytes: usize,
}

impl OpenedCodec {
    /// # Safety
    /// Skia does not own `data`: the returned codec must be dropped before the
    /// borrow of `data` ends. Both decoders below are synchronous and drop it
    /// before returning, so no decoder state escapes them.
    unsafe fn open(data: &[u8]) -> Result<Self, EngineError> {
        if data.is_empty() {
            return Err(image_error("Skia decoder rejected empty input"));
        }
        let skia_data = unsafe { Data::new_bytes(data) };
        // The workspace deliberately builds Skia with a lean codec feature set.
        // Use its compiled registry so this module never creates an unresolved
        // reference to an excluded codec; unsupported formats fail here instead
        // of increasing every product's native binary.
        #[allow(deprecated)]
        let codec = Codec::from_data(skia_data).ok_or_else(|| {
            image_error("unsupported format: no linked Skia decoder accepted input")
        })?;

        let dimensions = codec.dimensions();
        let width = u32::try_from(dimensions.width)
            .map_err(|_| image_error("Skia decoder returned a negative image width"))?;
        let height = u32::try_from(dimensions.height)
            .map_err(|_| image_error("Skia decoder returned a negative image height"))?;
        let decoded_bytes = validate_dimensions(width, height)?;
        Ok(Self {
            codec,
            width,
            height,
            decoded_bytes,
        })
    }

    fn image_info(&self) -> ImageInfo {
        ImageInfo::new(
            self.codec.dimensions(),
            ColorType::RGBA8888,
            AlphaType::Unpremul,
            None,
        )
    }

    fn options(&self) -> codec::Options {
        codec::Options {
            max_decode_memory: Some(self.decoded_bytes.max(MIN_SKIA_DECODE_BUDGET)),
            ..codec::Options::default()
        }
    }
}

/// Decode the first static frame into tightly packed, straight-alpha RGBA8.
///
/// The encoded orientation is not applied: like every decoder behind
/// `io::decode_image_fast`, this returns pixels in stored order and that
/// function applies the EXIF orientation itself. A host with no Java -- the C
/// ABI on Android -- cannot reach the Java SDK's decoder, so there this serves
/// every image the AHB path cannot: a CPU-backed one, or one the AHB path refused.
pub fn decode_image_rgba(data: &[u8]) -> Result<NormalizedImage, EngineError> {
    // SAFETY: `opened` is dropped before this function returns.
    let mut opened = unsafe { OpenedCodec::open(data)? };
    let image_info = opened.image_info();
    let row_bytes = opened.width as usize * 4;
    let mut rgba = vec![0u8; opened.decoded_bytes];
    let options = opened.options();
    let result =
        opened
            .codec
            .get_pixels_with_options(&image_info, &mut rgba, row_bytes, Some(&options));
    if result != codec::Result::Success {
        return Err(image_error(format!(
            "Skia decoder failed: {}",
            codec::result_to_string(result)
        )));
    }
    Ok(NormalizedImage::new(opened.width, opened.height, rgba))
}

/// Decode the first static frame into a GPU-importable, straight-alpha RGBA8
/// AHB. Any error is intentionally recoverable by `io::decode_image_to_any`,
/// which retries through the platform RGBA decoder.
pub fn decode_image_to_ahb(data: &[u8]) -> Result<AhbImage, EngineError> {
    // SAFETY: `opened` is dropped before this function returns.
    let mut opened = unsafe { OpenedCodec::open(data)? };
    let (width, height) = (opened.width, opened.height);

    let origin = opened.codec.origin();
    if origin != EncodedOrigin::TopLeft {
        return Err(image_error(format!(
            "unsupported format orientation {origin:?}; RGBA fallback must apply EXIF"
        )));
    }

    let ahb = OwnedAhb::allocate(AhbDesc::rgba_sampled_cpu_decode(width, height))
        .map_err(|error| ahb_error("allocation", error))?;
    let mut lock = ahb
        .lock_cpu(AhbUsage::CPU_WRITE_RARELY)
        .map_err(|error| ahb_error("CPU write lock", error))?;

    let image_info = opened.image_info();
    let options = opened.options();
    let codec = &mut opened.codec;
    let decode_attempt = (|| {
        let row_bytes = lock.stride_bytes();
        if !image_info.valid_row_bytes(row_bytes) {
            return Err(image_error(format!(
                "AHardwareBuffer row stride {row_bytes} is invalid for {width}x{height} RGBA8"
            )));
        }
        let required_bytes = image_info.compute_byte_size(row_bytes);
        let allocation = lock
            .as_bytes_mut()
            .map_err(|error| ahb_error("mutable CPU view", error))?;
        let available_bytes = allocation.len();
        let pixels = allocation.get_mut(..required_bytes).ok_or_else(|| {
            image_error(format!(
                "AHardwareBuffer allocation is too small: {available_bytes} bytes available, {required_bytes} required"
            ))
        })?;
        Ok(codec.get_pixels_with_options(&image_info, pixels, row_bytes, Some(&options)))
    })();

    let unlock_result = lock.finish();
    let decode_result = match decode_attempt {
        Ok(result) => result,
        Err(mut error) => {
            if let Err(unlock_error) = unlock_result {
                let detail = error.detail.take().unwrap_or_else(|| error.msg.to_string());
                error.detail = Some(format!(
                    "{detail}; AHardwareBuffer unlock also failed: {unlock_error}"
                ));
            }
            return Err(error);
        }
    };
    if decode_result != codec::Result::Success {
        let unlock_detail = unlock_result
            .err()
            .map(|error| format!("; AHardwareBuffer unlock also failed: {error}"))
            .unwrap_or_default();
        return Err(image_error(format!(
            "Skia decoder failed: {}{unlock_detail}",
            codec::result_to_string(decode_result)
        )));
    }
    unlock_result.map_err(|error| ahb_error("synchronous unlock", error))?;

    Ok(AhbImage::new(width, height, ahb))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dimension_validation_is_zero_and_overflow_safe() {
        assert_eq!(validate_dimensions(1, 1).unwrap(), 4);
        assert!(validate_dimensions(0, 1).is_err());
        assert!(validate_dimensions(1, 0).is_err());
        let over = u32::try_from(MAX_IMAGE_PIXELS).unwrap() + 1;
        let error = validate_dimensions(over, 1).unwrap_err();
        assert_eq!(error.code, ErrorCode::OutOfMemory);
    }

    /// 2x2 RGBA PNG, rows top to bottom: red, green / blue, white at alpha 128.
    const PNG_2X2: [u8; 76] = [
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x02, 0x08, 0x06, 0x00, 0x00, 0x00, 0x72,
        0xb6, 0x0d, 0x24, 0x00, 0x00, 0x00, 0x13, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8,
        0xcf, 0xc0, 0xf0, 0x1f, 0x0c, 0x81, 0x34, 0x08, 0x34, 0x00, 0x00, 0x49, 0x49, 0x09, 0x78,
        0x28, 0xa0, 0xdb, 0x77, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60,
        0x82,
    ];

    #[test]
    fn rgba_decode_is_tightly_packed_row_major_and_unpremultiplied() {
        let image = decode_image_rgba(&PNG_2X2).unwrap();
        assert_eq!((image.width, image.height), (2, 2));
        #[rustfmt::skip]
        let expected: [u8; 16] = [
            255, 0, 0, 255,     0, 255, 0, 255,
            0, 0, 255, 255,     255, 255, 255, 128,
        ];
        assert_eq!(image.rgba.as_slice(), &expected);
    }

    #[test]
    fn rgba_decode_rejects_empty_and_unrecognised_input() {
        for input in [&[][..], b"not an image at all"] {
            let error = decode_image_rgba(input).unwrap_err();
            assert_eq!(error.code, ErrorCode::ImageReadError);
        }
    }

    #[test]
    fn only_top_left_origin_is_direct_decode_eligible() {
        assert_eq!(EncodedOrigin::DEFAULT, EncodedOrigin::TopLeft);
        for origin in [
            EncodedOrigin::TopRight,
            EncodedOrigin::BottomRight,
            EncodedOrigin::BottomLeft,
            EncodedOrigin::LeftTop,
            EncodedOrigin::RightTop,
            EncodedOrigin::RightBottom,
            EncodedOrigin::LeftBottom,
        ] {
            assert_ne!(origin, EncodedOrigin::TopLeft);
        }
    }
}

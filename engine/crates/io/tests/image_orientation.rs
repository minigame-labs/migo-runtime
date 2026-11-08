//! Separate process: the platform decoder hooks are OnceLocks. The AHB hook
//! models a decoder that returns stored-order pixels and ignores EXIF (as
//! Skia's libpng backend does). This is host routing/pixel coverage, not an
//! Android device or GPU-import result.

use base64::Engine as _;
use migo_io::{decode_image_to_any, register_platform_ahb_decoder};
use shared::{
    error::EngineError,
    protocol::{
        ahb::{AhbDesc, AhbUsage, OwnedAhb, read_rgba_from_ahb},
        io_cmd::{AhbImage, DecodedImage, NormalizedImage},
    },
};
use std::sync::atomic::{AtomicUsize, Ordering};

static AHB_ATTEMPTS: AtomicUsize = AtomicUsize::new(0);
const FIXTURE: &str = include_str!("fixtures/images/orientation-cases.json");

fn stored_pixels(data: &[u8]) -> Vec<u8> {
    let fixture: serde_json::Value = serde_json::from_str(FIXTURE).unwrap();
    let format = if data.starts_with(b"\xff\xd8") {
        "JPEG"
    } else if data.starts_with(b"\x89PNG") {
        "PNG"
    } else {
        "WEBP"
    };
    let base = fixture["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["format"] == format && case["orientation"] == 1)
        .unwrap();
    base["rgba"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n.as_u64().unwrap() as u8)
        .collect()
}

fn stored_order_ahb(data: &[u8]) -> Result<AhbImage, EngineError> {
    AHB_ATTEMPTS.fetch_add(1, Ordering::Relaxed);
    let ahb = OwnedAhb::allocate(AhbDesc::rgba_sampled_cpu_decode(3, 2)).unwrap();
    let mut lock = ahb.lock_cpu(AhbUsage::CPU_WRITE_RARELY).unwrap();
    let stride = lock.stride_bytes();
    let output = lock.as_bytes_mut().unwrap();
    for (row, pixels) in stored_pixels(data).chunks_exact(12).enumerate() {
        output[row * stride..row * stride + 12].copy_from_slice(pixels);
    }
    lock.finish().unwrap();
    Ok(AhbImage::new(3, 2, ahb))
}

// Normal builds register no RGBA fallback that could hide a decoder failure.
// Feature-off builds explicitly supply the platform hook's stored-order contract.
#[cfg(not(feature = "rust-image-decode"))]
fn stored_order_platform(data: &[u8]) -> Result<NormalizedImage, EngineError> {
    Ok(NormalizedImage::new(3, 2, stored_pixels(data)))
}

#[test]
fn exif_images_are_normalized_once_and_rotated_images_skip_ahb() {
    register_platform_ahb_decoder(stored_order_ahb);
    #[cfg(not(feature = "rust-image-decode"))]
    migo_io::register_platform_decoder(stored_order_platform);
    let fixture: serde_json::Value = serde_json::from_str(FIXTURE).unwrap();
    for case in fixture["cases"].as_array().unwrap() {
        let encoded = base64::engine::general_purpose::STANDARD
            .decode(case["encoded"].as_str().unwrap())
            .unwrap();
        let before = AHB_ATTEMPTS.load(Ordering::Relaxed);
        let decoded = match decode_image_to_any(&encoded, None, true).unwrap() {
            DecodedImage::Rgba(decoded) => {
                assert_ne!(case["orientation"], 1);
                decoded
            }
            DecodedImage::HardwareBuffer(image) => {
                assert_eq!(
                    case["orientation"], 1,
                    "oriented images must normalize before reaching the renderer"
                );
                NormalizedImage::new(
                    image.width,
                    image.height,
                    read_rgba_from_ahb(&image.ahb).unwrap(),
                )
            }
            _ => panic!("unexpected compressed image"),
        };
        assert_eq!(
            AHB_ATTEMPTS.load(Ordering::Relaxed),
            before + usize::from(case["orientation"] == 1)
        );
        assert_eq!(
            (decoded.width, decoded.height),
            (
                case["width"].as_u64().unwrap() as u32,
                case["height"].as_u64().unwrap() as u32
            )
        );
        let expected = case["rgba"].as_array().unwrap();
        assert_eq!(decoded.rgba.len(), expected.len());
        for (i, (actual, expected)) in decoded.rgba.iter().zip(expected).enumerate() {
            let tolerance = if case["format"] == "JPEG" && i % 4 != 3 {
                4
            } else {
                0
            };
            assert!(
                (i64::from(*actual) - expected.as_i64().unwrap()).abs() <= tolerance,
                "{} orientation {}, byte {i}: {actual} vs {expected}",
                case["format"],
                case["orientation"]
            );
        }
    }
}

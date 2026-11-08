use base64::Engine as _;
use graphics::image_decode_ahb::decode_image_to_ahb;
use shared::{
    error::ErrorCode,
    protocol::{
        ahb::{AhbUsage, read_rgba_from_ahb},
        io_cmd::MAX_IMAGE_PIXELS,
    },
};

#[test]
fn rgb_icc_pngs_are_srgb_in_cpu_and_padded_ahb_rows() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../io/tests/fixtures/images/icc-cases.json"
    ))
    .unwrap();
    for case in fixture["cases"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|case| case["format"] == "PNG")
    {
        let encoded = base64::engine::general_purpose::STANDARD
            .decode(case["encoded"].as_str().unwrap())
            .unwrap();
        let cpu = graphics::image_decode_ahb::decode_image_rgba(&encoded).unwrap();
        let ahb = decode_image_to_ahb(&encoded).unwrap();
        assert_eq!(
            (cpu.width, cpu.height),
            (
                case["stored_width"].as_u64().unwrap() as u32,
                case["stored_height"].as_u64().unwrap() as u32
            )
        );
        assert_eq!((ahb.width, ahb.height), (cpu.width, cpu.height));
        let hardware_pixels = read_rgba_from_ahb(&ahb.ahb).unwrap();
        for pixels in [&*cpu.rgba, &hardware_pixels] {
            let expected = case["stored_rgba"].as_array().unwrap();
            assert_eq!(pixels.len(), expected.len());
            for (i, (actual, expected)) in pixels.iter().zip(expected).enumerate() {
                let tolerance = if i % 4 == 3
                    || case["profile"] == "untagged"
                    || case["profile"] == "invalid"
                {
                    0
                } else {
                    2
                };
                assert!(
                    (i64::from(*actual) - expected.as_i64().unwrap()).abs() <= tolerance,
                    "{} byte {i}: {actual} vs {expected}",
                    case["profile"]
                );
            }
        }
    }
}

fn encode_rgba_png(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    let mut encoded = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut encoded, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().expect("PNG header");
        writer.write_image_data(rgba).expect("PNG pixels");
    }
    encoded
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in bytes {
        crc ^= byte as u32;
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & (0u32.wrapping_sub(crc & 1)));
        }
    }
    !crc
}

fn push_png_chunk(out: &mut Vec<u8>, kind: &[u8; 4], payload: &[u8]) {
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(payload);
    let start = out.len() - kind.len() - payload.len();
    out.extend_from_slice(&crc32(&out[start..]).to_be_bytes());
}

fn png_header_only(width: u32, height: u32) -> Vec<u8> {
    let mut encoded = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    push_png_chunk(&mut encoded, b"IHDR", &ihdr);
    // Valid zlib stream for an empty payload. The declared image is not fully
    // decodable, but Skia can construct the codec and expose dimensions so the
    // pre-allocation pixel cap runs before any scanline decode.
    push_png_chunk(
        &mut encoded,
        b"IDAT",
        &[
            0x78, 0x01, 0x01, 0x00, 0x00, 0xff, 0xff, 0x00, 0x00, 0x00, 0x01,
        ],
    );
    push_png_chunk(&mut encoded, b"IEND", &[]);
    encoded
}

#[test]
fn decodes_straight_alpha_png_directly_into_padded_ahb_rows() {
    #[rustfmt::skip]
    let pixels = [
        240, 120,  60, 128,    0, 255,  10,  64,   10,  20, 250,   1,
          1,   2,   3,   0,  255, 255, 255, 255,   90,  80,  70, 200,
    ];
    let encoded = encode_rgba_png(3, 2, &pixels);

    let decoded = decode_image_to_ahb(&encoded).expect("AHB decode");

    assert_eq!((decoded.width, decoded.height), (3, 2));
    assert!(decoded.ahb.desc().stride_pixels > decoded.width);
    assert!(
        decoded
            .ahb
            .desc()
            .usage
            .contains(AhbUsage::GPU_SAMPLED_IMAGE | AhbUsage::CPU_WRITE_RARELY)
    );
    assert_eq!(read_rgba_from_ahb(&decoded.ahb).unwrap(), pixels);
}

#[test]
fn decodes_opaque_png_without_changing_rgba_order() {
    let pixels = [7, 31, 127, 255, 251, 19, 3, 255];
    let encoded = encode_rgba_png(2, 1, &pixels);
    let decoded = decode_image_to_ahb(&encoded).expect("AHB decode");
    assert_eq!(read_rgba_from_ahb(&decoded.ahb).unwrap(), pixels);
}

#[test]
fn rejects_empty_and_invalid_input_without_publishing_a_buffer() {
    for input in [&[][..], &b"not an image"[..]] {
        let err = decode_image_to_ahb(input).expect_err("invalid input must fail");
        assert_eq!(err.code, ErrorCode::ImageReadError);
    }
}

#[test]
fn dimension_guard_rejects_zero_and_more_than_shared_pixel_cap() {
    let zero = decode_image_to_ahb(&png_header_only(0, 1))
        .expect_err("zero-sized image must not allocate");
    assert_eq!(zero.code, ErrorCode::ImageReadError);

    let side = u32::try_from(MAX_IMAGE_PIXELS.isqrt()).unwrap() + 1;
    let err = decode_image_to_ahb(&png_header_only(side, side))
        .expect_err("pixel cap must reject before allocation");
    assert_eq!(err.code, ErrorCode::OutOfMemory);
}

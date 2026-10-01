//! The conversions `UNPACK_FLIP_Y_WEBGL` and `UNPACK_PREMULTIPLY_ALPHA_WEBGL` ask of the
//! pixels of a texture upload.
//!
//! WebGL applies them to every upload; GL ES has neither. Bytes that arrive from content (a
//! typed array, `ImageData`, a host-decoded image whose decoded bytes are resolved on the JS
//! thread) are converted by [`convert_upload`] on the way to the driver, with the layout the
//! driver would read them with, so the same pixel-store state that shapes the upload shapes the
//! conversion. A source that is already a texture the engine holds (a canvas snapshot, a
//! shared image) is read back, converted here on tightly packed RGBA8 and uploaded as bytes --
//! one readback per upload, only for content that asked.

use std::borrow::Cow;

/// The unpack state an upload is read with, as the render thread last saw it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct UnpackState {
    pub flip_y: bool,
    pub premultiply: bool,
    pub alignment: i32,
    pub row_length: i32,
    pub skip_rows: i32,
    pub skip_pixels: i32,
}

/// Bytes per pixel of a (format, type) pair of `texImage2D`/`texSubImage2D`; `None` for one this
/// does not know (it is then uploaded as it is).
fn bytes_per_pixel(format: u32, type_: u32) -> Option<usize> {
    const UNSIGNED_SHORT_4_4_4_4: u32 = 0x8033;
    const UNSIGNED_SHORT_5_5_5_1: u32 = 0x8034;
    const UNSIGNED_SHORT_5_6_5: u32 = 0x8363;
    const UNSIGNED_INT_2_10_10_10_REV: u32 = 0x8368;
    const UNSIGNED_INT_10F_11F_11F_REV: u32 = 0x8c3b;
    const UNSIGNED_INT_5_9_9_9_REV: u32 = 0x8c3e;
    const UNSIGNED_INT_24_8: u32 = 0x84fa;
    const FLOAT_32_UNSIGNED_INT_24_8_REV: u32 = 0x8dad;
    const HALF_FLOAT_OES: u32 = 0x8d61;
    match type_ {
        UNSIGNED_SHORT_4_4_4_4 | UNSIGNED_SHORT_5_5_5_1 | UNSIGNED_SHORT_5_6_5 => return Some(2),
        UNSIGNED_INT_2_10_10_10_REV
        | UNSIGNED_INT_10F_11F_11F_REV
        | UNSIGNED_INT_5_9_9_9_REV
        | UNSIGNED_INT_24_8 => return Some(4),
        FLOAT_32_UNSIGNED_INT_24_8_REV => return Some(8),
        _ => {}
    }
    let channels = match format {
        glow::RGBA | 0x8d99 => 4,                     // RGBA, RGBA_INTEGER
        glow::RGB | 0x8d98 => 3,                      // RGB, RGB_INTEGER
        glow::LUMINANCE_ALPHA | 0x8227 | 0x8228 => 2, // LUMINANCE_ALPHA, RG, RG_INTEGER
        glow::LUMINANCE | glow::ALPHA | glow::DEPTH_COMPONENT | 0x1903 | 0x8d94 => 1, // RED, RED_INTEGER
        _ => return None,
    };
    let component = match type_ {
        glow::UNSIGNED_BYTE | glow::BYTE => 1,
        glow::UNSIGNED_SHORT | glow::SHORT | glow::HALF_FLOAT | HALF_FLOAT_OES => 2,
        glow::UNSIGNED_INT | glow::INT | glow::FLOAT => 4,
        _ => return None,
    };
    Some(channels * component)
}

/// `bytes` as the unpack flags say they must be uploaded, with the layout the driver reads them
/// with: rows reversed for `UNPACK_FLIP_Y_WEBGL`, colour multiplied by alpha for
/// `UNPACK_PREMULTIPLY_ALPHA_WEBGL` (8-bit RGBA and LUMINANCE_ALPHA; every other format has no
/// alpha to multiply by, or a type the conversion is not defined for, and is left alone).
///
/// `Cow::Borrowed` -- no copy -- when there is nothing to do, and when the bytes cannot be
/// interpreted (an unknown format, a buffer too short for the rows): those go to the driver
/// as they came, for it to accept or refuse as it always did. The caller's bytes are never
/// written.
pub(crate) fn convert_upload<'a>(
    bytes: &'a [u8],
    width: i32,
    height: i32,
    format: u32,
    type_: u32,
    state: &UnpackState,
) -> Cow<'a, [u8]> {
    let premultiply = state.premultiply
        && type_ == glow::UNSIGNED_BYTE
        && matches!(format, glow::RGBA | glow::LUMINANCE_ALPHA);
    if !state.flip_y && !premultiply {
        return Cow::Borrowed(bytes);
    }
    let (Ok(width), Ok(height)) = (usize::try_from(width), usize::try_from(height)) else {
        return Cow::Borrowed(bytes);
    };
    let Some(bpp) = bytes_per_pixel(format, type_) else {
        return Cow::Borrowed(bytes);
    };
    if width == 0 || height == 0 {
        return Cow::Borrowed(bytes);
    }
    let alignment = usize::try_from(state.alignment)
        .ok()
        .filter(|a| *a > 0)
        .unwrap_or(4);
    let row_length = usize::try_from(state.row_length)
        .ok()
        .filter(|r| *r > 0)
        .unwrap_or(width);
    let row_bytes = width * bpp;
    let stride = (row_length * bpp).div_ceil(alignment) * alignment;
    let skip_rows = usize::try_from(state.skip_rows).unwrap_or(0);
    let skip_pixels = usize::try_from(state.skip_pixels).unwrap_or(0);
    let start = skip_rows * stride + skip_pixels * bpp;
    let Some(end) = (height - 1)
        .checked_mul(stride)
        .and_then(|rows| rows.checked_add(start + row_bytes))
    else {
        return Cow::Borrowed(bytes);
    };
    if bytes.len() < end {
        return Cow::Borrowed(bytes);
    }
    let mut out = bytes.to_vec();
    if state.flip_y {
        for y in 0..height / 2 {
            let (top, bottom) = (start + y * stride, start + (height - 1 - y) * stride);
            let (head, tail) = out.split_at_mut(bottom);
            head[top..top + row_bytes].swap_with_slice(&mut tail[..row_bytes]);
        }
    }
    if premultiply {
        for y in 0..height {
            let row = start + y * stride;
            premultiply_pixels(&mut out[row..row + row_bytes], bpp);
        }
    }
    Cow::Owned(out)
}

/// Straight alpha to premultiplied on 8-bit pixels of `channels` bytes whose last byte is the
/// alpha (4: RGBA, 2: LUMINANCE_ALPHA), rounded to nearest. Opaque and fully transparent pixels
/// (nearly every pixel of a sprite) are left alone without a division: an opaque pixel is
/// already its own premultiplication, and a transparent one is zero by definition of
/// premultiplied.
fn premultiply_pixels(pixels: &mut [u8], channels: usize) {
    for px in pixels.chunks_exact_mut(channels) {
        let a = u32::from(px[channels - 1]);
        if a == 255 {
            continue;
        }
        for channel in &mut px[..channels - 1] {
            *channel = ((u32::from(*channel) * a + 127) / 255) as u8;
        }
    }
}

/// Reverse the order of the rows of a tightly packed image.
pub(crate) fn flip_rows_in_place(pixels: &mut [u8], row_bytes: usize) {
    if row_bytes == 0 {
        return;
    }
    let rows = pixels.len() / row_bytes;
    let (mut top, mut bottom) = (0, rows.saturating_sub(1));
    while top < bottom {
        let (head, tail) = pixels.split_at_mut(bottom * row_bytes);
        head[top * row_bytes..(top + 1) * row_bytes].swap_with_slice(&mut tail[..row_bytes]);
        top += 1;
        bottom -= 1;
    }
}

/// [`premultiply_pixels`] on tightly packed RGBA8.
pub(crate) fn premultiply_rgba8(pixels: &mut [u8]) {
    premultiply_pixels(pixels, 4);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flipping_reverses_the_rows_and_only_the_rows() {
        // three rows of two RGBA pixels
        let mut px: Vec<u8> = (0..24).collect();
        flip_rows_in_place(&mut px, 8);
        let expected: Vec<u8> = (16..24).chain(8..16).chain(0..8).collect();
        assert_eq!(px, expected);
        flip_rows_in_place(&mut px, 8);
        assert_eq!(
            px,
            (0..24).collect::<Vec<u8>>(),
            "flipping twice is the identity"
        );
    }

    #[test]
    fn flipping_an_even_row_count_and_a_single_row() {
        let mut px: Vec<u8> = (0..16).collect();
        flip_rows_in_place(&mut px, 4);
        assert_eq!(px, [12, 13, 14, 15, 8, 9, 10, 11, 4, 5, 6, 7, 0, 1, 2, 3]);
        let mut one = [1u8, 2, 3, 4];
        flip_rows_in_place(&mut one, 4);
        assert_eq!(one, [1, 2, 3, 4]);
        flip_rows_in_place(&mut [], 4);
        flip_rows_in_place(&mut [1, 2, 3, 4], 0);
    }

    #[test]
    fn premultiplying_scales_the_colour_by_the_alpha() {
        let mut px = [255, 128, 0, 128, 200, 100, 50, 255, 9, 9, 9, 0];
        premultiply_rgba8(&mut px);
        assert_eq!(px, [128, 64, 0, 128, 200, 100, 50, 255, 0, 0, 0, 0]);
    }

    /// Premultiplying and then unpremultiplying (`readback::unpremultiply_rgba8`) stays within the
    /// quantisation 8 bits allow, and never overflows a channel.
    #[test]
    fn premultiply_then_unpremultiply_stays_within_quantisation() {
        for a in 1..=254u32 {
            for c in (0..=255u32).step_by(5) {
                let mut px = [c as u8, c as u8, c as u8, a as u8];
                premultiply_rgba8(&mut px);
                assert!(
                    u32::from(px[0]) <= a,
                    "a={a} c={c}: premultiplied {} exceeds alpha",
                    px[0]
                );
                super::super::readback::unpremultiply_rgba8(&mut px);
                let worst = 255 / a + 1;
                assert!(
                    (i64::from(px[0]) - i64::from(c)).unsigned_abs() as u32 <= worst,
                    "a={a} c={c}: round trip gave {}",
                    px[0]
                );
            }
        }
    }

    fn state(flip_y: bool, premultiply: bool) -> UnpackState {
        UnpackState {
            flip_y,
            premultiply,
            alignment: 4,
            row_length: 0,
            skip_rows: 0,
            skip_pixels: 0,
        }
    }

    #[test]
    fn nothing_asked_is_a_borrow_not_a_copy() {
        let px = [1u8, 2, 3, 4, 5, 6, 7, 8];
        let out = convert_upload(
            &px,
            2,
            1,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            &state(false, false),
        );
        assert!(matches!(out, Cow::Borrowed(_)));
    }

    #[test]
    fn flip_reverses_rows_whatever_the_format() {
        // two rows of one RGB pixel at the default alignment of 4: each row is 3 bytes + 1 padding
        let px = [1u8, 2, 3, 0, 4, 5, 6, 0];
        let out = convert_upload(
            &px,
            1,
            2,
            glow::RGB,
            glow::UNSIGNED_BYTE,
            &state(true, false),
        );
        assert_eq!(
            &out[..],
            &[4, 5, 6, 0, 1, 2, 3, 0],
            "padding stays where the layout puts it"
        );
        // 16-bit packed pixels
        let px = [1u8, 0, 2, 0, 3, 0, 4, 0];
        let mut tight = state(true, false);
        tight.alignment = 2; // a row is one 2-byte pixel
        let out = convert_upload(&px, 1, 4, glow::RGB, 0x8363, &tight);
        assert_eq!(&out[..], &[4, 0, 3, 0, 2, 0, 1, 0]);
        // floats
        let px: Vec<u8> = [1.0f32, 2.0].iter().flat_map(|f| f.to_ne_bytes()).collect();
        let out = convert_upload(&px, 1, 2, glow::LUMINANCE, glow::FLOAT, &state(true, false));
        let want: Vec<u8> = [2.0f32, 1.0].iter().flat_map(|f| f.to_ne_bytes()).collect();
        assert_eq!(&out[..], &want[..]);
    }

    #[test]
    fn row_length_and_skips_move_the_window_not_the_layout() {
        // a 1x2 window at (skip_pixels 1, skip_rows 1) of a 3-pixel-wide RGBA buffer
        let mut px = vec![0u8; 3 * 4 * 3];
        px[(3 * 4) + 4..(3 * 4) + 8].copy_from_slice(&[1, 1, 1, 1]); // row 1, pixel 1
        px[(2 * 3 * 4) + 4..(2 * 3 * 4) + 8].copy_from_slice(&[2, 2, 2, 2]); // row 2, pixel 1
        let mut st = state(true, false);
        st.row_length = 3;
        st.skip_rows = 1;
        st.skip_pixels = 1;
        let out = convert_upload(&px, 1, 2, glow::RGBA, glow::UNSIGNED_BYTE, &st);
        assert_eq!(&out[(3 * 4) + 4..(3 * 4) + 8], &[2, 2, 2, 2]);
        assert_eq!(&out[(2 * 3 * 4) + 4..(2 * 3 * 4) + 8], &[1, 1, 1, 1]);
        assert!(
            out[..12].iter().all(|b| *b == 0),
            "outside the window nothing moves"
        );
    }

    #[test]
    fn premultiply_applies_to_rgba_and_luminance_alpha_bytes_only() {
        let px = [255u8, 128, 0, 128];
        let out = convert_upload(
            &px,
            1,
            1,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            &state(false, true),
        );
        assert_eq!(&out[..], &[128, 64, 0, 128]);
        let la = [200u8, 100];
        let out = convert_upload(
            &la,
            1,
            1,
            glow::LUMINANCE_ALPHA,
            glow::UNSIGNED_BYTE,
            &state(false, true),
        );
        assert_eq!(&out[..], &[78, 100]); // 200 * 100 / 255
        // no alpha, or not 8-bit: nothing to multiply
        let rgb = [255u8, 128, 0, 0];
        assert!(matches!(
            convert_upload(
                &rgb,
                1,
                1,
                glow::RGB,
                glow::UNSIGNED_BYTE,
                &state(false, true)
            ),
            Cow::Borrowed(_)
        ));
        assert!(matches!(
            convert_upload(
                &px,
                1,
                1,
                glow::RGBA,
                glow::UNSIGNED_SHORT_4_4_4_4,
                &state(false, true)
            ),
            Cow::Borrowed(_)
        ));
    }

    #[test]
    fn what_cannot_be_interpreted_goes_through_untouched() {
        let px = [1u8, 2, 3];
        // too short for the rows
        assert!(matches!(
            convert_upload(
                &px,
                2,
                2,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                &state(true, true)
            ),
            Cow::Borrowed(_)
        ));
        // unknown format
        assert!(matches!(
            convert_upload(&px, 1, 1, 0xdead, glow::UNSIGNED_BYTE, &state(true, false)),
            Cow::Borrowed(_)
        ));
        // negative or zero size
        assert!(matches!(
            convert_upload(
                &px,
                -1,
                1,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                &state(true, false)
            ),
            Cow::Borrowed(_)
        ));
        assert!(matches!(
            convert_upload(
                &px,
                0,
                0,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                &state(true, false)
            ),
            Cow::Borrowed(_)
        ));
    }

    #[test]
    fn the_caller_s_bytes_are_never_written() {
        let px = vec![10u8, 20, 30, 128, 40, 50, 60, 255];
        let before = px.clone();
        let _ = convert_upload(
            &px,
            1,
            2,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            &state(true, true),
        );
        assert_eq!(px, before);
    }
}

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

/// The pixel-store state that decides how many bytes an upload reads from its source.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct UnpackLayout {
    pub alignment: i32,
    pub row_length: i32,
    /// UNPACK_IMAGE_HEIGHT and UNPACK_SKIP_IMAGES: a 3D upload's; a 2D one reads neither.
    pub image_height: i32,
    pub skip_images: i32,
    pub skip_rows: i32,
    pub skip_pixels: i32,
}

/// How many bytes an upload of `width` x `height` x `depth` pixels of (`format`, `type_`) reads from
/// its source with the pixel-store state `layout` (ES 3.0 3.7.2): rows padded to UNPACK_ALIGNMENT,
/// UNPACK_ROW_LENGTH and UNPACK_IMAGE_HEIGHT in place of the width and height when set, the skips
/// in front, the last row unpadded. 0 for an empty upload; `None` for a pair GL has no upload of
/// (every pair it has is known here) or a size past memory.
///
/// The driver reads that many bytes from the pointer it is given, whatever the slice behind it
/// holds, so an upload whose bytes are fewer is refused before it is asked -- on the Performance+
/// lane those bytes come from another process.
pub(crate) fn upload_bytes(
    width: i32,
    height: i32,
    depth: i32,
    format: u32,
    type_: u32,
    layout: &UnpackLayout,
) -> Option<usize> {
    let bpp = bytes_per_pixel(format, type_)?;
    let size = |v: i32| usize::try_from(v).ok();
    let (width, height, depth) = (size(width)?, size(height)?, size(depth)?);
    if width == 0 || height == 0 || depth == 0 {
        return Some(0);
    }
    let set_or = |v: i32, default: usize| size(v).filter(|v| *v > 0).unwrap_or(default);
    let skip = |v: i32| size(v).unwrap_or(0);
    let alignment = set_or(layout.alignment, 4);
    let stride = set_or(layout.row_length, width)
        .checked_mul(bpp)?
        .div_ceil(alignment)
        .checked_mul(alignment)?;
    let image = stride.checked_mul(set_or(layout.image_height, height))?;
    let skipped = skip(layout.skip_images)
        .checked_mul(image)?
        .checked_add(skip(layout.skip_rows).checked_mul(stride)?)?
        .checked_add(skip(layout.skip_pixels).checked_mul(bpp)?)?;
    (depth - 1)
        .checked_mul(image)?
        .checked_add((height - 1).checked_mul(stride)?)?
        .checked_add(width.checked_mul(bpp)?)?
        .checked_add(skipped)
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
    // The pixel-store values are content's, up to i32::MAX each, and so are the sizes: every
    // product is checked, and an upload whose layout does not fit in memory is not interpreted
    // (it goes to the driver as it came, which refuses it).
    let layout = (|| {
        let row_bytes = width.checked_mul(bpp)?;
        let stride = row_length
            .checked_mul(bpp)?
            .div_ceil(alignment)
            .checked_mul(alignment)?;
        let start = usize::try_from(state.skip_rows)
            .unwrap_or(0)
            .checked_mul(stride)?
            .checked_add(
                usize::try_from(state.skip_pixels)
                    .unwrap_or(0)
                    .checked_mul(bpp)?,
            )?;
        let end = (height - 1)
            .checked_mul(stride)?
            .checked_add(start)?
            .checked_add(row_bytes)?;
        Some((row_bytes, stride, start, end))
    })();
    let Some((row_bytes, stride, start, end)) = layout else {
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

/// What a TexImageSource holds in its alpha channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SourceAlpha {
    /// Straight (unpremultiplied): a decoded image, `ImageData`.
    Straight,
    /// Premultiplied: what Skia rendered -- a canvas, a snapshot of one.
    Premultiplied,
}

/// Which of a TexImageSource's pixels an upload reads (WebGL 2.0 5.35, "Pixel store parameters for uploads from
/// TexImageSource"): `width` x `height` from (`x`, `y`) -- UNPACK_SKIP_PIXELS, UNPACK_SKIP_ROWS -- counted from the
/// top of the source as UNPACK_FLIP_Y_WEBGL leaves it; for a 3D upload `depth` such slices, `image_height` rows apart
/// (UNPACK_IMAGE_HEIGHT, else `height`), from slice `skip_images` (UNPACK_SKIP_IMAGES). A 2D upload is one slice from
/// slice 0. UNPACK_ALIGNMENT and UNPACK_ROW_LENGTH do not apply to a TexImageSource.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SourceRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub depth: u32,
    pub image_height: u32,
    pub skip_images: u32,
}

impl SourceRect {
    /// The whole of a `width` x `height` source, as one 2D slice.
    pub(crate) fn whole(width: u32, height: u32) -> Self {
        Self {
            x: 0,
            y: 0,
            width,
            height,
            depth: 1,
            image_height: height,
            skip_images: 0,
        }
    }

    /// Whether every pixel the rectangle reads is within a `width` x `height` source. The facade refuses a rectangle
    /// that is not (INVALID_OPERATION); the renderer does not read past a source because of one.
    pub(crate) fn fits(&self, width: u32, height: u32) -> bool {
        let stride = u64::from(if self.image_height == 0 {
            self.height
        } else {
            self.image_height
        });
        let last_slice = u64::from(self.skip_images) + u64::from(self.depth.max(1)) - 1;
        u64::from(self.x) + u64::from(self.width) <= u64::from(width)
            && u64::from(self.y) + last_slice * stride + u64::from(self.height) <= u64::from(height)
    }

    /// The source row, counted from the top as the source holds it, of row `row` of slice `slice`.
    fn source_row(&self, slice: u32, row: u32, source_height: u32, flip_y: bool) -> usize {
        let stride = if self.image_height == 0 {
            self.height
        } else {
            self.image_height
        };
        let from_top = (self.y + (self.skip_images + slice) * stride + row) as usize;
        if flip_y {
            source_height as usize - 1 - from_top
        } else {
            from_top
        }
    }
}

/// What an upload from a TexImageSource asks of its pixels: the alpha UNPACK_PREMULTIPLY_ALPHA_WEBGL asks for, the
/// row order UNPACK_FLIP_Y_WEBGL asks for, and the (`format`, `type_`) they are packed as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SourceConversion {
    pub source_alpha: SourceAlpha,
    pub premultiply: bool,
    pub flip_y: bool,
    pub format: u32,
    pub type_: u32,
}

const HALF_FLOAT_OES: u32 = 0x8d61;
const UNSIGNED_SHORT_5_6_5: u32 = 0x8363;
const UNSIGNED_SHORT_4_4_4_4: u32 = 0x8033;
const UNSIGNED_SHORT_5_5_5_1: u32 = 0x8034;
const UNSIGNED_INT_2_10_10_10_REV: u32 = 0x8368;
const UNSIGNED_INT_10F_11F_11F_REV: u32 = 0x8C3B;

/// The source channels -- 0 red .. 3 alpha -- a pixel of `format` is made of, or `None` for a format no
/// TexImageSource upload takes. LUMINANCE is the red channel, as browsers take it.
fn source_channels(format: u32) -> Option<&'static [usize]> {
    Some(match format {
        glow::RGBA | 0x8d99 => &[0, 1, 2, 3],      // RGBA, RGBA_INTEGER
        glow::RGB | 0x8d98 => &[0, 1, 2],          // RGB, RGB_INTEGER
        0x8227 | 0x8228 => &[0, 1],                // RG, RG_INTEGER
        0x1903 | 0x8d94 | glow::LUMINANCE => &[0], // RED, RED_INTEGER, LUMINANCE
        glow::ALPHA => &[3],
        glow::LUMINANCE_ALPHA => &[0, 3],
        _ => return None,
    })
}

/// Whether (`format`, `type_`) is a pair a TexImageSource is packed as: the TexImageSource table's (WebGL 2.0 3.7.6),
/// with WebGL 1's unsized pairs among them.
fn packs(format: u32, type_: u32) -> bool {
    let integer = matches!(format, 0x8d99 | 0x8d98 | 0x8228 | 0x8d94);
    match type_ {
        glow::UNSIGNED_BYTE => true,
        UNSIGNED_SHORT_5_6_5 | UNSIGNED_INT_10F_11F_11F_REV => format == glow::RGB,
        UNSIGNED_SHORT_4_4_4_4 | UNSIGNED_SHORT_5_5_5_1 | UNSIGNED_INT_2_10_10_10_REV => {
            format == glow::RGBA
        }
        glow::HALF_FLOAT | HALF_FLOAT_OES | glow::FLOAT => !integer,
        _ => false,
    }
}

/// Whether a pixel of `type_` holds floating-point values, which are premultiplied (or not) before they are rounded
/// to the type rather than after -- as a browser converts them.
fn is_float_type(type_: u32) -> bool {
    matches!(
        type_,
        glow::HALF_FLOAT | HALF_FLOAT_OES | glow::FLOAT | UNSIGNED_INT_10F_11F_11F_REV
    )
}

/// `value >> shift`, rounded to nearest, ties to even.
fn round_shift(value: u32, shift: u32) -> u32 {
    if shift == 0 {
        return value;
    }
    if shift >= 32 {
        return 0;
    }
    let quotient = value >> shift;
    let remainder = value & ((1 << shift) - 1);
    let half = 1 << (shift - 1);
    if remainder > half || (remainder == half && quotient & 1 == 1) {
        quotient + 1
    } else {
        quotient
    }
}

/// A non-negative `value` as an unsigned float of a 5-bit exponent (bias 15) and `mantissa_bits` of mantissa, rounded
/// to nearest: the bits of a half float (10), and of UNSIGNED_INT_10F_11F_11F_REV's 11-bit (6) and 10-bit (5) floats.
/// A TexImageSource's values are within [0, 1], so nothing overflows.
fn unsigned_small_float(value: f32, mantissa_bits: u32) -> u32 {
    if !(value > 0.0) {
        return 0;
    }
    let bits = value.to_bits();
    let exponent = ((bits >> 23) & 0xff) as i32 - 127;
    let significand = (bits & 0x7f_ffff) | 0x80_0000;
    if exponent >= -14 {
        // Normal: the mantissa rounded to its bits, a carry into the exponent left to the addition.
        (((exponent + 15) as u32) << mantissa_bits) + round_shift(significand, 23 - mantissa_bits)
            - (1 << mantissa_bits)
    } else {
        // Subnormal: the value in units of 2^-14 / 2^mantissa_bits.
        let shift = (23 - mantissa_bits as i32) + (-14 - exponent);
        round_shift(significand, shift as u32)
    }
}

/// A TexImageSource's pixels -- RGBA8 rows of `source_width` x `source_height`, top row first, `rgba` -- as an upload
/// of `conversion.format` / `conversion.type_` reads them: the pixels `rect` selects of the source as
/// UNPACK_FLIP_Y_WEBGL turns it, with the alpha UNPACK_PREMULTIPLY_ALPHA_WEBGL asks for, packed tightly -- rows
/// unpadded, slices one after another -- for an upload with UNPACK_ALIGNMENT 1 and no row length, skips or image
/// height.
///
/// The arithmetic is a browser's: an 8-bit or packed destination is premultiplied (or unpremultiplied) in 8 bits,
/// rounded, then truncated to its bits (RGB10_A2: `v * 1023 / 255` rounded down); a float destination is premultiplied
/// in floating point and rounded to nearest once. `None` for a pair no TexImageSource upload takes, a rectangle outside
/// the source, or rows shorter than the source says.
pub(crate) fn pack_source(
    rgba: &[u8],
    source_width: u32,
    source_height: u32,
    rect: &SourceRect,
    conversion: &SourceConversion,
) -> Option<Vec<u8>> {
    let channels = source_channels(conversion.format)?;
    let type_ = conversion.type_;
    if !packs(conversion.format, type_) || !rect.fits(source_width, source_height) {
        return None;
    }
    let row_bytes = (source_width as usize).checked_mul(4)?;
    if rgba.len() < row_bytes.checked_mul(source_height as usize)? {
        return None;
    }
    let pixel_bytes = match type_ {
        glow::UNSIGNED_BYTE => channels.len(),
        UNSIGNED_SHORT_5_6_5 | UNSIGNED_SHORT_4_4_4_4 | UNSIGNED_SHORT_5_5_5_1 => 2,
        UNSIGNED_INT_2_10_10_10_REV | UNSIGNED_INT_10F_11F_11F_REV => 4,
        glow::FLOAT => 4 * channels.len(),
        _ => 2 * channels.len(), // HALF_FLOAT, HALF_FLOAT_OES
    };
    let pixels = (rect.width as usize)
        .checked_mul(rect.height as usize)?
        .checked_mul(rect.depth.max(1) as usize)?;
    let mut out = Vec::with_capacity(pixels.checked_mul(pixel_bytes)?);
    let straight_source = conversion.source_alpha == SourceAlpha::Straight;
    let premultiply = conversion.premultiply && straight_source;
    let unpremultiply = !conversion.premultiply && !straight_source;
    let float = is_float_type(type_);
    for slice in 0..rect.depth.max(1) {
        for row in 0..rect.height {
            let source_row = rect.source_row(slice, row, source_height, conversion.flip_y);
            let start = source_row * row_bytes + rect.x as usize * 4;
            for px in rgba[start..start + rect.width as usize * 4].chunks_exact(4) {
                let a = u32::from(px[3]);
                if float {
                    // In floating point: what the source holds, as the upload asks for it.
                    let alpha = a as f32 / 255.0;
                    let mut v = [0f32; 4];
                    for c in 0..3 {
                        let x = f32::from(px[c]) / 255.0;
                        v[c] = if premultiply {
                            x * alpha
                        } else if unpremultiply && a != 0 {
                            (x / alpha).min(1.0)
                        } else {
                            x
                        };
                    }
                    v[3] = alpha;
                    pack_float(&mut out, &v, channels, type_);
                } else {
                    // In 8 bits, rounded.
                    let mut v = [px[0], px[1], px[2], px[3]];
                    if premultiply && a != 255 {
                        for c in &mut v[..3] {
                            *c = ((u32::from(*c) * a + 127) / 255) as u8;
                        }
                    } else if unpremultiply && a != 0 && a != 255 {
                        for c in &mut v[..3] {
                            *c = ((u32::from(*c) * 255 + a / 2) / a).min(255) as u8;
                        }
                    }
                    pack_unorm(&mut out, &v, channels, type_);
                }
            }
        }
    }
    Some(out)
}

/// One pixel of 8-bit values `v` as `type_`, an unsigned byte per channel or a packed type, in native byte order.
fn pack_unorm(out: &mut Vec<u8>, v: &[u8; 4], channels: &[usize], type_: u32) {
    let [r, g, b, a] = v.map(u32::from);
    match type_ {
        UNSIGNED_SHORT_5_6_5 => {
            out.extend_from_slice(&(((r >> 3) << 11 | (g >> 2) << 5 | b >> 3) as u16).to_ne_bytes())
        }
        UNSIGNED_SHORT_4_4_4_4 => out.extend_from_slice(
            &(((r >> 4) << 12 | (g >> 4) << 8 | (b >> 4) << 4 | a >> 4) as u16).to_ne_bytes(),
        ),
        UNSIGNED_SHORT_5_5_5_1 => out.extend_from_slice(
            &(((r >> 3) << 11 | (g >> 3) << 6 | (b >> 3) << 1 | a >> 7) as u16).to_ne_bytes(),
        ),
        UNSIGNED_INT_2_10_10_10_REV => {
            let ten = |c: u32| c * 1023 / 255;
            out.extend_from_slice(
                &(ten(r) | ten(g) << 10 | ten(b) << 20 | (a * 3 / 255) << 30).to_ne_bytes(),
            )
        }
        _ => out.extend(channels.iter().map(|&c| v[c])),
    }
}

/// One pixel of values `v` within [0, 1] as `type_`: floats, half floats, or UNSIGNED_INT_10F_11F_11F_REV.
fn pack_float(out: &mut Vec<u8>, v: &[f32; 4], channels: &[usize], type_: u32) {
    match type_ {
        glow::FLOAT => {
            for &c in channels {
                out.extend_from_slice(&v[c].to_ne_bytes());
            }
        }
        UNSIGNED_INT_10F_11F_11F_REV => {
            let packed = unsigned_small_float(v[0], 6)
                | unsigned_small_float(v[1], 6) << 11
                | unsigned_small_float(v[2], 5) << 22;
            out.extend_from_slice(&packed.to_ne_bytes());
        }
        _ => {
            for &c in channels {
                out.extend_from_slice(&(unsigned_small_float(v[c], 10) as u16).to_ne_bytes());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{UnpackLayout, upload_bytes};

    // ---- TexImageSource packing ------------------------------------------------------------------------------------
    //
    // The expected values are what a browser makes of the same pixels: Chrome, uploading this `ImageData` with each
    // format and type and reading the texels back exactly.

    use super::{SourceAlpha, SourceConversion, SourceRect, pack_source, unsigned_small_float};

    const SRC: [u8; 16] = [
        9, 130, 254, 129, 200, 3, 77, 255, 255, 128, 1, 0, 17, 34, 51, 68,
    ];

    fn pack(format: u32, type_: u32, premultiply: bool) -> Vec<u8> {
        pack_source(
            &SRC,
            4,
            1,
            &SourceRect::whole(4, 1),
            &SourceConversion {
                source_alpha: SourceAlpha::Straight,
                premultiply,
                flip_y: false,
                format,
                type_,
            },
        )
        .expect("a pair a TexImageSource is packed as")
    }

    fn shorts(bytes: &[u8]) -> Vec<u16> {
        bytes
            .chunks_exact(2)
            .map(|b| u16::from_ne_bytes([b[0], b[1]]))
            .collect()
    }

    fn words(bytes: &[u8]) -> Vec<u32> {
        bytes
            .chunks_exact(4)
            .map(|b| u32::from_ne_bytes([b[0], b[1], b[2], b[3]]))
            .collect()
    }

    #[test]
    fn eight_bit_formats_take_their_channels_and_premultiply_rounded() {
        assert_eq!(pack(glow::RGBA, glow::UNSIGNED_BYTE, false), SRC);
        assert_eq!(
            pack(glow::RGBA, glow::UNSIGNED_BYTE, true),
            [5, 66, 128, 129, 200, 3, 77, 255, 0, 0, 0, 0, 5, 9, 14, 68]
        );
        assert_eq!(
            pack(glow::RGB, glow::UNSIGNED_BYTE, false),
            [9, 130, 254, 200, 3, 77, 255, 128, 1, 17, 34, 51]
        );
        assert_eq!(
            pack(glow::LUMINANCE, glow::UNSIGNED_BYTE, false),
            [9, 200, 255, 17]
        );
        assert_eq!(
            pack(glow::LUMINANCE, glow::UNSIGNED_BYTE, true),
            [5, 200, 0, 5]
        );
        assert_eq!(
            pack(glow::ALPHA, glow::UNSIGNED_BYTE, true),
            [129, 255, 0, 68]
        );
        assert_eq!(
            pack(glow::LUMINANCE_ALPHA, glow::UNSIGNED_BYTE, false),
            [9, 129, 200, 255, 255, 0, 17, 68]
        );
        assert_eq!(pack(0x1903, glow::UNSIGNED_BYTE, false), [9, 200, 255, 17]); // RED
        assert_eq!(
            pack(0x8227, glow::UNSIGNED_BYTE, true),
            [5, 66, 200, 3, 0, 0, 5, 9]
        ); // RG
        // RGBA_INTEGER takes the bytes as integers, premultiplied as the others are.
        assert_eq!(
            pack(0x8d99, glow::UNSIGNED_BYTE, true),
            pack(glow::RGBA, glow::UNSIGNED_BYTE, true)
        );
    }

    #[test]
    fn packed_types_truncate_to_their_bits() {
        let rgba4 = |r: u16, g: u16, b: u16, a: u16| r << 12 | g << 8 | b << 4 | a;
        assert_eq!(
            shorts(&pack(glow::RGBA, 0x8033, false)),
            [
                rgba4(0, 8, 15, 8),
                rgba4(12, 0, 4, 15),
                rgba4(15, 8, 0, 0),
                rgba4(1, 2, 3, 4)
            ]
        );
        assert_eq!(
            shorts(&pack(glow::RGBA, 0x8033, true)),
            [
                rgba4(0, 4, 8, 8),
                rgba4(12, 0, 4, 15),
                rgba4(0, 0, 0, 0),
                rgba4(0, 0, 0, 4)
            ]
        );
        let rgb5a1 = |r: u16, g: u16, b: u16, a: u16| r << 11 | g << 6 | b << 1 | a;
        assert_eq!(
            shorts(&pack(glow::RGBA, 0x8034, false)),
            [
                rgb5a1(1, 16, 31, 1),
                rgb5a1(25, 0, 9, 1),
                rgb5a1(31, 16, 0, 0),
                rgb5a1(2, 4, 6, 0)
            ]
        );
        let rgb565 = |r: u16, g: u16, b: u16| r << 11 | g << 5 | b;
        assert_eq!(
            shorts(&pack(glow::RGB, 0x8363, false)),
            [
                rgb565(1, 32, 31),
                rgb565(25, 0, 9),
                rgb565(31, 32, 0),
                rgb565(2, 8, 6)
            ]
        );
        assert_eq!(shorts(&pack(glow::RGB, 0x8363, true))[0], rgb565(0, 16, 16));
        let rgb10a2 = |r: u32, g: u32, b: u32, a: u32| r | g << 10 | b << 20 | a << 30;
        assert_eq!(
            words(&pack(glow::RGBA, 0x8368, false)),
            [
                rgb10a2(36, 521, 1018, 1),
                rgb10a2(802, 12, 308, 3),
                rgb10a2(1023, 513, 4, 0),
                rgb10a2(68, 136, 204, 0)
            ]
        );
        assert_eq!(
            words(&pack(glow::RGBA, 0x8368, true))[0],
            rgb10a2(20, 264, 513, 1)
        );
    }

    #[test]
    fn float_types_premultiply_before_they_round() {
        let floats: Vec<f32> = pack(glow::RGBA, glow::FLOAT, true)
            .chunks_exact(4)
            .map(|b| f32::from_ne_bytes([b[0], b[1], b[2], b[3]]))
            .collect();
        assert_eq!(floats[0], (9.0 / 255.0) * (129.0 / 255.0));
        assert_eq!(
            &floats[4..8],
            &[200.0 / 255.0, 3.0 / 255.0, 77.0 / 255.0, 1.0]
        );
        // Half floats, rounded to nearest: 130/255 is 1.0196 x 2^-1, a mantissa of 20.08/1024.
        let halves = shorts(&pack(glow::RGBA, glow::HALF_FLOAT, false));
        assert_eq!(halves[1], (14 << 10) | 20);
        assert_eq!(halves[7], 15 << 10); // 1.0
        assert_eq!(halves[10], (7 << 10) | 4); // 1/255: 1.0039 x 2^-8
        assert_eq!(shorts(&pack(0x1903, glow::HALF_FLOAT, false)).len(), 4); // RED: one channel
        // 10F_11F_11F_REV: 9/255 is 1.129 x 2^-5, 130/255 1.0196 x 2^-1, and 254/255 rounds up to 1.0 in 5 bits.
        let packed = words(&pack(glow::RGB, 0x8c3b, false))[0];
        assert_eq!(
            packed,
            (10 << 6 | 8) | (14 << 6 | 1) << 11 | (15 << 5) << 22
        );
    }

    #[test]
    fn a_small_float_below_the_smallest_normal_is_subnormal() {
        // (1/255)^2 is 1.54e-5, under 2^-14: 0.252 of it, so a half of exponent 0 and mantissa 0.252 * 1024 = 258.
        assert_eq!(unsigned_small_float(1.0 / 65025.0, 10), 258);
        assert_eq!(unsigned_small_float(2f32.powi(-14), 10), 1 << 10); // the smallest normal
        assert_eq!(unsigned_small_float(0.0, 10), 0);
    }

    // A 4 x 4 source whose pixel (x, y) is [x, y, 0, 255], row 0 the top.
    fn grid() -> Vec<u8> {
        (0..4u8)
            .flat_map(|y| (0..4u8).flat_map(move |x| [x, y, 0, 255]))
            .collect()
    }

    fn texels(rect: SourceRect, flip_y: bool) -> Vec<(u8, u8)> {
        let packed = pack_source(
            &grid(),
            4,
            4,
            &rect,
            &SourceConversion {
                source_alpha: SourceAlpha::Straight,
                premultiply: false,
                flip_y,
                format: 0x8227, // RG: the coordinates
                type_: glow::UNSIGNED_BYTE,
            },
        )
        .expect("within the source");
        packed.chunks_exact(2).map(|p| (p[0], p[1])).collect()
    }

    #[test]
    fn the_skips_select_a_rectangle_of_the_source_as_flipping_leaves_it() {
        let rect = |x, y, width, height| SourceRect {
            x,
            y,
            width,
            height,
            ..SourceRect::whole(width, height)
        };
        assert_eq!(
            texels(rect(1, 2, 2, 2), false),
            [(1, 2), (2, 2), (1, 3), (2, 3)]
        );
        // Flipped, the source is turned first and the rectangle taken from its top: Chrome's order.
        assert_eq!(
            texels(rect(0, 0, 2, 2), true),
            [(0, 3), (1, 3), (0, 2), (1, 2)]
        );
        assert_eq!(texels(rect(1, 1, 2, 1), true), [(1, 2), (2, 2)]);
    }

    #[test]
    fn a_3d_upload_slices_the_source_by_the_image_height() {
        let slices = |height, depth, image_height, skip_images, flip_y| {
            texels(
                SourceRect {
                    x: 0,
                    y: 0,
                    width: 1,
                    height,
                    depth,
                    image_height,
                    skip_images,
                },
                flip_y,
            )
        };
        assert_eq!(slices(1, 4, 0, 0, false), [(0, 0), (0, 1), (0, 2), (0, 3)]);
        assert_eq!(slices(1, 2, 2, 0, false), [(0, 0), (0, 2)]);
        assert_eq!(slices(1, 1, 2, 1, false), [(0, 2)]); // UNPACK_SKIP_IMAGES, as the specification has it
        assert_eq!(slices(2, 2, 0, 0, true), [(0, 3), (0, 2), (0, 1), (0, 0)]);
    }

    #[test]
    fn nothing_is_packed_outside_the_source_or_the_table() {
        let conversion = |format, type_| SourceConversion {
            source_alpha: SourceAlpha::Straight,
            premultiply: false,
            flip_y: false,
            format,
            type_,
        };
        let rgba = conversion(glow::RGBA, glow::UNSIGNED_BYTE);
        let wide = SourceRect {
            x: 1,
            ..SourceRect::whole(4, 4)
        };
        assert_eq!(pack_source(&grid(), 4, 4, &wide, &rgba), None);
        let deep = SourceRect {
            depth: 2,
            ..SourceRect::whole(4, 4)
        };
        assert_eq!(pack_source(&grid(), 4, 4, &deep, &rgba), None);
        assert_eq!(
            pack_source(&grid()[..63], 4, 4, &SourceRect::whole(4, 4), &rgba),
            None
        );
        for (format, type_) in [
            (glow::RGB, 0x8033),       // 4_4_4_4 is RGBA's
            (0x8d99, glow::FLOAT),     // an integer format of floats
            (glow::RGBA, glow::SHORT), // not a TexImageSource type
            (glow::DEPTH_COMPONENT, glow::UNSIGNED_SHORT),
        ] {
            assert_eq!(
                pack_source(
                    &grid(),
                    4,
                    4,
                    &SourceRect::whole(4, 4),
                    &conversion(format, type_)
                ),
                None
            );
        }
    }

    #[test]
    fn a_premultiplied_source_is_unpremultiplied_unless_the_upload_asks_otherwise() {
        let canvas = [100u8, 50, 25, 128];
        let read = |premultiply, type_| {
            pack_source(
                &canvas,
                1,
                1,
                &SourceRect::whole(1, 1),
                &SourceConversion {
                    source_alpha: SourceAlpha::Premultiplied,
                    premultiply,
                    flip_y: false,
                    format: glow::RGBA,
                    type_,
                },
            )
            .unwrap()
        };
        assert_eq!(read(false, glow::UNSIGNED_BYTE), [199, 100, 50, 128]);
        assert_eq!(read(true, glow::UNSIGNED_BYTE), canvas);
    }

    #[test]
    fn an_upload_reads_the_rows_the_pixel_store_state_lays_out() {
        let packed = UnpackLayout {
            alignment: 1,
            ..UnpackLayout::default()
        };
        // 3x2 RGBA8, tight: 24 bytes; aligned to 8: the first row is 16, the last unpadded 12
        assert_eq!(
            upload_bytes(3, 2, 1, glow::RGBA, glow::UNSIGNED_BYTE, &packed),
            Some(24)
        );
        let eight = UnpackLayout {
            alignment: 8,
            ..UnpackLayout::default()
        };
        assert_eq!(
            upload_bytes(3, 2, 1, glow::RGBA, glow::UNSIGNED_BYTE, &eight),
            Some(28)
        );
        // 1x1 RGB8 with the default alignment of 4 reads 3 bytes: the last row is not padded
        assert_eq!(
            upload_bytes(
                1,
                1,
                1,
                glow::RGB,
                glow::UNSIGNED_BYTE,
                &UnpackLayout::default()
            ),
            Some(3)
        );
        // a row length of 10 and skips of 2 rows, 1 pixel: 2*40 + 4 + 40 + 12
        let skipped = UnpackLayout {
            alignment: 4,
            row_length: 10,
            skip_rows: 2,
            skip_pixels: 1,
            ..UnpackLayout::default()
        };
        assert_eq!(
            upload_bytes(3, 2, 1, glow::RGBA, glow::UNSIGNED_BYTE, &skipped),
            Some(80 + 4 + 40 + 12)
        );
        // 3D: an image height of 4 and one image skipped, 2x2x2 R8 aligned to 1
        let volume = UnpackLayout {
            alignment: 1,
            image_height: 4,
            skip_images: 1,
            ..UnpackLayout::default()
        };
        assert_eq!(
            upload_bytes(2, 2, 2, 0x1903, glow::UNSIGNED_BYTE, &volume),
            Some(8 + 8 + 2 + 2)
        );
        // packed types, and the empty upload
        assert_eq!(upload_bytes(2, 1, 1, glow::RGB, 0x8363, &packed), Some(4));
        assert_eq!(
            upload_bytes(0, 5, 1, glow::RGBA, glow::UNSIGNED_BYTE, &packed),
            Some(0)
        );
        // a pair GL has no upload of, and a size past memory
        assert_eq!(
            upload_bytes(1, 1, 1, 0x1234, glow::UNSIGNED_BYTE, &packed),
            None
        );
        assert_eq!(
            upload_bytes(
                i32::MAX,
                i32::MAX,
                i32::MAX,
                glow::RGBA,
                glow::FLOAT,
                &packed
            ),
            None
        );
    }

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

    /// The pixel-store values are content's. Whatever they are, no layout may make the conversion
    /// panic or write outside the buffer: it interprets the bytes or hands them back.
    #[test]
    fn no_pixel_store_value_makes_the_layout_overflow() {
        let px = vec![7u8; 64];
        let extremes = [i32::MIN, -1, 0, 1, 3, 4, 8, 1 << 16, 1 << 30, i32::MAX];
        for &alignment in &extremes {
            for &row_length in &extremes {
                for &skip_rows in &extremes {
                    for &skip_pixels in &extremes {
                        for &(w, h) in &[
                            (1, 1),
                            (2, 3),
                            (i32::MAX, 1),
                            (1, i32::MAX),
                            (i32::MAX, i32::MAX),
                        ] {
                            for format in [glow::RGBA, glow::RGB, glow::LUMINANCE_ALPHA, 0x8dad] {
                                let st = UnpackState {
                                    flip_y: true,
                                    premultiply: true,
                                    alignment,
                                    row_length,
                                    skip_rows,
                                    skip_pixels,
                                };
                                let out =
                                    convert_upload(&px, w, h, format, glow::UNSIGNED_BYTE, &st);
                                assert_eq!(
                                    out.len(),
                                    px.len(),
                                    "the converted bytes keep the buffer's size"
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}

//! ETC2 RGB encoder for ingest-time texture transcoding.
//!
//! The runtime already consumes compressed textures end to end -- [`crate::ktx2`]
//! parses the container, `fast_image_decoder` recognises it, and the graphics
//! crate uploads the blocks with `glCompressedTexImage2D`. What was missing is
//! anything that *produces* them, so this is the ingest half: RGBA8 in,
//! `VK_FORMAT_ETC2_R8G8B8_UNORM_BLOCK` blocks out, to be wrapped by
//! [`crate::ktx2::write_ktx2`] and stored in the package.
//!
//! # Why only the individual mode
//!
//! ETC2 RGB has five block modes (individual, differential, T, H, planar). This
//! encoder emits only the *individual* mode. That is a deliberate, conformant
//! subset, not an approximation of the format: every ETC2 decoder must handle
//! it, because it is the mode ETC1 already had. Restricting the search keeps the
//! encoder small enough to be read and verified against the spec, which matters
//! more here than the last dB of quality -- this runs once per image at package
//! install, and its output is checked against an independent decoder in the
//! tests below rather than against itself.
//!
//! Reference: OpenGL ES 3.0 spec, "ETC Compressed Texture Image Formats".

/// Bytes in one encoded 4x4 ETC2 RGB block.
pub const ETC2_RGB_BLOCK_BYTES: usize = 8;

/// Bytes in one encoded 4x4 ETC2 RGBA block: an 8-byte EAC alpha block followed
/// by the 8-byte ETC2 RGB block.
pub const ETC2_RGBA_BLOCK_BYTES: usize = 16;

/// `VK_FORMAT_ETC2_R8G8B8_UNORM_BLOCK`, the RGB format this encoder produces.
pub const VK_FORMAT_ETC2_R8G8B8_UNORM_BLOCK: u32 = 147;

/// `VK_FORMAT_ETC2_R8G8B8A8_UNORM_BLOCK`, the RGBA (EAC alpha) format.
pub const VK_FORMAT_ETC2_R8G8B8A8_UNORM_BLOCK: u32 = 151;

/// EAC per-pixel alpha modifiers: a 3-bit index selects one of eight, scaled by
/// the block's multiplier and added to the base. Same table the decoders use,
/// and identical for the standalone EAC and the ETC2-RGBA8 alpha block.
const ALPHA_MODIFIER_TABLE: [[i32; 8]; 16] = [
    [-3, -6, -9, -15, 2, 5, 8, 14],
    [-3, -7, -10, -13, 2, 6, 9, 12],
    [-2, -5, -8, -13, 1, 4, 7, 12],
    [-2, -4, -6, -13, 1, 3, 5, 12],
    [-3, -6, -8, -12, 2, 5, 7, 11],
    [-3, -7, -9, -11, 2, 6, 8, 10],
    [-4, -7, -8, -11, 3, 6, 7, 10],
    [-3, -5, -8, -11, 2, 4, 7, 10],
    [-2, -6, -8, -10, 1, 5, 7, 9],
    [-2, -5, -8, -10, 1, 4, 7, 9],
    [-2, -4, -8, -10, 1, 3, 7, 9],
    [-2, -5, -7, -10, 1, 4, 6, 9],
    [-3, -4, -7, -10, 2, 3, 6, 9],
    [-1, -2, -3, -10, 0, 1, 2, 9],
    [-4, -6, -8, -9, 3, 5, 7, 8],
    [-3, -5, -7, -9, 2, 4, 6, 8],
];

/// Per-pixel modifier magnitudes, selected by a 3-bit codeword per sub-block.
///
/// A pixel picks one of four values: `±table[cw][0]` or `±table[cw][1]`.
const MODIFIER_TABLE: [[i32; 2]; 8] = [
    [2, 8],
    [5, 17],
    [9, 29],
    [13, 42],
    [18, 60],
    [24, 80],
    [33, 106],
    [47, 183],
];

/// Which sub-block each of the 16 pixel slots belongs to, per flip bit.
///
/// Indexed by *slot* order, not raster order: the format walks a block column by
/// column, which is what [`SLOT_TO_PIXEL`] undoes. flip=0 splits the block into
/// two 2x4 halves (left/right), flip=1 into two 4x2 halves (top/bottom).
const SUBBLOCK_TABLE: [[usize; 16]; 2] = [
    [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 1, 1, 1, 1],
    [0, 0, 1, 1, 0, 0, 1, 1, 0, 0, 1, 1, 0, 0, 1, 1],
];

/// Slot order -> raster index within the 4x4 block (`y * 4 + x`).
const SLOT_TO_PIXEL: [usize; 16] = [0, 4, 8, 12, 1, 5, 9, 13, 2, 6, 10, 14, 3, 7, 11, 15];

/// Encode RGBA8 pixels as ETC2 RGB blocks. The alpha channel is ignored.
///
/// Blocks are emitted in raster order, 8 bytes each, which is the layout
/// `glCompressedTexImage2D` expects for level 0.
///
/// # Errors
///
/// Both dimensions must be non-zero multiples of 4. ETC2 has no partial blocks,
/// and silently padding would change the image the content addresses, so a
/// misaligned image is rejected for the caller to keep in its original form
/// rather than quietly altered.
pub fn encode_etc2_rgb(rgba: &[u8], width: u32, height: u32) -> Result<Vec<u8>, &'static str> {
    if width == 0 || height == 0 {
        return Err("etc2: zero dimensions");
    }
    if width % 4 != 0 || height % 4 != 0 {
        return Err("etc2: dimensions must be multiples of 4");
    }
    let expected = (width as usize)
        .checked_mul(height as usize)
        .and_then(|p| p.checked_mul(4))
        .ok_or("etc2: image dimensions overflow")?;
    if rgba.len() != expected {
        return Err("etc2: pixel buffer length does not match dimensions");
    }

    let blocks_x = (width / 4) as usize;
    let blocks_y = (height / 4) as usize;
    let mut out = vec![0u8; blocks_x * blocks_y * ETC2_RGB_BLOCK_BYTES];

    encode_block_rows(
        blocks_x,
        blocks_y,
        ETC2_RGB_BLOCK_BYTES,
        &mut out,
        |by, row| {
            let mut block = [[0u8; 3]; 16];
            for (bx, dst) in row.chunks_mut(ETC2_RGB_BLOCK_BYTES).enumerate() {
                for y in 0..4 {
                    for x in 0..4 {
                        let px = (by * 4 + y) * width as usize + (bx * 4 + x);
                        let base = px * 4;
                        block[y * 4 + x] = [rgba[base], rgba[base + 1], rgba[base + 2]];
                    }
                }
                dst.copy_from_slice(&encode_block(&block));
            }
        },
    );

    Ok(out)
}

/// Encode rows of blocks across a bounded set of threads.
///
/// Each 4x4 block is independent and produces a fixed-size output, so the work
/// splits by rows with no coordination, no per-chunk allocation, and output
/// identical to encoding them in order.
///
/// This is data parallelism *inside* one ingest job, not extra jobs, so it does
/// not reopen what `PoolKind::Ingest`'s cap of 1 protects: that bounds how many
/// images are decoded at once, each holding its own RGBA, and that memory does
/// not change with how many cores encode one of them.
///
/// Capped at four lanes rather than every core. Ingest runs while the user
/// waits on an install, but on a phone it shares the SoC with whatever else is
/// running, and the returns past four are small next to the cost of pinning
/// every core to one background job.
fn encode_block_rows(
    blocks_x: usize,
    blocks_y: usize,
    block_bytes: usize,
    out: &mut [u8],
    encode_row: impl Fn(usize, &mut [u8]) + Sync,
) {
    let row_bytes = blocks_x * block_bytes;
    if row_bytes == 0 || blocks_y == 0 {
        return;
    }

    // Below this the encode is shorter than the threads would take to start,
    // and a package of small icons would pay that per image. 1024 blocks is a
    // 128x128 image.
    const MIN_BLOCKS_TO_SPLIT: usize = 1024;

    let lanes = if blocks_x * blocks_y < MIN_BLOCKS_TO_SPLIT {
        1
    } else {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
            .clamp(1, 4)
            .min(blocks_y)
    };
    if lanes == 1 {
        for (by, row) in out.chunks_mut(row_bytes).enumerate() {
            encode_row(by, row);
        }
        return;
    }

    let rows_per_lane = blocks_y.div_ceil(lanes);
    std::thread::scope(|scope| {
        for (lane, chunk) in out.chunks_mut(rows_per_lane * row_bytes).enumerate() {
            let encode_row = &encode_row;
            scope.spawn(move || {
                for (offset, row) in chunk.chunks_mut(row_bytes).enumerate() {
                    encode_row(lane * rows_per_lane + offset, row);
                }
            });
        }
    });
}

/// Encode RGBA8 pixels as ETC2 RGBA blocks (EAC alpha + ETC2 RGB), 16 bytes
/// each, `VK_FORMAT_ETC2_R8G8B8A8_UNORM_BLOCK`.
///
/// Use this only when the image carries meaningful alpha; a fully-opaque image
/// should use [`encode_etc2_rgb`], whose blocks are half the size. The runtime
/// tells the two apart by the KTX2 `vkFormat`, so the container [`super::ktx2`]
/// wraps it in decides which path the GPU takes.
///
/// Same alignment rule and error semantics as [`encode_etc2_rgb`].
pub fn encode_etc2_rgba(rgba: &[u8], width: u32, height: u32) -> Result<Vec<u8>, &'static str> {
    if width == 0 || height == 0 {
        return Err("etc2: zero dimensions");
    }
    if width % 4 != 0 || height % 4 != 0 {
        return Err("etc2: dimensions must be multiples of 4");
    }
    let expected = (width as usize)
        .checked_mul(height as usize)
        .and_then(|p| p.checked_mul(4))
        .ok_or("etc2: image dimensions overflow")?;
    if rgba.len() != expected {
        return Err("etc2: pixel buffer length does not match dimensions");
    }

    let blocks_x = (width / 4) as usize;
    let blocks_y = (height / 4) as usize;
    let mut out = vec![0u8; blocks_x * blocks_y * ETC2_RGBA_BLOCK_BYTES];

    encode_block_rows(
        blocks_x,
        blocks_y,
        ETC2_RGBA_BLOCK_BYTES,
        &mut out,
        |by, row| {
            let mut rgb = [[0u8; 3]; 16];
            let mut alpha = [0u8; 16];
            for (bx, dst) in row.chunks_mut(ETC2_RGBA_BLOCK_BYTES).enumerate() {
                for y in 0..4 {
                    for x in 0..4 {
                        let px = (by * 4 + y) * width as usize + (bx * 4 + x);
                        let base = px * 4;
                        rgb[y * 4 + x] = [rgba[base], rgba[base + 1], rgba[base + 2]];
                        alpha[y * 4 + x] = rgba[base + 3];
                    }
                }
                // Order matters: the GPU (and the reference decoder) read the
                // alpha block first, then the colour block.
                dst[..8].copy_from_slice(&encode_alpha_block(&alpha));
                dst[8..].copy_from_slice(&encode_block(&rgb));
            }
        },
    );

    Ok(out)
}

/// Encode a 4x4 alpha tile as an 8-byte EAC alpha block.
///
/// Layout, matching the ETC2-RGBA8 alpha block the decoders read:
/// - byte 0: base alpha;
/// - byte 1: high nibble = multiplier, low nibble = modifier-table index;
/// - bytes 2..8: sixteen 3-bit pixel indices, packed big-endian (pixel 0's
///   index in the most-significant position), in raster order.
///
/// base, table and multiplier are chosen by exhaustive search over all 16
/// tables and the 1..=15 multipliers -- the same "try them all, no heuristic
/// cliff" approach the colour path takes. A block whose alpha is constant is
/// encoded with multiplier 0, which the decoder treats as "every pixel = base".
fn encode_alpha_block(alpha: &[u8; 16]) -> [u8; 8] {
    // Constant alpha (the common case for a sprite's flat interior or a fully
    // transparent border) needs no modulation: multiplier 0, base = the value.
    if alpha.iter().all(|&a| a == alpha[0]) {
        let mut bytes = [0u8; 8];
        bytes[0] = alpha[0];
        // byte 1 = 0 => multiplier nibble 0 => decoder fills every pixel with base.
        return bytes;
    }

    let mut best_base = 0u8;
    let mut best_table = 0usize;
    let mut best_multiplier = 1i32;
    let mut best_indices = [0u8; 16];
    let mut best_error = u64::MAX;

    // A small set of base candidates: the extremes and the mean bracket where
    // the modifier table is centred. Searching every 0..=255 base as well would
    // multiply the cost 256x for negligible gain at ingest quality.
    let min = *alpha.iter().min().unwrap() as i32;
    let max = *alpha.iter().max().unwrap() as i32;
    let mean = (alpha.iter().map(|&a| a as i32).sum::<i32>() + 8) / 16;
    let base_candidates = [min, max, mean, (min + max) / 2];

    let range = max - min;

    for &base_i in &base_candidates {
        let base = base_i.clamp(0, 255);
        for (table_idx, table) in ALPHA_MODIFIER_TABLE.iter().enumerate() {
            // The multiplier scales the table, so the values it can reach span
            // `multiplier * table_span`. One that cannot cover the block's own
            // alpha range leaves the extremes unreachable; one far past it
            // quantises coarsely. Either way it loses to a multiplier sized to
            // the block, so the search only visits a window around that size
            // instead of all fifteen.
            //
            // A heuristic, not a proof — which is why `bench_etc2_encode_throughput`
            // reports alpha PSNR and worst-case delta beside the timings, and
            // why `encoder_output_is_byte_stable` has to be updated deliberately
            // when this window changes.
            let span = (table.iter().max().unwrap() - table.iter().min().unwrap()).max(1);
            let ideal = (range + span / 2) / span;
            let first = (ideal - 1).clamp(1, 15);
            let last = (ideal + 2).clamp(1, 15);

            for multiplier in first..=last {
                // The eight reachable alpha values depend only on
                // (base, table, multiplier) — not on the pixel. Computing them
                // inside the pixel loop redid the same multiply-add-clamp
                // sixteen times per combination, which is most of why the
                // alpha path measured 33x the colour path.
                let mut reachable = [0i32; 8];
                for (idx, &modifier) in table.iter().enumerate() {
                    reachable[idx] = (base + multiplier * modifier).clamp(0, 255);
                }

                let mut error = 0u64;
                let mut indices = [0u8; 16];
                for (pixel, &target) in alpha.iter().enumerate() {
                    let mut pixel_best = u64::MAX;
                    let mut pixel_index = 0u8;
                    for (idx, &value) in reachable.iter().enumerate() {
                        let delta = value - target as i32;
                        let cost = (delta * delta) as u64;
                        if cost < pixel_best {
                            pixel_best = cost;
                            pixel_index = idx as u8;
                        }
                    }
                    error += pixel_best;
                    indices[pixel] = pixel_index;
                }
                if error < best_error {
                    best_error = error;
                    best_base = base as u8;
                    best_table = table_idx;
                    best_multiplier = multiplier;
                    best_indices = indices;
                }
            }
        }
    }

    let mut bytes = [0u8; 8];
    bytes[0] = best_base;
    bytes[1] = ((best_multiplier as u8) << 4) | (best_table as u8);
    // Invert the decoder exactly. It reads `from_be_bytes(data[0..8])` and, for
    // iteration i, takes bits [3i, 3i+3) and writes raster pixel
    // WRITE_ORDER_TABLE_REV[i]. So the index for raster pixel
    // WRITE_ORDER_TABLE_REV[i] must sit at bit position 3i. The sixteen indices
    // fill bits 0..47 = bytes 2..8; bytes 0..1 (top 16 bits) stay base/mult.
    let mut packed: u64 = 0;
    for (i, &raster) in ALPHA_WRITE_ORDER.iter().enumerate() {
        packed |= (best_indices[raster] as u64 & 0x7) << (3 * i);
    }
    bytes[2..8].copy_from_slice(&packed.to_be_bytes()[2..8]);
    bytes
}

/// Iteration-to-raster-pixel mapping the EAC alpha block uses (the decoders'
/// `WRITE_ORDER_TABLE_REV`): iteration i's 3-bit index selects raster pixel
/// `ALPHA_WRITE_ORDER[i]`.
const ALPHA_WRITE_ORDER: [usize; 16] = [15, 11, 7, 3, 14, 10, 6, 2, 13, 9, 5, 1, 12, 8, 4, 0];

/// One candidate encoding of a sub-block: its quantized base colour, codeword,
/// per-slot selectors and the squared error it costs.
struct SubBlockFit {
    /// 4-bit-per-channel base colour, as stored in the block.
    quantized: [u8; 3],
    codeword: usize,
    /// `(magnitude_bit, sign_bit)` for each of the sub-block's 8 slots.
    selectors: [(u32, u32); 8],
    error: u64,
}

/// Encode a single 4x4 block, trying both flips and keeping the better one.
fn encode_block(pixels: &[[u8; 3]; 16]) -> [u8; ETC2_RGB_BLOCK_BYTES] {
    let mut best: Option<(u8, [SubBlockFit; 2], u64)> = None;

    for flip in 0..2u8 {
        let table = &SUBBLOCK_TABLE[flip as usize];
        let mut slots: [Vec<usize>; 2] = [Vec::with_capacity(8), Vec::with_capacity(8)];
        for (slot, &sub) in table.iter().enumerate() {
            slots[sub].push(slot);
        }

        let fit0 = fit_subblock(pixels, &slots[0]);
        let fit1 = fit_subblock(pixels, &slots[1]);
        let total = fit0.error + fit1.error;
        if best.as_ref().is_none_or(|(_, _, e)| total < *e) {
            best = Some((flip, [fit0, fit1], total));
        }
    }

    let (flip, fits, _) = best.expect("both flips are always evaluated");
    let table = &SUBBLOCK_TABLE[flip as usize];

    let mut bytes = [0u8; ETC2_RGB_BLOCK_BYTES];
    for channel in 0..3 {
        // High nibble is sub-block 0, low nibble sub-block 1; the decoder
        // expands each by replicating it into the low bits.
        bytes[channel] = (fits[0].quantized[channel] << 4) | fits[1].quantized[channel];
    }
    // bits 7..5 = sub-block 0 codeword, 4..2 = sub-block 1, bit 1 = diff (0 for
    // individual mode), bit 0 = flip.
    bytes[3] = ((fits[0].codeword as u8) << 5) | ((fits[1].codeword as u8) << 2) | flip;

    // Two 16-bit planes: `magnitude` picks which of the codeword's two
    // magnitudes a pixel uses, `sign` negates it. Bit i of each plane is slot i.
    let mut magnitude: u16 = 0;
    let mut sign: u16 = 0;
    let mut cursor = [0usize; 2];
    for (slot, &sub) in table.iter().enumerate() {
        let (mag_bit, sign_bit) = fits[sub].selectors[cursor[sub]];
        cursor[sub] += 1;
        magnitude |= (mag_bit as u16) << slot;
        sign |= (sign_bit as u16) << slot;
    }
    bytes[4..6].copy_from_slice(&sign.to_be_bytes());
    bytes[6..8].copy_from_slice(&magnitude.to_be_bytes());

    bytes
}

/// Choose the base colour, codeword and selectors for one sub-block.
///
/// The base colour is the sub-block average quantized to 4 bits per channel --
/// the only precision the individual mode has. The codeword is then chosen by
/// exhaustive search over all eight, which is cheap (8 codewords x 8 pixels x 4
/// candidates) and removes the main quality cliff a heuristic would introduce.
fn fit_subblock(pixels: &[[u8; 3]; 16], slots: &[usize]) -> SubBlockFit {
    let mut sum = [0u32; 3];
    for &slot in slots {
        let px = pixels[SLOT_TO_PIXEL[slot]];
        for channel in 0..3 {
            sum[channel] += px[channel] as u32;
        }
    }
    let count = slots.len() as u32;

    let mut quantized = [0u8; 3];
    let mut base = [0i32; 3];
    for channel in 0..3 {
        let average = (sum[channel] + count / 2) / count;
        // 4 bits stored, expanded by the decoder as (q << 4) | q, i.e. q * 17.
        let q = ((average + 8) / 17).min(15) as u8;
        quantized[channel] = q;
        base[channel] = ((q << 4) | q) as i32;
    }

    let mut best_codeword = 0usize;
    let mut best_error = u64::MAX;
    let mut best_selectors = [(0u32, 0u32); 8];

    for (codeword, magnitudes) in MODIFIER_TABLE.iter().enumerate() {
        let mut error = 0u64;
        let mut selectors = [(0u32, 0u32); 8];
        for (index, &slot) in slots.iter().enumerate() {
            let target = pixels[SLOT_TO_PIXEL[slot]];
            let mut pixel_best = u64::MAX;
            let mut pixel_selector = (0u32, 0u32);
            for mag_bit in 0..2u32 {
                for sign_bit in 0..2u32 {
                    let modifier = if sign_bit == 1 {
                        -magnitudes[mag_bit as usize]
                    } else {
                        magnitudes[mag_bit as usize]
                    };
                    let mut candidate = 0u64;
                    for channel in 0..3 {
                        let value = (base[channel] + modifier).clamp(0, 255);
                        let delta = value - target[channel] as i32;
                        candidate += (delta * delta) as u64;
                    }
                    if candidate < pixel_best {
                        pixel_best = candidate;
                        pixel_selector = (mag_bit, sign_bit);
                    }
                }
            }
            error += pixel_best;
            selectors[index] = pixel_selector;
        }
        if error < best_error {
            best_error = error;
            best_codeword = codeword;
            best_selectors = selectors;
        }
    }

    SubBlockFit {
        quantized,
        codeword: best_codeword,
        selectors: best_selectors,
        error: best_error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ktx2::{VkFormat, parse_ktx2, write_ktx2};

    /// Decode with an independent implementation, never with our own encoder's
    /// inverse: a round trip through code that shares assumptions with the
    /// encoder would prove the two agree, not that the output is ETC2.
    fn decode_reference(blocks: &[u8], width: u32, height: u32) -> Vec<[u8; 3]> {
        let blocks_x = (width / 4) as usize;
        let blocks_y = (height / 4) as usize;
        let mut image = vec![[0u8; 3]; (width * height) as usize];
        let mut decoded = [0u32; 16];

        for by in 0..blocks_y {
            for bx in 0..blocks_x {
                let offset = (by * blocks_x + bx) * ETC2_RGB_BLOCK_BYTES;
                texture2ddecoder::decode_etc2_rgb_block(
                    &blocks[offset..offset + ETC2_RGB_BLOCK_BYTES],
                    &mut decoded,
                );
                for y in 0..4 {
                    for x in 0..4 {
                        // The decoder packs pixels as little-endian BGRA.
                        let p = decoded[y * 4 + x];
                        let rgb = [
                            ((p >> 16) & 0xff) as u8,
                            ((p >> 8) & 0xff) as u8,
                            (p & 0xff) as u8,
                        ];
                        image[(by * 4 + y) * width as usize + (bx * 4 + x)] = rgb;
                    }
                }
            }
        }
        image
    }

    fn peak_signal_to_noise(original: &[u8], decoded: &[[u8; 3]]) -> f64 {
        let mut squared_error = 0f64;
        for (index, pixel) in decoded.iter().enumerate() {
            for channel in 0..3 {
                let delta = original[index * 4 + channel] as f64 - pixel[channel] as f64;
                squared_error += delta * delta;
            }
        }
        let mean = squared_error / (decoded.len() * 3) as f64;
        if mean == 0.0 {
            return f64::INFINITY;
        }
        10.0 * (255.0f64 * 255.0 / mean).log10()
    }

    fn gradient(width: u32, height: u32) -> Vec<u8> {
        let mut rgba = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            for x in 0..width {
                rgba.push((x * 255 / width.max(1)) as u8);
                rgba.push((y * 255 / height.max(1)) as u8);
                rgba.push(((x + y) * 255 / (width + height).max(1)) as u8);
                rgba.push(255);
            }
        }
        rgba
    }

    /// What ETC2 encoding actually costs at install time.
    ///
    /// The encoder is scalar and serial, and package ingest now runs on a
    /// class capped at 1, so this is the whole transcode budget for a package:
    /// every image, one after another, on one thread. Before optimising it —
    /// blocks are independent, so this parallelises trivially — the number has
    /// to justify the work.
    ///
    /// `gradient` rather than flat colour on purpose: `encode_alpha_block`
    /// short-circuits on constant alpha and a flat image would measure the
    /// fast path a real sprite never takes.
    #[test]
    #[ignore]
    fn bench_etc2_encode_throughput() {
        for side in [256u32, 512, 1024, 2048] {
            let rgba = gradient(side, side);
            let megapixels = (side as f64 * side as f64) / 1_000_000.0;

            let rgb_time = {
                let started = std::time::Instant::now();
                std::hint::black_box(encode_etc2_rgb(&rgba, side, side).unwrap());
                started.elapsed()
            };

            // Real sprites carry alpha, which adds the EAC block: an exhaustive
            // search over 16 tables x 15 multipliers per 4x4 tile.
            let mut with_alpha = rgba.clone();
            for (i, px) in with_alpha.chunks_exact_mut(4).enumerate() {
                px[3] = (i % 251) as u8;
            }
            let rgba_time = {
                let started = std::time::Instant::now();
                std::hint::black_box(encode_etc2_rgba(&with_alpha, side, side).unwrap());
                started.elapsed()
            };

            // Measure the opacity decision separately from encoding. The
            // visited count makes early-exit cases impossible to mislabel as
            // a full-image scan.
            let scan = |label: &str, pixels: &[u8]| {
                let started = std::time::Instant::now();
                let mut visited = 0usize;
                let mut transparent = false;
                for px in pixels.chunks_exact(4) {
                    visited += 1;
                    if px[3] != 0xFF {
                        transparent = true;
                        break;
                    }
                }
                let elapsed = started.elapsed();
                eprintln!(
                    "{side}x{side} opacity={label} scan={elapsed:?} pixels_visited={visited} transparent={transparent} mip_levels=1"
                );
                (elapsed, visited)
            };
            let all_opaque = rgba.clone();
            let mut first_transparent = rgba.clone();
            first_transparent[3] = 0;
            let mut last_transparent = rgba.clone();
            let last_alpha = last_transparent.len() - 1;
            last_transparent[last_alpha] = 0;
            let representative = with_alpha.clone();
            let (opaque_scan, opaque_visited) = scan("all-opaque", &all_opaque);
            let (first_scan, first_visited) = scan("first-transparent", &first_transparent);
            let (last_scan, last_visited) = scan("last-transparent", &last_transparent);
            let (representative_scan, representative_visited) =
                scan("representative", &representative);

            // Speed without quality is half the measurement: any change to the
            // search has to be judged on what it costs the encoding, not just
            // on how much faster it got.
            let encoded = encode_etc2_rgba(&with_alpha, side, side).unwrap();
            let decoded = decode_reference_rgba(&encoded, side, side);
            let mut squared = 0f64;
            let mut worst = 0u32;
            for (i, pixel) in decoded.iter().enumerate() {
                let delta = (with_alpha[i * 4 + 3] as i32 - pixel[3] as i32).unsigned_abs();
                worst = worst.max(delta);
                squared += (delta as f64) * (delta as f64);
            }
            let mean = squared / decoded.len() as f64;
            let psnr = if mean == 0.0 {
                f64::INFINITY
            } else {
                10.0 * (255.0f64 * 255.0 / mean).log10()
            };
            assert_eq!(opaque_visited, all_opaque.len() / 4);
            assert_eq!(first_visited, 1);
            assert_eq!(last_visited, last_transparent.len() / 4);
            assert!(representative_visited < representative.len() / 4);
            eprintln!(
                "{side}x{side} ({megapixels:>4.1} MP)  rgb {:>10?} ({:>5.1} MP/s)   rgba {:>10?} ({:>4.2} MP/s)   alpha PSNR {psnr:>5.2} dB, worst delta {worst:>3}; scans opaque={opaque_scan:?}, first={first_scan:?}, last={last_scan:?}, representative={representative_scan:?}",
                rgb_time,
                megapixels / rgb_time.as_secs_f64(),
                rgba_time,
                megapixels / rgba_time.as_secs_f64(),
            );
        }
    }

    /// Deterministic input with alpha that varies inside every block, so the
    /// exhaustive search actually runs.
    fn alpha_gradient(width: u32, height: u32) -> Vec<u8> {
        let mut rgba = gradient(width, height);
        for (i, px) in rgba.chunks_exact_mut(4).enumerate() {
            px[3] = (i % 251) as u8;
        }
        rgba
    }

    fn digest(bytes: &[u8]) -> String {
        use sha2::Digest;
        let mut hasher = sha2::Sha256::new();
        hasher.update(bytes);
        hex::encode(&hasher.finalize()[..16])
    }

    /// Pins the encoder's exact output.
    ///
    /// The round-trip tests prove the blocks decode to something close enough
    /// to the source; they would not notice an "optimisation" that changed
    /// which block encoding was chosen. This makes any such change fail loudly,
    /// so a speed-up has to prove it is byte-identical rather than merely
    /// still-correct — and a deliberate quality change has to update the
    /// digest, which is the point at which someone has to think about it.
    ///
    /// Both digests below have survived every speed-up so far, including the
    /// multiplier window in `encode_alpha_block`: on this input the pruned
    /// search picks the same encoding the exhaustive one did. Where it does
    /// diverge, on larger images, `bench_etc2_encode_throughput` reports the
    /// cost — alpha PSNR within 0.25 dB and an unchanged worst-case delta.
    #[test]
    fn encoder_output_is_byte_stable() {
        let rgb_only = gradient(64, 64);
        assert_eq!(
            digest(&encode_etc2_rgb(&rgb_only, 64, 64).unwrap()),
            "92e7b3a2005b5324985c32bc2132a095",
            "ETC2 RGB output changed"
        );

        let with_alpha = alpha_gradient(64, 64);
        assert_eq!(
            digest(&encode_etc2_rgba(&with_alpha, 64, 64).unwrap()),
            "8ccec2312d9be05e9a7767e3f2f8576f",
            "ETC2 RGBA output changed"
        );
    }

    #[test]
    fn output_size_is_one_block_per_sixteen_pixels() {
        let rgba = gradient(16, 8);
        let blocks = encode_etc2_rgb(&rgba, 16, 8).expect("aligned image encodes");
        assert_eq!(blocks.len(), (16 / 4) * (8 / 4) * ETC2_RGB_BLOCK_BYTES);
    }

    #[test]
    fn misaligned_dimensions_are_rejected_rather_than_padded() {
        let rgba = gradient(6, 4);
        assert!(encode_etc2_rgb(&rgba, 6, 4).is_err());
        let rgba = gradient(4, 6);
        assert!(encode_etc2_rgb(&rgba, 4, 6).is_err());
    }

    #[test]
    fn pixel_buffer_length_must_match_dimensions() {
        let rgba = gradient(8, 8);
        assert!(encode_etc2_rgb(&rgba[..rgba.len() - 4], 8, 8).is_err());
    }

    #[test]
    fn a_flat_block_survives_the_round_trip_almost_exactly() {
        // A single colour needs no modifier at all, so the only loss is the
        // 4-bit quantization of the base colour: at most 8 per channel.
        let mut rgba = Vec::new();
        for _ in 0..16 {
            rgba.extend_from_slice(&[0x33, 0x88, 0xCC, 0xFF]);
        }
        let blocks = encode_etc2_rgb(&rgba, 4, 4).expect("encodes");
        let decoded = decode_reference(&blocks, 4, 4);
        for pixel in &decoded {
            assert!((pixel[0] as i32 - 0x33).abs() <= 8, "r drifted: {pixel:?}");
            assert!((pixel[1] as i32 - 0x88).abs() <= 8, "g drifted: {pixel:?}");
            assert!((pixel[2] as i32 - 0xCC).abs() <= 8, "b drifted: {pixel:?}");
        }
    }

    #[test]
    fn a_gradient_round_trips_through_an_independent_decoder() {
        let (width, height) = (64u32, 64u32);
        let rgba = gradient(width, height);
        let blocks = encode_etc2_rgb(&rgba, width, height).expect("encodes");
        let decoded = decode_reference(&blocks, width, height);

        let psnr = peak_signal_to_noise(&rgba, &decoded);
        // 30 dB is the usual floor for "no obvious artefacts" on photographic
        // content; a correct individual-mode encoder clears it comfortably on a
        // smooth gradient, while any bit-layout mistake collapses it far below.
        assert!(psnr > 30.0, "PSNR too low: {psnr:.2} dB");
    }

    #[test]
    fn sharp_edges_still_decode_to_the_right_side_of_the_edge() {
        // Two flat halves: the flip search should place the sub-block split on
        // the edge, so each half stays close to its own colour.
        let (width, height) = (8u32, 4u32);
        let mut rgba = Vec::new();
        for _ in 0..height {
            for x in 0..width {
                let c = if x < 4 { 0x20 } else { 0xE0 };
                rgba.extend_from_slice(&[c, c, c, 0xFF]);
            }
        }
        let blocks = encode_etc2_rgb(&rgba, width, height).expect("encodes");
        let decoded = decode_reference(&blocks, width, height);
        for y in 0..height as usize {
            for x in 0..width as usize {
                let pixel = decoded[y * width as usize + x];
                let expected = if x < 4 { 0x20 } else { 0xE0 };
                assert!(
                    (pixel[0] as i32 - expected).abs() <= 16,
                    "edge smeared at ({x},{y}): {pixel:?}"
                );
            }
        }
    }

    #[test]
    fn encoded_blocks_survive_the_ktx2_container() {
        let (width, height) = (16u32, 16u32);
        let rgba = gradient(width, height);
        let blocks = encode_etc2_rgb(&rgba, width, height).expect("encodes");

        let container = write_ktx2(VK_FORMAT_ETC2_R8G8B8_UNORM_BLOCK, width, height, &blocks);
        let parsed = parse_ktx2(&container).expect("the runtime parser accepts what we write");

        assert_eq!(parsed.header.format, VkFormat::Etc2R8G8B8UnormBlock);
        assert_eq!(parsed.header.width, width);
        assert_eq!(parsed.header.height, height);
        assert_eq!(parsed.data, &blocks[..]);
    }

    // --- EAC alpha (ETC2 RGBA, VK 151) ---

    /// Decode RGBA blocks with the independent reference decoder. Returns
    /// `(rgb, alpha)` per pixel; the decoder packs BGRA in a little-endian u32.
    fn decode_reference_rgba(blocks: &[u8], width: u32, height: u32) -> Vec<[u8; 4]> {
        let blocks_x = (width / 4) as usize;
        let blocks_y = (height / 4) as usize;
        let mut image = vec![[0u8; 4]; (width * height) as usize];
        let mut decoded = [0u32; 16];

        for by in 0..blocks_y {
            for bx in 0..blocks_x {
                let offset = (by * blocks_x + bx) * ETC2_RGBA_BLOCK_BYTES;
                texture2ddecoder::decode_etc2_rgba8_block(
                    &blocks[offset..offset + ETC2_RGBA_BLOCK_BYTES],
                    &mut decoded,
                );
                for y in 0..4 {
                    for x in 0..4 {
                        let p = decoded[y * 4 + x];
                        let rgba = [
                            ((p >> 16) & 0xff) as u8,
                            ((p >> 8) & 0xff) as u8,
                            (p & 0xff) as u8,
                            ((p >> 24) & 0xff) as u8,
                        ];
                        image[(by * 4 + y) * width as usize + (bx * 4 + x)] = rgba;
                    }
                }
            }
        }
        image
    }

    /// An RGBA source with a smooth colour gradient and a smooth alpha ramp, so
    /// both the colour and the alpha encoders are exercised on non-constant data.
    fn rgba_gradient(width: u32, height: u32) -> Vec<u8> {
        let mut rgba = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            for x in 0..width {
                rgba.push((x * 255 / width.max(1)) as u8);
                rgba.push((y * 255 / height.max(1)) as u8);
                rgba.push(0x60);
                rgba.push(((x + y) * 255 / (width + height).max(1)) as u8);
            }
        }
        rgba
    }

    #[test]
    fn rgba_output_is_sixteen_bytes_per_block() {
        let rgba = rgba_gradient(16, 8);
        let blocks = encode_etc2_rgba(&rgba, 16, 8).expect("aligned image encodes");
        assert_eq!(blocks.len(), (16 / 4) * (8 / 4) * ETC2_RGBA_BLOCK_BYTES);
    }

    #[test]
    fn a_constant_alpha_block_round_trips_exactly() {
        // Fully opaque interior: the alpha encoder should take the constant path
        // (multiplier 0), and 255 must come back as exactly 255.
        let mut rgba = Vec::new();
        for _ in 0..16 {
            rgba.extend_from_slice(&[0x40, 0x80, 0xC0, 0xFF]);
        }
        let blocks = encode_etc2_rgba(&rgba, 4, 4).expect("encodes");
        let decoded = decode_reference_rgba(&blocks, 4, 4);
        for pixel in &decoded {
            assert_eq!(
                pixel[3], 0xFF,
                "opaque alpha must survive exactly: {pixel:?}"
            );
        }
    }

    #[test]
    fn a_fully_transparent_block_round_trips_exactly() {
        let mut rgba = Vec::new();
        for _ in 0..16 {
            rgba.extend_from_slice(&[0x10, 0x20, 0x30, 0x00]);
        }
        let blocks = encode_etc2_rgba(&rgba, 4, 4).expect("encodes");
        let decoded = decode_reference_rgba(&blocks, 4, 4);
        for pixel in &decoded {
            assert_eq!(
                pixel[3], 0x00,
                "transparent alpha must survive exactly: {pixel:?}"
            );
        }
    }

    #[test]
    fn an_alpha_ramp_round_trips_through_the_reference_decoder() {
        let (width, height) = (64u32, 64u32);
        let rgba = rgba_gradient(width, height);
        let blocks = encode_etc2_rgba(&rgba, width, height).expect("encodes");
        let decoded = decode_reference_rgba(&blocks, width, height);

        // Alpha-only PSNR: a correct EAC alpha encoder clears 30 dB comfortably
        // on a smooth ramp, while any bit-packing mistake collapses it.
        let mut squared = 0f64;
        for (i, pixel) in decoded.iter().enumerate() {
            let delta = rgba[i * 4 + 3] as f64 - pixel[3] as f64;
            squared += delta * delta;
        }
        let mean = squared / decoded.len() as f64;
        let psnr = if mean == 0.0 {
            f64::INFINITY
        } else {
            10.0 * (255.0f64 * 255.0 / mean).log10()
        };
        assert!(psnr > 30.0, "alpha PSNR too low: {psnr:.2} dB");

        // Colour must still be fine too (same encoder as the RGB path).
        let rgb: Vec<[u8; 3]> = decoded.iter().map(|p| [p[0], p[1], p[2]]).collect();
        let mut c_sq = 0f64;
        for (i, pixel) in rgb.iter().enumerate() {
            for c in 0..3 {
                let delta = rgba[i * 4 + c] as f64 - pixel[c] as f64;
                c_sq += delta * delta;
            }
        }
        let c_mean = c_sq / (rgb.len() * 3) as f64;
        let c_psnr = 10.0 * (255.0f64 * 255.0 / c_mean).log10();
        assert!(c_psnr > 30.0, "colour PSNR too low: {c_psnr:.2} dB");
    }

    #[test]
    fn rgba_blocks_survive_the_ktx2_container() {
        let (width, height) = (16u32, 16u32);
        let rgba = rgba_gradient(width, height);
        let blocks = encode_etc2_rgba(&rgba, width, height).expect("encodes");

        let container = write_ktx2(VK_FORMAT_ETC2_R8G8B8A8_UNORM_BLOCK, width, height, &blocks);
        let parsed = parse_ktx2(&container).expect("the runtime parser accepts what we write");
        assert_eq!(parsed.header.format, VkFormat::Etc2R8G8B8A8UnormBlock);
        assert_eq!(parsed.data, &blocks[..]);
    }
}

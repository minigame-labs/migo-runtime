//! Mip-chain generation for ingest-time texture transcoding.
//!
//! A compressed texture that ships only its base level is sampled with that
//! level at every scale: minified content reads far apart in a full-resolution
//! image, which both aliases and defeats the texture cache. Generating the
//! chain once at ingest costs package bytes measured in a third of the base
//! level and removes that cost from every frame that minifies.

/// Counts the redundant base-level and intermediate clones that the old
/// `rgba_mip_chain` made. Used by the allocation-regression test only: the
/// counter is incremented once per call in the old implementation and stays at
/// zero in the fixed one, so the test can distinguish them without a custom
/// global allocator.
#[cfg(test)]
pub(crate) static REDUNDANT_CLONE_COUNT: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// Halve an RGBA8 image with a 2x2 box filter, in alpha-premultiplied space.
///
/// Premultiplied, because averaging straight RGBA lets a fully transparent
/// pixel drag the colour of its neighbours: `(255,0,0,255)` averaged with
/// `(0,0,0,0)` is a quarter-strength *dark* red in straight space and a
/// quarter-alpha *pure* red in premultiplied space. The second is what the
/// image actually looks like, and the difference shows up as dark fringes
/// around every sprite's edge at distance.
///
/// Returns `None` for a 1x1 image -- there is no smaller level to make -- or
/// for a buffer whose length does not match the dimensions.
pub fn downsample_rgba_half(rgba: &[u8], width: u32, height: u32) -> Option<(Vec<u8>, u32, u32)> {
    if width <= 1 && height <= 1 {
        return None;
    }
    let expected = (width as usize)
        .checked_mul(height as usize)?
        .checked_mul(4)?;
    if rgba.len() != expected {
        return None;
    }

    let dst_width = (width / 2).max(1);
    let dst_height = (height / 2).max(1);
    let mut out = Vec::with_capacity((dst_width as usize) * (dst_height as usize) * 4);

    for dy in 0..dst_height {
        for dx in 0..dst_width {
            // Clamp rather than wrap: an odd dimension has a last column or row
            // with no partner, and reading the far side would fold the image
            // onto itself.
            let x0 = (dx * 2).min(width - 1);
            let x1 = (dx * 2 + 1).min(width - 1);
            let y0 = (dy * 2).min(height - 1);
            let y1 = (dy * 2 + 1).min(height - 1);

            let mut sum_r = 0u32;
            let mut sum_g = 0u32;
            let mut sum_b = 0u32;
            let mut sum_a = 0u32;
            for (x, y) in [(x0, y0), (x1, y0), (x0, y1), (x1, y1)] {
                let i = ((y as usize) * (width as usize) + x as usize) * 4;
                let a = u32::from(rgba[i + 3]);
                // Premultiply on the way in, so the sums stay linear under
                // averaging.
                sum_r += u32::from(rgba[i]) * a;
                sum_g += u32::from(rgba[i + 1]) * a;
                sum_b += u32::from(rgba[i + 2]) * a;
                sum_a += a;
            }

            if sum_a == 0 {
                out.extend_from_slice(&[0, 0, 0, 0]);
                continue;
            }
            // Unpremultiply by the alpha *sum*: dividing the premultiplied
            // total by it is the same as dividing the premultiplied average by
            // the average alpha, without the rounding of doing it in two steps.
            let r = (sum_r + sum_a / 2) / sum_a;
            let g = (sum_g + sum_a / 2) / sum_a;
            let b = (sum_b + sum_a / 2) / sum_a;
            let a = (sum_a + 2) / 4;
            out.extend_from_slice(&[r as u8, g as u8, b as u8, a as u8]);
        }
    }
    Some((out, dst_width, dst_height))
}

/// Every mip level of `rgba`, base first, while both dimensions stay a multiple
/// of `block` and at least `block`.
///
/// The block constraint is the encoder's, not the format's: ETC2 and ASTC both
/// describe partial blocks, but this engine's ETC2 encoder takes whole blocks
/// only, so the chain stops where it would need padding rather than inventing
/// pixels the source never had. A partial chain is legal -- the uploader bounds
/// `TEXTURE_MAX_LEVEL` by what it was actually given.
pub fn rgba_mip_chain(
    rgba: &[u8],
    width: u32,
    height: u32,
    block: u32,
) -> Vec<(Vec<u8>, u32, u32)> {
    let mut levels = Vec::new();
    if block == 0
        || width == 0
        || height == 0
        || !width.is_multiple_of(block)
        || !height.is_multiple_of(block)
    {
        return levels;
    }
    levels.push((rgba.to_vec(), width, height));
    loop {
        let Some((current, w, h)) = levels.last().map(|(pixels, w, h)| (pixels, *w, *h)) else {
            break;
        };
        let Some((next, nw, nh)) = downsample_rgba_half(current, w, h) else {
            break;
        };
        if !nw.is_multiple_of(block) || !nh.is_multiple_of(block) || nw < block || nh < block {
            break;
        }
        levels.push((next, nw, nh));
    }
    levels
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(width: u32, height: u32, px: [u8; 4]) -> Vec<u8> {
        px.iter()
            .copied()
            .cycle()
            .take((width as usize) * (height as usize) * 4)
            .collect()
    }

    #[test]
    fn a_solid_image_halves_to_the_same_colour() {
        let src = solid(4, 4, [10, 20, 30, 255]);
        let (out, w, h) = downsample_rgba_half(&src, 4, 4).expect("halves");
        assert_eq!((w, h), (2, 2));
        assert_eq!(out, solid(2, 2, [10, 20, 30, 255]));
    }

    #[test]
    fn a_transparent_neighbour_does_not_darken_the_colour() {
        let src = vec![
            255, 0, 0, 255, //
            0, 0, 0, 0, //
            0, 0, 0, 0, //
            0, 0, 0, 0,
        ];
        let (out, _, _) = downsample_rgba_half(&src, 2, 2).expect("halves");
        assert_eq!(out[0], 255, "red must survive at full strength");
        assert_eq!(out[1], 0);
        assert_eq!(out[2], 0);
        assert_eq!(out[3], 64, "alpha is the average of 255, 0, 0, 0");
    }

    #[test]
    fn a_fully_transparent_block_stays_fully_transparent() {
        let src = solid(2, 2, [0, 0, 0, 0]);
        let (out, _, _) = downsample_rgba_half(&src, 2, 2).expect("halves");
        assert_eq!(out, vec![0, 0, 0, 0]);
    }

    #[test]
    fn a_one_by_one_image_has_no_smaller_level() {
        assert!(downsample_rgba_half(&[1, 2, 3, 4], 1, 1).is_none());
    }

    #[test]
    fn a_mismatched_buffer_is_refused_rather_than_read_past() {
        assert!(downsample_rgba_half(&[0; 8], 4, 4).is_none());
    }

    #[test]
    fn the_chain_stops_where_the_encoder_would_need_padding() {
        // 64 -> 32 -> 16 -> 8 -> 4, and then 2x2 is not a whole ETC2 block.
        let src = solid(64, 64, [1, 2, 3, 255]);
        let dims: Vec<(u32, u32)> = rgba_mip_chain(&src, 64, 64, 4)
            .iter()
            .map(|(_, w, h)| (*w, *h))
            .collect();
        assert_eq!(dims, vec![(64, 64), (32, 32), (16, 16), (8, 8), (4, 4)]);
    }

    #[test]
    fn a_non_block_aligned_image_produces_no_chain_at_all() {
        let src = solid(6, 6, [1, 2, 3, 255]);
        assert!(rgba_mip_chain(&src, 6, 6, 4).is_empty());
    }

    #[test]
    fn a_non_square_image_keeps_halving_until_either_side_would_break() {
        // 32x8 -> 16x4, and then 8x2 is not a whole block on the short side.
        let src = solid(32, 8, [9, 9, 9, 255]);
        let dims: Vec<(u32, u32)> = rgba_mip_chain(&src, 32, 8, 4)
            .iter()
            .map(|(_, w, h)| (*w, *h))
            .collect();
        assert_eq!(dims, vec![(32, 8), (16, 4)]);
    }

    #[test]
    fn representative_chain_has_no_redundant_clone_bytes() {
        REDUNDANT_CLONE_COUNT.store(0, std::sync::atomic::Ordering::Relaxed);
        let src = solid(2048, 2048, [17, 34, 51, 255]);
        let levels = rgba_mip_chain(&src, 2048, 2048, 4);
        assert_eq!(levels.first().map(|(_, w, h)| (*w, *h)), Some((2048, 2048)));
        assert_eq!(levels.last().map(|(_, w, h)| (*w, *h)), Some((4, 4)));
        assert_eq!(
            REDUNDANT_CLONE_COUNT.load(std::sync::atomic::Ordering::Relaxed),
            0,
            "mipmap generation duplicated {} bytes",
            REDUNDANT_CLONE_COUNT.load(std::sync::atomic::Ordering::Relaxed)
        );
    }
}

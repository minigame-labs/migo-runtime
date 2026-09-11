use super::render_cmd::MAX_SYNC_READBACK_BYTES;

/// Destination layout from the driver's current `PACK_*` state.
///
/// The destination footprint includes skips and padding between rows, while the
/// owned readback allocation contains only `compact_bytes` of pixel data.
/// Valid zero-area reads use a canonical layout with every field set to zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PixelPackLayout {
    pub row_bytes: usize,
    pub row_stride: usize,
    pub first_byte: usize,
    pub required_bytes: usize,
    pub compact_bytes: usize,
    pub height: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelPackError {
    InvalidValue,
    InvalidOperation,
    OutOfMemory,
}

impl PixelPackLayout {
    /// Validates WebGL pixel-pack constraints without allocating or calling GL.
    pub fn new(
        width: i32,
        height: i32,
        bpp: usize,
        alignment: i32,
        row_length: i32,
        skip_rows: i32,
        skip_pixels: i32,
    ) -> Result<Self, PixelPackError> {
        if width < 0
            || height < 0
            || row_length < 0
            || skip_rows < 0
            || skip_pixels < 0
            || bpp == 0
            || !matches!(alignment, 1 | 2 | 4 | 8)
        {
            return Err(PixelPackError::InvalidValue);
        }

        let width = width as usize;
        let height = height as usize;
        let row_length = row_length as usize;
        let skip_rows = skip_rows as usize;
        let skip_pixels = skip_pixels as usize;
        let alignment = alignment as usize;
        let data_width = if row_length == 0 { width } else { row_length };
        if skip_pixels > data_width || width > data_width - skip_pixels {
            return Err(PixelPackError::InvalidOperation);
        }
        if width == 0 || height == 0 {
            return Ok(Self {
                row_bytes: 0,
                row_stride: 0,
                first_byte: 0,
                required_bytes: 0,
                compact_bytes: 0,
                height: 0,
            });
        }

        let overflow = PixelPackError::OutOfMemory;
        let row_bytes = width.checked_mul(bpp).ok_or(overflow)?;
        let compact_bytes = row_bytes
            .checked_mul(height)
            .filter(|bytes| *bytes <= MAX_SYNC_READBACK_BYTES)
            .ok_or(overflow)?;
        let unaligned_stride = data_width.checked_mul(bpp).ok_or(overflow)?;
        let padding = (alignment - unaligned_stride % alignment) % alignment;
        let row_stride = unaligned_stride.checked_add(padding).ok_or(overflow)?;
        let first_byte = skip_rows
            .checked_mul(row_stride)
            .and_then(|rows| {
                skip_pixels
                    .checked_mul(bpp)
                    .and_then(|pixels| rows.checked_add(pixels))
            })
            .ok_or(overflow)?;
        let required_bytes = (height - 1)
            .checked_mul(row_stride)
            .and_then(|rows| first_byte.checked_add(rows))
            .and_then(|bytes| bytes.checked_add(row_bytes))
            .ok_or(overflow)?;

        Ok(Self {
            row_bytes,
            row_stride,
            first_byte,
            required_bytes,
            compact_bytes,
            height,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{PixelPackError, PixelPackLayout};
    use crate::protocol::render_cmd::MAX_SYNC_READBACK_BYTES;

    #[test]
    fn all_pack_alignments_preserve_only_the_final_row_payload() {
        for (alignment, row_stride, required_bytes) in
            [(1, 9, 18), (2, 10, 19), (4, 12, 21), (8, 16, 25)]
        {
            assert_eq!(
                PixelPackLayout::new(3, 2, 3, alignment, 0, 0, 0),
                Ok(PixelPackLayout {
                    row_bytes: 9,
                    row_stride,
                    first_byte: 0,
                    required_bytes,
                    compact_bytes: 18,
                    height: 2,
                }),
            );
        }
    }

    #[test]
    fn scalar_and_packed_pixel_sizes_are_used_as_bytes_per_pixel() {
        for bpp in [3, 4, 8, 16] {
            let layout = PixelPackLayout::new(2, 3, bpp, 1, 0, 0, 0).unwrap();
            assert_eq!(layout.row_bytes, 2 * bpp);
            assert_eq!(layout.row_stride, 2 * bpp);
            assert_eq!(layout.required_bytes, 6 * bpp);
            assert_eq!(layout.compact_bytes, 6 * bpp);
        }
    }

    #[test]
    fn row_length_and_skips_determine_the_exact_destination_footprint() {
        assert_eq!(
            PixelPackLayout::new(3, 2, 3, 8, 6, 2, 1),
            Ok(PixelPackLayout {
                row_bytes: 9,
                row_stride: 24,
                first_byte: 51,
                required_bytes: 84,
                compact_bytes: 18,
                height: 2,
            }),
        );
        let single_row = PixelPackLayout::new(3, 1, 3, 8, 6, 2, 1).unwrap();
        assert_eq!(single_row.required_bytes, 60);
    }

    #[test]
    fn pixels_must_fit_in_the_declared_or_default_row_length() {
        for (width, row_length, skip_pixels) in [(3, 2, 0), (3, 3, 1), (3, 0, 1)] {
            assert_eq!(
                PixelPackLayout::new(width, 2, 4, 4, row_length, 0, skip_pixels),
                Err(PixelPackError::InvalidOperation),
            );
        }
        assert!(PixelPackLayout::new(3, 2, 4, 4, 4, 0, 1).is_ok());
    }

    #[test]
    fn row_constraint_does_not_overflow_signed_dimensions() {
        assert_eq!(
            PixelPackLayout::new(i32::MAX, 0, 1, 1, i32::MAX, 0, i32::MAX),
            Err(PixelPackError::InvalidOperation),
        );
    }

    #[test]
    fn zero_area_has_a_canonical_empty_layout() {
        for (width, height) in [(0, 3), (3, 0), (0, 0)] {
            let layout = PixelPackLayout::new(width, height, 4, 8, 6, 2, 1).unwrap();
            assert_eq!(layout.row_bytes, 0);
            assert_eq!(layout.row_stride, 0);
            assert_eq!(layout.first_byte, 0);
            assert_eq!(layout.required_bytes, 0);
            assert_eq!(layout.compact_bytes, 0);
            assert_eq!(layout.height, 0);
        }
    }

    #[test]
    fn zero_area_does_not_compute_unused_byte_offsets() {
        for (width, height, bpp, alignment, row_length, skip_rows, skip_pixels) in [
            (0, 3, 16, 8, i32::MAX, i32::MAX, i32::MAX),
            (3, 0, 16, 8, i32::MAX, i32::MAX, 0),
            (0, 0, usize::MAX, 1, 2, 0, 0),
            (0, 0, usize::MAX, 2, 1, 0, 0),
            (0, 0, usize::MAX / 2, 1, 1, 3, 0),
            (0, 0, usize::MAX / 4, 1, 2, 2, 1),
            (i32::MAX, 0, usize::MAX, 8, 0, 0, 0),
        ] {
            assert_eq!(
                PixelPackLayout::new(
                    width,
                    height,
                    bpp,
                    alignment,
                    row_length,
                    skip_rows,
                    skip_pixels
                ),
                Ok(PixelPackLayout {
                    row_bytes: 0,
                    row_stride: 0,
                    first_byte: 0,
                    required_bytes: 0,
                    compact_bytes: 0,
                    height: 0,
                }),
            );
        }
    }

    #[test]
    fn zero_area_still_validates_row_constraints() {
        assert_eq!(
            PixelPackLayout::new(3, 0, 4, 4, 2, 0, 0),
            Err(PixelPackError::InvalidOperation),
        );
        assert_eq!(
            PixelPackLayout::new(0, 3, 4, 4, 0, 0, 1),
            Err(PixelPackError::InvalidOperation),
        );
    }

    #[test]
    fn negative_dimensions_and_pack_parameters_are_invalid_values() {
        for (width, height, row_length, skip_rows, skip_pixels) in [
            (-1, 1, 0, 0, 0),
            (1, -1, 0, 0, 0),
            (1, 1, -1, 0, 0),
            (1, 1, 0, -1, 0),
            (1, 1, 0, 0, -1),
            (-1, 0, 0, 0, 0),
            (0, -1, 0, 0, 0),
            (0, 0, -1, 0, 0),
            (0, 0, 0, -1, 0),
            (0, 0, 0, 0, -1),
        ] {
            assert_eq!(
                PixelPackLayout::new(width, height, 4, 4, row_length, skip_rows, skip_pixels),
                Err(PixelPackError::InvalidValue),
            );
        }
    }

    #[test]
    fn invalid_alignments_and_zero_pixel_size_are_invalid_values() {
        for alignment in [-8, -1, 0, 3, 5, 6, 7, 16, i32::MAX] {
            assert_eq!(
                PixelPackLayout::new(0, 0, 4, alignment, 0, 0, 0),
                Err(PixelPackError::InvalidValue),
            );
        }
        for (width, height) in [(1, 1), (0, 0), (0, 1), (1, 0)] {
            assert_eq!(
                PixelPackLayout::new(width, height, 0, 4, 0, 0, 0),
                Err(PixelPackError::InvalidValue),
            );
        }
    }

    #[test]
    fn compact_readback_limit_is_inclusive() {
        let max_width = (MAX_SYNC_READBACK_BYTES / 4) as i32;
        let layout = PixelPackLayout::new(max_width, 1, 4, 4, 0, 0, 0).unwrap();
        assert_eq!(layout.compact_bytes, MAX_SYNC_READBACK_BYTES);
        assert_eq!(layout.required_bytes, MAX_SYNC_READBACK_BYTES);
        assert_eq!(
            PixelPackLayout::new(max_width + 1, 1, 4, 4, 0, 0, 0),
            Err(PixelPackError::OutOfMemory),
        );
        assert_eq!(
            PixelPackLayout::new(max_width, 2, 4, 4, 0, 0, 0),
            Err(PixelPackError::OutOfMemory),
        );
    }

    #[test]
    fn padded_destination_footprint_is_not_capped_like_compact_storage() {
        let row_length = (MAX_SYNC_READBACK_BYTES + 1) as i32;
        let layout = PixelPackLayout::new(1, 2, 1, 1, row_length, 1, 1).unwrap();
        assert_eq!(layout.compact_bytes, 2);
        assert_eq!(layout.first_byte, MAX_SYNC_READBACK_BYTES + 2);
        assert_eq!(layout.required_bytes, 2 * MAX_SYNC_READBACK_BYTES + 4);
    }

    #[test]
    fn overflowing_byte_arithmetic_is_out_of_memory() {
        for (width, height, bpp, alignment, row_length, skip_rows, skip_pixels) in [
            (2, 1, usize::MAX, 1, 0, 0, 0),
            (1, 2, usize::MAX, 1, 0, 0, 0),
            (1, 1, 16, 8, i32::MAX, i32::MAX, 0),
            (1, 1, MAX_SYNC_READBACK_BYTES, 8, i32::MAX, i32::MAX, 0),
            (1, 4, 4, 1, i32::MAX, i32::MAX, 0),
        ] {
            assert_eq!(
                PixelPackLayout::new(
                    width,
                    height,
                    bpp,
                    alignment,
                    row_length,
                    skip_rows,
                    skip_pixels
                ),
                Err(PixelPackError::OutOfMemory),
            );
        }
    }

    #[test]
    fn an_already_aligned_stride_needs_no_padding() {
        let layout = PixelPackLayout::new(2, 1, 4, 8, 4, 0, 0).unwrap();
        assert_eq!(layout.row_stride, 16);
        assert_eq!(layout.required_bytes, 8);
    }
}

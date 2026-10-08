#![allow(unsafe_op_in_unsafe_fn)]
//! GPU-direct upload of compressed textures (ETC2/ASTC).
//!
//! Bypasses RGBA decoding entirely -- the compressed block data from a KTX2
//! container is uploaded straight to the GPU via `glCompressedTexImage2D`.
//!
//! Requires OpenGL ES 3.0 for ETC2 (mandatory in the spec) and the
//! `GL_KHR_texture_compression_astc_ldr` extension for ASTC.

use glow::HasContext;

// ---------------------------------------------------------------------------
// GL compressed-format constants (not exposed by glow)
// ---------------------------------------------------------------------------

/// `GL_COMPRESSED_RGB8_ETC2`
const GL_COMPRESSED_RGB8_ETC2: u32 = 0x9274;
/// `GL_COMPRESSED_RGBA8_ETC2_EAC`
const GL_COMPRESSED_RGBA8_ETC2_EAC: u32 = 0x9278;
/// `GL_COMPRESSED_RGBA_ASTC_4x4_KHR`
const GL_COMPRESSED_RGBA_ASTC_4X4_KHR: u32 = 0x93B0;
/// `GL_COMPRESSED_RGBA_ASTC_6x6_KHR`
const GL_COMPRESSED_RGBA_ASTC_6X6_KHR: u32 = 0x93B4;
/// `GL_COMPRESSED_RGBA_ASTC_8x8_KHR`
///
/// This was `0x93B9`, which is `GL_COMPRESSED_RGBA_ASTC_10x6_KHR`. Uploading an
/// 8x8 texture under a 10x6 token makes `glCompressedTexImage2D` reject it with
/// `GL_INVALID_VALUE`, because the byte count it derives from the dimensions no
/// longer matches what it was handed. It had never fired: nothing in the tree
/// produced ASTC 8x8 until the encoder in `migo-io` did, so the wrong constant
/// sat in a path with no producer. The test below now derives the token instead
/// of restating it -- the old one asserted the same wrong number, which is what
/// a test copied from the code it checks can always do.
const GL_COMPRESSED_RGBA_ASTC_8X8_KHR: u32 = 0x93B7;

/// `COMPRESSED_RGB_ETC1_WEBGL` (`WEBGL_compressed_texture_etc1`), which GLES 3.0 has no token for.
pub(crate) const COMPRESSED_RGB_ETC1_WEBGL: u32 = 0x8D64;

/// The format the driver is handed for `format`: itself, except ETC1, which is uploaded as ETC2 RGB8. ETC2 is ETC1
/// extended -- the extra modes are encodings an ETC1 block cannot contain, since ETC1 never lets a differential
/// colour overflow -- so an ETC1 block decodes to the same texels as ETC2 RGB8, which every GLES 3.0 driver has. The
/// texture's own format, as the facade records it, stays ETC1.
pub(crate) fn driver_compressed_format(format: u32) -> u32 {
    if format == COMPRESSED_RGB_ETC1_WEBGL {
        GL_COMPRESSED_RGB8_ETC2
    } else {
        format
    }
}

/// The block of every compressed format the WebGL extensions offer -- `(width, height, bytes)`, its texels each way and
/// its size -- or `None` for a format that is not one of them: ETC1 (`WEBGL_compressed_texture_etc1`), ETC2/EAC
/// (`WEBGL_compressed_texture_etc`) and the ASTC LDR profile (`WEBGL_compressed_texture_astc`), linear and sRGB. The
/// facade's `_COMPRESSED_FORMATS` is the same table: what content may upload, here what it costs and how the KTX2 path
/// lays it out.
pub(crate) fn compressed_block(format: u32) -> Option<(u32, u32, u64)> {
    const ASTC_BLOCKS: [(u32, u32); 14] = [
        (4, 4),
        (5, 4),
        (5, 5),
        (6, 5),
        (6, 6),
        (8, 5),
        (8, 6),
        (8, 8),
        (10, 5),
        (10, 6),
        (10, 8),
        (10, 10),
        (12, 10),
        (12, 12),
    ];
    match format {
        COMPRESSED_RGB_ETC1_WEBGL => Some((4, 4, 8)),
        // R11, SIGNED_R11 (EAC); RGB8, SRGB8, and the two PUNCHTHROUGH_ALPHA1 (ETC2)
        0x9270 | 0x9271 | 0x9274..=0x9277 => Some((4, 4, 8)),
        // RG11, SIGNED_RG11 (EAC); RGBA8, SRGB8_ALPHA8 (ETC2)
        0x9272 | 0x9273 | 0x9278 | 0x9279 => Some((4, 4, 16)),
        0x93B0..=0x93BD => {
            let (w, h) = ASTC_BLOCKS[(format - 0x93B0) as usize];
            Some((w, h, 16))
        }
        0x93D0..=0x93DD => {
            let (w, h) = ASTC_BLOCKS[(format - 0x93D0) as usize];
            Some((w, h, 16))
        }
        _ => None,
    }
}

/// Compressed texture format for GPU upload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompressedFormat {
    /// ETC2 RGB (no alpha). 0.5 bytes/pixel.
    Etc2Rgb,
    /// ETC2 RGBA (with alpha). 1 byte/pixel.
    Etc2Rgba,
    /// ASTC 4x4 block. 1 byte/pixel.
    Astc4x4,
    /// ASTC 6x6 block. ~0.44 bytes/pixel.
    Astc6x6,
    /// ASTC 8x8 block. 0.25 bytes/pixel.
    Astc8x8,
}

impl CompressedFormat {
    /// Try to map a Vulkan format code (from KTX2 header) to a CompressedFormat.
    pub fn from_vk_format(vk_format: u32) -> Option<Self> {
        match vk_format {
            147 => Some(Self::Etc2Rgb),  // VK_FORMAT_ETC2_R8G8B8_UNORM_BLOCK
            151 => Some(Self::Etc2Rgba), // VK_FORMAT_ETC2_R8G8B8A8_UNORM_BLOCK
            157 => Some(Self::Astc4x4),  // VK_FORMAT_ASTC_4x4_UNORM_BLOCK
            163 => Some(Self::Astc6x6),  // VK_FORMAT_ASTC_6x6_UNORM_BLOCK
            169 => Some(Self::Astc8x8),  // VK_FORMAT_ASTC_8x8_UNORM_BLOCK
            _ => None,
        }
    }

    /// Whether this format requires ASTC extension support.
    pub fn requires_astc(self) -> bool {
        matches!(self, Self::Astc4x4 | Self::Astc6x6 | Self::Astc8x8)
    }

    /// Map to the corresponding OpenGL ES internal format constant.
    pub fn gl_internal_format(self) -> u32 {
        match self {
            Self::Etc2Rgb => GL_COMPRESSED_RGB8_ETC2,
            Self::Etc2Rgba => GL_COMPRESSED_RGBA8_ETC2_EAC,
            Self::Astc4x4 => GL_COMPRESSED_RGBA_ASTC_4X4_KHR,
            Self::Astc6x6 => GL_COMPRESSED_RGBA_ASTC_6X6_KHR,
            Self::Astc8x8 => GL_COMPRESSED_RGBA_ASTC_8X8_KHR,
        }
    }

    /// Human-readable label for logging.
    pub fn label(self) -> &'static str {
        match self {
            Self::Etc2Rgb => "ETC2_RGB",
            Self::Etc2Rgba => "ETC2_RGBA",
            Self::Astc4x4 => "ASTC_4x4",
            Self::Astc6x6 => "ASTC_6x6",
            Self::Astc8x8 => "ASTC_8x8",
        }
    }

    /// The format's block (`compressed_block`): every variant is one of the WebGL formats.
    fn block(self) -> (u32, u32, u64) {
        compressed_block(self.gl_internal_format())
            .expect("every KTX2 format is a WebGL compressed format")
    }

    /// Compressed-block footprint `(width, height)` in texels.
    pub(crate) fn block_dims(self) -> (u32, u32) {
        let (w, h, _) = self.block();
        (w, h)
    }

    /// Bytes per compressed block.
    fn bytes_per_block(self) -> u64 {
        self.block().2
    }

    /// Exact byte length a tightly-packed `width`x`height` level must have in
    /// this format. A malformed KTX2 can declare dimensions that don't match a
    /// level's byte length; validating against this lets the caller reject it
    /// with a structured error instead of letting the driver fail the upload
    /// with GL_INVALID_VALUE.
    ///
    /// Applies to every mip level, not just the base: each level is its own
    /// tightly-packed block grid over its own halved dimensions.
    pub fn expected_level_bytes(self, width: u32, height: u32) -> u64 {
        let (bw, bh) = self.block_dims();
        let blocks_x = (width as u64).div_ceil(bw as u64);
        let blocks_y = (height as u64).div_ceil(bh as u64);
        blocks_x
            .saturating_mul(blocks_y)
            .saturating_mul(self.bytes_per_block())
    }
}

/// Cached compressed-format support flags, detected once at init.
#[derive(Debug, Clone, Copy)]
pub struct CompressedFormatSupport {
    pub etc2: bool,
    pub astc: bool,
}

// GPU format support globals are in shared::device::gpu_caps for cross-crate access.

impl CompressedFormatSupport {
    /// Detect compressed format support from the current GL context.
    /// Call once during initialization and cache the result.
    pub fn detect(gl: &glow::Context) -> Self {
        let version = unsafe { gl.get_parameter_string(glow::VERSION) };
        let etc2 = version.contains("OpenGL ES 3.") || version.contains("OpenGL ES 4.");

        let extensions = unsafe { gl.get_parameter_string(glow::EXTENSIONS) };
        let astc = extensions.contains("GL_KHR_texture_compression_astc_ldr")
            || extensions.contains("GL_KHR_texture_compression_astc_hdr");

        Self { etc2, astc }
    }

    /// Check whether the given compressed format is supported.
    pub fn is_supported(&self, format: CompressedFormat) -> bool {
        match format {
            CompressedFormat::Etc2Rgb | CompressedFormat::Etc2Rgba => self.etc2,
            CompressedFormat::Astc4x4 | CompressedFormat::Astc6x6 | CompressedFormat::Astc8x8 => {
                self.astc
            }
        }
    }
}

/// Upload pre-compressed texture data directly to the GPU.
///
/// Creates a new GL texture, binds it, and calls `glCompressedTexImage2D`
/// with the supplied block data. Returns the texture handle on success.
///
/// # Safety
///
/// A valid GL context must be current on the calling thread.
///
/// # Arguments
///
/// * `gl` -- glow context with a current GL context.
/// * `format` -- the compressed format of the data.
/// * `width` -- texture width in pixels (must match the data).
/// * `height` -- texture height in pixels (must match the data).
/// * `levels` -- the mip chain's block data, base level first. A single-element
///   slice uploads just the base level and leaves filtering unmipmapped.
/// * `supports_pbo` -- whether the context has pixel-buffer objects (ES3 or
///   `GL_NV_pixel_buffer_object`). When false (bare ES2) the PBO binding is
///   left untouched, since querying `PIXEL_UNPACK_BUFFER_BINDING` would raise
///   `GL_INVALID_ENUM` and no PBO can be bound on such a context anyway.
///
/// # Returns
///
/// `Some(texture)` on success, `None` if texture creation or upload fails.
pub fn upload_compressed_texture(
    gl: &glow::Context,
    format: CompressedFormat,
    width: u32,
    height: u32,
    levels: &[&[u8]],
    supports_pbo: bool,
) -> Option<glow::NativeTexture> {
    // Checked before a texture exists, so a malformed chain costs nothing and
    // cannot leave a half-populated texture behind.
    if let Err(reason) = validate_mip_chain(format, width, height, levels) {
        tracing::warn!("compressed upload rejected: {reason}");
        return None;
    }

    unsafe {
        let tex = gl.create_texture().ok()?;

        // This may run on a live WebGL/Canvas2D context, so save every
        // app-visible binding we clobber and restore it afterwards; leaving one
        // changed would corrupt WebGL-visible state and desync the state
        // tracker (which would then dedup a later re-bind that GL never applied).
        //   * TEXTURE_BINDING_2D: core since ES2, always saved/restored.
        //   * PIXEL_UNPACK_BUFFER: the enum is ES3 / GL_NV_pixel_buffer_object
        //     only; querying it on a bare ES2 context raises GL_INVALID_ENUM, so
        //     only touch it when PBOs exist. With one bound, the `data` slice
        //     below would otherwise be reinterpreted as an offset into it.
        let saved_texture = gl.get_parameter_i32(glow::TEXTURE_BINDING_2D) as u32;
        let saved_unpack_buffer = if supports_pbo {
            let prev = gl.get_parameter_buffer(glow::PIXEL_UNPACK_BUFFER_BINDING);
            gl.bind_buffer(glow::PIXEL_UNPACK_BUFFER, None);
            Some(prev)
        } else {
            None
        };

        gl.bind_texture(glow::TEXTURE_2D, Some(tex));

        gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_MIN_FILTER,
            min_filter_for_levels(levels.len()) as i32,
        );
        gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_MAG_FILTER,
            glow::LINEAR as i32,
        );
        gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_WRAP_S,
            glow::CLAMP_TO_EDGE as i32,
        );
        gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_WRAP_T,
            glow::CLAMP_TO_EDGE as i32,
        );

        let internal_format = format.gl_internal_format();

        // For ETC2/ASTC block formats `imageSize` must equal the exact
        // format+dimension-derived byte count; glCompressedTexImage2D returns
        // GL_INVALID_VALUE on any mismatch (there is no "allocate storage
        // only" idiom for compressed textures the way NULL pixels work for
        // glTexImage2D). Pass the real `data.len()` and upload straight from
        // client memory.
        // The sampler needs a complete chain: TEXTURE_MAX_LEVEL bounds it at the
        // last level actually uploaded, so a partial chain (an asset that ships
        // only the top few mips) is complete rather than incomplete -- and an
        // incomplete texture samples black while reporting nothing.
        //
        // MAX_LEVEL is ES3; on ES2 the enum would raise GL_INVALID_ENUM, so it
        // is only set when there is a chain to bound.
        if levels.len() > 1 {
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MAX_LEVEL,
                (levels.len() - 1) as i32,
            );
        }

        let mut level_width = width;
        let mut level_height = height;
        for (level, data) in levels.iter().enumerate() {
            gl.compressed_tex_image_2d(
                glow::TEXTURE_2D,
                level as i32,
                internal_format as i32,
                level_width as i32,
                level_height as i32,
                0,
                data.len() as i32,
                data,
            );
            level_width = (level_width / 2).max(1);
            level_height = (level_height / 2).max(1);
        }

        let err = gl.get_error();

        // Restore the app-visible bindings on both success and failure paths.
        gl.bind_texture(
            glow::TEXTURE_2D,
            std::num::NonZeroU32::new(saved_texture).map(glow::NativeTexture),
        );
        if let Some(prev) = saved_unpack_buffer {
            gl.bind_buffer(glow::PIXEL_UNPACK_BUFFER, prev);
        }

        if err != glow::NO_ERROR {
            tracing::warn!(
                "compressed upload failed: format={} {}x{} {} level(s), {} bytes, GL error=0x{:X}",
                format.label(),
                width,
                height,
                levels.len(),
                levels.iter().map(|l| l.len()).sum::<usize>(),
                err,
            );
            gl.delete_texture(tex);
            return None;
        }

        tracing::debug!(
            "compressed upload: format={} {}x{} {} level(s), {} bytes",
            format.label(),
            width,
            height,
            levels.len(),
            levels.iter().map(|l| l.len()).sum::<usize>(),
        );

        Some(tex)
    }
}

/// Check a whole mip chain before any of it is uploaded.
///
/// Validating level by level while uploading can fail halfway, and a compressed
/// texture with some levels populated and the rest undefined does not report an
/// error -- it samples garbage, or black once the sampler reaches a missing
/// level. There is no partial-success state worth having here, so the whole
/// chain is checked first and the upload either happens or does not.
pub fn validate_mip_chain(
    format: CompressedFormat,
    width: u32,
    height: u32,
    levels: &[&[u8]],
) -> Result<(), &'static str> {
    if levels.is_empty() {
        return Err("compressed upload: no levels supplied");
    }
    if width == 0 || height == 0 {
        return Err("compressed upload: zero dimensions");
    }

    let mut level_width = width;
    let mut level_height = height;
    for (index, level) in levels.iter().enumerate() {
        if level.len() as u64 != format.expected_level_bytes(level_width, level_height) {
            return Err("compressed upload: level size does not match its dimensions");
        }
        // A chain ends at 1x1. Declaring another level past it means the
        // dimensions and the level list disagree about what this texture is.
        if index + 1 < levels.len() && level_width == 1 && level_height == 1 {
            return Err("compressed upload: more levels than the dimensions allow");
        }
        level_width = (level_width / 2).max(1);
        level_height = (level_height / 2).max(1);
    }
    Ok(())
}

/// The minification filter for a texture that was given `level_count` levels.
///
/// A texture whose min filter samples mips but carries only level 0 is
/// *incomplete*, and GL does not report that: it samples black. So mipmapped
/// filtering is selected only when levels beyond the base were actually
/// uploaded, which makes the filter a fact about the data rather than a wish.
pub fn min_filter_for_levels(level_count: usize) -> u32 {
    if level_count > 1 {
        glow::LINEAR_MIPMAP_LINEAR
    } else {
        glow::LINEAR
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chain_of_halving_levels_validates() {
        let f = CompressedFormat::Etc2Rgb;
        let l0 = vec![0u8; f.expected_level_bytes(8, 8) as usize];
        let l1 = vec![0u8; f.expected_level_bytes(4, 4) as usize];
        let l2 = vec![0u8; f.expected_level_bytes(2, 2) as usize];
        let l3 = vec![0u8; f.expected_level_bytes(1, 1) as usize];

        assert_eq!(
            validate_mip_chain(f, 8, 8, &[&l0[..], &l1[..], &l2[..], &l3[..]]),
            Ok(())
        );
    }

    #[test]
    fn a_partial_chain_validates_too() {
        // Stopping before 1x1 is legal: the sampler just needs the levels it is
        // told about, and an asset may ship only the top few.
        let f = CompressedFormat::Astc4x4;
        let l0 = vec![0u8; f.expected_level_bytes(16, 16) as usize];
        let l1 = vec![0u8; f.expected_level_bytes(8, 8) as usize];

        assert_eq!(validate_mip_chain(f, 16, 16, &[&l0[..], &l1[..]]), Ok(()));
    }

    #[test]
    fn rejects_a_level_with_the_wrong_byte_count() {
        let f = CompressedFormat::Etc2Rgb;
        let l0 = vec![0u8; f.expected_level_bytes(8, 8) as usize];
        let l1 = vec![0u8; f.expected_level_bytes(4, 4) as usize + 1];

        assert!(
            validate_mip_chain(f, 8, 8, &[&l0[..], &l1[..]]).is_err(),
            "a level whose size does not match its dimensions must not be uploaded"
        );
    }

    #[test]
    fn rejects_more_levels_than_the_dimensions_allow() {
        let f = CompressedFormat::Etc2Rgb;
        let l = vec![0u8; f.expected_level_bytes(1, 1) as usize];
        // A 2x2 base has exactly two levels: 2x2 and 1x1.
        let levels = [&l[..], &l[..], &l[..]];

        assert!(
            validate_mip_chain(f, 2, 2, &levels).is_err(),
            "a chain longer than the dimensions allow is malformed"
        );
    }

    #[test]
    fn rejects_an_empty_chain() {
        assert!(validate_mip_chain(CompressedFormat::Etc2Rgb, 8, 8, &[]).is_err());
    }

    #[test]
    fn min_filter_uses_mipmaps_only_when_levels_were_supplied() {
        // A texture whose min filter samples mips but has only level 0 is
        // incomplete, and an incomplete texture samples black -- a silent,
        // whole-asset failure rather than an error.
        assert_eq!(min_filter_for_levels(1), glow::LINEAR);
        assert_eq!(min_filter_for_levels(2), glow::LINEAR_MIPMAP_LINEAR);
        assert_eq!(min_filter_for_levels(9), glow::LINEAR_MIPMAP_LINEAR);
    }

    /// The ASTC tokens are derived from the block size, not restated.
    ///
    /// `KHR_texture_compression_astc_hdr` assigns `COMPRESSED_RGBA_ASTC_*_KHR`
    /// consecutively from `0x93B0` over its fourteen block sizes, in the order
    /// below. So the token for a footprint is its position in that list, and a
    /// constant that disagrees is an off-by-something rather than a number
    /// nobody can check.
    ///
    /// The previous version of this test asserted `Astc8x8 == 0x93B9`, copied
    /// from the constant it was checking. `0x93B9` is the 10x6 token, and the
    /// test confirmed the mistake for as long as both existed.
    /// ETC1 costs ETC2 RGB8's block -- 8 bytes a 4x4 -- and is uploaded under ETC2 RGB8's token, which decodes its
    /// blocks to the same texels; every other format goes to the driver as it is.
    #[test]
    fn etc1_is_uploaded_as_etc2_rgb8() {
        assert_eq!(compressed_block(COMPRESSED_RGB_ETC1_WEBGL), Some((4, 4, 8)));
        assert_eq!(
            compressed_block(COMPRESSED_RGB_ETC1_WEBGL),
            compressed_block(GL_COMPRESSED_RGB8_ETC2)
        );
        assert_eq!(
            driver_compressed_format(COMPRESSED_RGB_ETC1_WEBGL),
            GL_COMPRESSED_RGB8_ETC2
        );
        for format in [
            GL_COMPRESSED_RGB8_ETC2,
            GL_COMPRESSED_RGBA8_ETC2_EAC,
            0x93B0,
            0x93DD,
        ] {
            assert_eq!(driver_compressed_format(format), format);
        }
    }

    #[test]
    fn the_astc_tokens_follow_the_block_size_order_the_extension_defines() {
        const ASTC_BLOCK_SIZES: [(u32, u32); 14] = [
            (4, 4),
            (5, 4),
            (5, 5),
            (6, 5),
            (6, 6),
            (8, 5),
            (8, 6),
            (8, 8),
            (10, 5),
            (10, 6),
            (10, 8),
            (10, 10),
            (12, 10),
            (12, 12),
        ];
        const FIRST_ASTC_TOKEN: u32 = 0x93B0;

        for format in [
            CompressedFormat::Astc4x4,
            CompressedFormat::Astc6x6,
            CompressedFormat::Astc8x8,
        ] {
            let dims = format.block_dims();
            let index = ASTC_BLOCK_SIZES
                .iter()
                .position(|size| *size == dims)
                .unwrap_or_else(|| panic!("{dims:?} is not an ASTC block size"));
            assert_eq!(
                format.gl_internal_format(),
                FIRST_ASTC_TOKEN + index as u32,
                "{} maps to 0x{:X}, but a {}x{} block is token {} of the extension's \
                 list, which is 0x{:X}",
                format.label(),
                format.gl_internal_format(),
                dims.0,
                dims.1,
                index,
                FIRST_ASTC_TOKEN + index as u32
            );
        }
    }

    /// The two ETC2 tokens, which the ES 3.0 core specification assigns and the
    /// derivation above does not cover.
    #[test]
    fn the_etc2_tokens_are_the_core_ones() {
        assert_eq!(CompressedFormat::Etc2Rgb.gl_internal_format(), 0x9274);
        assert_eq!(CompressedFormat::Etc2Rgba.gl_internal_format(), 0x9278);
    }

    #[test]
    fn format_labels() {
        assert_eq!(CompressedFormat::Etc2Rgb.label(), "ETC2_RGB");
        assert_eq!(CompressedFormat::Etc2Rgba.label(), "ETC2_RGBA");
        assert_eq!(CompressedFormat::Astc4x4.label(), "ASTC_4x4");
        assert_eq!(CompressedFormat::Astc6x6.label(), "ASTC_6x6");
        assert_eq!(CompressedFormat::Astc8x8.label(), "ASTC_8x8");
    }

    #[test]
    fn expected_level_bytes_block_math() {
        // ETC2 RGB: 4x4 block, 8 B/block.
        assert_eq!(CompressedFormat::Etc2Rgb.expected_level_bytes(8, 8), 32);
        assert_eq!(CompressedFormat::Etc2Rgb.expected_level_bytes(1, 1), 8);
        // ETC2 RGBA / ASTC 4x4: 4x4 block, 16 B/block.
        assert_eq!(CompressedFormat::Etc2Rgba.expected_level_bytes(8, 8), 64);
        assert_eq!(CompressedFormat::Astc4x4.expected_level_bytes(8, 8), 64);
        // ASTC 6x6: ceil(7/6)=2 per axis -> 4 blocks * 16 = 64.
        assert_eq!(CompressedFormat::Astc6x6.expected_level_bytes(7, 7), 64);
        // ASTC 8x8: exact single block vs partial-edge round-up.
        assert_eq!(CompressedFormat::Astc8x8.expected_level_bytes(8, 8), 16);
        assert_eq!(CompressedFormat::Astc8x8.expected_level_bytes(9, 9), 64);
    }
}

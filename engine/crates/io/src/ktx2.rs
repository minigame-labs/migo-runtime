//! Zero-copy KTX2 container parser for compressed GPU textures.
//!
//! Parses the KTX2 fixed-size header and the level index, and hands out a
//! borrowed slice per mip level without copying any texture data.
//!
//! Only ETC2 and ASTC formats are recognized; everything else maps to
//! `VkFormat::Unknown(code)`.
//!
//! Reference: <https://registry.khronos.org/KTX/specs/2.0/ktxspec.v2.html>

/// KTX2 file identifier (12 bytes).
const KTX2_MAGIC: [u8; 12] = [
    0xAB, 0x4B, 0x54, 0x58, 0x20, 0x32, 0x30, 0xBB, 0x0D, 0x0A, 0x1A, 0x0A,
];

/// Subset of Vulkan format codes relevant to mobile compressed textures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VkFormat {
    /// `VK_FORMAT_ETC2_R8G8B8_UNORM_BLOCK` (147)
    Etc2R8G8B8UnormBlock,
    /// `VK_FORMAT_ETC2_R8G8B8A8_UNORM_BLOCK` (151)
    Etc2R8G8B8A8UnormBlock,
    /// `VK_FORMAT_ASTC_4x4_UNORM_BLOCK` (157)
    Astc4x4UnormBlock,
    /// `VK_FORMAT_ASTC_6x6_UNORM_BLOCK` (163)
    Astc6x6UnormBlock,
    /// `VK_FORMAT_ASTC_8x8_UNORM_BLOCK` (169)
    Astc8x8UnormBlock,
    /// Unrecognized Vulkan format code.
    Unknown(u32),
}

impl VkFormat {
    fn from_u32(code: u32) -> Self {
        match code {
            147 => Self::Etc2R8G8B8UnormBlock,
            151 => Self::Etc2R8G8B8A8UnormBlock,
            157 => Self::Astc4x4UnormBlock,
            163 => Self::Astc6x6UnormBlock,
            169 => Self::Astc8x8UnormBlock,
            other => Self::Unknown(other),
        }
    }
}

/// Parsed KTX2 header fields needed for GPU upload.
#[derive(Debug, Clone)]
pub struct Ktx2Header {
    pub format: VkFormat,
    pub width: u32,
    pub height: u32,
    pub mip_levels: u32,
}

/// A parsed KTX2 file with borrowed slices to the compressed level data.
///
/// Every level in the index is validated by [`parse_ktx2`] before this exists,
/// so [`Ktx2File::levels`] cannot fail and cannot hand out bytes that lie
/// outside the buffer or inside another level.
#[derive(Debug)]
pub struct Ktx2File<'a> {
    pub header: Ktx2Header,
    /// Compressed texture data for mip level 0.
    ///
    /// Kept as a field because the base level is what a caller that does not do
    /// mipmapping wants, and because it predates [`Ktx2File::levels`].
    pub data: &'a [u8],
    /// The whole container, so the level index can be read on demand rather
    /// than copied into an owned list of slices at parse time.
    file: &'a [u8],
}

impl<'a> Ktx2File<'a> {
    /// The mip levels, base level first.
    ///
    /// The KTX2 level index is ordered by level, but the level *data* is
    /// conventionally stored smallest-level-first so a reader can stream the
    /// small levels before the large ones. Offsets therefore descend while
    /// levels ascend, and nothing here may assume otherwise.
    pub fn levels(&self) -> impl Iterator<Item = &'a [u8]> + '_ {
        let file = self.file;
        (0..self.header.mip_levels as usize).map(move |level| {
            let entry = HEADER_SIZE + level * LEVEL_INDEX_ENTRY_SIZE;
            // Both reads were bounds-checked against `file` during parsing.
            let offset = read_u64(file, entry) as usize;
            let length = read_u64(file, entry + 8) as usize;
            &file[offset..offset + length]
        })
    }
}

/// KTX2 header layout (offsets from the spec, all little-endian):
///
/// | Offset | Size | Field                |
/// |--------|------|----------------------|
/// | 0      | 12   | identifier (magic)   |
/// | 12     | 4    | vkFormat             |
/// | 16     | 4    | typeSize             |
/// | 20     | 4    | pixelWidth           |
/// | 24     | 4    | pixelHeight          |
/// | 28     | 4    | pixelDepth           |
/// | 32     | 4    | layerCount           |
/// | 36     | 4    | faceCount            |
/// | 40     | 4    | levelCount           |
/// | 44     | 4    | supercompressionScheme |
///
/// After the 48-byte header comes the index section:
///
/// | 48     | 4    | dfdByteOffset        |
/// | 52     | 4    | dfdByteLength        |
/// | 56     | 4    | kvdByteOffset        |
/// | 60     | 4    | kvdByteLength        |
/// | 64     | 8    | sgdByteOffset        |
/// | 72     | 8    | sgdByteLength        |
///
/// Then the level index (one entry per mip level):
///
/// | 80 + i*24 | 8  | byteOffset  |
/// | 88 + i*24 | 8  | byteLength  |
/// | 96 + i*24 | 8  | uncompressedByteLength |
const HEADER_SIZE: usize = 80;
const LEVEL_INDEX_ENTRY_SIZE: usize = 24;

/// Read a little-endian u32 from `data` at `offset`.
fn read_u32(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ])
}

/// Read a little-endian u64 from `data` at `offset`.
fn read_u64(data: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
        data[offset + 4],
        data[offset + 5],
        data[offset + 6],
        data[offset + 7],
    ])
}

/// Parse a KTX2 container, returning the header and borrowed slices to the
/// compressed level data.
///
/// Every declared level is validated here, not lazily at upload time. A chain
/// that is checked level by level while uploading can fail halfway, leaving a
/// texture with some levels populated and the rest undefined -- which samples as
/// garbage rather than as an error. Supercompression is not supported: the data
/// must be stored uncompressed in the container.
///
/// # Errors
///
/// Returns an error string if:
/// - The magic bytes do not match.
/// - The file is truncated.
/// - Supercompression is used (we only handle `None` = 0).
/// - The level count exceeds the mip chain the dimensions can have.
/// - Any level is empty, runs past the buffer, overlaps the header or level
///   index, or overlaps another level.
pub fn parse_ktx2(data: &[u8]) -> Result<Ktx2File<'_>, &'static str> {
    // Check magic bytes.
    if data.len() < HEADER_SIZE {
        return Err("ktx2: file too short for header");
    }
    if data[..12] != KTX2_MAGIC {
        return Err("ktx2: invalid magic bytes");
    }

    let vk_format = read_u32(data, 12);
    let width = read_u32(data, 20);
    let height = read_u32(data, 24);
    let level_count = read_u32(data, 40);
    let supercompression_scheme = read_u32(data, 44);

    if width == 0 || height == 0 {
        return Err("ktx2: zero dimensions");
    }

    // We only support uncompressed-in-container (scheme 0).
    if supercompression_scheme != 0 {
        return Err("ktx2: supercompression not supported");
    }

    // Level count 0 means "auto full mip chain" per spec; a file that declares
    // it carries one stored level and expects the loader to generate the rest.
    let actual_levels = if level_count == 0 { 1 } else { level_count };

    // Verify that the whole level index fits in the buffer. `level_count`
    // is attacker-controlled, so the index span is computed with checked
    // arithmetic: a wrapped product would produce a small `level_index_end`
    // that passes the length check below while describing a span that isn't
    // there.
    let level_index_start = HEADER_SIZE;
    let level_index_end = (actual_levels as usize)
        .checked_mul(LEVEL_INDEX_ENTRY_SIZE)
        .and_then(|span| level_index_start.checked_add(span))
        .ok_or("ktx2: level index span overflow")?;
    if data.len() < level_index_end {
        return Err("ktx2: file too short for level index");
    }

    // A 2D image of these dimensions cannot have more levels than its longest
    // side has halvings; rejecting a larger count is the spec's own rule. It
    // runs after the two structural checks above rather than before them so
    // that each guard still has inputs only it rejects: a huge `level_count`
    // is caught as an index that does not fit, which is the more precise thing
    // to say about it, and the overflow guard stays reachable.
    let max_levels = u32::BITS - width.max(height).leading_zeros();
    if actual_levels > max_levels {
        return Err("ktx2: level count exceeds the mip chain for these dimensions");
    }

    // Validate every level. All comparisons happen in `u64` before narrowing:
    // on a 32-bit target `as usize` would truncate a large offset into a small
    // in-bounds one.
    let mut ranges: Vec<(u64, u64)> = Vec::with_capacity(actual_levels as usize);
    for level in 0..actual_levels as usize {
        let entry = level_index_start + level * LEVEL_INDEX_ENTRY_SIZE;
        let offset = read_u64(data, entry);
        let length = read_u64(data, entry + 8);

        if length == 0 {
            return Err("ktx2: level has zero length");
        }

        let end = offset
            .checked_add(length)
            .ok_or("ktx2: level range overflow")?;

        if end > data.len() as u64 {
            return Err("ktx2: level data out of bounds");
        }

        // Payload must start after the level index. Without this a crafted file
        // can point a level at the header or at the index itself, and those
        // bytes would be handed to the GPU as texture data.
        if offset < level_index_end as u64 {
            return Err("ktx2: level data overlaps the header or level index");
        }

        // Two levels sharing bytes is not a mip chain. It is cheap to check
        // here and impossible to notice on a GPU, where the result is a texture
        // whose lower levels are a re-reading of the higher ones.
        if ranges
            .iter()
            .any(|&(other_start, other_end)| offset < other_end && other_start < end)
        {
            return Err("ktx2: levels overlap");
        }

        ranges.push((offset, end));
    }

    let (level0_offset, level0_end) = ranges[0];

    Ok(Ktx2File {
        header: Ktx2Header {
            format: VkFormat::from_u32(vk_format),
            width,
            height,
            mip_levels: actual_levels,
        },
        data: &data[level0_offset as usize..level0_end as usize],
        file: data,
    })
}

/// Check if the given data starts with the KTX2 magic bytes.
pub fn is_ktx2(data: &[u8]) -> bool {
    data.len() >= 12 && data[..12] == KTX2_MAGIC
}

/// Write a non-supercompressed KTX2 container around a whole mip chain.
///
/// Levels are given base-first. They are *stored* smallest-first, which is the
/// layout the spec's mip streaming assumes and what other KTX2 writers emit --
/// a reader that wants only the small levels then gets them without seeking
/// past the large one. The level index stays in level order regardless, so the
/// storage order is invisible to anything but a byte-level diff.
///
/// Returns `None` for an empty chain or one longer than the dimensions allow;
/// both would produce a container [`parse_ktx2`] refuses, and failing here is
/// how the writer stays the parser's inverse rather than a separate opinion
/// about what a valid file is.
pub fn write_ktx2_levels(
    vk_format: u32,
    width: u32,
    height: u32,
    levels: &[&[u8]],
) -> Option<Vec<u8>> {
    if levels.is_empty() || width == 0 || height == 0 {
        return None;
    }
    let max_levels = u32::BITS - width.max(height).leading_zeros();
    if levels.len() as u64 > u64::from(max_levels) {
        return None;
    }
    if levels.iter().any(|level| level.is_empty()) {
        return None;
    }

    let index_size = levels.len() * LEVEL_INDEX_ENTRY_SIZE;
    let mut buf = vec![0u8; HEADER_SIZE + index_size];

    buf[..12].copy_from_slice(&KTX2_MAGIC);
    buf[12..16].copy_from_slice(&vk_format.to_le_bytes());
    // typeSize is 1 for every block-compressed format.
    buf[16..20].copy_from_slice(&1u32.to_le_bytes());
    buf[20..24].copy_from_slice(&width.to_le_bytes());
    buf[24..28].copy_from_slice(&height.to_le_bytes());
    // pixelDepth 0 = 2D, layerCount 0 = non-array, faceCount 1 = not a cubemap.
    buf[28..32].copy_from_slice(&0u32.to_le_bytes());
    buf[32..36].copy_from_slice(&0u32.to_le_bytes());
    buf[36..40].copy_from_slice(&1u32.to_le_bytes());
    buf[40..44].copy_from_slice(&(levels.len() as u32).to_le_bytes());
    // supercompressionScheme 0 = none; the parser rejects anything else.
    buf[44..48].copy_from_slice(&0u32.to_le_bytes());

    // The dfd/kvd/sgd index entries stay zero: those sections are absent.

    let mut placed = vec![(0u64, 0u64); levels.len()];
    for level in (0..levels.len()).rev() {
        let offset = buf.len() as u64;
        buf.extend_from_slice(levels[level]);
        placed[level] = (offset, levels[level].len() as u64);
    }
    for (level, (offset, length)) in placed.iter().enumerate() {
        let entry = HEADER_SIZE + level * LEVEL_INDEX_ENTRY_SIZE;
        buf[entry..entry + 8].copy_from_slice(&offset.to_le_bytes());
        buf[entry + 8..entry + 16].copy_from_slice(&length.to_le_bytes());
        // uncompressedByteLength equals byteLength when nothing is supercompressed.
        buf[entry + 16..entry + 24].copy_from_slice(&length.to_le_bytes());
    }
    Some(buf)
}

/// Write a single-level, non-supercompressed KTX2 container around already
/// compressed block data.
///
/// This is the ingest-side counterpart of [`parse_ktx2`]: transcoding a source
/// image at package-install time produces block data that still needs a
/// container the runtime can recognise. Keeping the writer beside the parser is
/// deliberate -- they share the offset constants and the magic, so the layout
/// cannot drift apart, and `parse_ktx2` is the round-trip oracle for the writer.
///
/// Emits exactly what the parser needs and nothing else: no DFD, no key/value
/// data, no supercompression global data. Those sections are optional in the
/// spec, and every consumer in this engine reads only the header, the level
/// index and the level bytes. It writes a single level: nothing in this engine
/// generates a mip chain at ingest yet, while `parse_ktx2` has to read the
/// chains that authored assets arrive with.
pub fn write_ktx2(vk_format: u32, width: u32, height: u32, level0: &[u8]) -> Vec<u8> {
    let level0_offset = (HEADER_SIZE + LEVEL_INDEX_ENTRY_SIZE) as u64;
    let level0_length = level0.len() as u64;

    let mut buf = vec![0u8; HEADER_SIZE + LEVEL_INDEX_ENTRY_SIZE + level0.len()];

    buf[..12].copy_from_slice(&KTX2_MAGIC);
    buf[12..16].copy_from_slice(&vk_format.to_le_bytes());
    // typeSize is 1 for every block-compressed format.
    buf[16..20].copy_from_slice(&1u32.to_le_bytes());
    buf[20..24].copy_from_slice(&width.to_le_bytes());
    buf[24..28].copy_from_slice(&height.to_le_bytes());
    // pixelDepth 0 = 2D, layerCount 0 = non-array, faceCount 1 = not a cubemap.
    buf[28..32].copy_from_slice(&0u32.to_le_bytes());
    buf[32..36].copy_from_slice(&0u32.to_le_bytes());
    buf[36..40].copy_from_slice(&1u32.to_le_bytes());
    buf[40..44].copy_from_slice(&1u32.to_le_bytes());
    // supercompressionScheme 0 = none; the parser rejects anything else.
    buf[44..48].copy_from_slice(&0u32.to_le_bytes());

    // The dfd/kvd/sgd index entries stay zero: those sections are absent.

    let li = HEADER_SIZE;
    buf[li..li + 8].copy_from_slice(&level0_offset.to_le_bytes());
    buf[li + 8..li + 16].copy_from_slice(&level0_length.to_le_bytes());
    // uncompressedByteLength equals byteLength when nothing is supercompressed.
    buf[li + 16..li + 24].copy_from_slice(&level0_length.to_le_bytes());

    buf[level0_offset as usize..].copy_from_slice(level0);

    buf
}

#[cfg(test)]
mod writer_tests {
    use super::*;

    #[test]
    fn a_written_mip_chain_reads_back_level_for_level() {
        let l0 = vec![0xA0u8; 128];
        let l1 = vec![0xA1u8; 32];
        let l2 = vec![0xA2u8; 8];
        let container = write_ktx2_levels(147, 16, 16, &[&l0[..], &l1[..], &l2[..]])
            .expect("a three-level chain is writable");

        let parsed = parse_ktx2(&container).expect("the runtime parser accepts what we write");
        assert_eq!(parsed.header.mip_levels, 3);
        let levels: Vec<&[u8]> = parsed.levels().collect();
        assert_eq!(levels, vec![&l0[..], &l1[..], &l2[..]]);
        assert_eq!(parsed.data, &l0[..], "level 0 stays reachable as `data`");
    }

    #[test]
    fn a_single_level_chain_matches_the_single_level_writer() {
        let level0 = vec![0xC3u8; 64];
        let via_chain = write_ktx2_levels(151, 8, 8, &[&level0[..]]).expect("writable");
        let via_single = write_ktx2(151, 8, 8, &level0);
        assert_eq!(
            via_chain, via_single,
            "one writer must not produce a different container from the other for \
             the same input, or the two paths drift apart"
        );
    }

    #[test]
    fn an_empty_chain_is_refused() {
        assert!(write_ktx2_levels(147, 8, 8, &[]).is_none());
    }

    #[test]
    fn more_levels_than_the_dimensions_allow_are_refused() {
        let l = vec![0u8; 8];
        // 2x2 has exactly two levels.
        assert!(write_ktx2_levels(147, 2, 2, &[&l[..], &l[..], &l[..]]).is_none());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal valid KTX2 buffer for testing.
    ///
    /// Delegates to the production writer so these tests exercise the real
    /// layout rather than a second copy of it that could drift from it.
    fn make_test_ktx2(vk_format: u32, width: u32, height: u32, payload: &[u8]) -> Vec<u8> {
        write_ktx2(vk_format, width, height, payload)
    }

    /// Build a multi-level KTX2 buffer.
    ///
    /// `payloads[0]` is the base level. The level *index* is written in level
    /// order, but the level *data* is stored smallest-level-first, which is what
    /// real KTX2 writers emit and what the spec's mip-streaming layout is for.
    /// Offsets in the index therefore descend, so an implementation that assumes
    /// the index is ordered by offset fails these tests instead of passing them
    /// by accident.
    fn make_multi_level_ktx2(
        vk_format: u32,
        width: u32,
        height: u32,
        payloads: &[&[u8]],
    ) -> Vec<u8> {
        let level_count = payloads.len();
        let index_size = level_count * LEVEL_INDEX_ENTRY_SIZE;
        let data_start = HEADER_SIZE + index_size;

        let mut buf = vec![0u8; data_start];
        buf[..12].copy_from_slice(&KTX2_MAGIC);
        buf[12..16].copy_from_slice(&vk_format.to_le_bytes());
        buf[16..20].copy_from_slice(&1u32.to_le_bytes());
        buf[20..24].copy_from_slice(&width.to_le_bytes());
        buf[24..28].copy_from_slice(&height.to_le_bytes());
        buf[28..32].copy_from_slice(&0u32.to_le_bytes());
        buf[32..36].copy_from_slice(&0u32.to_le_bytes());
        buf[36..40].copy_from_slice(&1u32.to_le_bytes());
        buf[40..44].copy_from_slice(&(level_count as u32).to_le_bytes());
        buf[44..48].copy_from_slice(&0u32.to_le_bytes());

        // Append data smallest level first, recording where each one landed.
        let mut placed = vec![(0u64, 0u64); level_count];
        for level in (0..level_count).rev() {
            let offset = buf.len() as u64;
            buf.extend_from_slice(payloads[level]);
            placed[level] = (offset, payloads[level].len() as u64);
        }

        for (level, (offset, length)) in placed.iter().enumerate() {
            let li = HEADER_SIZE + level * LEVEL_INDEX_ENTRY_SIZE;
            buf[li..li + 8].copy_from_slice(&offset.to_le_bytes());
            buf[li + 8..li + 16].copy_from_slice(&length.to_le_bytes());
            buf[li + 16..li + 24].copy_from_slice(&length.to_le_bytes());
        }
        buf
    }

    /// Overwrite one level index entry, to build the malformed cases.
    fn patch_level(buf: &mut [u8], level: usize, offset: u64, length: u64) {
        let li = HEADER_SIZE + level * LEVEL_INDEX_ENTRY_SIZE;
        buf[li..li + 8].copy_from_slice(&offset.to_le_bytes());
        buf[li + 8..li + 16].copy_from_slice(&length.to_le_bytes());
        buf[li + 16..li + 24].copy_from_slice(&length.to_le_bytes());
    }

    #[test]
    fn levels_yields_every_mip_in_level_order() {
        let l0 = vec![0x10; 64];
        let l1 = vec![0x11; 16];
        let l2 = vec![0x12; 8];
        let buf = make_multi_level_ktx2(147, 8, 8, &[&l0, &l1, &l2]);

        let ktx2 = parse_ktx2(&buf).expect("should parse");
        assert_eq!(ktx2.header.mip_levels, 3);

        let levels: Vec<&[u8]> = ktx2.levels().collect();
        assert_eq!(levels.len(), 3, "every declared level must be reachable");
        assert_eq!(levels[0], &l0[..]);
        assert_eq!(levels[1], &l1[..]);
        assert_eq!(levels[2], &l2[..]);
    }

    #[test]
    fn base_level_still_reachable_through_data_for_single_level_files() {
        let payload = vec![0xCC; 32];
        let buf = make_test_ktx2(147, 4, 4, &payload);

        let ktx2 = parse_ktx2(&buf).expect("should parse");
        assert_eq!(ktx2.data, &payload[..]);
        assert_eq!(ktx2.levels().count(), 1);
        assert_eq!(ktx2.levels().next().unwrap(), &payload[..]);
    }

    #[test]
    fn rejects_a_level_whose_data_runs_past_the_end() {
        let l0 = vec![0x10; 64];
        let l1 = vec![0x11; 16];
        let mut buf = make_multi_level_ktx2(147, 8, 8, &[&l0, &l1]);
        let past_the_end = buf.len() as u64;
        patch_level(&mut buf, 1, past_the_end - 4, 64);

        assert!(
            parse_ktx2(&buf).is_err(),
            "a level extending past the buffer must not parse"
        );
    }

    #[test]
    fn rejects_levels_that_overlap() {
        let l0 = vec![0x10; 64];
        let l1 = vec![0x11; 16];
        let buf_ok = make_multi_level_ktx2(147, 8, 8, &[&l0, &l1]);
        let mut buf = buf_ok.clone();
        // Point level 1 into the middle of level 0's bytes.
        let l0_offset = u64::from_le_bytes(buf[HEADER_SIZE..HEADER_SIZE + 8].try_into().unwrap());
        patch_level(&mut buf, 1, l0_offset + 8, 16);

        assert!(
            parse_ktx2(&buf).is_err(),
            "overlapping levels are not a mip chain and must not parse"
        );
    }

    #[test]
    fn rejects_a_zero_length_level() {
        let l0 = vec![0x10; 64];
        let l1 = vec![0x11; 16];
        let mut buf = make_multi_level_ktx2(147, 8, 8, &[&l0, &l1]);
        patch_level(&mut buf, 1, HEADER_SIZE as u64, 0);

        assert!(
            parse_ktx2(&buf).is_err(),
            "a declared level with no bytes cannot be uploaded"
        );
    }

    #[test]
    fn parse_valid_etc2_rgb() {
        let payload = vec![0xAA; 64]; // fake compressed data
        let buf = make_test_ktx2(147, 256, 256, &payload);

        let ktx2 = parse_ktx2(&buf).expect("should parse");
        assert_eq!(ktx2.header.format, VkFormat::Etc2R8G8B8UnormBlock);
        assert_eq!(ktx2.header.width, 256);
        assert_eq!(ktx2.header.height, 256);
        assert_eq!(ktx2.header.mip_levels, 1);
        assert_eq!(ktx2.data, &payload[..]);
    }

    #[test]
    fn parse_valid_astc_4x4() {
        let payload = vec![0xBB; 128];
        let buf = make_test_ktx2(157, 512, 512, &payload);

        let ktx2 = parse_ktx2(&buf).expect("should parse");
        assert_eq!(ktx2.header.format, VkFormat::Astc4x4UnormBlock);
        assert_eq!(ktx2.header.width, 512);
        assert_eq!(ktx2.header.height, 512);
        assert_eq!(ktx2.data.len(), 128);
    }

    #[test]
    fn reject_bad_magic() {
        let buf = vec![0u8; 128];
        assert_eq!(parse_ktx2(&buf).unwrap_err(), "ktx2: invalid magic bytes");
    }

    #[test]
    fn reject_truncated() {
        assert_eq!(
            parse_ktx2(&[0xAB, 0x4B]).unwrap_err(),
            "ktx2: file too short for header"
        );
    }

    #[test]
    fn reject_supercompression() {
        let mut buf = make_test_ktx2(147, 4, 4, &[0; 8]);
        // Set supercompressionScheme to 1 (BasisLZ)
        buf[44..48].copy_from_slice(&1u32.to_le_bytes());
        assert_eq!(
            parse_ktx2(&buf).unwrap_err(),
            "ktx2: supercompression not supported"
        );
    }

    #[test]
    fn reject_zero_dimensions() {
        let buf = make_test_ktx2(147, 0, 256, &[0; 8]);
        assert_eq!(parse_ktx2(&buf).unwrap_err(), "ktx2: zero dimensions");
    }

    #[test]
    fn is_ktx2_detection() {
        let buf = make_test_ktx2(147, 4, 4, &[0; 8]);
        assert!(is_ktx2(&buf));
        assert!(!is_ktx2(&[0u8; 12]));
        assert!(!is_ktx2(&[0u8; 4]));
    }

    #[test]
    fn unknown_format_passthrough() {
        let payload = vec![0xCC; 32];
        let buf = make_test_ktx2(999, 64, 64, &payload);

        let ktx2 = parse_ktx2(&buf).expect("should parse");
        assert_eq!(ktx2.header.format, VkFormat::Unknown(999));
    }

    #[test]
    fn reject_level0_pointing_into_the_header() {
        let payload = vec![0xEE; 32];
        let mut buf = make_test_ktx2(147, 4, 4, &payload);
        // Offset 0 is in bounds and `offset + length` still fits, so only an
        // explicit lower bound stops the magic and header being handed to the
        // GPU as texture data.
        buf[HEADER_SIZE..HEADER_SIZE + 8].copy_from_slice(&0u64.to_le_bytes());

        assert_eq!(
            parse_ktx2(&buf).unwrap_err(),
            "ktx2: level data overlaps the header or level index"
        );
    }

    #[test]
    fn reject_level0_overlapping_the_level_index() {
        let payload = vec![0xEE; 32];
        let mut buf = make_test_ktx2(147, 4, 4, &payload);
        // One byte short of where the index ends.
        let offset = (HEADER_SIZE + LEVEL_INDEX_ENTRY_SIZE - 1) as u64;
        buf[HEADER_SIZE..HEADER_SIZE + 8].copy_from_slice(&offset.to_le_bytes());
        buf[HEADER_SIZE + 8..HEADER_SIZE + 16].copy_from_slice(&8u64.to_le_bytes());

        assert_eq!(
            parse_ktx2(&buf).unwrap_err(),
            "ktx2: level data overlaps the header or level index"
        );
    }

    #[test]
    fn reject_level_count_whose_index_span_overflows() {
        let payload = vec![0xEE; 32];
        let mut buf = make_test_ktx2(147, 4, 4, &payload);
        // `level_count * 24` is the product an attacker controls. On a 32-bit
        // target this wraps; on 64-bit it stays huge. Either way the file is
        // nowhere near long enough, and neither outcome may index the buffer.
        buf[40..44].copy_from_slice(&u32::MAX.to_le_bytes());

        let err = parse_ktx2(&buf).unwrap_err();
        assert!(
            err == "ktx2: level index span overflow"
                || err == "ktx2: file too short for level index",
            "unexpected error: {err}"
        );
    }

    #[test]
    fn reject_level0_length_running_past_the_buffer() {
        let payload = vec![0xEE; 32];
        let mut buf = make_test_ktx2(147, 4, 4, &payload);
        buf[HEADER_SIZE + 8..HEADER_SIZE + 16].copy_from_slice(&u64::MAX.to_le_bytes());

        let err = parse_ktx2(&buf).unwrap_err();
        assert!(
            err == "ktx2: level range overflow" || err == "ktx2: level data out of bounds",
            "unexpected error: {err}"
        );
    }

    #[test]
    fn level_count_zero_treated_as_one() {
        let payload = vec![0xDD; 16];
        let mut buf = make_test_ktx2(147, 4, 4, &payload);
        // Set levelCount to 0
        buf[40..44].copy_from_slice(&0u32.to_le_bytes());

        let ktx2 = parse_ktx2(&buf).expect("should parse");
        assert_eq!(ktx2.header.mip_levels, 1);
        assert_eq!(ktx2.data, &payload[..]);
    }
}

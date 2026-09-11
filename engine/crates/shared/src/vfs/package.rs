//! Migo Package format — zstd-chunked single-file archive for `/code` mount.
//!
//! Each entry is split into fixed-size chunks (default 64 KiB uncompressed),
//! each independently zstd-compressed.  Reading a byte range only decompresses
//! the overlapping chunks, enabling true random access.
//!
//! # Format layout
//!
//! ```text
//! [Header: 32 bytes]
//!   magic: "MPKG"
//!   version: u32 = 1
//!   entry_count: u32
//!   chunk_count: u32
//!   chunk_table_offset: u64
//!   index_offset: u64
//!
//! [Chunk Data Area]
//!   chunk 0 payload (zstd frame or stored)
//!   chunk 1 payload
//!   ...
//!
//! [Chunk Table]  (16 bytes per chunk)
//!   data_offset: u64
//!   compressed_size: u32
//!   raw_size: u32
//!
//! [Entry Index]
//!   per entry:
//!     path_len: u16
//!     path: [u8]
//!     raw_size: u64
//!     crc32: u32
//!     first_chunk: u32
//!     chunk_count: u32
//! ```

use std::collections::{BTreeSet, HashMap};
use std::fmt;
use std::io::{self, BufReader, Read, Seek, SeekFrom, Write};
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use digest::Digest;
use lru::LruCache;
use parking_lot::Mutex;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const MAGIC: [u8; 4] = *b"MPKG";
pub(crate) const FORMAT_VERSION: u32 = 1;
pub(crate) const HEADER_SIZE: u64 = 32;
/// Default uncompressed chunk size: 64 KiB.
pub const DEFAULT_CHUNK_SIZE: u32 = 64 * 1024;
/// Hard upper bound for any single chunk's raw_size.
/// 4x default chunk size to allow for custom chunk sizes while still
/// preventing a malicious package from triggering huge allocations.
const MAX_CHUNK_RAW_SIZE: u32 = 4 * DEFAULT_CHUNK_SIZE;
/// Zstd compression level for chunks.
const ZSTD_LEVEL: i32 = 3;

// ---------------------------------------------------------------------------
// PackageIdentity
// ---------------------------------------------------------------------------

/// Stable identity for a mounted package.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PackageIdentity {
    pub name: String,
    pub version: String,
    /// Deterministic CRC32 derived from entry metadata.
    pub checksum: u32,
}

// ---------------------------------------------------------------------------
// PackageDigest
// ---------------------------------------------------------------------------

/// SHA-256 over a package file's bytes.
///
/// This is what a cross-Session cache may key pack-backed entries on, and the
/// property it has to carry is that **producing the key requires holding the
/// content**: two mounts agree only if their packages are byte-identical, so
/// sharing a decoded entry can never hand a Session bytes it did not already
/// have. Entry metadata cannot carry that property, because the format's
/// per-entry integrity primitive is a CRC32 and a package can be built to match
/// another's per-entry paths, sizes and CRC32s.
///
/// Packing is deterministic — see `streaming_add_matches_buffered_add_bit_for_bit`
/// — so the same content packed by the same writer still shares. Content packed
/// with different chunk sizes does not, which loses sharing rather than safety.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PackageDigest([u8; 32]);

impl PackageDigest {
    /// Digest bytes already held in memory. The install path reads the whole
    /// package to hand it to the signature verifier, so it pays no extra I/O.
    pub fn of_bytes(bytes: &[u8]) -> Self {
        let mut hasher = sha2::Sha256::new();
        hasher.update(bytes);
        Self(hasher.finalize().into())
    }

    /// Digest a package file, streaming through a fixed buffer so a large
    /// package costs one buffer rather than its own size in memory.
    pub fn of_file(path: &Path) -> io::Result<Self> {
        const CHUNK: usize = 64 * 1024;
        let mut file = std::fs::File::open(path)?;
        let mut hasher = sha2::Sha256::new();
        let mut buf = vec![0u8; CHUNK];
        loop {
            let read = file.read(&mut buf)?;
            if read == 0 {
                break;
            }
            hasher.update(&buf[..read]);
        }
        Ok(Self(hasher.finalize().into()))
    }

    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    pub fn parse_hex(text: &str) -> Option<Self> {
        let bytes = hex::decode(text).ok()?;
        Some(Self(bytes.try_into().ok()?))
    }

    /// The identity a cache keys on, truncated to the width of the key space it
    /// feeds: [`super::ResolvedCode::source_identity`] is a `u64` and the
    /// on-disk derived-asset key encodes eight bytes, so a wider value would be
    /// truncated at the next hop rather than carried.
    pub fn cache_identity(&self) -> u64 {
        let mut head = [0u8; 8];
        head.copy_from_slice(&self.0[..8]);
        u64::from_le_bytes(head)
    }
}

// ---------------------------------------------------------------------------
// PackageError
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum PackageError {
    Io(io::Error),
    BadMagic,
    UnsupportedVersion(u32),
    BadIndex(String),
    InvalidEntryPath(String),
    ChecksumMismatch {
        path: String,
        expected: u32,
        actual: u32,
    },
    DecompressSizeMismatch {
        path: String,
        expected: u64,
        actual: usize,
    },
}

impl fmt::Display for PackageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "package IO error: {e}"),
            Self::BadMagic => write!(f, "not a valid MPKG file (bad magic)"),
            Self::UnsupportedVersion(v) => write!(f, "unsupported package version: {v}"),
            Self::BadIndex(msg) => write!(f, "corrupt package index: {msg}"),
            Self::InvalidEntryPath(p) => write!(f, "invalid entry path: {p}"),
            Self::ChecksumMismatch {
                path,
                expected,
                actual,
            } => {
                write!(
                    f,
                    "CRC32 mismatch for '{path}': expected {expected:#010x}, got {actual:#010x}"
                )
            }
            Self::DecompressSizeMismatch {
                path,
                expected,
                actual,
            } => {
                write!(
                    f,
                    "decompress size mismatch for '{path}': expected {expected}, got {actual}"
                )
            }
        }
    }
}

impl std::error::Error for PackageError {}

impl From<io::Error> for PackageError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

// ---------------------------------------------------------------------------
// Path validation
// ---------------------------------------------------------------------------

pub(crate) fn validate_entry_path(path: &str) -> Result<String, PackageError> {
    if path.is_empty() {
        return Err(PackageError::InvalidEntryPath("empty path".into()));
    }
    if path.starts_with('/') {
        return Err(PackageError::InvalidEntryPath(format!(
            "absolute path: {path}"
        )));
    }
    for b in path.bytes() {
        if b < 0x20 || b == b'\\' {
            return Err(PackageError::InvalidEntryPath(format!(
                "illegal character in path: {path}"
            )));
        }
    }
    let mut parts: Vec<&str> = Vec::new();
    for component in path.split('/') {
        match component {
            "" | "." => continue,
            ".." => {
                if parts.pop().is_none() {
                    return Err(PackageError::InvalidEntryPath(format!(
                        "path traversal: {path}"
                    )));
                }
            }
            c => parts.push(c),
        }
    }
    if parts.is_empty() {
        return Err(PackageError::InvalidEntryPath(format!(
            "empty after normalization: {path}"
        )));
    }
    Ok(parts.join("/"))
}

// ---------------------------------------------------------------------------
// HashState (streaming digest)
// ---------------------------------------------------------------------------

enum HashState {
    Md5(md5::Md5),
    Sha1(sha1::Sha1),
    Sha256(sha2::Sha256),
}

impl HashState {
    fn new(algorithm: &str) -> Result<Self, PackageError> {
        match algorithm {
            "md5" => Ok(Self::Md5(md5::Md5::new())),
            "sha1" => Ok(Self::Sha1(sha1::Sha1::new())),
            "sha256" => Ok(Self::Sha256(sha2::Sha256::new())),
            _ => Err(PackageError::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("unsupported digestAlgorithm: {algorithm}"),
            ))),
        }
    }
    fn update(&mut self, data: &[u8]) {
        match self {
            Self::Md5(h) => h.update(data),
            Self::Sha1(h) => h.update(data),
            Self::Sha256(h) => h.update(data),
        }
    }
    fn finalize_hex(self) -> String {
        match self {
            Self::Md5(h) => hex::encode(h.finalize()),
            Self::Sha1(h) => hex::encode(h.finalize()),
            Self::Sha256(h) => hex::encode(h.finalize()),
        }
    }
}

// ---------------------------------------------------------------------------
// Chunk table entry
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct ChunkEntry {
    data_offset: u64,
    compressed_size: u32,
    raw_size: u32,
}

// ---------------------------------------------------------------------------
// Per-entry metadata
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct EntryMeta {
    path: String,
    raw_size: u64,
    crc32: u32,
    first_chunk: u32,
    chunk_count: u32,
}

// ---------------------------------------------------------------------------
// Header
// ---------------------------------------------------------------------------

struct Header {
    entry_count: u32,
    chunk_count: u32,
    chunk_table_offset: u64,
    index_offset: u64,
}

impl Header {
    fn read_from(r: &mut impl Read) -> Result<Self, PackageError> {
        let mut buf = [0u8; HEADER_SIZE as usize];
        r.read_exact(&mut buf)?;
        if buf[0..4] != MAGIC {
            return Err(PackageError::BadMagic);
        }
        let version = u32::from_le_bytes(buf[4..8].try_into().unwrap());
        if version != FORMAT_VERSION {
            return Err(PackageError::UnsupportedVersion(version));
        }
        Ok(Self {
            entry_count: u32::from_le_bytes(buf[8..12].try_into().unwrap()),
            chunk_count: u32::from_le_bytes(buf[12..16].try_into().unwrap()),
            chunk_table_offset: u64::from_le_bytes(buf[16..24].try_into().unwrap()),
            index_offset: u64::from_le_bytes(buf[24..32].try_into().unwrap()),
        })
    }

    fn write_to(&self, w: &mut impl Write) -> io::Result<()> {
        w.write_all(&MAGIC)?;
        w.write_all(&FORMAT_VERSION.to_le_bytes())?;
        w.write_all(&self.entry_count.to_le_bytes())?;
        w.write_all(&self.chunk_count.to_le_bytes())?;
        w.write_all(&self.chunk_table_offset.to_le_bytes())?;
        w.write_all(&self.index_offset.to_le_bytes())?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// PackageWriter
// ---------------------------------------------------------------------------

/// Builds a `.mpkg` package with zstd-chunked compression.
pub struct PackageWriter<W: Write + Seek> {
    writer: W,
    entries: Vec<EntryMeta>,
    chunks: Vec<ChunkEntry>,
    seen_paths: BTreeSet<String>,
    data_pos: u64,
    chunk_size: u32,
    poisoned: bool,
}

impl<W: Write + Seek> PackageWriter<W> {
    pub fn new(writer: W) -> io::Result<Self> {
        Self::with_chunk_size(writer, DEFAULT_CHUNK_SIZE)
    }

    pub fn with_chunk_size(mut writer: W, chunk_size: u32) -> io::Result<Self> {
        let placeholder = Header {
            entry_count: 0,
            chunk_count: 0,
            chunk_table_offset: 0,
            index_offset: 0,
        };
        placeholder.write_to(&mut writer)?;
        Ok(Self {
            writer,
            entries: Vec::new(),
            chunks: Vec::new(),
            seen_paths: BTreeSet::new(),
            data_pos: HEADER_SIZE,
            chunk_size,
            poisoned: false,
        })
    }

    /// Reject duplicate paths and file/directory prefix conflicts without
    /// scanning every prior entry.  The old full scan made each insertion
    /// quadratic and allocated `existing + "/"` for every comparison.
    fn check_path_conflict(&self, normalized: &str) -> Result<(), PackageError> {
        if self.seen_paths.contains(normalized) {
            return Err(PackageError::InvalidEntryPath(format!(
                "duplicate entry: {normalized}"
            )));
        }

        // A successor beginning with `normalized/` means this new file would
        // shadow an already-added child.  Build that lower bound once per new
        // path; unlike the old loop, it is never allocated per comparison.
        let child_prefix = format!("{normalized}/");
        if let Some(existing) = self
            .seen_paths
            .range::<str, _>((
                std::ops::Bound::Included(child_prefix.as_str()),
                std::ops::Bound::Unbounded,
            ))
            .next()
        {
            if existing.starts_with(&child_prefix) {
                return Err(PackageError::InvalidEntryPath(format!(
                    "prefix conflict: '{normalized}' conflicts with '{existing}'"
                )));
            }
        }

        // Walk the new path's ancestors.  Each lookup is logarithmic in the
        // ordered set and creates no owned path.
        let mut ancestor = normalized;
        while let Some((parent, _)) = ancestor.rsplit_once('/') {
            if self.seen_paths.contains(parent) {
                return Err(PackageError::InvalidEntryPath(format!(
                    "prefix conflict: '{normalized}' conflicts with '{parent}'"
                )));
            }
            ancestor = parent;
        }

        Ok(())
    }

    /// Streaming variant of [`Self::add_entry`].
    ///
    /// Reads the entry in chunks of at most `chunk_size` bytes and
    /// writes each compressed chunk immediately, so the peak memory
    /// footprint per entry is bounded by the chunk size (default
    /// 64 KiB) instead of the uncompressed entry size. Use this when
    /// the producer is itself a streaming source (zip entry, HTTP
    /// response body) so the ingest path never materialises a 20 MiB
    /// file as a 20 MiB `Vec<u8>`.
    ///
    /// `max_entry_bytes` caps the total bytes this call will accept;
    /// an entry exceeding the cap returns `InvalidData` mid-stream
    /// and the writer is marked poisoned — any previously-emitted
    /// chunks in this call have already been flushed to the output
    /// stream but the index will not mention this entry, so the
    /// resulting package is truncated garbage and must be discarded.
    /// Callers that want atomic-abort semantics should wrap the
    /// writer in a tmp-file + rename (which [`ingest_zip_to_package`]
    /// already does).
    ///
    /// CRC-32 is computed incrementally over the streamed bytes so
    /// we don't need to buffer the full entry for integrity.
    pub fn add_entry_streaming<R: std::io::Read>(
        &mut self,
        path: &str,
        mut reader: R,
        max_entry_bytes: u64,
    ) -> Result<(), PackageError> {
        if self.poisoned {
            return Err(PackageError::Io(io::Error::new(
                io::ErrorKind::Other,
                "writer poisoned: a previous write failed",
            )));
        }

        let normalized = validate_entry_path(path)?;
        self.check_path_conflict(&normalized)?;

        let cs = self.chunk_size as usize;
        let first_chunk = self.chunks.len() as u32;
        let mut hasher = crc32fast::Hasher::new();
        let mut total_bytes: u64 = 0;
        let mut chunk_buf: Vec<u8> = Vec::with_capacity(cs);

        loop {
            // Fill `chunk_buf` up to `cs` bytes or until EOF.
            chunk_buf.clear();
            let mut remaining = cs;
            while remaining > 0 {
                // Read into a small stack buffer then extend the
                // chunk. Going via a small stack buffer keeps the
                // peak allocator footprint at `cs` + O(1) instead
                // of doubling the chunk buffer's capacity.
                let mut tmp = [0u8; 8192];
                let want = tmp.len().min(remaining);
                let n = reader.read(&mut tmp[..want]).map_err(PackageError::Io)?;
                if n == 0 {
                    break;
                }
                chunk_buf.extend_from_slice(&tmp[..n]);
                remaining -= n;
            }
            if chunk_buf.is_empty() {
                break; // EOF
            }

            total_bytes = total_bytes.saturating_add(chunk_buf.len() as u64);
            if total_bytes > max_entry_bytes {
                self.poisoned = true;
                return Err(PackageError::Io(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "streaming entry '{}' exceeds max_entry_bytes limit {}",
                        normalized, max_entry_bytes
                    ),
                )));
            }
            hasher.update(&chunk_buf);

            let raw_size = chunk_buf.len() as u32;
            let compressed = zstd::bulk::compress(&chunk_buf, ZSTD_LEVEL).map_err(|e| {
                PackageError::Io(io::Error::new(
                    io::ErrorKind::Other,
                    format!("zstd compress: {e}"),
                ))
            })?;
            if let Err(e) = self.writer.write_all(&compressed) {
                self.poisoned = true;
                return Err(PackageError::Io(e));
            }
            self.chunks.push(ChunkEntry {
                data_offset: self.data_pos,
                compressed_size: compressed.len() as u32,
                raw_size,
            });
            self.data_pos += compressed.len() as u64;
        }

        let chunk_count = self.chunks.len() as u32 - first_chunk;
        self.seen_paths.insert(normalized.clone());
        self.entries.push(EntryMeta {
            path: normalized,
            raw_size: total_bytes,
            crc32: hasher.finalize(),
            first_chunk,
            chunk_count,
        });
        Ok(())
    }

    /// Add a file entry, splitting into zstd-compressed chunks.
    pub fn add_entry(&mut self, path: &str, data: &[u8]) -> Result<(), PackageError> {
        if self.poisoned {
            return Err(PackageError::Io(io::Error::new(
                io::ErrorKind::Other,
                "writer poisoned: a previous write failed",
            )));
        }

        let normalized = validate_entry_path(path)?;
        self.check_path_conflict(&normalized)?;

        let crc = crc32fast::hash(data);
        let first_chunk = self.chunks.len() as u32;
        let cs = self.chunk_size as usize;
        let mut offset = 0usize;

        while offset < data.len() {
            let end = (offset + cs).min(data.len());
            let chunk_data = &data[offset..end];
            let raw_size = chunk_data.len() as u32;

            let compressed = zstd::bulk::compress(chunk_data, ZSTD_LEVEL).map_err(|e| {
                PackageError::Io(io::Error::new(
                    io::ErrorKind::Other,
                    format!("zstd compress: {e}"),
                ))
            })?;

            if let Err(e) = self.writer.write_all(&compressed) {
                self.poisoned = true;
                return Err(PackageError::Io(e));
            }

            self.chunks.push(ChunkEntry {
                data_offset: self.data_pos,
                compressed_size: compressed.len() as u32,
                raw_size,
            });
            self.data_pos += compressed.len() as u64;
            offset = end;
        }

        let chunk_count = self.chunks.len() as u32 - first_chunk;
        self.seen_paths.insert(normalized.clone());
        self.entries.push(EntryMeta {
            path: normalized,
            raw_size: data.len() as u64,
            crc32: crc,
            first_chunk,
            chunk_count,
        });
        Ok(())
    }

    /// Finalize: write chunk table + entry index + fixup header.
    pub fn finish(
        mut self,
        package_name: &str,
        package_version: &str,
    ) -> Result<PackageIdentity, PackageError> {
        if self.poisoned {
            return Err(PackageError::Io(io::Error::new(
                io::ErrorKind::Other,
                "writer poisoned: cannot finalize",
            )));
        }

        let chunk_table_offset = self.data_pos;

        // Chunk table: 16 bytes per chunk.
        for chunk in &self.chunks {
            self.writer.write_all(&chunk.data_offset.to_le_bytes())?;
            self.writer
                .write_all(&chunk.compressed_size.to_le_bytes())?;
            self.writer.write_all(&chunk.raw_size.to_le_bytes())?;
        }

        let index_offset = chunk_table_offset + (self.chunks.len() as u64 * 16);

        // Entry index.
        for entry in &self.entries {
            let pb = entry.path.as_bytes();
            self.writer.write_all(&(pb.len() as u16).to_le_bytes())?;
            self.writer.write_all(pb)?;
            self.writer.write_all(&entry.raw_size.to_le_bytes())?;
            self.writer.write_all(&entry.crc32.to_le_bytes())?;
            self.writer.write_all(&entry.first_chunk.to_le_bytes())?;
            self.writer.write_all(&entry.chunk_count.to_le_bytes())?;
        }

        // Fixup header.
        self.writer.seek(SeekFrom::Start(0))?;
        Header {
            entry_count: self.entries.len() as u32,
            chunk_count: self.chunks.len() as u32,
            chunk_table_offset,
            index_offset,
        }
        .write_to(&mut self.writer)?;
        self.writer.flush()?;

        // Hashed in sorted path order, and CRC32 is order-dependent, so the rule
        // has to be the same one the reader uses -- it walks a `HashMap`, whose
        // iteration order differs per instance. Without a shared rule an identity
        // computed here and one read back from the same file disagree.
        let mut ordered: Vec<&EntryMeta> = self.entries.iter().collect();
        ordered.sort_unstable_by(|a, b| a.path.cmp(&b.path));
        let mut hasher = crc32fast::Hasher::new();
        for e in ordered {
            hasher.update(&e.crc32.to_le_bytes());
            hasher.update(e.path.as_bytes());
        }
        Ok(PackageIdentity {
            name: package_name.to_string(),
            version: package_version.to_string(),
            checksum: hasher.finalize(),
        })
    }
}

// ---------------------------------------------------------------------------
// PackageReader
// ---------------------------------------------------------------------------

/// Maximum entry size (uncompressed) eligible for the inflate cache.
/// Larger entries skip the cache so a single audio/video file can't
/// evict every small JSON we just decompressed.
const INFLATE_CACHE_MAX_ENTRY_BYTES: u64 = 128 * 1024;

/// LRU bound on cache entry count. With the per-entry size cap above
/// the worst-case footprint is ~16 MiB; in practice the working set
/// of small atlases / configs hit during a hot menu sits well below.
const INFLATE_CACHE_CAPACITY: usize = 128;

struct ReaderInner {
    path: PathBuf,
    chunks: Vec<ChunkEntry>,
    entries: HashMap<String, EntryMeta>,
    identity: PackageIdentity,
    /// Per-package decompressed-entry cache. Keyed by normalized
    /// relative path; values are full entry bytes shared via `Arc` so
    /// repeated reads avoid both the disk hop and the zstd inflate.
    /// Menu re-entry in cocos titles re-reads the same atlas/json
    /// dozens of times — the cache turns each subsequent miss into a
    /// memcpy.
    inflate_cache: Mutex<LruCache<String, Arc<Vec<u8>>>>,
}

pub struct PackageReader {
    inner: Arc<ReaderInner>,
}

impl PackageReader {
    pub fn open(
        path: &Path,
        package_name: &str,
        package_version: &str,
    ) -> Result<Self, PackageError> {
        let file = std::fs::File::open(path)?;
        let file_len = file.metadata()?.len();
        let mut reader = BufReader::new(file);
        let header = Header::read_from(&mut reader)?;

        // Bounds.
        let chunk_table_end = header
            .chunk_table_offset
            .checked_add(
                (header.chunk_count as u64)
                    .checked_mul(16)
                    .ok_or_else(|| PackageError::BadIndex("chunk table size overflow".into()))?,
            )
            .ok_or_else(|| PackageError::BadIndex("chunk table end overflow".into()))?;
        if chunk_table_end > file_len {
            return Err(PackageError::BadIndex("chunk table past EOF".into()));
        }
        if header.index_offset < chunk_table_end {
            return Err(PackageError::BadIndex("index overlaps chunk table".into()));
        }
        // `entry_count` sizes an allocation below, and it arrives from the file.
        // `chunk_count` is already bounded by the chunk-table-past-EOF check
        // above, but nothing bounded this one, so a 32-byte package declaring
        // `u32::MAX` entries asked for a ~300 GiB `HashMap` and the allocator
        // aborted the process -- on Android, the whole app.
        //
        // The bound is derived from the file rather than picked: the smallest
        // possible index record is a 2-byte path length plus the 20-byte
        // metadata block, so a package cannot honestly declare more entries
        // than its index region has room for.
        const MIN_INDEX_RECORD_BYTES: u64 = 22;
        let index_region = file_len.saturating_sub(header.index_offset);
        if (header.entry_count as u64) > index_region / MIN_INDEX_RECORD_BYTES {
            return Err(PackageError::BadIndex(format!(
                "entry_count {} exceeds what {index_region} index bytes can hold",
                header.entry_count
            )));
        }

        // Read chunk table.
        reader.seek(SeekFrom::Start(header.chunk_table_offset))?;
        let mut chunks = Vec::with_capacity(header.chunk_count as usize);
        for i in 0..header.chunk_count {
            let mut buf = [0u8; 16];
            reader.read_exact(&mut buf)?;
            let data_offset = u64::from_le_bytes(buf[0..8].try_into().unwrap());
            let compressed_size = u32::from_le_bytes(buf[8..12].try_into().unwrap());
            let raw_size = u32::from_le_bytes(buf[12..16].try_into().unwrap());

            let chunk_end = data_offset
                .checked_add(compressed_size as u64)
                .ok_or_else(|| PackageError::BadIndex(format!("chunk {i} offset overflow")))?;
            if chunk_end > header.chunk_table_offset {
                return Err(PackageError::BadIndex(format!(
                    "chunk {i} extends into table"
                )));
            }
            if data_offset < HEADER_SIZE {
                return Err(PackageError::BadIndex(format!("chunk {i} in header area")));
            }
            // Hard cap: no chunk can decompress to more than MAX_CHUNK_RAW_SIZE.
            // This prevents a malicious package from causing huge allocations.
            if raw_size > MAX_CHUNK_RAW_SIZE {
                return Err(PackageError::BadIndex(format!(
                    "chunk {i} raw_size {} exceeds limit {}",
                    raw_size, MAX_CHUNK_RAW_SIZE
                )));
            }
            chunks.push(ChunkEntry {
                data_offset,
                compressed_size,
                raw_size,
            });
        }

        // Read entry index.
        reader.seek(SeekFrom::Start(header.index_offset))?;
        let mut entries = HashMap::with_capacity(header.entry_count as usize);
        for _ in 0..header.entry_count {
            let mut len_buf = [0u8; 2];
            reader.read_exact(&mut len_buf)?;
            let path_len = u16::from_le_bytes(len_buf) as usize;
            let mut path_buf = vec![0u8; path_len];
            reader.read_exact(&mut path_buf)?;
            let path_str = std::str::from_utf8(&path_buf)
                .map_err(|e| PackageError::BadIndex(format!("path not UTF-8: {e}")))?;

            let mut meta = [0u8; 20];
            reader.read_exact(&mut meta)?;
            let raw_size = u64::from_le_bytes(meta[0..8].try_into().unwrap());
            let crc32 = u32::from_le_bytes(meta[8..12].try_into().unwrap());
            let first_chunk = u32::from_le_bytes(meta[12..16].try_into().unwrap());
            let chunk_count = u32::from_le_bytes(meta[16..20].try_into().unwrap());

            let normalized = validate_entry_path(path_str)?;
            if entries.contains_key(&normalized) {
                return Err(PackageError::InvalidEntryPath(format!(
                    "duplicate: {normalized}"
                )));
            }

            let last = first_chunk
                .checked_add(chunk_count)
                .ok_or_else(|| PackageError::BadIndex("chunk range overflow".into()))?;
            if last > chunks.len() as u32 {
                return Err(PackageError::BadIndex(format!(
                    "entry '{normalized}' chunks [{first_chunk},{last}) exceeds {}",
                    chunks.len()
                )));
            }

            // Index self-consistency — keep the random-access reader
            // (`read_range`) sound against a corrupt/malicious .mpkg:
            //   * A non-empty entry must reference at least one chunk, and
            //     its first chunk's `raw_size` (used as the chunk-size
            //     divisor in `read_range`) must be non-zero — otherwise a
            //     read would divide by zero.
            //   * The sum of referenced chunk `raw_size`s must equal the
            //     entry's advertised `raw_size`, so `read_range`'s
            //     `Vec::with_capacity(end - position)` can't be inflated
            //     beyond the bytes that actually exist in the chunks.
            if chunk_count == 0 {
                if raw_size != 0 {
                    return Err(PackageError::BadIndex(format!(
                        "entry '{normalized}' has raw_size {raw_size} but zero chunks"
                    )));
                }
            } else {
                let stride = chunks[first_chunk as usize].raw_size;
                if stride == 0 {
                    return Err(PackageError::BadIndex(format!(
                        "entry '{normalized}' first chunk has zero raw_size"
                    )));
                }
                let mut chunk_sum: u64 = 0;
                let span = &chunks[first_chunk as usize..last as usize];
                for (offset, c) in span.iter().enumerate() {
                    // `read_range` locates chunk *i* at `i * stride`, taking the
                    // first chunk's size as the stride for all of them. That is
                    // true of everything `PackageWriter` emits -- it cuts at a
                    // fixed size, so only the tail is short -- and was nowhere
                    // enforced, which left a reader deriving byte offsets from an
                    // assumption a crafted index could simply violate. Checking
                    // it here makes the assumption hold by construction instead
                    // of by provenance.
                    let is_last = offset + 1 == span.len();
                    if !is_last && c.raw_size != stride {
                        return Err(PackageError::BadIndex(format!(
                            "entry '{normalized}' chunk {} raw_size {} breaks stride {stride}",
                            first_chunk as usize + offset,
                            c.raw_size
                        )));
                    }
                    if is_last && c.raw_size > stride {
                        return Err(PackageError::BadIndex(format!(
                            "entry '{normalized}' final chunk raw_size {} exceeds stride {stride}",
                            c.raw_size
                        )));
                    }
                    chunk_sum = chunk_sum.checked_add(c.raw_size as u64).ok_or_else(|| {
                        PackageError::BadIndex(format!("entry '{normalized}' chunk sum overflow"))
                    })?;
                }
                if chunk_sum != raw_size {
                    return Err(PackageError::BadIndex(format!(
                        "entry '{normalized}' raw_size {raw_size} != chunk raw_size sum {chunk_sum}"
                    )));
                }
            }

            entries.insert(
                normalized,
                EntryMeta {
                    path: path_str.to_string(),
                    raw_size,
                    crc32,
                    first_chunk,
                    chunk_count,
                },
            );
        }

        // Prefix conflict check.
        let paths: Vec<&String> = entries.keys().collect();
        for (i, a) in paths.iter().enumerate() {
            let ap = format!("{}/", a);
            for b in &paths[i + 1..] {
                if b.starts_with(&ap) || a.starts_with(&format!("{}/", b)) {
                    return Err(PackageError::InvalidEntryPath(format!(
                        "prefix conflict: '{}' and '{}'",
                        a, b
                    )));
                }
            }
        }

        // Sorted, because `entries` is a `HashMap` and CRC32 is order-dependent:
        // iterating it directly made this identity differ between two opens of the
        // same file, since every `HashMap` instance hashes with its own seed. The
        // field is documented as deterministic and is used to tell one package from
        // another, so a per-instance value would be worse than useless.
        let mut ordered: Vec<(&String, &EntryMeta)> = entries.iter().collect();
        ordered.sort_unstable_by(|a, b| a.0.cmp(b.0));
        let mut hasher = crc32fast::Hasher::new();
        for (p, e) in ordered {
            hasher.update(&e.crc32.to_le_bytes());
            hasher.update(p.as_bytes());
        }

        Ok(Self {
            inner: Arc::new(ReaderInner {
                path: path.to_path_buf(),
                chunks,
                entries,
                identity: PackageIdentity {
                    name: package_name.to_string(),
                    version: package_version.to_string(),
                    checksum: hasher.finalize(),
                },
                inflate_cache: Mutex::new(LruCache::new(
                    NonZeroUsize::new(INFLATE_CACHE_CAPACITY).unwrap(),
                )),
            }),
        })
    }

    /// Read a byte range, decompressing only overlapping chunks.
    pub fn read_range(
        &self,
        relative_path: &str,
        position: u64,
        length: Option<u64>,
    ) -> Result<Vec<u8>, PackageError> {
        let entry =
            self.inner.entries.get(relative_path).ok_or_else(|| {
                io::Error::new(io::ErrorKind::NotFound, relative_path.to_string())
            })?;

        if position >= entry.raw_size || length == Some(0) {
            return Ok(Vec::new());
        }
        let end = match length {
            // `saturating_add` so a caller-supplied `len` near `u64::MAX`
            // can't overflow (debug panic / release wrap → later
            // `end - position` underflow). This is a public API; it must
            // stay sound without relying on the JS-layer MAX_READ_LENGTH clamp.
            Some(len) => position.saturating_add(len).min(entry.raw_size),
            None => entry.raw_size,
        };
        if entry.chunk_count == 0 {
            return Ok(Vec::new());
        }

        // Inflate-cache fast path: if we previously decompressed this
        // entry in full, slice the requested range out of the cached
        // bytes. Avoids both the file open + seek and the zstd inflate
        // for the menu-switch repeat-read pattern.
        let cacheable = entry.raw_size <= INFLATE_CACHE_MAX_ENTRY_BYTES;
        if cacheable {
            if let Some(full) = self
                .inner
                .inflate_cache
                .lock()
                .get(relative_path)
                .map(Arc::clone)
            {
                let lo = position as usize;
                let hi = end as usize;
                return Ok(full[lo..hi].to_vec());
            }
        }

        // `PackageReader::open` validates that a non-empty entry's first
        // chunk has a non-zero `raw_size`, so `chunk_size` is > 0 here.
        // Guard anyway (defense in depth) so a future code path that
        // bypasses open-time validation degrades to an error, never a
        // divide-by-zero panic.
        let chunk_size = self.inner.chunks[entry.first_chunk as usize].raw_size as u64;
        if chunk_size == 0 {
            return Err(PackageError::BadIndex(format!(
                "entry '{relative_path}' first chunk has zero raw_size"
            )));
        }
        let first_needed = (position / chunk_size) as u32;
        let last_needed = ((end - 1) / chunk_size) as u32;

        let mut result = Vec::with_capacity((end - position) as usize);
        let mut file = std::fs::File::open(&self.inner.path)?;

        for i in first_needed..=last_needed.min(entry.chunk_count - 1) {
            let ci = (entry.first_chunk + i) as usize;
            let chunk = &self.inner.chunks[ci];

            file.seek(SeekFrom::Start(chunk.data_offset))?;
            let mut compressed = vec![0u8; chunk.compressed_size as usize];
            file.read_exact(&mut compressed)?;

            // Cap decompress output at raw_size (already validated <= MAX_CHUNK_RAW_SIZE).
            let max_output = (chunk.raw_size as usize).min(MAX_CHUNK_RAW_SIZE as usize);
            let decompressed = zstd::bulk::decompress(&compressed, max_output).map_err(|e| {
                PackageError::Io(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("zstd decompress chunk {ci}: {e}"),
                ))
            })?;

            let chunk_offset = i as u64 * chunk_size;
            let copy_start = if position > chunk_offset {
                (position - chunk_offset) as usize
            } else {
                0
            };
            let copy_end = if end < chunk_offset + decompressed.len() as u64 {
                (end - chunk_offset) as usize
            } else {
                decompressed.len()
            };
            if copy_start < copy_end {
                result.extend_from_slice(&decompressed[copy_start..copy_end]);
            }
        }
        // Populate the inflate cache when the request covered the
        // entire entry. We don't cache partial reads even if the entry
        // is small — the next reader of the rest of the entry would
        // miss anyway, and a separate full-read would have to do its
        // own decompress + populate.
        if cacheable && position == 0 && end == entry.raw_size {
            let arc = Arc::new(result.clone());
            self.inner
                .inflate_cache
                .lock()
                .put(relative_path.to_string(), arc);
        }
        Ok(result)
    }

    /// Read entire entry + CRC32 verify.
    pub fn read_entry(&self, relative_path: &str) -> Result<Vec<u8>, PackageError> {
        let data = self.read_range(relative_path, 0, None)?;
        let entry = self.inner.entries.get(relative_path).unwrap();
        let actual = crc32fast::hash(&data);
        if actual != entry.crc32 {
            return Err(PackageError::ChecksumMismatch {
                path: relative_path.to_string(),
                expected: entry.crc32,
                actual,
            });
        }
        Ok(data)
    }

    /// Read range with inflate size limit.
    ///
    /// Each chunk is capped at `MAX_CHUNK_RAW_SIZE` during decompress.
    /// The `max_inflate` parameter is additionally checked against the
    /// entry's total raw_size to reject obviously oversized reads.
    pub fn read_range_limited(
        &self,
        relative_path: &str,
        position: u64,
        length: Option<u64>,
        max_inflate: u64,
    ) -> Result<Vec<u8>, PackageError> {
        let entry =
            self.inner.entries.get(relative_path).ok_or_else(|| {
                io::Error::new(io::ErrorKind::NotFound, relative_path.to_string())
            })?;
        // Reject if the entry's total uncompressed size exceeds the limit.
        if entry.raw_size > max_inflate {
            return Err(PackageError::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "entry '{}' raw_size {} exceeds limit {}",
                    relative_path, entry.raw_size, max_inflate
                ),
            )));
        }
        self.read_range(relative_path, position, length)
    }

    pub fn contains(&self, path: &str) -> bool {
        self.inner.entries.contains_key(path)
    }
    pub fn identity(&self) -> &PackageIdentity {
        &self.inner.identity
    }
    pub fn entry_count(&self) -> usize {
        self.inner.entries.len()
    }
    pub fn entry_raw_size(&self, path: &str) -> Option<u64> {
        self.inner.entries.get(path).map(|e| e.raw_size)
    }
    pub fn entry_paths(&self) -> impl Iterator<Item = &str> {
        self.inner.entries.keys().map(String::as_str)
    }
    pub fn package_path(&self) -> &Path {
        &self.inner.path
    }

    /// Compute file info (size + digest) by streaming through chunks.
    pub fn get_file_info(
        &self,
        relative_path: &str,
        algorithm: &str,
    ) -> Result<(u64, String), PackageError> {
        let entry =
            self.inner.entries.get(relative_path).ok_or_else(|| {
                io::Error::new(io::ErrorKind::NotFound, relative_path.to_string())
            })?;
        let mut hasher = HashState::new(algorithm)?;
        let mut file = std::fs::File::open(&self.inner.path)?;
        let mut total = 0u64;
        for i in 0..entry.chunk_count {
            let ci = (entry.first_chunk + i) as usize;
            let chunk = &self.inner.chunks[ci];
            file.seek(SeekFrom::Start(chunk.data_offset))?;
            let mut compressed = vec![0u8; chunk.compressed_size as usize];
            file.read_exact(&mut compressed)?;
            let decompressed = zstd::bulk::decompress(&compressed, chunk.raw_size as usize)
                .map_err(|e| {
                    PackageError::Io(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("zstd: {e}"),
                    ))
                })?;
            hasher.update(&decompressed);
            total += decompressed.len() as u64;
        }
        Ok((total, hasher.finalize_hex()))
    }
}

impl Clone for PackageReader {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

// ---------------------------------------------------------------------------
// PackSource — MountBackend
// ---------------------------------------------------------------------------

pub struct PackSource {
    reader: PackageReader,
    /// Computed once at construction, not per lookup: `source_identity` is read
    /// on every `/code` resolve of a pack-backed path.
    cache_identity: u64,
}

impl PackSource {
    /// Open a package and derive its identity from its bytes.
    pub fn open(path: &Path, name: &str, version: &str) -> Result<Self, PackageError> {
        let digest = PackageDigest::of_file(path)?;
        Self::open_with_recorded_digest(path, name, version, &digest)
    }

    /// Open a package whose digest a runtime-written record already holds, which
    /// costs no read of the payload.
    ///
    /// The digest must be over *this file's* bytes. Only records the runtime owns
    /// may supply one: a digest the game could write would be a label it chooses,
    /// which is what [`PackageDigest`] exists to stop being. The install store is
    /// outside every VFS mapping for that reason — see
    /// [`GamePaths::sandbox_cache_dir`](super::GamePaths::sandbox_cache_dir).
    pub fn open_with_recorded_digest(
        path: &Path,
        name: &str,
        version: &str,
        digest: &PackageDigest,
    ) -> Result<Self, PackageError> {
        Ok(Self {
            reader: PackageReader::open(path, name, version)?,
            cache_identity: digest.cache_identity(),
        })
    }

    pub fn identity(&self) -> &PackageIdentity {
        self.reader.identity()
    }
    pub fn reader(&self) -> &PackageReader {
        &self.reader
    }
}

impl fmt::Debug for PackSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PackSource")
            .field("identity", self.reader.identity())
            .field("entries", &self.reader.entry_count())
            .finish()
    }
}

impl super::MountBackend for PackSource {
    /// A digest of the package's bytes, which means the same thing in every
    /// Session -- unlike a position in one Session's mount table. Two Sessions
    /// that mounted byte-identical packages get the same value and may share
    /// decoded bytes; a package built to resemble another's entry metadata cannot
    /// reach it without holding that package's bytes.
    fn source_identity(&self) -> Option<u64> {
        Some(self.cache_identity)
    }

    fn read(&self, p: &str) -> io::Result<Vec<u8>> {
        self.reader.read_entry(p).map_err(|e| match e {
            PackageError::Io(io_err) => io_err,
            other => io::Error::new(io::ErrorKind::Other, other.to_string()),
        })
    }
    fn exists(&self, p: &str) -> bool {
        self.reader.contains(p)
    }
    fn real_path(&self, _: &str) -> Option<PathBuf> {
        None
    }
    fn root_dir(&self) -> Option<&Path> {
        None
    }

    fn is_file(&self, p: &str) -> bool {
        self.reader.contains(p)
    }
    fn is_dir(&self, p: &str) -> bool {
        if p.is_empty() {
            return true;
        }
        let prefix = format!("{p}/");
        self.reader.entry_paths().any(|e| e.starts_with(&prefix))
    }
    fn list_dir(&self, dir: &str) -> Vec<String> {
        let prefix = if dir.is_empty() {
            String::new()
        } else {
            format!("{dir}/")
        };
        let mut seen = std::collections::HashSet::new();
        for path in self.reader.entry_paths() {
            let tail = if prefix.is_empty() {
                path
            } else {
                match path.strip_prefix(&prefix) {
                    Some(t) => t,
                    None => continue,
                }
            };
            if let Some(name) = tail.split('/').next() {
                if !name.is_empty() {
                    seen.insert(name.to_string());
                }
            }
        }
        seen.into_iter().collect()
    }
    fn entry_size(&self, p: &str) -> Option<u64> {
        self.reader.entry_raw_size(p)
    }

    fn read_range(&self, p: &str, pos: u64, len: Option<u64>) -> io::Result<Vec<u8>> {
        self.reader.read_range(p, pos, len).map_err(|e| match e {
            PackageError::Io(io_err) => io_err,
            other => io::Error::new(io::ErrorKind::Other, other.to_string()),
        })
    }
    fn read_range_limited(
        &self,
        p: &str,
        pos: u64,
        len: Option<u64>,
        max: u64,
    ) -> io::Result<Vec<u8>> {
        self.reader
            .read_range_limited(p, pos, len, max)
            .map_err(|e| match e {
                PackageError::Io(io_err) => io_err,
                other => io::Error::new(io::ErrorKind::Other, other.to_string()),
            })
    }
    fn get_file_info(&self, p: &str, algorithm: &str) -> io::Result<(u64, String)> {
        self.reader
            .get_file_info(p, algorithm)
            .map_err(|e| match e {
                PackageError::Io(io_err) => io_err,
                other => io::Error::new(io::ErrorKind::Other, other.to_string()),
            })
    }
    fn copy_to_writer(&self, p: &str, writer: &mut dyn io::Write) -> io::Result<()> {
        let data = self.read(p)?;
        writer.write_all(&data)
    }
}

// ---------------------------------------------------------------------------
// validate_package
// ---------------------------------------------------------------------------

pub fn validate_package(
    path: &Path,
    verify_checksums: bool,
) -> Result<PackageIdentity, PackageError> {
    let reader = PackageReader::open(path, "", "")?;
    if verify_checksums {
        for p in reader.entry_paths().collect::<Vec<_>>() {
            reader.read_entry(p)?; // CRC32 verified inside
        }
    }
    Ok(reader.identity().clone())
}

// ---------------------------------------------------------------------------
// Optional signature verifier (trust chain)
// ---------------------------------------------------------------------------

use std::sync::OnceLock;

/// Verifier function that the host can register once at startup to
/// establish a *trust chain* for subpackage installs.
///
/// The runtime calls it as `verify(pkg_bytes, manifest_bytes, signature_bytes)`
/// and refuses to mount the package unless the verifier returns `Ok(())`.
/// Host apps that want the old "CDN-trusted" behaviour can simply
/// leave the verifier unregistered; the runtime warns about this
/// once at startup so the absence of a trust chain is auditable.
///
/// `pkg_bytes` is the full `.mpkg` file contents; `manifest` and
/// `signature` are whatever opaque bytes the host's `SubpackageHandler`
/// returned alongside the package path. The runtime treats them as
/// opaque: the host controls the manifest schema, the signature
/// format, and the trusted keyring.
pub type SignatureVerifier =
    fn(pkg_bytes: &[u8], manifest: &[u8], signature: &[u8]) -> Result<(), String>;

static PACKAGE_SIGNATURE_VERIFIER: OnceLock<SignatureVerifier> = OnceLock::new();

/// Register the signature verifier. Returns `true` on first
/// registration, `false` if a verifier was already set.
pub fn register_signature_verifier(f: SignatureVerifier) -> bool {
    PACKAGE_SIGNATURE_VERIFIER.set(f).is_ok()
}

/// Run the registered verifier, if any. When no verifier has been
/// registered the runtime surfaces a one-shot warning and accepts
/// the package — that matches the previous "host said so, so it's
/// fine" behaviour so existing deployments keep working while the
/// trust chain is being rolled out.
///
/// **New code should always register a verifier.** Call sites that
/// get an `Err(_)` back from this must abort the install: the runtime
/// provides no silent fallback.
pub fn verify_package_signature(
    pkg_bytes: &[u8],
    manifest: Option<&[u8]>,
    signature: Option<&[u8]>,
) -> Result<(), PackageError> {
    static MISSING_WARNED: OnceLock<()> = OnceLock::new();
    match PACKAGE_SIGNATURE_VERIFIER.get() {
        Some(verify) => {
            let manifest = manifest.ok_or_else(|| {
                PackageError::BadIndex(
                    "signature verifier registered but host did not supply manifest bytes"
                        .to_string(),
                )
            })?;
            let signature = signature.ok_or_else(|| {
                PackageError::BadIndex(
                    "signature verifier registered but host did not supply signature bytes"
                        .to_string(),
                )
            })?;
            verify(pkg_bytes, manifest, signature).map_err(PackageError::BadIndex)
        }
        None => {
            MISSING_WARNED.get_or_init(|| {
                tracing::warn!(
                    "package signature verifier not registered — subpackages are being \
                     mounted on host-supplied integrity only. Call \
                     shared::vfs::package::register_signature_verifier once at startup \
                     to enable runtime-side verification."
                );
            });
            Ok(())
        }
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("migo_pkg_test_{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    // -- Path validation --

    #[test]
    fn valid_paths() {
        assert!(validate_entry_path("main.js").is_ok());
        assert!(validate_entry_path("lib/utils.js").is_ok());
    }

    #[test]
    fn reject_traversal() {
        assert!(validate_entry_path("../escape").is_err());
        assert!(validate_entry_path("a/../../b").is_err());
    }

    #[test]
    fn reject_absolute() {
        assert!(validate_entry_path("/etc/passwd").is_err());
    }

    #[test]
    fn reject_empty() {
        assert!(validate_entry_path("").is_err());
    }

    #[test]
    fn reject_backslash() {
        assert!(validate_entry_path("a\\b").is_err());
    }

    #[test]
    fn reject_duplicate_in_writer() {
        let mut buf = io::Cursor::new(Vec::new());
        let mut w = PackageWriter::new(&mut buf).unwrap();
        w.add_entry("a.js", b"x").unwrap();
        assert!(w.add_entry("a.js", b"y").is_err());
    }

    #[test]
    fn reject_prefix_conflict() {
        let mut buf = io::Cursor::new(Vec::new());
        let mut w = PackageWriter::new(&mut buf).unwrap();
        w.add_entry("a", b"file").unwrap();
        assert!(w.add_entry("a/b.txt", b"child").is_err());
    }

    #[test]
    fn path_conflict_semantics_are_pinned() {
        // Exact duplicate paths conflict.
        let mut duplicate_buf = io::Cursor::new(Vec::new());
        let mut duplicate = PackageWriter::new(&mut duplicate_buf).unwrap();
        duplicate.add_entry("a", b"one").unwrap();
        assert!(duplicate.add_entry("a", b"two").is_err());

        // A file cannot also be a directory prefix, in either insertion order.
        let mut parent_first_buf = io::Cursor::new(Vec::new());
        let mut parent_first = PackageWriter::new(&mut parent_first_buf).unwrap();
        parent_first.add_entry("a", b"file").unwrap();
        assert!(parent_first.add_entry("a/b", b"child").is_err());

        let mut child_first_buf = io::Cursor::new(Vec::new());
        let mut child_first = PackageWriter::new(&mut child_first_buf).unwrap();
        child_first.add_entry("a/b", b"child").unwrap();
        assert!(child_first.add_entry("a", b"file").is_err());

        // A path is not a prefix merely because it shares characters.
        let mut adjacent_buf = io::Cursor::new(Vec::new());
        let mut adjacent = PackageWriter::new(&mut adjacent_buf).unwrap();
        adjacent.add_entry("a", b"one").unwrap();
        adjacent.add_entry("ab", b"two").unwrap();

        // A sibling may sort before the slash in `a/`; the ordered successor
        // check must still find the descendant rather than stopping at `a!`.
        let mut punctuation_buf = io::Cursor::new(Vec::new());
        let mut punctuation = PackageWriter::new(&mut punctuation_buf).unwrap();
        punctuation.add_entry("a!", b"sibling").unwrap();
        punctuation.add_entry("a/b", b"child").unwrap();
        assert!(punctuation.add_entry("a", b"file").is_err());
    }

    /// The old scan allocated and compared every prior path for every entry,
    /// producing the quadratic allocation growth recorded by
    /// docs/audits/2026-09-09/full/io-network.md:35-39.  The shared crate already
    /// installs its counting allocator for tests, so use its per-thread event
    /// count as the deterministic growth proxy.
    #[test]
    fn path_conflict_growth_is_subquadratic() {
        fn insertion_events(count: usize) -> u64 {
            let names: Vec<String> = (0..count).map(|i| format!("entry_{i:05}.bin")).collect();
            let mut writer = PackageWriter::new(io::Cursor::new(Vec::new())).unwrap();
            let before = migo_alloc_probe::thread_counts();
            for name in &names {
                writer.add_entry(name, &[]).unwrap();
            }
            let after = migo_alloc_probe::thread_counts();
            after
                .allocation_events()
                .saturating_sub(before.allocation_events())
        }

        let events_1k = insertion_events(1_024);
        let events_2k = insertion_events(2_048);
        let events_4k = insertion_events(4_096);
        assert!(
            events_2k < events_1k * 3,
            "1k → 2k allocation growth is quadratic: {events_1k} → {events_2k}"
        );
        assert!(
            events_4k < events_2k * 3,
            "2k → 4k allocation growth is quadratic: {events_2k} → {events_4k}"
        );
    }

    // -- Crafted headers (what `PackageWriter` would never emit) --

    /// Assemble a package file byte for byte, so a test can state a header the
    /// writer cannot produce. The asserts pin the layout the caller described,
    /// so a mis-specified fixture fails here rather than inside `open`.
    fn crafted_package(
        entry_count: u32,
        chunk_table_offset: u64,
        index_offset: u64,
        data: &[u8],
        chunks: &[(u64, u32, u32)],
        index: &[u8],
    ) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&MAGIC);
        bytes.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
        bytes.extend_from_slice(&entry_count.to_le_bytes());
        bytes.extend_from_slice(&(chunks.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&chunk_table_offset.to_le_bytes());
        bytes.extend_from_slice(&index_offset.to_le_bytes());
        assert_eq!(bytes.len() as u64, HEADER_SIZE);
        bytes.extend_from_slice(data);
        assert_eq!(
            bytes.len() as u64,
            chunk_table_offset,
            "fixture: data must end where the chunk table begins"
        );
        for (data_offset, compressed_size, raw_size) in chunks {
            bytes.extend_from_slice(&data_offset.to_le_bytes());
            bytes.extend_from_slice(&compressed_size.to_le_bytes());
            bytes.extend_from_slice(&raw_size.to_le_bytes());
        }
        assert_eq!(
            bytes.len() as u64,
            index_offset,
            "fixture: chunk table must end where the index begins"
        );
        bytes.extend_from_slice(index);
        bytes
    }

    fn index_record(path: &str, raw_size: u64, first_chunk: u32, chunk_count: u32) -> Vec<u8> {
        let mut record = Vec::new();
        record.extend_from_slice(&(path.len() as u16).to_le_bytes());
        record.extend_from_slice(path.as_bytes());
        record.extend_from_slice(&raw_size.to_le_bytes());
        record.extend_from_slice(&0u32.to_le_bytes()); // crc32: `open` does not check it
        record.extend_from_slice(&first_chunk.to_le_bytes());
        record.extend_from_slice(&chunk_count.to_le_bytes());
        record
    }

    fn open_crafted(name: &str, bytes: &[u8]) -> Result<PackageReader, PackageError> {
        let dir = make_test_dir(name);
        let path = dir.join("crafted.mpkg");
        std::fs::write(&path, bytes).unwrap();
        PackageReader::open(&path, "crafted", "1.0")
    }

    /// The declared entry count sizes a `HashMap`, so it has to be bounded by
    /// what the file can actually hold.
    ///
    /// Note what failure looks like without the bound: not a failing assertion
    /// but an aborted test binary, because `HashMap::with_capacity(u32::MAX)`
    /// asks the allocator for hundreds of gigabytes. That is also what it did in
    /// production, from a 32-byte file.
    #[test]
    fn an_entry_count_larger_than_the_index_region_is_refused() {
        let bytes = crafted_package(u32::MAX, HEADER_SIZE, HEADER_SIZE, &[], &[], &[]);
        assert_eq!(
            bytes.len() as u64,
            HEADER_SIZE,
            "the whole file is a header"
        );

        match open_crafted("entry_count_bound", &bytes) {
            Err(PackageError::BadIndex(msg)) => {
                assert!(msg.contains("entry_count"), "wrong rejection: {msg}");
            }
            Err(other) => panic!("rejected for an unrelated reason: {other}"),
            Ok(_) => panic!("a 32-byte package declaring u32::MAX entries was accepted"),
        }
    }

    /// `read_range` locates chunk *i* at `i * first_chunk.raw_size`, so a chunk
    /// table whose interior chunks disagree about that stride puts every offset
    /// after the first one in the wrong place.
    ///
    /// The pre-existing sum check cannot see this: 10 + 20 + 10 totals the same
    /// 40 that a well-formed 20 + 20 would, which is why the fixture is three
    /// chunks and not two -- with two, the first chunk's size *is* the second
    /// one's offset, so the stride assumption holds no matter what the sizes are.
    #[test]
    fn a_chunk_table_that_breaks_its_own_stride_is_refused() {
        let chunks = [
            (HEADER_SIZE, 1u32, 10u32),
            (HEADER_SIZE + 1, 1, 20),
            (HEADER_SIZE + 2, 1, 10),
        ];
        let chunk_table_offset = HEADER_SIZE + 3;
        let index_offset = chunk_table_offset + 3 * 16;
        let index = index_record("a.js", 40, 0, 3);
        let bytes = crafted_package(
            1,
            chunk_table_offset,
            index_offset,
            &[0, 0, 0],
            &chunks,
            &index,
        );

        match open_crafted("chunk_stride", &bytes) {
            Err(PackageError::BadIndex(msg)) => {
                assert!(msg.contains("stride"), "wrong rejection: {msg}");
            }
            Err(other) => panic!("rejected for an unrelated reason: {other}"),
            Ok(_) => panic!("a chunk table that breaks its own stride was accepted"),
        }
    }

    /// The positive control for both fixtures above: the same assembly path,
    /// with a uniform stride and an honest entry count, opens. Without this a
    /// `crafted_package` that produced garbage would satisfy every rejection
    /// assertion here while proving nothing about either guard.
    #[test]
    fn a_crafted_package_with_a_uniform_stride_opens() {
        let chunks = [
            (HEADER_SIZE, 1u32, 20u32),
            (HEADER_SIZE + 1, 1, 20),
            (HEADER_SIZE + 2, 1, 10),
        ];
        let chunk_table_offset = HEADER_SIZE + 3;
        let index_offset = chunk_table_offset + 3 * 16;
        let index = index_record("a.js", 50, 0, 3);
        let bytes = crafted_package(
            1,
            chunk_table_offset,
            index_offset,
            &[0, 0, 0],
            &chunks,
            &index,
        );

        let reader = open_crafted("uniform_stride", &bytes)
            .expect("a uniform stride with an honest entry count must open");
        assert_eq!(reader.entry_count(), 1);
    }

    // -- Roundtrip --

    #[test]
    fn roundtrip_small() {
        let dir = make_test_dir("rt_small");
        let p = dir.join("test.mpkg");
        {
            let f = std::fs::File::create(&p).unwrap();
            let mut w = PackageWriter::new(io::BufWriter::new(f)).unwrap();
            w.add_entry("main.js", b"console.log('hello')").unwrap();
            w.add_entry("lib/utils.js", b"export function x() {}")
                .unwrap();
            w.finish("test", "1.0").unwrap();
        }
        let r = PackageReader::open(&p, "test", "1.0").unwrap();
        assert_eq!(r.entry_count(), 2);
        assert_eq!(r.read_entry("main.js").unwrap(), b"console.log('hello')");
        assert_eq!(
            r.read_entry("lib/utils.js").unwrap(),
            b"export function x() {}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn roundtrip_large_chunked() {
        let dir = make_test_dir("rt_large");
        let p = dir.join("test.mpkg");
        let content: Vec<u8> = (0..200_000).map(|i| (i % 256) as u8).collect();
        {
            let f = std::fs::File::create(&p).unwrap();
            let mut w = PackageWriter::new(io::BufWriter::new(f)).unwrap();
            w.add_entry("big.bin", &content).unwrap();
            w.finish("test", "1.0").unwrap();
        }
        let r = PackageReader::open(&p, "test", "1.0").unwrap();
        assert_eq!(r.read_entry("big.bin").unwrap(), content);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Contract test for [`PackageWriter::add_entry_streaming`]:
    /// result must be **byte-identical** to the non-streaming
    /// `add_entry` path given the same input bytes.
    #[test]
    fn streaming_add_matches_buffered_add_bit_for_bit() {
        let dir = make_test_dir("stream_vs_buffered");
        let p_stream = dir.join("stream.mpkg");
        let p_buffer = dir.join("buffer.mpkg");

        // ~140 KB -> crosses default 64 KiB chunk boundary at least once.
        let content: Vec<u8> = (0..140_000u32)
            .map(|i| i.wrapping_mul(2654435761) as u8)
            .collect();

        {
            let f = std::fs::File::create(&p_stream).unwrap();
            let mut w = PackageWriter::new(io::BufWriter::new(f)).unwrap();
            // Wrap in a Cursor so the reader has an owned source.
            let src = std::io::Cursor::new(content.clone());
            w.add_entry_streaming("big.bin", src, u64::MAX).unwrap();
            w.finish("test", "1.0").unwrap();
        }
        {
            let f = std::fs::File::create(&p_buffer).unwrap();
            let mut w = PackageWriter::new(io::BufWriter::new(f)).unwrap();
            w.add_entry("big.bin", &content).unwrap();
            w.finish("test", "1.0").unwrap();
        }

        let r_stream = PackageReader::open(&p_stream, "test", "1.0").unwrap();
        let r_buffer = PackageReader::open(&p_buffer, "test", "1.0").unwrap();
        let s_bytes = r_stream.read_entry("big.bin").unwrap();
        let b_bytes = r_buffer.read_entry("big.bin").unwrap();
        assert_eq!(s_bytes, content, "streaming entry content mismatch");
        assert_eq!(b_bytes, content, "buffered entry content mismatch");
        assert_eq!(s_bytes, b_bytes, "streaming vs buffered differ");

        // Bonus: exercise cross-chunk random access on the streamed
        // package to prove the chunk index is well-formed.
        let middle = r_stream
            .read_range("big.bin", 60_000, Some(40_000))
            .unwrap();
        assert_eq!(middle, &content[60_000..100_000]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn streaming_add_rejects_oversized_entry_midstream() {
        let dir = make_test_dir("stream_limit");
        let p = dir.join("test.mpkg");
        let content: Vec<u8> = vec![0xAA; 130_000];

        let f = std::fs::File::create(&p).unwrap();
        let mut w = PackageWriter::new(io::BufWriter::new(f)).unwrap();
        let src = std::io::Cursor::new(content);
        let err = w
            .add_entry_streaming("big.bin", src, 64_000)
            .expect_err("must reject once past the cap");
        // Must be an IO/InvalidData-flavoured error.
        assert!(
            matches!(err, PackageError::Io(_)),
            "wrong error variant: {err:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn streaming_add_empty_source_writes_zero_byte_entry() {
        let dir = make_test_dir("stream_empty");
        let p = dir.join("test.mpkg");
        {
            let f = std::fs::File::create(&p).unwrap();
            let mut w = PackageWriter::new(io::BufWriter::new(f)).unwrap();
            let src: &[u8] = &[];
            w.add_entry_streaming("empty.bin", src, u64::MAX).unwrap();
            w.finish("test", "1.0").unwrap();
        }
        let r = PackageReader::open(&p, "test", "1.0").unwrap();
        let bytes = r.read_entry("empty.bin").unwrap();
        assert!(bytes.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    // -- Random access --

    #[test]
    fn random_read_cross_chunk() {
        let dir = make_test_dir("cross_chunk");
        let p = dir.join("test.mpkg");
        let content: Vec<u8> = (0..200_000).map(|i| (i % 256) as u8).collect();
        {
            let f = std::fs::File::create(&p).unwrap();
            let mut w = PackageWriter::with_chunk_size(io::BufWriter::new(f), 1024).unwrap();
            w.add_entry("data.bin", &content).unwrap();
            w.finish("test", "1.0").unwrap();
        }
        let r = PackageReader::open(&p, "test", "1.0").unwrap();
        // Cross multiple 1KB chunks.
        assert_eq!(
            r.read_range("data.bin", 500, Some(3000)).unwrap(),
            &content[500..3500]
        );
        // Near end.
        assert_eq!(
            r.read_range("data.bin", 199_000, None).unwrap(),
            &content[199_000..]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_range_huge_length_does_not_overflow() {
        // Regression: `position + len` must not overflow u64 for a
        // caller-supplied length near the max. Before the
        // `saturating_add` fix this panicked in debug / wrapped in
        // release, corrupting the `end` bound.
        let dir = make_test_dir("read_range_overflow");
        let p = dir.join("test.mpkg");
        let content: Vec<u8> = (0..1000).map(|i| (i % 256) as u8).collect();
        {
            let f = std::fs::File::create(&p).unwrap();
            let mut w = PackageWriter::new(io::BufWriter::new(f)).unwrap();
            w.add_entry("data.bin", &content).unwrap();
            w.finish("test", "1.0").unwrap();
        }
        let r = PackageReader::open(&p, "test", "1.0").unwrap();
        // position within bounds, length so large that position+length
        // overflows u64 without saturation. Must clamp to entry size.
        let got = r.read_range("data.bin", 100, Some(u64::MAX)).unwrap();
        assert_eq!(got, &content[100..]);
        // position + len overflow with position 0 too.
        let all = r.read_range("data.bin", 0, Some(u64::MAX - 1)).unwrap();
        assert_eq!(all, content);
        // read_range_limited delegates to read_range, so the same
        // saturation must hold once the max_inflate gate is passed.
        let limited = r
            .read_range_limited("data.bin", 100, Some(u64::MAX), u64::MAX)
            .unwrap();
        assert_eq!(limited, &content[100..]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn random_read_empty() {
        let dir = make_test_dir("empty_read");
        let p = dir.join("test.mpkg");
        {
            let f = std::fs::File::create(&p).unwrap();
            let mut w = PackageWriter::new(io::BufWriter::new(f)).unwrap();
            w.add_entry("x.txt", b"hello").unwrap();
            w.finish("test", "1.0").unwrap();
        }
        let r = PackageReader::open(&p, "test", "1.0").unwrap();
        assert!(r.read_range("x.txt", 100, None).unwrap().is_empty());
        assert!(r.read_range("x.txt", 0, Some(0)).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    // -- Checksum --

    #[test]
    fn checksum_mismatch() {
        let dir = make_test_dir("crc_bad");
        let p = dir.join("test.mpkg");
        {
            let f = std::fs::File::create(&p).unwrap();
            let mut w = PackageWriter::new(io::BufWriter::new(f)).unwrap();
            w.add_entry("a.js", b"correct").unwrap();
            w.finish("test", "1.0").unwrap();
        }
        // Corrupt chunk data area.
        {
            let mut f = std::fs::OpenOptions::new().write(true).open(&p).unwrap();
            f.seek(SeekFrom::Start(HEADER_SIZE)).unwrap();
            f.write_all(b"CORRUPT").unwrap();
        }
        // Corruption may be caught at open (chunk bounds) or read (decompress/CRC).
        let result = PackageReader::open(&p, "test", "1.0").and_then(|r| r.read_entry("a.js"));
        assert!(result.is_err(), "corrupted package must fail");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bad_magic() {
        let dir = make_test_dir("bad_magic");
        let p = dir.join("test.mpkg");
        std::fs::write(&p, b"NOT_MPKG_HEADER_PADDING_TO_32B!!").unwrap();
        assert!(matches!(
            PackageReader::open(&p, "t", "1"),
            Err(PackageError::BadMagic)
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    // -- PackSource as MountBackend --

    #[test]
    fn pack_source_mount() {
        use super::super::MountBackend;
        let dir = make_test_dir("pack_mount");
        let p = dir.join("test.mpkg");
        {
            let f = std::fs::File::create(&p).unwrap();
            let mut w = PackageWriter::new(io::BufWriter::new(f)).unwrap();
            w.add_entry("game.js", b"// game").unwrap();
            w.add_entry("res/bg.png", b"\x89PNG").unwrap();
            w.finish("test", "1.0").unwrap();
        }
        let src = PackSource::open(&p, "test", "1.0").unwrap();
        assert!(src.exists("game.js"));
        assert!(src.exists("res/bg.png"));
        assert!(!src.exists("nope"));
        assert_eq!(src.read("game.js").unwrap(), b"// game");
        assert!(src.real_path("game.js").is_none());
        assert!(src.root_dir().is_none());
        assert!(src.is_file("game.js"));
        assert!(src.is_dir("res"));
        assert!(!src.is_dir("game.js"));
        let listing = src.list_dir("");
        assert!(listing.contains(&"game.js".to_string()));
        assert!(listing.contains(&"res".to_string()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    // -- Writer poisoning --

    #[test]
    fn writer_poisoned() {
        struct FailWriter {
            count: usize,
            limit: usize,
            buf: Vec<u8>,
        }
        impl io::Write for FailWriter {
            fn write(&mut self, b: &[u8]) -> io::Result<usize> {
                self.buf.write(b)
            }
            fn write_all(&mut self, b: &[u8]) -> io::Result<()> {
                self.count += 1;
                if self.count > self.limit {
                    return Err(io::Error::new(io::ErrorKind::Other, "fail"));
                }
                self.buf.write_all(b)
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        impl io::Seek for FailWriter {
            fn seek(&mut self, _: SeekFrom) -> io::Result<u64> {
                Ok(0)
            }
        }
        // Header writes: magic(1) + ver(1) + entry_count(1) + chunk_count(1) + ct_offset(1) + idx_offset(1) = 6
        let mut fw = FailWriter {
            count: 0,
            limit: 6,
            buf: Vec::new(),
        };
        let mut w = PackageWriter::new(&mut fw).unwrap();
        assert!(w.add_entry("a.txt", b"data").is_err());
        let err = w.add_entry("b.txt", b"x");
        assert!(format!("{}", err.unwrap_err()).contains("poisoned"));
    }

    // -- Validate --

    #[test]
    fn validate_good() {
        let dir = make_test_dir("val_good");
        let p = dir.join("test.mpkg");
        {
            let f = std::fs::File::create(&p).unwrap();
            let mut w = PackageWriter::new(io::BufWriter::new(f)).unwrap();
            w.add_entry("a.js", b"hello").unwrap();
            w.finish("test", "1.0").unwrap();
        }
        assert!(validate_package(&p, true).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    // -- Identity --

    #[test]
    fn identity_differs() {
        let dir = make_test_dir("id_diff");
        let p1 = dir.join("v1.mpkg");
        let p2 = dir.join("v2.mpkg");
        {
            let f = std::fs::File::create(&p1).unwrap();
            let mut w = PackageWriter::new(io::BufWriter::new(f)).unwrap();
            w.add_entry("a.js", b"v1").unwrap();
            let id1 = w.finish("g", "1").unwrap();

            let f2 = std::fs::File::create(&p2).unwrap();
            let mut w2 = PackageWriter::new(io::BufWriter::new(f2)).unwrap();
            w2.add_entry("a.js", b"v2").unwrap();
            let id2 = w2.finish("g", "1").unwrap();

            assert_ne!(id1.checksum, id2.checksum);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Content whose CRC32 equals another's at the same length, which the
    /// format's per-entry integrity primitive cannot tell apart. Appending a
    /// message's own CRC32 in little-endian order drives the CRC of the result
    /// to a constant, so any two messages of equal length yield equal-length,
    /// equal-CRC32 contents.
    fn crc_twin(message: &[u8]) -> Vec<u8> {
        let mut out = message.to_vec();
        out.extend_from_slice(&crc32fast::hash(message).to_le_bytes());
        out
    }

    fn write_single_entry_package(path: &Path, entry: &str, content: &[u8]) {
        let f = std::fs::File::create(path).unwrap();
        let mut w = PackageWriter::new(io::BufWriter::new(f)).unwrap();
        w.add_entry(entry, content).unwrap();
        w.finish("shared-name", "1.0").unwrap();
    }

    #[test]
    fn packages_agreeing_on_entry_metadata_do_not_share_an_identity() {
        use super::super::MountBackend;
        let dir = make_test_dir("identity_is_content_bound");
        let victim_pkg = dir.join("victim.mpkg");
        let forged_pkg = dir.join("forged.mpkg");

        let victim = crc_twin(b"victim!!");
        let forged = crc_twin(b"forged!!");

        // The fixture is only the defect it claims if the two packages really do
        // agree on every field a metadata-level identity covers.
        assert_ne!(victim, forged);
        assert_eq!(victim.len(), forged.len());
        assert_eq!(crc32fast::hash(&victim), crc32fast::hash(&forged));

        write_single_entry_package(&victim_pkg, "res/logo.png", &victim);
        write_single_entry_package(&forged_pkg, "res/logo.png", &forged);

        let victim_src = PackSource::open(&victim_pkg, "shared-name", "1.0").unwrap();
        let forged_src = PackSource::open(&forged_pkg, "shared-name", "1.0").unwrap();
        assert_eq!(
            victim_src.entry_size("res/logo.png"),
            forged_src.entry_size("res/logo.png"),
        );

        assert_ne!(
            victim_src.source_identity(),
            forged_src.source_identity(),
            "a package matching another's entry paths, sizes and CRC32s claimed \
             its identity, so it would be served the other's decoded pixels",
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn byte_identical_packages_share_an_identity() {
        use super::super::MountBackend;
        let dir = make_test_dir("identity_is_shared");
        let one = dir.join("one.mpkg");
        let two = dir.join("two.mpkg");

        write_single_entry_package(&one, "res/logo.png", b"the same pixels");
        write_single_entry_package(&two, "res/logo.png", b"the same pixels");
        assert_eq!(std::fs::read(&one).unwrap(), std::fs::read(&two).unwrap());

        let first = PackSource::open(&one, "game-a", "1.0").unwrap();
        let second = PackSource::open(&two, "game-b", "9.9").unwrap();

        assert_eq!(
            first.source_identity(),
            second.source_identity(),
            "two Sessions holding the same bytes must still share one decoded copy",
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}

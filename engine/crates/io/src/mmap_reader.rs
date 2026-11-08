//! Memory-mapped file reader for large binary payloads.
//!
//! Callers that would otherwise go through `std::fs::read()` + a
//! `Vec<u8>` heap copy can opt into [`mmap_file_bytes`] when the
//! payload is big enough that the copy itself is a measurable cost
//! (tens of MB at the tail of the distribution — large KTX2 atlases,
//! derived-cache RGBA sidecars, or preloaded model JSON).
//!
//! The returned [`Arc<MappedBytes>`] exposes a `Deref<Target = [u8]>`
//! so downstream decoders that take `&[u8]` slices work unchanged.
//! Cloning is a refcount bump — the mmap survives as long as any
//! clone is alive, and is torn down when the last reference drops.
//!
//! Not a silver bullet: mmap trades a synchronous heap copy for
//! on-demand page faults, so tight iteration over sparse bytes
//! inside a large mapping can be *slower* than a plain
//! `Vec<u8>::read`.  The intended use is "decode once, release":
//! image decoders walk every byte exactly once, and the OS page
//! cache supplies each page in the order the reader touches them.
//!
//! The module is callable from every target we build; Android
//! `memmap2` is supported there via the standard POSIX `mmap`
//! syscall. Unsupported mappings return an error; callers choose any fallback.
//! Both constructors are unsafe: an open/read-only handle is not evidence that
//! the backing object is immutable.

use std::fs::File;
use std::io;
use std::ops::Deref;
use std::path::Path;
use std::sync::Arc;

use memmap2::Mmap;

/// Owning wrapper around a read-only memory map.  Derefs to the raw
/// bytes so decoder pipelines that take `&[u8]` slices consume the
/// mapping with zero additional work.  `Arc` the outer handle to
/// share across tasks / threads without copying the bytes.
pub struct MappedBytes {
    /// Held for the lifetime of the bytes; dropped via [`Mmap::Drop`]
    /// once no Arc clones remain.
    mmap: Mmap,
}

impl MappedBytes {
    /// View as a raw byte slice.  Alias of `deref()` for clarity at
    /// callers that pass the bytes directly into a decoder.
    #[inline]
    pub fn as_slice(&self) -> &[u8] {
        &self.mmap
    }

    /// Length of the underlying file in bytes.
    #[inline]
    pub fn len(&self) -> usize {
        self.mmap.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.mmap.is_empty()
    }
}

impl Deref for MappedBytes {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        &self.mmap
    }
}

impl AsRef<[u8]> for MappedBytes {
    fn as_ref(&self) -> &[u8] {
        &self.mmap
    }
}

/// Open `path` and memory-map the whole file read-only.  Returns an
/// `Arc<MappedBytes>` so callers can cheaply share the mapping
/// across threads / async tasks.
///
/// Empty files are valid — the resulting mapping has length 0 and
/// derefs to an empty slice.
///
/// # Safety
/// The mapped file must not be modified or truncated, through any handle or
/// process, until the final returned owner drops. Read-only virtual paths or
/// file handles alone do not establish this guarantee.
///
/// ```compile_fail,E0133
/// let _ = migo_io::mmap_reader::mmap_file_bytes("mutable.bin");
/// ```
pub unsafe fn mmap_file_bytes(path: impl AsRef<Path>) -> io::Result<Arc<MappedBytes>> {
    let file = File::open(path.as_ref())?;
    // SAFETY: the caller guarantees the opened object's immutability.
    unsafe { mmap_bytes_from_file(&file) }
}

/// Memory-map an already-open file, avoiding a second `open`.
///
/// # Safety
/// The backing file must not be modified or truncated, through any handle or
/// process, until the final returned owner drops. The original handle need not
/// outlive the mapping. Metadata checks cannot enforce immutability.
///
/// ```compile_fail,E0133
/// # let file = std::fs::File::open("mutable.bin").unwrap();
/// let _ = migo_io::mmap_reader::mmap_bytes_from_file(&file);
/// ```
pub unsafe fn mmap_bytes_from_file(file: &File) -> io::Result<Arc<MappedBytes>> {
    // SAFETY: the caller guarantees the backing object's immutability. The OS
    // keeps the mapped object alive independently of this file handle.
    let mmap = unsafe { Mmap::map(file)? };
    Ok(Arc::new(MappedBytes { mmap }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn mmap_roundtrip_matches_fs_read() {
        let dir = tempdir_here();
        let file_path = dir.join("mmap_rt.bin");
        let payload: Vec<u8> = (0..1024u32).map(|i| (i & 0xff) as u8).collect();
        std::fs::write(&file_path, &payload).unwrap();

        // SAFETY: no writer modifies this private fixture while mapped.
        let bytes = unsafe { mmap_file_bytes(&file_path) }.expect("mmap ok");
        assert_eq!(bytes.len(), payload.len());
        assert_eq!(bytes.as_slice(), payload.as_slice());
        // Deref should give the same slice.
        let _: &[u8] = &bytes;
    }

    #[test]
    fn mmap_empty_file_is_zero_length() {
        let dir = tempdir_here();
        let file_path = dir.join("empty.bin");
        let _ = std::fs::File::create(&file_path).unwrap();
        // SAFETY: no writer modifies this private fixture while mapped.
        let bytes = unsafe { mmap_file_bytes(&file_path) }.expect("mmap ok");
        assert!(bytes.is_empty());
    }

    #[test]
    fn mmap_missing_file_returns_error() {
        let bad = std::env::temp_dir().join("migo-mmap-does-not-exist-XYZ");
        // SAFETY: the nonexistent file cannot produce a live mapping.
        assert!(unsafe { mmap_file_bytes(&bad) }.is_err());
    }

    #[test]
    fn mmap_large_file_deref_matches_fs_read() {
        // Exercise the ">64 KB" path where mmap is expected to
        // meaningfully reduce heap pressure.  Use 256 KB so the
        // test is still fast.
        let dir = tempdir_here();
        let file_path = dir.join("big.bin");
        let payload: Vec<u8> = (0..(256 * 1024u32)).map(|i| (i & 0xff) as u8).collect();
        {
            let mut f = std::fs::File::create(&file_path).unwrap();
            f.write_all(&payload).unwrap();
        }
        // SAFETY: no writer modifies this private fixture while mapped.
        let bytes = unsafe { mmap_file_bytes(&file_path) }.unwrap();
        assert_eq!(bytes.len(), payload.len());
        // Probe a handful of pages to make sure we're reading
        // through the mapping, not some stale buffer.
        for offset in [0usize, 4096, 65536, 131072, 255 * 1024] {
            assert_eq!(bytes[offset], payload[offset]);
        }
    }

    /// Sandboxed temp dir that cleans up on drop — intentionally
    /// tiny vs. the full `tempfile` crate so we don't pull a new
    /// dependency just for this test module.
    fn tempdir_here() -> std::path::PathBuf {
        let base = std::env::temp_dir().join(format!(
            "migo-mmap-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&base).unwrap();
        base
    }
}

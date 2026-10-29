//! Pinned package reads use the same bounded media pool as regular files.
//! Construct/read on the Pack class; no new executor or whole-entry fallback.
use crate::{
    pools::IoPools,
    read_at_file::{ChunkPool, FILE_CHUNK_BYTES, FileChunk, check_extent, control_limit},
};
use parking_lot::Mutex;
use shared::vfs::{
    mount::PinnedPackRead,
    package::{PackageEntryReader, PackageReadLimits},
};
use std::io;

pub struct ReadAtPackage {
    source: Mutex<PackageEntryReader>,
    len: u64,
    max_control_bytes: usize,
    pool: ChunkPool,
}
impl ReadAtPackage {
    /// Reserve the worst-case persistent Rust payload before allocation, through
    /// the existing process byte budget. Includes both possible media slots.
    /// The zstd native context and shared package index are not included here.
    pub fn reservation_bytes(limits: PackageReadLimits) -> io::Result<u64> {
        limits
            .max_scratch_bytes()?
            .checked_add(2 * FILE_CHUNK_BYTES)
            .and_then(|n| u64::try_from(n).ok())
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "package reservation overflow")
            })
    }
    pub fn new(pin: PinnedPackRead, pools: &IoPools, max_control_bytes: usize) -> io::Result<Self> {
        control_limit(max_control_bytes)?;
        let limits = PackageReadLimits::default();
        let lease = pools
            .reserve_bytes(Self::reservation_bytes(limits)?)
            .map_err(io::Error::other)?;
        let source = pin.open_reader(limits)?;
        let len = source.len();
        Ok(Self {
            source: Mutex::new(source),
            len,
            max_control_bytes,
            pool: ChunkPool::new(Some(lease)),
        })
    }
    pub fn len(&self) -> u64 {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub fn read_exact_at(&self, offset: u64, len: usize) -> io::Result<Vec<u8>> {
        check_extent(self.len, offset, len, self.max_control_bytes)?;
        let mut output = vec![0; len];
        self.source
            .lock()
            .read_exact_at(offset, &mut output)
            .map_err(io::Error::other)?;
        Ok(output)
    }
    pub fn chunk_at(&self, offset: u64, len: usize) -> io::Result<FileChunk> {
        check_extent(self.len, offset, len, FILE_CHUNK_BYTES)?;
        self.pool.fill(len, |buffer| {
            self.source
                .lock()
                .read_exact_at(offset, buffer)
                .map_err(io::Error::other)
        })
    }
    /// Reuse the package reader's bounded scratch to fill a caller's buffer.
    /// On error, the caller must discard the buffer contents.
    pub fn read_into_at(&self, offset: u64, output: &mut [u8]) -> io::Result<()> {
        check_extent(self.len, offset, output.len(), self.max_control_bytes)?;
        self.source
            .lock()
            .read_exact_at(offset, output)
            .map_err(io::Error::other)
    }
}

#[cfg(test)]
mod tests;

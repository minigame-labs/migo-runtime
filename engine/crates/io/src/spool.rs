//! Bounded host-private temporary files, sealed before any reader is exposed.
//! Ephemeral page-cache writes need no crash-durability fsync. A disk ticket is
//! separate from the executor's in-memory ByteTicket and survives through the
//! final positioned-read owner, including an IO job outliving its caller.
use crate::read_at_file::{FILE_CHUNK_BYTES, ReadAtFile};
use parking_lot::Mutex;
use std::{
    fs::File,
    io::{self, Write},
    path::Path,
    sync::{Arc, OnceLock},
};

pub const DEFAULT_SPOOL_FILE_BYTES: u64 = 64 * 1024 * 1024;
const PROCESS_SPOOL_BYTES: u64 = 256 * 1024 * 1024;
// Every HTTP audio stream keeps its body here for its life (seek and loop
// replay from it), so the count follows the streams a game holds; the byte
// budget is what bounds the disk.
const PROCESS_SPOOL_FILES: usize = 64;
#[derive(Default)]
struct Usage {
    bytes: u64,
    files: usize,
}
struct Budget {
    max_bytes: u64,
    max_files: usize,
    used: Mutex<Usage>,
}
#[derive(Clone)]
pub struct SpoolBudget(Arc<Budget>);
impl SpoolBudget {
    pub fn new(max_bytes: u64, max_files: usize) -> io::Result<Self> {
        if max_bytes == 0 || max_files == 0 {
            return Err(invalid("invalid spool budget"));
        }
        Ok(Self(Arc::new(Budget {
            max_bytes,
            max_files,
            used: Mutex::new(Usage::default()),
        })))
    }
    pub fn process() -> Self {
        static VALUE: OnceLock<SpoolBudget> = OnceLock::new();
        VALUE
            .get_or_init(|| Self::new(PROCESS_SPOOL_BYTES, PROCESS_SPOOL_FILES).unwrap())
            .clone()
    }
    fn reserve(&self, bytes: u64) -> io::Result<DiskTicket> {
        let mut used = self.0.used.lock();
        let total = used
            .bytes
            .checked_add(bytes)
            .ok_or_else(|| invalid("spool quota overflow"))?;
        if total > self.0.max_bytes || used.files >= self.0.max_files {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "spool disk quota exhausted",
            ));
        }
        used.bytes = total;
        used.files += 1;
        Ok(DiskTicket {
            budget: self.clone(),
            bytes,
        })
    }
    #[cfg(test)]
    fn usage(&self) -> (u64, usize) {
        let used = self.0.used.lock();
        (used.bytes, used.files)
    }
}
struct DiskTicket {
    budget: SpoolBudget,
    bytes: u64,
}
impl DiskTicket {
    fn shrink_to(&mut self, bytes: u64) {
        assert!(bytes <= self.bytes);
        self.budget.0.used.lock().bytes -= self.bytes - bytes;
        self.bytes = bytes;
    }
}
impl Drop for DiskTicket {
    fn drop(&mut self) {
        let mut used = self.budget.0.used.lock();
        used.bytes -= self.bytes;
        used.files -= 1;
    }
}
fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

/// Writer operations belong on the existing Fs executor. The host supplies an
/// unmapped directory; this is not an API for a game-writable VFS pathname.
/// No file handle or temporary pathname is exposed, and seal consumes write
/// authority. Any failed append permanently poisons this writer.
/// A fixed-size spool file written and read at arbitrary offsets, for a media
/// object fetched out of order (HTTP ranges). The caller tracks which extents
/// hold data; the disk quota covers the whole object from creation.
pub struct SparseSpool {
    file: File,
    _ticket: DiskTicket,
    len: u64,
}
impl SparseSpool {
    pub fn create(dir: &Path, budget: &SpoolBudget, len: u64, max_bytes: u64) -> io::Result<Self> {
        if !dir.is_absolute() {
            return Err(invalid("spool directory must be host-private and absolute"));
        }
        if len == 0 || len > max_bytes {
            return Err(invalid("spool object length exceeds limit"));
        }
        let ticket = budget.reserve(len)?;
        std::fs::create_dir_all(dir)?;
        let file = tempfile::tempfile_in(dir)?;
        file.set_len(len)?;
        Ok(Self {
            file,
            _ticket: ticket,
            len,
        })
    }
    pub fn len(&self) -> u64 {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    fn extent(&self, offset: u64, n: usize) -> io::Result<()> {
        match offset.checked_add(n as u64) {
            Some(end) if end <= self.len => Ok(()),
            _ => Err(invalid("sparse spool access outside the object")),
        }
    }
    pub fn write_at(&self, offset: u64, bytes: &[u8]) -> io::Result<()> {
        self.extent(offset, bytes.len())?;
        shared::vfs::file_io::write_all_at(&self.file, bytes, offset)
    }
    pub fn read_at(&self, offset: u64, output: &mut [u8]) -> io::Result<()> {
        self.extent(offset, output.len())?;
        shared::vfs::file_io::read_exact_at(&self.file, output, offset)
    }
}

pub struct SpoolWriter {
    // Drop/close the file before returning its disk credit.
    file: File,
    ticket: DiskTicket,
    max_bytes: u64,
    expected: Option<u64>,
    written: u64,
    failed: bool,
}
impl SpoolWriter {
    pub fn create(
        dir: &Path,
        budget: &SpoolBudget,
        max_bytes: u64,
        expected: Option<u64>,
    ) -> io::Result<Self> {
        if !dir.is_absolute() {
            return Err(invalid("spool directory must be host-private and absolute"));
        }
        if max_bytes == 0 || expected.is_some_and(|n| n > max_bytes) {
            return Err(invalid("spool object length exceeds limit"));
        }
        let ticket = budget.reserve(expected.unwrap_or(max_bytes))?;
        std::fs::create_dir_all(dir)?;
        let file = tempfile::tempfile_in(dir)?;
        Ok(Self {
            file,
            ticket,
            max_bytes,
            expected,
            written: 0,
            failed: false,
        })
    }
    pub fn append(&mut self, bytes: &[u8]) -> io::Result<()> {
        if self.failed {
            return Err(invalid("spool writer previously failed"));
        }
        self.failed = true;
        let next = self
            .written
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| invalid("spool length overflow"))?;
        if bytes.len() > FILE_CHUNK_BYTES
            || next > self.max_bytes
            || self.expected.is_some_and(|n| next > n)
        {
            return Err(invalid("spool write exceeds bound"));
        }
        self.file.write_all(bytes)?;
        self.written = next;
        self.failed = false;
        Ok(())
    }
    pub fn seal(self, max_control_bytes: usize) -> io::Result<SealedSpool> {
        self.seal_with(max_control_bytes, ReadAtFile::new)
    }
    /// Admit both possible media slots before allocating the sealed reader.
    /// Call on the original Fs executor, just like seal and append.
    pub fn seal_with_budget(
        self,
        max_control_bytes: usize,
        pools: &crate::pools::IoPools,
    ) -> io::Result<SealedSpool> {
        self.seal_with(max_control_bytes, |file, limit| {
            ReadAtFile::with_budget(file, limit, pools)
        })
    }
    pub(crate) fn seal_with_buffer(
        self,
        max_control_bytes: usize,
        buffer: Vec<u8>,
        lease: crate::pools::ByteTicket,
    ) -> io::Result<SealedSpool> {
        self.seal_with(max_control_bytes, |file, limit| {
            ReadAtFile::from_buffer(file, limit, buffer, lease)
        })
    }
    fn seal_with(
        mut self,
        max_control_bytes: usize,
        reader: impl FnOnce(File, usize) -> io::Result<ReadAtFile>,
    ) -> io::Result<SealedSpool> {
        if self.failed || self.expected.is_some_and(|n| n != self.written) {
            return Err(invalid("spool is incomplete or failed"));
        }
        let reader = reader(self.file, max_control_bytes)?;
        if reader.len() != self.written {
            return Err(invalid("spool file length changed"));
        }
        self.ticket.shrink_to(self.written);
        Ok(SealedSpool {
            reader,
            _ticket: self.ticket,
        })
    }
}
/// The only output of a successful seal: read-only operations on the owned
/// anonymous object, never a raw writable File or reopenable temporary path.
pub struct SealedSpool {
    reader: ReadAtFile,
    _ticket: DiskTicket,
}
impl SealedSpool {
    pub fn reader(&self) -> &ReadAtFile {
        &self.reader
    }
}
#[cfg(test)]
mod tests;

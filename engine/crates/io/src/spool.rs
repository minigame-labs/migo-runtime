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
const PROCESS_SPOOL_FILES: usize = 8;
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
    pub fn seal(mut self, max_control_bytes: usize) -> io::Result<SealedSpool> {
        if self.failed || self.expected.is_some_and(|n| n != self.written) {
            return Err(invalid("spool is incomplete or failed"));
        }
        let reader = ReadAtFile::new(self.file, max_control_bytes)?;
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

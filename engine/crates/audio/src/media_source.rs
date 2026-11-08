//! Blocking byte sources for the upstream format readers.
//!
//! Symphonia's readers pull bytes through `MediaSource` (`Read + Seek`) and treat
//! an IO error inside a packet as the end of the stream, so a source must block
//! until its bytes exist rather than report "not yet". Each source therefore
//! runs only on a stream's own decode thread, never on an async executor.
use shared::audio_file::{AudioFile, AudioFileCancel};
use std::io::{self, Read, Seek, SeekFrom};
use std::sync::Arc;
use symphonia_core::io::MediaSource;

/// Reads larger than this are split; the reader's own buffer is 32-64 KiB.
const MAX_READ: usize = 1 << 20;

pub(crate) fn cancelled() -> io::Error {
    io::Error::new(io::ErrorKind::Interrupted, "audio source cancelled")
}

/// A sealed, immutable audio object: a local file, a package entry or a fully
/// spooled HTTP body.
pub(crate) struct AudioFileSource {
    file: Arc<dyn AudioFile>,
    cancel: Arc<dyn AudioFileCancel>,
    len: u64,
    pos: u64,
}

impl AudioFileSource {
    pub(crate) fn new(file: Arc<dyn AudioFile>, cancel: Arc<dyn AudioFileCancel>) -> Self {
        let len = file.length();
        Self {
            file,
            cancel,
            len,
            pos: 0,
        }
    }
}

impl Read for AudioFileSource {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.cancel.is_cancelled() {
            return Err(cancelled());
        }
        let remaining = self.len.saturating_sub(self.pos);
        let n = buf.len().min(MAX_READ).min(remaining as usize);
        if n == 0 {
            return Ok(0);
        }
        self.file
            .read_blocking(self.pos, &mut buf[..n])
            .map_err(|error| io::Error::other(error.to_string()))?;
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for AudioFileSource {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        self.pos = resolve_seek(self.pos, self.len, to)?;
        Ok(self.pos)
    }
}

impl MediaSource for AudioFileSource {
    fn is_seekable(&self) -> bool {
        true
    }
    fn byte_len(&self) -> Option<u64> {
        Some(self.len)
    }
}

/// `SeekFrom` against a known length. Seeking past the end is allowed, as for a
/// file; reads there return end of stream.
pub(crate) fn resolve_seek(pos: u64, len: u64, to: SeekFrom) -> io::Result<u64> {
    let target = match to {
        SeekFrom::Start(offset) => Some(offset),
        SeekFrom::Current(delta) => pos.checked_add_signed(delta),
        SeekFrom::End(delta) => len.checked_add_signed(delta),
    };
    target.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "seek before start"))
}

#[cfg(test)]
#[path = "media_source_tests.rs"]
pub(crate) mod tests;

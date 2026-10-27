//! Positioned IO on already-open immutable regular files.
//!
//! Handles pin path identity; metadata guards only detect observed mutation.
//! The owner must keep the inode immutable. This is not a byte snapshot.
use std::{
    fs::{File, Metadata},
    io,
    time::SystemTime,
};

#[derive(PartialEq, Eq)]
pub struct FileStamp {
    pub len: u64,
    modified: SystemTime,
    #[cfg(unix)]
    unix: (u64, u64),
}
impl FileStamp {
    pub fn of(meta: Metadata) -> io::Result<Self> {
        if !meta.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "source is not a regular file",
            ));
        }
        Ok(Self {
            len: meta.len(),
            modified: meta.modified()?,
            #[cfg(unix)]
            unix: {
                use std::os::unix::fs::MetadataExt;
                (meta.dev(), meta.ino())
            },
        })
    }
    pub fn verify(&self, file: &File) -> io::Result<()> {
        if Self::of(file.metadata()?)? != *self {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "file representation changed",
            ));
        }
        Ok(())
    }
}

/// Caller validates its logical extent and checks its FileStamp before/after IO.
/// Unix leaves the cursor unchanged; Windows seek_read uses the explicit offset
/// and may move the cursor. Never combine this with cursor-based reads.
pub fn read_exact_at(file: &File, buffer: &mut [u8], offset: u64) -> io::Result<()> {
    offset
        .checked_add(buffer.len() as u64)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "file offset overflow"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileExt;
        file.read_exact_at(buffer, offset)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileExt;
        let mut at = 0;
        while at < buffer.len() {
            match file.seek_read(&mut buffer[at..], offset + at as u64) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "short pinned file read",
                    ));
                }
                Ok(n) => at += n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
    #[cfg(not(any(unix, windows)))]
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "positioned file IO unavailable",
    ))
}

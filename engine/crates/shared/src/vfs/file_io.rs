//! Positioned IO on already-open immutable regular files.
//!
//! Handles pin path identity; metadata guards only detect observed mutation.
//! The owner must keep the inode immutable. This is not a byte snapshot.
use std::{
    fs::{File, Metadata},
    io,
    time::SystemTime,
};

/// Native link count of the same retained object, including all Windows hard
/// links. Metadata obtained by reopening a path is not alias evidence.
pub fn link_count(file: &File) -> io::Result<u64> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(file.metadata()?.nlink())
    }
    #[cfg(windows)]
    {
        super::windows::link_count(file)
            .map(u64::from)
            .map_err(io::Error::other)
    }
    #[cfg(not(any(unix, windows)))]
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "native link count unavailable",
    ))
}

/// Identity of an opened native object, independent of its current pathname.
/// Windows keeps the complete 128-bit ID and volume, not the legacy file index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FileIdentity {
    #[cfg(unix)]
    unix: (u64, u64),
    #[cfg(windows)]
    windows: (u64, [u8; 16]),
}
impl FileIdentity {
    pub fn of(file: &File) -> io::Result<Self> {
        #[cfg(unix)]
        {
            Ok(Self::from_metadata(&file.metadata()?))
        }
        #[cfg(windows)]
        {
            super::windows::file_identity(file)
                .map(|windows| Self { windows })
                .map_err(io::Error::other)
        }
        #[cfg(not(any(unix, windows)))]
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "native file identity unavailable",
        ))
    }
    #[cfg(unix)]
    fn from_metadata(meta: &Metadata) -> Self {
        use std::os::unix::fs::MetadataExt;
        Self {
            unix: (meta.dev(), meta.ino()),
        }
    }
    /// Classify and identify the same live regular-file handle.
    /// Unix needs one fstat; Windows reuses the VFS's native FileIdInfo query.
    pub fn of_regular(file: &File) -> io::Result<Option<Self>> {
        let meta = file.metadata()?;
        if !meta.is_file() {
            return Ok(None);
        }
        #[cfg(unix)]
        let identity = Self::from_metadata(&meta);
        #[cfg(not(unix))]
        let identity = Self::of(file)?;
        Ok(Some(identity))
    }
}

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

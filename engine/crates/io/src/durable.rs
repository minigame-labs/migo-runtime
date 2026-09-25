//! Making written bytes durable, with the primitive each platform recommends
//! for it.
//!
//! Everywhere but Apple this is `fsync` (`File::sync_all`), which on Linux and
//! Android also flushes the device's write cache.
//!
//! On Apple platforms the standard library's `sync_all` is `F_FULLFSYNC`: it
//! asks the drive to write out its *entire* cache, every process's dirty data
//! included, before returning. Measured on the iOS simulator (2026-09-19): 19
//! ms on an idle disk, and 17 s for one game save -- two of them, file and
//! directory -- while the system was flushing a build. A `writeFileSync` blocks
//! the game's JavaScript for that long. Apple's guidance for the pattern the
//! engine's durable writes use -- write a temporary file, make it durable,
//! rename it into place -- is `F_BARRIERFSYNC`: an I/O barrier that orders this
//! file's writes before anything issued after it, so the rename can never reach
//! the disk ahead of the data. After a crash or a power loss the file is the old
//! contents or the new, never a mix -- the guarantee a save needs -- at ~1 ms.
//! What it gives up is the drive-cache flush: a power loss in the moments after
//! a save returned can roll it back to the previous save. An app being killed
//! loses nothing either way; the kernel owns the page cache.
//!
//! A file system that does not support the barrier (`ENOTSUP`, `EINVAL`, as a
//! network mount may answer) gets `F_FULLFSYNC`: the stronger primitive, never
//! a weaker one.

use std::fs::File;
use std::io;
use std::path::Path;

/// Make `file`'s written data and metadata durable before anything issued
/// after this call.
pub fn sync(file: &File) -> io::Result<()> {
    #[cfg(target_vendor = "apple")]
    {
        barrier(file)
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        file.sync_all()
    }
}

/// [`sync`] for the file's data, where its metadata does not need to be: the
/// descriptor a game opened with a durable append mode (`'as'`).
pub fn sync_data(file: &File) -> io::Result<()> {
    #[cfg(target_vendor = "apple")]
    {
        barrier(file)
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        file.sync_data()
    }
}

/// Make a directory's entries -- a rename, a new name -- durable. A no-op on
/// Windows, whose directory handles take no flush; there a rename's
/// durability is `MoveFileEx`'s to give.
pub fn sync_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        sync(&File::open(dir)?)
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        Ok(())
    }
}

#[cfg(target_vendor = "apple")]
fn barrier(file: &File) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    // SAFETY: `fcntl` with F_BARRIERFSYNC takes no pointer argument and the
    // descriptor is borrowed from a live `File` for the call.
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_BARRIERFSYNC) } != -1 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        Some(libc::ENOTSUP) | Some(libc::EINVAL) | Some(libc::ENOTTY) => file.sync_all(),
        _ => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn a_written_file_and_its_directory_sync() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("save.bin");
        let mut file = File::create(&path).unwrap();
        file.write_all(b"level 3").unwrap();
        sync(&file).unwrap();
        sync_data(&file).unwrap();
        sync_dir(dir.path()).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"level 3");
    }
}

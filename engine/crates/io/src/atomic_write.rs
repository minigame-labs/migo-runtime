//! Crash-safe atomic file writes.
//!
//! The classic `std::fs::write(path, bytes)` is not atomic: if the
//! process crashes or the device loses power mid-write, readers can
//! observe a truncated or zero-length file.  This module provides the
//! standard `temp -> write -> sync_all -> rename -> dir sync_all`
//! sequence so a reader either sees the **old** contents or the
//! **new** contents, never a mix.
//!
//! # Guarantees
//! * After a crash or a power loss, `path` holds the old contents or the
//!   new, never a mix. On success it holds the new contents across a full
//!   power cycle everywhere but Apple platforms, where the sync is an I/O
//!   barrier rather than a drive-cache flush and a power loss in the moments
//!   after success can roll the write back to the old contents -- see
//!   [`crate::durable`] for why, and what it measured.
//! * On failure, `path` is unchanged; the temporary file is best-
//!   effort cleaned up.
//! * Concurrent callers writing to the same `path` do not interleave
//!   bytes — each call materialises either its full payload or
//!   nothing (last writer wins after rename).
//!
//! # Non-goals
//! * This is not a transactional multi-file write.  Use SQLite or
//!   similar for those.
//! * Windows directory `fsync` is not a no-op replacement; we skip
//!   the `dir_sync` call there.  Callers that need cross-platform
//!   crash safety must accept that Windows can still lose the
//!   rename on power loss.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Monotonic counter so two concurrent `atomic_write` calls targeting
/// the same path don't clobber each other's temp file.
static TMP_ID: AtomicU64 = AtomicU64::new(0);

/// Compute the temporary filename used for an atomic write of `path`.
///
/// Kept `pub(crate)` so tests can verify cleanup behaviour; callers
/// should not rely on the exact naming scheme.
pub(crate) fn tmp_path_for(path: &Path) -> PathBuf {
    let id = TMP_ID.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let file_name = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "atomic".to_string());
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    parent.join(format!(".{}.{}.{}.atomic.tmp", file_name, pid, id))
}

/// Fsync the directory containing `path` so the rename is durable on
/// Linux/macOS.  Ignored on Windows (directory handles don't accept
/// `FlushFileBuffers`; `MoveFileEx` with `MOVEFILE_WRITE_THROUGH`
/// is the Windows-native answer and is not wired up here).
#[inline]
fn sync_parent_dir(path: &Path) -> io::Result<()> {
    #[cfg(not(windows))]
    {
        if let Some(parent) = path.parent() {
            // `""` is fine to skip — we are writing to CWD.
            if parent.as_os_str().is_empty() {
                return Ok(());
            }
            return crate::durable::sync_dir(parent);
        }
    }
    #[cfg(windows)]
    {
        let _ = path;
    }
    Ok(())
}

/// Core write-and-fsync primitive.  Wraps the `File::create + write_all
/// + sync_all` sequence and guarantees the temp file is removed on
/// error (best-effort; a crash between `create` and this function's
/// return does leave a `.atomic.tmp` behind, which is exactly what
/// [`cleanup_stale_tmp`] is for).
fn write_and_fsync(tmp: &Path, bytes: &[u8]) -> io::Result<()> {
    // `create` truncates any stale tmp file with the same name.
    // `write` + `create` is the mode reqired; readers never see this
    // partial file because we rename into place after `sync_all`.
    let result = (|| -> io::Result<()> {
        let mut f = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(tmp)?;
        f.write_all(bytes)?;
        crate::durable::sync(&f)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(tmp);
    }
    result
}

/// Atomically write `bytes` to `path`.
///
/// On Unix, the parent directory is also `fsync`'d so the rename
/// survives power loss.  On Windows the directory fsync is skipped.
///
/// Errors:
/// * `NotFound` — the parent directory does not exist.  The caller is
///   responsible for creating it (e.g. via `create_dir_all`); this
///   matches `std::fs::write` semantics.
/// * Any other `io::Error` is propagated verbatim from the underlying
///   `open`/`write`/`fsync`/`rename` syscall.
pub fn atomic_write(path: impl AsRef<Path>, bytes: &[u8]) -> io::Result<()> {
    let path = path.as_ref();
    let tmp = tmp_path_for(path);
    write_and_fsync(&tmp, bytes)?;

    // Rename is atomic on POSIX within a filesystem.  If it fails
    // (e.g. cross-device, or destination dir vanished), wipe the
    // tmp file so we don't accumulate garbage.
    if let Err(e) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    sync_parent_dir(path)?;
    Ok(())
}

/// Like [`atomic_write`], but the caller drives the write via a
/// closure so the payload can be built directly into the file
/// without an intermediate `Vec`.
///
/// The closure receives a freshly `create`d, truncated file
/// positioned at offset 0.  After it returns `Ok(())`, this
/// function will `sync_all` and rename.
pub fn atomic_write_with<F>(path: impl AsRef<Path>, writer: F) -> io::Result<()>
where
    F: FnOnce(&mut File) -> io::Result<()>,
{
    let path = path.as_ref();
    let tmp = tmp_path_for(path);

    let result = (|| -> io::Result<()> {
        let mut f = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp)?;
        writer(&mut f)?;
        crate::durable::sync(&f)?;
        Ok(())
    })();
    if let Err(e) = result {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }

    if let Err(e) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    sync_parent_dir(path)?;
    Ok(())
}

/// Best-effort cleanup of `.atomic.tmp` leftovers in `dir`.
///
/// Call at startup for directories that host atomic writes; stale
/// temp files from a previous crash are removed so they don't
/// eventually fill the filesystem.  Non-fatal: any error reading the
/// directory or unlinking an entry is swallowed.
pub fn cleanup_stale_tmp(dir: &Path) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for entry in rd.flatten() {
        if let Some(name) = entry.file_name().to_str()
            && name.ends_with(".atomic.tmp")
        {
            let _ = fs::remove_file(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::Barrier;
    use std::thread;
    use tempfile::tempdir;

    #[test]
    fn writes_and_reads_back_exact_bytes() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("hello.bin");
        atomic_write(&path, b"hello world").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"hello world");
    }

    #[test]
    fn overwrites_existing_file_atomically() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("kv.bin");
        atomic_write(&path, b"v1").unwrap();
        atomic_write(&path, b"v2-longer").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"v2-longer");
    }

    #[test]
    fn writer_closure_produces_correct_content() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("streamed.bin");
        atomic_write_with(&path, |f| {
            f.write_all(b"chunk1-")?;
            f.write_all(b"chunk2")?;
            Ok(())
        })
        .unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"chunk1-chunk2");
    }

    #[test]
    fn empty_payload_is_supported() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("empty.bin");
        atomic_write(&path, b"").unwrap();
        assert!(path.exists());
        assert_eq!(fs::metadata(&path).unwrap().len(), 0);
    }

    #[test]
    fn does_not_leave_tmp_files_on_success() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("clean.bin");
        atomic_write(&path, b"payload").unwrap();

        // The only file in the directory should be the final one;
        // no `.atomic.tmp` sibling.
        let entries: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(entries, vec!["clean.bin".to_string()]);
    }

    #[test]
    fn missing_parent_dir_yields_not_found() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nope/child.bin");
        let err = atomic_write(&path, b"x").expect_err("should fail");
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        // No tmp file leaks in the parent tempdir either.
        let entries: Vec<_> = fs::read_dir(dir.path()).unwrap().collect();
        assert!(entries.is_empty(), "stale entries: {:?}", entries);
    }

    #[test]
    fn writer_closure_error_is_propagated_and_tmp_cleaned() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("err.bin");

        let res = atomic_write_with(&path, |_f| {
            Err(io::Error::new(io::ErrorKind::Other, "boom"))
        });
        assert!(res.is_err());
        assert!(!path.exists(), "final file must not exist");
        let stale: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".atomic.tmp"))
            .collect();
        assert!(stale.is_empty(), "tmp files leaked: {:?}", stale);
    }

    #[test]
    fn concurrent_writers_yield_one_of_the_payloads() {
        // Not testing "last writer wins" (that depends on scheduling
        // order); we assert that (a) the final file exists, (b) its
        // contents exactly match one of the inputs — no byte-level
        // interleave — and (c) no tmp files are left behind.
        let dir = tempdir().unwrap();
        let path = Arc::new(dir.path().join("concurrent.bin"));
        let barrier = Arc::new(Barrier::new(8));

        let mut handles = Vec::new();
        for i in 0..8u8 {
            let p = path.clone();
            let b = barrier.clone();
            handles.push(thread::spawn(move || {
                b.wait();
                // 64 KiB of a single byte so byte-interleave would be
                // obvious (and so sync_all isn't a no-op).
                let buf = vec![b'a' + i; 64 * 1024];
                atomic_write(&*p, &buf).unwrap();
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        let content = fs::read(&*path).unwrap();
        assert_eq!(content.len(), 64 * 1024);
        let first = content[0];
        assert!(content.iter().all(|&b| b == first), "interleaved write");

        // No tmp residue.
        let stale: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".atomic.tmp"))
            .collect();
        assert!(stale.is_empty(), "tmp files leaked: {:?}", stale);
    }

    #[test]
    fn cleanup_stale_tmp_removes_only_atomic_tmp_suffix() {
        let dir = tempdir().unwrap();
        let keep = dir.path().join("data.bin");
        let stale1 = dir.path().join(".data.bin.1.1.atomic.tmp");
        let stale2 = dir.path().join(".foo.99.99.atomic.tmp");
        let not_ours = dir.path().join("scratch.tmp"); // different suffix

        fs::write(&keep, b"keep").unwrap();
        fs::write(&stale1, b"stale").unwrap();
        fs::write(&stale2, b"stale").unwrap();
        fs::write(&not_ours, b"other").unwrap();

        cleanup_stale_tmp(dir.path());

        assert!(keep.exists());
        assert!(not_ours.exists());
        assert!(!stale1.exists());
        assert!(!stale2.exists());
    }
}

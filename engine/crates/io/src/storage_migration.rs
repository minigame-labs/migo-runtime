//! One-time namespace handover of SQLite and all its journals before content
//! execution. No database copies, format conversions or per-key import occur.
//! Older SDK processes must be stopped for an SDK storage-layout upgrade; the
//! existing KvStore cached totals do not support multi-process writers.
use shared::{
    error::{EngineError, ErrorCode},
    vfs::{file_io::link_count, game_paths::StoragePaths, read_root::ReadRoot},
};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read},
    path::Path,
};

const READY: &str = "storage-v1.ready";
const MAGIC: &[u8] = b"migo-private-kv-v1\n";
const LOCK: &str = "storage-init.lock";

pub fn prepare(paths: &StoragePaths) -> Result<(), EngineError> {
    paths.prepare_with(|| {
        prepare_inner(paths).map_err(|error| {
            EngineError::new(ErrorCode::IoError)
                .with_msg("prepare private storage")
                .with_detail(error.to_string())
        })
    })
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
fn pin(path: &Path) -> io::Result<ReadRoot> {
    ReadRoot::open(path).map_err(io::Error::other)
}

fn regular(root: &ReadRoot, name: &Path) -> io::Result<File> {
    let file = root.open_regular(name).map_err(io::Error::other)?;
    if link_count(&file)? > 1 {
        return Err(invalid("storage file has another hard link"));
    }
    Ok(file)
}

fn validate_directory(path: &Path, all_files: bool) -> io::Result<ReadRoot> {
    let root = pin(path)?;
    if all_files {
        // This backend has always used WAL. Retain unfamiliar rollback
        // recovery states for explicit recovery instead of opening them as
        // part of a namespace upgrade.
        if fs::symlink_metadata(path.join("storage.db-journal")).is_ok() {
            return Err(invalid(
                "legacy rollback journal requires recovery before upgrade",
            ));
        }
        // Before game execution this is a stable legacy namespace. Reject
        // links/nonfiles instead of importing aliases into private storage.
        for entry in fs::read_dir(path)? {
            regular(&root, Path::new(&entry?.file_name()))?;
        }
    } else {
        // Ready stores may have live SQLite WAL rotation. Missing optional
        // journals are harmless; an observed unsafe object always fails.
        for name in [
            "storage.db",
            "storage.db-wal",
            "storage.db-shm",
            "storage.db-journal",
        ] {
            match root.open_regular(Path::new(name)) {
                Ok(file) if link_count(&file)? > 1 => {
                    return Err(invalid("storage file has another hard link"));
                }
                Ok(_) | Err(shared::vfs::VfsOpenError::NotFound) => {}
                Err(error) => return Err(io::Error::other(error)),
            }
        }
    }
    let db = fs::symlink_metadata(path.join("storage.db"));
    if matches!(db, Err(ref e) if e.kind()==io::ErrorKind::NotFound)
        && ["storage.db-wal", "storage.db-shm", "storage.db-journal"]
            .iter()
            .any(|name| fs::symlink_metadata(path.join(name)).is_ok())
    {
        return Err(invalid("storage journals exist without their database"));
    }
    Ok(root)
}

fn directory_exists(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(true),
        Ok(_) => Err(invalid("storage directory is not a plain directory")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn prepare_inner(paths: &StoragePaths) -> io::Result<()> {
    prepare_inner_with(paths, || {})
}

fn prepare_inner_with(paths: &StoragePaths, after_lock: impl FnOnce()) -> io::Result<()> {
    let target = paths.directory();
    let parent = target
        .parent()
        .ok_or_else(|| invalid("private storage has no parent"))?;
    fs::create_dir_all(parent)?;
    let parent_root = pin(parent)?;
    let lock = match OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(parent.join(LOCK))
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            regular(&parent_root, Path::new(LOCK))?
        }
        Err(error) => return Err(error),
    };
    // Kernel lock serializes independent GamePaths instances and namespace
    // preparation across processes. It does not add multi-process KV support.
    lock.lock()?;
    after_lock();
    let ready = match parent_root.open_regular(Path::new(READY)) {
        Ok(mut file) => {
            if link_count(&file)? > 1 || file.metadata()?.len() != MAGIC.len() as u64 {
                return Err(invalid("invalid private storage readiness record"));
            }
            let mut bytes = [0; MAGIC.len()];
            file.read_exact(&mut bytes)?;
            if bytes != MAGIC {
                return Err(invalid("invalid private storage readiness record"));
            }
            true
        }
        Err(shared::vfs::VfsOpenError::NotFound) => false,
        Err(error) => return Err(io::Error::other(error)),
    };
    if ready {
        validate_directory(target, false)?;
        return Ok(());
    }
    let user_root = paths
        .legacy()
        .parent()
        .ok_or_else(|| invalid("legacy storage has no parent"))?;
    let has_user = directory_exists(user_root)?;
    if has_user {
        let _ = pin(user_root)?;
    }
    let has_legacy = has_user && directory_exists(paths.legacy())?;
    if directory_exists(target)? {
        if has_legacy {
            return Err(invalid(
                "ambiguous legacy and private storage; neither was overwritten",
            ));
        }
        // A previous handover may have completed before marker publication.
        validate_directory(target, true)?;
    } else if has_legacy {
        let source = validate_directory(paths.legacy(), true)?;
        fs::rename(paths.legacy(), target)?;
        let moved = validate_directory(target, true)?;
        if moved.identity()? != source.identity()? {
            return Err(invalid("storage directory changed during handover"));
        }
    } else {
        fs::create_dir(target)?;
    }
    // Order namespace handover before publishing readiness. Errors after rename
    // keep data at the private target and remain retryable, never create an
    // empty replacement or switch back to a game-visible database.
    crate::durable::sync_dir(target)?;
    crate::durable::sync_dir(parent)?;
    if has_user {
        crate::durable::sync_dir(user_root)?;
    }
    crate::atomic_write::atomic_write(parent.join(READY), MAGIC)?;
    Ok(())
}

#[cfg(test)]
mod tests;

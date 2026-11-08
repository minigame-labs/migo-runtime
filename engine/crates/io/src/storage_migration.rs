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

/// The only names SQLite opens for this store: the database and its journals.
const FAMILY: [&str; 4] = [
    "storage.db",
    "storage.db-wal",
    "storage.db-shm",
    "storage.db-journal",
];

fn is_family(name: &std::ffi::OsStr) -> bool {
    FAMILY.iter().any(|member| name == *member)
}

/// Pin `path` and check every SQLite name present there. Only these names are
/// ever opened, so only they can alias private storage: each must be a regular
/// file with one link. Missing journals are normal; a journal without its
/// database is not a state this store produces.
fn validate_family(path: &Path) -> io::Result<(ReadRoot, bool)> {
    let root = pin(path)?;
    let mut present = [false; FAMILY.len()];
    for (index, name) in FAMILY.iter().enumerate() {
        match root.open_regular(Path::new(name)) {
            Ok(file) if link_count(&file)? > 1 => {
                return Err(invalid("storage file has another hard link"));
            }
            Ok(_) => present[index] = true,
            Err(shared::vfs::VfsOpenError::NotFound) => {}
            Err(error) => return Err(io::Error::other(error)),
        }
    }
    if !present[0] && (present[1] || present[3]) {
        return Err(invalid("storage journals exist without their database"));
    }
    Ok((root, present[0]))
}

/// The legacy directory sat inside the game's writable data root, so anything
/// besides the SQLite family belongs to the game. Hand it back there.
fn return_foreign_entries(target: &Path, legacy: &Path) -> io::Result<bool> {
    let mut moved = false;
    for entry in fs::read_dir(target)? {
        let name = entry?.file_name();
        if is_family(&name) {
            continue;
        }
        if !moved {
            match fs::create_dir(legacy) {
                Err(error) if error.kind() != io::ErrorKind::AlreadyExists => return Err(error),
                _ => {}
            }
            moved = true;
        }
        let destination = legacy.join(&name);
        if fs::symlink_metadata(&destination).is_ok() {
            return Err(invalid(
                "a game entry exists in both legacy and private storage",
            ));
        }
        fs::rename(target.join(&name), destination)?;
    }
    Ok(moved)
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

fn lock_initialization(file: &File) -> io::Result<()> {
    // Rust 1.95's File::lock omits Android. Bionic provides flock at our API
    // floor; use the same primitive on Linux so host tests exercise this path.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        use std::os::fd::AsRawFd;
        loop {
            // SAFETY: the borrowed file keeps this descriptor valid throughout
            // the syscall. Ownership stays with File; closing it releases lock.
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } == 0 {
                return Ok(());
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    file.lock()
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
    lock_initialization(&lock)?;
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
        validate_family(target)?;
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
    let legacy = paths.legacy();
    let legacy_store = if has_user && directory_exists(legacy)? {
        Some(validate_family(legacy)?)
    } else {
        None
    };
    let has_legacy_db = legacy_store.as_ref().is_some_and(|(_, db)| *db);
    let mut returned = false;
    if directory_exists(target)? {
        if has_legacy_db {
            return Err(invalid(
                "ambiguous legacy and private storage; neither was overwritten",
            ));
        }
        // A previous handover may have completed before marker publication.
        validate_family(target)?;
        returned = return_foreign_entries(target, legacy)?;
    } else if let Some((source, true)) = legacy_store {
        // One rename moves the database with every journal, so no crash can
        // separate committed WAL frames or a hot journal from their database.
        fs::rename(legacy, target)?;
        let (moved, _) = validate_family(target)?;
        if moved.identity()? != source.identity()? {
            return Err(invalid("storage directory changed during handover"));
        }
        returned = return_foreign_entries(target, legacy)?;
    } else {
        // No legacy database: a game-owned directory of that name stays put.
        fs::create_dir(target)?;
    }
    if returned {
        crate::durable::sync_dir(legacy)?;
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

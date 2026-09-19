//! The file-system service ops, as the external session answers them.
//!
//! Each op is the embedded runtime's op of the same name: the arguments arrive
//! already converted by deno_core's rule for its parameter (see
//! `service_args`), the work is the same `migo_services::fs` function the
//! embedded op calls, and the answer is encoded as the value serde_v8 would
//! have handed JavaScript -- see [`serde_u64`] for the one place that takes
//! care. Structured answers (a stat, a saved-file list, zip entries) travel as
//! arrays in the struct's field order; the producer rebuilds the object with
//! the same keys in the same order, so content sees what it sees in process.

use std::sync::Arc;

use frame_wire::value::OwnedValue;
use futures::future::BoxFuture;
use migo_services::ServiceError;
use migo_services::fs::{self, FsEnv};
use shared::protocol::io_cmd::{FileStat, SavedFileInfo, StatResult, ZipEntryData, ZipEntryResult};

use super::service_args::{
    boolean, exactly, not_a, optional_bytes, optional_string, optional_u64, string, u32_of, u64_of,
};
use super::service_ops::id;

/// The largest integer a JavaScript Number holds exactly: 2^53 - 1.
const MAX_SAFE_INTEGER: u64 = (1 << 53) - 1;

/// A `u64` inside a `#[serde]` answer, as serde_v8 serializes one: a Number
/// while it is a safe integer, a BigInt past that.
fn serde_u64(value: u64) -> OwnedValue {
    if value <= MAX_SAFE_INTEGER {
        OwnedValue::F64(value as f64)
    } else {
        OwnedValue::U64(value)
    }
}

/// `FileStat`, in its field order: mode, size, atime, mtime, is_file,
/// is_directory.
fn file_stat(stat: FileStat) -> OwnedValue {
    OwnedValue::Array(vec![
        OwnedValue::U32(stat.mode),
        serde_u64(stat.size),
        serde_u64(stat.atime),
        serde_u64(stat.mtime),
        OwnedValue::Bool(stat.is_file),
        OwnedValue::Bool(stat.is_directory),
    ])
}

/// `StatResult` is untagged: a single stat is an object, a recursive one an
/// array of `{path, stat}`. The wire says which with a leading 0 or 1.
fn stat_result(result: StatResult) -> OwnedValue {
    match result {
        StatResult::Single(stat) => OwnedValue::Array(vec![OwnedValue::U32(0), file_stat(stat)]),
        StatResult::Recursive(entries) => OwnedValue::Array(vec![
            OwnedValue::U32(1),
            OwnedValue::Array(
                entries
                    .into_iter()
                    .map(|entry| {
                        OwnedValue::Array(vec![OwnedValue::Str(entry.path), file_stat(entry.stat)])
                    })
                    .collect(),
            ),
        ]),
    }
}

fn names(list: Vec<String>) -> OwnedValue {
    OwnedValue::Array(list.into_iter().map(OwnedValue::Str).collect())
}

/// `(u64, String)`: a serde tuple is an array.
fn file_info((size, digest): (u64, String)) -> OwnedValue {
    OwnedValue::Array(vec![serde_u64(size), OwnedValue::Str(digest)])
}

/// `SavedFileInfo`, in its field order: filePath, size, createTime.
fn saved_files(list: Vec<SavedFileInfo>) -> OwnedValue {
    OwnedValue::Array(
        list.into_iter()
            .map(|info| {
                OwnedValue::Array(vec![
                    OwnedValue::Str(info.file_path),
                    serde_u64(info.size),
                    serde_u64(info.create_time),
                ])
            })
            .collect(),
    )
}

/// The embedded op's `ZipEntryPayload`, in its field order: path, text,
/// bytes, errMsg -- with null where the embedded op omits the field.
fn zip_entries(list: Vec<ZipEntryResult>) -> OwnedValue {
    OwnedValue::Array(
        list.into_iter()
            .map(|entry| {
                let (text, data) = match entry.data {
                    Some(ZipEntryData::Text(text)) => (OwnedValue::Str(text), OwnedValue::Null),
                    Some(ZipEntryData::Binary(data)) => (OwnedValue::Null, OwnedValue::Bytes(data)),
                    None => (OwnedValue::Null, OwnedValue::Null),
                };
                OwnedValue::Array(vec![
                    OwnedValue::Str(entry.path),
                    text,
                    data,
                    OwnedValue::Str(entry.err_msg),
                ])
            })
            .collect(),
    )
}

/// The filled prefix of a staged read, without copying it.
fn filled(read: migo_io::fs_ops::OwnedFileRead) -> OwnedValue {
    let (storage, filled) = read.into_parts();
    let mut bytes = storage.into_vec();
    bytes.truncate(filled);
    OwnedValue::Bytes(bytes)
}

/// A read length as the host can allocate it.
fn length(op: u32, index: usize, value: OwnedValue) -> Result<usize, ServiceError> {
    usize::try_from(u64_of(op, index, value)?).map_err(|_| {
        ServiceError::classed(
            fs::CLASS_IO_ERROR,
            "read length exceeds this platform's address space",
        )
    })
}

/// A write's data: the caller's bytes, moved rather than copied, or its
/// string in its encoding.
fn write_data(
    op: u32,
    first: usize,
    data: OwnedValue,
    text: OwnedValue,
    encoding: OwnedValue,
) -> Result<impl FnOnce() -> Result<Vec<u8>, ServiceError>, ServiceError> {
    let data = optional_bytes(op, first, data)?;
    let text = optional_string(op, first + 1, text)?;
    let encoding = optional_string(op, first + 2, encoding)?;
    Ok(move || fs::owned_payload(data, text, encoding))
}

/// Whether `op` is a synchronous file-system op this module answers.
pub(crate) fn is_sync(op: u32) -> bool {
    matches!(
        op,
        id::op_access_sync
            | id::op_write_or_append_file_sync
            | id::op_open_file_sync
            | id::op_close_file_sync
            | id::op_copy_file_sync
            | id::op_fstat_sync
            | id::op_ftruncate_sync
            | id::op_mkdir_sync
            | id::op_readdir_sync
            | id::op_unlink_sync
            | id::op_rename_sync
            | id::op_rmdir_sync
            | id::op_stat_sync
            | id::op_write_file_sync
            | id::op_read_file_sync
            | id::op_read_fd_sync
            | id::op_read_fd_into_sync
            | id::op_read_compressed_file_sync
            | id::op_get_file_info_sync
    )
}

/// Whether `op` is an awaited file-system op this module answers.
pub(crate) fn is_async(op: u32) -> bool {
    matches!(
        op,
        id::op_access
            | id::op_write_or_append_file
            | id::op_open_file
            | id::op_close_file
            | id::op_copy_file
            | id::op_fstat
            | id::op_ftruncate
            | id::op_mkdir
            | id::op_readdir
            | id::op_unlink
            | id::op_rename
            | id::op_rmdir
            | id::op_stat
            | id::op_write_file
            | id::op_read_file
            | id::op_read_fd
            | id::op_read_fd_into
            | id::op_read_compressed_file
            | id::op_read_zip_entry
            | id::op_unzip
            | id::op_get_file_info
            | id::op_list_saved_files
    )
}

/// Run a synchronous file-system op. Blocks: called off the session thread.
pub(crate) fn call_sync(
    env: &FsEnv,
    op: u32,
    args: Vec<OwnedValue>,
) -> Result<OwnedValue, ServiceError> {
    match op {
        id::op_access_sync => {
            let [path] = exactly(op, args)?;
            fs::access_sync(env, &string(op, 0, path)?).map(OwnedValue::Bool)
        }
        id::op_write_or_append_file_sync => {
            let [path, data, text, encoding, append, durable] = exactly(op, args)?;
            let path = string(op, 0, path)?;
            let prepare = write_data(op, 1, data, text, encoding)?;
            let (append, durable) = (boolean(op, 4, append)?, boolean(op, 5, durable)?);
            fs::write_or_append_sync(env, &path, prepare, append, durable).map(OwnedValue::Bool)
        }
        id::op_open_file_sync => {
            let [path, flag] = exactly(op, args)?;
            fs::open_sync(env, &string(op, 0, path)?, &string(op, 1, flag)?).map(OwnedValue::U32)
        }
        id::op_close_file_sync => {
            let [rid] = exactly(op, args)?;
            fs::close(env, u32_of(op, 0, rid)?).map(|()| OwnedValue::Null)
        }
        id::op_copy_file_sync => {
            let [src, dest] = exactly(op, args)?;
            fs::copy_sync(env, &string(op, 0, src)?, &string(op, 1, dest)?)
                .map(|()| OwnedValue::Null)
        }
        id::op_fstat_sync => {
            let [rid] = exactly(op, args)?;
            fs::fstat(env, u32_of(op, 0, rid)?).map(file_stat)
        }
        id::op_ftruncate_sync => {
            let [rid, len] = exactly(op, args)?;
            fs::ftruncate(env, u32_of(op, 0, rid)?, u64_of(op, 1, len)?).map(|()| OwnedValue::Null)
        }
        id::op_mkdir_sync => {
            let [path, recursive] = exactly(op, args)?;
            fs::mkdir_sync(env, &string(op, 0, path)?, boolean(op, 1, recursive)?)
                .map(|()| OwnedValue::Null)
        }
        id::op_readdir_sync => {
            let [path] = exactly(op, args)?;
            fs::readdir_sync(env, &string(op, 0, path)?).map(names)
        }
        id::op_unlink_sync => {
            let [path] = exactly(op, args)?;
            fs::unlink_sync(env, &string(op, 0, path)?).map(|()| OwnedValue::Null)
        }
        id::op_rename_sync => {
            let [old, new] = exactly(op, args)?;
            fs::rename_sync(env, &string(op, 0, old)?, &string(op, 1, new)?)
                .map(|()| OwnedValue::Null)
        }
        id::op_rmdir_sync => {
            let [path, recursive] = exactly(op, args)?;
            fs::rmdir_sync(env, &string(op, 0, path)?, boolean(op, 1, recursive)?)
                .map(|()| OwnedValue::Null)
        }
        id::op_stat_sync => {
            let [path, recursive] = exactly(op, args)?;
            fs::stat_sync(env, &string(op, 0, path)?, boolean(op, 1, recursive)?).map(stat_result)
        }
        id::op_write_file_sync => {
            let [rid, data, text, encoding, position] = exactly(op, args)?;
            let rid = u32_of(op, 0, rid)?;
            let prepare = write_data(op, 1, data, text, encoding)?;
            let position = optional_u64(op, 4, position)?;
            // `#[bigint] usize`: a BigInt, always.
            fs::write_fd_sync(env, rid, prepare, position).map(|n| OwnedValue::U64(n as u64))
        }
        id::op_read_file_sync => {
            let [path, position, len] = exactly(op, args)?;
            let path = string(op, 0, path)?;
            let (position, len) = (optional_u64(op, 1, position)?, optional_u64(op, 2, len)?);
            fs::read_file_sync(env, &path, position, len).map(OwnedValue::Bytes)
        }
        id::op_read_fd_sync => {
            let [rid, len, position] = exactly(op, args)?;
            let (rid, len) = (u32_of(op, 0, rid)?, u64_of(op, 1, len)?);
            fs::read_fd_sync(env, rid, len, optional_u64(op, 2, position)?).map(OwnedValue::Bytes)
        }
        id::op_read_fd_into_sync => {
            // The caller's buffer is in another process: the bytes come back
            // and the producer writes them into it.
            let [rid, len, position] = exactly(op, args)?;
            let (rid, len) = (u32_of(op, 0, rid)?, length(op, 1, len)?);
            fs::read_fd_for_buffer_sync(env, rid, len, optional_u64(op, 2, position)?).map(filled)
        }
        id::op_read_compressed_file_sync => {
            let [path] = exactly(op, args)?;
            fs::read_compressed_sync(env, &string(op, 0, path)?).map(OwnedValue::Bytes)
        }
        id::op_get_file_info_sync => {
            let [path, algorithm] = exactly(op, args)?;
            fs::get_file_info_sync(env, &string(op, 0, path)?, &string(op, 1, algorithm)?)
                .map(file_info)
        }
        other => Err(not_a(other, "synchronous file-system")),
    }
}

/// Start an awaited file-system op. The future owns what it needs.
pub(crate) fn call_async(
    env: FsEnv,
    op: u32,
    args: Vec<OwnedValue>,
) -> Result<BoxFuture<'static, Result<OwnedValue, ServiceError>>, ServiceError> {
    Ok(match op {
        id::op_access => {
            let [path] = exactly(op, args)?;
            let path = string(op, 0, path)?;
            Box::pin(async move { fs::access(env, path).await.map(OwnedValue::Bool) })
        }
        id::op_write_or_append_file => {
            let [path, data, text, encoding, append, durable] = exactly(op, args)?;
            let path = string(op, 0, path)?;
            let prepare = write_data(op, 1, data, text, encoding)?;
            let (append, durable) = (boolean(op, 4, append)?, boolean(op, 5, durable)?);
            // The embedded op resolves and prepares before it returns its
            // future; here that is before the future starts, which content
            // cannot tell apart -- both fail the same promise.
            Box::pin(async move {
                fs::write_or_append(&env, &path, prepare, append, durable)?
                    .await
                    .map(OwnedValue::Bool)
            })
        }
        id::op_open_file => {
            let [path, flag] = exactly(op, args)?;
            let (path, flag) = (string(op, 0, path)?, string(op, 1, flag)?);
            Box::pin(async move { fs::open(env, path, flag).await.map(OwnedValue::U32) })
        }
        id::op_close_file => {
            let [rid] = exactly(op, args)?;
            let rid = u32_of(op, 0, rid)?;
            Box::pin(async move { fs::close(&env, rid).map(|()| OwnedValue::Null) })
        }
        id::op_copy_file => {
            let [src, dest] = exactly(op, args)?;
            let (src, dest) = (string(op, 0, src)?, string(op, 1, dest)?);
            Box::pin(async move { fs::copy(env, src, dest).await.map(|()| OwnedValue::Null) })
        }
        id::op_fstat => {
            let [rid] = exactly(op, args)?;
            let rid = u32_of(op, 0, rid)?;
            Box::pin(async move { fs::fstat(&env, rid).map(file_stat) })
        }
        id::op_ftruncate => {
            let [rid, len] = exactly(op, args)?;
            let (rid, len) = (u32_of(op, 0, rid)?, u64_of(op, 1, len)?);
            Box::pin(async move { fs::ftruncate(&env, rid, len).map(|()| OwnedValue::Null) })
        }
        id::op_mkdir => {
            let [path, recursive] = exactly(op, args)?;
            let (path, recursive) = (string(op, 0, path)?, boolean(op, 1, recursive)?);
            Box::pin(async move {
                fs::mkdir(env, path, recursive)
                    .await
                    .map(|()| OwnedValue::Null)
            })
        }
        id::op_readdir => {
            let [path] = exactly(op, args)?;
            let path = string(op, 0, path)?;
            Box::pin(async move { fs::readdir(env, path).await.map(names) })
        }
        id::op_unlink => {
            let [path] = exactly(op, args)?;
            let path = string(op, 0, path)?;
            Box::pin(async move { fs::unlink(env, path).await.map(|()| OwnedValue::Null) })
        }
        id::op_rename => {
            let [old, new] = exactly(op, args)?;
            let (old, new) = (string(op, 0, old)?, string(op, 1, new)?);
            Box::pin(async move { fs::rename(env, old, new).await.map(|()| OwnedValue::Null) })
        }
        id::op_rmdir => {
            let [path, recursive] = exactly(op, args)?;
            let (path, recursive) = (string(op, 0, path)?, boolean(op, 1, recursive)?);
            Box::pin(async move {
                fs::rmdir(env, path, recursive)
                    .await
                    .map(|()| OwnedValue::Null)
            })
        }
        id::op_stat => {
            let [path, recursive] = exactly(op, args)?;
            let (path, recursive) = (string(op, 0, path)?, boolean(op, 1, recursive)?);
            Box::pin(async move { fs::stat(env, path, recursive).await.map(stat_result) })
        }
        id::op_write_file => {
            let [rid, data, text, encoding, position] = exactly(op, args)?;
            let rid = u32_of(op, 0, rid)?;
            let prepare = write_data(op, 1, data, text, encoding)?;
            let position = optional_u64(op, 4, position)?;
            Box::pin(async move {
                fs::write_fd(&env, rid, prepare, position)?
                    .await
                    .map(|n| OwnedValue::U64(n as u64))
            })
        }
        id::op_read_file => {
            let [path, position, len] = exactly(op, args)?;
            let path = string(op, 0, path)?;
            let (position, len) = (optional_u64(op, 1, position)?, optional_u64(op, 2, len)?);
            Box::pin(async move {
                fs::read_file(env, path, position, len)
                    .await
                    .map(OwnedValue::Bytes)
            })
        }
        id::op_read_fd => {
            let [rid, len, position] = exactly(op, args)?;
            let (rid, len) = (u32_of(op, 0, rid)?, u64_of(op, 1, len)?);
            let position = optional_u64(op, 2, position)?;
            Box::pin(async move {
                fs::read_fd(env, rid, len, position)
                    .await
                    .map(OwnedValue::Bytes)
            })
        }
        id::op_read_fd_into => {
            let [rid, len, position] = exactly(op, args)?;
            let (rid, len) = (u32_of(op, 0, rid)?, length(op, 1, len)?);
            let position = optional_u64(op, 2, position)?;
            Box::pin(async move {
                fs::read_fd_for_buffer(env, rid, len, position)
                    .await
                    .map(filled)
            })
        }
        id::op_read_compressed_file => {
            let [path] = exactly(op, args)?;
            let path = string(op, 0, path)?;
            Box::pin(async move { fs::read_compressed(env, path).await.map(OwnedValue::Bytes) })
        }
        id::op_read_zip_entry => {
            let [path, entries] = exactly(op, args)?;
            let (path, entries) = (string(op, 0, path)?, string(op, 1, entries)?);
            Box::pin(async move {
                fs::read_zip_entry(env, path, entries)
                    .await
                    .map(zip_entries)
            })
        }
        id::op_unzip => {
            let [zip, target] = exactly(op, args)?;
            let (zip, target) = (string(op, 0, zip)?, string(op, 1, target)?);
            // No platform unzip on this lane: the `zip` crate on the archive
            // pool, as every non-Android host uses.
            Box::pin(async move {
                fs::unzip(env, zip, target, None)
                    .await
                    .map(|()| OwnedValue::Null)
            })
        }
        id::op_get_file_info => {
            let [path, algorithm] = exactly(op, args)?;
            let (path, algorithm) = (string(op, 0, path)?, string(op, 1, algorithm)?);
            Box::pin(async move { fs::get_file_info(env, path, algorithm).await.map(file_info) })
        }
        id::op_list_saved_files => {
            let [dir, prefix] = exactly(op, args)?;
            let (dir, prefix) = (string(op, 0, dir)?, string(op, 1, prefix)?);
            Box::pin(async move {
                fs::list_saved_files(env, dir, prefix)
                    .await
                    .map(saved_files)
            })
        }
        other => return Err(not_a(other, "awaited file-system")),
    })
}

/// The scheduler and sandbox a call reads, for a caller that has them.
pub(crate) fn env(
    scheduler: Arc<migo_io::scheduler::IoScheduler>,
    content: Option<&migo_services::content::MountedContent>,
) -> FsEnv {
    FsEnv {
        scheduler,
        vfs: content.map(|content| Arc::clone(&content.vfs)),
        mount_table: content.map(|content| Arc::clone(&content.mount_table)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_u64_is_a_number_while_it_is_safe_and_a_bigint_past_it() {
        assert_eq!(serde_u64(0), OwnedValue::F64(0.0));
        assert_eq!(
            serde_u64(MAX_SAFE_INTEGER),
            OwnedValue::F64(MAX_SAFE_INTEGER as f64)
        );
        assert_eq!(
            serde_u64(MAX_SAFE_INTEGER + 1),
            OwnedValue::U64(MAX_SAFE_INTEGER + 1)
        );
    }

    #[test]
    fn every_numbered_file_op_is_answered_in_exactly_one_shape() {
        for (name, op) in super::super::service_ops::ALL {
            let file_op = name.starts_with("op_")
                && migo_services_fs_op(name)
                && *name != "op_require_resolve_and_read";
            if file_op {
                assert!(
                    is_sync(*op) ^ is_async(*op),
                    "{name} is answered as {} ",
                    if is_sync(*op) { "both" } else { "neither" }
                );
                assert_eq!(is_sync(*op), name.ends_with("_sync"), "{name}");
            }
        }
    }

    /// The file-system ops by name: the embedded `host_v8_file` extension's.
    fn migo_services_fs_op(name: &str) -> bool {
        const FILE_OPS: &[&str] = &[
            "op_access",
            "op_write_or_append_file",
            "op_open_file",
            "op_close_file",
            "op_copy_file",
            "op_fstat",
            "op_ftruncate",
            "op_mkdir",
            "op_readdir",
            "op_unlink",
            "op_rename",
            "op_rmdir",
            "op_stat",
            "op_write_file",
            "op_read_file",
            "op_read_fd",
            "op_read_fd_into",
            "op_read_compressed_file",
            "op_read_zip_entry",
            "op_unzip",
            "op_get_file_info",
            "op_list_saved_files",
        ];
        let base = name.strip_suffix("_sync").unwrap_or(name);
        FILE_OPS.contains(&base)
    }
}

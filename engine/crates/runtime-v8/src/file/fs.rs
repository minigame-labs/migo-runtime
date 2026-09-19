//! The file-system ops: `migo.getFileSystemManager()`'s calls.
//!
//! Each op converts its arguments out of V8 and calls the same-named function
//! of [`migo_services::fs`], where the work is -- path resolution through the
//! sandbox, the IO scheduler, the limits and the messages -- so the external
//! session's service dispatcher answers the same call with the same rules.
//! What stays here is what only V8 has: borrowing a caller's buffer while the
//! isolate is blocked, and handing bytes back as a `Uint8Array` without a copy.

use std::{cell::RefCell, rc::Rc, sync::Arc};

use deno_core::{JsBuffer, OpState, ToJsBuffer, op2};
use migo_io::{fs_ops, scheduler::IoScheduler};
use migo_services::ServiceError;
use migo_services::fs::{self, FsEnv, Payload};
use shared::{
    error::{EngineError, ErrorCode},
    op_state::HostOpState,
    protocol::io_cmd::{FileId, FileStat, SavedFileInfo, StatResult, ZipEntryData},
};

use crate::io_state::IoSchedulerState;

#[derive(Debug, thiserror::Error, deno_error::JsError)]
pub enum IOError {
    #[class("IOError")]
    #[error("{0}")]
    Message(String),
}

impl From<&str> for IOError {
    #[inline]
    fn from(value: &str) -> Self {
        IOError::Message(value.to_string())
    }
}

impl From<String> for IOError {
    #[inline]
    fn from(value: String) -> Self {
        IOError::Message(value)
    }
}

impl From<ErrorCode> for IOError {
    #[inline]
    fn from(e: ErrorCode) -> Self {
        IOError::Message(e.default_message().to_string())
    }
}

impl From<EngineError> for IOError {
    #[inline]
    fn from(e: EngineError) -> Self {
        IOError::Message(fs::engine_err(e).message)
    }
}

/// Every file-system failure is an `IOError`; the service already says which
/// message.
impl From<ServiceError> for IOError {
    #[inline]
    fn from(e: ServiceError) -> Self {
        IOError::Message(e.message)
    }
}

#[inline]
fn ioerr(msg: impl Into<String>) -> IOError {
    IOError::Message(msg.into())
}

#[inline]
fn get_scheduler(state: &OpState) -> Arc<IoScheduler> {
    state.borrow::<IoSchedulerState>().0.clone()
}

/// What the service reads: this runtime's scheduler and the game's sandbox.
#[inline]
fn fs_env(state: &OpState) -> FsEnv {
    let host = state.borrow::<HostOpState>();
    FsEnv {
        scheduler: get_scheduler(state),
        vfs: host.vfs.clone(),
        mount_table: host.mount_table.clone(),
    }
}

#[inline]
fn fs_env_async(state: &Rc<RefCell<OpState>>) -> FsEnv {
    fs_env(&state.borrow())
}

/// Validate backing-store metadata before creating any Rust byte slice.
/// `#[buffer]` uses deno_core's direct converter, which bypasses serde_v8's
/// shared/resizable rejection. Even JsBuffer::len() dereferences the bytes.
fn validate_file_buffer(buf: JsBuffer) -> Result<JsBuffer, ServiceError> {
    let slice = buf.into_parts();
    let (store, _) = slice.clone().into_parts();
    if store.is_shared() || store.is_resizable_by_user_javascript() {
        return Err(ServiceError::classed(
            fs::CLASS_IO_ERROR,
            "file IO requires a fixed, nonshared buffer",
        ));
    }
    Ok(JsBuffer::from_parts(slice))
}

/// Resolve the write payload from the buffer/string pair the op received.
/// Sync calls may borrow validated JS bytes while V8 is blocked; async calls
/// take owned bytes before returning their future.
fn prepare_data(
    data_buf: Option<JsBuffer>,
    data_str: Option<String>,
    encoding: Option<String>,
) -> Result<Payload<JsBuffer>, ServiceError> {
    let data_buf = data_buf.map(validate_file_buffer).transpose()?;
    fs::payload(data_buf, data_str, encoding)
}

//
// Access - check if path exists (uses VFS)
//
#[op2(async(lazy), fast)]
pub async fn op_access(
    state: Rc<RefCell<OpState>>,
    #[string] path: String,
) -> Result<bool, IOError> {
    Ok(fs::access(fs_env_async(&state), path).await?)
}

#[op2(fast)]
pub fn op_access_sync(state: &mut OpState, #[string] path: String) -> Result<bool, IOError> {
    Ok(fs::access_sync(&fs_env(state), &path)?)
}

//
// Write / append (path) - uses VFS with Write/Create permission
//
#[op2(async(lazy))]
pub fn op_write_or_append_file(
    state: Rc<RefCell<OpState>>,
    #[string] path: String,
    #[buffer] data_buf: Option<JsBuffer>,
    #[string] data_str: Option<String>,
    #[string] encoding: Option<String>,
    append: bool,
    durable: bool,
) -> Result<impl std::future::Future<Output = Result<bool, IOError>>, IOError> {
    let write = fs::write_or_append(
        &fs_env_async(&state),
        &path,
        || prepare_data(data_buf, data_str, encoding)?.into_owned(),
        append,
        durable,
    )?;
    Ok(async move { Ok(write.await?) })
}

#[op2]
pub fn op_write_or_append_file_sync(
    state: &mut OpState,
    #[string] path: String,
    #[buffer] data_buf: Option<JsBuffer>,
    #[string] data_str: Option<String>,
    #[string] encoding: Option<String>,
    append: bool,
    durable: bool,
) -> Result<bool, IOError> {
    Ok(fs::write_or_append_sync(
        &fs_env(state),
        &path,
        || prepare_data(data_buf, data_str, encoding),
        append,
        durable,
    )?)
}

//
// Open / close - uses VFS with appropriate permission based on open flag
//
#[op2(async(lazy), fast)]
pub async fn op_open_file(
    state: Rc<RefCell<OpState>>,
    #[string] path: String,
    #[string] flag: String,
) -> Result<u32, IOError> {
    Ok(fs::open(fs_env_async(&state), path, flag).await?)
}

#[op2(fast)]
pub fn op_open_file_sync(
    state: &mut OpState,
    #[string] path: String,
    #[string] flag: String,
) -> Result<u32, IOError> {
    Ok(fs::open_sync(&fs_env(state), &path, &flag)?)
}

#[op2(async(lazy), fast)]
pub async fn op_close_file(state: Rc<RefCell<OpState>>, #[smi] rid: FileId) -> Result<(), IOError> {
    Ok(fs::close(&fs_env_async(&state), rid)?)
}

#[op2(fast)]
pub fn op_close_file_sync(state: &mut OpState, #[smi] rid: FileId) -> Result<(), IOError> {
    Ok(fs::close(&fs_env(state), rid)?)
}

//
// Copy - uses VFS for both source (Read) and destination (Create)
//
#[op2(async(lazy), fast)]
pub async fn op_copy_file(
    state: Rc<RefCell<OpState>>,
    #[string] src_path: String,
    #[string] dest_path: String,
) -> Result<(), IOError> {
    Ok(fs::copy(fs_env_async(&state), src_path, dest_path).await?)
}

#[op2(fast)]
pub fn op_copy_file_sync(
    state: &mut OpState,
    #[string] src_path: String,
    #[string] dest_path: String,
) -> Result<(), IOError> {
    Ok(fs::copy_sync(&fs_env(state), &src_path, &dest_path)?)
}

//
// fstat / ftruncate
//
#[op2(async(lazy), fast)]
#[serde]
pub async fn op_fstat(
    state: Rc<RefCell<OpState>>,
    #[smi] rid: FileId,
) -> Result<FileStat, IOError> {
    Ok(fs::fstat(&fs_env_async(&state), rid)?)
}

#[op2]
#[serde]
pub fn op_fstat_sync(state: &mut OpState, #[smi] rid: FileId) -> Result<FileStat, IOError> {
    Ok(fs::fstat(&fs_env(state), rid)?)
}

#[op2(async(lazy), fast)]
pub async fn op_ftruncate(
    state: Rc<RefCell<OpState>>,
    #[smi] rid: FileId,
    #[smi] len: u64,
) -> Result<(), IOError> {
    Ok(fs::ftruncate(&fs_env_async(&state), rid, len)?)
}

#[op2(fast)]
pub fn op_ftruncate_sync(
    state: &mut OpState,
    #[smi] rid: FileId,
    #[smi] len: u64,
) -> Result<(), IOError> {
    Ok(fs::ftruncate(&fs_env(state), rid, len)?)
}

//
// mkdir / readdir - uses VFS with Create/Read permissions
//
#[op2(async(lazy), fast)]
pub async fn op_mkdir(
    state: Rc<RefCell<OpState>>,
    #[string] dir_path: String,
    recursive: bool,
) -> Result<(), IOError> {
    Ok(fs::mkdir(fs_env_async(&state), dir_path, recursive).await?)
}

#[op2(fast)]
pub fn op_mkdir_sync(
    state: &mut OpState,
    #[string] dir_path: String,
    recursive: bool,
) -> Result<(), IOError> {
    Ok(fs::mkdir_sync(&fs_env(state), &dir_path, recursive)?)
}

#[op2(async(lazy), fast)]
#[serde]
pub async fn op_readdir(
    state: Rc<RefCell<OpState>>,
    #[string] dir_path: String,
) -> Result<Vec<String>, IOError> {
    Ok(fs::readdir(fs_env_async(&state), dir_path).await?)
}

#[op2]
#[serde]
pub fn op_readdir_sync(
    state: &mut OpState,
    #[string] dir_path: String,
) -> Result<Vec<String>, IOError> {
    Ok(fs::readdir_sync(&fs_env(state), &dir_path)?)
}

//
// unlink / rename / rmdir - uses VFS with Delete/Write permissions
//
#[op2(async(lazy), fast)]
pub async fn op_unlink(
    state: Rc<RefCell<OpState>>,
    #[string] file_path: String,
) -> Result<(), IOError> {
    Ok(fs::unlink(fs_env_async(&state), file_path).await?)
}

#[op2(fast)]
pub fn op_unlink_sync(state: &mut OpState, #[string] file_path: String) -> Result<(), IOError> {
    Ok(fs::unlink_sync(&fs_env(state), &file_path)?)
}

#[op2(async(lazy), fast)]
pub async fn op_rename(
    state: Rc<RefCell<OpState>>,
    #[string] old_path: String,
    #[string] new_path: String,
) -> Result<(), IOError> {
    Ok(fs::rename(fs_env_async(&state), old_path, new_path).await?)
}

#[op2(fast)]
pub fn op_rename_sync(
    state: &mut OpState,
    #[string] old_path: String,
    #[string] new_path: String,
) -> Result<(), IOError> {
    Ok(fs::rename_sync(&fs_env(state), &old_path, &new_path)?)
}

#[op2(async(lazy), fast)]
pub async fn op_rmdir(
    state: Rc<RefCell<OpState>>,
    #[string] dir_path: String,
    recursive: bool,
) -> Result<(), IOError> {
    Ok(fs::rmdir(fs_env_async(&state), dir_path, recursive).await?)
}

#[op2(fast)]
pub fn op_rmdir_sync(
    state: &mut OpState,
    #[string] dir_path: String,
    recursive: bool,
) -> Result<(), IOError> {
    Ok(fs::rmdir_sync(&fs_env(state), &dir_path, recursive)?)
}

//
// stat - uses VFS with Read permission
//
#[op2(async(lazy), fast)]
#[serde]
pub async fn op_stat(
    state: Rc<RefCell<OpState>>,
    #[string] path: String,
    recursive: bool,
) -> Result<StatResult, IOError> {
    Ok(fs::stat(fs_env_async(&state), path, recursive).await?)
}

#[op2]
#[serde]
pub fn op_stat_sync(
    state: &mut OpState,
    #[string] path: String,
    recursive: bool,
) -> Result<StatResult, IOError> {
    Ok(fs::stat_sync(&fs_env(state), &path, recursive)?)
}

//
// write(fd)
//
#[op2(async(lazy))]
#[bigint]
pub fn op_write_file(
    state: Rc<RefCell<OpState>>,
    #[smi] rid: FileId,
    #[buffer] data_buf: Option<JsBuffer>,
    #[string] data_str: Option<String>,
    #[string] encoding: Option<String>,
    #[bigint] position: Option<u64>,
) -> Result<impl std::future::Future<Output = Result<usize, IOError>>, IOError> {
    let write = fs::write_fd(
        &fs_env_async(&state),
        rid,
        || prepare_data(data_buf, data_str, encoding)?.into_owned(),
        position,
    )?;
    Ok(async move { Ok(write.await?) })
}

#[op2]
#[bigint]
pub fn op_write_file_sync(
    state: &mut OpState,
    #[smi] rid: FileId,
    #[buffer] data_buf: Option<JsBuffer>,
    #[string] data_str: Option<String>,
    #[string] encoding: Option<String>,
    #[bigint] position: Option<u64>,
) -> Result<usize, IOError> {
    Ok(fs::write_fd_sync(
        &fs_env(state),
        rid,
        || prepare_data(data_buf, data_str, encoding),
        position,
    )?)
}

//
// readFile (path) - uses VFS for path resolution
//
#[op2(async(lazy))]
#[serde]
pub async fn op_read_file(
    state: Rc<RefCell<OpState>>,
    #[string] path: String,
    #[bigint] position: Option<u64>,
    #[bigint] length: Option<u64>,
) -> Result<ToJsBuffer, IOError> {
    Ok(fs::read_file(fs_env_async(&state), path, position, length)
        .await?
        .into())
}

#[op2]
#[serde]
pub fn op_read_file_sync(
    state: &mut OpState,
    #[string] path: String,
    #[bigint] position: Option<u64>,
    #[bigint] length: Option<u64>,
) -> Result<ToJsBuffer, IOError> {
    Ok(fs::read_file_sync(&fs_env(state), &path, position, length)?.into())
}

//
// read(fd) - fd-based read into buffer
//
#[op2(async(lazy))]
#[serde]
pub async fn op_read_fd(
    state: Rc<RefCell<OpState>>,
    #[smi] rid: FileId,
    #[bigint] length: u64,
    #[bigint] position: Option<u64>,
) -> Result<ToJsBuffer, IOError> {
    Ok(fs::read_fd(fs_env_async(&state), rid, length, position)
        .await?
        .into())
}

#[op2]
#[serde]
pub fn op_read_fd_sync(
    state: &mut OpState,
    #[smi] rid: FileId,
    #[bigint] length: u64,
    #[bigint] position: Option<u64>,
) -> Result<ToJsBuffer, IOError> {
    Ok(fs::read_fd_sync(&fs_env(state), rid, length, position)?.into())
}

/// Expose the filled prefix as a Uint8Array while its backing ArrayBuffer
/// represents all initialized storage retained until V8 releases the result.
pub struct FileReadBuffer(fs_ops::OwnedFileRead);

impl<'a> deno_core::ToV8<'a> for FileReadBuffer {
    type Error = IOError;

    fn to_v8<'i>(
        self,
        scope: &mut deno_core::v8::PinScope<'a, 'i>,
    ) -> Result<deno_core::v8::Local<'a, deno_core::v8::Value>, Self::Error> {
        use deno_core::v8;
        let (storage, filled) = self.0.into_parts();
        let buffer = if storage.is_empty() {
            v8::ArrayBuffer::new(scope, 0)
        } else {
            let backing =
                v8::ArrayBuffer::new_backing_store_from_boxed_slice(storage).make_shared();
            v8::ArrayBuffer::with_backing_store(scope, &backing)
        };
        v8::Uint8Array::new(scope, buffer, 0, filled)
            .map(|view| view.into())
            .ok_or_else(|| ioerr("failed to create file read view"))
    }
}

// Async BYOB reads stage owned bytes on the worker. Only the JS completion
// writes the original view; retaining a backing store never grants exclusivity.
#[op2(async(lazy))]
pub fn op_read_fd_into(
    state: Rc<RefCell<OpState>>,
    #[smi] rid: FileId,
    #[buffer] buf: JsBuffer,
    #[bigint] position: Option<u64>,
) -> Result<impl std::future::Future<Output = Result<FileReadBuffer, IOError>>, IOError> {
    let buf = validate_file_buffer(buf)?;
    let len = buf.len();
    drop(buf);
    let read = fs::read_fd_for_buffer(fs_env_async(&state), rid, len, position);
    Ok(async move { Ok(FileReadBuffer(read.await?)) })
}

// The sync path blocks V8 until the worker joins, so validated fixed/nonshared
// backing can be written directly. Number returns preserve counts above i32::MAX.

#[op2]
#[number]
pub fn op_read_fd_into_sync(
    state: &mut OpState,
    #[smi] rid: FileId,
    #[buffer] buf: JsBuffer,
    #[bigint] position: Option<u64>,
) -> Result<usize, IOError> {
    let buf = validate_file_buffer(buf)?;
    Ok(fs::read_fd_into_sync(&fs_env(state), rid, buf, position)?)
}

//
// readCompressedFile (path) - read brotli-compressed file
//
#[op2(async(lazy), fast)]
#[serde]
pub async fn op_read_compressed_file(
    state: Rc<RefCell<OpState>>,
    #[string] path: String,
) -> Result<ToJsBuffer, IOError> {
    Ok(fs::read_compressed(fs_env_async(&state), path)
        .await?
        .into())
}

#[op2]
#[serde]
pub fn op_read_compressed_file_sync(
    state: &mut OpState,
    #[string] path: String,
) -> Result<ToJsBuffer, IOError> {
    Ok(fs::read_compressed_sync(&fs_env(state), &path)?.into())
}

// ============================ ReadZipEntry ============================

/// One entry's result on its way to JS.
///
/// `text` and `bytes` are mutually exclusive, expressed as two optional
/// fields rather than an enum on purpose: `serde(untagged)` routes through
/// serde's content-buffering serializer, which loses the marker serde_v8 uses
/// to turn a [`ToJsBuffer`] into a `Uint8Array` — the bytes would arrive in JS
/// as a plain array of numbers. Two fields keep the buffer on serde_v8's fast
/// path, and the JS shim picks whichever is present.
#[derive(serde::Serialize)]
struct ZipEntryPayload {
    path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bytes: Option<ToJsBuffer>,
    #[serde(rename = "errMsg")]
    err_msg: String,
}

#[op2(async(lazy), fast)]
#[serde]
pub async fn op_read_zip_entry(
    state: Rc<RefCell<OpState>>,
    #[string] zip_path: String,
    #[string] entries_json: String,
) -> Result<Vec<ZipEntryPayload>, IOError> {
    let results = fs::read_zip_entry(fs_env_async(&state), zip_path, entries_json).await?;
    // A list, not a map: the shim builds the keyed object, and a list keeps the
    // archive's own entry order instead of `serde_json::Map`'s sorted one.
    // Binary payloads ride out as `ToJsBuffer`, which serde_v8 adopts as a
    // `Uint8Array` backing store rather than copying.
    Ok(results
        .into_iter()
        .map(|entry| {
            let (text, bytes) = match entry.data {
                Some(ZipEntryData::Text(s)) => (Some(s), None),
                Some(ZipEntryData::Binary(b)) => (None, Some(b.into())),
                None => (None, None),
            };
            ZipEntryPayload {
                path: entry.path,
                text,
                bytes,
                err_msg: entry.err_msg,
            }
        })
        .collect())
}

// ============================ Unzip ============================

/// Unzip operation with platform service dispatch.
///
/// If the platform provides a `FileService` (e.g. Android's JNI `java.util.zip`),
/// uses that. Otherwise falls back to Rust `zip` crate via the archive scheduler pool.
#[op2(async(lazy), fast)]
pub async fn op_unzip(
    state: Rc<RefCell<OpState>>,
    #[string] zip_file_path: String,
    #[string] target_path: String,
) -> Result<(), IOError> {
    let (env, file_service) = {
        let st = state.borrow();
        let host = st.borrow::<HostOpState>();
        let file_service = host.device_services.as_ref().and_then(|s| s.file());
        (fs_env(&st), file_service)
    };
    Ok(fs::unzip(env, zip_file_path, target_path, file_service).await?)
}

// ============================ GetFileInfo ============================

#[op2(async(lazy), fast)]
#[serde]
pub async fn op_get_file_info(
    state: Rc<RefCell<OpState>>,
    #[string] path: String,
    #[string] algorithm: String,
) -> Result<(u64, String), IOError> {
    Ok(fs::get_file_info(fs_env_async(&state), path, algorithm).await?)
}

#[op2]
#[serde]
pub fn op_get_file_info_sync(
    state: &mut OpState,
    #[string] path: String,
    #[string] algorithm: String,
) -> Result<(u64, String), IOError> {
    Ok(fs::get_file_info_sync(&fs_env(state), &path, &algorithm)?)
}

// ============================ ListSavedFiles ============================

#[op2(async(lazy), fast)]
#[serde]
pub async fn op_list_saved_files(
    state: Rc<RefCell<OpState>>,
    #[string] dir: String,
    #[string] prefix: String,
) -> Result<Vec<SavedFileInfo>, IOError> {
    Ok(fs::list_saved_files(fs_env_async(&state), dir, prefix).await?)
}

#[cfg(test)]
mod tests {
    use std::{path::PathBuf, sync::Arc};

    /// No op here escapes to tokio's unbounded blocking pool; the service
    /// module holds the same line for the work (`fs_tests.rs`).
    #[test]
    fn q12_production_file_ops_do_not_escape_to_tokio_blocking_pool() {
        let source = include_str!("fs.rs");
        let production = source
            .split("#[cfg(test)]")
            .next()
            .expect("fs production source");

        assert!(
            !production.contains("tokio::task::spawn_blocking"),
            "production file ops bypass the bounded R5 executor"
        );
    }

    /// The one thing about the zip-entry payload that cannot be reasoned out
    /// from the type: what serde_v8 actually hands JS for a `ToJsBuffer` that
    /// sits inside a struct inside a `Vec`.
    ///
    /// If it degrades to a plain array of numbers, the whole point of moving
    /// off base64 is lost and the JS shim would silently produce garbage. This
    /// runs the real serializer through a real isolate and asks JS.
    #[test]
    fn zip_entry_bytes_reach_js_as_a_uint8array() {
        use deno_core::{FastString, JsRuntime, RuntimeOptions, op2};

        #[op2]
        #[serde]
        fn op_zip_payload_fixture() -> Vec<super::ZipEntryPayload> {
            vec![
                super::ZipEntryPayload {
                    path: "img/bg.png".to_string(),
                    text: None,
                    bytes: Some(vec![0x00, 0xFF, 0x80, 0x01].into()),
                    err_msg: String::new(),
                },
                super::ZipEntryPayload {
                    path: "cfg.json".to_string(),
                    text: Some("{\"a\":1}".to_string()),
                    bytes: None,
                    err_msg: String::new(),
                },
                super::ZipEntryPayload {
                    path: "gone.txt".to_string(),
                    text: None,
                    bytes: None,
                    err_msg: "entry not found".to_string(),
                },
            ]
        }

        deno_core::extension!(zip_payload_fixture, ops = [op_zip_payload_fixture]);

        let mut rt = JsRuntime::new(RuntimeOptions {
            extensions: vec![zip_payload_fixture::init()],
            ..Default::default()
        });

        // Assertions as `throw`s: `execute_script` surfaces them as `Err`.
        let script = r#"
            const r = Deno.core.ops.op_zip_payload_fixture();
            if (r.length !== 3) throw new Error("length " + r.length);

            const bin = r[0];
            if (bin.path !== "img/bg.png") throw new Error("path " + bin.path);
            if (!(bin.bytes instanceof Uint8Array)) {
                throw new Error("bytes is " + Object.prototype.toString.call(bin.bytes)
                    + " -- serde_v8 did not produce a typed array");
            }
            if (bin.bytes.length !== 4) throw new Error("byte length " + bin.bytes.length);
            if (bin.bytes[0] !== 0 || bin.bytes[1] !== 255 || bin.bytes[2] !== 128 || bin.bytes[3] !== 1) {
                throw new Error("bytes " + Array.from(bin.bytes).join(","));
            }
            // The shim hands `.buffer` to callers, so the view must cover it exactly.
            if (bin.bytes.byteOffset !== 0 || bin.bytes.byteLength !== bin.bytes.buffer.byteLength) {
                throw new Error("view is not exact: offset=" + bin.bytes.byteOffset
                    + " len=" + bin.bytes.byteLength + " buf=" + bin.bytes.buffer.byteLength);
            }
            if (bin.text !== undefined) throw new Error("text should be absent");

            const txt = r[1];
            if (txt.text !== '{"a":1}') throw new Error("text " + txt.text);
            if (txt.bytes !== undefined) throw new Error("bytes should be absent");

            const err = r[2];
            if (err.errMsg !== "entry not found") throw new Error("errMsg " + err.errMsg);
            if (err.text !== undefined || err.bytes !== undefined) {
                throw new Error("a failed entry must carry no payload");
            }
        "#;

        rt.execute_script("<test:zip-payload>", FastString::from_static(script))
            .expect("zip entry payload shape");
    }

    /// The other half of the zip-entry path: the shim that turns the op's list
    /// into the `{entries: {...}}` object callers actually see. Driven through
    /// the real ESM export in a live runtime, so it is the shipped function
    /// under test rather than a copy of it.
    #[test]
    fn zip_entries_shim_maps_bytes_text_and_failures() {
        use deno_core::{FastString, JsRuntime, RuntimeOptions};

        deno_core::extension!(
            zip_shim_bridge,
            deps = [host_v8_file],
            esm_entry_point = "ext:zip_shim_bridge/bridge.js",
            esm = ["ext:zip_shim_bridge/bridge.js" = {
                source = r#"
                    import { zipEntriesFromOpResult } from "ext:host_v8_file/02_file_manager.js";
                    globalThis.__zipEntriesFromOpResult = zipEntriesFromOpResult;
                "#
            },],
        );

        let mut extensions = crate::main_extensions(zip_test_host_state());
        extensions.push(zip_shim_bridge::init());
        let mut rt = JsRuntime::new(RuntimeOptions {
            extensions,
            ..Default::default()
        });

        let script = r#"
            const shape = globalThis.__zipEntriesFromOpResult([
                { path: "img/bg.png", bytes: new Uint8Array([0, 255, 128, 1]), errMsg: "" },
                { path: "cfg.json",   text: '{"a":1}',                          errMsg: "" },
                { path: "gone.txt",                                             errMsg: "entry not found" },
            ]);

            const bin = shape.entries["img/bg.png"];
            if (!(bin.data instanceof ArrayBuffer)) {
                throw new Error("binary data is " + Object.prototype.toString.call(bin.data));
            }
            const view = new Uint8Array(bin.data);
            if (view.length !== 4 || view[1] !== 255 || view[2] !== 128) {
                throw new Error("bytes " + Array.from(view).join(","));
            }
            if (bin.errMsg !== "") throw new Error("errMsg " + bin.errMsg);

            const txt = shape.entries["cfg.json"];
            if (txt.data !== '{"a":1}') throw new Error("text " + txt.data);

            const gone = shape.entries["gone.txt"];
            if (gone.data !== null) throw new Error("a failed entry must carry null data");
            if (gone.errMsg !== "entry not found") throw new Error("errMsg " + gone.errMsg);

            // Archive order, not sorted order -- the op returns a list for this reason.
            const keys = Object.keys(shape.entries).join(",");
            if (keys !== "img/bg.png,cfg.json,gone.txt") throw new Error("order " + keys);
        "#;

        rt.execute_script("<test:zip-shim>", FastString::from_static(script))
            .expect("zip entries shim");
    }

    pub(super) fn zip_test_host_state() -> shared::op_state::HostOpState {
        use shared::channel::ThreadWakeup;
        use shared::device::gpu_caps::GpuCaps;
        use shared::op_state::{AudioSender, HostOpState, NetworkPolicy};
        use shared::render_command_sender::CommandSender;
        use std::sync::atomic::AtomicBool;

        let (render_tx, _render_rx) = CommandSender::new();
        let (host_tx, _critical_host_tx, _host_rx) = shared::host_channel::channel(1);

        HostOpState {
            callback_ids: Arc::new(shared::callback_id::CallbackIdAllocator::default()),
            runtime_generation: 1,
            id: 1,
            app_cache_dir: PathBuf::from("/tmp/cache"),
            app_files_dir: PathBuf::from("/tmp/files"),
            code_dir: None,
            game_paths: None,
            vfs: None,
            mount_table: None,
            render_tx,
            text_measurer: None,
            audio_tx: AudioSender::new(shared::audio_channel::disconnected(), ThreadWakeup::new()),
            host_tx,
            device_services: None,
            raf_rx: None,
            raf_demand: Arc::new(shared::raf_signal::RafDemand::new()),
            request_vsync: None,
            sub_packages: Vec::new(),
            workers_path: None,
            network_policy: NetworkPolicy::default(),
            backgrounded: Arc::new(AtomicBool::new(false)),
            timer_backgrounded: Arc::new(AtomicBool::new(false)),
            webgl_context_created: Arc::new(AtomicBool::new(false)),
            context_lost: Arc::new(shared::op_state::ContextLostState::default()),
            code_signing_enabled: false,
            gpu_caps: GpuCaps::new(),
        }
    }
}

#[cfg(test)]
#[path = "fs_byob_tests.rs"]
mod byob_tests;

//! Content's `uploadFile`: a multipart POST that streams a file out of the
//! session's sandbox.
//!
//! The file is fed straight from disk into the multipart encoder, so the
//! process never holds a second copy: peak user-space memory for an upload is
//! one 64 KiB chunk, not the file. The path content names is virtual
//! (`/user/save.png`) and is resolved through the same VFS boundary the file
//! API uses, so an upload reads exactly what a read may. The destination is
//! held to the network policy, as every request is.
//!
//! A failure is answered, not thrown: the facade reports `uploadFile:fail ...`
//! through the result, and content's `fail` callback is what sees it.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures::{Stream, StreamExt};
use http::HeaderMap;
use http::header::{CONTENT_LENGTH, CONTENT_TYPE, HOST, HeaderName, HeaderValue};
use reqwest::{Body, Client};
use tracing::debug;
use url::Url;

use shared::op_state::NetworkPolicy;

use crate::ServiceError;

use super::gate::{self, GateKind};
use super::resources::{CancelFlag, ResourceId, ResourceTable};

/// How much of an upload's answer is held for content to read.
///
/// The request body streams; the response is handed back whole, so it is
/// bounded on its own. `Content-Length` is not enough -- a chunked answer is
/// checked as its bytes arrive.
const MAX_BUFFERED_RESPONSE_BYTES: usize = 32 * 1024 * 1024;

/// What one read pulls off the file. Large enough that the syscalls are not
/// the cost, small enough that an upload paused by backpressure pins kilobytes
/// rather than megabytes per connection.
const UPLOAD_CHUNK_BYTES: usize = 64 * 1024;

/// What an upload answers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UploadAnswer {
    pub data: String,
    pub status_code: u16,
    pub headers: Vec<(Vec<u8>, Vec<u8>)>,
    pub total_bytes_sent: u64,
    pub error: Option<String>,
}

impl UploadAnswer {
    /// What an aborted upload answers, for a caller that finds the handle gone
    /// before the upload starts.
    pub fn aborted_answer() -> Self {
        Self::aborted()
    }

    /// What an aborted upload answers: content sees `uploadFile:fail aborted`
    /// rather than a throw, which is what its facade reports.
    fn aborted() -> Self {
        Self {
            error: Some("uploadFile:fail aborted".to_string()),
            ..Default::default()
        }
    }

    fn failed(message: impl Into<String>) -> Self {
        Self {
            error: Some(message.into()),
            ..Default::default()
        }
    }
}

/// What an upload is made with: the session's policy, its client and the
/// sandbox the file is read from.
pub struct UploadEnv<'a> {
    pub policy: &'a NetworkPolicy,
    pub client: &'a Client,
    pub resources: &'a ResourceTable,
    pub vfs: Option<&'a shared::vfs::VirtualFS>,
    pub mount_table: Option<&'a shared::vfs::MountTable>,
}

/// One upload's parameters, as content gave them.
pub struct UploadRequest {
    pub cancel_rid: ResourceId,
    pub url: String,
    pub file_path: String,
    /// The multipart field the file is sent as.
    pub name: String,
    /// The file name the server is told, which is content's, never the
    /// engine's internal path.
    pub filename: String,
    pub headers: Vec<(Vec<u8>, Vec<u8>)>,
    pub form_data: Vec<(String, String)>,
    pub timeout_ms: u32,
}

/// The handle an in-flight upload is aborted with.
///
/// Content is given it before it awaits the upload, keeps it on the task, and
/// `abort()` closes it -- which cancels the upload. The same shape a download
/// gets from `op_fetch`.
pub fn cancel_handle(resources: &ResourceTable) -> ResourceId {
    resources.add_cancel(Arc::new(CancelFlag::default()))
}

/// Stream `request.file_path` to `request.url` as one multipart part.
pub async fn upload(
    env: UploadEnv<'_>,
    request: UploadRequest,
) -> Result<UploadAnswer, ServiceError> {
    let UploadRequest {
        cancel_rid,
        url,
        file_path,
        name,
        filename,
        headers,
        form_data,
        timeout_ms,
    } = request;

    // Resolved before any asynchronous file work, through the boundary the
    // file API uses.
    let real_path = resolve_upload_path(env.vfs, env.mount_table, &file_path)?;

    // The cancel handle is taken now, before the first await. Content's
    // `abort()` closes it synchronously, so a handle that is already gone
    // means cancelled -- never "cannot be cancelled".
    let cancel = if cancel_rid == 0 {
        None
    } else {
        match env.resources.cancel_flag(cancel_rid) {
            Some(flag) => Some(flag),
            None => return Ok(UploadAnswer::aborted()),
        }
    };
    let until = |flag: &Option<Arc<CancelFlag>>| flag.clone();

    let file = match run(until(&cancel), tokio::fs::File::open(&real_path)).await {
        Cancellable::Cancelled => return Ok(UploadAnswer::aborted()),
        Cancellable::Done(Err(error)) => {
            return Err(generic(format!(
                "uploadFile:fail open {file_path}: {error}"
            )));
        }
        Cancellable::Done(Ok(file)) => file,
    };
    let file_size = match run(until(&cancel), file.metadata()).await {
        Cancellable::Cancelled => return Ok(UploadAnswer::aborted()),
        Cancellable::Done(metadata) => metadata.map(|metadata| metadata.len()).unwrap_or(0),
    };

    // One `file_name` for both bodies, because two of them is how the branches
    // came to disagree: the zero-length branch named the part after the
    // engine's internal path, which leaked the sandbox's layout to the server
    // for every file whose metadata reported no length.
    let part = if file_size == 0 {
        // No length to declare, so the part is chunked; reqwest picks that for
        // a part with no declared length.
        match run(until(&cancel), tokio::fs::File::open(&real_path)).await {
            Cancellable::Cancelled => return Ok(UploadAnswer::aborted()),
            Cancellable::Done(Err(error)) => {
                return Err(generic(format!("uploadFile:fail reopen: {error}")));
            }
            Cancellable::Done(Ok(reopened)) => {
                reqwest::multipart::Part::stream(Body::wrap_stream(chunks(reopened)))
            }
        }
    } else {
        reqwest::multipart::Part::stream_with_length(Body::wrap_stream(chunks(file)), file_size)
    }
    .file_name(filename.clone())
    .mime_str(mime_of(&filename))
    .map_err(|error| generic(error.to_string()))?;

    let mut form = reqwest::multipart::Form::new().part(name, part);
    for (key, value) in form_data {
        form = form.text(key, value);
    }

    let parsed = Url::parse(&url).map_err(|_| type_error("Invalid URL"))?;
    // The same preflight `fetch` runs: scheme, allow list, HTTPS, IP literals.
    gate::enforce(&parsed, env.policy, GateKind::FetchUpload).map_err(ServiceError::generic)?;
    debug!("Upload request: POST {parsed}");

    let mut map = HeaderMap::new();
    for (key, value) in headers {
        let name = HeaderName::from_bytes(&key).map_err(|_| type_error("Invalid Header"))?;
        let value =
            HeaderValue::from_bytes(&value).map_err(|_| type_error("Invalid Header Value"))?;
        // Content-Type and Content-Length are the multipart encoder's; Host
        // and the forwarding headers are refused for the reason `fetch`
        // refuses them.
        if name != CONTENT_TYPE
            && name != CONTENT_LENGTH
            && name != HOST
            && !super::fetch::is_blocked_header(&name)
        {
            map.append(name, value);
        }
    }

    let request = env
        .client
        .post(parsed)
        .timeout(Duration::from_millis(u64::from(timeout_ms)))
        .multipart(form)
        .headers(map);

    // The send and the read of the answer are one cancellable unit: an abort
    // mid-body stops both.
    let exchange = async move {
        let response = request.send().await.map_err(|error| error.to_string())?;
        let status = response.status().as_u16();
        let headers: Vec<(Vec<u8>, Vec<u8>)> = response
            .headers()
            .iter()
            .map(|(name, value)| (name.as_str().as_bytes().to_vec(), value.as_bytes().to_vec()))
            .collect();
        let mut stream = response.bytes_stream();
        let mut body = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|error| error.to_string())?;
            if body.len().saturating_add(chunk.len()) > MAX_BUFFERED_RESPONSE_BYTES {
                return Ok::<UploadAnswer, String>(UploadAnswer {
                    status_code: status,
                    headers,
                    total_bytes_sent: file_size,
                    error: Some("uploadFile:fail response body exceeds limit".to_string()),
                    ..Default::default()
                });
            }
            body.extend_from_slice(&chunk);
        }
        Ok(UploadAnswer {
            data: String::from_utf8_lossy(&body).into_owned(),
            status_code: status,
            headers,
            total_bytes_sent: file_size,
            error: None,
        })
    };

    Ok(match run(until(&cancel), exchange).await {
        Cancellable::Cancelled => UploadAnswer::aborted(),
        // A network or decode failure is answered through `error`, as a failed
        // send is: content's `fail` callback reads it rather than catching.
        Cancellable::Done(Err(error)) => UploadAnswer::failed(error),
        Cancellable::Done(Ok(answer)) => answer,
    })
}

/// What a step of the upload answered, or that it was cancelled first.
enum Cancellable<T> {
    Done(T),
    Cancelled,
}

/// Run `work`, stopping if the upload is cancelled. With no handle it simply
/// runs -- content that asked for no cancel handle cannot abort.
async fn run<T>(
    cancel: Option<Arc<CancelFlag>>,
    work: impl std::future::Future<Output = T>,
) -> Cancellable<T> {
    match cancel {
        None => Cancellable::Done(work.await),
        Some(flag) => match flag.until_cancelled(work).await {
            Some(value) => Cancellable::Done(value),
            None => Cancellable::Cancelled,
        },
    }
}

/// The media type a file name implies, which is what the server is told.
fn mime_of(filename: &str) -> &'static str {
    match filename
        .rsplit('.')
        .next()
        .map(str::to_lowercase)
        .as_deref()
    {
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("png") => "image/png",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("mp3") => "audio/mpeg",
        Some("mp4") => "video/mp4",
        Some("wav") => "audio/wav",
        Some("ogg") => "audio/ogg",
        Some("json") => "application/json",
        Some("xml") => "application/xml",
        Some("txt") => "text/plain",
        Some("zip") => "application/zip",
        _ => "application/octet-stream",
    }
}

/// The file as a stream of chunks the multipart encoder pulls from, so the
/// body is never materialized.
fn chunks(file: tokio::fs::File) -> impl Stream<Item = std::io::Result<Bytes>> {
    use tokio::io::AsyncReadExt;
    futures::stream::unfold((file, false), |(mut file, done)| async move {
        if done {
            return None;
        }
        let mut buffer = vec![0u8; UPLOAD_CHUNK_BYTES];
        match file.read(&mut buffer).await {
            Ok(0) => None,
            Ok(count) => {
                buffer.truncate(count);
                Some((Ok(Bytes::from(buffer)), (file, false)))
            }
            Err(error) => Some((Err(error), (file, true))),
        }
    })
}

/// The virtual path content named, as a real one.
///
/// Neither the file API's resolver nor its error variants are reused: an
/// upload only cares whether the path is readable, and its failures keep the
/// `uploadFile:` prefix the facade reports.
fn resolve_upload_path(
    vfs: Option<&shared::vfs::VirtualFS>,
    mount_table: Option<&shared::vfs::MountTable>,
    path: &str,
) -> Result<PathBuf, ServiceError> {
    use shared::vfs::VfsError;

    let virtual_path = if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/code/{path}")
    };

    // `/code` goes through the mount table first, because it may be
    // package-backed -- and a file inside a package cannot be streamed.
    if (virtual_path == "/code" || virtual_path.starts_with("/code/"))
        && let Some(table) = mount_table
        && let Some(resolved) = table.resolve_code_path(&virtual_path)
    {
        if let Some(real) = resolved.real_path {
            return Ok(real);
        }
        return Err(generic(format!(
            "uploadFile:fail {path} is inside a package and cannot be streamed"
        )));
    }

    let vfs = vfs.ok_or_else(|| generic("uploadFile:fail VFS not initialised"))?;
    vfs.resolve(&virtual_path, shared::vfs::FileOp::Read)
        .map_err(|error| {
            generic(match error {
                VfsError::PathNotAllowed => format!(
                    "uploadFile:fail path not allowed: {path} (use /user, /cache, /code, /tmp)"
                ),
                VfsError::PermissionDenied => format!("uploadFile:fail permission denied: {path}"),
                VfsError::PathTraversal => format!("uploadFile:fail path traversal: {path}"),
                VfsError::SymlinkEscape => format!("uploadFile:fail symlink escape: {path}"),
                VfsError::SymlinkNotAllowed => {
                    format!("uploadFile:fail symlinks not allowed: {path}")
                }
                VfsError::InvalidPath => format!("uploadFile:fail invalid path: {path}"),
            })
        })
}

fn generic(message: impl Into<String>) -> ServiceError {
    ServiceError::generic(message)
}

fn type_error(message: impl Into<String>) -> ServiceError {
    ServiceError::classed("TypeError", message)
}

#[cfg(test)]
#[path = "upload_tests.rs"]
mod tests;

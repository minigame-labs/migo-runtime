//! Paths content names in a request to the host, made into paths the host can
//! open -- and only for files the game can itself read.
//!
//! A host is handed a real path, never content's string: content chooses the
//! path, and a host that opened it verbatim would read whatever its own process
//! can -- `saveImageToPhotosAlbum({filePath: "<the host app's database>"})` once
//! put that file in the player's photo album. Every path goes through the
//! sandbox first. What a result names comes back through
//! `shared::services::host_files`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

use deno_core::OpState;
use deno_core::serde_json::{self, Value};
use deno_error::JsErrorBox;
use migo_services::fs;
use shared::op_state::HostOpState;
use shared::services::host_files::{self, Step};
use shared::vfs::FileOp;

use crate::io_state::IoSchedulerState;

/// Whether a request may name a remote resource where it names a file.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Remote {
    /// A viewer can show an http(s) URL itself.
    Allowed,
    Refused,
}

/// `request_json` with every path `fields` names replaced by a real path the host
/// can open.
///
/// A path in the game's package that has no file of its own (a pack-backed
/// `/code`) is copied out; the copy is held for request `requestId` and removed
/// when its result arrives.
pub(crate) async fn export_paths(
    state: &Rc<RefCell<OpState>>,
    request_json: &str,
    fields: &[&[Step]],
    remote: Remote,
) -> Result<String, JsErrorBox> {
    let mut request: Value = serde_json::from_str(request_json)
        .map_err(|e| JsErrorBox::generic(format!("malformed request: {e}")))?;
    let request_id = request
        .get("requestId")
        .and_then(Value::as_u64)
        .ok_or_else(|| JsErrorBox::generic("malformed request: no requestId"))?;
    export_fields(state, &mut request, request_id, fields, remote).await?;
    Ok(request.to_string())
}

/// `message`'s paths, as `export_paths` makes them, for a message no result
/// answers -- a command or a reply: what it copies out is held for the rest of
/// the session (`host_files::hold_export`'s request 0).
pub(crate) async fn export_paths_for_session(
    state: &Rc<RefCell<OpState>>,
    message: &mut Value,
    fields: &[&[Step]],
    remote: Remote,
) -> Result<(), JsErrorBox> {
    export_fields(state, message, 0, fields, remote).await
}

async fn export_fields(
    state: &Rc<RefCell<OpState>>,
    request: &mut Value,
    request_id: u64,
    fields: &[&[Step]],
    remote: Remote,
) -> Result<(), JsErrorBox> {
    let mut named: Vec<String> = Vec::new();
    for field in fields {
        host_files::visit_fields(request, field, &mut |value| match value {
            Value::String(path) => {
                if !named.contains(path) {
                    named.push(path.clone());
                }
                Ok(())
            }
            _ => Err("a file path is not a string".to_string()),
        })
        .map_err(JsErrorBox::generic)?;
    }

    let (host_id, scheduler, vfs, mount_table) = {
        let st = state.borrow();
        let host = st.borrow::<HostOpState>();
        (
            host.id,
            st.borrow::<IoSchedulerState>().0.clone(),
            host.vfs.clone(),
            host.mount_table.clone(),
        )
    };

    let mut real: HashMap<String, String> = HashMap::with_capacity(named.len());
    for path in named {
        if remote == Remote::Allowed
            && (path.starts_with("https://") || path.starts_with("http://"))
        {
            real.insert(path.clone(), path);
            continue;
        }
        let resolved =
            fs::resolve_path_vfs(vfs.as_deref(), mount_table.as_deref(), &path, FileOp::Read)
                .map_err(|e| JsErrorBox::generic(e.message))?;
        let host_path = match resolved {
            fs::ResolvedPath::Filesystem(file) => {
                if !std::fs::metadata(&file).is_ok_and(|meta| meta.is_file()) {
                    return Err(JsErrorBox::generic(format!("file not found: {path}")));
                }
                file
            }
            fs::ResolvedPath::Pack { virtual_path } => {
                let mount_table = mount_table
                    .clone()
                    .ok_or_else(|| JsErrorBox::generic("mount table not initialized"))?;
                // The extension is what tells the host's decoder or viewer what
                // the bytes are.
                let suffix = std::path::Path::new(&virtual_path)
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .map(|ext| format!(".{ext}"))
                    .unwrap_or_default();
                let copy = fs::materialize_pack_to_temp_async(
                    scheduler.clone(),
                    mount_table,
                    virtual_path,
                    suffix,
                )
                .await
                .map_err(|e| JsErrorBox::generic(e.message))?;
                if !host_files::hold_export(host_id, request_id, PathBuf::from(&copy)) {
                    let _ = std::fs::remove_file(&copy);
                    return Err(JsErrorBox::generic("the session has ended"));
                }
                copy
            }
        };
        real.insert(path, host_path);
    }

    for field in fields {
        host_files::visit_fields(request, field, &mut |value| {
            if let Value::String(path) = value
                && let Some(host_path) = real.get(path.as_str())
            {
                *value = Value::String(host_path.clone());
            }
            Ok(())
        })
        .map_err(JsErrorBox::generic)?;
    }
    Ok(())
}

//! Paths content names in a request to the host, made into paths the host can
//! open -- and only for files the game can itself read. Both executions export
//! through here: the embedded ops, and the external session's host for content
//! in another process.
//!
//! A host is handed a real path, never content's string: content chooses the
//! path, and a host that opened it verbatim would read whatever its own process
//! can -- `saveImageToPhotosAlbum({filePath: "<the host app's database>"})` once
//! put that file in the player's photo album. Every path goes through the
//! sandbox first. What a result names comes back through
//! `shared::services::host_files`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use migo_io::scheduler::IoScheduler;
use serde_json::Value;
use shared::services::host_files::{self, Step};
use shared::vfs::{FileOp, MountTable, VirtualFS};

use crate::error::ServiceError;
use crate::fs;

/// Whether a request may name a remote resource where it names a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Remote {
    /// A viewer can show an http(s) URL itself.
    Allowed,
    Refused,
}

/// Where a request names content's files, and whether a network resource may
/// stand in for one. Which fields are files is the engine's to know, not
/// content's to declare: each is resolved through the sandbox before the
/// request leaves.
#[derive(Debug, Clone, Copy)]
pub struct Files {
    pub fields: &'static [&'static [Step]],
    pub remote: Remote,
}

/// `saveImageToPhotosAlbum`: the file to save.
pub const SAVE_IMAGE_TO_PHOTOS_ALBUM: Files = Files {
    fields: &[&[Step::Key("filePath")]],
    remote: Remote::Refused,
};
/// `compressImage`: the image to compress.
pub const COMPRESS_IMAGE: Files = Files {
    fields: &[&[Step::Key("src")]],
    remote: Remote::Refused,
};
/// `previewImage`: every url, and the one shown first, which is one of them.
pub const PREVIEW_IMAGE: Files = Files {
    fields: &[&[Step::Key("urls"), Step::Each], &[Step::Key("current")]],
    remote: Remote::Allowed,
};
/// `previewMedia`: each source and its cover.
pub const PREVIEW_MEDIA: Files = Files {
    fields: &[
        &[Step::Key("sources"), Step::Each, Step::Key("url")],
        &[Step::Key("sources"), Step::Each, Step::Key("poster")],
    ],
    remote: Remote::Allowed,
};
/// `saveFileToDisk`: the file to save, which must be content's own.
pub const SAVE_FILE_TO_DISK: Files = Files {
    fields: &[&[Step::Key("filePath")]],
    remote: Remote::Refused,
};
/// `shareAppMessage`'s image, which may also be a network image.
pub const SHARE_APP_MESSAGE: Files = Files {
    fields: &[&[Step::Key("imageUrl")]],
    remote: Remote::Allowed,
};
/// `shareMessageToFriend`'s image, which may not.
pub const SHARE_MESSAGE_TO_FRIEND: Files = Files {
    fields: &[&[Step::Key("imageUrl")]],
    remote: Remote::Refused,
};
/// `showShareImageMenu`'s image, a local file.
pub const SHOW_SHARE_IMAGE_MENU: Files = Files {
    fields: &[&[Step::Key("path")]],
    remote: Remote::Refused,
};
/// The images a menu share's content may name, network images included. Each
/// is exported on its own: one that is not content's file is dropped, and the
/// host shares with its own default.
pub const MENU_SHARE: [&[Step]; 2] = [
    &[Step::Key("content"), Step::Key("imageUrl")],
    &[Step::Key("content"), Step::Key("imagePreviewUrl")],
];
/// A video decoder's source, held for the session: the decoder reads the file
/// for as long as it runs.
pub const VIDEO_DECODER_START: Files = Files {
    fields: &[&[Step::Key("source")]],
    remote: Remote::Allowed,
};

/// The files an ecosystem request for `api` names, if it names any.
pub fn ecosystem_files(api: &str) -> Option<Files> {
    let fields: &'static [&'static [Step]] = match api {
        "shareImageToGroup" | "shareEmojiToGroup" => {
            &[&[Step::Key("options"), Step::Key("imagePath")]]
        }
        "shareVideoToGroup" => &[
            &[Step::Key("options"), Step::Key("videoPath")],
            &[Step::Key("options"), Step::Key("thumbPath")],
        ],
        _ => return None,
    };
    Some(Files {
        fields,
        remote: Remote::Refused,
    })
}

/// The game's sandbox, as a session holds it: what a path content names is
/// resolved in, and where a package entry is copied out from.
pub struct Sandbox {
    /// The session, whose `/tmp` holds what is copied out.
    pub host_id: i32,
    pub scheduler: Arc<IoScheduler>,
    pub vfs: Option<Arc<VirtualFS>>,
    pub mount_table: Option<Arc<MountTable>>,
}

/// `request_json` with every path `fields` names replaced by a real path the
/// host can open.
///
/// A path in the game's package that has no file of its own (a pack-backed
/// `/code`) is copied out; the copy is held for request `requestId` and removed
/// when its result arrives.
pub async fn export_paths(
    sandbox: &Sandbox,
    request_json: &str,
    files: Files,
) -> Result<String, ServiceError> {
    let mut request: Value = serde_json::from_str(request_json)
        .map_err(|e| ServiceError::generic(format!("malformed request: {e}")))?;
    let request_id = request
        .get("requestId")
        .and_then(Value::as_u64)
        .ok_or_else(|| ServiceError::generic("malformed request: no requestId"))?;
    export_fields(
        sandbox,
        &mut request,
        request_id,
        files.fields,
        files.remote,
    )
    .await?;
    Ok(request.to_string())
}

/// `message`'s paths, as `export_paths` makes them, for a message no result
/// answers -- a command or a reply: what it copies out is held for the rest of
/// the session (`host_files::hold_export`'s request 0).
pub async fn export_paths_for_session(
    sandbox: &Sandbox,
    message: &mut Value,
    fields: &[&[Step]],
    remote: Remote,
) -> Result<(), ServiceError> {
    export_fields(sandbox, message, 0, fields, remote).await
}

/// A menu share's reply with its images exported for the session. An image
/// that is not one of content's files is dropped rather than failing the
/// reply: the host shares with its own default.
pub async fn export_menu_share(sandbox: &Sandbox, reply: &mut Value) {
    for field in MENU_SHARE {
        if let Err(error) =
            export_paths_for_session(sandbox, reply, &[field], Remote::Allowed).await
        {
            tracing::warn!("share menu image dropped: {}", error.message);
            let Some((Step::Key(name), _)) = field.split_last() else {
                continue;
            };
            if let Some(content) = reply.get_mut("content").and_then(Value::as_object_mut) {
                content.remove(*name);
            }
        }
    }
}

async fn export_fields(
    sandbox: &Sandbox,
    request: &mut Value,
    request_id: u64,
    fields: &[&[Step]],
    remote: Remote,
) -> Result<(), ServiceError> {
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
        .map_err(ServiceError::generic)?;
    }

    let mut real: HashMap<String, String> = HashMap::with_capacity(named.len());
    for path in named {
        if remote == Remote::Allowed
            && (path.starts_with("https://") || path.starts_with("http://"))
        {
            real.insert(path.clone(), path);
            continue;
        }
        let resolved = fs::resolve_path_vfs(
            sandbox.vfs.as_deref(),
            sandbox.mount_table.as_deref(),
            &path,
            FileOp::Read,
        )
        .map_err(|e| ServiceError::generic(e.message))?;
        let host_path = match resolved {
            fs::ResolvedPath::Filesystem(file) => {
                if !std::fs::metadata(&file).is_ok_and(|meta| meta.is_file()) {
                    return Err(ServiceError::generic(format!("file not found: {path}")));
                }
                file
            }
            fs::ResolvedPath::Pack { virtual_path } => {
                let mount_table = sandbox
                    .mount_table
                    .clone()
                    .ok_or_else(|| ServiceError::generic("mount table not initialized"))?;
                // The extension is what tells the host's decoder or viewer what
                // the bytes are.
                let suffix = std::path::Path::new(&virtual_path)
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .map(|ext| format!(".{ext}"))
                    .unwrap_or_default();
                let copy = fs::materialize_pack_to_temp_async(
                    Arc::clone(&sandbox.scheduler),
                    mount_table,
                    virtual_path,
                    suffix,
                )
                .await
                .map_err(|e| ServiceError::generic(e.message))?;
                if !host_files::hold_export(sandbox.host_id, request_id, PathBuf::from(&copy)) {
                    let _ = std::fs::remove_file(&copy);
                    return Err(ServiceError::generic("the session has ended"));
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
        .map_err(ServiceError::generic)?;
    }
    Ok(())
}

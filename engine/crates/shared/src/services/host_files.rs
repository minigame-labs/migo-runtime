//! Files that cross between content and the host.
//!
//! Content names files by sandbox path (`/tmp/a.png`, `/user/save.png`, a path in
//! its package); a host names them by real path. Neither may see the other's
//! kind of name.
//!
//! **Inbound** -- a result that names files the host produced (a picked photo, a
//! compressed image). The host *hands them over* when it completes the call: the
//! result is delivered through [`deliver_result`], which moves each named file into
//! the session's `/tmp` and rewrites the field to the sandbox path before content
//! sees it. A host path never reaches JavaScript, so content cannot learn where the
//! host keeps anything, and what it receives is a path every file API resolves.
//! The move is a rename when the file is on the same volume as the session's
//! temporary directory, so a host that stages its files beside the engine's cache
//! pays nothing for the handover; otherwise -- another volume, a symbolic link, a
//! file with other hard links -- the bytes are copied and the host's name removed.
//! Either way the host must name only files it owns: a file dialog's answer is the
//! user's original, and a host copies that first.
//!
//! **Outbound** -- a request that names content's files (the image to save, the one
//! to compress). The runtime resolves each path through the sandbox before the
//! request leaves, so a host is only ever asked to read a file the game itself can
//! read. A path inside a package has no real file; it is copied out, the copy is
//! held here by [`hold_export`] under the request it was made for, and removed when
//! that request's result is delivered or the session ends.
//!
//! Which result fields name files is part of each call's contract
//! (`contracts/runtime/host-services.json`, `files`); [`HostFiles`] is that list.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::Mutex;
use serde_json::Value;

/// One step along the path to a result field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// The member of an object with this name. A missing member names no file.
    Key(&'static str),
    /// Every element of an array.
    Each,
}

/// The fields of one call's result that name files the host hands over.
#[derive(Debug, PartialEq, Eq)]
pub struct HostFiles(pub &'static [&'static [Step]]);

impl HostFiles {
    /// A result that names no files.
    pub const NONE: Self = Self(&[]);

    /// `chooseImage`: `tempFilePaths` and `tempFiles[].path` name the same files.
    pub const CHOOSE_IMAGE: Self = Self(&[
        &[Step::Key("tempFilePaths"), Step::Each],
        &[Step::Key("tempFiles"), Step::Each, Step::Key("path")],
    ]);

    /// `chooseMessageFile`.
    pub const CHOOSE_MESSAGE_FILE: Self =
        Self(&[&[Step::Key("tempFiles"), Step::Each, Step::Key("path")]]);

    /// `chooseMedia`: each item's file, and a video's cover.
    pub const CHOOSE_MEDIA: Self = Self(&[
        &[
            Step::Key("tempFiles"),
            Step::Each,
            Step::Key("tempFilePath"),
        ],
        &[
            Step::Key("tempFiles"),
            Step::Each,
            Step::Key("thumbTempFilePath"),
        ],
    ]);

    /// `compressImage`.
    pub const COMPRESS_IMAGE: Self = Self(&[&[Step::Key("tempFilePath")]]);

    /// The fields as the contract spells them: `tempFiles[].path`.
    pub fn describe(&self) -> Vec<String> {
        self.0
            .iter()
            .map(|field| {
                let mut out = String::new();
                for step in *field {
                    match step {
                        Step::Key(key) => {
                            if !out.is_empty() {
                                out.push('.');
                            }
                            out.push_str(key);
                        }
                        Step::Each => out.push_str("[]"),
                    }
                }
                out
            })
            .collect()
    }
}

/// What this module keeps for one session.
struct SessionFiles {
    /// The real directory behind the session's `/tmp`.
    temp_dir: PathBuf,
    /// Copies made for requests still waiting on the host, by request id.
    exports: HashMap<u64, Vec<PathBuf>>,
}

/// Per session, keyed by host id: a result can arrive on any host thread, and
/// one session's request number must not reach another's files.
static SESSIONS: LazyLock<Mutex<HashMap<i32, SessionFiles>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Names handed out for imported files. Process-wide, so two sessions of one game
/// -- which have separate `/tmp` directories anyway -- never need to coordinate.
static NEXT_IMPORT: AtomicU64 = AtomicU64::new(1);

/// Make `temp_dir` the session's `/tmp` for the files hosts hand over.
pub fn register_session_temp(host_id: i32, temp_dir: PathBuf) {
    let previous = SESSIONS.lock().insert(
        host_id,
        SessionFiles {
            temp_dir,
            exports: HashMap::new(),
        },
    );
    if let Some(previous) = previous {
        remove_exports(previous.exports.into_values().flatten());
    }
}

/// Forget the session at teardown, removing every copy still held for it.
///
/// Its `/tmp` -- where imported files live -- is removed by its owner.
pub fn unregister_session(host_id: i32) {
    let removed = SESSIONS.lock().remove(&host_id);
    if let Some(session) = removed {
        remove_exports(session.exports.into_values().flatten());
    }
}

/// Hold `path`, a copy made for request `request_id`, until its result arrives.
/// Request 0, which no request carries, holds it for the rest of the session --
/// for a copy a command names, which no result releases.
///
/// Returns `false` when the session is gone or was never registered; the caller
/// then removes the copy itself, since nothing will.
#[must_use]
pub fn hold_export(host_id: i32, request_id: u64, path: PathBuf) -> bool {
    let mut sessions = SESSIONS.lock();
    let Some(session) = sessions.get_mut(&host_id) else {
        return false;
    };
    session.exports.entry(request_id).or_default().push(path);
    true
}

fn remove_exports(paths: impl IntoIterator<Item = PathBuf>) {
    for path in paths {
        if let Err(error) = std::fs::remove_file(&path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!("host file export {} not removed: {error}", path.display());
        }
    }
}

/// The result a host completed a call with, as content may see it.
///
/// Releases the copies held for the request, then takes over every file `files`
/// names: each is moved into the session's `/tmp` and its field rewritten to the
/// sandbox path. A failure to take one fails the result -- content is never told
/// about a file it cannot open -- and a result that is a failure, or names no
/// request, has those fields removed instead, since nothing will consume them.
pub fn deliver_result(host_id: i32, files: &HostFiles, result_json: &str) -> String {
    let Ok(Value::Object(mut fields)) = serde_json::from_str::<Value>(result_json) else {
        // Not a result document at all: nothing in it can be correlated or
        // trusted, and it may still spell a host path.
        return r#"{"error":"unreadable host result"}"#.to_string();
    };
    let request_id = fields.get("requestId").and_then(Value::as_u64);

    let (temp_dir, exports) = {
        let mut sessions = SESSIONS.lock();
        match sessions.get_mut(&host_id) {
            Some(session) => (
                Some(session.temp_dir.clone()),
                request_id.and_then(|id| session.exports.remove(&id)),
            ),
            None => (None, None),
        }
    };
    if let Some(exports) = exports {
        remove_exports(exports);
    }

    let failed = fields.get("error").is_some_and(|error| !error.is_null());
    let mut result = Value::Object(std::mem::take(&mut fields));
    if failed || request_id.is_none() || files.0.is_empty() {
        strip(&mut result, files);
        return result.to_string();
    }
    let Some(temp_dir) = temp_dir else {
        return failure(request_id, "the session has no temporary directory");
    };

    let mut imported: HashMap<String, String> = HashMap::new();
    let mut created: Vec<PathBuf> = Vec::new();
    let outcome = files.0.iter().try_for_each(|field| {
        visit_fields(&mut result, field, &mut |value| {
            let Value::String(host_path) = value else {
                return Err("a file field is not a string".to_string());
            };
            if host_path.is_empty() {
                return Ok(());
            }
            // `chooseImage` names each file twice; the second mention must find
            // the file where the first one put it.
            let sandbox_path = match imported.get(host_path.as_str()) {
                Some(done) => done.clone(),
                None => {
                    let (real, name) = take_file(Path::new(host_path.as_str()), &temp_dir)?;
                    created.push(real);
                    let sandbox_path = format!("/tmp/{name}");
                    imported.insert(host_path.clone(), sandbox_path.clone());
                    sandbox_path
                }
            };
            *value = Value::String(sandbox_path);
            Ok(())
        })
    });
    match outcome {
        Ok(()) => result.to_string(),
        Err(reason) => {
            remove_exports(created);
            failure(request_id, &reason)
        }
    }
}

fn failure(request_id: Option<u64>, reason: &str) -> String {
    let mut out = serde_json::Map::new();
    if let Some(id) = request_id {
        out.insert("requestId".into(), Value::from(id));
    }
    out.insert("error".into(), Value::from(reason));
    Value::Object(out).to_string()
}

/// Remove every field `files` names, wherever it sits.
fn strip(result: &mut Value, files: &HostFiles) {
    for field in files.0 {
        let Some((last, parents)) = field.split_last() else {
            continue;
        };
        let _ = visit_fields(result, parents, &mut |parent| {
            match (last, parent) {
                (Step::Key(key), Value::Object(object)) => {
                    object.remove(*key);
                }
                (Step::Each, Value::Array(items)) => items.clear(),
                _ => {}
            }
            Ok(())
        });
    }
}

/// Call `f` on every value `steps` reaches. A missing member reaches nothing; a
/// member that is there but has the wrong shape is malformed.
pub fn visit_fields(
    value: &mut Value,
    steps: &[Step],
    f: &mut dyn FnMut(&mut Value) -> Result<(), String>,
) -> Result<(), String> {
    let Some((step, rest)) = steps.split_first() else {
        return f(value);
    };
    match step {
        Step::Key(key) => match value {
            Value::Object(object) => match object.get_mut(*key) {
                Some(child) => visit_fields(child, rest, f),
                None => Ok(()),
            },
            _ => Err(format!("malformed: expected an object holding `{key}`")),
        },
        Step::Each => match value {
            Value::Array(items) => items
                .iter_mut()
                .try_for_each(|item| visit_fields(item, rest, f)),
            _ => Err("malformed: expected an array".to_string()),
        },
    }
}

/// Move the host's file into `temp_dir` under a fresh name; returns the new real
/// path and the name.
fn take_file(source: &Path, temp_dir: &Path) -> Result<(PathBuf, String), String> {
    let shown = source.display();
    if !source.is_absolute() {
        return Err(format!("{shown} is not an absolute path"));
    }
    let link = std::fs::symlink_metadata(source).map_err(|e| format!("{shown}: {e}"))?;
    // A rename would move a symbolic link -- one naming anything at all -- into
    // the sandbox, and would share an inode the host still reaches by another
    // name. Both are copied instead.
    let movable = link.file_type().is_file() && sole_link(&link);
    if !movable {
        let target = std::fs::metadata(source).map_err(|e| format!("{shown}: {e}"))?;
        if !target.is_file() {
            return Err(format!("{shown} is not a regular file"));
        }
    }

    let (dest, name) = reserve(temp_dir, extension(source))?;
    let moved = movable && std::fs::rename(source, &dest).is_ok();
    if !moved {
        if let Err(error) = std::fs::copy(source, &dest) {
            let _ = std::fs::remove_file(&dest);
            return Err(format!("{shown}: {error}"));
        }
        if let Err(error) = std::fs::remove_file(source) {
            tracing::warn!("host file {shown} copied but not removed: {error}");
        }
    }
    make_owner_writable(&dest);
    Ok((dest, name))
}

/// Claim a name no other file in `dir` has.
fn reserve(dir: &Path, extension: Option<String>) -> Result<(PathBuf, String), String> {
    loop {
        let n = NEXT_IMPORT.fetch_add(1, Ordering::Relaxed);
        let name = match &extension {
            Some(ext) => format!("host-{n}.{ext}"),
            None => format!("host-{n}"),
        };
        let path = dir.join(&name);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(_) => return Ok((path, name)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(format!("{}: {error}", dir.display())),
        }
    }
}

/// The source's extension, if it is a short alphanumeric one: it is what tells
/// an image decoder and a host's viewer what the bytes are.
fn extension(source: &Path) -> Option<String> {
    let ext = source.extension()?.to_str()?;
    (!ext.is_empty() && ext.len() <= 10 && ext.bytes().all(|b| b.is_ascii_alphanumeric()))
        .then(|| ext.to_ascii_lowercase())
}

#[cfg(unix)]
fn sole_link(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    metadata.nlink() == 1
}

#[cfg(not(unix))]
fn sole_link(_metadata: &std::fs::Metadata) -> bool {
    true
}

/// Content may write what it was given, as it may any other temporary file.
fn make_owner_writable(path: &Path) {
    let Ok(metadata) = std::fs::metadata(path) else {
        return;
    };
    let mut permissions = metadata.permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = permissions.mode();
        if mode & 0o200 != 0 {
            return;
        }
        permissions.set_mode(mode | 0o200);
    }
    #[cfg(not(unix))]
    {
        if !permissions.readonly() {
            return;
        }
        permissions.set_readonly(false);
    }
    let _ = std::fs::set_permissions(path, permissions);
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "migo-host-files-{tag}-{}-{}",
                std::process::id(),
                NEXT_IMPORT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(dir.join("host")).unwrap();
            std::fs::create_dir_all(dir.join("tmp")).unwrap();
            Self(dir)
        }
        fn host_file(&self, name: &str, bytes: &[u8]) -> String {
            let path = self.0.join("host").join(name);
            std::fs::write(&path, bytes).unwrap();
            path.to_string_lossy().into_owned()
        }
        fn tmp(&self) -> PathBuf {
            self.0.join("tmp")
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn json(s: &str) -> Value {
        serde_json::from_str(s).unwrap()
    }

    #[test]
    fn a_picked_file_reaches_content_as_a_sandbox_path_and_leaves_the_host() {
        let scratch = Scratch::new("pick");
        let host = 7101;
        register_session_temp(host, scratch.tmp());
        let picked = scratch.host_file("IMG_1.JPG", b"jpeg bytes");

        let delivered = json(&deliver_result(
            host,
            &HostFiles::CHOOSE_IMAGE,
            &serde_json::json!({
                "requestId": 4,
                "tempFilePaths": [picked],
                "tempFiles": [{ "path": picked, "size": 10 }],
            })
            .to_string(),
        ));
        unregister_session(host);

        let path = delivered["tempFilePaths"][0].as_str().unwrap();
        assert!(
            path.starts_with("/tmp/host-") && path.ends_with(".jpg"),
            "{delivered}"
        );
        assert_eq!(
            delivered["tempFiles"][0]["path"], path,
            "one file, one name"
        );
        assert_eq!(delivered["tempFiles"][0]["size"], 10);
        assert_eq!(delivered["requestId"], 4);
        assert!(!delivered.to_string().contains(scratch.0.to_str().unwrap()));
        let name = path.strip_prefix("/tmp/").unwrap();
        assert_eq!(
            std::fs::read(scratch.tmp().join(name)).unwrap(),
            b"jpeg bytes"
        );
        assert!(!Path::new(&picked).exists(), "handed over, not shared");
    }

    #[test]
    fn a_failure_or_an_unaddressed_result_carries_no_host_path() {
        let scratch = Scratch::new("fail");
        let host = 7102;
        register_session_temp(host, scratch.tmp());
        let picked = scratch.host_file("a.png", b"png");

        for raw in [
            serde_json::json!({ "requestId": 1, "error": "cancel", "tempFilePath": picked }),
            serde_json::json!({ "tempFilePath": picked }),
        ] {
            let delivered = deliver_result(host, &HostFiles::COMPRESS_IMAGE, &raw.to_string());
            assert!(!delivered.contains("a.png"), "{delivered}");
        }
        assert_eq!(
            deliver_result(host, &HostFiles::COMPRESS_IMAGE, "not json /etc/passwd"),
            r#"{"error":"unreadable host result"}"#
        );
        unregister_session(host);
        assert!(
            Path::new(&picked).exists(),
            "nothing consumed it, nothing moved it"
        );
    }

    #[test]
    fn a_file_that_cannot_be_taken_fails_the_whole_result() {
        let scratch = Scratch::new("partial");
        let host = 7103;
        register_session_temp(host, scratch.tmp());
        let first = scratch.host_file("one.mp4", b"video");

        let delivered = json(&deliver_result(
            host,
            &HostFiles::CHOOSE_MEDIA,
            &serde_json::json!({
                "requestId": 9,
                "tempFiles": [
                    { "tempFilePath": first, "fileType": "video" },
                    { "tempFilePath": "relative/two.mp4", "fileType": "video" },
                ],
            })
            .to_string(),
        ));
        unregister_session(host);

        assert_eq!(delivered["requestId"], 9);
        assert!(
            delivered["error"]
                .as_str()
                .unwrap()
                .contains("not an absolute path")
        );
        assert_eq!(std::fs::read_dir(scratch.tmp()).unwrap().count(), 0);
    }

    #[test]
    fn a_symbolic_link_is_copied_never_moved_into_the_sandbox() {
        #[cfg(unix)]
        {
            let scratch = Scratch::new("link");
            let host = 7104;
            register_session_temp(host, scratch.tmp());
            let target = scratch.host_file("target.png", b"real");
            let link = scratch.0.join("host").join("link.png");
            std::os::unix::fs::symlink(&target, &link).unwrap();

            let delivered = json(&deliver_result(
                host,
                &HostFiles::COMPRESS_IMAGE,
                &serde_json::json!({ "requestId": 2, "tempFilePath": link }).to_string(),
            ));
            unregister_session(host);

            let name = delivered["tempFilePath"]
                .as_str()
                .unwrap()
                .strip_prefix("/tmp/")
                .unwrap();
            let imported = scratch.tmp().join(name);
            assert!(
                std::fs::symlink_metadata(&imported)
                    .unwrap()
                    .file_type()
                    .is_file()
            );
            assert_eq!(std::fs::read(imported).unwrap(), b"real");
            assert!(
                Path::new(&target).exists(),
                "the link's target is not the host's to give"
            );
        }
    }

    #[test]
    fn copies_made_for_a_request_go_when_its_result_arrives_or_the_session_ends() {
        let scratch = Scratch::new("export");
        let host = 7105;
        register_session_temp(host, scratch.tmp());
        let settled = PathBuf::from(scratch.host_file("export-1.png", b"x"));
        let abandoned = PathBuf::from(scratch.host_file("export-2.png", b"y"));
        assert!(hold_export(host, 5, settled.clone()));
        assert!(hold_export(host, 6, abandoned.clone()));

        deliver_result(host, &HostFiles::NONE, r#"{"requestId":5}"#);
        assert!(!settled.exists());
        assert!(abandoned.exists());

        unregister_session(host);
        assert!(!abandoned.exists());
        assert!(
            !hold_export(host, 7, settled),
            "a gone session holds nothing"
        );
    }

    #[test]
    fn the_contract_spelling_of_each_field() {
        assert_eq!(
            HostFiles::CHOOSE_IMAGE.describe(),
            ["tempFilePaths[]", "tempFiles[].path"]
        );
        assert_eq!(HostFiles::COMPRESS_IMAGE.describe(), ["tempFilePath"]);
        assert!(HostFiles::NONE.describe().is_empty());
    }
}

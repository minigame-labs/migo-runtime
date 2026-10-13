//! `setCursor(path, x, y)` as the window service is asked it: a CSS cursor
//! keyword by name, or an image the game can read made into a real file the
//! host can open. Both executions shape the request here -- the embedded op,
//! and the external session's host answering a producer in another process --
//! so a game names the same cursors on both.

use std::collections::HashMap;
use std::path::PathBuf;

use parking_lot::Mutex;
use serde_json::json;
use shared::services::host_files;
use shared::vfs::FileOp;

use crate::fs;
use crate::host_paths::Sandbox;

/// The CSS cursor keywords `setCursor` passes through by name; anything else is
/// a path.
pub const KEYWORDS: &[&str] = &[
    "default",
    "auto",
    "none",
    "context-menu",
    "help",
    "pointer",
    "progress",
    "wait",
    "cell",
    "crosshair",
    "text",
    "vertical-text",
    "alias",
    "copy",
    "move",
    "no-drop",
    "not-allowed",
    "grab",
    "grabbing",
    "all-scroll",
    "col-resize",
    "row-resize",
    "n-resize",
    "e-resize",
    "s-resize",
    "w-resize",
    "ne-resize",
    "nw-resize",
    "se-resize",
    "sw-resize",
    "ew-resize",
    "ns-resize",
    "nesw-resize",
    "nwse-resize",
    "zoom-in",
    "zoom-out",
];

/// The copies made of package entries named as cursors, one per entry for the
/// session: a game switching between a few cursors names the same few files,
/// and a fresh copy for every switch would grow without bound. One per
/// session, held by whatever answers its `setCursor`.
#[derive(Default)]
pub struct CursorCopies(Mutex<HashMap<String, String>>);

/// The window service's `set_cursor` request for `setCursor(path, x, y)` --
/// `{"keyword"}`, or `{"path", "x", "y"}` with a real file -- or `None` when
/// `path` is neither a keyword nor a file the game can read.
pub fn request(
    sandbox: &Sandbox,
    copies: &CursorCopies,
    path: &str,
    x: f64,
    y: f64,
) -> Option<String> {
    if KEYWORDS.contains(&path) {
        return Some(json!({ "keyword": path }).to_string());
    }
    let file = file(sandbox, copies, path)?;
    let finite = |value: f64| if value.is_finite() { value } else { 0.0 };
    Some(json!({ "path": file, "x": finite(x), "y": finite(y) }).to_string())
}

/// The real file a cursor path names, or `None` when it names none the game can
/// read.
fn file(sandbox: &Sandbox, copies: &CursorCopies, path: &str) -> Option<String> {
    match fs::resolve_path_vfs(
        sandbox.vfs.as_deref(),
        sandbox.mount_table.as_deref(),
        path,
        FileOp::Read,
    )
    .ok()?
    {
        fs::ResolvedPath::Filesystem(file) => std::fs::metadata(&file)
            .is_ok_and(|meta| meta.is_file())
            .then_some(file),
        fs::ResolvedPath::Pack { virtual_path } => {
            // Held across the copy: two calls naming one entry make one copy.
            let mut copies = copies.0.lock();
            if let Some(copy) = copies.get(&virtual_path) {
                return Some(copy.clone());
            }
            let suffix = std::path::Path::new(&virtual_path)
                .extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| format!(".{ext}"))
                .unwrap_or_default();
            // A cursor is a few kilobytes: copied here, in the call that names it.
            let copy = fs::materialize_pack_to_temp_checked(
                &sandbox.scheduler,
                sandbox.mount_table.as_deref(),
                &virtual_path,
                &suffix,
            )
            .ok()?;
            if !host_files::hold_export(sandbox.host_id, 0, PathBuf::from(&copy)) {
                let _ = std::fs::remove_file(&copy);
                return None;
            }
            copies.insert(virtual_path, copy.clone());
            Some(copy)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use migo_io::scheduler::IoScheduler;
    use shared::services::host_files;
    use shared::vfs::{MountBackend, MountTable};

    use super::*;

    /// A package whose one entry, `aim.cur`, has no file of its own -- a
    /// pack-backed `/code` -- counting the copies made of it.
    #[derive(Debug)]
    struct Package {
        copies: Arc<AtomicUsize>,
    }

    impl MountBackend for Package {
        fn read(&self, _relative_path: &str) -> io::Result<Vec<u8>> {
            Ok(b"cur".to_vec())
        }
        fn exists(&self, relative_path: &str) -> bool {
            relative_path == "aim.cur"
        }
        fn real_path(&self, _relative_path: &str) -> Option<PathBuf> {
            None
        }
        fn root_dir(&self) -> Option<&Path> {
            None
        }
        fn copy_to_writer(
            &self,
            _relative_path: &str,
            writer: &mut dyn io::Write,
        ) -> io::Result<()> {
            self.copies.fetch_add(1, Ordering::SeqCst);
            writer.write_all(b"cur")
        }
        fn is_file(&self, relative_path: &str) -> bool {
            relative_path == "aim.cur"
        }
    }

    /// A keyword is passed by name; a package entry is copied out once for the
    /// session however often it is named, its hotspot made finite; a path that
    /// is not the game's file is no request; and the copy goes with the session.
    #[test]
    fn a_keyword_passes_by_name_and_a_package_entry_is_copied_once_for_the_session() {
        let dir = std::env::temp_dir().join(format!("migo_services_cursor_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("base")).unwrap();
        let host_id = 9_301;
        host_files::register_session_temp(host_id, dir.join("tmp"));
        let mount_table = Arc::new(MountTable::new(dir.join("base")));
        let copies = Arc::new(AtomicUsize::new(0));
        assert!(mount_table.mount_overlay(
            "package".to_string(),
            String::new(),
            Arc::new(Package {
                copies: Arc::clone(&copies)
            }),
        ));
        let sandbox = Sandbox {
            host_id,
            scheduler: Arc::new(IoScheduler::new(host_id)),
            vfs: None,
            mount_table: Some(mount_table),
        };
        let cursors = CursorCopies::default();

        assert_eq!(
            request(&sandbox, &cursors, "pointer", 1.0, 2.0).as_deref(),
            Some(r#"{"keyword":"pointer"}"#)
        );
        let first: serde_json::Value =
            serde_json::from_str(&request(&sandbox, &cursors, "aim.cur", 4.0, f64::NAN).unwrap())
                .unwrap();
        let file = first["path"].as_str().expect("a real path").to_owned();
        assert_eq!(std::fs::read(&file).unwrap(), b"cur");
        assert_eq!(
            (first["x"].as_f64(), first["y"].as_f64()),
            (Some(4.0), Some(0.0))
        );
        let again: serde_json::Value =
            serde_json::from_str(&request(&sandbox, &cursors, "/code/aim.cur", 0.0, 0.0).unwrap())
                .unwrap();
        assert_eq!(again["path"].as_str(), Some(file.as_str()), "the same copy");
        assert_eq!(copies.load(Ordering::SeqCst), 1, "copied once");

        assert_eq!(request(&sandbox, &cursors, "missing.cur", 0.0, 0.0), None);
        assert_eq!(request(&sandbox, &cursors, "/etc/hosts", 0.0, 0.0), None);

        host_files::unregister_session(host_id);
        assert!(
            !Path::new(&file).exists(),
            "the session's end removes the copy"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

//! What `require()` reads: a CommonJS module of the game's package, found the
//! way Node finds one and read as text.
//!
//! The engine's `require` shim (`01_amdshim.js`) evaluates the module itself;
//! this is the part that touches the package -- resolving the specifier against
//! the requiring module's directory, honouring the mount table's overlays, and
//! refusing a path that leaves `/code`. Moved here from the embedded runtime's
//! op so the external session resolves exactly the same file for the same
//! `require`.

use std::path::{Path, PathBuf};

use shared::vfs::MountTable;

use crate::error::ServiceError;

/// A module found and read: its source, the path it is known by (the cache
/// key and the `sourceURL`), and the directory its own `require`s resolve
/// against.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RequiredModule {
    pub code: String,
    pub abs_path: String,
    pub dir: String,
}

fn fail(message: String) -> ServiceError {
    // The embedded op's error is a plain `Error`.
    ServiceError::generic(message)
}

/// Check whether `specifier` looks like an absolute filesystem path.
///
/// Matches Unix absolute paths (`/foo`) and Windows drive-letter paths (`C:\foo`).
#[inline]
fn is_absolute_path(specifier: &str) -> bool {
    specifier.starts_with('/')
        || (specifier.len() >= 3
            && specifier.as_bytes()[0].is_ascii_alphabetic()
            && specifier.as_bytes()[1] == b':'
            && matches!(specifier.as_bytes()[2], b'/' | b'\\'))
}

/// Resolve `path` using the Node.js-style extension/index resolution order:
///
/// 1. exact path
/// 2. path.js
/// 3. path.json
/// 4. path/index.js
///
/// Checks both filesystem AND MountTable (for pack-backed overlays where
/// files don't exist on disk but are accessible via the mount).
fn resolve_module_path(path: PathBuf, mount_table: Option<&MountTable>, code_dir: &str) -> PathBuf {
    let code_path = Path::new(code_dir);

    // Helper: check if a candidate path exists on filesystem OR in mount table.
    let exists = |p: &Path| -> bool {
        if p.is_file() {
            return true;
        }
        // Check mount table for pack-backed entries.
        if let Some(mt) = mount_table {
            if let Ok(rel) = p.strip_prefix(code_path) {
                if let Some(rel_str) = rel.to_str() {
                    return mt.is_file(rel_str);
                }
            }
        }
        false
    };

    if exists(&path) {
        return path;
    }

    // Try appending .js
    if !path.extension().is_some_and(|e| e == "js" || e == "json") {
        let with_js = path.with_extension("js");
        if exists(&with_js) {
            return with_js;
        }
        let with_json = path.with_extension("json");
        if exists(&with_json) {
            return with_json;
        }
    }

    // Try path/index.js (directory as module)
    let index_js = path.join("index.js");
    if exists(&index_js) {
        return index_js;
    }

    // Fall back to original (will produce a clear "not found" error)
    path
}

/// Find and read the module `specifier` names, relative to `referrer_dir` --
/// or to `code_dir`, the package root, when the referrer is the entry.
pub fn resolve_and_read(
    mount_table: Option<&MountTable>,
    code_dir: &str,
    specifier: &str,
    referrer_dir: &str,
) -> Result<RequiredModule, ServiceError> {
    let base_dir = if referrer_dir.is_empty() {
        code_dir
    } else {
        referrer_dir
    };

    // Reject absolute paths — they must go through /code resolution.
    if is_absolute_path(specifier) {
        return Err(fail(format!(
            "require: absolute path not allowed: {specifier}"
        )));
    }

    let raw_path = PathBuf::from(base_dir).join(specifier);
    let resolved = resolve_module_path(raw_path, mount_table, code_dir);

    // Compute a normalized relative path for the canonical module key.
    // This ensures ./foo and ./a/../foo produce the same cache key.
    let code_path = Path::new(code_dir);
    let normalized_relative = resolved
        .strip_prefix(code_path)
        .ok()
        .and_then(|r| r.to_str())
        .map(|s| {
            // Normalize .. and . textually.
            let mut parts: Vec<&str> = Vec::new();
            for c in s.split('/') {
                match c {
                    "" | "." => {}
                    ".." => {
                        parts.pop();
                    }
                    c => parts.push(c),
                }
            }
            parts.join("/")
        });

    if let (Some(mt), Some(rel)) = (mount_table, &normalized_relative) {
        // Use resolve() as the single source of truth for overlay shadow semantics.
        // resolve() returns:
        //   Some(real_path=Some) → file on disk (dir-backed overlay or base)
        //   Some(real_path=None) → file in pack-backed overlay
        //   None + overlay matches → shadow: file missing in overlay, don't fall to base
        //   None + no overlay → path not in any overlay, may fall to base filesystem
        let resolved_info = mt.resolve(rel);
        let overlay_claims_subtree = mt.has_overlay_for(rel);

        match &resolved_info {
            Some(info) => {
                // MountTable found the file. Read it.
                return match mt.read(rel) {
                    Ok(bytes) => {
                        let content = String::from_utf8(bytes)
                            .map_err(|e| fail(format!("require: not UTF-8: {e}")))?;
                        let is_pack = info.real_path.is_none();
                        let abs_path = if is_pack {
                            // Per-source mounted_at: only changes when THIS source
                            // is replaced, not when other overlays change.
                            format!(
                                "{}#s{}",
                                code_path.join(rel).display(),
                                info.source_mounted_at
                            )
                        } else {
                            code_path.join(rel).to_string_lossy().into_owned()
                        };
                        let parent = code_path
                            .join(rel)
                            .parent()
                            .map(|p| p.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        Ok(RequiredModule {
                            code: content,
                            abs_path,
                            dir: parent,
                        })
                    }
                    Err(e) => Err(fail(format!(
                        "require: resolved but read failed: {rel}: {e}"
                    ))),
                };
            }
            None if overlay_claims_subtree => {
                // An overlay covers this subtree but the file doesn't exist in it.
                // Shadow: do NOT fall through to base.
                return Err(fail(format!(
                    "require: module not found (shadowed by overlay): {rel}"
                )));
            }
            None => {
                // No overlay covers this path. Fall through to base filesystem.
            }
        }
    }

    // Fallback: base filesystem read (only for paths NOT shadowed by an overlay).
    let path = std::fs::canonicalize(&resolved).unwrap_or(resolved);

    // Sandbox: reject paths outside code_dir.
    if !code_dir.is_empty() && !path.starts_with(code_path) {
        return Err(fail(format!(
            "require: path escapes /code sandbox: {}",
            path.display()
        )));
    }

    let abs_path = path.to_string_lossy().into_owned();
    let content = std::fs::read_to_string(&path)
        .map_err(|e| fail(format!("require: cannot read {}: {}", abs_path, e)))?;
    let parent = path
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();

    Ok(RequiredModule {
        code: content,
        abs_path,
        dir: parent,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn package(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "migo-services-require-{label}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("js/lib")).unwrap();
        std::fs::write(dir.join("game.js"), "require('./js/main');").unwrap();
        std::fs::write(dir.join("js/main.js"), "module.exports = 1;").unwrap();
        std::fs::write(dir.join("js/lib/index.js"), "module.exports = 2;").unwrap();
        std::fs::write(dir.join("js/data.json"), "{\"a\":1}").unwrap();
        // Canonical, because the base-filesystem path is canonicalized before
        // it is compared with the package root.
        std::fs::canonicalize(&dir).unwrap()
    }

    #[test]
    fn a_specifier_resolves_as_node_resolves_it() {
        let root = package("node");
        let code = root.to_str().unwrap();
        let main = resolve_and_read(None, code, "./js/main", "").unwrap();
        assert_eq!(main.code, "module.exports = 1;");
        assert_eq!(main.dir, root.join("js").to_string_lossy());
        let lib = resolve_and_read(None, code, "./lib", &main.dir).unwrap();
        assert_eq!(lib.code, "module.exports = 2;");
        let data = resolve_and_read(None, code, "./data", &main.dir).unwrap();
        assert_eq!(data.code, "{\"a\":1}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_path_out_of_the_package_is_refused() {
        let root = package("escape");
        let code = root.to_str().unwrap();
        std::fs::write(root.parent().unwrap().join("outside.js"), "1").ok();
        let error = resolve_and_read(None, code, "../outside.js", "").unwrap_err();
        assert!(error.message.contains("escapes /code sandbox"), "{error}");
        let absolute = resolve_and_read(None, code, "/etc/hosts", "").unwrap_err();
        assert!(absolute.message.contains("absolute path not allowed"));
        assert_eq!(absolute.class, crate::error::CLASS_ERROR);
        let _ = std::fs::remove_dir_all(&root);
    }
}

//! What a game asks about its subpackages.
//!
//! A mini-game ships as a base package and any number of subpackages it loads
//! when it reaches them. Before loading one, content asks whether it is already
//! there -- mounted in this session, or installed durably from a previous run --
//! and what the mounted copy is, so a cache keyed on it is invalidated when the
//! subpackage is replaced and not before.
//!
//! All of it is reading the session's mount table and the game's package store,
//! so it lives here and both executions call it: the embedded ops as adapters
//! over the runtime's state, the external session's dispatcher over the content
//! it mounted.

use std::path::Path;

use shared::vfs::{GamePaths, MountTable};

/// The entry points a subpackage root may hold, in the order they are looked
/// for. A root that is a directory with anything in it counts too, which is
/// what a subpackage of assets with no code is.
const ENTRY_POINTS: [&str; 3] = ["game.js", "index.js", "main.js"];

/// The subpackages a session was started with, as the JSON content reads.
pub fn sub_packages_json(sub_packages: &[(String, String)]) -> String {
    if sub_packages.is_empty() {
        return "[]".to_string();
    }
    let entries: Vec<serde_json::Value> = sub_packages
        .iter()
        .map(|(name, root)| serde_json::json!({ "name": name, "root": root }))
        .collect();
    serde_json::to_string(&entries).unwrap_or_else(|_| "[]".to_string())
}

/// Whether the subpackage at `root` is reachable in this session.
///
/// True when its content is visible through the mount table, whether that is an
/// installed overlay or files in the base package -- which is what lets content
/// skip a download for a subpackage that shipped with the game.
pub fn is_installed(mount_table: Option<&MountTable>, root: &str) -> bool {
    let Some(mount_table) = mount_table else {
        return false;
    };
    for entry in ENTRY_POINTS {
        let candidate = format!("{root}/{entry}");
        if mount_table.exists(&candidate) || mount_table.exists_or_is_dir(&candidate) {
            return true;
        }
    }
    !mount_table.list_dir(root).is_empty()
}

/// Whether the subpackage is durably installed in this game's package store.
///
/// Both halves must match -- the key derived from the name and the prefix the
/// entry claims -- and the package file must be there, so a stale or mismatched
/// manifest entry does not report a subpackage that cannot be mounted.
pub fn is_persisted(game_paths: Option<&GamePaths>, name: &str, root: &str) -> bool {
    let Some(game_paths) = game_paths else {
        return false;
    };
    let Ok(key) = package_key(name) else {
        return false;
    };
    let store = shared::vfs::mount::package_store_dir(game_paths.cache_dir());
    let manifest = shared::vfs::mount::PackageManifest::load(&store);
    manifest
        .packages
        .get(&key)
        .is_some_and(|entry| entry.prefix == root && store.join(format!("{key}.mpkg")).exists())
}

/// What is covering `root`: a token that changes when that subpackage is
/// replaced and not when anything else is, or empty when the base package
/// serves it.
pub fn identity(mount_table: Option<&MountTable>, root: &str) -> String {
    mount_table.map_or_else(String::new, |table| table.overlay_identity_for(root))
}

/// The mount table's generation, which content compares to know that what it
/// resolved earlier still resolves the same way.
pub fn mount_generation(mount_table: Option<&MountTable>) -> u64 {
    mount_table.map_or(0, MountTable::generation)
}

/// A file-system-safe key for a package name, collision-free because every
/// byte outside `[a-zA-Z0-9._-]` is percent-encoded rather than dropped.
///
/// The same key an install writes, which is why a query can find what an
/// install left.
pub fn package_key(name: &str) -> Result<String, String> {
    let trimmed = name.trim_matches('/');
    if trimmed.is_empty() || trimmed.len() > 256 {
        return Err(format!("invalid name: empty or too long ({})", name.len()));
    }
    if trimmed.contains("..") || trimmed.bytes().any(|byte| byte < 0x20) {
        return Err(format!("invalid characters in name: {name}"));
    }
    let mut key = String::with_capacity(trimmed.len());
    for byte in trimmed.bytes() {
        match byte {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'.' | b'-' | b'_' => key.push(byte as char),
            _ => key.push_str(&format!("%{byte:02X}")),
        }
    }
    Ok(key)
}

/// Where the package store for a game's cache directory is.
pub fn store_dir(cache_dir: &Path) -> std::path::PathBuf {
    shared::vfs::mount::package_store_dir(cache_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_subpackage_list_is_json_content_can_read() {
        assert_eq!(sub_packages_json(&[]), "[]");
        assert_eq!(
            sub_packages_json(&[("stage1".to_string(), "sub/stage1".to_string())]),
            r#"[{"name":"stage1","root":"sub/stage1"}]"#
        );
    }

    /// The key is the file system's, so every byte a path cannot carry is
    /// encoded rather than dropped: two names that differ only there must not
    /// become one package.
    #[test]
    fn a_package_key_is_collision_free() {
        assert_eq!(package_key("stage1").unwrap(), "stage1");
        assert_eq!(package_key("/stage 1/").unwrap(), "stage%201");
        assert_ne!(
            package_key("a/b").unwrap(),
            package_key("a_b").unwrap(),
            "a separator and an underscore are different names"
        );
        assert_eq!(package_key("é").unwrap(), "%C3%A9");
    }

    /// A name that could leave the store is refused rather than encoded.
    #[test]
    fn a_name_that_could_escape_the_store_is_refused() {
        assert!(package_key("").is_err());
        assert!(package_key("//").is_err());
        assert!(package_key("../secrets").is_err());
        assert!(package_key("a\u{1}b").is_err());
        assert!(package_key(&"x".repeat(257)).is_err());
        assert!(package_key(&"x".repeat(256)).is_ok());
    }

    /// With nothing mounted, every question answers no rather than failing:
    /// content asks these before it has loaded anything.
    #[test]
    fn nothing_mounted_answers_nothing_installed() {
        assert!(!is_installed(None, "sub/stage1"));
        assert!(!is_persisted(None, "stage1", "sub/stage1"));
        assert_eq!(identity(None, "sub/stage1"), "");
        assert_eq!(mount_generation(None), 0);
    }
}

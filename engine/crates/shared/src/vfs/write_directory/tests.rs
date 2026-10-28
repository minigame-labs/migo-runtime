use super::*;
use crate::vfs::{FilePermissions, VirtualFS};
use std::{
    fs,
    io::Write,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "migo-write-directory-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&dir).unwrap();
        for child in ["cache", "code", "user", "tmp", "outside"] {
            fs::create_dir(dir.join(child)).unwrap();
        }
        Self(dir)
    }
    fn vfs(&self) -> VirtualFS {
        VirtualFS::new(
            self.0.join("code"),
            self.0.join("user"),
            self.0.join("cache"),
            self.0.join("tmp"),
        )
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn child_names_reject_traversal_aliases_and_absolute_paths_before_native_io() {
    let root = Scratch::new();
    let directory = root.vfs().write_directory("/cache/buffers", true).unwrap();
    fs::write(root.0.join("cache/victim"), b"outside the capability").unwrap();
    let absolute = root.0.join("cache/victim").to_string_lossy().into_owned();
    for name in [
        "",
        ".",
        "..",
        "../victim",
        "nested/file",
        "./victim",
        "victim/",
        "victim//",
        "con",
        "file:stream",
        "a\\b",
        "a\0b",
        "a\u{0085}b",
        absolute.as_str(),
    ] {
        assert_eq!(
            directory.create_new(name).unwrap_err().kind(),
            io::ErrorKind::InvalidInput,
            "{name:?}"
        );
        assert_eq!(
            directory.remove_file(name).unwrap_err().kind(),
            io::ErrorKind::InvalidInput,
            "{name:?}"
        );
    }
    assert_eq!(
        fs::read(root.0.join("cache/victim")).unwrap(),
        b"outside the capability"
    );
    assert_eq!(
        fs::read_dir(root.0.join("cache/buffers")).unwrap().count(),
        0
    );
}

#[test]
fn mutation_capability_requires_write_create_and_delete_permissions() {
    let root = Scratch::new();
    let mut vfs = root.vfs();
    assert!(vfs.write_directory("/code/forbidden", true).is_err());
    assert!(!root.0.join("code/forbidden").exists());
    for permissions in [
        FilePermissions {
            read: true,
            write: false,
            create: true,
            delete: true,
        },
        FilePermissions {
            read: true,
            write: true,
            create: false,
            delete: true,
        },
        FilePermissions {
            read: true,
            write: true,
            create: true,
            delete: false,
        },
    ] {
        vfs.mappings.get_mut("/cache").unwrap().permissions = permissions;
        assert_eq!(
            vfs.write_directory("/cache/forbidden", true)
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
        assert!(!root.0.join("cache/forbidden").exists());
    }
}

#[test]
fn opening_a_missing_directory_is_noncreating_and_creation_is_explicit() {
    let root = Scratch::new();
    let vfs = root.vfs();
    assert_eq!(
        vfs.write_directory("/cache/a/b", false).unwrap_err().kind(),
        io::ErrorKind::NotFound
    );
    assert!(!root.0.join("cache/a").exists());
    let directory = vfs.write_directory("/cache/a/b", true).unwrap();
    directory
        .create_new("data")
        .unwrap()
        .write_all(b"payload")
        .unwrap();
    assert_eq!(fs::read(root.0.join("cache/a/b/data")).unwrap(), b"payload");
    directory.remove_file("data").unwrap();
    assert!(!root.0.join("cache/a/b/data").exists());
}

#[test]
fn exclusive_creation_does_not_overwrite_an_existing_file() {
    let root = Scratch::new();
    let directory = root.vfs().write_directory("/cache", false).unwrap();
    fs::write(root.0.join("cache/occupied"), b"existing").unwrap();
    assert_eq!(
        directory.create_new("occupied").unwrap_err().kind(),
        io::ErrorKind::AlreadyExists
    );
    assert_eq!(
        fs::read(root.0.join("cache/occupied")).unwrap(),
        b"existing"
    );
}

#[cfg(unix)]
#[test]
fn directory_traversal_never_follows_symlinked_ancestors() {
    let root = Scratch::new();
    std::os::unix::fs::symlink(root.0.join("outside"), root.0.join("cache/link")).unwrap();
    for create in [false, true] {
        assert!(
            root.vfs()
                .write_directory("/cache/link/new", create)
                .is_err()
        );
    }
    assert_eq!(fs::read_dir(root.0.join("outside")).unwrap().count(), 0);
}

#[cfg(unix)]
#[test]
fn creation_and_deletion_use_the_captured_mapping_after_root_path_replacement() {
    let root = Scratch::new();
    let vfs = root.vfs();
    fs::rename(root.0.join("cache"), root.0.join("old-cache")).unwrap();
    fs::create_dir(root.0.join("cache")).unwrap();
    let directory = vfs.write_directory("/cache/buffers", true).unwrap();
    directory
        .create_new("data")
        .unwrap()
        .write_all(b"old generation")
        .unwrap();
    assert_eq!(
        fs::read(root.0.join("old-cache/buffers/data")).unwrap(),
        b"old generation"
    );
    assert_eq!(fs::read_dir(root.0.join("cache")).unwrap().count(), 0);
    directory.remove_file("data").unwrap();
    assert!(!root.0.join("old-cache/buffers/data").exists());
}

#[cfg(unix)]
#[test]
fn a_captured_child_directory_survives_rename_and_symlink_replacement() {
    let root = Scratch::new();
    let directory = root.vfs().write_directory("/cache/buffers", true).unwrap();
    fs::rename(
        root.0.join("cache/buffers"),
        root.0.join("cache/old-buffers"),
    )
    .unwrap();
    std::os::unix::fs::symlink(root.0.join("outside"), root.0.join("cache/buffers")).unwrap();
    fs::write(root.0.join("outside/same"), b"outside").unwrap();
    directory
        .create_new("same")
        .unwrap()
        .write_all(b"pinned")
        .unwrap();
    assert_eq!(
        fs::read(root.0.join("cache/old-buffers/same")).unwrap(),
        b"pinned"
    );
    directory.remove_file("same").unwrap();
    assert_eq!(fs::read(root.0.join("outside/same")).unwrap(), b"outside");
}

#[cfg(unix)]
#[test]
fn unlink_removes_a_leaf_symlink_itself_and_never_a_directory() {
    let root = Scratch::new();
    let directory = root.vfs().write_directory("/cache", false).unwrap();
    fs::write(root.0.join("outside/victim"), b"outside").unwrap();
    std::os::unix::fs::symlink(root.0.join("outside/victim"), root.0.join("cache/link")).unwrap();
    assert!(directory.create_new("link").is_err());
    directory.remove_file("link").unwrap();
    assert!(fs::symlink_metadata(root.0.join("cache/link")).is_err());
    assert_eq!(fs::read(root.0.join("outside/victim")).unwrap(), b"outside");
    fs::create_dir(root.0.join("cache/directory")).unwrap();
    assert!(directory.remove_file("directory").is_err());
    assert!(root.0.join("cache/directory").is_dir());
}

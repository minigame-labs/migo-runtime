use super::*;
use crate::vfs::{DirSource, MountBackend, MountTable, StagingArea};
use std::{
    io::{self, Read},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "migo-read-root-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn put(root: &Path, value: &str) {
    std::fs::create_dir_all(root.join("nested")).unwrap();
    std::fs::write(root.join("clip"), value).unwrap();
    std::fs::write(root.join("nested/clip"), value).unwrap();
}
fn text(mut file: File) -> String {
    let mut s = String::new();
    file.read_to_string(&mut s).unwrap();
    s
}

#[test]
fn root_capture_survives_rename_and_same_path_replacement() {
    let t = Temp::new();
    let path = t.0.join("code");
    put(&path, "original");
    let root = ReadRoot::open(&path).unwrap();
    std::fs::rename(&path, t.0.join("retired")).unwrap();
    put(&path, "replacement");
    assert_eq!(
        text(root.open_regular(Path::new("nested/clip")).unwrap()),
        "original"
    );
}
#[test]
fn portable_names_and_nonrelative_paths_fail_before_native_open() {
    let t = Temp::new();
    put(&t.0, "data");
    let root = ReadRoot::open(&t.0).unwrap();
    #[cfg(unix)]
    for name in [
        "NUL",
        "clip:stream",
        "trailing.",
        "COM³.txt",
        "bad\u{7f}",
        "bad\u{85}",
    ] {
        std::fs::write(t.0.join(name), "must not open").unwrap();
    }
    for name in [
        "",
        "..",
        "../clip",
        "/clip",
        "NUL",
        "clip:stream",
        "trailing.",
        "COM³.txt",
        "nested\\clip",
        "bad\0name",
        "bad\u{7f}",
        "bad\u{85}",
    ] {
        assert!(root.open_regular(Path::new(name)).is_err(), "{name:?}");
    }
}
#[cfg(unix)]
#[test]
fn links_and_special_files_never_become_regular_assets() {
    use std::os::unix::fs::symlink;
    let t = Temp::new();
    put(&t.0, "data");
    let root = ReadRoot::open(&t.0).unwrap();
    symlink(t.0.join("clip"), t.0.join("alias")).unwrap();
    symlink(t.0.join("nested"), t.0.join("parent-alias")).unwrap();
    for path in ["alias", "parent-alias/clip", "nested"] {
        assert!(root.open_regular(Path::new(path)).is_err());
    }
    let path = t.0.join("pipe");
    let c = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: c is a live NUL-terminated path; mkfifo borrows it for the call.
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
    let started = std::time::Instant::now();
    assert!(root.open_regular(Path::new("pipe")).is_err());
    assert!(started.elapsed() < std::time::Duration::from_secs(1));
}
#[cfg(unix)]
#[test]
fn replacing_a_parent_after_traversal_keeps_the_opened_directory() {
    let t = Temp::new();
    let inside = t.0.join("code");
    let outside = t.0.join("outside");
    put(&inside, "original");
    put(&outside, "outside");
    let root = ReadRoot::open(&inside).unwrap();
    let file = root
        .open_with_hook(Path::new("nested/clip"), || {
            std::fs::rename(inside.join("nested"), inside.join("retired")).unwrap();
            std::os::unix::fs::symlink(&outside, inside.join("nested")).unwrap();
        })
        .unwrap();
    assert_eq!(text(file), "original");
}
#[test]
fn directory_mount_uses_captured_root_not_a_later_path_lookup() {
    let t = Temp::new();
    let code = t.0.join("code");
    put(&code, "original");
    let mt = MountTable::new(code.clone());
    std::fs::rename(&code, t.0.join("old")).unwrap();
    put(&code, "replacement");
    let pin = mt.pin_directory_read("clip", mt.generation()).unwrap();
    assert_eq!(text(pin.open().unwrap()), "original");
}
#[test]
fn overlay_identity_generation_and_retirement_are_fixed_at_admission() {
    let t = Temp::new();
    let a = t.0.join("a");
    let b = t.0.join("b");
    let c = t.0.join("c");
    put(&a, "base");
    put(&b, "overlay");
    put(&c, "nested");
    let mt = MountTable::new(a);
    assert!(mt.mount_overlay(
        "outer".into(),
        "level".into(),
        Arc::new(DirSource::try_new(b).unwrap())
    ));
    assert!(mt.mount_overlay(
        "inner".into(),
        "level/deep".into(),
        Arc::new(DirSource::try_new(c).unwrap())
    ));
    assert!(mt.mount_overlay(
        "reserved".into(),
        "NUL".into(),
        Arc::new(DirSource::try_new(t.0.join("a")).unwrap()),
    ));
    assert!(mt.pin_directory_read("NUL/clip", mt.generation()).is_err());
    let generation = mt.generation();
    let old = mt
        .pin_directory_read("level/deep/clip", generation)
        .unwrap();
    assert_eq!(text(old.open().unwrap()), "nested");
    assert_eq!(old.generation(), generation);
    let outer = mt.pin_directory_read("level/clip", generation).unwrap();
    assert_eq!(text(outer.open().unwrap()), "overlay");
    assert_ne!(old.source_identity(), outer.source_identity());
    assert!(mt.unmount_overlay("level/deep"));
    assert!(
        mt.pin_directory_read("level/deep/clip", generation)
            .is_err()
    );
    assert_eq!(text(old.open().unwrap()), "nested");
    assert!(mt.pin_directory_read("/clip", mt.generation()).is_err());
    assert!(mt.pin_directory_read("../clip", mt.generation()).is_err());
}
#[test]
fn pinning_needs_explicit_authority_and_does_not_stat_or_reopen_paths() {
    #[derive(Debug)]
    struct NoAuthority(PathBuf);
    impl MountBackend for NoAuthority {
        fn read(&self, _: &str) -> io::Result<Vec<u8>> {
            panic!("whole-file fallback")
        }
        fn exists(&self, _: &str) -> bool {
            panic!("pin performed IO")
        }
        fn is_file(&self, _: &str) -> bool {
            panic!("pin performed IO")
        }
        fn real_path(&self, _: &str) -> Option<PathBuf> {
            panic!("pin reopened a pathname")
        }
        fn root_dir(&self) -> Option<&Path> {
            Some(&self.0)
        }
    }
    let t = Temp::new();
    put(&t.0, "data");
    let mt = MountTable::new(t.0.clone());
    mt.swap_base(Arc::new(NoAuthority(t.0.clone())));
    assert!(
        matches!(mt.pin_directory_read("clip",mt.generation()),Err(e)if e.kind()==io::ErrorKind::Unsupported)
    );
    #[derive(Debug)]
    struct Authority(Arc<ReadRoot>);
    impl MountBackend for Authority {
        fn directory_read_root(&self) -> Option<Arc<ReadRoot>> {
            Some(self.0.clone())
        }
        fn read(&self, _: &str) -> io::Result<Vec<u8>> {
            panic!("whole-file fallback")
        }
        fn exists(&self, _: &str) -> bool {
            panic!("pin performed IO")
        }
        fn is_file(&self, _: &str) -> bool {
            panic!("pin performed IO")
        }
        fn real_path(&self, _: &str) -> Option<PathBuf> {
            panic!("pin reopened a pathname")
        }
        fn root_dir(&self) -> Option<&Path> {
            None
        }
    }
    mt.swap_base(Arc::new(Authority(Arc::new(ReadRoot::open(&t.0).unwrap()))));
    assert_eq!(
        text(
            mt.pin_directory_read("clip", mt.generation())
                .unwrap()
                .open()
                .unwrap()
        ),
        "data"
    );
    let absent = t.0.join("absent");
    assert!(DirSource::try_new(absent.clone()).is_err());
    mt.swap_base(Arc::new(DirSource::new(absent.clone())));
    put(&absent, "late");
    assert!(mt.pin_directory_read("clip", mt.generation()).is_err());
}
#[test]
fn cleaned_old_directory_fails_closed_but_an_open_file_keeps_its_bytes() {
    let t = Temp::new();
    let code = t.0.join("code");
    put(&code, "original");
    let mt = MountTable::new(code.clone());
    let pending = mt.pin_directory_read("clip", mt.generation()).unwrap();
    let opened = pending.open().unwrap();
    std::fs::remove_dir_all(&code).unwrap();
    put(&code, "replacement");
    assert!(
        pending.open().is_err(),
        "removed generation retargeted a new directory"
    );
    assert_eq!(text(opened), "original");
}
#[test]
fn staging_install_still_replaces_a_live_directory_generation() {
    let t = Temp::new();
    let base = t.0.join("base");
    let active = t.0.join("active");
    put(&base, "base");
    put(&active, "old");
    let mt = MountTable::new(base);
    mt.mount_overlay(
        "subpackage:stage".into(),
        "stage".into(),
        Arc::new(DirSource::try_new(active.clone()).unwrap()),
    );
    let old = mt
        .pin_directory_read("stage/clip", mt.generation())
        .unwrap();
    let file = old.open().unwrap();
    let staging = StagingArea::create(&t.0, "stage").unwrap();
    put(staging.dir(), "new");
    staging.install(&mt, &active, "stage").unwrap();
    assert_eq!(text(file), "old");
    assert_eq!(
        text(
            mt.pin_directory_read("stage/clip", mt.generation())
                .unwrap()
                .open()
                .unwrap()
        ),
        "new"
    );
    assert!(old.open().is_err());
}

#[cfg(windows)]
#[test]
fn replacement_between_root_path_query_and_guard_open_is_rejected() {
    let t = Temp::new();
    let code = t.0.join("code");
    put(&code, "original");
    let root = ReadRoot::open(&code).unwrap();
    let result = root.open_with_guard_hook(Path::new("clip"), |queried| {
        std::fs::rename(queried, t.0.join("retired")).unwrap();
        put(&code, "replacement");
    });
    assert!(
        result.is_err(),
        "query/open race retargeted the replacement"
    );
}

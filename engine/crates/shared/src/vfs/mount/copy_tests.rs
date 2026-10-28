use super::*;
use std::{io::Read, sync::Weak};
#[derive(Debug)]
struct Backend {
    mounts: Weak<MountTable>,
    replace: bool,
}
impl MountBackend for Backend {
    fn read(&self, _: &str) -> io::Result<Vec<u8>> {
        panic!("copy collected a whole backend entry")
    }
    fn exists(&self, _: &str) -> bool {
        true
    }
    fn real_path(&self, _: &str) -> Option<PathBuf> {
        None
    }
    fn root_dir(&self) -> Option<&Path> {
        None
    }
    fn open_copy_source(&self, relative: &str) -> io::Result<CopySource> {
        assert_eq!(relative, "clip");
        let mounts = self.mounts.upgrade().unwrap();
        assert!(
            mounts.inner.try_write().is_some(),
            "native/backend IO ran under mount lock"
        );
        if self.replace {
            assert!(mounts.mount_overlay(
                "new".into(),
                "nested".into(),
                Arc::new(Backend {
                    mounts: self.mounts.clone(),
                    replace: false
                })
            ));
        }
        Ok(CopySource::Stream(Box::new(io::Cursor::new(
            if self.replace {
                &b"old"[..]
            } else {
                &b"new"[..]
            },
        ))))
    }
}
#[test]
fn copy_source_selection_releases_mount_lock_before_open_and_pins_old_backend() {
    let mounts = Arc::new(MountTable::new(std::env::temp_dir()));
    assert!(mounts.mount_overlay(
        "old".into(),
        "nested".into(),
        Arc::new(Backend {
            mounts: Arc::downgrade(&mounts),
            replace: true
        })
    ));
    let CopySource::Stream(mut source) = mounts.open_copy_source("nested/clip").unwrap() else {
        panic!("wrong source")
    };
    let mut bytes = Vec::new();
    source.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"old");
    let CopySource::Stream(mut source) = mounts.open_copy_source("nested/clip").unwrap() else {
        panic!("wrong source")
    };
    bytes.clear();
    source.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"new");
}
#[test]
fn copy_source_rejects_unsafe_logical_prefixes_before_backend_open() {
    let mounts = Arc::new(MountTable::new(std::env::temp_dir()));
    for path in [
        "../clip",
        "/clip",
        "NUL/clip",
        "nested\\clip",
        "nested/clip:stream",
    ] {
        assert!(
            mounts.open_copy_source(path).is_err(),
            "unsafe copy source {path}"
        );
    }
}

#[test]
fn admitted_copy_keeps_backend_selected_before_a_queued_open() {
    let mounts = Arc::new(MountTable::new(std::env::temp_dir()));
    assert!(mounts.mount_overlay(
        "old".into(),
        "nested".into(),
        Arc::new(Backend {
            mounts: Arc::downgrade(&mounts),
            replace: true
        })
    ));
    let admitted = mounts.pin_copy_source("nested/clip").unwrap();
    assert!(mounts.mount_overlay(
        "new".into(),
        "nested".into(),
        Arc::new(Backend {
            mounts: Arc::downgrade(&mounts),
            replace: false
        })
    ));
    let CopySource::Stream(mut source) = admitted.open().unwrap() else {
        panic!("wrong source")
    };
    let mut bytes = Vec::new();
    source.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"old");
}

#[test]
fn unsupported_copy_backend_never_falls_back_to_whole_entry_read() {
    #[derive(Debug)]
    struct NoCopy;
    impl MountBackend for NoCopy {
        fn read(&self, _: &str) -> io::Result<Vec<u8>> {
            panic!("whole-entry fallback was invoked")
        }
        fn exists(&self, _: &str) -> bool {
            true
        }
        fn real_path(&self, _: &str) -> Option<PathBuf> {
            None
        }
        fn root_dir(&self) -> Option<&Path> {
            None
        }
    }
    let mounts = MountTable::new(std::env::temp_dir());
    assert!(mounts.mount_overlay("unsupported".into(), String::new(), Arc::new(NoCopy)));
    assert!(
        matches!(mounts.open_copy_source("clip"),Err(error) if error.kind()==io::ErrorKind::Unsupported)
    );
}

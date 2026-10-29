use super::*;
use crate::vfs::{PackSource, PackageWriter};
use std::{io::Read, sync::atomic::AtomicUsize};
static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "migo-asset-pin-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
    fn directory(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let p = self.0.join(name);
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(p.join("clip"), bytes).unwrap();
        p
    }
    fn package(&self) -> Arc<dyn MountBackend> {
        let p = self.0.join("asset.mpkg");
        let mut w = PackageWriter::new(std::fs::File::create(&p).unwrap()).unwrap();
        w.add_entry("clip", b"package").unwrap();
        w.finish("game", "1").unwrap();
        Arc::new(PackSource::open(&p, "game", "1").unwrap())
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn text(pin: PinnedAssetRead) -> Vec<u8> {
    match pin {
        PinnedAssetRead::Directory(pin) => {
            let mut b = Vec::new();
            pin.open().unwrap().read_to_end(&mut b).unwrap();
            b
        }
        PinnedAssetRead::Package(pin) => {
            let mut r = pin.open_reader(Default::default()).unwrap();
            let mut b = vec![0; r.len() as usize];
            r.read_exact_at(0, &mut b).unwrap();
            b
        }
    }
}
#[test]
fn automatic_selection_retains_the_original_directory_and_package_across_replacements() {
    let f = Fixture::new();
    let a = f.directory("old", b"directory");
    let mt = MountTable::new(a);
    let directory = mt.pin_asset_read("clip", mt.generation()).unwrap();
    mt.swap_base(f.package());
    let package = mt.pin_asset_read("clip", mt.generation()).unwrap();
    mt.swap_base(Arc::new(
        DirSource::try_new(f.directory("new", b"replacement")).unwrap(),
    ));
    assert_eq!(text(directory), b"directory");
    assert_eq!(text(package), b"package");
    assert_eq!(
        text(mt.pin_asset_read("clip", mt.generation()).unwrap()),
        b"replacement"
    );
}
#[test]
fn overlay_choice_and_generation_are_captured_before_queued_open() {
    let f = Fixture::new();
    let mt = MountTable::new(f.directory("base", b"base"));
    assert!(mt.mount_overlay("package".into(), "sub".into(), f.package()));
    let generation = mt.generation();
    let pin = mt.pin_asset_read("sub/clip", generation).unwrap();
    assert!(mt.mount_overlay(
        "directory".into(),
        "sub".into(),
        Arc::new(DirSource::try_new(f.directory("dir", b"directory")).unwrap())
    ));
    assert!(mt.pin_asset_read("sub/clip", generation).is_err());
    assert_eq!(text(pin), b"package");
    assert_eq!(
        text(mt.pin_asset_read("sub/clip", mt.generation()).unwrap()),
        b"directory"
    );
}
#[derive(Debug)]
struct NoIo {
    package: bool,
}
impl MountBackend for NoIo {
    fn has_package_reader(&self) -> bool {
        self.package
    }
    fn read(&self, _: &str) -> io::Result<Vec<u8>> {
        panic!("whole read during admission")
    }
    fn exists(&self, _: &str) -> bool {
        panic!("exists during admission")
    }
    fn real_path(&self, _: &str) -> Option<PathBuf> {
        panic!("path lookup during admission")
    }
    fn root_dir(&self) -> Option<&Path> {
        panic!("root lookup during admission")
    }
    fn is_file(&self, _: &str) -> bool {
        panic!("file stat during admission")
    }
    fn open_package_entry(
        &self,
        _: &str,
        _: super::super::package::PackageReadLimits,
    ) -> io::Result<super::super::package::PackageEntryReader> {
        Err(io::Error::other("selected package open failed"))
    }
}
#[test]
fn selection_uses_explicit_authority_without_path_probes_or_entry_reads() {
    let mt = MountTable::new(std::env::temp_dir());
    mt.swap_base(Arc::new(NoIo { package: true }));
    let PinnedAssetRead::Package(pin) = mt.pin_asset_read("clip", mt.generation()).unwrap() else {
        panic!("wrong class")
    };
    assert!(pin.open_reader(Default::default()).is_err());
}
#[test]
fn unsupported_backends_are_not_assumed_to_be_packages() {
    let mt = MountTable::new(std::env::temp_dir());
    mt.swap_base(Arc::new(NoIo { package: false }));
    assert!(mt.pin_asset_read("clip", mt.generation()).is_err());
}
#[test]
fn unsafe_prefixes_cannot_be_hidden_by_overlay_selection() {
    let f = Fixture::new();
    let mt = MountTable::new(f.directory("base", b"base"));
    assert!(mt.mount_overlay("bad".into(), "NUL".into(), f.package()));
    for p in [
        "/clip",
        "../clip",
        "NUL/clip",
        "clip:stream",
        "sub\\clip",
        "bad\u{85}/clip",
    ] {
        assert!(mt.pin_asset_read(p, mt.generation()).is_err(), "{p}");
    }
}
#[test]
fn failed_selected_overlay_never_retries_the_base() {
    let f = Fixture::new();
    let mt = MountTable::new(f.directory("base", b"base"));
    assert!(mt.mount_overlay(
        "failed".into(),
        "sub".into(),
        Arc::new(NoIo { package: true })
    ));
    let pin = mt.pin_asset_read("sub/clip", mt.generation()).unwrap();
    let PinnedAssetRead::Package(pin) = pin else {
        panic!("overlay not selected")
    };
    assert!(pin.open_reader(Default::default()).is_err());
}

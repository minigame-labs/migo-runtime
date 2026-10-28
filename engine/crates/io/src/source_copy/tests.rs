use super::*;
use crate::fs_ops;
use shared::vfs::{
    MountTable,
    file_io::FileStamp,
    package::{PackSource, PackageWriter},
};
use std::{io::Read, sync::Arc};

fn track(file: &File) -> FileMutations {
    FileMutations::for_file(file).unwrap().unwrap()
}
fn prepared(path: &Path) -> PreparedCopy {
    PreparedCopy::new(CopySource::File(open_source(path).unwrap())).unwrap()
}
fn package(path: &Path, bytes: &[u8]) -> Arc<PackSource> {
    let mut writer = PackageWriter::new(File::create(path).unwrap()).unwrap();
    writer.add_entry("clip", bytes).unwrap();
    writer.finish("game", "1").unwrap();
    Arc::new(PackSource::open(path, "game", "1").unwrap())
}

#[test]
fn native_copy_revokes_restored_metadata_and_preserves_complete_payload() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    let target = root.path().join("target");
    let bytes = (0..196_777).map(|n| (n % 251) as u8).collect::<Vec<_>>();
    std::fs::write(&source, &bytes).unwrap();
    std::fs::write(&target, vec![b'X'; bytes.len()]).unwrap();
    let file = File::options()
        .read(true)
        .write(true)
        .open(&target)
        .unwrap();
    let mutations = track(&file);
    let before = mutations.read_epoch().unwrap();
    let modified = file.metadata().unwrap().modified().unwrap();
    let stamp = FileStamp::of(file.metadata().unwrap()).unwrap();
    fs_ops::copy(source.to_str().unwrap(), target.to_str().unwrap()).unwrap();
    file.set_times(std::fs::FileTimes::new().set_modified(modified))
        .unwrap();
    assert!(stamp.verify(&file).is_ok());
    assert!(before.verify().is_err());
    assert!(mutations.read_epoch().is_ok());
    assert_eq!(std::fs::read(target).unwrap(), bytes);
}
#[test]
fn aliases_fail_before_any_payload_or_epoch_mutation() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("source");
    std::fs::write(&path, b"original").unwrap();
    let file = File::open(&path).unwrap();
    let mutations = track(&file);
    let before = mutations.read_epoch().unwrap();
    assert!(fs_ops::copy(path.to_str().unwrap(), path.to_str().unwrap()).is_err());
    assert!(before.verify().is_ok());
    assert_eq!(std::fs::read(&path).unwrap(), b"original");
    #[cfg(unix)]
    for symbolic in [false, true] {
        let alias = root
            .path()
            .join(if symbolic { "symlink" } else { "hardlink" });
        if symbolic {
            std::os::unix::fs::symlink(&path, &alias).unwrap();
        } else {
            std::fs::hard_link(&path, &alias).unwrap();
        }
        assert!(fs_ops::copy(path.to_str().unwrap(), alias.to_str().unwrap()).is_err());
        assert!(before.verify().is_ok());
        assert_eq!(std::fs::read(&path).unwrap(), b"original");
    }
}
#[test]
fn native_open_precedes_registration_without_truncation_and_source_is_pinned() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    let target = root.path().join("target");
    std::fs::write(&source, b"old-source").unwrap();
    std::fs::write(&target, b"old-target-longer").unwrap();
    let target_file = File::open(&target).unwrap();
    let mutations = track(&target_file);
    let before = mutations.read_epoch().unwrap();
    prepared(&source)
        .copy_to_path_with(&target, |opened| {
            assert_eq!(
                opened.metadata().unwrap().len(),
                17,
                "native open truncated before registration"
            );
            assert!(before.verify().is_ok());
            std::fs::rename(&source, root.path().join("retained-source")).unwrap();
            std::fs::write(&source, b"new-source").unwrap();
        })
        .unwrap();
    assert!(before.verify().is_err());
    assert_eq!(std::fs::read(&target).unwrap(), b"old-source");
}
#[test]
fn stream_transfer_holds_epoch_through_partial_io_failure() {
    struct Reader {
        file: File,
        reads: usize,
    }
    impl Read for Reader {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            assert!(
                track(&self.file).read_epoch().is_err(),
                "copy writer epoch ended before payload IO"
            );
            self.reads += 1;
            if self.reads == 1 {
                buffer[0] = b'n';
                Ok(1)
            } else {
                Err(io::Error::other("injected read failure"))
            }
        }
    }
    let root = tempfile::tempdir().unwrap();
    let target = root.path().join("target");
    std::fs::write(&target, b"old").unwrap();
    let file = File::open(&target).unwrap();
    let mutations = track(&file);
    let before = mutations.read_epoch().unwrap();
    let source = CopySource::Stream(Box::new(Reader { file, reads: 0 }));
    assert!(
        PreparedCopy::new(source)
            .unwrap()
            .copy_to_path(&target)
            .is_err()
    );
    assert!(before.verify().is_err());
    assert!(mutations.read_epoch().is_ok());
    assert_eq!(std::fs::read(target).unwrap(), b"n");
}
#[test]
fn directory_source_survives_root_and_overlay_replacement() {
    let root = tempfile::tempdir().unwrap();
    let code = root.path().join("code");
    std::fs::create_dir(&code).unwrap();
    std::fs::write(code.join("clip"), b"old").unwrap();
    let mounts = MountTable::new(code.clone());
    let source = mounts.open_copy_source("clip").unwrap();
    std::fs::rename(&code, root.path().join("retired")).unwrap();
    std::fs::create_dir(&code).unwrap();
    std::fs::write(code.join("clip"), b"new").unwrap();
    let target = root.path().join("target");
    PreparedCopy::new(source)
        .unwrap()
        .copy_to_path(&target)
        .unwrap();
    assert_eq!(std::fs::read(&target).unwrap(), b"old");
    fs_ops::copy_mount_entry_to_path(&mounts, "clip", &target).unwrap();
    assert_eq!(std::fs::read(&target).unwrap(), b"old");
    let overlay = root.path().join("overlay");
    std::fs::create_dir(&overlay).unwrap();
    std::fs::write(overlay.join("clip"), b"overlay").unwrap();
    assert!(mounts.mount_overlay(
        "overlay".into(),
        String::new(),
        Arc::new(shared::vfs::DirSource::new(overlay))
    ));
    fs_ops::copy_mount_entry_to_path(&mounts, "clip", &target).unwrap();
    assert_eq!(std::fs::read(&target).unwrap(), b"overlay");
}
#[test]
fn package_copy_streams_exact_bytes_and_tracks_destination() {
    let root = tempfile::tempdir().unwrap();
    let mounts = MountTable::new(root.path().to_owned());
    let bytes = (0..1_048_649).map(|n| (n % 251) as u8).collect::<Vec<_>>();
    assert!(mounts.mount_overlay(
        "pack".into(),
        String::new(),
        package(&root.path().join("input.mpkg"), &bytes)
    ));
    let target = root.path().join("target");
    std::fs::write(&target, b"previous-tail").unwrap();
    let file = File::open(&target).unwrap();
    let mutations = track(&file);
    let before = mutations.read_epoch().unwrap();
    fs_ops::copy_mount_entry_to_path(&mounts, "clip", &target).unwrap();
    assert!(before.verify().is_err());
    assert_eq!(std::fs::read(target).unwrap(), bytes);
    assert!(mutations.read_epoch().is_ok());
    let path = fs_ops::materialize_mount_entry_to_temp(&mounts, "clip", ".bin").unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    std::fs::remove_file(&path).unwrap();
}
#[test]
fn package_container_and_hardlink_alias_are_rejected_without_damaging_archive() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("input.mpkg");
    let backend = package(&path, b"clip");
    let original = std::fs::read(&path).unwrap();
    let mounts = MountTable::new(root.path().to_owned());
    assert!(mounts.mount_overlay("pack".into(), String::new(), backend));
    assert!(fs_ops::copy_mount_entry_to_path(&mounts, "clip", &path).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), original);
    #[cfg(unix)]
    {
        let alias = root.path().join("alias");
        std::fs::hard_link(&path, &alias).unwrap();
        assert!(fs_ops::copy_mount_entry_to_path(&mounts, "clip", &alias).is_err());
        assert_eq!(std::fs::read(alias).unwrap(), original);
    }
}
#[test]
fn source_failure_precedes_destination_open_and_failed_native_write_revokes_epoch() {
    let root = tempfile::tempdir().unwrap();
    let missing = root.path().join("missing");
    let target = root.path().join("target");
    assert!(fs_ops::copy(missing.to_str().unwrap(), target.to_str().unwrap()).is_err());
    assert!(!target.exists());
    assert!(fs_ops::copy(root.path().to_str().unwrap(), target.to_str().unwrap()).is_err());
    assert!(!target.exists());
    std::fs::write(&missing, b"source").unwrap();
    std::fs::write(&target, b"old").unwrap();
    let mut readonly = File::open(&target).unwrap();
    let mutations = track(&readonly);
    let before = mutations.read_epoch().unwrap();
    assert!(prepared(&missing).copy_to_file(&mut readonly).is_err());
    assert!(before.verify().is_err());
    assert!(mutations.read_epoch().is_ok());
    assert_eq!(std::fs::read(target).unwrap(), b"old");
}
#[test]
fn native_copy_resets_source_and_destination_cursors_and_preserves_permissions() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    let target = root.path().join("target");
    std::fs::write(&source, b"source").unwrap();
    std::fs::write(&target, b"long-old-target").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&source, Permissions::from_mode(0o600)).unwrap();
    }
    let mut source = File::open(&source).unwrap();
    source.seek(io::SeekFrom::Start(3)).unwrap();
    let mut destination = File::options()
        .read(true)
        .write(true)
        .open(&target)
        .unwrap();
    destination.seek(io::SeekFrom::End(0)).unwrap();
    PreparedCopy::new(CopySource::File(source))
        .unwrap()
        .copy_to_file(&mut destination)
        .unwrap();
    assert_eq!(std::fs::read(&target).unwrap(), b"source");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            destination.metadata().unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

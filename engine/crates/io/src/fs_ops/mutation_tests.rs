use super::*;
use crate::domain::IoDomain;
use std::fs::File;
fn fixture() -> (tempfile::TempDir, PathBuf, File, FileMutations) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("file");
    std::fs::write(&path, b"old").unwrap();
    let file = File::open(&path).unwrap();
    let mutations = FileMutations::for_file(&file).unwrap().unwrap();
    (root, path, file, mutations)
}
#[test]
fn descriptor_mutations_coordinate_across_independent_domains() {
    let (_root, path, _file, mutations) = fixture();
    let old = mutations.read_epoch().unwrap();
    let a = IoDomain::new();
    let b = IoDomain::new();
    let fa = a.open_file(&path, OpenFlag::ReadWrite, None, None).unwrap();
    let fb = b.open_file(&path, OpenFlag::ReadWrite, None, None).unwrap();
    assert!(
        old.verify().is_ok(),
        "idle writable opens are not payload mutations"
    );
    a.write_file(fa, b"new", Some(0)).unwrap();
    assert!(old.verify().is_err());
    let read = mutations.read_epoch().unwrap();
    b.write_file(fb, b"X", Some(1)).unwrap();
    assert!(read.verify().is_err());
    let read = mutations.read_epoch().unwrap();
    b.ftruncate(fb, 1).unwrap();
    assert!(read.verify().is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"n");
}
#[test]
fn truncating_open_preserves_bytes_until_registration_then_revokes_the_epoch() {
    for flag in [
        OpenFlag::WriteTruncateCreate,
        OpenFlag::ReadWriteTruncateCreate,
    ] {
        let (_root, path, _file, mutations) = fixture();
        let before = mutations.read_epoch().unwrap();
        let check = mutations.clone();
        let mut table = FileTable::new();
        table.after_native_open = Some(Box::new(move |file| {
            assert_eq!(
                file.metadata().unwrap().len(),
                3,
                "native open truncated before mutation registration"
            );
            assert!(check.read_epoch().is_ok());
        }));
        table
            .open(path.to_str().unwrap(), flag, None, None)
            .unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);
        assert!(before.verify().is_err());
    }
}
#[test]
fn path_fast_overwrite_and_durable_or_fast_append_revoke_the_right_object() {
    let (_root, path, file, mutations) = fixture();
    let before = mutations.read_epoch().unwrap();
    write_file(
        path.to_str().unwrap(),
        b"new",
        WriteMode::Overwrite,
        WriteDurability::Fast,
    )
    .unwrap();
    assert!(before.verify().is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"new");
    for durability in [WriteDurability::Fast, WriteDurability::Durable] {
        let before = mutations.read_epoch().unwrap();
        write_file(path.to_str().unwrap(), b"x", WriteMode::Append, durability).unwrap();
        assert!(before.verify().is_err());
    }
    let before = mutations.read_epoch().unwrap();
    write_file(
        path.to_str().unwrap(),
        b"replacement",
        WriteMode::Overwrite,
        WriteDurability::Durable,
    )
    .unwrap();
    assert!(
        before.verify().is_ok(),
        "atomic replacement rewrote the anchored old object"
    );
    assert_ne!(
        shared::vfs::file_io::FileIdentity::of(&file).unwrap(),
        shared::vfs::file_io::FileIdentity::of(&File::open(&path).unwrap()).unwrap()
    );
    let mut old = [0; 5];
    shared::vfs::file_io::read_exact_at(&file, &mut old, 0).unwrap();
    assert_eq!(&old, b"newxx");
}
#[test]
fn atomic_writer_enrols_its_temporary_object_before_calling_the_writer() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("file");
    crate::atomic_write::atomic_write_with(&path, |file| {
        let mutations = FileMutations::for_file(file)?.unwrap();
        assert!(
            mutations.read_epoch().is_err(),
            "atomic temporary writer was unregistered"
        );
        file.write_all(b"new")
    })
    .unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"new");
    assert!(
        crate::atomic_write::atomic_write_with(&path, |file| {
            assert!(
                FileMutations::for_file(file)?
                    .unwrap()
                    .read_epoch()
                    .is_err()
            );
            file.write_all(b"partial")?;
            Err(std::io::Error::other("injected writer failure"))
        })
        .is_err()
    );
    assert_eq!(std::fs::read(&path).unwrap(), b"new");
}
#[test]
fn native_write_and_truncate_failures_revoke_but_readonly_opens_do_not() {
    let (_root, path, file, mutations) = fixture();
    let mut fd = FdIo::new(file, false, Some(mutations.clone()));
    let before = mutations.read_epoch().unwrap();
    assert!(fd.write(b"x", None).is_err());
    assert!(before.verify().is_err());
    let before = mutations.read_epoch().unwrap();
    assert!(fd.ftruncate(0).is_err());
    assert!(before.verify().is_err());
    let before = mutations.read_epoch().unwrap();
    let mut table = FileTable::new();
    let id = table
        .open(path.to_str().unwrap(), OpenFlag::Read, None, None)
        .unwrap();
    assert!(table.get_fd_entry(id).unwrap().lock().mutations.is_none());
    assert!(before.verify().is_ok());
}
#[test]
fn repeated_descriptor_writes_have_no_steady_rust_heap_allocation() {
    let (_root, path, _file, _mutations) = fixture();
    let mut table = FileTable::new();
    let id = table
        .open(path.to_str().unwrap(), OpenFlag::ReadWrite, None, None)
        .unwrap();
    let owner = table.get_fd_entry(id).unwrap();
    let mut fd = owner.lock();
    migo_alloc_probe::assert_no_steady_state_allocation(
        migo_alloc_probe::Burst {
            path: "tracked descriptor write/truncate",
            warmup: 8,
            measured: 96,
        },
        |_| {
            fd.write(b"new", Some(0)).unwrap();
            fd.ftruncate(3).unwrap();
        },
    );
}

#[test]
fn cross_fs_transfer_uses_active_epoch_and_atomic_publication() {
    let (root, dest, old_file, old_mutations) = fixture();
    let old = old_mutations.read_epoch().unwrap();
    let source = root.path().join("source");
    std::fs::write(&source, b"replacement").unwrap();
    let mut retained = None;
    rename_cross_fs_with(
        source.to_str().unwrap(),
        dest.to_str().unwrap(),
        |src, dst| {
            let mutations = FileMutations::for_file(dst)?.unwrap();
            assert!(
                mutations.read_epoch().is_err(),
                "cross-FS stage writer is untracked"
            );
            assert!(
                old.verify().is_ok(),
                "stage truncated the anchored old target"
            );
            // Keep the native object alive until the epoch check: a registration
            // alone cannot prevent inode reuse after an unlinked stage closes.
            retained = Some((dst.try_clone()?, mutations));
            std::io::copy(src, dst)?;
            Ok(())
        },
    )
    .unwrap();
    assert!(
        retained.as_ref().unwrap().1.read_epoch().is_ok(),
        "stage guard leaked"
    );
    assert!(old.verify().is_ok());
    assert!(!source.exists());
    assert_eq!(std::fs::read(&dest).unwrap(), b"replacement");
    let mut previous = [0; 3];
    shared::vfs::file_io::read_exact_at(&old_file, &mut previous, 0).unwrap();
    assert_eq!(&previous, b"old");
    assert_eq!(
        std::fs::read_dir(root.path()).unwrap().count(),
        1,
        "stage leaked"
    );
}

#[test]
fn cross_fs_transfer_failure_keeps_source_and_target_and_cleans_partial_stage() {
    let (root, dest, _old_file, mutations) = fixture();
    let before = mutations.read_epoch().unwrap();
    let source = root.path().join("source");
    std::fs::write(&source, b"source").unwrap();
    let mut retained = None;
    let result = rename_cross_fs_with(
        source.to_str().unwrap(),
        dest.to_str().unwrap(),
        |_, dst| {
            let mutations = FileMutations::for_file(dst)?.unwrap();
            assert!(mutations.read_epoch().is_err());
            // Keep the native object alive until the epoch check: a registration
            // alone cannot prevent inode reuse after an unlinked stage closes.
            retained = Some((dst.try_clone()?, mutations));
            dst.write_all(b"partial")?;
            Err(std::io::Error::other("injected partial transfer failure"))
        },
    );
    assert!(result.is_err());
    assert!(retained.as_ref().unwrap().1.read_epoch().is_ok());
    assert!(before.verify().is_ok());
    assert_eq!(std::fs::read(&source).unwrap(), b"source");
    assert_eq!(std::fs::read(&dest).unwrap(), b"old");
    assert_eq!(
        std::fs::read_dir(root.path()).unwrap().count(),
        2,
        "partial stage leaked"
    );
}

#[test]
fn cross_fs_production_transfer_preserves_a_large_payload() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    let dest = root.path().join("dest");
    let bytes = (0..196_777).map(|n| (n % 251) as u8).collect::<Vec<_>>();
    std::fs::write(&source, &bytes).unwrap();
    rename_cross_fs(source.to_str().unwrap(), dest.to_str().unwrap()).unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), bytes);
    assert!(!source.exists());
}

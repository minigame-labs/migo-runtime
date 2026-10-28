use super::*;
use crate::vfs::{file_io::read_exact_at, file_mutation::FileMutations};
use std::os::unix::fs::PermissionsExt;
use std::{fs, fs::File, io::Write};

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let id = NEXT_IMPORT.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("migo-import-epoch-{}-{id}", std::process::id()));
        fs::create_dir(&dir).unwrap();
        fs::create_dir(dir.join("tmp")).unwrap();
        fs::write(dir.join("original"), vec![42; 192 * 1024 + 17]).unwrap();
        std::os::unix::fs::symlink(dir.join("original"), dir.join("link.mp3")).unwrap();
        Self(dir)
    }
    fn source(&self) -> PathBuf {
        self.0.join("link.mp3")
    }
    fn temp(&self) -> PathBuf {
        self.0.join("tmp")
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn import_copy_tracks_the_reserved_object_and_reads_the_pinned_source() {
    let root = Scratch::new();
    fs::set_permissions(root.0.join("original"), fs::Permissions::from_mode(0o440)).unwrap();
    let mut held = None;
    let (path, _) = take_file_with(&root.source(), &root.temp(), |input, output| {
        let target = fs::read_dir(root.temp())?.next().unwrap()?.path();
        let reader = File::open(&target)?;
        let mutations = FileMutations::for_file(&reader)?.unwrap();
        assert!(
            mutations.read_epoch().is_err(),
            "import output was not enrolled"
        );
        fs::rename(root.0.join("original"), root.0.join("pinned"))?;
        fs::write(root.0.join("original"), b"replacement source")?;
        copy_import(input, output)?;
        assert!(
            mutations.read_epoch().is_err(),
            "epoch ended before copy completion"
        );
        held = Some((reader, mutations));
        Ok(())
    })
    .unwrap();
    assert_eq!(fs::read(&path).unwrap(), vec![42; 192 * 1024 + 17]);
    assert_eq!(
        fs::metadata(path).unwrap().permissions().mode() & 0o777,
        0o640
    );
    assert_eq!(
        fs::read(root.0.join("original")).unwrap(),
        b"replacement source"
    );
    assert!(!root.source().exists());
    let (_reader, mutations) = held.unwrap();
    assert!(mutations.read_epoch().is_ok());
}

#[test]
fn import_copy_failure_preserves_the_source_and_removes_partial_output() {
    let root = Scratch::new();
    let mut held = None;
    let error = take_file_with(&root.source(), &root.temp(), |_, output| {
        let target = fs::read_dir(root.temp())?.next().unwrap()?.path();
        let reader = File::open(target)?;
        let mutations = FileMutations::for_file(&reader)?.unwrap();
        assert!(mutations.read_epoch().is_err());
        output.write_all(b"partial")?;
        held = Some((reader, mutations));
        Err(std::io::Error::other("injected import error"))
    })
    .unwrap_err();
    assert!(error.contains("injected import error"));
    assert!(root.source().exists());
    assert_eq!(fs::read_dir(root.temp()).unwrap().count(), 0);
    let (_reader, mutations) = held.unwrap();
    assert!(mutations.read_epoch().is_ok());
}

#[test]
fn import_copy_never_reopens_the_reserved_destination_name() {
    let root = Scratch::new();
    let mut held = None;
    let (path, _) = take_file_with(&root.source(), &root.temp(), |input, output| {
        let target = fs::read_dir(root.temp())?.next().unwrap()?.path();
        let reader = File::open(&target)?;
        let mutations = FileMutations::for_file(&reader)?.unwrap();
        assert!(mutations.read_epoch().is_err());
        fs::rename(&target, root.temp().join("pinned-output"))?;
        fs::write(&target, b"another object's payload")?;
        fs::set_permissions(&target, fs::Permissions::from_mode(0o400))?;
        copy_import(input, output)?;
        held = Some((reader, mutations));
        Ok(())
    })
    .unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"another object's payload");
    assert_eq!(
        fs::metadata(path).unwrap().permissions().mode() & 0o777,
        0o400
    );
    assert_eq!(
        fs::read(root.temp().join("pinned-output")).unwrap(),
        vec![42; 192 * 1024 + 17]
    );
    let (reader, mutations) = held.unwrap();
    let mut bytes = [0; 3];
    read_exact_at(&reader, &mut bytes, 0).unwrap();
    assert_eq!(bytes, [42; 3]);
    assert!(mutations.read_epoch().is_ok());
}

use super::*;
use shared::vfs::{file_io::FileStamp, file_mutation::FileMutations};
use std::io::Write;

fn tracked(file: &File) -> FileMutations {
    FileMutations::for_file(file).unwrap().unwrap()
}

fn archive(root: &Path, payload: &[u8]) -> PathBuf {
    let path = root.join("input.zip");
    let mut zip = zip::ZipWriter::new(File::create(&path).unwrap());
    zip.start_file(
        "entry",
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored),
    )
    .unwrap();
    zip.write_all(payload).unwrap();
    zip.finish().unwrap();
    path
}

#[test]
fn output_open_preserves_existing_payload_until_registration() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("entry");
    fs::write(&path, b"old").unwrap();
    let file = File::open(&path).unwrap();
    let mutations = tracked(&file);
    let before = mutations.read_epoch().unwrap();
    let check = mutations.clone();
    let mut tree = OutputTree::new(root.path()).unwrap();
    tree.after_native_open = Some(Box::new(move |opened| {
        assert_eq!(
            opened.metadata().unwrap().len(),
            3,
            "open truncated before registration"
        );
        assert!(check.read_epoch().is_ok());
    }));
    let (output, write) = tree.create_file(Path::new("entry")).unwrap();
    assert_eq!(output.metadata().unwrap().len(), 0);
    assert!(before.verify().is_err());
    assert!(mutations.read_epoch().is_err());
    drop(write);
    assert!(mutations.read_epoch().is_ok());
}

#[test]
fn output_guard_covers_writes_and_error_unwind_without_changing_other_objects() {
    let root = tempfile::tempdir().unwrap();
    let other = root.path().join("other");
    fs::write(&other, b"unrelated").unwrap();
    let other_file = File::open(&other).unwrap();
    let unrelated = tracked(&other_file).read_epoch().unwrap();
    let mut tree = OutputTree::new(root.path()).unwrap();
    let mut retained = None;
    let result = (|| -> io::Result<()> {
        let (mut output, _write) = tree.create_file(Path::new("entry"))?;
        let mutations = tracked(&output);
        assert!(
            mutations.read_epoch().is_err(),
            "unregistered active output"
        );
        retained = Some(mutations);
        output.write_all(b"partial")?;
        Err(io::Error::other("injected output failure"))
    })();
    assert!(result.is_err());
    assert!(
        retained.unwrap().read_epoch().is_ok(),
        "guard leaked after an error"
    );
    assert!(unrelated.verify().is_ok());
    assert_eq!(fs::read(root.path().join("entry")).unwrap(), b"partial");
}

#[test]
fn extraction_and_empty_entries_revoke_even_if_length_and_mtime_are_restored() {
    for payload in [&b"new"[..], &b""[..]] {
        let root = tempfile::tempdir().unwrap();
        let dest = root.path().join("dest");
        fs::create_dir(&dest).unwrap();
        let path = dest.join("entry");
        fs::write(
            &path,
            if payload.is_empty() {
                &b""[..]
            } else {
                &b"old"[..]
            },
        )
        .unwrap();
        let file = File::options().read(true).write(true).open(&path).unwrap();
        let modified = file.metadata().unwrap().modified().unwrap();
        let stamp = FileStamp::of(file.metadata().unwrap()).unwrap();
        let mutations = tracked(&file);
        let before = mutations.read_epoch().unwrap();
        let zip = archive(root.path(), payload);
        extract_zip_with_budget(&zip, &dest, None, ExtractBudget::default()).unwrap();
        file.set_times(fs::FileTimes::new().set_modified(modified))
            .unwrap();
        assert!(stamp.verify(&file).is_ok(), "metadata restoration failed");
        assert!(
            before.verify().is_err(),
            "extraction was invisible to the old epoch"
        );
        assert!(
            mutations.read_epoch().is_ok(),
            "completed output guard leaked"
        );
        assert_eq!(fs::read(&path).unwrap(), payload);
    }
}

#[test]
fn failed_entry_revokes_an_already_open_output_and_releases_guard() {
    let root = tempfile::tempdir().unwrap();
    let dest = root.path().join("dest");
    fs::create_dir(&dest).unwrap();
    let path = dest.join("entry");
    fs::write(&path, b"old").unwrap();
    let file = File::open(&path).unwrap();
    let mutations = tracked(&file);
    let before = mutations.read_epoch().unwrap();
    let zip = archive(root.path(), b"payload");
    let mut bytes = fs::read(&zip).unwrap();
    let central = bytes.windows(4).position(|w| w == b"PK\x01\x02").unwrap();
    // Valid layout and size; only the entry CRC is wrong, after the output
    // has been opened. Header budget rejection would not test this path.
    bytes[central + 16] ^= 1;
    fs::write(&zip, bytes).unwrap();
    assert!(extract_zip_with_budget(&zip, &dest, None, ExtractBudget::default()).is_err());
    assert!(
        before.verify().is_err(),
        "failed extraction escaped tracking"
    );
    assert!(mutations.read_epoch().is_ok());
}

#[cfg(unix)]
#[test]
fn output_rejects_nonregular_files_without_waiting_for_a_fifo_reader() {
    use std::{
        ffi::CString,
        os::unix::{
            ffi::OsStrExt,
            fs::{FileTypeExt, OpenOptionsExt},
        },
    };
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("entry");
    let name = CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: the live CString is NUL-terminated and mode is valid for mkfifo.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    let mut tree = OutputTree::new(root.path()).unwrap();
    assert!(tree.create_file(Path::new("entry")).is_err());
    let _reader = File::options()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(&path)
        .unwrap();
    let result = tree.create_file(Path::new("entry"));
    assert!(matches!(result, Err(err) if err.kind() == io::ErrorKind::InvalidInput));
    assert!(fs::symlink_metadata(path).unwrap().file_type().is_fifo());
}

#[test]
fn entry_transfer_holds_epoch_during_each_read_and_releases_it_after_read_failure() {
    struct ObservedReader {
        mutations: FileMutations,
        reads: usize,
    }
    impl Read for ObservedReader {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            assert!(
                self.mutations.read_epoch().is_err(),
                "epoch ended before entry copy"
            );
            self.reads += 1;
            if self.reads == 1 {
                buffer[0] = b'x';
                Ok(1)
            } else {
                Err(io::Error::other(
                    "injected read error after a partial write",
                ))
            }
        }
    }
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("entry");
    fs::write(&path, b"old").unwrap();
    let file = File::open(&path).unwrap();
    let mutations = tracked(&file);
    let before = mutations.read_epoch().unwrap();
    let mut reader = ObservedReader {
        mutations: mutations.clone(),
        reads: 0,
    };
    let mut tree = OutputTree::new(root.path()).unwrap();
    assert!(copy_entry(&mut tree, Path::new("entry"), &mut reader).is_err());
    assert_eq!(reader.reads, 2);
    assert_eq!(fs::read(path).unwrap(), b"x");
    assert!(before.verify().is_err());
    assert!(mutations.read_epoch().is_ok());
}

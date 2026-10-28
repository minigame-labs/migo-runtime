use super::*;
use shared::vfs::{file_mutation::FileMutations, package::PackageReader};
use std::fs::{self, File};
use std::io::{self, Cursor, SeekFrom, Write};
use std::sync::mpsc;
use std::time::Duration;

fn zip_bytes(name: &str, payload: &[u8]) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    zip.start_file(
        name,
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored),
    )
    .unwrap();
    zip.write_all(payload).unwrap();
    zip.finish().unwrap().into_inner()
}

pub(super) fn stages(dir: &Path) -> Vec<PathBuf> {
    fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.to_string_lossy().ends_with(".atomic.tmp"))
        .collect()
}

struct ObservedZip {
    bytes: Cursor<Vec<u8>>,
    data_start: u64,
    on_payload: Option<Box<dyn FnOnce() + Send>>,
}

impl Read for ObservedZip {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if !buf.is_empty()
            && self.bytes.position() == self.data_start
            && let Some(observe) = self.on_payload.take()
        {
            observe();
        }
        self.bytes.read(buf)
    }
}

impl Seek for ObservedZip {
    fn seek(&mut self, offset: SeekFrom) -> io::Result<u64> {
        self.bytes.seek(offset)
    }
}

fn observed_archive(
    bytes: Vec<u8>,
    observe: impl FnOnce() + Send + 'static,
) -> zip::ZipArchive<ObservedZip> {
    let data_start = zip::ZipArchive::new(Cursor::new(&bytes))
        .unwrap()
        .by_index(0)
        .unwrap()
        .data_start()
        .unwrap();
    zip::ZipArchive::new(ObservedZip {
        bytes: Cursor::new(bytes),
        data_start,
        on_payload: Some(Box::new(observe)),
    })
    .unwrap()
}

#[test]
fn concurrent_ingests_keep_independent_tracked_stages_until_complete_publication() {
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("game.mpkg");
    fs::write(&output, b"previous package").unwrap();
    let old_file = File::open(&output).unwrap();
    let old_mutations = FileMutations::for_file(&old_file).unwrap().unwrap();
    let old_epoch = old_mutations.read_epoch().unwrap();
    let mut writers = Vec::new();
    for byte in [b'a', b'b'] {
        let (ready_tx, ready_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        let target = output.clone();
        let payload = vec![byte; 768 * 1024 + 17];
        let archive = observed_archive(zip_bytes("audio.bin", &payload), move || {
            ready_tx.send(()).unwrap();
            resume_rx.recv_timeout(Duration::from_secs(20)).unwrap();
        });
        let worker = std::thread::spawn(move || {
            ingest_archive_to_package(
                archive,
                &target,
                "game",
                "1",
                ExtractBudget::default(),
                GpuCapsSnapshot::default(),
            )
        });
        writers.push((ready_rx, resume_tx, worker, payload));
    }
    for (ready, _, _, _) in &writers {
        ready.recv_timeout(Duration::from_secs(10)).unwrap();
    }
    let temporary = stages(dir.path());
    assert_eq!(temporary.len(), 2, "ingests must own different stages");
    let handles: Vec<_> = temporary.iter().map(|p| File::open(p).unwrap()).collect();
    let mutations: Vec<_> = handles
        .iter()
        .map(|file| FileMutations::for_file(file).unwrap().unwrap())
        .collect();
    assert_ne!(mutations[0].identity(), mutations[1].identity());
    assert!(mutations.iter().all(|m| m.read_epoch().is_err()));
    assert_eq!(fs::read(&output).unwrap(), b"previous package");

    // Publish in a known order while the other real ingest is paused. Each
    // returned identity must belong to its complete, readable package.
    for (_, resume, worker, payload) in writers {
        resume.send(()).unwrap();
        let identity = worker.join().unwrap().unwrap();
        let reader = PackageReader::open(&output, "game", "1").unwrap();
        assert_eq!(reader.identity(), &identity);
        assert_eq!(reader.read_entry("audio.bin").unwrap(), payload);
    }
    assert!(stages(dir.path()).is_empty());
    assert!(mutations.iter().all(|m| m.read_epoch().is_ok()));
    assert!(
        old_epoch.verify().is_ok(),
        "publication must not edit the old object"
    );
}

#[test]
fn public_ingest_leaves_an_existing_legacy_temporary_object_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.zip");
    let output = dir.path().join("game.mpkg");
    let legacy = output.with_extension("mpkg.tmp");
    fs::write(&input, zip_bytes("main.js", b"game payload")).unwrap();
    fs::write(&legacy, b"another owner").unwrap();
    let legacy_file = File::open(&legacy).unwrap();
    let legacy_mutations = FileMutations::for_file(&legacy_file).unwrap().unwrap();
    let epoch = legacy_mutations.read_epoch().unwrap();
    let identity =
        ingest_zip_to_package(&input, &output, "game", "2", GpuCapsSnapshot::default()).unwrap();
    let reader = PackageReader::open(&output, "game", "2").unwrap();
    assert_eq!(reader.identity(), &identity);
    assert_eq!(reader.read_entry("main.js").unwrap(), b"game payload");
    assert_eq!(fs::read(&legacy).unwrap(), b"another owner");
    assert!(epoch.verify().is_ok());
    assert!(stages(dir.path()).is_empty());
}

#[test]
fn corrupted_zip_after_partial_transfer_keeps_old_output_and_removes_stage() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.zip");
    let output = dir.path().join("game.mpkg");
    let payload = vec![42; 768 * 1024 + 17];
    let mut bytes = zip_bytes("audio.bin", &payload);
    let start = zip::ZipArchive::new(Cursor::new(&bytes))
        .unwrap()
        .by_index(0)
        .unwrap()
        .data_start()
        .unwrap() as usize;
    bytes[start + payload.len() - 1] ^= 1;
    fs::write(&input, bytes).unwrap();
    fs::write(&output, b"previous package").unwrap();
    let error = ingest_zip_to_package(&input, &output, "game", "1", GpuCapsSnapshot::default())
        .unwrap_err();
    assert!(matches!(error, PackageError::Io(_)), "{error:?}");
    assert_eq!(fs::read(&output).unwrap(), b"previous package");
    assert!(
        stages(dir.path()).is_empty(),
        "failed ingest leaked a stage"
    );
}

#[test]
fn invalid_entry_path_preserves_package_error_type_and_previous_output() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.zip");
    let output = dir.path().join("game.mpkg");
    fs::write(&input, zip_bytes("../outside", b"invalid")).unwrap();
    fs::write(&output, b"previous package").unwrap();
    let error = ingest_zip_to_package(&input, &output, "game", "1", GpuCapsSnapshot::default())
        .unwrap_err();
    assert!(
        matches!(error, PackageError::InvalidEntryPath(_)),
        "{error:?}"
    );
    assert_eq!(fs::read(&output).unwrap(), b"previous package");
    assert!(stages(dir.path()).is_empty());
}

struct TrackedWriter<'a> {
    file: &'a mut File,
    mutations: &'a FileMutations,
    calls: &'a mut [usize; 3],
}

impl Write for TrackedWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        assert!(
            self.mutations.read_epoch().is_err(),
            "untracked package write"
        );
        self.calls[0] += 1;
        self.file.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        assert!(
            self.mutations.read_epoch().is_err(),
            "untracked buffer flush"
        );
        self.calls[1] += 1;
        self.file.flush()
    }
}

impl Seek for TrackedWriter<'_> {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        assert!(
            self.mutations.read_epoch().is_err(),
            "untracked header fixup"
        );
        self.calls[2] += 1;
        self.file.seek(position)
    }
}

#[test]
fn typed_atomic_writer_tracks_package_flush_and_header_fixup_then_returns_identity() {
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("game.mpkg");
    let mut calls = [0; 3];
    let identity = crate::atomic_write::atomic_write_with_result(&output, |file| {
        let mutations = FileMutations::for_file(file)?.unwrap();
        let mut writer = PackageWriter::new(std::io::BufWriter::new(TrackedWriter {
            file,
            mutations: &mutations,
            calls: &mut calls,
        }))?;
        writer.add_entry("main.js", b"game payload")?;
        writer.finish("game", "3")
    })
    .unwrap();
    assert!(calls.iter().all(|count| *count > 0));
    let reader = PackageReader::open(&output, "game", "3").unwrap();
    assert_eq!(reader.identity(), &identity);
    assert_eq!(reader.read_entry("main.js").unwrap(), b"game payload");
    assert!(stages(dir.path()).is_empty());
}

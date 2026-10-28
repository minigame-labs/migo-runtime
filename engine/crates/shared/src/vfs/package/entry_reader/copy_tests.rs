use super::*;
use std::fs::File;
use std::sync::atomic::{AtomicU64, Ordering};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture {
    path: PathBuf,
    reader: PackageReader,
    bytes: Vec<u8>,
}
impl Fixture {
    fn new(len: usize) -> Self {
        let path = std::env::temp_dir().join(format!(
            "migo-copy-entry-{}-{}.mpkg",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let bytes = (0..len).map(|i| (i % 251) as u8).collect::<Vec<_>>();
        let mut writer =
            PackageWriter::with_chunk_size(File::create(&path).unwrap(), 4096).unwrap();
        writer.add_entry("clip", &bytes).unwrap();
        writer.finish("game", "1").unwrap();
        let reader = PackageReader::open(&path, "game", "1").unwrap();
        Self {
            path,
            reader,
            bytes,
        }
    }
    fn source(&self) -> PackageEntryReader {
        self.reader
            .entry_reader("clip", Default::default())
            .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}
#[derive(Default)]
struct Sink {
    count: usize,
    crc: crc32fast::Hasher,
}
impl Write for Sink {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.count += bytes.len();
        self.crc.update(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
#[test]
fn whole_copy_equals_original_including_empty_and_multichunk_entries() {
    for size in [0, 1, 4096, 8199, 1_048_649] {
        let fixture = Fixture::new(size);
        let mut source = fixture.source();
        let mut sink = Vec::new();
        assert_eq!(source.copy_to(&mut sink).unwrap(), size as u64);
        assert_eq!(sink, fixture.bytes);
        assert!(source.scratch_bytes() <= zstd::zstd_safe::compress_bound(4096) + 4096);
        let mut one = [0; 1];
        if size > 0 {
            source.read_exact_at(0, &mut one).unwrap();
            assert_eq!(one[0], fixture.bytes[0]);
        }
        let mut again = Sink::default();
        assert_eq!(source.copy_to(&mut again).unwrap(), size as u64);
        assert_eq!(again.crc.finalize(), crc32fast::hash(&fixture.bytes));
    }
}
#[test]
fn crc_mismatch_poison_is_sticky_even_after_all_bytes_were_written() {
    let fixture = Fixture::new(8199);
    let mut source = fixture.source();
    source.entry.crc32 ^= 1;
    assert!(matches!(
        source.copy_to(&mut Sink::default()),
        Err(PackageError::ChecksumMismatch { .. })
    ));
    assert!(source.copy_to(&mut Sink::default()).is_err());
    assert!(source.read_exact_at(0, &mut [0; 1]).is_err());
}
#[test]
fn failed_partial_writer_poison_prevents_read_or_copy_reuse() {
    struct Partial {
        calls: usize,
    }
    impl Write for Partial {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.calls += 1;
            if self.calls == 1 {
                Ok(bytes.len().min(7))
            } else {
                Err(io::Error::other("partial sink failure"))
            }
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let fixture = Fixture::new(8199);
    let mut source = fixture.source();
    let mut sink = Partial { calls: 0 };
    assert!(source.copy_to(&mut sink).is_err());
    assert_eq!(sink.calls, 2);
    assert!(source.copy_to(&mut Sink::default()).is_err());
    assert!(source.read_exact_at(0, &mut [0; 1]).is_err());
}
#[test]
fn mutation_after_first_decoded_chunk_fails_final_source_verification() {
    struct Mutating {
        file: File,
        once: bool,
    }
    impl Write for Mutating {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if !self.once {
                self.once = true;
                let changed = self.file.metadata()?.modified()? + std::time::Duration::from_secs(2);
                self.file
                    .set_times(std::fs::FileTimes::new().set_modified(changed))?;
            }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let fixture = Fixture::new(8199);
    let mut source = fixture.source();
    let file = File::options().write(true).open(&fixture.path).unwrap();
    assert!(source.copy_to(&mut Mutating { file, once: false }).is_err());
    assert!(source.copy_to(&mut Sink::default()).is_err());
}
#[test]
fn streaming_scratch_has_no_entry_sized_or_steady_rust_heap_allocation() {
    let fixture = Fixture::new(2_097_225);
    let mut source = fixture.source();
    let scratch = source.scratch_bytes();
    migo_alloc_probe::assert_no_steady_state_allocation(
        migo_alloc_probe::Burst {
            path: "package complete streaming copy",
            warmup: 2,
            measured: 8,
        },
        |_| {
            let mut sink = Sink::default();
            source.copy_to(&mut sink).unwrap();
            assert_eq!(sink.count, fixture.bytes.len());
            assert_eq!(sink.crc.finalize(), crc32fast::hash(&fixture.bytes));
        },
    );
    assert_eq!(source.scratch_bytes(), scratch);
    assert!(scratch < 16 * 1024);
}

#[test]
fn mutation_before_copy_is_rejected_without_writing_any_output() {
    let fixture = Fixture::new(8199);
    let mut source = fixture.source();
    let file = File::options().write(true).open(&fixture.path).unwrap();
    let changed = file.metadata().unwrap().modified().unwrap() + std::time::Duration::from_secs(2);
    file.set_times(std::fs::FileTimes::new().set_modified(changed))
        .unwrap();
    let mut sink = Sink::default();
    assert!(source.copy_to(&mut sink).is_err());
    assert_eq!(sink.count, 0);
}

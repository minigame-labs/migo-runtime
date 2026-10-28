use super::*;
use shared::vfs::{MountBackend, MountTable, PackSource, PackageWriter};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};
static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Fixture {
    path: PathBuf,
    data: Vec<u8>,
}
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "migo-package-io-{}-{}.mpkg",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let data = (0..3 * FILE_CHUNK_BYTES + 37)
            .map(|n| ((n * 137 + n / 11) % 251) as u8)
            .collect::<Vec<_>>();
        let mut writer = PackageWriter::new(std::fs::File::create(&path).unwrap()).unwrap();
        writer.add_entry("clip", &data).unwrap();
        writer.finish("game", "1").unwrap();
        Self { path, data }
    }
    fn pin(&self) -> PinnedPackRead {
        let mt = MountTable::new(self.path.parent().unwrap().to_path_buf());
        mt.swap_base(Arc::new(PackSource::open(&self.path, "game", "1").unwrap()));
        mt.pin_pack_read("clip", mt.generation()).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}
fn budget() -> u64 {
    ReadAtPackage::reservation_bytes(Default::default()).unwrap()
}

#[test]
fn persistent_credit_lasts_until_the_final_media_chunk_is_dropped() {
    let f = Fixture::new();
    let pools = IoPools::with_byte_budget(budget());
    let reader = ReadAtPackage::new(f.pin(), &pools, 2 * 1024 * 1024).unwrap();
    assert!(pools.reserve_bytes(1).is_err());
    let chunk = reader.chunk_at(0, 8).unwrap();
    drop(reader);
    assert!(
        pools.reserve_bytes(1).is_err(),
        "credit returned while media chunk is retained"
    );
    drop(chunk);
    assert!(
        pools.reserve_bytes(budget()).is_ok(),
        "persistent credit leaked"
    );
}
#[test]
fn exhausted_budget_rejects_before_opening_or_allocating_package_scratch() {
    #[derive(Debug)]
    struct Probe(Arc<AtomicBool>);
    impl MountBackend for Probe {
        fn read(&self, _: &str) -> io::Result<Vec<u8>> {
            panic!("whole-file fallback")
        }
        fn exists(&self, _: &str) -> bool {
            true
        }
        fn is_file(&self, _: &str) -> bool {
            true
        }
        fn real_path(&self, _: &str) -> Option<PathBuf> {
            None
        }
        fn root_dir(&self) -> Option<&Path> {
            None
        }
        fn open_package_entry(
            &self,
            _: &str,
            _: PackageReadLimits,
        ) -> io::Result<PackageEntryReader> {
            self.0.store(true, Ordering::Release);
            Err(io::Error::other("open was reached"))
        }
    }
    let opened = Arc::new(AtomicBool::new(false));
    let mt = MountTable::new(std::env::temp_dir());
    mt.swap_base(Arc::new(Probe(opened.clone())));
    let pools = IoPools::with_byte_budget(budget() - 1);
    assert!(
        ReadAtPackage::new(
            mt.pin_pack_read("clip", mt.generation()).unwrap(),
            &pools,
            128
        )
        .is_err()
    );
    assert!(
        !opened.load(Ordering::Acquire),
        "budget failure still opened package scratch"
    );
    assert!(pools.reserve_bytes(budget() - 1).is_ok());
}
#[test]
fn package_media_uses_the_same_two_recycled_bounded_slots() {
    let f = Fixture::new();
    let pools = IoPools::with_byte_budget(budget());
    let reader = ReadAtPackage::new(f.pin(), &pools, 128).unwrap();
    assert_eq!(reader.len(), f.data.len() as u64);
    assert!(!reader.is_empty());
    assert_eq!(reader.read_exact_at(31, 128).unwrap(), f.data[31..159]);
    assert!(reader.read_exact_at(0, 129).is_err());
    assert!(reader.chunk_at(u64::MAX, 8).is_err());
    assert!(reader.chunk_at(0, FILE_CHUNK_BYTES + 1).is_err());
    let a = reader.chunk_at(0, FILE_CHUNK_BYTES).unwrap();
    let b = reader
        .chunk_at(FILE_CHUNK_BYTES as u64, FILE_CHUNK_BYTES)
        .unwrap();
    assert_eq!(a.as_ref(), &f.data[..FILE_CHUNK_BYTES]);
    assert_eq!(b.as_ref(), &f.data[FILE_CHUNK_BYTES..2 * FILE_CHUNK_BYTES]);
    assert_eq!(
        reader.chunk_at(0, 8).err().unwrap().kind(),
        io::ErrorKind::WouldBlock
    );
    let pointer = a.as_ref().as_ptr();
    drop(a);
    let c = reader.chunk_at(17, 33).unwrap();
    assert_eq!(pointer, c.as_ref().as_ptr());
    assert_eq!(c.as_ref(), &f.data[17..50]);
    drop(b);
    drop(c);
    assert!(
        reader
            .chunk_at(reader.len(), 0)
            .unwrap()
            .as_ref()
            .is_empty()
    );
}
#[test]
fn package_chunk_pool_has_no_steady_rust_heap_allocation() {
    let f = Fixture::new();
    let pools = IoPools::with_byte_budget(budget());
    let reader = ReadAtPackage::new(f.pin(), &pools, 128).unwrap();
    let a = reader.chunk_at(0, 128).unwrap();
    let b = reader.chunk_at(128, 128).unwrap();
    drop(a);
    drop(b);
    migo_alloc_probe::assert_no_steady_state_allocation(
        migo_alloc_probe::Burst {
            path: "package pooled chunks",
            warmup: 8,
            measured: 96,
        },
        |n| {
            let at = (n * 4073) % (f.data.len() - 777);
            let chunk = reader.chunk_at(at as u64, 777).unwrap();
            assert_eq!(chunk.as_ref(), &f.data[at..at + 777]);
        },
    );
}
#[test]
fn mutation_after_media_is_rejected_including_verified_eof() {
    let f = Fixture::new();
    let pools = IoPools::with_byte_budget(budget());
    let reader = ReadAtPackage::new(f.pin(), &pools, 128).unwrap();
    drop(reader.chunk_at(0, 8).unwrap());
    let file = std::fs::File::options().write(true).open(&f.path).unwrap();
    file.set_len(file.metadata().unwrap().len() - 1).unwrap();
    assert!(reader.chunk_at(reader.len(), 0).is_err());
    assert!(reader.chunk_at(0, 8).is_err());
}

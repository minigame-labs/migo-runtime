use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "migo-pack-range-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn bytes(len: usize) -> Vec<u8> {
    (0..len).map(|i| ((i * 137 + i / 11) % 251) as u8).collect()
}
fn write(path: &Path, data: &[u8], chunk: u32) {
    let file = std::fs::File::create(path).unwrap();
    let mut w = PackageWriter::with_chunk_size(io::BufWriter::new(file), chunk).unwrap();
    w.add_entry("clip", data).unwrap();
    w.add_entry("empty", &[]).unwrap();
    w.finish("game", "1").unwrap();
}
fn fixture(data: &[u8], chunk: u32) -> (Temp, PathBuf, PackageReader) {
    let temp = Temp::new();
    let path = temp.0.join("asset.mpkg");
    write(&path, data, chunk);
    let reader = PackageReader::open(&path, "game", "1").unwrap();
    (temp, path, reader)
}
fn source(reader: &PackageReader) -> PackageEntryReader {
    reader
        .entry_reader("clip", PackageReadLimits::default())
        .unwrap()
}

#[test]
fn positioned_ranges_share_index_and_equal_original_bytes() {
    for chunk in [1024, DEFAULT_CHUNK_SIZE, MAX_CHUNK_RAW_SIZE] {
        let data = bytes(chunk as usize * 5 + 73);
        let (_temp, _path, reader) = fixture(&data, chunk);
        let mut s = source(&reader);
        assert!(
            s.scratch_bytes() <= zstd::zstd_safe::compress_bound(chunk as usize) + chunk as usize
        );
        for (at, len) in [
            (0, 17),
            (chunk as usize - 7, 33),
            (data.len() - 1, 1),
            (1, 0),
            (31, 777),
            (0, data.len()),
            (data.len(), 0),
        ] {
            let mut out = vec![0; len];
            s.read_exact_at(at as u64, &mut out).unwrap();
            assert_eq!(out, data[at..at + len]);
        }
        assert_eq!(reader.read_entry("clip").unwrap(), data);
        assert_eq!(
            reader.read_range("clip", 17, Some(71)).unwrap(),
            data[17..88]
        );
        let mut empty = reader
            .entry_reader("empty", PackageReadLimits::default())
            .unwrap();
        assert!(empty.is_empty());
        empty.read_exact_at(0, &mut []).unwrap();
        assert_eq!(empty.scratch_bytes(), 0);
    }
}
#[test]
fn scratch_limits_reject_before_payload_or_decoder_work() {
    let (_temp, _path, reader) = fixture(&bytes(20000), 1024);
    for limits in [
        PackageReadLimits {
            max_encoded_chunk: 1,
            ..Default::default()
        },
        PackageReadLimits {
            max_decoded_chunk: 1,
            ..Default::default()
        },
        PackageReadLimits {
            max_encoded_chunk: 0,
            ..Default::default()
        },
        PackageReadLimits {
            max_encoded_chunk: usize::MAX,
            ..Default::default()
        },
        PackageReadLimits {
            max_decoded_chunk: MAX_CHUNK_RAW_SIZE as usize + 1,
            ..Default::default()
        },
    ] {
        assert!(
            matches!(reader.entry_reader("clip",limits),Err(PackageError::Io(e)) if e.kind()==io::ErrorKind::InvalidInput)
        );
    }
    assert!(reader.entry_reader("../clip", Default::default()).is_err());
}
#[test]
fn bad_extents_are_sticky_and_never_decode_a_payload() {
    let (_temp, _path, reader) = fixture(&bytes(20000), 1024);
    for offset in [u64::MAX, 19999, 20001] {
        let mut s = source(&reader);
        assert!(s.read_exact_at(offset, &mut [0; 8]).is_err());
        assert_eq!(s.decodes, 0);
        assert!(s.read_exact_at(0, &mut [0; 8]).is_err());
    }
}
#[test]
fn subranges_reuse_the_last_decompressed_chunk() {
    let data = bytes(20000);
    let (_temp, _path, reader) = fixture(&data, 4096);
    let mut s = source(&reader);
    for offset in [0, 37, 91, 3000, 17] {
        let mut out = [0; 111];
        s.read_exact_at(offset, &mut out).unwrap();
        assert_eq!(out, data[offset as usize..offset as usize + 111]);
    }
    assert_eq!(
        s.decodes, 1,
        "subranges decompressed the same chunk repeatedly"
    );
    s.read_exact_at(4096, &mut [0; 8]).unwrap();
    assert_eq!(s.decodes, 2);
}
#[test]
fn mutation_before_cached_read_or_after_fill_never_publishes_success() {
    let data = bytes(5000);
    for after in [false, true] {
        let (_temp, path, reader) = fixture(&data, 1024);
        let mut s = source(&reader);
        s.read_exact_at(0, &mut [0; 8]).unwrap();
        assert_eq!(s.decodes, 1);
        let file = std::fs::File::options().write(true).open(&path).unwrap();
        let len = file.metadata().unwrap().len();
        if after {
            s.after_read = Some(Box::new(move || file.set_len(len - 1).unwrap()));
        } else {
            file.set_len(len - 1).unwrap();
        }
        let mut output = [0xa5; 8];
        assert!(
            s.read_exact_at(9, &mut output).is_err(),
            "changed cached package became success"
        );
        if !after {
            assert_eq!(
                output, [0xa5; 8],
                "known mutation still copied cached bytes"
            );
        }
        assert_eq!(s.decodes, 1);
        assert!(s.read_exact_at(0, &mut [0; 8]).is_err());
        assert!(reader.read_range("clip", 0, Some(8)).is_err());
    }
}
#[cfg(unix)]
#[test]
fn path_replacement_keeps_ranges_digest_and_crc_on_the_open_file() {
    let a = bytes(5000);
    let (_temp, path, reader) = fixture(&a, 1024);
    let pack = PackSource::open(&path, "game", "1").unwrap();
    std::fs::rename(&path, path.with_extension("old")).unwrap();
    write(&path, &vec![41; a.len()], 1024);
    let mut out = vec![0; a.len()];
    source(&reader).read_exact_at(0, &mut out).unwrap();
    assert_eq!(out, a);
    assert_eq!(reader.read_entry("clip").unwrap(), a);
    let (len, hash) = reader.get_file_info("clip", "sha256").unwrap();
    assert_eq!(len, a.len() as u64);
    assert_eq!(hash, hex::encode(sha2::Sha256::digest(&a)));
    let other = PackSource::open(&path, "game", "1").unwrap();
    use crate::vfs::MountBackend;
    assert_ne!(pack.source_identity(), other.source_identity());
    let mut s = pack.open_package_entry("clip", Default::default()).unwrap();
    s.read_exact_at(0, &mut out).unwrap();
    assert_eq!(out, a);
}
fn replace_frame(path: &Path, frame: &[u8]) {
    let old = std::fs::read(path).unwrap();
    let table = u64::from_le_bytes(old[16..24].try_into().unwrap()) as usize;
    let index = u64::from_le_bytes(old[24..32].try_into().unwrap()) as usize;
    let mut new = old[..HEADER_SIZE as usize].to_vec();
    new.extend_from_slice(frame);
    let next = new.len();
    new.extend_from_slice(&old[table..]);
    new[16..24].copy_from_slice(&(next as u64).to_le_bytes());
    new[24..32].copy_from_slice(&((next + index - table) as u64).to_le_bytes());
    new[next + 8..next + 12].copy_from_slice(&(frame.len() as u32).to_le_bytes());
    std::fs::write(path, new).unwrap();
}
#[test]
fn short_decoded_chunk_cannot_expose_initialized_scratch_tail() {
    let (_temp, path, reader) = fixture(&vec![0; 1024], 1024);
    drop(reader);
    replace_frame(&path, &zstd::bulk::compress(&vec![0; 1023], 3).unwrap());
    let reader = PackageReader::open(&path, "game", "1").unwrap();
    let mut s = source(&reader);
    assert!(matches!(
        s.read_exact_at(0, &mut [0; 1024]),
        Err(PackageError::DecompressSizeMismatch { .. })
    ));
    assert!(s.read_exact_at(0, &mut [0; 8]).is_err());
}
#[test]
fn trailing_zstd_frames_are_not_a_single_package_chunk() {
    let (_temp, path, reader) = fixture(&vec![0; 1024], 1024);
    drop(reader);
    let mut frame = zstd::bulk::compress(&vec![0; 1024], 3).unwrap();
    frame.extend_from_slice(&zstd::bulk::compress(&[], 3).unwrap());
    // The underlying one-shot decoder accepts this concatenation; our format
    // boundary must reject it rather than rely on an incidental codec error.
    assert_eq!(
        zstd::bulk::Decompressor::new()
            .unwrap()
            .decompress_to_buffer(&frame, &mut [0; 1024][..])
            .unwrap(),
        1024
    );
    replace_frame(&path, &frame);
    let reader = PackageReader::open(&path, "game", "1").unwrap();
    let mut s = source(&reader);
    assert!(s.read_exact_at(0, &mut [0; 1024]).is_err());
    assert!(s.read_exact_at(0, &mut [0; 8]).is_err());
}
#[test]
fn range_steps_reuse_scratch_without_steady_rust_heap_allocations() {
    let data = bytes(32768 + 73);
    let (_temp, _path, reader) = fixture(&data, 4096);
    let mut s = source(&reader);
    let mut out = [0; 777];
    migo_alloc_probe::assert_no_steady_state_allocation(
        migo_alloc_probe::Burst {
            path: "package positioned ranges",
            warmup: 8,
            measured: 96,
        },
        |n| {
            let offset = (n * 4073) % (data.len() - out.len());
            s.read_exact_at(offset as u64, &mut out).unwrap();
            assert_eq!(out, data[offset..offset + 777]);
        },
    );
}
#[test]
fn admitted_package_backend_survives_mount_replacement_and_rejects_stale_admission() {
    use crate::vfs::{MountBackend, MountTable};
    let a = bytes(5000);
    let (ta, pa, _reader) = fixture(&a, 1024);
    let (_tb, pb, _reader) = fixture(&vec![41; a.len()], 1024);
    let mt = MountTable::new(ta.0.clone());
    mt.swap_base(Arc::new(PackSource::open(&pa, "game", "1").unwrap()));
    let generation = mt.generation();
    let admitted = mt.pin_pack_read("clip", generation).unwrap();
    mt.swap_base(Arc::new(PackSource::open(&pb, "game", "2").unwrap()));
    assert!(mt.pin_pack_read("clip", generation).is_err());
    let mut old = admitted.open_reader(Default::default()).unwrap();
    let mut out = vec![0; a.len()];
    old.read_exact_at(0, &mut out).unwrap();
    assert_eq!(out, a);
    let new = mt.pin_pack_read("clip", mt.generation()).unwrap();
    new.open_reader(Default::default())
        .unwrap()
        .read_exact_at(0, &mut out)
        .unwrap();
    assert_eq!(out, vec![41; a.len()]);
    mt.mount_overlay(
        "sub".into(),
        "sub".into(),
        Arc::new(PackSource::open(&pa, "game", "1").unwrap()),
    );
    let overlay = mt.pin_pack_read("sub/clip", mt.generation()).unwrap();
    overlay
        .open_reader(Default::default())
        .unwrap()
        .read_exact_at(0, &mut out)
        .unwrap();
    assert_eq!(out, a);
    // A generic backend must not silently inflate/copy an entire file to emulate ranges.
    #[derive(Debug)]
    struct WholeOnly;
    impl MountBackend for WholeOnly {
        fn read(&self, _: &str) -> io::Result<Vec<u8>> {
            panic!("whole-file fallback executed")
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
    }
    mt.swap_base(Arc::new(WholeOnly));
    let pin = mt.pin_pack_read("clip", mt.generation()).unwrap();
    assert!(
        matches!(pin.open_reader(Default::default()),Err(e)if e.kind()==io::ErrorKind::Unsupported)
    );
}

#[test]
fn package_chunk_offsets_above_u32_are_read_without_truncation() {
    let temp = Temp::new();
    let path = temp.0.join("sparse.mpkg");
    let mut file = std::fs::File::create(&path).unwrap();
    let data = b"positioned package";
    let frame = zstd::bulk::compress(data, 3).unwrap();
    let offset = (8_u64 << 30) + 17;
    let table = offset + frame.len() as u64;
    Header {
        entry_count: 1,
        chunk_count: 1,
        chunk_table_offset: table,
        index_offset: table + 16,
    }
    .write_to(&mut file)
    .unwrap();
    file.seek(SeekFrom::Start(offset)).unwrap();
    file.write_all(&frame).unwrap();
    file.write_all(&offset.to_le_bytes()).unwrap();
    file.write_all(&(frame.len() as u32).to_le_bytes()).unwrap();
    file.write_all(&(data.len() as u32).to_le_bytes()).unwrap();
    file.write_all(&4_u16.to_le_bytes()).unwrap();
    file.write_all(b"clip").unwrap();
    file.write_all(&(data.len() as u64).to_le_bytes()).unwrap();
    file.write_all(&crc32fast::hash(data).to_le_bytes())
        .unwrap();
    file.write_all(&0_u32.to_le_bytes()).unwrap();
    file.write_all(&1_u32.to_le_bytes()).unwrap();
    drop(file);
    let reader = PackageReader::open(&path, "game", "1").unwrap();
    let mut out = [0; 18];
    source(&reader).read_exact_at(0, &mut out).unwrap();
    assert_eq!(&out, data);
}

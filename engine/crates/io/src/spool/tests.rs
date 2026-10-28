use super::*;
#[test]
fn exact_unknown_length_and_pooled_reads_preserve_bytes_and_quota() {
    let root = tempfile::tempdir().unwrap();
    let budget = SpoolBudget::new(200000, 4).unwrap();
    let mut writer = SpoolWriter::create(root.path(), &budget, 100000, None).unwrap();
    assert_eq!(budget.usage(), (100000, 1));
    for _ in 0..4 {
        writer.append(&[9; 16384]).unwrap();
    }
    let sealed = writer.seal(100000).unwrap();
    assert_eq!(budget.usage(), (65536, 1));
    assert_eq!(
        sealed.reader().read_exact_at(100, 1000).unwrap(),
        vec![9; 1000]
    );
    let chunk = sealed.reader().chunk_at(0, 65536).unwrap();
    assert!(chunk.as_ref().iter().all(|v| *v == 9));
    drop(sealed);
    assert_eq!(budget.usage(), (0, 0));
    assert_eq!(chunk.as_ref().len(), 65536);
}
#[test]
fn quota_and_header_limit_reject_before_filesystem_work() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("absent");
    let budget = SpoolBudget::new(8, 4).unwrap();
    assert!(SpoolWriter::create(Path::new("."), &budget, 8, None).is_err());
    assert!(SpoolWriter::create(&dir, &SpoolBudget::new(16, 1).unwrap(), 8, Some(9)).is_err());
    assert!(!dir.exists());
    let held = SpoolWriter::create(root.path(), &budget, 8, None).unwrap();
    assert!(SpoolWriter::create(&dir, &budget, 1, None).is_err());
    assert!(!dir.exists());
    drop(held);
    assert_eq!(budget.usage(), (0, 0));
}
#[test]
fn size_chunk_and_partial_write_failures_are_sticky_and_never_seal() {
    let root = tempfile::tempdir().unwrap();
    let budget = SpoolBudget::new(400000, 8).unwrap();
    let mut writer = SpoolWriter::create(root.path(), &budget, 8, None).unwrap();
    writer.append(b"12345678").unwrap();
    assert!(writer.append(b"9").is_err());
    assert!(writer.append(b"").is_err());
    assert!(writer.seal(8).is_err());
    let mut writer = SpoolWriter::create(root.path(), &budget, 200000, None).unwrap();
    assert!(writer.append(&vec![0; FILE_CHUNK_BYTES + 1]).is_err());
    assert!(writer.seal(8).is_err());
    let mut writer = SpoolWriter::create(root.path(), &budget, 8, Some(8)).unwrap();
    writer.append(b"short").unwrap();
    assert!(writer.seal(8).is_err());
    let path = root.path().join("readonly");
    std::fs::write(&path, b"x").unwrap();
    let mut writer = SpoolWriter::create(root.path(), &budget, 8, None).unwrap();
    writer.file = File::open(&path).unwrap();
    assert!(writer.append(b"x").is_err());
    writer.file = tempfile::tempfile_in(root.path()).unwrap();
    assert!(writer.append(b"").is_err());
    assert!(writer.seal(8).is_err());
    assert_eq!(budget.usage(), (0, 0));
}
#[test]
fn empty_spools_still_hold_a_file_slot_until_the_last_reader_owner() {
    let root = tempfile::tempdir().unwrap();
    let budget = SpoolBudget::new(8, 1).unwrap();
    let reader = Arc::new(
        SpoolWriter::create(root.path(), &budget, 8, Some(0))
            .unwrap()
            .seal(8)
            .unwrap(),
    );
    let job = reader.clone();
    drop(reader);
    assert_eq!(budget.usage(), (0, 1));
    assert!(SpoolWriter::create(root.path(), &budget, 8, Some(0)).is_err());
    drop(job);
    assert_eq!(budget.usage(), (0, 0));
}
#[test]
fn failed_create_and_seal_release_reservations_and_native_files() {
    let root = tempfile::tempdir().unwrap();
    let budget = SpoolBudget::new(8, 2).unwrap();
    let file = root.path().join("file");
    std::fs::write(&file, b"occupied").unwrap();
    assert!(SpoolWriter::create(&file, &budget, 8, None).is_err());
    assert_eq!(budget.usage(), (0, 0));
    assert!(
        SpoolWriter::create(root.path(), &budget, 8, None)
            .unwrap()
            .seal(0)
            .is_err()
    );
    assert_eq!(budget.usage(), (0, 0));
    #[cfg(unix)]
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
}
#[test]
fn concurrent_reservations_cannot_overbook_the_process_ledger() {
    let root = tempfile::tempdir().unwrap();
    let budget = SpoolBudget::new(8, 2).unwrap();
    let gate = Arc::new(std::sync::Barrier::new(8));
    let jobs: Vec<_> = (0..8)
        .map(|_| {
            let budget = budget.clone();
            let gate = gate.clone();
            let dir = root.path().to_path_buf();
            std::thread::spawn(move || {
                let writer = SpoolWriter::create(&dir, &budget, 4, None);
                gate.wait();
                writer
            })
        })
        .collect();
    let writers: Vec<_> = jobs.into_iter().map(|j| j.join().unwrap()).collect();
    assert_eq!(writers.iter().filter(|r| r.is_ok()).count(), 2);
    assert_eq!(budget.usage(), (8, 2));
    drop(writers);
    assert_eq!(budget.usage(), (0, 0));
}

#[test]
fn checked_lengths_reject_overflow_before_native_write_or_quota_change() {
    let root = tempfile::tempdir().unwrap();
    let budget = SpoolBudget::new(u64::MAX, 2).unwrap();
    let mut writer = SpoolWriter::create(root.path(), &budget, 8, None).unwrap();
    writer.written = u64::MAX;
    assert!(writer.append(b"x").is_err());
    drop(writer);
    assert_eq!(budget.usage(), (0, 0));
    let mut held = budget.reserve(u64::MAX).unwrap();
    assert!(budget.reserve(1).is_err());
    held.shrink_to(0);
    assert_eq!(budget.usage(), (0, 1));
    drop(held);
    assert_eq!(budget.usage(), (0, 0));
}

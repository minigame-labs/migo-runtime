use super::*;
use std::{
    fs::OpenOptions,
    io::{Seek, SeekFrom, Write},
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Temp(PathBuf);
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn fixture() -> (Temp, File) {
    let root = std::env::temp_dir().join(format!(
        "migo-mutation-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&root).unwrap();
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(root.join("file"))
        .unwrap();
    (Temp(root), file)
}
fn tracker(file: &File) -> FileMutations {
    FileMutations::for_file(file).unwrap().unwrap()
}

#[test]
fn native_identity_is_object_based_across_handles_rename_and_replacement() {
    let (root, file) = fixture();
    let a = FileIdentity::of(&file).unwrap();
    let alias = File::open(root.0.join("file")).unwrap();
    assert_eq!(a, FileIdentity::of(&alias).unwrap());
    std::fs::rename(root.0.join("file"), root.0.join("old")).unwrap();
    let replacement = File::create(root.0.join("file")).unwrap();
    assert_eq!(a, FileIdentity::of(&file).unwrap());
    assert_ne!(a, FileIdentity::of(&replacement).unwrap());
    #[cfg(unix)]
    {
        std::fs::hard_link(root.0.join("old"), root.0.join("alias")).unwrap();
        assert_eq!(
            a,
            FileIdentity::of(&File::open(root.0.join("alias")).unwrap()).unwrap()
        );
    }
}
#[test]
fn independent_handles_share_one_tracker_but_other_objects_do_not() {
    let (root, file) = fixture();
    let a = tracker(&file);
    let b = tracker(&File::open(root.0.join("file")).unwrap());
    let other = tracker(&File::create(root.0.join("other")).unwrap());
    assert!(Arc::ptr_eq(&a.0, &b.0));
    assert!(!Arc::ptr_eq(&a.0, &other.0));
    let read = a.read_epoch().unwrap();
    let unrelated = other.begin_write().unwrap();
    assert!(read.verify().is_ok());
    drop(unrelated);
    let writing = b.begin_write().unwrap();
    assert!(a.read_epoch().is_err());
    assert!(read.verify().is_err());
    drop(writing);
    assert!(read.verify().is_err());
    assert!(a.read_epoch().unwrap().verify().is_ok());
}
#[test]
fn active_and_completed_writes_are_visible_even_with_restored_metadata() {
    let (_root, mut file) = fixture();
    file.write_all(b"old").unwrap();
    let mutations = tracker(&file);
    let read = mutations.read_epoch().unwrap();
    let original = super::super::file_io::FileStamp::of(file.metadata().unwrap()).unwrap();
    let modified = file.metadata().unwrap().modified().unwrap();
    let write = mutations.begin_write().unwrap();
    assert!(mutations.read_epoch().is_err());
    assert!(read.verify().is_err());
    file.seek(SeekFrom::Start(0)).unwrap();
    file.write_all(b"new").unwrap();
    file.set_times(std::fs::FileTimes::new().set_modified(modified))
        .unwrap();
    drop(write);
    original
        .verify(&file)
        .expect("test must defeat length/mtime-only validation");
    assert!(
        read.verify().is_err(),
        "restored metadata hid a participating write"
    );
    assert!(mutations.read_epoch().unwrap().verify().is_ok());
}
#[test]
fn overlapping_writers_and_failed_attempts_revoke_existing_epochs() {
    let (_root, file) = fixture();
    let mutations = tracker(&file);
    let old = mutations.read_epoch().unwrap();
    let a = mutations.begin_write().unwrap();
    let b = mutations.begin_write().unwrap();
    drop(a);
    assert!(mutations.read_epoch().is_err());
    drop(b);
    assert!(old.verify().is_err());
    assert!(
        mutations.read_epoch().is_ok(),
        "completed writers stayed active"
    );
    let fresh = mutations.read_epoch().unwrap();
    let attempt = mutations.begin_write().unwrap();
    drop(attempt);
    assert!(
        fresh.verify().is_err(),
        "failed/partial writes must still invalidate"
    );
    assert!(mutations.read_epoch().unwrap().verify().is_ok());
}
#[test]
fn reader_epochs_keep_the_registration_alive_after_original_tracker_drop() {
    let (_root, file) = fixture();
    let mutations = tracker(&file);
    let id = mutations.0.identity;
    let read = mutations.read_epoch().unwrap();
    let weak = Arc::downgrade(&mutations.0);
    drop(mutations);
    assert!(weak.upgrade().is_some());
    let replacement = tracker(&file);
    assert!(Weak::ptr_eq(&weak, &Arc::downgrade(&replacement.0)));
    drop(replacement.begin_write().unwrap());
    assert!(read.verify().is_err());
    drop((replacement, read));
    assert!(weak.upgrade().is_none());
    assert!(!REGISTRY.lock().contains_key(&id));
}
#[test]
fn owned_write_guard_moves_registration_until_the_last_operation_owner_drops() {
    let (_root, file) = fixture();
    let mutations = tracker(&file);
    let weak = Arc::downgrade(&mutations.0);
    let before = mutations.read_epoch().unwrap();
    let write = mutations.into_write().unwrap();
    let other = tracker(&file);
    assert!(Weak::ptr_eq(&weak, &Arc::downgrade(&other.0)));
    assert!(other.read_epoch().is_err());
    drop(write);
    assert!(before.verify().is_err());
    assert!(other.read_epoch().is_ok(), "owned writer stayed active");
    assert!(other.read_epoch().unwrap().verify().is_ok());
    drop((other, before));
    assert!(weak.upgrade().is_none());
}
#[test]
fn old_destructor_cannot_remove_a_new_registration_for_the_same_object() {
    let (_root, file) = fixture();
    let original = tracker(&file);
    let id = original.0.identity;
    let mut registry = REGISTRY.lock();
    let dropping = std::thread::spawn(move || drop(original));
    while registry.get(&id).unwrap().strong_count() != 0 {
        std::thread::yield_now();
    }
    let fresh = Arc::new(Tracked {
        identity: id,
        state: Mutex::new(State::default()),
    });
    registry.insert(id, Arc::downgrade(&fresh));
    drop(registry);
    dropping.join().unwrap();
    assert!(
        REGISTRY
            .lock()
            .get(&id)
            .is_some_and(|entry| Weak::ptr_eq(entry, &Arc::downgrade(&fresh))),
        "old destructor removed a replacement registration"
    );
    drop(fresh);
    assert!(!REGISTRY.lock().contains_key(&id));
}
#[test]
fn revision_and_writer_overflow_poison_without_wrapping_or_native_io() {
    let (_root, file) = fixture();
    let mutations = tracker(&file);
    let old = mutations.read_epoch().unwrap();
    mutations.0.state.lock().revision = u64::MAX;
    assert!(mutations.start_write().is_err());
    assert!(old.verify().is_err());
    assert!(mutations.read_epoch().is_err());
    let (_root, file) = fixture();
    let mutations = tracker(&file);
    mutations.0.state.lock().writers = usize::MAX;
    assert!(mutations.start_write().is_err());
    assert!(mutations.0.state.lock().poisoned);
}
#[test]
fn retained_write_and_read_epochs_have_no_steady_rust_heap_allocation() {
    let (_root, file) = fixture();
    let mutations = tracker(&file);
    migo_alloc_probe::assert_no_steady_state_allocation(
        migo_alloc_probe::Burst {
            path: "per-object mutation/read epochs",
            warmup: 8,
            measured: 128,
        },
        |_| {
            let read = mutations.read_epoch().unwrap();
            read.verify().unwrap();
            drop(read);
            drop(mutations.begin_write().unwrap());
        },
    );
}
#[test]
fn concurrent_handles_cannot_start_a_read_while_any_writer_remains() {
    let (root, file) = fixture();
    let mutations = tracker(&file);
    let old = mutations.read_epoch().unwrap();
    let barrier = Arc::new(std::sync::Barrier::new(5));
    let gate = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
    let jobs: Vec<_> = (0..4)
        .map(|_| {
            let handle = File::open(root.0.join("file")).unwrap();
            let barrier = barrier.clone();
            let gate = gate.clone();
            std::thread::spawn(move || {
                let mutations = tracker(&handle);
                let _write = mutations.begin_write().unwrap();
                barrier.wait();
                let (lock, cv) = &*gate;
                let mut done = lock.lock().unwrap();
                while !*done {
                    done = cv.wait(done).unwrap();
                }
            })
        })
        .collect();
    barrier.wait();
    assert_eq!(mutations.0.state.lock().writers, 4);
    assert!(mutations.read_epoch().is_err());
    assert!(old.verify().is_err());
    let (lock, cv) = &*gate;
    *lock.lock().unwrap() = true;
    cv.notify_all();
    for job in jobs {
        job.join().unwrap();
    }
    assert!(old.verify().is_err());
    assert!(mutations.read_epoch().unwrap().verify().is_ok());
}

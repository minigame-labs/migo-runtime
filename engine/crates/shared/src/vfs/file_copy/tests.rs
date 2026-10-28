use super::*;
use std::{
    io::{Read, Seek, SeekFrom, Write},
    sync::atomic::{AtomicU64, Ordering},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
#[test]
fn file_copy_preserves_concrete_file_payload_and_resets_source_without_steady_allocation() {
    let base = std::env::temp_dir().join(format!(
        "migo-native-copy-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&base).unwrap();
    let source = base.join("source");
    let target = base.join("target");
    let bytes = (0..65_573).map(|n| (n % 251) as u8).collect::<Vec<_>>();
    std::fs::write(&source, &bytes).unwrap();
    let mut source = File::open(source).unwrap();
    let mut destination = File::options()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&target)
        .unwrap();
    migo_alloc_probe::assert_no_steady_state_allocation(
        migo_alloc_probe::Burst {
            path: "native File copy",
            warmup: 2,
            measured: 8,
        },
        |_| {
            source.seek(SeekFrom::Start(3)).unwrap();
            destination.rewind().unwrap();
            destination.set_len(0).unwrap();
            copy_contents(&mut source, &mut destination).unwrap();
        },
    );
    destination.flush().unwrap();
    destination.rewind().unwrap();
    let mut actual = Vec::new();
    destination.read_to_end(&mut actual).unwrap();
    assert_eq!(actual, bytes);
    drop(source);
    drop(destination);
    std::fs::remove_dir_all(base).unwrap();
}

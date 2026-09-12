//! Inject failure in the actual Rust payload allocator in a child process.
//! An infallible allocation abort must fail this test without aborting its parent.
use migo_io::fs_ops::FileTable;
use shared::protocol::io_cmd::OpenFlag;
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
};

thread_local! {
    static FAIL_AT: Cell<usize> = const { Cell::new(usize::MAX) };
}

struct FailingAllocator;

fn fail(size: usize) -> bool {
    FAIL_AT
        .try_with(|threshold| {
            if size >= threshold.get() {
                threshold.set(usize::MAX);
                true
            } else {
                false
            }
        })
        .unwrap_or(false)
}

// SAFETY: successful allocations and all deallocations forward unchanged to
// System; a selected allocation returns null, as GlobalAlloc permits on failure.
unsafe impl GlobalAlloc for FailingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if fail(layout.size()) {
            std::ptr::null_mut()
        } else {
            unsafe { System.alloc(layout) }
        }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if fail(layout.size()) {
            std::ptr::null_mut()
        } else {
            unsafe { System.alloc_zeroed(layout) }
        }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if fail(new_size) {
            std::ptr::null_mut()
        } else {
            unsafe { System.realloc(ptr, layout, new_size) }
        }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: FailingAllocator = FailingAllocator;

#[test]
fn owned_byob_payload_allocation_failure_returns_an_error() {
    const CHILD: &str = "MIGO_BYOB_ALLOCATION_FAILURE_SIZE";
    if let Ok(size) = std::env::var(CHILD) {
        let threshold: usize = size.parse().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data");
        std::fs::write(&path, vec![7; 200_000]).unwrap();
        let mut table = FileTable::new();
        let fd = table
            .open(path.to_str().unwrap(), OpenFlag::Read, None, None)
            .unwrap();
        FAIL_AT.with(|value| value.set(threshold));
        let result = table.read_for_buffer(fd, 200_000, None);
        let remaining = FAIL_AT.with(|value| value.replace(usize::MAX));
        assert_eq!(
            remaining,
            usize::MAX,
            "allocation failure was not exercised"
        );
        assert!(
            result.is_err(),
            "payload allocation failure was not returned"
        );
        return;
    }
    for threshold in [64 * 1024, 64 * 1024 + 1] {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "owned_byob_payload_allocation_failure_returns_an_error",
                "--nocapture",
            ])
            .env(CHILD, threshold.to_string())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "allocation failure {threshold} aborted or failed: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

//! Isolated allocator refusal proof; this module never enters the product.
use crate::decoder::pcm::PcmCollector;
use shared::error::ErrorCode;
use std::{
    alloc::{GlobalAlloc, Layout},
    cell::Cell,
};

thread_local! {
    static FAIL_BYTES: Cell<usize> = const { Cell::new(0) };
    static FAILURES: Cell<usize> = const { Cell::new(0) };
}
fn refuse(bytes: usize) -> bool {
    FAIL_BYTES
        .try_with(|target| {
            if target.get() != 0 && target.get() == bytes {
                target.set(0);
                let _ = FAILURES.try_with(|count| count.set(count.get() + 1));
                true
            } else {
                false
            }
        })
        .unwrap_or(false)
}
pub(super) struct Allocator(migo_alloc_probe::CountingAllocator);
impl Allocator {
    pub(super) const fn new() -> Self {
        Self(migo_alloc_probe::CountingAllocator::system())
    }
}
// SAFETY: all valid layouts/pointers are forwarded unchanged to the original
// allocator, except an armed allocation/reallocation returns null as permitted
// by GlobalAlloc. A refused realloc leaves the original block untouched.
unsafe impl GlobalAlloc for Allocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if refuse(layout.size()) {
            std::ptr::null_mut()
        } else {
            unsafe { self.0.alloc(layout) }
        }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if refuse(layout.size()) {
            std::ptr::null_mut()
        } else {
            unsafe { self.0.alloc_zeroed(layout) }
        }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        if refuse(size) {
            std::ptr::null_mut()
        } else {
            unsafe { self.0.realloc(ptr, layout, size) }
        }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { self.0.dealloc(ptr, layout) }
    }
}
fn failing<T>(bytes: usize, work: impl FnOnce() -> T) -> (T, usize) {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            FAIL_BYTES.with(|v| v.set(0));
        }
    }
    FAILURES.with(|v| v.set(0));
    FAIL_BYTES.with(|v| {
        assert_eq!(v.get(), 0);
        v.set(bytes);
    });
    let reset = Reset;
    let result = work();
    drop(reset);
    (result, FAILURES.with(Cell::get))
}
#[test]
fn unknown_page_allocation_refusal_is_an_error_and_retires_pcm() {
    let mut collector = PcmCollector::new();
    let (result, failures) = failing(65536, || collector.append_with(&[0.25; 17], |v| v));
    assert_eq!(
        failures, 1,
        "a real allocator request must have been refused"
    );
    assert_eq!(result.unwrap_err().code, ErrorCode::OutOfMemory);
    assert!(collector.append_with(&[0.25], |v| v).is_err());
    assert!(collector.finish().is_err());
}
#[test]
fn unknown_descriptor_allocation_refusal_is_an_error_and_retires_pcm() {
    let mut collector = PcmCollector::new();
    let (result, failures) = failing(4 * std::mem::size_of::<Vec<f32>>(), || {
        collector.append_with(&[0.25; 17], |v| v)
    });
    assert_eq!(failures, 1);
    assert_eq!(result.unwrap_err().code, ErrorCode::OutOfMemory);
    assert!(collector.finish().is_err());
}
#[test]
fn unknown_final_allocation_refusal_releases_the_existing_pages() {
    let mut collector = PcmCollector::new();
    for _ in 0..4 {
        collector.append_with(&[0.25; 8192], |v| v).unwrap();
    }
    let before = migo_alloc_probe::thread_counts();
    let (result, failures) = failing(131072, || collector.seal());
    assert_eq!(failures, 1);
    assert_eq!(result.unwrap_err().code, ErrorCode::OutOfMemory);
    assert!(migo_alloc_probe::thread_counts().bytes_freed - before.bytes_freed >= 131072);
    assert!(collector.finish().is_err());
}
#[test]
fn known_reservation_keeps_its_allocation_error_and_releases_storage() {
    let mut collector = PcmCollector::with_expected_samples(Some(16384));
    let (result, failures) = failing(65536, || collector.step());
    assert_eq!(failures, 1);
    assert_eq!(result.unwrap_err().code, ErrorCode::OutOfMemory);
    assert!(collector.finish().is_err());
}

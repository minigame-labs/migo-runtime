//! What the burst assertion can and cannot see, measured against a real allocator.
//!
//! These live in an integration test rather than beside the code because the
//! property under test is a property of a *binary*: the assertion is only
//! meaningful where a counting `#[global_allocator]` is installed, and an
//! integration test is the smallest unit that can install one. The crate's own
//! unit tests are the opposite control — that binary installs no counting
//! allocator, so the self-check must refuse to certify anything there.

use migo_alloc_probe::{
    Burst, CountingAllocator, Cycle, assert_no_steady_state_allocation,
    assert_no_steady_state_growth, thread_counts,
};

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator::system();

#[test]
fn releasing_a_block_returns_the_bytes_it_took() {
    let before = thread_counts();
    let block: Vec<u8> = Vec::with_capacity(64);
    std::hint::black_box(&block);
    drop(block);
    let after = thread_counts();

    assert_eq!(
        after.live_bytes() - before.live_bytes(),
        0,
        "an allocate-and-release pair must net to zero, or growth cannot be measured"
    );
    assert!(
        after.bytes_freed > before.bytes_freed,
        "the release was seen"
    );
}

#[test]
fn a_retained_block_shows_as_live_bytes() {
    let before = thread_counts();
    let block: Vec<u8> = Vec::with_capacity(4096);
    let after = thread_counts();
    std::hint::black_box(&block);

    assert!(
        after.live_bytes() - before.live_bytes() >= 4096,
        "a block still held must count as live"
    );
}

#[test]
fn growing_a_block_nets_only_the_difference() {
    let mut block: Vec<u8> = Vec::with_capacity(64);
    let before = thread_counts();
    block.reserve_exact(4096);
    let after = thread_counts();
    std::hint::black_box(&block);

    assert_eq!(
        after.live_bytes() - before.live_bytes(),
        4096 - 64,
        "a resize must net the difference, not the whole new block"
    );
}

#[test]
fn a_cycle_that_gives_back_what_it_takes_reports_no_growth() {
    assert_no_steady_state_growth(
        Cycle {
            path: "allocate and release",
            warmup: 2,
            measured: 16,
        },
        |_| {
            let block: Vec<u8> = Vec::with_capacity(512);
            std::hint::black_box(&block);
        },
    );
}

#[test]
#[should_panic(expected = "leaks a block: retained")]
fn a_cycle_that_leaks_fails_and_names_the_path() {
    assert_no_steady_state_growth(
        Cycle {
            path: "leaks a block",
            warmup: 2,
            measured: 16,
        },
        |_| std::mem::forget(Vec::<u8>::with_capacity(512)),
    );
}

/// The boundary of a delta measurement, pinned so the gate is not over-claimed.
///
/// A block allocated *before* the window and leaked *inside* it moves neither
/// counter, so the cycle reads as balanced. This is the pooled-vector case, and it
/// is the reason a growth gate is not a leak detector: it gates the bytes the
/// measured window itself took. A lost loan surfaces only once the drained pool
/// forces a fresh allocation — the same second-order signal a burst relies on.
#[test]
fn a_block_taken_from_an_earlier_population_and_leaked_is_not_visible() {
    let mut population: Vec<Vec<u8>> = (0..64).map(|_| Vec::with_capacity(512)).collect();
    assert_no_steady_state_growth(
        Cycle {
            path: "leaks a block it was handed",
            warmup: 2,
            measured: 16,
        },
        |_| {
            if let Some(loan) = population.pop() {
                std::mem::forget(loan);
            }
        },
    );
}

#[test]
fn a_cycle_that_only_releases_is_not_growth() {
    let mut blocks: Vec<Vec<u8>> = (0..32).map(|_| Vec::with_capacity(256)).collect();
    assert_no_steady_state_growth(
        Cycle {
            path: "drains a backlog",
            warmup: 2,
            measured: 16,
        },
        |_| {
            blocks.pop();
        },
    );
}

#[test]
fn a_bounded_cache_filling_during_warmup_is_not_a_leak() {
    let mut cache: Vec<Vec<u8>> = Vec::with_capacity(8);
    assert_no_steady_state_growth(
        Cycle {
            path: "bounded cache reaching its bound",
            warmup: 8,
            measured: 16,
        },
        |_| {
            if cache.len() == 8 {
                cache.remove(0);
            }
            cache.push(Vec::with_capacity(256));
        },
    );
}

#[test]
#[should_panic(expected = "unbounded cache: retained")]
fn a_cache_that_never_evicts_fails() {
    let mut cache: Vec<Vec<u8>> = Vec::with_capacity(1024);
    assert_no_steady_state_growth(
        Cycle {
            path: "unbounded cache",
            warmup: 8,
            measured: 16,
        },
        |_| cache.push(Vec::with_capacity(256)),
    );
}

#[test]
fn a_fresh_block_is_counted_as_an_allocation() {
    let before = thread_counts();
    let block: Vec<u8> = Vec::with_capacity(64);
    std::hint::black_box(&block);
    let after = thread_counts();

    assert_eq!(after.allocations - before.allocations, 1);
    assert!(after.bytes_allocated - before.bytes_allocated >= 64);
    assert_eq!(after.reallocations - before.reallocations, 0);
}

#[test]
fn growing_past_reserved_capacity_is_counted_as_an_allocation_event() {
    let mut block: Vec<u8> = Vec::with_capacity(4);
    let before = thread_counts();
    block.extend_from_slice(&[0; 4096]);
    std::hint::black_box(&block);
    let after = thread_counts();

    assert!(after.reallocations > before.reallocations);
    assert!(after.allocation_events() > before.allocation_events());
}

#[test]
fn releasing_a_block_is_not_an_allocation_event() {
    let mut blocks: Vec<Vec<u8>> = (0..8).map(|_| Vec::with_capacity(64)).collect();
    let before = thread_counts();
    while let Some(block) = blocks.pop() {
        drop(block);
    }
    let after = thread_counts();

    assert!(after.deallocations > before.deallocations);
    assert_eq!(after.allocation_events(), before.allocation_events());
}

#[test]
fn a_burst_that_stays_off_the_heap_passes() {
    let mut total = 0u64;
    assert_no_steady_state_allocation(
        Burst {
            path: "arithmetic only",
            warmup: 2,
            measured: 16,
        },
        |iteration| {
            total += iteration as u64;
            total
        },
    );
    assert_eq!(total, (0..18u64).sum::<u64>());
}

#[test]
#[should_panic(expected = "pointer motion: 16 heap allocation event(s)")]
fn a_burst_that_allocates_every_iteration_fails_and_names_the_path() {
    assert_no_steady_state_allocation(
        Burst {
            path: "pointer motion",
            warmup: 2,
            measured: 16,
        },
        |_| Vec::<u8>::with_capacity(32),
    );
}

#[test]
fn allocation_during_warmup_is_not_steady_state() {
    assert_no_steady_state_allocation(
        Burst {
            path: "lazily initialised on first use",
            warmup: 3,
            measured: 16,
        },
        |iteration| {
            if iteration < 3 {
                Some(Vec::<u8>::with_capacity(32))
            } else {
                None
            }
        },
    );
}

#[test]
#[should_panic(expected = "1 heap allocation event(s)")]
fn a_single_allocation_in_the_last_measured_iteration_still_fails() {
    assert_no_steady_state_allocation(
        Burst {
            path: "allocates once, late",
            warmup: 2,
            measured: 16,
        },
        |iteration| {
            if iteration == 17 {
                Some(Vec::<u8>::with_capacity(32))
            } else {
                None
            }
        },
    );
}

#[test]
fn a_measured_burst_reports_the_iteration_count_it_covered() {
    let panic = std::panic::catch_unwind(|| {
        assert_no_steady_state_allocation(
            Burst {
                path: "reported",
                warmup: 1,
                measured: 5,
            },
            |_| Vec::<u8>::with_capacity(8),
        );
    })
    .expect_err("an allocating burst must fail");
    let message = panic
        .downcast_ref::<String>()
        .expect("assertion messages are formatted");

    assert!(
        message.contains("over 5 measured iteration(s)"),
        "message did not report the measured span: {message}"
    );
}

#[cfg(any(unix, windows))]
#[test]
fn system_allocator_activity_is_outside_the_global_counter() {
    let (before, after) = migo_alloc_probe::on_native_thread(|| {
        use std::alloc::{GlobalAlloc, Layout, System};
        let layout = Layout::from_size_align(64, 8).unwrap();
        let before = thread_counts();
        // System deliberately bypasses the configured GlobalAlloc; std Thread
        // metadata uses this path. A zero here cannot certify native heaps.
        let pointer = unsafe { System.alloc(layout) };
        if pointer.is_null() {
            std::alloc::handle_alloc_error(layout);
        }
        std::hint::black_box(pointer);
        unsafe {
            System.dealloc(pointer, layout);
        }
        (before, thread_counts())
    })
    .unwrap();
    assert_eq!(
        before, after,
        "native interception is needed beyond GlobalAlloc"
    );
}

#[cfg(all(any(unix, windows), panic = "unwind"))]
#[test]
fn native_thread_panics_return_to_the_rust_caller() {
    let result = std::panic::catch_unwind(|| {
        migo_alloc_probe::on_native_thread(|| panic!("native probe assertion")).unwrap();
    });
    let panic = result.expect_err("native assertion must propagate");
    assert_eq!(
        panic.downcast_ref::<&str>(),
        Some(&"native probe assertion")
    );
}

#[cfg(all(target_os = "linux", not(target_env = "ohos")))]
#[test]
#[ignore = "explicit LLVM RTSan native runtime matrix"]
fn native_realtime_runtime_matrix() {
    let probe = migo_alloc_probe::RealtimeProbe::load().unwrap();
    let case = std::env::var("MIGO_RTSAN_CASE").unwrap();
    migo_alloc_probe::on_native_thread(move || match case.as_str() {
        "clean" => probe.run_traced(|| std::hint::black_box(17u64)),
        "system-allocation" => probe.run_traced(|| {
            use std::alloc::{GlobalAlloc, Layout, System};
            let layout = Layout::from_size_align(64, 8).unwrap();
            let pointer = unsafe { System.alloc(layout) };
            if pointer.is_null() {
                std::alloc::handle_alloc_error(layout);
            }
            std::hint::black_box(pointer);
            unsafe {
                System.dealloc(pointer, layout);
            }
            0
        }),
        "system-deallocation" => {
            use std::alloc::{GlobalAlloc, Layout, System};
            let layout = Layout::from_size_align(64, 8).unwrap();
            let pointer = unsafe { System.alloc(layout) };
            if pointer.is_null() {
                std::alloc::handle_alloc_error(layout);
            }
            probe.run_traced(|| unsafe { System.dealloc(pointer, layout) });
            0
        }
        "thread-current" => probe.run_traced(|| {
            std::hint::black_box(std::thread::current());
            0
        }),
        "pthread-mutex" => {
            let mut mutex = std::mem::MaybeUninit::<libc::pthread_mutex_t>::uninit();
            assert_eq!(
                unsafe { libc::pthread_mutex_init(mutex.as_mut_ptr(), std::ptr::null()) },
                0
            );
            let mut mutex = unsafe { mutex.assume_init() };
            probe.run_traced(|| unsafe { libc::pthread_mutex_lock(&mut mutex) });
            unsafe {
                libc::pthread_mutex_unlock(&mut mutex);
                libc::pthread_mutex_destroy(&mut mutex);
            }
            0
        }
        "blocking-syscall" => probe.run_traced(|| {
            let value = std::sync::atomic::AtomicU32::new(0);
            let timeout = libc::timespec {
                tv_sec: 0,
                tv_nsec: 0,
            };
            // This deliberate zero-duration wait never hangs the control,
            // but an independent trace must reject its WAIT opcode.
            unsafe {
                libc::syscall(
                    libc::SYS_futex,
                    value.as_ptr(),
                    libc::FUTEX_WAIT | libc::FUTEX_PRIVATE_FLAG,
                    0,
                    &timeout as *const libc::timespec,
                );
            }
            0
        }),
        _ => panic!("unknown RTSan fixture case"),
    })
    .unwrap();
}

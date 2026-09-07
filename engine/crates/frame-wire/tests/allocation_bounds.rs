//! What the parser and the ingress allocate, asserted rather than described.
//!
//! Two different claims, and conflating them would weaken both. The parser
//! allocates *nothing*, ever, for any input: that is the first rule in this
//! crate's threat model and it holds on the first call. The ingress allocates
//! during warm-up -- it copies each packet into a buffer this process owns,
//! because the caller's slice is borrowed for one call -- and then allocates
//! nothing, because the pool hands back the buffer a finished frame released.
//!
//! Pre-allocating the ceiling instead would make the second claim hold from the
//! first call and cost twelve megabytes a session at the 4 MiB cap, almost all
//! of it never touched. Peak memory is what this lane is measured on.
//!
//! "Validate before allocating" is the first rule in this crate's threat model,
//! and until now it was a comment. A comment cannot notice a `Vec` added inside
//! a rejection path, or a `format!` in an error message, or a collect that
//! looked harmless because the count was small in the test.
//!
//! So a counting allocator is installed for this test binary and armed around
//! the measured calls only. The window is armed *after* every input has been
//! built, because the inputs themselves allocate and a probe that measures its
//! own setup reports a number nobody can act on.
//!
//! The counters are per-thread, and that is the correction rather than a detail.
//! They were process-global, so while the window was armed *any* thread's
//! allocation was charged to the call being measured. This file limited itself to
//! one test to keep other tests out of the window -- and that defence covered only
//! half of it, because libtest's own harness thread runs throughout: it registers
//! the blocking receive, formats event and case names, and does its own bookkeeping,
//! and whether those allocations land inside the window depends on which thread the
//! scheduler picks. On a 2-core runner that is not the same probability as on a
//! 32-core development machine, and it showed: identical code, three runs, two green
//! and one reporting "validate allocated 4 times" -- 400 local runs under full load,
//! 300 pinned to one core with `taskset`, and 10 of CI's exact command reproduced
//! nothing.
//!
//! Raising the expected count would have switched the probe off. The claim is that
//! *this* call does not allocate, which is a claim about the thread making it, so the
//! counters follow the thread.
//!
//! Which thread supplied CI's four is still not known, and no longer needs to be:
//! `Interloper` below makes the pollution reproducible on demand -- restore the
//! counters to statics and this file reports over a thousand -- so the property that
//! holds now is "no other thread can appear in this number", which is stronger than
//! having identified one that did. `const`-initialised `Cell`s, so nothing here can
//! allocate or register a destructor -- an allocator hook that allocated to record an
//! allocation would recurse -- and `try_with`, so a thread tearing down cannot make
//! the hook panic.

use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    hint::{black_box, spin_loop},
    sync::atomic::{AtomicBool, Ordering},
};

use frame_wire::{
    FrameIngress, HEADER_BYTES, SECTION_KIND_COMMAND_STREAM, SECTION_KIND_DAMAGE,
    SECTION_KIND_INLINE_DATA, SECTION_KIND_RESOURCE_REFERENCES, builder::WireFrameBuilder,
    stamp_checksum, validate,
};

thread_local! {
    static ARMED: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<u64> = const { Cell::new(0) };
    static ALLOCATED_BYTES: Cell<u64> = const { Cell::new(0) };
}

/// Charge one allocation to the calling thread, if that thread is measuring.
///
/// `try_with` on every access: this runs inside the global allocator, on every
/// thread in the process, including threads on their way out. A failed access means
/// this thread is not measuring, which is the right answer for every thread but one.
fn record(size: usize) {
    let _ = ARMED.try_with(|armed| {
        if armed.get() {
            let _ = ALLOCATIONS.try_with(|count| count.set(count.get() + 1));
            let _ = ALLOCATED_BYTES.try_with(|bytes| bytes.set(bytes.get() + size as u64));
        }
    });
}

fn arm(measuring: bool) {
    ARMED.with(|armed| armed.set(measuring));
}

/// The count so far, and reset it to zero.
fn take_allocations() -> u64 {
    ALLOCATIONS.with(|count| count.replace(0))
}

fn take_allocated_bytes() -> u64 {
    ALLOCATED_BYTES.with(|bytes| bytes.replace(0))
}

struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record(layout.size());
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        record(new_size);
        unsafe { System.realloc(pointer, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// A helper thread that allocates hard, on command, inside the measured window.
///
/// This is the scenario that made this file fail on CI while passing everywhere it
/// was reproduced: another thread allocating while the window is armed. With the
/// counters process-global it charged those allocations to the call being measured;
/// with them per-thread it cannot, and asserting that on every run is worth more
/// than having reasoned it once.
///
/// Coordination is two atomics and a spin, deliberately. Anything richer -- a
/// channel, a condvar, a join -- would allocate on *this* thread inside the window
/// and the probe would be measuring its own test harness, which is the mistake one
/// layer up.
struct Interloper {
    go: &'static AtomicBool,
    done: &'static AtomicBool,
    join: Option<std::thread::JoinHandle<()>>,
}

static INTERLOPER_GO: AtomicBool = AtomicBool::new(false);
static INTERLOPER_DONE: AtomicBool = AtomicBool::new(false);

impl Interloper {
    /// Spawned *before* the window opens: spawning allocates on the caller.
    fn spawn() -> Self {
        let join = std::thread::spawn(|| {
            while !INTERLOPER_GO.load(Ordering::Acquire) {
                spin_loop();
            }
            // Enough that a global counter could not have missed it.
            let mut sink: Vec<Vec<u8>> = Vec::new();
            for size in 0..1024usize {
                sink.push(vec![0xA5u8; size % 97 + 1]);
            }
            black_box(&sink);
            INTERLOPER_DONE.store(true, Ordering::Release);
        });
        Self {
            go: &INTERLOPER_GO,
            done: &INTERLOPER_DONE,
            join: Some(join),
        }
    }

    /// Let it run and wait for it to finish, without allocating here.
    fn run_inside_the_window(&self) {
        self.go.store(true, Ordering::Release);
        while !self.done.load(Ordering::Acquire) {
            spin_loop();
        }
    }

    /// Joined after the window closes, because joining allocates.
    fn finish(mut self) {
        if let Some(join) = self.join.take() {
            join.join().expect("the interloper thread must not panic");
        }
    }
}

/// A deterministic byte generator. A fuzzer belongs in the fuzz target; what
/// this needs is a fixed sweep that runs on every `cargo test`.
fn xorshift(state: &mut u64) -> u8 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    (*state & 0xFF) as u8
}

const NONCE: u128 = 0xFEDC_BA98_7654_3210_0123_4567_89AB_CDEF;
const GENERATION: u64 = 3;

/// One valid packet at `sequence`, so a submit loop can run without tripping the
/// contiguity rule. An earlier version of this file reused one packet for every
/// call; the warm-up pass then consumed sequence 1 and the measured pass was
/// rejected on every input, reporting zero allocations for a loop that had done
/// nothing.
fn valid_at(sequence: u64) -> Vec<u8> {
    let stream = [0u8; 32];
    let inline = [9u8; 21];
    let refs = [1u8; 12];
    let damage = [2u8; 16];
    let mut packet = WireFrameBuilder::new();
    packet.launch_nonce = NONCE;
    packet.runtime_generation = GENERATION;
    packet.sequence = sequence;
    packet
        .section(SECTION_KIND_COMMAND_STREAM, 8, &stream)
        .section(SECTION_KIND_INLINE_DATA, 21, &inline)
        .section(SECTION_KIND_RESOURCE_REFERENCES, 3, &refs)
        .section(SECTION_KIND_DAMAGE, 1, &damage)
        .build()
}

fn inputs() -> Vec<Vec<u8>> {
    let mut inputs = Vec::new();

    let stream = [0u8; 32];
    let inline = [9u8; 21];
    let refs = [1u8; 12];
    let damage = [2u8; 16];

    let mut valid = WireFrameBuilder::new();
    valid.launch_nonce = NONCE;
    valid.runtime_generation = GENERATION;
    inputs.push(
        valid
            .section(SECTION_KIND_COMMAND_STREAM, 8, &stream)
            .section(SECTION_KIND_INLINE_DATA, 21, &inline)
            .section(SECTION_KIND_RESOURCE_REFERENCES, 3, &refs)
            .section(SECTION_KIND_DAMAGE, 1, &damage)
            .build(),
    );

    // Single-byte corruption at every header and table offset, restamped and
    // not, so both the rejection paths and the checksum path are measured.
    let template = inputs[0].clone();
    let table_end = HEADER_BYTES as usize + 4 * 16;
    for offset in 0..table_end.min(template.len()) {
        for value in [0u8, 0x7F, 0xFF] {
            let mut bytes = template.clone();
            bytes[offset] = value;
            inputs.push(bytes.clone());
            stamp_checksum(&mut bytes);
            inputs.push(bytes);
        }
    }

    // Arbitrary bytes at a spread of lengths, including lengths around the
    // header size where the truncation branches live.
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    for length in [0usize, 1, 17, 63, 79, 80, 81, 96, 200, 4096] {
        let mut bytes = vec![0u8; length];
        for byte in bytes.iter_mut() {
            *byte = xorshift(&mut state);
        }
        inputs.push(bytes);
    }

    inputs
}

#[test]
fn the_parser_allocates_nothing_and_a_warm_ingress_allocates_nothing() {
    let inputs = inputs();

    // --- claim one: the parser, on its very first call ----------------------
    //
    // No warm-up to hide behind. "Validate before allocating" is the first rule
    // in this crate's threat model, and it has to hold for the first packet a
    // session ever sees.
    let interloper = Interloper::spawn();

    arm(true);
    let mut walked = 0usize;
    for bytes in &inputs {
        if let Ok(frame) = validate(bytes) {
            for section in frame.sections() {
                walked += section.bytes.len();
            }
            walked += frame
                .command_stream()
                .map_or(0, |section| section.bytes.len());
            walked += usize::from(frame.references_resources());
        }
    }
    // Inside the window on purpose. See `Interloper`.
    interloper.run_inside_the_window();
    arm(false);
    let parser_allocations = take_allocations();
    let _ = take_allocated_bytes();
    interloper.finish();
    assert_eq!(
        parser_allocations, 0,
        "validate allocated {parser_allocations} times; content must not be able to make \
         the parser allocate, and it has no warm-up to hide behind. Another thread \
         allocated 1024 times inside this same window and must not appear in this \
         number -- if it does, the counters have gone back to being process-global"
    );
    assert!(walked > 0, "no section was ever walked");

    // --- claim two: the ingress, once its pool is warm -----------------------
    //
    // The ingress copies each packet into a buffer this process owns, because
    // the caller's slice is borrowed for one call. That allocates while the
    // pool fills and never again, which is the property that matters on a
    // render path -- and is why the pool grows to the sizes actually seen
    // rather than reserving the 4 MiB ceiling for every credit.
    let mut ingress = FrameIngress::new(NONCE, GENERATION);
    // The packets below name resources, and resource admission is closed until
    // the host says the epoch's table is verified. Opening it here is what the
    // host does after upload; without it every packet is rejected with 1006 and
    // the measured loop would report zero allocations for having done nothing.
    ingress.mark_resources_ready();
    let mut sequence = 1u64;
    let mut warmed = 0u64;
    for _ in 0..8 {
        let bytes = valid_at(sequence);
        let (outcome, frame) = ingress.submit(&bytes);
        assert_eq!(
            outcome.decision,
            frame_wire::IngressDecision::Accepted,
            "warm-up packet {sequence} was refused with {}",
            outcome.wire_error_code
        );
        warmed += 1;
        drop(frame);
        sequence += 1;
    }
    assert!(warmed >= 1, "the warm-up pass accepted nothing");
    let pool_allocations_after_warmup = ingress.pool().allocations();

    // Built before the window opens: constructing a packet allocates, and a
    // probe that measures its own inputs reports a number nobody can act on.
    let stream: Vec<Vec<u8>> = (0..64).map(|offset| valid_at(sequence + offset)).collect();

    arm(true);
    let mut accepted = 0u64;
    let mut codes = 0usize;
    for bytes in &stream {
        let (outcome, frame) = ingress.submit(bytes);
        if frame.is_some() {
            accepted += 1;
        }
        // Dropped inside the measured window on purpose: returning the buffer
        // to the pool is part of the steady state being measured.
        drop(frame);
        codes += outcome.wire_error_code as usize;
    }
    // And the rejection paths, which must not allocate either.
    for bytes in &inputs {
        let (outcome, frame) = ingress.submit(bytes);
        drop(frame);
        codes += outcome.wire_error_code as usize;
    }
    arm(false);

    let allocations = take_allocations();
    let bytes_allocated = take_allocated_bytes();
    assert_eq!(
        allocations,
        0,
        "a warm submit loop allocated {allocations} times ({bytes_allocated} bytes) across \
         {} valid packets and {} malformed ones",
        stream.len(),
        inputs.len()
    );
    assert_eq!(
        ingress.pool().allocations(),
        pool_allocations_after_warmup,
        "the pool grew after warm-up"
    );
    assert_eq!(
        accepted,
        stream.len() as u64,
        "every packet in the stream must be accepted"
    );
    assert!(
        codes > 0,
        "no input was ever rejected, so no rejection path was measured"
    );
    assert!(
        inputs.len() > 100,
        "the sweep is too small to mean anything"
    );
}

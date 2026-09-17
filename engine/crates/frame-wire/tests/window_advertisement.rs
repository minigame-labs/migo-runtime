//! The window a producer sends against, read the way a frame-clock tick reads
//! it: from another thread, without the ingress lock.
//!
//! What has to hold is the contract's sentence "a producer that follows it is
//! never told `WOULD_BLOCK`". The deterministic tests pin each rule the read
//! applies; the last one runs a producer, a clock and a renderer on three
//! threads and asserts the sentence itself.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::thread;

use frame_wire::builder::WireFrameBuilder;
use frame_wire::{
    FrameIngress, IngressDecision, PooledFrame, SECTION_KIND_COMMAND_STREAM, WindowAdvertisement,
};

const NONCE: u128 = 0xA3;

fn packet(sequence: u64) -> Vec<u8> {
    let mut builder = WireFrameBuilder::new();
    builder.launch_nonce = NONCE;
    builder.sequence = sequence;
    builder
        .section(SECTION_KIND_COMMAND_STREAM, 2, &[0; 8])
        .build()
}

/// What the producer may send, by the contract's formula.
fn window(advertised: WindowAdvertisement, sent: u64) -> u64 {
    u64::from(advertised.remaining_credits)
        .saturating_sub(sent.saturating_sub(advertised.accepted_sequence))
}

#[test]
fn before_the_first_packet_is_accepted_the_window_is_one_whatever_is_free() {
    let ingress = FrameIngress::new(NONCE, 1);
    assert_eq!(ingress.remaining_credits(), 2, "both credits are free");
    assert_eq!(
        ingress.window_source().read(),
        WindowAdvertisement {
            remaining_credits: 1,
            accepted_sequence: 0,
        },
        "two would let sequence 2 leave beside sequence 1 and overtake it, and \
         ingress holds nothing ahead of sequence 1"
    );
}

#[test]
fn once_a_packet_is_accepted_the_whole_free_count_is_advertised() {
    let mut ingress = FrameIngress::new(NONCE, 1);
    let source = ingress.window_source();
    let (outcome, frame) = ingress.submit(&packet(1));
    assert_eq!(outcome.decision, IngressDecision::Accepted);
    assert_eq!(
        source.read(),
        WindowAdvertisement {
            remaining_credits: 1,
            accepted_sequence: 1,
        },
        "the accepted frame still holds its credit"
    );
    drop(frame);
    assert_eq!(
        source.read(),
        WindowAdvertisement {
            remaining_credits: 2,
            accepted_sequence: 1,
        },
        "and the credit it returns is advertised at once"
    );
}

#[test]
fn a_packet_that_took_its_credit_and_has_not_committed_is_counted_against_the_window() {
    // The consumer runs after the credit is taken and before the sequence is
    // committed -- exactly the moment a tick on another thread can read. Whatever
    // it reads then must not let the producer send past what is free.
    let mut ingress = FrameIngress::new(NONCE, 1);
    let source = ingress.window_source();
    let (_, first) = ingress.submit(&packet(1));
    let first = first.expect("sequence 1 is accepted");

    let mut read_mid_admission = None;
    let mut second = None;
    let outcome = ingress.submit_with(&packet(2), |frame| {
        read_mid_admission = Some(source.read());
        second = Some(frame);
        Ok(())
    });
    assert_eq!(outcome.decision, IngressDecision::Accepted);
    let advertised = read_mid_admission.expect("the consumer ran");
    assert_eq!(
        advertised.accepted_sequence, 1,
        "sequence 2 has not committed yet"
    );
    assert_eq!(
        advertised.remaining_credits, 0,
        "but it has taken its credit"
    );
    assert_eq!(
        window(advertised, 2),
        0,
        "counted twice, never zero times: nothing more may be sent"
    );
    drop((first, second));
}

#[test]
fn the_source_follows_a_tightened_window() {
    let mut ingress = FrameIngress::new(NONCE, 1).with_max_credits(1);
    let source = ingress.window_source();
    let (_, frame) = ingress.submit(&packet(1));
    drop(frame);
    assert_eq!(
        source.read(),
        WindowAdvertisement {
            remaining_credits: 1,
            accepted_sequence: 1,
        },
        "a source taken after `with_max_credits` reads that window, not the default"
    );
}

/// The contract's claim, end to end: a producer that sends only against the
/// latest advertisement a concurrent reader published is never told
/// `WOULD_BLOCK`, while a renderer returns credits at its own pace.
///
/// What this cannot do is force the one interleaving that tells the two read
/// orders apart -- the free count read before a packet takes its credit and the
/// sequence read after it commits. Measured: with the reads swapped in
/// `WindowSource::read`, this passed three runs of three, because the two loads
/// are nanoseconds apart and a submit is not. The order is pinned by the
/// argument on `read` and by the mid-admission test above; this pins that the
/// rules, together, hold up under real scheduling.
#[test]
fn a_producer_that_follows_the_advertisements_is_never_told_to_wait() {
    const FRAMES: u64 = 20_000;

    let ingress = Arc::new(Mutex::new(FrameIngress::new(NONCE, 1)));
    let source = ingress.lock().expect("ingress").window_source();
    let latest = Arc::new(Mutex::new(source.read()));
    let done = Arc::new(AtomicBool::new(false));

    // The clock: publishes advertisements as fast as it can, from a thread that
    // never takes the ingress lock.
    let clock = {
        let latest = Arc::clone(&latest);
        let done = Arc::clone(&done);
        thread::spawn(move || {
            let mut reads = 0u64;
            while !done.load(Ordering::Relaxed) {
                *latest.lock().expect("latest") = source.read();
                reads += 1;
            }
            reads
        })
    };

    // The renderer: finishes frames in order, a little behind.
    let (to_renderer, frames) = mpsc::channel::<PooledFrame>();
    let renderer = thread::spawn(move || {
        let mut finished = 0u64;
        for frame in frames {
            if finished.is_multiple_of(7) {
                thread::yield_now();
            }
            drop(frame);
            finished += 1;
        }
        finished
    });

    let mut sent = 0u64;
    let mut waited = 0u64;
    while sent < FRAMES {
        let advertised = *latest.lock().expect("latest");
        if window(advertised, sent) == 0 {
            waited += 1;
            thread::yield_now();
            continue;
        }
        let bytes = packet(sent + 1);
        let (outcome, frame) = ingress.lock().expect("ingress").submit(&bytes);
        assert_ne!(
            outcome.decision,
            IngressDecision::WouldBlock,
            "sequence {} was sent against {advertised:?} and told to wait",
            sent + 1
        );
        assert_eq!(outcome.decision, IngressDecision::Accepted);
        sent += 1;
        to_renderer
            .send(frame.expect("an accepted packet has a frame"))
            .expect("the renderer is running");
    }

    drop(to_renderer);
    done.store(true, Ordering::Relaxed);
    let finished = renderer.join().expect("renderer");
    let reads = clock.join().expect("clock");
    assert_eq!(finished, FRAMES);
    assert!(
        reads > 0,
        "the clock published nothing, so nothing was tested"
    );
    assert!(
        waited > 0,
        "the producer never had to wait, so the window never bound and this proved nothing"
    );
}

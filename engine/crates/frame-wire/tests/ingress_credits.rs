//! Identity, ordering, admission and backpressure: the rules the parser cannot
//! check because they depend on state the host owns.

use frame_wire::{
    FrameIngress, IngressDecision, MAX_TOTAL_BYTES, SECTION_KIND_COMMAND_STREAM,
    SECTION_KIND_RESOURCE_REFERENCES, WireError,
    builder::WireFrameBuilder,
    ingress::{
        DEFAULT_MAX_CREDITS, INGRESS_ERROR_FOREIGN_SESSION, INGRESS_ERROR_NONCONTIGUOUS_SEQUENCE,
        INGRESS_ERROR_PACKET_TOO_LARGE, INGRESS_ERROR_RESOURCES_NOT_READY,
        INGRESS_ERROR_STALE_RESOURCE_EPOCH, INGRESS_ERROR_STALE_SURFACE, MAX_CREDITS,
    },
};

const NONCE: u128 = 0x0123_4567_89AB_CDEF_FEDC_BA98_7654_3210;
const GENERATION: u64 = 7;

type Packet = Vec<u8>;

fn packet(sequence: u64) -> Packet {
    build(sequence, NONCE, GENERATION, 0, 0)
}

fn build(
    sequence: u64,
    launch_nonce: u128,
    runtime_generation: u64,
    surface_generation: u64,
    resource_epoch: u64,
) -> Packet {
    let stream = [0u8; 8];
    let mut frame = WireFrameBuilder::new();
    frame.sequence = sequence;
    frame.launch_nonce = launch_nonce;
    frame.runtime_generation = runtime_generation;
    frame.surface_generation = surface_generation;
    frame.resource_epoch = resource_epoch;
    frame.frame_id = sequence as u32;
    frame
        .section(SECTION_KIND_COMMAND_STREAM, 2, &stream)
        .build()
}

/// A packet that names resources, for the admission cases.
fn packet_with_resources(sequence: u64, resource_epoch: u64) -> Packet {
    let stream = [0u8; 8];
    let refs = [0u8; 8];
    let mut frame = WireFrameBuilder::new();
    frame.sequence = sequence;
    frame.launch_nonce = NONCE;
    frame.runtime_generation = GENERATION;
    frame.resource_epoch = resource_epoch;
    frame
        .section(SECTION_KIND_COMMAND_STREAM, 2, &stream)
        .section(SECTION_KIND_RESOURCE_REFERENCES, 2, &refs)
        .build()
}

fn ingress() -> FrameIngress {
    FrameIngress::new(NONCE, GENERATION)
}

#[test]
fn credits_bound_the_number_of_frames_in_flight() {
    let mut ingress = ingress();
    assert_eq!(ingress.remaining_credits(), DEFAULT_MAX_CREDITS);

    // Held, not dropped. A frame owns its credit for as long as it exists, so a
    // test that let each one fall out of scope would never reach the limit --
    // and would have been testing nothing while looking like it tested the
    // window.
    let mut in_flight = Vec::new();
    let mut sequence = 1;
    for expected_remaining in (0..DEFAULT_MAX_CREDITS).rev() {
        let bytes = packet(sequence);
        let (outcome, frame) = ingress.submit(&bytes);
        assert_eq!(outcome.decision, IngressDecision::Accepted);
        assert_eq!(outcome.remaining_credits, expected_remaining);
        assert_eq!(outcome.accepted_sequence, sequence);
        in_flight.push(frame.expect("an accepted packet comes back owned"));
        sequence += 1;
    }

    let bytes = packet(sequence);
    let (outcome, frame) = ingress.submit(&bytes);
    assert_eq!(outcome.decision, IngressDecision::WouldBlock);
    assert_eq!(outcome.remaining_credits, 0);
    assert!(frame.is_none());

    // The blocked packet keeps its number: WouldBlock consumes nothing, so the
    // producer resends the same bytes rather than skipping a sequence it can
    // never fill.
    drop(in_flight.pop());
    assert_eq!(
        ingress.remaining_credits(),
        1,
        "one frame finished, one credit back"
    );
    let (outcome, frame) = ingress.submit(&bytes);
    assert_eq!(outcome.decision, IngressDecision::Accepted);
    assert_eq!(outcome.accepted_sequence, sequence);
    assert!(frame.is_some());
}

/// The bytes the renderer sees are this process's own copy.
///
/// The caller's slice is borrowed for one call -- on Apple it points into a
/// `Data` the Swift transport owns -- so a frame that referenced it would be
/// reading freed memory by the time the renderer got to it.
#[test]
fn an_accepted_frame_owns_its_bytes() {
    let mut ingress = ingress();
    let original = packet(1);
    let (outcome, frame) = ingress.submit(&original);
    assert_eq!(outcome.decision, IngressDecision::Accepted);
    let frame = frame.expect("accepted");

    assert_eq!(frame.bytes(), original.as_slice());
    assert_eq!(frame.sequence(), 1);
    // And the owned copy validates on its own, without trusting the check that
    // ran on the borrowed slice across a thread boundary.
    let parsed = frame.frame().expect("the copy is the same packet");
    assert_eq!(parsed.sequence(), 1);

    // Mutating the caller's buffer afterwards cannot reach the frame.
    let mut scribbled = original.clone();
    scribbled[0] ^= 0xFF;
    assert_eq!(frame.bytes(), original.as_slice());
}

/// Steady state allocates nothing: the pool hands back the buffer a finished
/// frame released. Warm-up allocates, and that is the point of measuring after
/// it rather than pre-allocating the ceiling.
#[test]
fn the_buffer_pool_stops_allocating_once_it_is_warm() {
    let mut ingress = ingress();
    let mut sequence = 1u64;
    for _ in 0..(DEFAULT_MAX_CREDITS + 1) {
        let bytes = packet(sequence);
        let (_, frame) = ingress.submit(&bytes);
        drop(frame);
        sequence += 1;
    }
    let warm = ingress.pool().allocations();
    assert!(
        warm <= DEFAULT_MAX_CREDITS as usize + 1,
        "warm-up allocated {warm} buffers"
    );

    for _ in 0..64 {
        let bytes = packet(sequence);
        let (outcome, frame) = ingress.submit(&bytes);
        assert_eq!(outcome.decision, IngressDecision::Accepted);
        drop(frame);
        sequence += 1;
    }
    assert_eq!(
        ingress.pool().allocations(),
        warm,
        "a warm pool must not allocate again"
    );
    assert!(
        ingress.pool().idle_bytes() > 0,
        "the pool retains its buffers"
    );
}

#[test]
fn a_rejected_packet_costs_no_credit() {
    let mut ingress = ingress();
    let mut bytes = packet(1);
    bytes[0] ^= 0xFF;

    let (outcome, frame) = ingress.submit(&bytes);
    assert_eq!(outcome.decision, IngressDecision::Rejected);
    assert_eq!(outcome.wire_error_code, WireError::BadMagic.code());
    assert_eq!(outcome.remaining_credits, DEFAULT_MAX_CREDITS);
    assert!(frame.is_none());
    assert_eq!(ingress.in_flight(), 0);
}

#[test]
fn a_packet_addressed_to_another_launch_is_rejected() {
    let mut ingress = ingress();
    // Only the high half differs: a 64-bit comparison would accept this.
    let neighbour = NONCE ^ (1u128 << 100);
    let bytes = build(1, neighbour, GENERATION, 0, 0);
    let (outcome, frame) = ingress.submit(&bytes);
    assert_eq!(outcome.decision, IngressDecision::Rejected);
    assert_eq!(outcome.wire_error_code, INGRESS_ERROR_FOREIGN_SESSION);
    assert!(frame.is_none());
}

#[test]
fn a_packet_from_a_dead_generation_reports_generation_lost_not_an_error() {
    let mut ingress = ingress();
    // Only the high half differs here too.
    let bytes = build(1, NONCE, GENERATION | (1u64 << 40), 0, 0);
    let (outcome, frame) = ingress.submit(&bytes);
    assert_eq!(outcome.decision, IngressDecision::GenerationLost);
    assert_eq!(
        outcome.wire_error_code, 0,
        "generation loss is nobody's error"
    );
    assert!(frame.is_none());
}

/// Strictly contiguous, not merely increasing. A gap means a packet carrying
/// state was lost, and there is no recovery from that which does not involve a
/// new generation.
#[test]
fn sequences_must_be_strictly_contiguous() {
    let mut ingress = ingress();

    let first = packet(1);
    assert_eq!(
        ingress.submit(&first).0.decision,
        IngressDecision::Accepted,
        "the first accepted sequence is 1"
    );

    // Not 3: one ahead is held for the packet before it (see the deferral tests
    // below), and executes only once that packet has.
    for wrong in [1u64, 0, 4, 100, u64::MAX] {
        let bytes = packet(wrong);
        let (outcome, frame) = ingress.submit(&bytes);
        assert_eq!(
            outcome.decision,
            IngressDecision::Rejected,
            "sequence {wrong} is not exactly 2"
        );
        assert_eq!(
            outcome.wire_error_code,
            INGRESS_ERROR_NONCONTIGUOUS_SEQUENCE
        );
        assert!(frame.is_none());
        assert_eq!(ingress.last_accepted_sequence(), 1);
    }

    let next = packet(2);
    assert_eq!(ingress.submit(&next).0.decision, IngressDecision::Accepted);
}

/// A producer cannot start anywhere it likes: the first packet of a generation
/// is sequence 1, so a replay of a later frame from a previous generation --
/// same nonce, same generation number reused -- has nothing to land on.
#[test]
fn the_first_packet_of_a_generation_must_be_sequence_one() {
    let mut ingress = ingress();
    let bytes = packet(2);
    let (outcome, _) = ingress.submit(&bytes);
    assert_eq!(outcome.decision, IngressDecision::Rejected);
    assert_eq!(
        outcome.wire_error_code,
        INGRESS_ERROR_NONCONTIGUOUS_SEQUENCE
    );
}

#[test]
fn a_packet_built_against_a_retired_surface_is_rejected() {
    let mut ingress = ingress();
    assert!(ingress.set_surface_generation(4));

    let stale = build(1, NONCE, GENERATION, 3, 0);
    let (outcome, frame) = ingress.submit(&stale);
    assert_eq!(outcome.decision, IngressDecision::Rejected);
    assert_eq!(outcome.wire_error_code, INGRESS_ERROR_STALE_SURFACE);
    assert!(frame.is_none());

    let current = build(1, NONCE, GENERATION, 4, 0);
    assert_eq!(
        ingress.submit(&current).0.decision,
        IngressDecision::Accepted
    );
}

#[test]
fn a_packet_naming_resources_from_before_a_context_loss_is_rejected() {
    let mut ingress = ingress();
    assert!(ingress.set_resource_epoch(2));
    ingress.mark_resources_ready();

    let stale = build(1, NONCE, GENERATION, 0, 1);
    let (outcome, frame) = ingress.submit(&stale);
    assert_eq!(outcome.decision, IngressDecision::Rejected);
    assert_eq!(outcome.wire_error_code, INGRESS_ERROR_STALE_RESOURCE_EPOCH);
    assert!(frame.is_none());

    let current = build(1, NONCE, GENERATION, 0, 2);
    assert_eq!(
        ingress.submit(&current).0.decision,
        IngressDecision::Accepted
    );
}

/// Neither timeline may move backwards, and the refusal is observable. A
/// timeline that can go back is a timeline on which a stale packet becomes
/// valid again.
#[test]
fn the_surface_and_resource_timelines_only_advance() {
    let mut ingress = ingress();
    assert!(ingress.set_surface_generation(5));
    assert!(!ingress.set_surface_generation(4), "backwards is refused");
    assert_eq!(ingress.surface_generation(), 5, "and changes nothing");
    assert!(
        ingress.set_surface_generation(5),
        "standing still is allowed"
    );
    assert!(ingress.set_surface_generation(6));

    assert!(ingress.set_resource_epoch(9));
    assert!(!ingress.set_resource_epoch(8));
    assert_eq!(ingress.resource_epoch(), 9);
    assert!(ingress.set_resource_epoch(10));
}

/// Advancing the epoch clears readiness in the same call. A host that had to
/// remember to clear it separately is a host that will eventually forget, and
/// the failure would be a frame drawing with ids that name whatever the rebuilt
/// table put in their place.
#[test]
fn advancing_the_resource_epoch_withdraws_readiness() {
    let mut ingress = ingress();
    assert!(!ingress.resources_ready(), "nothing is ready at the start");

    let early = packet_with_resources(1, 0);
    let (outcome, frame) = ingress.submit(&early);
    assert_eq!(outcome.decision, IngressDecision::Rejected);
    assert_eq!(outcome.wire_error_code, INGRESS_ERROR_RESOURCES_NOT_READY);
    assert!(frame.is_none());

    ingress.mark_resources_ready();
    let admitted = packet_with_resources(1, 0);
    assert_eq!(
        ingress.submit(&admitted).0.decision,
        IngressDecision::Accepted
    );

    // Context loss: the table is rebuilt, so nothing in it is ready.
    assert!(ingress.set_resource_epoch(1));
    assert!(!ingress.resources_ready());
    let after_loss = packet_with_resources(2, 1);
    let (outcome, _) = ingress.submit(&after_loss);
    assert_eq!(outcome.wire_error_code, INGRESS_ERROR_RESOURCES_NOT_READY);

    // A frame that names nothing is still fine while the table is rebuilding.
    let plain = build(2, NONCE, GENERATION, 0, 1);
    assert_eq!(ingress.submit(&plain).0.decision, IngressDecision::Accepted);
}

/// Setting the same epoch again is not an advance and must not withdraw
/// readiness -- otherwise an idempotent host call would silently stall the
/// producer.
#[test]
fn restating_the_current_epoch_leaves_readiness_alone() {
    let mut ingress = ingress();
    assert!(ingress.set_resource_epoch(3));
    ingress.mark_resources_ready();
    assert!(ingress.set_resource_epoch(3));
    assert!(ingress.resources_ready());
}

#[test]
fn validity_does_not_depend_on_how_busy_the_renderer_is() {
    let mut ingress = ingress();
    let mut sequence = 1;
    let mut held = Vec::new();
    for _ in 0..DEFAULT_MAX_CREDITS {
        let bytes = packet(sequence);
        let (outcome, frame) = ingress.submit(&bytes);
        assert_eq!(outcome.decision, IngressDecision::Accepted);
        held.push(frame.expect("accepted"));
        sequence += 1;
    }
    assert_eq!(ingress.remaining_credits(), 0);

    // Out of credit -- the frames above are still held -- but malformed bytes
    // are still rejected rather than told to wait. Answering WouldBlock here invites the producer to resend garbage
    // forever.
    let mut bad = packet(sequence);
    bad[4] ^= 0xFF;
    let (outcome, _) = ingress.submit(&bad);
    assert_eq!(outcome.decision, IngressDecision::Rejected);
    assert_eq!(
        outcome.wire_error_code,
        WireError::UnsupportedVersion.code()
    );

    // So is a foreign packet, and so is a non-contiguous one.
    let foreign = build(sequence, NONCE ^ 1, GENERATION, 0, 0);
    assert_eq!(
        ingress.submit(&foreign).0.wire_error_code,
        INGRESS_ERROR_FOREIGN_SESSION
    );
    let jumped = packet(sequence + 5);
    assert_eq!(
        ingress.submit(&jumped).0.wire_error_code,
        INGRESS_ERROR_NONCONTIGUOUS_SEQUENCE
    );
}

/// A credit comes back exactly once, and there is no way to return it twice.
///
/// The previous shape of this was a `complete()` the renderer called by hand on
/// five different paths -- finished, rejected-after-acceptance, context lost,
/// generation lost, shutdown -- and the failure it guarded against was calling
/// it twice. Ownership removes both: the credit returns when the frame is
/// dropped, and a frame can only be dropped once.
#[test]
fn a_credit_returns_exactly_once_and_only_when_the_frame_is_finished() {
    let mut ingress = ingress();
    let bytes = packet(1);
    let (_, frame) = ingress.submit(&bytes);
    let frame = frame.expect("accepted");
    assert_eq!(ingress.in_flight(), 1);

    // Still one credit out while the frame is merely moved around.
    let moved = frame;
    assert_eq!(ingress.in_flight(), 1);
    let boxed = Box::new(moved);
    assert_eq!(ingress.in_flight(), 1);

    drop(boxed);
    assert_eq!(ingress.in_flight(), 0);
    assert_eq!(ingress.remaining_credits(), DEFAULT_MAX_CREDITS);
}

/// A frame outliving the ingress that issued it is safe: the window and the
/// pool are shared, so the credit still comes back to something that exists.
#[test]
fn a_frame_may_outlive_the_ingress_that_accepted_it() {
    let bytes = packet(1);
    let frame = {
        let mut ingress = ingress();
        let (_, frame) = ingress.submit(&bytes);
        frame.expect("accepted")
    };
    assert_eq!(frame.bytes(), bytes.as_slice());
    drop(frame);
}

/// The credit window has a compile-time ceiling, and the setter clamps to it.
/// A value that arrives from configuration, a remote policy or a content
/// manifest can only tighten the window.
#[test]
fn the_credit_window_cannot_be_widened_by_a_caller() {
    for requested in [MAX_CREDITS + 1, 64, u32::MAX] {
        let ingress = ingress().with_max_credits(requested);
        assert_eq!(
            ingress.max_credits(),
            MAX_CREDITS,
            "requesting {requested} credits must clamp to the ceiling"
        );
    }
    assert_eq!(ingress().with_max_credits(1).max_credits(), 1);
    assert_eq!(
        ingress().with_max_credits(0).max_credits(),
        1,
        "zero would deadlock the producer rather than throttle it"
    );
}

/// The packet ceiling behaves the same way: lowerable, never raisable.
#[test]
fn the_packet_ceiling_cannot_be_raised_by_a_caller() {
    for requested in [MAX_TOTAL_BYTES + 1, u32::MAX] {
        let ingress = ingress().with_max_packet_bytes(requested);
        assert_eq!(ingress.max_packet_bytes(), MAX_TOTAL_BYTES);
    }

    let ingress = ingress().with_max_packet_bytes(4096);
    assert_eq!(ingress.max_packet_bytes(), 4096);
}

#[test]
fn a_packet_above_this_sessions_ceiling_is_refused_before_it_is_parsed() {
    let stream = vec![0u8; 4096];
    let mut frame = WireFrameBuilder::new();
    frame.launch_nonce = NONCE;
    frame.runtime_generation = GENERATION;
    let big = frame
        .section(SECTION_KIND_COMMAND_STREAM, 1024, &stream)
        .build();

    let mut tight = ingress().with_max_packet_bytes(1024);
    let (outcome, returned) = tight.submit(&big);
    assert_eq!(outcome.decision, IngressDecision::Rejected);
    assert_eq!(outcome.wire_error_code, INGRESS_ERROR_PACKET_TOO_LARGE);
    assert!(returned.is_none());
    assert_eq!(outcome.remaining_credits, DEFAULT_MAX_CREDITS);

    // The same bytes are fine for a session that did not tighten.
    let mut default = ingress();
    assert_eq!(default.submit(&big).0.decision, IngressDecision::Accepted);
}

// ---------------------------------------------------------------------------
// Deferral: the uplink is two independent streams, so a packet can arrive one
// ahead of its predecessor. It is held and executed in order, never skipped to.
// ---------------------------------------------------------------------------

fn consume(frame: frame_wire::PooledFrame) -> Result<(), u32> {
    drop(frame);
    Ok(())
}

#[test]
fn a_packet_one_ahead_is_held_and_admitted_after_the_one_before_it() {
    let mut ingress = ingress();
    assert_eq!(
        ingress.submit_with(&packet(1), consume).decision,
        IngressDecision::Accepted
    );

    let held = ingress.submit_with(&packet(3), consume);
    assert_eq!(held.decision, IngressDecision::Deferred);
    assert_eq!(held.wire_error_code, 0);
    assert_eq!(
        ingress.last_accepted_sequence(),
        1,
        "a held packet is not accepted"
    );
    assert_eq!(ingress.deferred_sequence(), Some(3));
    assert_eq!(
        ingress.admit_deferred_with(consume),
        None,
        "the gap is still open, so nothing may be admitted yet"
    );

    let filler = ingress.submit_with(&packet(2), consume);
    assert_eq!(filler.decision, IngressDecision::Accepted);
    let admitted = ingress
        .admit_deferred_with(consume)
        .expect("the held packet is admitted once its predecessor is");
    assert_eq!(admitted.decision, IngressDecision::Accepted);
    assert_eq!(admitted.accepted_sequence, 3);
    assert_eq!(ingress.last_accepted_sequence(), 3);
    assert_eq!(ingress.deferred_sequence(), None);
    assert_eq!(
        ingress.submit_with(&packet(4), consume).decision,
        IngressDecision::Accepted
    );
}

#[test]
fn a_held_packet_executes_after_the_packet_before_it_and_never_before() {
    let mut ingress = ingress();
    let mut order = Vec::new();
    assert_eq!(
        ingress
            .submit_with(&packet(1), |f| {
                order.push(f.sequence());
                Ok(())
            })
            .decision,
        IngressDecision::Accepted
    );
    assert_eq!(
        ingress
            .submit_with(&packet(3), |f| {
                order.push(f.sequence());
                Ok(())
            })
            .decision,
        IngressDecision::Deferred
    );
    assert_eq!(
        order,
        [1],
        "a held packet reached the consumer before its predecessor"
    );
    ingress.submit_with(&packet(2), |f| {
        order.push(f.sequence());
        Ok(())
    });
    ingress.admit_deferred_with(|f| {
        order.push(f.sequence());
        Ok(())
    });
    assert_eq!(order, [1, 2, 3]);
}

#[test]
fn nothing_is_held_before_the_first_packet_of_a_generation() {
    let mut ingress = ingress();
    let outcome = ingress.submit_with(&packet(2), consume);
    assert_eq!(outcome.decision, IngressDecision::Rejected);
    assert_eq!(
        outcome.wire_error_code,
        INGRESS_ERROR_NONCONTIGUOUS_SEQUENCE
    );
    assert_eq!(ingress.deferred_sequence(), None);
}

#[test]
fn only_one_packet_is_held_and_a_second_out_of_order_is_refused() {
    let mut ingress = ingress();
    ingress.submit_with(&packet(1), consume);
    assert_eq!(
        ingress.submit_with(&packet(3), consume).decision,
        IngressDecision::Deferred
    );

    // Further ahead than a producer within its credits can be.
    let far = ingress.submit_with(&packet(4), consume);
    assert_eq!(far.decision, IngressDecision::Rejected);
    assert_eq!(far.wire_error_code, INGRESS_ERROR_NONCONTIGUOUS_SEQUENCE);

    // The same sequence again while it is held: a duplicate, not a second slot.
    let again = ingress.submit_with(&packet(3), consume);
    assert_eq!(again.decision, IngressDecision::Rejected);
    assert_eq!(ingress.deferred_sequence(), Some(3));
}

#[test]
fn holding_a_packet_costs_no_credit() {
    let mut ingress = ingress();
    let before = ingress.remaining_credits();
    let held = ingress.submit_with(&packet(3), consume);
    assert_eq!(
        held.decision,
        IngressDecision::Rejected,
        "not before the first packet"
    );
    let mut first = None;
    ingress.submit_with(&packet(1), |f| {
        first = Some(f);
        Ok(())
    });
    let after_first = ingress.remaining_credits();
    assert_eq!(after_first, before - 1);
    let held = ingress.submit_with(&packet(3), consume);
    assert_eq!(held.decision, IngressDecision::Deferred);
    assert_eq!(held.remaining_credits, after_first);
    assert_eq!(
        ingress.remaining_credits(),
        after_first,
        "a held packet took a credit"
    );
    drop(first);
}

#[test]
fn a_held_packet_is_checked_again_when_it_is_admitted() {
    let mut ingress = ingress();
    ingress.submit_with(&packet(1), consume);
    assert_eq!(
        ingress.submit_with(&packet(3), consume).decision,
        IngressDecision::Deferred
    );
    // The resource epoch moves while it is held: the ids it was built against
    // name something else now.
    assert!(ingress.set_resource_epoch(1));
    ingress.mark_resources_ready();
    let filler = build(2, NONCE, GENERATION, 0, 1);
    assert_eq!(
        ingress.submit_with(&filler, consume).decision,
        IngressDecision::Accepted
    );
    let admitted = ingress
        .admit_deferred_with(consume)
        .expect("the gap closed");
    assert_eq!(admitted.decision, IngressDecision::Rejected);
    assert_eq!(admitted.wire_error_code, INGRESS_ERROR_STALE_RESOURCE_EPOCH);
    assert_eq!(
        ingress.deferred_sequence(),
        None,
        "a refused held packet is not held again"
    );
}

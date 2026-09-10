//! The synchronous barrier, and every way a blocked producer gets woken.
//!
//! The cases that matter here are the failures. A producer inside
//! `Atomics.wait` has stopped; whether it starts again is decided entirely by
//! whether one of these paths settles its request. A path that forgets leaves
//! an agent blocked until its process is reclaimed, which on a phone is a game
//! that stopped drawing and never said why.

use frame_wire::sync::{
    MAX_IN_FLIGHT, MAX_REPLY_BYTES, SYNC_LAYOUT, SYNC_RECORD_BYTES, SyncError, SyncMailbox,
    SyncRequest, SyncState,
};

const GENERATION: u64 = 7;
const NOW: u64 = 1_000_000_000;
const OP_READ_PIXELS: u32 = 1;

fn request() -> SyncRequest {
    SyncRequest {
        request_id: 0,
        runtime_generation: GENERATION,
        surface_generation: 3,
        resource_epoch: 2,
        triggering_sequence: 41,
        operation: OP_READ_PIXELS,
        max_reply_bytes: 4096,
        deadline_nanos: NOW + 100_000_000,
    }
}

fn mailbox() -> SyncMailbox {
    SyncMailbox::new(GENERATION)
}

#[test]
fn the_record_layout_is_gapless_and_matches_the_declared_size() {
    let mut expected = 0u32;
    for field in SYNC_LAYOUT {
        assert_eq!(
            field.offset, expected,
            "{} starts at {} but the previous field ends at {expected}",
            field.name, field.offset
        );
        expected += field.size;
    }
    assert_eq!(expected, SYNC_RECORD_BYTES);
    // The state is first and four bytes wide because the producer reads it with
    // an atomic load out of a shared cell.
    assert_eq!(SYNC_LAYOUT[0].name, "state");
    assert_eq!(SYNC_LAYOUT[0].offset, 0);
    assert_eq!(SYNC_LAYOUT[0].size, 4);
}

#[test]
fn a_request_is_accepted_answered_and_acknowledged() {
    let mut mailbox = mailbox();
    assert_eq!(mailbox.state(), SyncState::Free);

    let id = mailbox
        .post(request(), NOW)
        .expect("a first request is accepted");
    assert_ne!(
        id, 0,
        "zero is what a cleared mailbox holds, never a request"
    );
    assert_eq!(mailbox.state(), SyncState::Pending);
    assert!(!mailbox.state().is_settled());

    mailbox
        .complete(id, 4096)
        .expect("a reply that fits is accepted");
    assert_eq!(mailbox.state(), SyncState::Ready);
    assert_eq!(mailbox.reply_bytes(), 4096);
    assert!(mailbox.state().is_settled());

    mailbox.acknowledge();
    assert_eq!(mailbox.state(), SyncState::Free);
    assert_eq!(mailbox.reply_bytes(), 0);
    assert!(mailbox.request().is_none());
}

#[test]
fn only_one_request_may_be_outstanding() {
    assert_eq!(MAX_IN_FLIGHT, 1);
    let mut mailbox = mailbox();
    mailbox.post(request(), NOW).expect("first");
    assert_eq!(
        mailbox.post(request(), NOW),
        Err(SyncError::AlreadyPending),
        "a second request would need a second mailbox and a second waiter"
    );
    // And the first is untouched: a refused post must not disturb what is
    // already blocking a producer.
    assert_eq!(mailbox.state(), SyncState::Pending);
}

#[test]
fn a_reply_that_answers_another_request_is_refused() {
    let mut mailbox = mailbox();
    let id = mailbox.post(request(), NOW).expect("posted");
    assert_eq!(
        mailbox.complete(id.wrapping_add(1), 16),
        Err(SyncError::RequestIdMismatch)
    );
    // The request fails rather than staying pending: a host that answered the
    // wrong id is a host whose next answer cannot be trusted either.
    assert_eq!(mailbox.state(), SyncState::Failed);
    assert_eq!(mailbox.error(), Some(SyncError::RequestIdMismatch));
    assert_eq!(mailbox.reply_bytes(), 0);
}

#[test]
fn a_reply_larger_than_the_producer_reserved_is_refused_rather_than_truncated() {
    let mut mailbox = mailbox();
    let mut request = request();
    request.max_reply_bytes = 1024;
    let id = mailbox.post(request, NOW).expect("posted");

    assert_eq!(mailbox.complete(id, 1025), Err(SyncError::ReplyTooLarge));
    assert_eq!(mailbox.state(), SyncState::Failed);
    assert_eq!(
        mailbox.reply_bytes(),
        0,
        "a truncated readPixels is a wrong answer that looks like a right one"
    );
}

#[test]
fn a_reservation_outside_the_protocol_is_refused_before_anything_blocks() {
    let mut mailbox = mailbox();
    for bad in [0, MAX_REPLY_BYTES + 1] {
        let mut request = request();
        request.max_reply_bytes = bad;
        assert_eq!(
            mailbox.post(request, NOW),
            Err(SyncError::BadReplyReservation)
        );
        assert_eq!(mailbox.state(), SyncState::Free, "nothing was left pending");
    }
}

#[test]
fn a_deadline_that_is_not_in_the_future_is_refused() {
    let mut mailbox = mailbox();
    for deadline in [0, NOW - 1, NOW] {
        let mut request = request();
        request.deadline_nanos = deadline;
        assert_eq!(mailbox.post(request, NOW), Err(SyncError::BadDeadline));
        assert_eq!(mailbox.state(), SyncState::Free);
    }
}

#[test]
fn a_passed_deadline_wakes_the_waiter() {
    let mut mailbox = mailbox();
    let request = request();
    mailbox.post(request, NOW).expect("posted");

    assert!(!mailbox.expire_if_due(request.deadline_nanos - 1));
    assert_eq!(mailbox.state(), SyncState::Pending);

    assert!(mailbox.expire_if_due(request.deadline_nanos));
    assert_eq!(mailbox.state(), SyncState::Failed);
    assert_eq!(mailbox.error(), Some(SyncError::TimedOut));

    // Expiring twice settles nothing further.
    assert!(!mailbox.expire_if_due(request.deadline_nanos + 1));
}

#[test]
fn a_reply_after_the_request_settled_is_refused() {
    let mut mailbox = mailbox();
    let request = request();
    let id = mailbox.post(request, NOW).expect("posted");
    assert!(mailbox.expire_if_due(request.deadline_nanos));

    assert_eq!(
        mailbox.complete(id, 16),
        Err(SyncError::LateReply),
        "the producer has moved on and its reply buffer may be someone else's"
    );
    assert_eq!(
        mailbox.error(),
        Some(SyncError::TimedOut),
        "the reason is unchanged"
    );
}

#[test]
fn a_generation_move_under_a_waiter_fails_it() {
    let mut mailbox = mailbox();
    mailbox.post(request(), NOW).expect("posted");
    assert!(mailbox.invalidate());
    assert_eq!(mailbox.state(), SyncState::Failed);
    assert_eq!(mailbox.error(), Some(SyncError::StaleGeneration));
    assert!(!mailbox.invalidate(), "there is nothing left to invalidate");
}

#[test]
fn a_request_built_against_a_dead_generation_is_refused() {
    let mut mailbox = mailbox();
    let mut request = request();
    request.runtime_generation = GENERATION + 1;
    assert_eq!(mailbox.post(request, NOW), Err(SyncError::StaleGeneration));
    assert_eq!(mailbox.state(), SyncState::Free);
}

#[test]
fn teardown_wakes_the_waiter_and_refuses_every_later_request() {
    let mut mailbox = mailbox();
    mailbox.post(request(), NOW).expect("posted");

    assert!(mailbox.end_session());
    assert_eq!(mailbox.state(), SyncState::Failed);
    assert_eq!(
        mailbox.error(),
        Some(SyncError::SessionEnded),
        "a producer inside Atomics.wait on a session that is gone stays blocked \
         until its agent is destroyed"
    );

    mailbox.acknowledge();
    assert_eq!(
        mailbox.post(request(), NOW),
        Err(SyncError::SessionEnded),
        "a request posted after teardown would block on nothing"
    );
}

#[test]
fn a_cancelled_request_settles_without_an_error() {
    let mut mailbox = mailbox();
    let id = mailbox.post(request(), NOW).expect("posted");
    assert!(mailbox.cancel());
    assert_eq!(mailbox.state(), SyncState::Cancelled);
    assert_eq!(
        mailbox.error(),
        None,
        "the producer withdrew; nothing went wrong"
    );
    assert_eq!(mailbox.complete(id, 8), Err(SyncError::LateReply));
    assert!(!mailbox.cancel());
}

#[test]
fn request_ids_advance_and_never_reuse_zero() {
    let mut mailbox = mailbox();
    let mut seen = Vec::new();
    for _ in 0..4 {
        let id = mailbox.post(request(), NOW).expect("posted");
        seen.push(id);
        mailbox.complete(id, 8).expect("answered");
        mailbox.acknowledge();
    }
    assert!(seen.iter().all(|id| *id != 0));
    let mut sorted = seen.clone();
    sorted.dedup();
    assert_eq!(sorted.len(), seen.len(), "ids are not reused back to back");
}

/// Every settled state must be reachable, or one of them is a branch nothing
/// exercises and the producer's matching arm is dead too.
#[test]
fn every_settled_state_is_reachable() {
    let mut ready = mailbox();
    let id = ready.post(request(), NOW).expect("posted");
    ready.complete(id, 8).expect("answered");
    assert_eq!(ready.state(), SyncState::Ready);

    let mut failed = mailbox();
    failed.post(request(), NOW).expect("posted");
    failed.invalidate();
    assert_eq!(failed.state(), SyncState::Failed);

    let mut cancelled = mailbox();
    cancelled.post(request(), NOW).expect("posted");
    cancelled.cancel();
    assert_eq!(cancelled.state(), SyncState::Cancelled);

    for state in [SyncState::Ready, SyncState::Failed, SyncState::Cancelled] {
        assert!(state.is_settled());
        assert_eq!(SyncState::from_code(state.code()), Some(state));
    }
    assert!(!SyncState::Free.is_settled());
    assert!(!SyncState::Pending.is_settled());
    assert_eq!(SyncState::from_code(99), None);
}

// ---------------------------------------------------------------------------
// The operation's arguments, which do not live in the record
// ---------------------------------------------------------------------------

use frame_wire::sync::{
    GL_RGBA, GL_UNSIGNED_BYTE, READ_PIXELS_PARAMS_BYTES, ReadPixelsParams, SYNC_OP_READ_PIXELS,
};

fn read_pixels_bytes(width: i32, height: i32, format: u32, type_: u32) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(READ_PIXELS_PARAMS_BYTES);
    for word in [1u32, 0, 0, width as u32, height as u32, format, type_, 0] {
        bytes.extend_from_slice(&word.to_le_bytes());
    }
    bytes
}

#[test]
fn read_pixels_params_decode_round_trips_a_well_formed_record() {
    let params = ReadPixelsParams::decode(&read_pixels_bytes(4, 3, GL_RGBA, GL_UNSIGNED_BYTE))
        .expect("a well-formed record decodes");
    assert_eq!(params.canvas_id, 1);
    assert_eq!((params.width, params.height), (4, 3));
    assert_eq!(params.reply_bytes(), Some(4 * 3 * 4));
    assert_eq!(SYNC_OP_READ_PIXELS, OP_READ_PIXELS);
}

#[test]
fn read_pixels_params_refuse_a_record_of_the_wrong_length() {
    // Not a clamp and not a best effort: a short record is a record whose
    // remaining fields would be read out of whatever followed it.
    let mut short = read_pixels_bytes(4, 3, GL_RGBA, GL_UNSIGNED_BYTE);
    short.pop();
    assert_eq!(
        ReadPixelsParams::decode(&short),
        Err(SyncError::UnsupportedOperation)
    );

    let mut long = read_pixels_bytes(4, 3, GL_RGBA, GL_UNSIGNED_BYTE);
    long.push(0);
    assert_eq!(
        ReadPixelsParams::decode(&long),
        Err(SyncError::UnsupportedOperation)
    );
}

#[test]
fn read_pixels_params_refuse_an_empty_rectangle() {
    for (width, height) in [(0, 3), (4, 0), (-1, 3), (4, -1)] {
        assert_eq!(
            ReadPixelsParams::decode(&read_pixels_bytes(width, height, GL_RGBA, GL_UNSIGNED_BYTE)),
            Err(SyncError::UnsupportedOperation),
            "a {width}x{height} rectangle has no pixels to answer with"
        );
    }
}

#[test]
fn read_pixels_params_refuse_a_format_this_host_does_not_read_back() {
    // GL_RGB / GL_UNSIGNED_SHORT_5_6_5. Answering these by pretending they were
    // RGBA8 would hand the producer a buffer whose bytes mean something else.
    assert_eq!(
        ReadPixelsParams::decode(&read_pixels_bytes(4, 3, 0x1907, GL_UNSIGNED_BYTE)),
        Err(SyncError::UnsupportedOperation)
    );
    assert_eq!(
        ReadPixelsParams::decode(&read_pixels_bytes(4, 3, GL_RGBA, 0x8363)),
        Err(SyncError::UnsupportedOperation)
    );
}

#[test]
fn read_pixels_reply_size_refuses_to_overflow_rather_than_wrapping() {
    // 40000 x 40000 x 4 is 6.4e9, past u32. A wrapped product would be a small
    // number that passes every later bound and sizes a buffer nothing fills.
    let params = ReadPixelsParams::decode(&read_pixels_bytes(
        40_000,
        40_000,
        GL_RGBA,
        GL_UNSIGNED_BYTE,
    ))
    .expect("the rectangle itself is well formed");
    assert_eq!(params.reply_bytes(), None);
}

// ---------------------------------------------------------------------------
// Failing a request the host cannot answer
// ---------------------------------------------------------------------------

#[test]
fn fail_request_wakes_the_producer_with_the_stated_reason() {
    let mut mailbox = SyncMailbox::new(GENERATION);
    let id = mailbox.post(request(), NOW).expect("posts");
    assert_eq!(mailbox.state(), SyncState::Pending);

    mailbox
        .fail_request(id, SyncError::UnsupportedOperation)
        .expect("the outstanding request may be failed");
    assert_eq!(mailbox.state(), SyncState::Failed);
    assert_eq!(mailbox.error(), Some(SyncError::UnsupportedOperation));
    assert_eq!(mailbox.reply_bytes(), 0);
}

#[test]
fn fail_request_refuses_an_id_that_is_not_outstanding() {
    let mut mailbox = SyncMailbox::new(GENERATION);
    let id = mailbox.post(request(), NOW).expect("posts");

    assert_eq!(
        mailbox.fail_request(id.wrapping_add(1), SyncError::TimedOut),
        Err(SyncError::RequestIdMismatch)
    );
    // And the outstanding request is untouched: a failure aimed at another
    // request must not settle this one.
    assert_eq!(mailbox.state(), SyncState::Pending);
    assert_eq!(mailbox.error(), None);
}

#[test]
fn fail_request_refuses_once_the_request_is_settled() {
    let mut mailbox = SyncMailbox::new(GENERATION);
    let id = mailbox.post(request(), NOW).expect("posts");
    mailbox.complete(id, 16).expect("completes");

    assert_eq!(
        mailbox.fail_request(id, SyncError::TimedOut),
        Err(SyncError::LateReply)
    );
    // The answer the producer is about to read stays the answer.
    assert_eq!(mailbox.state(), SyncState::Ready);
    assert_eq!(mailbox.reply_bytes(), 16);
}

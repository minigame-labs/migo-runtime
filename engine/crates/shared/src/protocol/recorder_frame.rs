//! Byte-level admission for audio recorder capture frames.
//!
//! Every `RecorderFrameData` command that is NOT a termination frame (`is_last_frame ==
//! false`) must acquire a `RecorderFrameCredit` **before** the JNI copy that fills
//! its `data: Vec<u8>`.  The credit tracks how many bytes of in-flight recorder payload
//! the host queue is carrying; if the budget is exhausted the frame is dropped rather
//! than submitted, and the drop count is incremented for observability.
//!
//! Termination frames (`is_last_frame == true`) bypass the budget entirely and travel
//! on the reliable channel, so a saturated data queue can never prevent the host from
//! seeing the final-frame or stop events.
//!
//! This design mirrors `camera_frame.rs`: the credit is RAII, acquired before the
//! payload copy, carried on the `HostCommand::RecorderFrameData` variant as the
//! mandatory `RecorderFrameCredit`, and released when the command is dropped —
//! whether that happens during normal dispatch or because the host queue itself
//! dropped the command. Termination commands carry a zero-byte credit so the
//! protocol cannot represent an unaccounted frame.

use std::{
    collections::HashMap,
    sync::{
        Arc, LazyLock, Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
};
/// Maximum total bytes of in-flight recorder frame payload per Host.
///
/// This reuses the existing bounded audio-command payload ceiling rather than
/// inventing a recorder-specific memory number. Recorder frames are admitted
/// against this byte budget before their JNI copy; termination frames reserve
/// zero bytes and use the reliable lane.
pub const RECORDER_FRAME_BYTE_BUDGET: usize = crate::audio_channel::MAX_AUDIO_COMMAND_QUEUED_BYTES;

#[derive(Debug)]
struct RecorderFrameState {
    in_flight_bytes: AtomicUsize,
    dropped: AtomicU64,
    budget: usize,
}

/// RAII byte reservation for one recorder frame.
///
/// Acquired before the JNI `byte[]` copy for non-termination frames. Carried on
/// the `HostCommand::RecorderFrameData` command as mandatory
/// `credit: RecorderFrameCredit`. Dropping the credit — whether the command is
/// dispatched normally or the queue discards it — releases the reserved bytes
/// exactly once.
#[derive(Debug)]
pub struct RecorderFrameCredit {
    state: Arc<RecorderFrameState>,
    bytes: usize,
}

impl Drop for RecorderFrameCredit {
    fn drop(&mut self) {
        self.state
            .in_flight_bytes
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                Some(used.saturating_sub(self.bytes))
            })
            .ok();
    }
}

static RECORDER_FRAME_STATES: LazyLock<Mutex<HashMap<i32, Arc<RecorderFrameState>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn recorder_frame_state(host_id: i32) -> Arc<RecorderFrameState> {
    RECORDER_FRAME_STATES
        .lock()
        .expect("recorder frame state registry poisoned")
        .entry(host_id)
        .or_insert_with(|| {
            Arc::new(RecorderFrameState {
                in_flight_bytes: AtomicUsize::new(0),
                dropped: AtomicU64::new(0),
                budget: RECORDER_FRAME_BYTE_BUDGET,
            })
        })
        .clone()
}

/// Try to reserve `bytes` from the per-Host recorder frame byte budget.
///
/// Returns `Some(credit)` if the reservation fits within the budget.  Returns
/// `None` and increments the drop counter if the budget is exhausted.
///
/// Call this **before** the JNI copy so no copy is made for a frame that will
/// be dropped.  Pass `bytes` as the anticipated frame size (e.g. `frameSize *
/// 1024` in Java, or the actual length returned after the copy).
pub fn try_reserve_recorder_frame_bytes(host_id: i32, bytes: usize) -> Option<RecorderFrameCredit> {
    let state = recorder_frame_state(host_id);
    let result = state
        .in_flight_bytes
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
            used.checked_add(bytes)
                .filter(|&total| total <= state.budget)
        });
    if result.is_ok() {
        Some(RecorderFrameCredit { state, bytes })
    } else {
        state.dropped.fetch_add(1, Ordering::Relaxed);
        None
    }
}

/// Reserve recorder bytes, then invoke the deferred JNI-copy operation.
///
/// The closure is evaluated only after admission succeeds, making the
/// pre-copy ordering directly testable without a `JNIEnv`.
pub fn with_recorder_frame_credit<T>(
    host_id: i32,
    bytes: usize,
    copy: impl FnOnce() -> T,
) -> Option<(RecorderFrameCredit, T)> {
    let credit = try_reserve_recorder_frame_bytes(host_id, bytes)?;
    Some((credit, copy()))
}

/// Number of recorder frames dropped for this Host because the byte budget was
/// exhausted.  Observable via the debug stats surface and tests.
pub fn recorder_frame_dropped_count(host_id: i32) -> u64 {
    recorder_frame_state(host_id)
        .dropped
        .load(Ordering::Relaxed)
}

/// Current in-flight byte count for this Host (for testing / observability).
#[cfg(test)]
pub fn recorder_frame_in_flight_bytes(host_id: i32) -> usize {
    recorder_frame_state(host_id)
        .in_flight_bytes
        .load(Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Each test uses a distinct host_id drawn from a range unlikely to collide
    // with any other test in this binary.  The global statics persist across
    // tests within the same process; unique ids keep tests isolated.

    const BASE_HOST: i32 = 8200;

    #[test]
    fn byte_budget_admits_within_limit_and_refuses_when_exhausted() {
        let host_id = BASE_HOST;
        // Fresh host: budget fully available.
        let half = RECORDER_FRAME_BYTE_BUDGET / 2;

        let credit_a = try_reserve_recorder_frame_bytes(host_id, half)
            .expect("first reservation must fit within budget");

        // Second reservation of half the budget: total == budget, still allowed.
        let credit_b = try_reserve_recorder_frame_bytes(host_id, half)
            .expect("second reservation that exactly reaches the ceiling must be admitted");

        // One more byte over the budget must be refused.
        assert!(
            try_reserve_recorder_frame_bytes(host_id, 1).is_none(),
            "reservation that would exceed the budget must be refused"
        );
        assert_eq!(
            recorder_frame_dropped_count(host_id),
            1,
            "the refused reservation must increment the drop counter"
        );

        drop(credit_a);
        drop(credit_b);
    }

    #[test]
    fn drop_releases_bytes_exactly_once_and_allows_re_reservation() {
        let host_id = BASE_HOST + 1;
        let bytes = 512 * 1024; // 512 KiB

        let credit =
            try_reserve_recorder_frame_bytes(host_id, bytes).expect("reservation must be admitted");
        assert_eq!(recorder_frame_in_flight_bytes(host_id), bytes);

        drop(credit);
        assert_eq!(
            recorder_frame_in_flight_bytes(host_id),
            0,
            "Drop must release bytes"
        );

        // Re-reservation after release must succeed.
        let _credit2 = try_reserve_recorder_frame_bytes(host_id, bytes)
            .expect("re-reservation after release must be admitted");
    }

    #[test]
    fn dropped_count_is_observable_and_independent_per_host() {
        let host_a = BASE_HOST + 2;
        let host_b = BASE_HOST + 3;

        // Exhaust host_a's budget.
        let _credit = try_reserve_recorder_frame_bytes(host_a, RECORDER_FRAME_BYTE_BUDGET)
            .expect("exact-budget reservation must be admitted");
        // One byte over → drop.
        assert!(try_reserve_recorder_frame_bytes(host_a, 1).is_none());

        assert_eq!(
            recorder_frame_dropped_count(host_a),
            1,
            "host_a must show 1 drop"
        );
        assert_eq!(
            recorder_frame_dropped_count(host_b),
            0,
            "host_b must show 0 drops; hosts are isolated"
        );
    }

    #[test]
    fn zero_byte_reservation_is_admitted_and_releases_cleanly() {
        let host_id = BASE_HOST + 4;
        // A zero-byte frame (possible for edge-case flusher on empty accumulator,
        // even though Java guards it) must not corrupt the counter.
        let credit = try_reserve_recorder_frame_bytes(host_id, 0)
            .expect("zero-byte reservation must be admitted");
        drop(credit);
        assert_eq!(recorder_frame_in_flight_bytes(host_id), 0);
        assert_eq!(recorder_frame_dropped_count(host_id), 0);
    }
    #[test]
    fn reliable_termination_event_survives_saturated_data_queue() {
        let (data_tx, reliable_tx, mut receiver) = crate::host_channel::channel_with_reserve(1, 1);
        data_tx
            .try_send(crate::protocol::host_cmd::HostCommand::OnHide)
            .expect("first data command fills the normal lane");
        assert!(
            data_tx
                .try_send(crate::protocol::host_cmd::HostCommand::OnShow {
                    options_json: Some("{}".to_owned()),
                })
                .is_err(),
            "normal data lane must be saturated"
        );

        reliable_tx
            .send(crate::protocol::host_cmd::HostCommand::RecorderEvent {
                event_type: "stop".to_owned(),
                json_payload: "{}".to_owned(),
                runtime_generation: None,
            })
            .expect("reliable termination event must bypass normal saturation");

        assert!(matches!(
            receiver.try_recv().expect("normal command remains queued"),
            crate::protocol::host_cmd::HostCommand::OnHide
        ));
        assert!(matches!(
            receiver.try_recv().expect("termination event remains queued"),
            crate::protocol::host_cmd::HostCommand::RecorderEvent { event_type, .. }
                if event_type == "stop"
        ));
    }
    #[test]
    fn recorder_admission_prevents_deferred_copy_when_budget_is_full() {
        let host_id = BASE_HOST + 5;
        let _held = try_reserve_recorder_frame_bytes(host_id, RECORDER_FRAME_BYTE_BUDGET)
            .expect("exact-budget reservation must be admitted");
        let mut copy_called = false;
        let result = with_recorder_frame_credit(host_id, 1, || {
            copy_called = true;
            Vec::<u8>::new()
        });
        assert!(
            result.is_none(),
            "saturated byte lane must refuse the frame"
        );
        assert!(
            !copy_called,
            "refused frame must not invoke its deferred copy"
        );
    }
}

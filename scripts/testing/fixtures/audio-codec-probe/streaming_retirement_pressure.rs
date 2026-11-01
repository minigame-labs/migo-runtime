//! Reuse the paced production player; add natural device-owner churn.
use super::*;
use crate::device_prepare::{DeviceRetirementBatch, DeviceRetirementLoad};
use std::sync::OnceLock;

#[derive(Default)]
struct State {
    enabled: bool,
    prepared: Option<DeviceRetirementBatch>,
    load: Option<DeviceRetirementLoad>,
    start: Option<Instant>,
    callbacks: usize,
    setup_callbacks: usize,
    retirement_callbacks: usize,
    completed_at_end: usize,
}
static STATE: OnceLock<Mutex<State>> = OnceLock::new();
fn state() -> &'static Mutex<State> {
    STATE.get_or_init(|| Mutex::new(State::default()))
}

pub(super) fn on_playing() {
    let mut state = state().lock().unwrap();
    if state.enabled {
        let first = state.prepared.take().unwrap();
        state.load = Some(DeviceRetirementLoad::start(first));
    }
    state.start = Some(Instant::now());
}
pub(super) fn on_tick() {
    let mut state = state().lock().unwrap();
    state.callbacks += 1;
    if state.enabled {
        let load = state
            .load
            .as_ref()
            .expect("device churn worker was not started");
        let (completed, phase, _, bad_thread) = load.snapshot();
        assert!(!bad_thread, "device destruction left existing CPU workers");
        if state.start.unwrap().elapsed() > Duration::from_secs(2) {
            assert!(completed > 0, "device churn made no measured progress");
        }
        if phase == 1 {
            state.setup_callbacks += 1;
        }
        if phase == 2 {
            state.retirement_callbacks += 1;
        }
    }
}
pub(super) fn on_finished() {
    let mut state = state().lock().unwrap();
    if let Some(load) = &state.load {
        let completed = load.snapshot().0;
        load.stop();
        state.completed_at_end = completed;
    }
}
struct Cleanup;
impl Drop for Cleanup {
    fn drop(&mut self) {
        let (batch, load) = {
            let mut state = state().lock().unwrap_or_else(|e| e.into_inner());
            (state.prepared.take(), state.load.take())
        };
        if let Some(batch) = batch {
            batch.discard();
        }
        drop(load);
    }
}

#[test]
fn paced_player_and_whole_jobs_survive_device_owner_churn() {
    let enabled = std::env::var("MIGO_DEVICE_CHURN").unwrap_or_else(|_| "1".into()) == "1";
    let _cleanup = Cleanup;
    let prepared = enabled.then(DeviceRetirementBatch::prepare);
    *state().lock().unwrap() = State {
        enabled,
        prepared,
        ..State::default()
    };
    super::sample_clock_consumption_preserves_pcm_under_unbarriered_pressure();
    let (mut load, callbacks, setup_callbacks, retirement_callbacks, completed_at_end) = {
        let mut state = state().lock().unwrap();
        (
            state.load.take(),
            state.callbacks,
            state.setup_callbacks,
            state.retirement_callbacks,
            state.completed_at_end,
        )
    };
    if let Some(load) = &mut load {
        let summary = load.finish();
        assert!(
            completed_at_end >= 3,
            "device churn did not coexist with paced playback"
        );
        assert!(
            !summary.bad_thread,
            "device destruction left existing CPU workers"
        );
        assert_eq!(summary.retired, summary.batches * 32);
        assert!(
            summary.setup_requests > 0,
            "no foreground requests during filter construction"
        );
        assert!(
            setup_callbacks > 0,
            "no paced callback inside device construction windows"
        );
        println!(
            "MIGO_DEVICE_CHURN {{\"enabled\":true,\"callbacks\":{callbacks},\"setup_callbacks\":{setup_callbacks},\"retirement_callbacks\":{retirement_callbacks},\"completed_during_playback\":{completed_at_end},\"device\":{}}}",
            summary.json()
        );
    } else {
        assert!(!enabled, "device churn worker was not started");
        println!("MIGO_DEVICE_CHURN {{\"enabled\":false,\"callbacks\":{callbacks}}}");
    }
}

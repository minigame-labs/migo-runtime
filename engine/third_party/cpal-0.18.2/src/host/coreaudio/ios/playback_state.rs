//! Requested playback and system interruption are independent. Native operations run
//! under StreamInner's lock; callbacks are passed in so this policy is testable
//! without an Apple SDK. No operation here is on the render callback.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum State {
    #[default]
    Stopped,
    Playing,
    Interrupted,
}

#[derive(Default)]
pub(super) struct PlaybackState {
    state: State,
    interruption_in_progress: bool,
}

impl PlaybackState {
    pub(super) fn play<E>(&mut self, start: impl FnOnce() -> Result<(), E>) -> Result<(), E> {
        if self.interruption_in_progress {
            // Record explicit play intent, but never activate/start while the
            // system holds the session. End still has to grant ShouldResume.
            self.state = State::Interrupted;
        } else if self.state != State::Playing {
            start()?;
            self.state = State::Playing;
        }
        Ok(())
    }

    pub(super) fn pause<E>(&mut self, stop: impl FnOnce() -> Result<(), E>) -> Result<(), E> {
        if self.state == State::Playing {
            stop()?;
        }
        // A background/idle pause revokes automatic resume, including a pause
        // issued while the OS has already stopped the unit.
        self.state = State::Stopped;
        Ok(())
    }

    pub(super) fn begin(&mut self, stop: impl FnOnce()) {
        self.interruption_in_progress = true;
        if self.state == State::Playing {
            stop();
            self.state = State::Interrupted;
        }
    }

    pub(super) fn end<E>(
        &mut self,
        should_resume: bool,
        start: impl FnOnce() -> Result<(), E>,
    ) -> Result<(), E> {
        if !self.interruption_in_progress {
            return Ok(());
        }
        self.interruption_in_progress = false;
        if should_resume && self.state == State::Interrupted {
            self.play(start)?;
        }
        Ok(())
    }

    pub(super) fn requires_resume(&self) -> bool {
        !self.interruption_in_progress && self.state == State::Interrupted
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn playing() -> PlaybackState {
        let mut state = PlaybackState::default();
        state.play(|| Ok::<_, ()>(())).unwrap();
        state
    }

    #[test]
    fn allowed_end_resumes_once_and_duplicate_events_are_idempotent() {
        let mut state = playing();
        let stops = Cell::new(0);
        let starts = Cell::new(0);
        for _ in 0..2 {
            state.begin(|| stops.set(stops.get() + 1));
        }
        for _ in 0..2 {
            state
                .end(true, || {
                    starts.set(starts.get() + 1);
                    Ok::<_, ()>(())
                })
                .unwrap();
        }
        assert_eq!((stops.get(), starts.get()), (1, 1));
        assert!(!state.requires_resume());
    }

    #[test]
    fn denied_end_waits_for_a_new_play_request() {
        let mut state = playing();
        state.begin(|| {});
        state
            .end(false, || -> Result<(), ()> { panic!("denied auto resume") })
            .unwrap();
        assert!(state.requires_resume());
        state.play(|| Ok::<_, ()>(())).unwrap();
        assert!(!state.requires_resume());
    }

    #[test]
    fn stale_end_cannot_override_a_completed_denial() {
        let mut state = playing();
        state.begin(|| {});
        state.end(false, || Ok::<_, ()>(())).unwrap();
        state
            .end(true, || -> Result<(), ()> {
                panic!("stale end resumed denied audio")
            })
            .unwrap();
        assert!(state.requires_resume());
    }

    #[test]
    fn pause_during_interruption_revokes_automatic_resume() {
        let mut state = playing();
        state.begin(|| {});
        state
            .pause(|| -> Result<(), ()> { panic!("the OS already stopped it") })
            .unwrap();
        state
            .end(true, || -> Result<(), ()> { panic!("paused app resumed") })
            .unwrap();
        assert_eq!(state.state, State::Stopped);
        assert!(!state.requires_resume());
    }

    #[test]
    fn play_during_interruption_is_deferred_and_does_not_override_denial() {
        let mut state = PlaybackState::default();
        state.begin(|| panic!("idle unit stopped"));
        state
            .play(|| -> Result<(), ()> { panic!("started during interruption") })
            .unwrap();
        assert!(!state.requires_resume());
        state
            .end(false, || -> Result<(), ()> { panic!("denied auto resume") })
            .unwrap();
        assert!(state.requires_resume());
    }

    #[test]
    fn paused_idle_and_background_streams_do_not_reactivate_the_session() {
        for initial_playing in [false, true] {
            let mut state = if initial_playing {
                playing()
            } else {
                PlaybackState::default()
            };
            state.pause(|| Ok::<_, ()>(())).unwrap();
            state.begin(|| panic!("paused unit stopped"));
            state
                .end(true, || -> Result<(), ()> {
                    panic!("idle session activated")
                })
                .unwrap();
            assert_eq!(state.state, State::Stopped);
            state.play(|| Ok::<_, ()>(())).unwrap();
            assert_eq!(state.state, State::Playing);
        }
    }

    #[test]
    fn native_failures_do_not_commit_a_successful_transition() {
        let mut state = playing();
        assert!(state.pause(|| Err(())).is_err());
        assert_eq!(state.state, State::Playing);
        state.begin(|| {});
        assert!(state.end(true, || Err(())).is_err());
        assert!(state.requires_resume());
        assert!(state.play(|| Err(())).is_err());
        assert!(state.requires_resume());
        state.play(|| Ok::<_, ()>(())).unwrap();
        assert!(!state.requires_resume());
    }

    #[test]
    fn permitted_end_defers_to_the_latest_play_or_pause_intent() {
        let mut state = playing();
        for _ in 0..64 {
            state.begin(|| {});
            state.pause(|| Ok::<_, ()>(())).unwrap();
            state
                .play(|| -> Result<(), ()> { panic!("system holds session") })
                .unwrap();
            state.end(true, || Ok::<_, ()>(())).unwrap();
            assert_eq!(state.state, State::Playing);
            state.begin(|| {});
            state
                .play(|| -> Result<(), ()> { panic!("system holds session") })
                .unwrap();
            state.pause(|| Ok::<_, ()>(())).unwrap();
            state
                .end(true, || -> Result<(), ()> {
                    panic!("latest intent was pause")
                })
                .unwrap();
            assert_eq!(state.state, State::Stopped);
            state.play(|| Ok::<_, ()>(())).unwrap();
        }
    }
}

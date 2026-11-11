//! Mini-game session policy, separate from CPAL's hardware/interruption owner.
//! Serialize native category application with initialization, so an initial
//! default cannot finish after a newer successful explicit request.
use std::sync::Mutex;

use shared::error::EngineResult;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudioSessionOptions {
    pub obey_mute_switch: bool,
}

impl Default for AudioSessionOptions {
    fn default() -> Self {
        Self {
            obey_mute_switch: true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SessionCategory {
    SoloAmbient,
    Playback,
}

impl SessionCategory {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::SoloAmbient => "soloAmbient",
            Self::Playback => "playback",
        }
    }
}

impl AudioSessionOptions {
    pub(crate) fn category(self) -> SessionCategory {
        if self.obey_mute_switch {
            SessionCategory::SoloAmbient
        } else {
            SessionCategory::Playback
        }
    }
}

#[derive(Default)]
struct State {
    options: AudioSessionOptions,
    applied: bool,
    /// The host's recorder holds the session: it set a category that records,
    /// and nothing here is applied over it until it is taken back.
    lent: bool,
}

#[derive(Default)]
pub(crate) struct SessionConfiguration {
    state: Mutex<State>,
}

impl SessionConfiguration {
    pub(crate) fn configure(
        &self,
        options: AudioSessionOptions,
        apply: impl FnOnce(AudioSessionOptions) -> EngineResult<()>,
    ) -> EngineResult<()> {
        let mut state = self
            .state
            .lock()
            .expect("the session state is never poisoned");
        if state.lent {
            // Kept for when the session comes back: applying it now would take
            // the microphone from the recording under way.
            state.options = options;
            state.applied = false;
            return Ok(());
        }
        apply(options)?;
        state.options = options;
        state.applied = true;
        Ok(())
    }

    /// The host's recorder takes the session, for a category that records.
    pub(crate) fn lend(&self) {
        self.state
            .lock()
            .expect("the session state is never poisoned")
            .lent = true;
    }

    /// The recorder is done with the session: the category content's options
    /// ask for goes back on, whether or not one was on before -- the recorder
    /// left its own. `false` when it was not lent.
    pub(crate) fn take_back(
        &self,
        apply: impl FnOnce(AudioSessionOptions) -> EngineResult<()>,
    ) -> EngineResult<bool> {
        let mut state = self
            .state
            .lock()
            .expect("the session state is never poisoned");
        if !state.lent {
            return Ok(false);
        }
        state.lent = false;
        // Not applied until it is: a category that failed to go back on is
        // tried again by the next initialization.
        state.applied = false;
        apply(state.options)?;
        state.applied = true;
        Ok(true)
    }

    pub(crate) fn ensure_configured(
        &self,
        apply: impl FnOnce(AudioSessionOptions) -> EngineResult<()>,
    ) -> EngineResult<bool> {
        let mut state = self
            .state
            .lock()
            .expect("the session state is never poisoned");
        if state.applied || state.lent {
            return Ok(false);
        }
        apply(state.options)?;
        state.applied = true;
        Ok(true)
    }
}

pub(crate) fn input_source_names(any: bool, built_in: bool, headset: bool) -> Vec<String> {
    let mut names = Vec::new();
    if any || built_in || headset {
        names.push(crate::platform::AUTO.to_string());
    }
    if built_in {
        names.push("buildInMic".to_string());
    }
    if headset {
        names.push("headsetMic".to_string());
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared::error::{EngineError, ErrorCode};
    use std::sync::Arc;

    #[test]
    fn mini_game_defaults_obey_mute_and_both_categories_play_alone() {
        assert_eq!(
            AudioSessionOptions::default().category(),
            SessionCategory::SoloAmbient
        );
        assert_eq!(SessionCategory::SoloAmbient.name(), "soloAmbient");
        assert_eq!(
            AudioSessionOptions {
                obey_mute_switch: false
            }
            .category(),
            SessionCategory::Playback
        );
        assert_eq!(SessionCategory::Playback.name(), "playback");
    }

    #[test]
    fn input_names_preserve_mini_game_spelling_and_auto_fallback() {
        assert_eq!(
            input_source_names(true, true, false),
            ["auto", "buildInMic"]
        );
        assert_eq!(
            input_source_names(true, true, true),
            ["auto", "buildInMic", "headsetMic"]
        );
        assert_eq!(input_source_names(true, false, false), ["auto"]);
        assert!(input_source_names(false, false, false).is_empty());
    }

    #[test]
    fn failed_application_is_not_cached_and_initialization_can_retry() {
        let configuration = SessionConfiguration::default();
        let options = AudioSessionOptions {
            obey_mute_switch: false,
        };
        assert!(
            configuration
                .configure(options, |_| Err(EngineError::new(ErrorCode::Internal)))
                .is_err()
        );
        assert!(!configuration.state.lock().unwrap().applied);
        configuration
            .ensure_configured(|options| {
                assert_eq!(options, AudioSessionOptions::default());
                Ok(())
            })
            .unwrap();
        configuration
            .ensure_configured(|_| panic!("already initialized"))
            .unwrap();
        configuration.configure(options, |_| Ok(())).unwrap();
        // Reassert explicit requests: the host could have changed its process
        // session category since the previous call, even if our cache agrees.
        let mut called = false;
        configuration
            .configure(options, |_| {
                called = true;
                Ok(())
            })
            .unwrap();
        assert!(called);
        assert_eq!(configuration.state.lock().unwrap().options, options);
    }

    #[test]
    fn a_lent_session_is_left_alone_until_it_is_taken_back() {
        let configuration = SessionConfiguration::default();
        configuration
            .configure(AudioSessionOptions::default(), |_| Ok(()))
            .unwrap();
        configuration.lend();
        // Neither content's options nor a late initialization touch the
        // recorder's category.
        let through_mute = AudioSessionOptions {
            obey_mute_switch: false,
        };
        configuration
            .configure(through_mute, |_| panic!("applied over a recording"))
            .unwrap();
        assert!(
            !configuration
                .ensure_configured(|_| panic!("initialized over a recording"))
                .unwrap()
        );
        // Taken back, the options asked for meanwhile are what goes on.
        let mut applied = None;
        assert!(
            configuration
                .take_back(|options| {
                    applied = Some(options);
                    Ok(())
                })
                .unwrap()
        );
        assert_eq!(applied, Some(through_mute));
        assert!(
            !configuration
                .take_back(|_| panic!("taken back twice"))
                .unwrap()
        );
        configuration
            .ensure_configured(|_| panic!("already applied"))
            .unwrap();
    }

    #[test]
    fn a_category_that_does_not_go_back_on_is_tried_again() {
        let configuration = SessionConfiguration::default();
        configuration.lend();
        assert!(
            configuration
                .take_back(|_| Err(EngineError::new(ErrorCode::Internal)))
                .is_err()
        );
        let mut retried = false;
        assert!(
            configuration
                .ensure_configured(|_| {
                    retried = true;
                    Ok(())
                })
                .unwrap()
        );
        assert!(retried);
    }

    #[test]
    fn native_initialization_retains_configuration_ownership() {
        let configuration = Arc::new(SessionConfiguration::default());
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let initial = configuration.clone();
        let initializer = std::thread::spawn(move || {
            initial.ensure_configured(|_| {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(())
            })
        });
        entered_rx.recv().unwrap();
        let locked_during_native_apply = configuration.state.try_lock().is_err();
        let explicit = configuration.clone();
        let writer = std::thread::spawn(move || {
            explicit.configure(
                AudioSessionOptions {
                    obey_mute_switch: false,
                },
                |_| Ok(()),
            )
        });
        release_tx.send(()).unwrap();
        initializer.join().unwrap().unwrap();
        writer.join().unwrap().unwrap();
        assert!(
            locked_during_native_apply,
            "a stale initialization can overwrite a newer native category if it releases ownership"
        );
        assert!(!configuration.state.lock().unwrap().options.obey_mute_switch);
    }
}

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
        apply(options)?;
        state.options = options;
        state.applied = true;
        Ok(())
    }

    pub(crate) fn ensure_configured(
        &self,
        apply: impl FnOnce(AudioSessionOptions) -> EngineResult<()>,
    ) -> EngineResult<bool> {
        let mut state = self
            .state
            .lock()
            .expect("the session state is never poisoned");
        if state.applied {
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

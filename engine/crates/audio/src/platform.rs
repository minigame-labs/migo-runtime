//! `setInnerAudioOption` and `getAvailableAudioSources` where the engine is the
//! platform's audio: it opens the process's output itself, so the settings
//! that govern that output, and the inputs beside it, are the engine's to
//! answer -- on every system but Android, whose SDK answers from the
//! `AudioManager` it already holds, and OpenHarmony, whose OHAudio stream
//! options are not wired yet.
//!
//! - iOS: the process's `AVAudioSession` ([`crate::apple_session`]).
//! - macOS, Windows and Linux: one shared mixer. Nothing on a desktop takes
//!   another app's audio away, there is no silent switch and no receiver to
//!   route to, so each option has nothing to act on and setting it succeeds,
//!   as the mini-game API's own clients ignore the options their platform has
//!   no use for (`obeyMuteSwitch` off iOS). The input is the system's default
//!   one, which is the API's `auto`.

use std::sync::Arc;

use shared::services::AudioPlatformService;

/// The engine's audio settings on this system, or `None` where the platform's
/// own SDK answers them (Android) or nothing does yet (OpenHarmony).
#[cfg(target_os = "ios")]
pub fn service() -> Option<Arc<dyn AudioPlatformService>> {
    Some(crate::apple_session::platform_service())
}

/// The engine's audio settings on this system, or `None` where the platform's
/// own SDK answers them (Android) or nothing does yet (OpenHarmony).
#[cfg(any(
    target_os = "macos",
    target_os = "windows",
    all(target_os = "linux", not(target_env = "ohos"))
))]
pub fn service() -> Option<Arc<dyn AudioPlatformService>> {
    static SERVICE: std::sync::OnceLock<Arc<desktop::DesktopAudio>> = std::sync::OnceLock::new();
    Some(
        SERVICE
            .get_or_init(|| Arc::new(desktop::DesktopAudio))
            .clone(),
    )
}

/// The engine's audio settings on this system, or `None` where the platform's
/// own SDK answers them (Android) or nothing does yet (OpenHarmony).
#[cfg(not(any(
    target_os = "ios",
    target_os = "macos",
    target_os = "windows",
    all(target_os = "linux", not(target_env = "ohos"))
)))]
pub fn service() -> Option<Arc<dyn AudioPlatformService>> {
    None
}

/// The API's name for the system's default input: on every platform, the
/// microphone the system would record from now.
pub const AUTO: &str = "auto";

#[cfg(any(
    target_os = "macos",
    target_os = "windows",
    all(target_os = "linux", not(target_env = "ohos"))
))]
mod desktop {
    use cpal::traits::HostTrait;
    use shared::protocol::error::ServiceError;
    use shared::services::AudioPlatformService;

    pub(super) struct DesktopAudio;

    impl AudioPlatformService for DesktopAudio {
        /// A desktop has no silent switch to obey.
        fn set_inner_audio_option(&self, _obey_mute_switch: bool) -> Result<(), ServiceError> {
            Ok(())
        }

        fn get_available_audio_sources(&self) -> Result<Vec<String>, ServiceError> {
            Ok(sources(has_default_input()))
        }
    }

    /// The sources a desktop offers: the default input, when there is one.
    pub(super) fn sources(has_input: bool) -> Vec<String> {
        if has_input {
            vec![super::AUTO.to_string()]
        } else {
            Vec::new()
        }
    }

    /// Whether the system names a default input: the system's own setting,
    /// read without touching the device. Asking the device anything -- even
    /// its format -- is microphone access on macOS: it waits on the privacy
    /// prompt (measured: 454 s in a process with no one to answer it, then
    /// refused), and in an app it would ask the player for the microphone
    /// because a game listed its sources. ALSA names `default` whether or not
    /// a capture device stands behind it, so on Linux this is always true.
    fn has_default_input() -> bool {
        cpal::default_host().default_input_device().is_some()
    }
}

#[cfg(all(
    test,
    any(
        target_os = "macos",
        target_os = "windows",
        all(target_os = "linux", not(target_env = "ohos"))
    )
))]
mod tests {
    use super::*;

    /// A desktop offers its default input as `auto`, and nothing without one.
    #[test]
    fn a_desktop_offers_its_default_input() {
        assert_eq!(desktop::sources(true), ["auto"]);
        assert!(desktop::sources(false).is_empty());
    }

    /// Where the engine is the platform, it answers both calls: the options
    /// are accepted, and the inputs are what the system has -- the default
    /// one, or none on a machine with no microphone (a CI runner).
    #[test]
    fn the_engine_answers_on_a_desktop() {
        let service = service().expect("a desktop's audio is the engine's");
        for obey in [true, false] {
            service
                .set_inner_audio_option(obey)
                .expect("a desktop accepts the option");
        }
        let sources = service
            .get_available_audio_sources()
            .expect("a desktop says what it can record from");
        assert!(sources.is_empty() || sources == ["auto"], "{sources:?}");
    }
}

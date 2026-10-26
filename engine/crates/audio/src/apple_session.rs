//! The engine's `AVAudioSession`, on iOS.
//!
//! An iOS process plays through one session, and until it has a category and is
//! active, the output unit this crate opens is silent or refuses to start.
//! There is no host call for that on this lane -- the C ABI has no audio
//! entry point, and the Performance+ host is a Swift app that never sees this
//! crate -- so the engine configures its own session, which is also the only
//! thing `setInnerAudioOption` can act on:
//!
//! - `obeyMuteSwitch` chooses the category: `SoloAmbient` respects the silent
//!   switch, `Playback` plays through it. Neither mixes with other apps' audio:
//!   with the speaker on the reference plays alone whatever `mixWithOther` says,
//!   and that is decided before this is asked (`migo_services::audio`).
//! - `speakerOn` is the speaker, which both categories route to. The receiver
//!   route needs `PlayAndRecord`, and that asks for the microphone -- a
//!   permission this profile does not grant -- so `false` is answered as not
//!   supported, before this is asked.
//!
//! `getAvailableAudioSources` reads the session's inputs: `auto`, and the
//! built-in and headset microphones by the names the API gives them.
//!
//! The session also says when the system took audio away: a phone call, Siri,
//! another app. That arrives as `AVAudioSessionInterruptionNotification`, and
//! this delivers it to the host as `OnAudioInterruptionBegin`/`End`, which both
//! executions hand to content's `onAudioInterruptionBegin` listeners. The
//! hardware stream, session reactivation and ShouldResume policy belong to
//! CPAL. The subscription only forwards public mini-game events; it never drives
//! a second hardware pause/resume gate.
//!
//! Category/input queries reuse CPAL's existing typed AVFAudio bindings. Interruptions reuse
//! CPAL's typed, process-wide observer and native-before-public ordering.

#![cfg(target_os = "ios")]

use std::sync::{Arc, OnceLock};

use objc2_avf_audio::{
    AVAudioSession, AVAudioSessionCategoryOptions, AVAudioSessionCategoryPlayback,
    AVAudioSessionCategorySoloAmbient, AVAudioSessionPortBuiltInMic, AVAudioSessionPortHeadsetMic,
};
use shared::error::{EngineError, EngineResult, ErrorCode};
use shared::protocol::host_cmd::HostCommand;
use tracing::{info, warn};

pub use crate::apple_session_policy::AudioSessionOptions;
use crate::apple_session_policy::{SessionCategory, SessionConfiguration, input_source_names};

fn configuration() -> &'static SessionConfiguration {
    static STATE: OnceLock<SessionConfiguration> = OnceLock::new();
    STATE.get_or_init(SessionConfiguration::default)
}

/// Subscribe through CPAL's process-session observer, after its native state
/// transition. The guard is owned by this audio thread, including silent-device
/// fallback; shutdown unregisters it. No process-wide list retains dead hosts.
pub fn watch_interruptions(
    host_tx: shared::host_channel::HostCommandSender,
) -> cpal::platform::InterruptionObserver {
    cpal::platform::observe_audio_session_interruptions(move |event| {
        let command = match event {
            cpal::platform::InterruptionEvent::Began => HostCommand::OnAudioInterruptionBegin,
            cpal::platform::InterruptionEvent::Ended { .. } => HostCommand::OnAudioInterruptionEnd,
        };
        // Use the existing reserved transition lane. Saturation cannot delete
        // the subscription: later events still have the same live observer.
        let _ = host_tx.try_send_reliable(None, command);
    })
}

/// Apply the mini-game category. CPAL owns activation when the stream plays.
/// Uses the same typed AVFAudio cohort CPAL already needs, including ownership
/// of the singleton and NSError. No raw objc_msgSend ABI or selector allocation.
fn apply_category(options: AudioSessionOptions) -> EngineResult<()> {
    let category = options.category();
    // SAFETY: AVAudioSession and both categories are available on the selected
    // iOS15+ product. Retained owns the returned session; no capture is opened.
    let session = unsafe { AVAudioSession::sharedInstance() };
    let native_category = unsafe {
        match category {
            SessionCategory::SoloAmbient => AVAudioSessionCategorySoloAmbient,
            SessionCategory::Playback => AVAudioSessionCategoryPlayback,
        }
    }
    .ok_or_else(|| {
        EngineError::from_detail(
            ErrorCode::Unsupported,
            "setInnerAudioOption:fail this process has no requested audio session category",
        )
    })?;
    // Neither category mixes with other apps. The category options deliberately
    // stay empty; no receiver/input category or microphone permission is added.
    unsafe {
        session
            .setCategory_withOptions_error(native_category, AVAudioSessionCategoryOptions::empty())
    }
    .map_err(|error| {
        EngineError::from_detail(
            ErrorCode::Internal,
            format!(
                "setInnerAudioOption:fail the audio session refused the category (NSError code {})",
                error.code()
            ),
        )
    })?;
    Ok(())
}

pub fn configure(options: AudioSessionOptions) -> EngineResult<()> {
    configuration().configure(options, apply_category)?;
    // Subscriber code must not run while category configuration is locked.
    info!(
        "audio session configured: category={}",
        options.category().name()
    );
    Ok(())
}

pub fn ensure_configured() {
    match configuration().ensure_configured(apply_category) {
        Ok(true) => info!("audio session initialized"),
        Ok(false) => {}
        // The host may already have a usable category. Device open still has
        // its normal silent-output fallback and subsequent recovery policy.
        Err(error) => warn!("the audio session was not configured: {error}"),
    }
}

/// The inputs the session offers now, in the mini-game API's words: `auto`
/// first whenever there is one, then the built-in and wired-headset
/// microphones by name (`buildInMic`, `headsetMic`, iOS's two). Any other port
/// -- a Bluetooth headset, a USB interface -- is reached through `auto`.
///
/// A category that cannot record lists no ports; the session still says
/// whether any input exists, which is then all `auto` can stand for.
fn available_sources() -> EngineResult<Vec<String>> {
    // SAFETY: these read-only iOS15+ session/port APIs return retained typed
    // objects. They do not open an input stream or request microphone access.
    let session = unsafe { AVAudioSession::sharedInstance() };
    let (mut built_in, mut headset, mut any) = (false, false, false);
    if let Some(inputs) = unsafe { session.availableInputs() } {
        for port in inputs.iter() {
            let kind = unsafe { port.portType() };
            any = true;
            built_in |= unsafe { AVAudioSessionPortBuiltInMic }
                .is_some_and(|name| kind.isEqualToString(name));
            headset |= unsafe { AVAudioSessionPortHeadsetMic }
                .is_some_and(|name| kind.isEqualToString(name));
        }
    }
    any |= unsafe { session.isInputAvailable() };
    Ok(input_source_names(any, built_in, headset))
}

/// `setInnerAudioOption` and `getAvailableAudioSources`, as the platform audio
/// service.
pub struct AppleAudioPlatform;

impl shared::services::AudioPlatformService for AppleAudioPlatform {
    fn set_inner_audio_option(
        &self,
        obey_mute_switch: bool,
    ) -> Result<(), shared::protocol::error::ServiceError> {
        configure(AudioSessionOptions { obey_mute_switch }).map_err(|error| {
            shared::protocol::error::ServiceError::system(
                error.detail.unwrap_or_else(|| error.msg.to_string()),
            )
        })
    }

    fn get_available_audio_sources(
        &self,
    ) -> Result<Vec<String>, shared::protocol::error::ServiceError> {
        available_sources().map_err(|error| {
            shared::protocol::error::ServiceError::system(
                error.detail.unwrap_or_else(|| error.msg.to_string()),
            )
        })
    }
}

/// The platform's audio service: the process's session, which is the engine's
/// on this platform.
pub fn platform_service() -> Arc<dyn shared::services::AudioPlatformService> {
    static SERVICE: OnceLock<Arc<AppleAudioPlatform>> = OnceLock::new();
    SERVICE.get_or_init(|| Arc::new(AppleAudioPlatform)).clone()
}

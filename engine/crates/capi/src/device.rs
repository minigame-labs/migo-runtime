//! Device capabilities for a C host: what content asks the host to do
//! (vibrate, keep the display awake, keep a log entry) and what the host tells
//! content about the device (its network and battery).
//!
//! The first three are callbacks, offered to content exactly when the host
//! installed them -- the rule the keyboard follows, and for the same reason: a
//! capability the host did not supply must fail the way it fails on a device
//! without it, not succeed into nothing.
//!
//! The network and the battery are the other direction. Content reads them
//! synchronously, and a callback cannot answer synchronously -- it runs on the
//! host's dispatcher, whenever the host gets to it. So the host reports each
//! when it changes and the engine answers from the last report. That is also
//! how the platforms already work: iOS and Android both *notify* a change, and
//! a read between notifications is the last value anyway.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

use migo_capi_abi::{
    MIGO_ERROR_INTERNAL, MIGO_ERROR_INVALID_ARGUMENT, MIGO_OK, MigoResult,
    callbacks::{
        MIGO_VIBRATION_LONG, MIGO_VIBRATION_SHORT_HEAVY, MIGO_VIBRATION_SHORT_LIGHT,
        MIGO_VIBRATION_SHORT_MEDIUM,
    },
};
use migo_core::services::{
    BatteryService, GameLogService, NetworkService, ScreenService, VibrationService,
};
use shared::protocol::error::ServiceError;
use shared::protocol::host_cmd::HostCommand;

use crate::{
    MigoSession, SurfaceTransition, callbacks::Notifier, panic_barrier::guard, pin_session,
};

/// `MIGO_NETWORK_*` from `include/migo/session.h`, in the API's own words.
const NETWORK_TYPES: [&str; 7] = ["none", "wifi", "unknown", "2g", "3g", "4g", "5g"];
const MIGO_NETWORK_NONE: u32 = 0;

/// `MIGO_BATTERY_FLAG_*`.
const MIGO_BATTERY_FLAG_CHARGING: u32 = 1 << 0;
const MIGO_BATTERY_FLAG_LOW_POWER_MODE: u32 = 1 << 1;
const MIGO_BATTERY_FLAGS_KNOWN: u32 = MIGO_BATTERY_FLAG_CHARGING | MIGO_BATTERY_FLAG_LOW_POWER_MODE;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Network {
    type_index: usize,
    connected: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Battery {
    level_percent: u32,
    flags: u32,
}

/// What the host last reported, per Session. It outlives Hosts: a report made
/// before the first attach, or between two games, is still the device's state.
#[derive(Default)]
pub(crate) struct DeviceState {
    network: Mutex<Option<Network>>,
    battery: Mutex<Option<Battery>>,
    /// Whether content is listening for network changes. A change reported
    /// while nobody listens is kept but not delivered: waking content's thread
    /// to tell no listener would cost a frame's work for nothing.
    network_monitored: AtomicBool,
}

impl DeviceState {
    fn network(&self) -> Option<Network> {
        *self
            .network
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn battery(&self) -> Option<Battery> {
        *self
            .battery
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// The services a C host's device capabilities become, for one Host.
pub(crate) struct CapiDevice {
    notifier: Option<Arc<Notifier>>,
    state: Arc<DeviceState>,
}

impl CapiDevice {
    pub(crate) fn new(notifier: Option<Arc<Notifier>>, state: Arc<DeviceState>) -> Arc<Self> {
        Arc::new(Self { notifier, state })
    }

    fn notifier_if(&self, supplied: impl Fn(&Notifier) -> bool) -> bool {
        self.notifier.as_deref().is_some_and(supplied)
    }

    pub(crate) fn vibration(self: &Arc<Self>) -> Option<Arc<dyn VibrationService>> {
        self.notifier_if(Notifier::supplies_vibration)
            .then(|| Arc::clone(self) as Arc<dyn VibrationService>)
    }

    pub(crate) fn screen(self: &Arc<Self>) -> Option<Arc<dyn ScreenService>> {
        self.notifier_if(Notifier::supplies_keep_screen_on)
            .then(|| Arc::clone(self) as Arc<dyn ScreenService>)
    }

    pub(crate) fn game_log(self: &Arc<Self>) -> Option<Arc<dyn GameLogService>> {
        self.notifier_if(Notifier::supplies_game_log)
            .then(|| Arc::clone(self) as Arc<dyn GameLogService>)
    }

    /// Always offered: whether there is anything to answer with is decided per
    /// call by whether the host has reported yet, which can change mid-game.
    pub(crate) fn battery(self: &Arc<Self>) -> Arc<dyn BatteryService> {
        Arc::clone(self) as Arc<dyn BatteryService>
    }

    pub(crate) fn network(self: &Arc<Self>) -> Arc<dyn NetworkService> {
        Arc::clone(self) as Arc<dyn NetworkService>
    }

    fn post(&self, what: &str, post: impl FnOnce(&Notifier) -> bool) -> Result<(), ServiceError> {
        match self.notifier.as_deref() {
            Some(notifier) if post(notifier) => Ok(()),
            _ => Err(ServiceError::not_supported(format!(
                "{what}:fail host dispatcher refused the request"
            ))),
        }
    }
}

impl VibrationService for CapiDevice {
    fn vibrate_short(&self, type_: &str) -> Result<(), ServiceError> {
        // The API's own values; anything else is what the platform does with
        // no type, which is the medium strength.
        let vibration = match type_ {
            "light" => MIGO_VIBRATION_SHORT_LIGHT,
            "heavy" => MIGO_VIBRATION_SHORT_HEAVY,
            _ => MIGO_VIBRATION_SHORT_MEDIUM,
        };
        self.post("vibrateShort", |notifier| notifier.vibrate(vibration))
    }

    fn vibrate_long(&self) -> Result<(), ServiceError> {
        self.post("vibrateLong", |notifier| {
            notifier.vibrate(MIGO_VIBRATION_LONG)
        })
    }
}

/// Only the one screen capability the ABI carries; the rest keep the trait's
/// "not supported" answers.
impl ScreenService for CapiDevice {
    fn set_keep_screen_on(&self, keep_on: bool) -> Result<(), ServiceError> {
        self.post("setKeepScreenOn", |notifier| {
            notifier.keep_screen_on(keep_on)
        })
    }
}

impl GameLogService for CapiDevice {
    fn report_log(&self, log_json: &str) -> Result<(), ServiceError> {
        self.post("gameLog.log", |notifier| {
            notifier.game_log(log_json.to_owned())
        })
    }
}

impl BatteryService for CapiDevice {
    fn get_info_json(&self) -> Result<String, ServiceError> {
        let Some(battery) = self.state.battery() else {
            return Err(ServiceError::not_supported(
                "getBatteryInfo:fail not supported",
            ));
        };
        // The level is a string in the API's answer, as the Android SDK sends it.
        Ok(format!(
            r#"{{"level":"{}","isCharging":{},"isLowPowerModeEnabled":{}}}"#,
            battery.level_percent,
            battery.flags & MIGO_BATTERY_FLAG_CHARGING != 0,
            battery.flags & MIGO_BATTERY_FLAG_LOW_POWER_MODE != 0,
        ))
    }
}

impl NetworkService for CapiDevice {
    fn start_monitoring(&self) -> Result<(), ServiceError> {
        if self.state.network().is_none() {
            return Err(ServiceError::not_supported(
                "onNetworkStatusChange:fail not supported",
            ));
        }
        self.state.network_monitored.store(true, Ordering::Release);
        Ok(())
    }

    fn stop_monitoring(&self) -> Result<(), ServiceError> {
        self.state.network_monitored.store(false, Ordering::Release);
        Ok(())
    }

    fn get_network_type_json(&self) -> Result<String, ServiceError> {
        let Some(network) = self.state.network() else {
            return Err(ServiceError::not_supported(
                "getNetworkType:fail not supported",
            ));
        };
        Ok(format!(
            r#"{{"networkType":"{}","isConnected":{}}}"#,
            NETWORK_TYPES[network.type_index], network.connected
        ))
    }
}

/// # Safety
/// `session` must be a live session handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn migo_session_set_network_status(
    session: *mut MigoSession,
    network_type: u32,
    connected: u8,
) -> MigoResult {
    guard("migo_session_set_network_status", || {
        let session = match unsafe { pin_session(session) } {
            Ok(session) => session,
            Err(error) => return error,
        };
        let connected = match migo_capi_abi::validate::validate_bool(connected) {
            Ok(connected) => connected,
            Err(error) => return error,
        };
        let type_index = network_type as usize;
        if type_index >= NETWORK_TYPES.len() || (network_type == MIGO_NETWORK_NONE && connected) {
            return MIGO_ERROR_INVALID_ARGUMENT;
        }
        let reported = Network {
            type_index,
            connected,
        };

        // SessionControl first, then the report: a Host attaching concurrently
        // either sees this report when content asks, or is the Host this sends
        // the change to -- never neither.
        let Ok(state) = session.state.lock() else {
            return MIGO_ERROR_INTERNAL;
        };
        let device = &session.device;
        let previous = {
            let mut network = device
                .network
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            network.replace(reported)
        };
        if previous == Some(reported) || !device.network_monitored.load(Ordering::Acquire) {
            return MIGO_OK;
        }
        let deliverable = state.active_attachment.is_some()
            && state.surface_transition != SurfaceTransition::Detaching;
        let Some(host) = state
            .host
            .as_ref()
            .filter(|_| deliverable)
            .map(crate::SessionEngine::id)
        else {
            return MIGO_OK;
        };
        drop(state);
        // The report is kept whether or not the Host takes the event: content
        // reading the type afterwards gets it either way.
        if let Err(error) = migo_core::send_reliable_command_to_host(
            host,
            HostCommand::OnNetworkStatusChange {
                is_connected: connected,
                network_type: std::borrow::Cow::Borrowed(NETWORK_TYPES[type_index]),
            },
        ) {
            tracing::warn!("migo_session_set_network_status: change not delivered: {error}");
        }
        MIGO_OK
    })
}

/// # Safety
/// `session` must be a live session handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn migo_session_set_battery_status(
    session: *mut MigoSession,
    level_percent: u32,
    flags: u32,
) -> MigoResult {
    guard("migo_session_set_battery_status", || {
        let session = match unsafe { pin_session(session) } {
            Ok(session) => session,
            Err(error) => return error,
        };
        if level_percent > 100 || flags & !MIGO_BATTERY_FLAGS_KNOWN != 0 {
            return MIGO_ERROR_INVALID_ARGUMENT;
        }
        *session
            .device
            .battery
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Battery {
            level_percent,
            flags,
        });
        MIGO_OK
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::callbacks::{MigoHostCallbacks, MigoTaskFn};
    use crate::test_support::{callback_session_pin, with_session};
    use std::ffi::c_void;
    use std::os::raw::c_char;

    unsafe extern "C" fn inline_dispatch(
        _dispatcher: *mut c_void,
        task: MigoTaskFn,
        context: *mut c_void,
    ) -> MigoResult {
        unsafe { task(context) };
        MIGO_OK
    }

    /// What the host's device callbacks heard, in order. Per test: `user_data`
    /// points at it, so tests running in parallel never share one.
    #[derive(Default)]
    struct Heard(Mutex<Vec<String>>);

    unsafe fn heard<'a>(user: *mut c_void) -> &'a Heard {
        unsafe { &*(user as *const Heard) }
    }
    unsafe extern "C" fn on_vibrate(user: *mut c_void, _session: *mut c_void, vibration: u32) {
        unsafe { heard(user) }
            .0
            .lock()
            .unwrap()
            .push(format!("vibrate {vibration}"));
    }
    unsafe extern "C" fn on_keep_screen_on(user: *mut c_void, _session: *mut c_void, keep_on: u8) {
        unsafe { heard(user) }
            .0
            .lock()
            .unwrap()
            .push(format!("keep {keep_on}"));
    }
    unsafe extern "C" fn on_game_log(
        user: *mut c_void,
        _session: *mut c_void,
        entry: *const c_char,
        length: u32,
    ) {
        let bytes = unsafe { std::slice::from_raw_parts(entry.cast::<u8>(), length as usize) };
        let entry = std::str::from_utf8(bytes).expect("utf-8");
        unsafe { heard(user) }
            .0
            .lock()
            .unwrap()
            .push(format!("log {entry}"));
    }

    fn device_with(heard: &Heard, installed: bool) -> (Arc<MigoSession>, Arc<CapiDevice>) {
        let mut raw = MigoHostCallbacks::empty();
        raw.user_data = heard as *const Heard as *mut c_void;
        raw.dispatch = Some(inline_dispatch);
        if installed {
            raw.on_vibrate = Some(on_vibrate);
            raw.on_keep_screen_on = Some(on_keep_screen_on);
            raw.on_game_log = Some(on_game_log);
        }
        let session = callback_session_pin();
        let notifier = Arc::new(Notifier::new(
            raw.validate().expect("valid").expect("a dispatcher"),
            Arc::downgrade(&session),
        ));
        let device = CapiDevice::new(Some(notifier), Arc::clone(&session.device));
        (session, device)
    }

    /// Content's calls reach the callbacks the host installed, with the API's
    /// strengths mapped onto the header's values -- an unknown one is the
    /// platform's default, medium, not a refusal.
    #[test]
    fn installed_device_callbacks_hear_content_s_requests() {
        let heard = Heard::default();
        let (_session, device) = device_with(&heard, true);
        let vibration = device.vibration().expect("offered");
        vibration.vibrate_short("light").unwrap();
        vibration.vibrate_short("heavy").unwrap();
        vibration.vibrate_short("medium").unwrap();
        vibration.vibrate_short("").unwrap();
        vibration.vibrate_long().unwrap();
        device
            .screen()
            .expect("offered")
            .set_keep_screen_on(true)
            .unwrap();
        device
            .screen()
            .expect("offered")
            .set_keep_screen_on(false)
            .unwrap();
        device
            .game_log()
            .expect("offered")
            .report_log(r#"{"key":"k"}"#)
            .unwrap();
        assert_eq!(
            *heard.0.lock().unwrap(),
            [
                format!("vibrate {MIGO_VIBRATION_SHORT_LIGHT}"),
                format!("vibrate {MIGO_VIBRATION_SHORT_HEAVY}"),
                format!("vibrate {MIGO_VIBRATION_SHORT_MEDIUM}"),
                format!("vibrate {MIGO_VIBRATION_SHORT_MEDIUM}"),
                format!("vibrate {MIGO_VIBRATION_LONG}"),
                "keep 1".to_string(),
                "keep 0".to_string(),
                r#"log {"key":"k"}"#.to_string(),
            ]
        );
    }

    /// Offered exactly when installed: a host without a vibrator must make
    /// content's `vibrateShort` fail, as a phone without one does.
    #[test]
    fn device_callbacks_left_null_are_not_offered() {
        let heard = Heard::default();
        let (_session, device) = device_with(&heard, false);
        assert!(device.vibration().is_none());
        assert!(device.screen().is_none());
        assert!(device.game_log().is_none());
    }

    /// The battery and the network answer from the host's last report, fail
    /// in the platform's words before the first one, and keep monitoring off
    /// until there is something to monitor.
    #[test]
    fn battery_and_network_answer_the_host_s_last_report() {
        with_session("device-report", |raw| {
            let session = unsafe { pin_session(raw) }.expect("live");
            let device = CapiDevice::new(None, Arc::clone(&session.device));
            let battery = device.battery();
            let network = device.network();
            assert_eq!(
                battery.get_info_json().unwrap_err().message,
                "getBatteryInfo:fail not supported"
            );
            assert_eq!(
                network.get_network_type_json().unwrap_err().message,
                "getNetworkType:fail not supported"
            );
            assert!(
                network.start_monitoring().is_err(),
                "nothing to monitor yet"
            );

            assert_eq!(
                unsafe {
                    migo_session_set_battery_status(
                        raw,
                        85,
                        MIGO_BATTERY_FLAG_CHARGING | MIGO_BATTERY_FLAG_LOW_POWER_MODE,
                    )
                },
                MIGO_OK
            );
            assert_eq!(
                battery.get_info_json().unwrap(),
                r#"{"level":"85","isCharging":true,"isLowPowerModeEnabled":true}"#
            );
            assert_eq!(
                unsafe { migo_session_set_battery_status(raw, 7, 0) },
                MIGO_OK
            );
            assert_eq!(
                battery.get_info_json().unwrap(),
                r#"{"level":"7","isCharging":false,"isLowPowerModeEnabled":false}"#
            );

            // No Surface is attached: the report is kept, and nothing is sent.
            assert_eq!(
                unsafe { migo_session_set_network_status(raw, 1, 1) },
                MIGO_OK
            );
            assert_eq!(
                network.get_network_type_json().unwrap(),
                r#"{"networkType":"wifi","isConnected":true}"#
            );
            network.start_monitoring().expect("a report exists");
            assert_eq!(
                unsafe { migo_session_set_network_status(raw, 0, 0) },
                MIGO_OK
            );
            assert_eq!(
                network.get_network_type_json().unwrap(),
                r#"{"networkType":"none","isConnected":false}"#
            );
            assert_eq!(
                unsafe { migo_session_set_network_status(raw, 6, 1) },
                MIGO_OK
            );
            assert_eq!(
                network.get_network_type_json().unwrap(),
                r#"{"networkType":"5g","isConnected":true}"#
            );
        });
    }

    /// A report the header does not define is refused and leaves the last
    /// good one in place.
    #[test]
    fn reports_outside_the_header_are_refused() {
        with_session("device-refused", |raw| {
            for (network_type, connected) in [(7, 1), (u32::MAX, 0), (0, 1), (1, 2)] {
                assert_eq!(
                    unsafe { migo_session_set_network_status(raw, network_type, connected) },
                    MIGO_ERROR_INVALID_ARGUMENT,
                    "type {network_type} connected {connected}"
                );
            }
            for (level, flags) in [(101, 0), (50, 1 << 2)] {
                assert_eq!(
                    unsafe { migo_session_set_battery_status(raw, level, flags) },
                    MIGO_ERROR_INVALID_ARGUMENT,
                    "level {level} flags {flags}"
                );
            }
            let session = unsafe { pin_session(raw) }.expect("live");
            assert!(session.device.network().is_none());
            assert!(session.device.battery().is_none());
        });
    }
}

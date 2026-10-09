//! What the host has decided and what its device says: content's permission
//! scopes, the system switches, and the app's own OS authorisations.
//!
//! These are standing answers content reads synchronously -- `getSetting`
//! during layout, a scope check on every gated call -- so, like the battery and
//! the network, the host reports them when they change and the engine answers
//! from the last report. A callback cannot answer synchronously.
//!
//! Each is a closed set, so each has a typed entry point rather than a JSON
//! payload: a host should not need a JSON parser to grant the camera.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU8, AtomicU32, Ordering},
};

use migo_capi_abi::{
    MIGO_ERROR_INTERNAL, MIGO_ERROR_INVALID_ARGUMENT, MIGO_OK, MigoResult,
    host_services::{
        MIGO_AUTHORIZATION_AUTHORIZED, MIGO_AUTHORIZATION_DENIED, MIGO_HOST_SERVICE_PERMISSION,
        MIGO_HOST_SERVICE_SETTING, MIGO_SCOPE_COUNT, MIGO_SCOPE_STATE_DENIED,
        MIGO_SCOPE_STATE_GRANTED, MIGO_SCOPE_STATE_UNKNOWN,
        MIGO_SYSTEM_SETTING_FLAG_BLUETOOTH_ENABLED, MIGO_SYSTEM_SETTING_FLAG_LOCATION_ENABLED,
        MIGO_SYSTEM_SETTING_FLAG_WIFI_ENABLED, MIGO_SYSTEM_SETTING_FLAGS_KNOWN,
        MigoAppAuthorizeSetting,
    },
};
use migo_core::services::{Scope, ScopeState, ServiceError, SystemInfoService};
use shared::device::{Orientation, SystemSettings};
use shared::surface::{HostWindowInfo, HostWindowState};

use crate::{MigoSession, host_services::CapiHostServices, panic_barrier::guard, pin_session};

/// The host's standing reports, per Session. Like the battery's, they outlive
/// Hosts: a decision recorded before the first attach is still the decision.
pub(crate) struct HostReports {
    /// One `MIGO_SCOPE_STATE_*` per scope, indexed by `MIGO_SCOPE_*`, which is
    /// `Scope::ALL`'s order. Atomic because every gated call reads one.
    scopes: [AtomicU8; MIGO_SCOPE_COUNT as usize],
    /// `MIGO_SYSTEM_SETTING_FLAG_*`. Nothing reported reads as every switch
    /// off: a switch nobody said is on is not one content may rely on.
    system_settings: AtomicU32,
    app_authorize: Mutex<Option<MigoAppAuthorizeSetting>>,
}

impl Default for HostReports {
    fn default() -> Self {
        Self {
            scopes: std::array::from_fn(|_| AtomicU8::new(MIGO_SCOPE_STATE_UNKNOWN as u8)),
            system_settings: AtomicU32::new(0),
            app_authorize: Mutex::new(None),
        }
    }
}

/// `MIGO_SCOPE_*` is `Scope::ALL`'s index; the test below holds the two together.
fn scope_index(scope: Scope) -> usize {
    Scope::ALL
        .iter()
        .position(|candidate| *candidate == scope)
        .expect("Scope::ALL lists every scope")
}

impl HostReports {
    pub(crate) fn scope_state(&self, scope: Scope) -> ScopeState {
        match u32::from(self.scopes[scope_index(scope)].load(Ordering::Acquire)) {
            MIGO_SCOPE_STATE_GRANTED => ScopeState::Granted,
            MIGO_SCOPE_STATE_DENIED => ScopeState::Denied,
            _ => ScopeState::Unknown,
        }
    }

    #[cfg(test)]
    pub(crate) fn record_scope_for_test(&self, scope: Scope, state: ScopeState) {
        let value = match state {
            ScopeState::Unknown => MIGO_SCOPE_STATE_UNKNOWN,
            ScopeState::Granted => MIGO_SCOPE_STATE_GRANTED,
            ScopeState::Denied => MIGO_SCOPE_STATE_DENIED,
        };
        self.scopes[scope_index(scope)].store(value as u8, Ordering::Release);
    }

    fn system_settings(&self) -> u32 {
        self.system_settings.load(Ordering::Acquire)
    }

    fn app_authorize(&self) -> Option<MigoAppAuthorizeSetting> {
        *self
            .app_authorize
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// `getAppAuthorizeSetting`'s words for a `MIGO_AUTHORIZATION_*`.
fn authorization_word(state: u8) -> &'static str {
    match state {
        MIGO_AUTHORIZATION_AUTHORIZED => "authorized",
        MIGO_AUTHORIZATION_DENIED => "denied",
        _ => "not determined",
    }
}

/// System information for a C host: the window it attached, the switches and
/// authorisations it reported, and -- when it declared the setting service --
/// the settings pages it can open.
pub(crate) struct CapiSystemInfo {
    window: Arc<HostWindowState>,
    reports: Arc<HostReports>,
    /// Present exactly when the host declared `MIGO_HOST_SERVICE_SETTING`.
    settings: Option<Arc<CapiHostServices>>,
}

impl CapiSystemInfo {
    pub(crate) fn new(
        window: Arc<HostWindowState>,
        reports: Arc<HostReports>,
        services: Option<&Arc<CapiHostServices>>,
    ) -> Self {
        Self {
            window,
            reports,
            settings: services
                .filter(|services| services.supplies(MIGO_HOST_SERVICE_SETTING))
                .cloned(),
        }
    }
}

impl SystemInfoService for CapiSystemInfo {
    fn get_window_info_json(&self) -> Result<String, ServiceError> {
        HostWindowInfo::new(Arc::clone(&self.window)).get_window_info_json()
    }

    /// The switches as reported, and the orientation the attached window has --
    /// which the engine knows better than any report could.
    fn get_system_settings_json(&self) -> Result<String, ServiceError> {
        let flags = self.reports.system_settings();
        let metrics = self.window.snapshot();
        let settings = SystemSettings {
            bluetooth_enabled: flags & MIGO_SYSTEM_SETTING_FLAG_BLUETOOTH_ENABLED != 0,
            location_enabled: flags & MIGO_SYSTEM_SETTING_FLAG_LOCATION_ENABLED != 0,
            wifi_enabled: flags & MIGO_SYSTEM_SETTING_FLAG_WIFI_ENABLED != 0,
            orientation: if metrics.width_pixels() > metrics.height_pixels() {
                Orientation::Landscape
            } else {
                Orientation::Portrait
            },
        };
        serde_json::to_string(&settings).map_err(|error| ServiceError::system(error.to_string()))
    }

    /// Before the host reports, every authorisation is undetermined -- which is
    /// the truth about an answer nobody has given.
    fn get_app_authorization_setting_json(&self) -> Result<String, ServiceError> {
        let report = self.reports.app_authorize();
        let word = |pick: fn(&MigoAppAuthorizeSetting) -> u8| {
            report
                .as_ref()
                .map_or("not determined", |r| authorization_word(pick(r)))
        };
        Ok(serde_json::json!({
            "albumAuthorized": word(|r| r.album),
            "bluetoothAuthorized": word(|r| r.bluetooth),
            "cameraAuthorized": word(|r| r.camera),
            "locationAuthorized": word(|r| r.location),
            "locationReducedAccuracy": report.is_some_and(|r| r.location_reduced_accuracy == 1),
            "microphoneAuthorized": word(|r| r.microphone),
            "notificationAuthorized": word(|r| r.notification),
            "notificationAlertAuthorized": word(|r| r.notification_alert),
            "notificationBadgeAuthorized": word(|r| r.notification_badge),
            "notificationSoundAuthorized": word(|r| r.notification_sound),
            "phoneCalendarAuthorized": word(|r| r.phone_calendar),
        })
        .to_string())
    }

    fn open_setting(&self, options_json: &str) -> Result<(), ServiceError> {
        match &self.settings {
            Some(settings) => settings.open_setting(options_json),
            None => Err(ServiceError::not_supported(
                "openSetting:fail not supported",
            )),
        }
    }

    fn open_bluetooth_settings(&self, request_id: i32) -> Result<(), ServiceError> {
        match &self.settings {
            Some(settings) => settings.open_system_bluetooth_setting(request_id),
            None => Err(ServiceError::not_supported(
                "openSystemBluetoothSetting:fail not supported",
            )),
        }
    }

    fn open_app_authorize_setting(&self, request_id: i32) -> Result<(), ServiceError> {
        match &self.settings {
            Some(settings) => settings.open_app_authorize_setting(request_id),
            None => Err(ServiceError::not_supported(
                "openAppAuthorizeSetting:fail not supported",
            )),
        }
    }
}

// ---- entry points ---------------------------------------------------------------

/// # Safety
/// `session` must be a live session handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn migo_session_set_scope_state(
    session: *mut MigoSession,
    scope: u32,
    state: u32,
) -> MigoResult {
    guard("migo_session_set_scope_state", || {
        let session = match unsafe { pin_session(session) } {
            Ok(session) => session,
            Err(error) => return error,
        };
        if scope >= MIGO_SCOPE_COUNT || state > MIGO_SCOPE_STATE_DENIED {
            return MIGO_ERROR_INVALID_ARGUMENT;
        }
        // A decision for a host that supplies no permission service would be
        // read by nobody -- the engine denies every scope without one -- and a
        // grant that silently does nothing is the failure to report.
        let Ok(control) = session.state.lock() else {
            return MIGO_ERROR_INTERNAL;
        };
        let declared = control
            .callbacks
            .as_ref()
            .is_some_and(|callbacks| callbacks.supplies_host_service(MIGO_HOST_SERVICE_PERMISSION));
        drop(control);
        if !declared {
            return MIGO_ERROR_INVALID_ARGUMENT;
        }
        session.host_reports.scopes[scope as usize].store(state as u8, Ordering::Release);
        MIGO_OK
    })
}

/// # Safety
/// `session` must be a live session handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn migo_session_set_system_settings(
    session: *mut MigoSession,
    flags: u32,
) -> MigoResult {
    guard("migo_session_set_system_settings", || {
        let session = match unsafe { pin_session(session) } {
            Ok(session) => session,
            Err(error) => return error,
        };
        if flags & !MIGO_SYSTEM_SETTING_FLAGS_KNOWN != 0 {
            return MIGO_ERROR_INVALID_ARGUMENT;
        }
        session
            .host_reports
            .system_settings
            .store(flags, Ordering::Release);
        MIGO_OK
    })
}

/// # Safety
/// `session` must be a live session handle and `setting` null or readable for
/// its announced size.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn migo_session_set_app_authorize_setting(
    session: *mut MigoSession,
    setting: *const MigoAppAuthorizeSetting,
) -> MigoResult {
    guard("migo_session_set_app_authorize_setting", || {
        let session = match unsafe { pin_session(session) } {
            Ok(session) => session,
            Err(error) => return error,
        };
        let report = match unsafe { MigoAppAuthorizeSetting::parse(setting) } {
            Ok(report) => report,
            Err(error) => return error,
        };
        *session
            .host_reports
            .app_authorize
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(report);
        MIGO_OK
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        callbacks::{MigoHostCallbacks, MigoTaskFn},
        test_support::with_session,
    };
    use migo_capi_abi::{
        MIGO_ABI_VERSION_CURRENT, VersionedHeader,
        host_services::{
            MIGO_SCOPE_ADD_PHONE_CALENDAR, MIGO_SCOPE_ADD_PHONE_CONTACT, MIGO_SCOPE_ADDRESS,
            MIGO_SCOPE_BLUETOOTH, MIGO_SCOPE_CAMERA, MIGO_SCOPE_FRIEND_INTERACTION,
            MIGO_SCOPE_GAME_CLUB_DATA, MIGO_SCOPE_INVOICE, MIGO_SCOPE_INVOICE_TITLE,
            MIGO_SCOPE_RECORD, MIGO_SCOPE_USER_INFO, MIGO_SCOPE_USER_LOCATION,
            MIGO_SCOPE_USER_LOCATION_BACKGROUND, MIGO_SCOPE_WERUN, MIGO_SCOPE_WRITE_PHOTOS_ALBUM,
            MigoHostServiceCall,
        },
    };
    use shared::surface::{HostWindowMetrics, PixelRatio};
    use std::ffi::c_void;

    /// The header's numbers are `Scope::ALL`'s order, scope by scope, and cover
    /// every scope: a scope added to the enum without a number here is one a C
    /// host could never grant.
    #[test]
    fn scope_numbers_are_the_scope_order() {
        let numbered = [
            (MIGO_SCOPE_USER_INFO, Scope::UserInfo),
            (MIGO_SCOPE_USER_LOCATION, Scope::UserLocation),
            (
                MIGO_SCOPE_USER_LOCATION_BACKGROUND,
                Scope::UserLocationBackground,
            ),
            (MIGO_SCOPE_ADDRESS, Scope::Address),
            (MIGO_SCOPE_INVOICE_TITLE, Scope::InvoiceTitle),
            (MIGO_SCOPE_INVOICE, Scope::Invoice),
            (MIGO_SCOPE_WERUN, Scope::WeRun),
            (MIGO_SCOPE_RECORD, Scope::Record),
            (MIGO_SCOPE_WRITE_PHOTOS_ALBUM, Scope::WritePhotosAlbum),
            (MIGO_SCOPE_CAMERA, Scope::Camera),
            (MIGO_SCOPE_BLUETOOTH, Scope::Bluetooth),
            (MIGO_SCOPE_ADD_PHONE_CONTACT, Scope::AddPhoneContact),
            (MIGO_SCOPE_ADD_PHONE_CALENDAR, Scope::AddPhoneCalendar),
            (MIGO_SCOPE_FRIEND_INTERACTION, Scope::FriendInteraction),
            (MIGO_SCOPE_GAME_CLUB_DATA, Scope::GameClubData),
        ];
        assert_eq!(numbered.len(), Scope::ALL.len());
        assert_eq!(MIGO_SCOPE_COUNT as usize, Scope::ALL.len());
        for (number, scope) in numbered {
            assert_eq!(
                Scope::ALL[number as usize],
                scope,
                "{}",
                scope.as_minigame_str()
            );
        }
    }

    unsafe extern "C" fn inline_dispatch(
        _dispatcher: *mut c_void,
        task: MigoTaskFn,
        context: *mut c_void,
    ) -> MigoResult {
        unsafe { task(context) };
        MIGO_OK
    }

    unsafe extern "C" fn noop_call(_: *mut c_void, _: *mut c_void, _: *const MigoHostServiceCall) {}

    fn install(session: *mut MigoSession, services: u64) {
        let mut raw = MigoHostCallbacks::empty();
        raw.dispatch = Some(inline_dispatch);
        raw.on_host_service_call = Some(noop_call);
        raw.host_services = services;
        assert_eq!(
            unsafe { crate::migo_session_set_host_callbacks(session, &raw) },
            MIGO_OK
        );
    }

    fn reports_of(session: *mut MigoSession) -> Arc<HostReports> {
        let pinned = unsafe { pin_session(session) }.expect("live");
        Arc::clone(&pinned.host_reports)
    }

    #[test]
    fn scope_decisions_are_read_back_and_need_the_permission_service() {
        with_session("scope-state-undeclared", |session| {
            install(
                session,
                1 << migo_capi_abi::host_services::MIGO_HOST_SERVICE_AUTH,
            );
            assert_eq!(
                unsafe {
                    migo_session_set_scope_state(
                        session,
                        MIGO_SCOPE_CAMERA,
                        MIGO_SCOPE_STATE_GRANTED,
                    )
                },
                MIGO_ERROR_INVALID_ARGUMENT,
                "a grant nobody would read"
            );
        });
        with_session("scope-state", |session| {
            install(session, 1 << MIGO_HOST_SERVICE_PERMISSION);
            let reports = reports_of(session);
            assert_eq!(reports.scope_state(Scope::Camera), ScopeState::Unknown);
            let set = |scope, state| unsafe { migo_session_set_scope_state(session, scope, state) };
            assert_eq!(set(MIGO_SCOPE_CAMERA, MIGO_SCOPE_STATE_GRANTED), MIGO_OK);
            assert_eq!(set(MIGO_SCOPE_RECORD, MIGO_SCOPE_STATE_DENIED), MIGO_OK);
            assert_eq!(reports.scope_state(Scope::Camera), ScopeState::Granted);
            assert_eq!(reports.scope_state(Scope::Record), ScopeState::Denied);
            assert_eq!(reports.scope_state(Scope::UserInfo), ScopeState::Unknown);
            // A revocation is a report like any other.
            assert_eq!(set(MIGO_SCOPE_CAMERA, MIGO_SCOPE_STATE_UNKNOWN), MIGO_OK);
            assert_eq!(reports.scope_state(Scope::Camera), ScopeState::Unknown);
            assert_eq!(
                set(MIGO_SCOPE_COUNT, MIGO_SCOPE_STATE_GRANTED),
                MIGO_ERROR_INVALID_ARGUMENT
            );
            assert_eq!(set(MIGO_SCOPE_CAMERA, 3), MIGO_ERROR_INVALID_ARGUMENT);
        });
    }

    fn system_info(width: u32, height: u32, reports: Arc<HostReports>) -> CapiSystemInfo {
        CapiSystemInfo::new(
            Arc::new(HostWindowState::new(HostWindowMetrics::new(
                width,
                height,
                PixelRatio::new(1.0).expect("ratio"),
            ))),
            reports,
            None,
        )
    }

    fn json(text: Result<String, ServiceError>) -> serde_json::Value {
        serde_json::from_str(&text.expect("answered")).expect("json")
    }

    #[test]
    fn system_settings_are_the_report_and_the_window_s_shape() {
        with_session("system-settings", |session| {
            let reports = reports_of(session);
            let info = system_info(1280, 720, Arc::clone(&reports));
            assert_eq!(
                json(info.get_system_settings_json()),
                serde_json::json!({"bluetooth_enabled": false, "location_enabled": false,
                    "wifi_enabled": false, "orientation": "landscape"})
            );
            assert_eq!(
                unsafe {
                    migo_session_set_system_settings(
                        session,
                        MIGO_SYSTEM_SETTING_FLAG_BLUETOOTH_ENABLED
                            | MIGO_SYSTEM_SETTING_FLAG_WIFI_ENABLED,
                    )
                },
                MIGO_OK
            );
            assert_eq!(
                json(system_info(720, 1280, reports).get_system_settings_json()),
                serde_json::json!({"bluetooth_enabled": true, "location_enabled": false,
                    "wifi_enabled": true, "orientation": "portrait"})
            );
            assert_eq!(
                unsafe { migo_session_set_system_settings(session, 1 << 3) },
                MIGO_ERROR_INVALID_ARGUMENT
            );
        });
    }

    #[test]
    fn app_authorizations_are_undetermined_until_reported() {
        with_session("app-authorize", |session| {
            let reports = reports_of(session);
            let before =
                json(system_info(1, 1, Arc::clone(&reports)).get_app_authorization_setting_json());
            assert_eq!(before["cameraAuthorized"], "not determined");
            assert_eq!(before["locationReducedAccuracy"], false);

            let report = MigoAppAuthorizeSetting {
                header: VersionedHeader {
                    struct_size: size_of::<MigoAppAuthorizeSetting>() as u32,
                    abi_version: MIGO_ABI_VERSION_CURRENT,
                },
                album: MIGO_AUTHORIZATION_DENIED,
                bluetooth: 0,
                camera: MIGO_AUTHORIZATION_AUTHORIZED,
                location: MIGO_AUTHORIZATION_AUTHORIZED,
                microphone: 0,
                notification: 0,
                notification_alert: 0,
                notification_badge: 0,
                notification_sound: 0,
                phone_calendar: 0,
                location_reduced_accuracy: 1,
                reserved0: 0,
            };
            assert_eq!(
                unsafe { migo_session_set_app_authorize_setting(session, &report) },
                MIGO_OK
            );
            let after = json(system_info(1, 1, reports).get_app_authorization_setting_json());
            assert_eq!(after["cameraAuthorized"], "authorized");
            assert_eq!(after["albumAuthorized"], "denied");
            assert_eq!(after["microphoneAuthorized"], "not determined");
            assert_eq!(after["locationReducedAccuracy"], true);
            assert_eq!(
                unsafe { migo_session_set_app_authorize_setting(session, std::ptr::null()) },
                MIGO_ERROR_INVALID_ARGUMENT
            );
        });
    }

    #[test]
    fn settings_pages_are_offered_only_by_a_host_that_declared_them() {
        with_session("settings-undeclared", |session| {
            let info = system_info(1, 1, reports_of(session));
            assert!(info.open_setting(r#"{"requestId":1}"#).is_err());
            assert!(info.open_bluetooth_settings(1).is_err());
            assert!(info.open_app_authorize_setting(1).is_err());
        });
    }
}

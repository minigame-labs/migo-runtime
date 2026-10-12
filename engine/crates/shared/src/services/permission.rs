//! Permission service: the host decides what a game is allowed to do.
//!
//! # Why the runtime cannot answer this itself
//!
//! A mainstream mini-game platform is both the runtime and the host: it owns
//! the user relationship, the consent dialog, and the persistent record of
//! what each mini-game was granted. Migo is embedded in someone else's app and
//! owns none of those. So the three things that platform convention merges
//! have to be split:
//!
//! - **the game declares** which scopes it uses, in `game.json`'s `permission`
//!   field, together with the reason text a prompt should show;
//! - **the host decides**, because it is the one with a user to ask and a place
//!   to remember the answer;
//! - **the runtime carries**, and does not decide, prompt, or store.
//!
//! Same shape as advertising, auth and payment: the capability lives with
//! whoever owns the relationship, and the runtime is transport.
//!
//! # Default deny
//!
//! With no host permission handler installed, every scope is denied.
//!
//! The state this replaces was a JavaScript object with every scope hardcoded
//! to `true`, so `migo.getSetting()` told content it held permissions nobody
//! had granted and `migo.authorize()` -- an API whose entire purpose is to ask
//! the user -- returned success without asking anyone. In a game centre that
//! means a third-party title reaches the camera under the *host app's* grant,
//! with nobody ever asked whether that game should have it.
//!
//! A runtime that grants permissions on its own is the same class of defect as
//! one that mints ad rewards on its own, and gets the same answer: without a
//! host to decide, the answer is no.
//!
//! # Declaration is not grant
//!
//! `game.json`'s `permission` block is passed to the host so it can show a
//! meaningful prompt, and so a game centre can see at listing time what a title
//! will ask for. It is never read as permission.

use crate::protocol::error::ServiceError;

/// A mini-game authorisation scope.
///
/// Content addresses these by their platform-facing string (`"scope.camera"`),
/// which is what crosses the boundary; this enum exists so the runtime's own
/// call sites name a capability rather than repeat a string literal, and so
/// adding a capability is a compile error at every site that must classify it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scope {
    UserInfo,
    UserLocation,
    UserLocationBackground,
    Address,
    InvoiceTitle,
    Invoice,
    WeRun,
    Record,
    WritePhotosAlbum,
    Camera,
    Bluetooth,
    AddPhoneContact,
    AddPhoneCalendar,
    FriendInteraction,
    GameClubData,
}

impl Scope {
    /// Every scope the common mini-game surface defines, in the order
    /// `migo.getSetting()` reports them.
    ///
    /// Exhaustive on purpose: a new variant that is not listed here is a
    /// capability `getSetting` would silently omit, so the list and the enum
    /// are checked against each other in this module's tests.
    pub const ALL: &'static [Scope] = &[
        Scope::UserInfo,
        Scope::UserLocation,
        Scope::UserLocationBackground,
        Scope::Address,
        Scope::InvoiceTitle,
        Scope::Invoice,
        Scope::WeRun,
        Scope::Record,
        Scope::WritePhotosAlbum,
        Scope::Camera,
        Scope::Bluetooth,
        Scope::AddPhoneContact,
        Scope::AddPhoneCalendar,
        Scope::FriendInteraction,
        Scope::GameClubData,
    ];

    /// The platform-facing name, e.g. `"scope.camera"`.
    pub const fn as_minigame_str(self) -> &'static str {
        match self {
            Scope::UserInfo => "scope.userInfo",
            Scope::UserLocation => "scope.userLocation",
            Scope::UserLocationBackground => "scope.userLocationBackground",
            Scope::Address => "scope.address",
            Scope::InvoiceTitle => "scope.invoiceTitle",
            Scope::Invoice => "scope.invoice",
            Scope::WeRun => "scope.werun",
            Scope::Record => "scope.record",
            Scope::WritePhotosAlbum => "scope.writePhotosAlbum",
            Scope::Camera => "scope.camera",
            Scope::Bluetooth => "scope.bluetooth",
            Scope::AddPhoneContact => "scope.addPhoneContact",
            Scope::AddPhoneCalendar => "scope.addPhoneCalendar",
            Scope::FriendInteraction => "scope.friendInteraction",
            Scope::GameClubData => "scope.gameClubData",
        }
    }

    /// Parse a platform-facing scope name.
    ///
    /// Unknown names return `None` rather than a default: content may pass an
    /// arbitrary string to `migo.authorize`, and treating an unrecognised scope
    /// as any known one would grant the wrong capability.
    pub fn from_minigame_str(name: &str) -> Option<Scope> {
        Scope::ALL
            .iter()
            .copied()
            .find(|s| s.as_minigame_str() == name)
    }
}

/// Device-service accessors that may only be used with a granted scope.
///
/// The runtime coverage contract derives service-wide permission surfaces from
/// this table, then requires each operation to be explicitly classified as a
/// gate or a cleanup exemption in the runtime layer. This keeps service
/// ownership here without making `shared` know runtime operation names.
///
/// Only capabilities the common mini-game surface itself gates appear here.
/// `clipboard` and `scan_code` are absent because that surface does not put
/// them behind a scope, and inventing scopes it has no notion of would mean
/// existing games hitting an authorisation flow that does not exist on the
/// platform they were written for.
///
/// `image_api` is deliberately absent despite `saveImageToPhotosAlbum` needing
/// `scope.writePhotosAlbum`: the accessor also serves `compressImage` and
/// friends, which need nothing, and gating the accessor would gate those too.
/// It has a per-op check rather than an accessor-level one.
#[allow(dead_code)]
pub const GATED_ACCESSORS: &[(&str, Scope)] = &[
    ("camera", Scope::Camera),
    ("recorder", Scope::Record),
    ("location", Scope::UserLocation),
    ("bluetooth", Scope::Bluetooth),
];

/// Permission-sensitive methods on services whose other methods are unscoped.
///
/// The service layer owns these method names; the runtime coverage contract
/// derives their caller ops and checks the runtime-owned policy classification.
#[allow(dead_code)]
pub const GATED_SERVICE_METHODS: &[(&str, Scope)] = &[
    ("save_image_to_photos_album", Scope::WritePhotosAlbum),
    ("get_user_info", Scope::UserInfo),
];

/// Operations that must hold a scope before reaching a host capability, by op
/// name: what both executions enforce. The embedded runtime's ops each guard
/// themselves, and the coverage contract checks every guard against this table
/// in both directions and rejects unclassified service-wide operations; the
/// external session's host channel, which has no op functions of its own,
/// gates each request by looking its op up here ([`op_scope`]).
pub const PERMISSION_GATED_OPS: &[(&str, Scope)] = &[
    ("op_camera_create", Scope::Camera),
    ("op_camera_take_photo", Scope::Camera),
    ("op_camera_start_record", Scope::Camera),
    ("op_camera_set_zoom", Scope::Camera),
    ("op_camera_listen_frame_change", Scope::Camera),
    ("op_save_image_to_photos_album", Scope::WritePhotosAlbum),
    ("op_recorder_start", Scope::Record),
    ("op_recorder_pause", Scope::Record),
    ("op_recorder_resume", Scope::Record),
    ("op_get_location", Scope::UserLocation),
    ("op_get_fuzzy_location", Scope::UserLocation),
    ("op_open_bluetooth_adapter", Scope::Bluetooth),
    ("op_get_bluetooth_adapter_state", Scope::Bluetooth),
    ("op_start_bluetooth_devices_discovery", Scope::Bluetooth),
    ("op_get_bluetooth_devices", Scope::Bluetooth),
    ("op_get_connected_bluetooth_devices", Scope::Bluetooth),
    ("op_make_bluetooth_pair", Scope::Bluetooth),
    ("op_is_bluetooth_device_paired", Scope::Bluetooth),
    ("op_create_ble_connection", Scope::Bluetooth),
    ("op_get_ble_device_services", Scope::Bluetooth),
    ("op_get_ble_device_characteristics", Scope::Bluetooth),
    ("op_read_ble_characteristic_value", Scope::Bluetooth),
    ("op_write_ble_characteristic_value", Scope::Bluetooth),
    (
        "op_notify_ble_characteristic_value_change",
        Scope::Bluetooth,
    ),
    ("op_get_ble_device_rssi", Scope::Bluetooth),
    ("op_set_ble_mtu", Scope::Bluetooth),
    ("op_get_ble_mtu", Scope::Bluetooth),
    ("op_start_beacon_discovery", Scope::Bluetooth),
    ("op_get_beacons", Scope::Bluetooth),
    ("op_get_user_info", Scope::UserInfo),
];

/// Operations that may reach an existing capability only to release it.
///
/// Denial must not trap a camera, microphone, scan, or connection that was
/// acquired while permission was granted.
pub const PERMISSION_CLEANUP_OPS: &[(&str, Scope)] = &[
    ("op_camera_destroy", Scope::Camera),
    ("op_camera_stop_record", Scope::Camera),
    ("op_camera_close_frame_change", Scope::Camera),
    ("op_recorder_stop", Scope::Record),
    ("op_close_bluetooth_adapter", Scope::Bluetooth),
    ("op_stop_bluetooth_devices_discovery", Scope::Bluetooth),
    ("op_close_ble_connection", Scope::Bluetooth),
    ("op_stop_beacon_discovery", Scope::Bluetooth),
];

/// The scope an op must hold before it reaches the host, if it is gated.
pub fn op_scope(op: &str) -> Option<Scope> {
    PERMISSION_GATED_OPS
        .iter()
        .find(|(name, _)| *name == op)
        .map(|(_, scope)| *scope)
}

/// `getSetting`'s `authSetting`: only the scopes somebody has decided -- a
/// granted one `true`, a refused one `false` -- and one nobody has been asked
/// about absent. Content tells the last two apart: the mini-game convention
/// does not re-prompt after a refusal, so `false` sends the player to
/// `openSetting` while an absent scope is one to authorize, and reporting the
/// undecided as `false` made every game that followed it send the player to
/// settings for a question it had never asked. No permission service is
/// nobody decided anything.
pub fn auth_setting_json(permission: Option<&dyn PermissionService>) -> String {
    let mut out = String::from("{");
    let mut first = true;
    let Some(permission) = permission else {
        out.push('}');
        return out;
    };
    for scope in Scope::ALL {
        let granted = match permission.scope_state(*scope) {
            ScopeState::Granted => true,
            ScopeState::Denied => false,
            ScopeState::Unknown => continue,
        };
        if !first {
            out.push(',');
        }
        first = false;
        out.push('"');
        out.push_str(scope.as_minigame_str());
        out.push_str("\":");
        out.push_str(if granted { "true" } else { "false" });
    }
    out.push('}');
    out
}

/// What the host has decided about one scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeState {
    /// The host has not been asked, or has not answered yet.
    ///
    /// Distinct from `Denied` because the common mini-game convention does not
    /// re-prompt after a refusal: content that was denied must be sent to
    /// `openSetting` instead, and collapsing the two would either re-prompt
    /// forever or refuse to ever ask.
    Unknown,
    Granted,
    Denied,
}

/// Host-provided permission decisions.
///
/// Every method defaults to refusing, so a platform with no permission
/// integration denies rather than invents an answer.
pub trait PermissionService: Send + Sync {
    /// The host's current decision for one scope, without asking the user.
    ///
    /// Must not prompt: this backs `migo.getSetting()`, which content calls to
    /// decide whether to *offer* a feature, often during layout.
    fn scope_state(&self, _scope: Scope) -> ScopeState {
        ScopeState::Unknown
    }

    /// Ask the host to decide, prompting the user if it sees fit.
    ///
    /// JSON fields (input):
    /// - `requestId`: number, correlates the reply
    /// - `scope`: string, the platform-facing scope name
    /// - `desc`: string, the reason text the game declared in `game.json`
    ///   (empty when it declared none) -- a host cannot write an honest prompt
    ///   without it
    ///
    /// Result delivered via the permission-result callback. Fire-and-forget:
    /// the host may take arbitrarily long, because there is a human in it.
    fn request_scope(&self, _request_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "authorize:fail no permission handler",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `ALL` and the enum must not drift: a scope missing from `ALL` is one
    /// `migo.getSetting()` never reports, which reads to content as "this
    /// capability does not exist" rather than "not granted".
    #[test]
    fn every_scope_round_trips_through_its_minigame_name() {
        for scope in Scope::ALL {
            let name = scope.as_minigame_str();
            assert_eq!(
                Scope::from_minigame_str(name),
                Some(*scope),
                "{name} did not round-trip"
            );
            assert!(name.starts_with("scope."), "{name} is not a scope name");
        }
    }

    #[test]
    fn friend_interaction_uses_a_neutral_scope_name() {
        assert_eq!(
            Scope::FriendInteraction.as_minigame_str(),
            "scope.friendInteraction"
        );
    }

    #[test]
    fn scope_names_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for scope in Scope::ALL {
            assert!(
                seen.insert(scope.as_minigame_str()),
                "{} appears twice in Scope::ALL",
                scope.as_minigame_str()
            );
        }
        assert_eq!(seen.len(), Scope::ALL.len());
    }

    #[test]
    fn unknown_scope_names_are_rejected_rather_than_defaulted() {
        for name in [
            "",
            "scope",
            "scope.",
            "scope.nope",
            "camera",
            "SCOPE.CAMERA",
        ] {
            assert_eq!(
                Scope::from_minigame_str(name),
                None,
                "{name:?} was accepted"
            );
        }
    }

    /// The default is refusal on every method, so a platform that implements
    /// nothing denies rather than grants.
    #[test]
    fn the_default_implementation_grants_nothing() {
        struct Silent;
        impl PermissionService for Silent {}

        for scope in Scope::ALL {
            assert_eq!(Silent.scope_state(*scope), ScopeState::Unknown);
        }
        assert!(Silent.request_scope("{}").is_err());
    }
}

//! Scope enforcement for capability ops.
//!
//! The decision belongs to the host (see [`shared::services::PermissionService`]
//! for why the runtime cannot make it). This module is the one place ops ask.
//!
//! # Where the checks go
//!
//! Service-wide capability use is derived from device-service accessors, then
//! every matching operation is explicitly classified below. Explicit cleanup
//! entries matter because a revoked scope must stop new use without preventing
//! release of resources acquired while it was granted. Per-operation scopes
//! for shared services, such as album writes and user info, live in the same
//! policy.
//!
//! # Denied by default
//!
//! No host permission service means no grants. This matches the ad bridge's
//! answer to the same question: a runtime that hands out a capability nobody
//! approved is the same defect as one that pays out an advert nobody watched,
//! and "there is nobody to ask" resolves to no in both.

use deno_core::OpState;
use deno_error::JsErrorBox;
use shared::op_state::HostOpState;
use shared::services::{Scope, ScopeState};

/// Refuse an operation unless the host has granted its scope.
///
/// The error text follows the common mini-game platform's `auth deny`
/// convention, so content that already branches on it behaves the same way.
pub(crate) fn require_scope(state: &OpState, scope: Scope) -> Result<(), JsErrorBox> {
    if scope_state(state, scope) == ScopeState::Granted {
        return Ok(());
    }
    Err(JsErrorBox::generic(format!(
        "auth deny: {} is not granted",
        scope.as_minigame_str()
    )))
}

/// The host's current decision for one scope.
///
/// `Unknown` when no host permission service is installed: nobody has been
/// asked, which is not the same as having been refused. `migo.getSetting()`
/// reports the difference and content uses it to decide between prompting and
/// sending the user to `openSetting`.
pub(crate) fn scope_state(state: &OpState, scope: Scope) -> ScopeState {
    state
        .borrow::<HostOpState>()
        .device_services
        .as_ref()
        .and_then(|services| services.permission())
        .map(|permission| permission.scope_state(scope))
        .unwrap_or(ScopeState::Unknown)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, atomic::AtomicBool};

    use super::*;
    use shared::{
        channel::ThreadWakeup,
        device::gpu_caps::GpuCaps,
        op_state::{AudioSender, NetworkPolicy},
        render_command_sender::CommandSender,
        services::{
            CommerceServices, ConnectivityServices, DeviceServices, MediaServices,
            PermissionService, SensorServices, SystemUtilServices,
        },
    };

    struct FixedPermissions(ScopeState);
    impl PermissionService for FixedPermissions {
        fn scope_state(&self, _scope: Scope) -> ScopeState {
            self.0
        }
    }

    struct Bundle(ScopeState);
    impl SensorServices for Bundle {}
    impl MediaServices for Bundle {}
    impl ConnectivityServices for Bundle {}
    impl CommerceServices for Bundle {}
    impl SystemUtilServices for Bundle {
        fn permission(&self) -> Option<Arc<dyn PermissionService>> {
            Some(Arc::new(FixedPermissions(self.0)))
        }
    }

    fn state_with(services: Option<Arc<dyn DeviceServices>>) -> OpState {
        let (render_tx, _render_rx) = CommandSender::new();
        let (host_tx, _critical_host_tx, _host_rx) = shared::host_channel::channel(1);
        let host = HostOpState {
            callback_ids: std::sync::Arc::new(shared::callback_id::CallbackIdAllocator::default()),
            runtime_generation: 1,
            id: 1,
            app_cache_dir: std::path::PathBuf::from("/tmp/cache"),
            app_files_dir: std::path::PathBuf::from("/tmp/files"),
            code_dir: None,
            game_paths: None,
            vfs: None,
            mount_table: None,
            render_tx,
            text_measurer: None,
            audio_tx: AudioSender::new(shared::audio_channel::disconnected(), ThreadWakeup::new()),
            host_tx,
            device_services: services,
            raf_rx: None,
            raf_demand: Arc::new(shared::raf_signal::RafDemand::new()),
            request_vsync: None,
            sub_packages: Vec::new(),
            workers_path: None,
            network_policy: NetworkPolicy::default(),
            backgrounded: Arc::new(AtomicBool::new(false)),
            timer_backgrounded: Arc::new(AtomicBool::new(false)),
            webgl_context_created: Arc::new(AtomicBool::new(false)),
            context_lost: Arc::new(shared::op_state::ContextLostState::default()),
            code_signing_enabled: false,
            gpu_caps: GpuCaps::new(),
        };
        let mut state = OpState::new(None);
        state.put(host);
        state
    }

    /// The default that matters: nothing installed means nothing granted.
    #[test]
    fn no_permission_service_denies_every_scope() {
        let state = state_with(None);
        for scope in Scope::ALL {
            assert!(
                require_scope(&state, *scope).is_err(),
                "{} was allowed with no host to grant it",
                scope.as_minigame_str()
            );
            assert_eq!(scope_state(&state, *scope), ScopeState::Unknown);
        }
    }

    #[test]
    fn a_granting_host_allows_the_operation() {
        let services: Arc<dyn DeviceServices> = Arc::new(Bundle(ScopeState::Granted));
        let state = state_with(Some(services));
        for scope in Scope::ALL {
            assert!(require_scope(&state, *scope).is_ok());
        }
    }

    /// Refusal and "not yet asked" both stop the operation, and both must stop
    /// it: the distinction exists for what content is told afterwards, not for
    /// whether the call proceeds.
    #[test]
    fn denied_and_unknown_both_refuse() {
        for reported in [ScopeState::Denied, ScopeState::Unknown] {
            let services: Arc<dyn DeviceServices> = Arc::new(Bundle(reported));
            let state = state_with(Some(services));
            let result = require_scope(&state, Scope::Camera);
            assert!(result.is_err(), "{reported:?} allowed the operation");
        }
    }

    /// The message is what content matches on; the convention says `auth deny`.
    #[test]
    fn the_refusal_names_the_scope_in_minigame_form() {
        let state = state_with(None);
        let message = require_scope(&state, Scope::Camera)
            .unwrap_err()
            .to_string();
        assert!(message.contains("auth deny"), "got {message}");
        assert!(message.contains("scope.camera"), "got {message}");
    }
}

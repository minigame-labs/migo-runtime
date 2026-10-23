//! Ad APIs: banner, custom, grid, interstitial, rewarded video, game banner,
//! game icon and game portal.
//!
//! # Host-authoritative, not simulated
//!
//! The runtime does not link an ad SDK. Every command here is forwarded to the
//! host's [`AdService`], and every ad event comes back through the inbound
//! host bridge as `_internalOnAdEvent`.
//!
//! The commercially load-bearing consequence is that **the runtime cannot
//! originate a reward**. When no host ad service is installed, content still
//! gets working ad objects, but incentivised video closes with
//! `isEnded: false`: no advert was shown, so no reward is owed. See
//! `shared/src/services/ad.rs` for the full invariant and
//! `scripts/test-ad-reward-integrity-contract.sh` for the guard that keeps it
//! from regressing.
//!
//! All ops are Mode C (fire-and-forget; results arrive as events) except
//! `op_ad_is_supported`, which is Mode B (sync) and is called at most once per
//! isolate -- the JS layer memoises it, so steady-state ad traffic costs no
//! extra op calls.

use deno_core::{Extension, OpState, op2};
use deno_error::JsErrorBox;
use shared::op_state::HostOpState;
use shared::services::AdService;
use std::sync::Arc;

/// Resolve the host's ad service, if the platform installed one.
#[inline]
fn ad_service(state: &OpState) -> Option<Arc<dyn AdService>> {
    state
        .borrow::<HostOpState>()
        .device_services
        .as_ref()
        .and_then(|services| services.ad())
}

/// Whether a host ad service is available.
///
/// The JS layer calls this lazily once and caches the answer for the life of
/// the isolate. It is deliberately not evaluated at module scope: embedded
/// modules are executed while building the V8 startup snapshot, where no
/// `HostOpState` exists yet.
#[op2(fast)]
pub fn op_ad_is_supported(state: &mut OpState) -> bool {
    ad_service(state).is_some()
}

/// Forward a fire-and-forget ad request to the host.
///
/// When no host ad service is installed the op returns `Ok(())` rather than an
/// error: the JS layer has already decided to run its no-host path, and making
/// these ops throw would turn "this build has no ad host" into a content-
/// visible exception on every ad call.
///
/// One function per op rather than one macro stamping them, so each op's
/// signature is one the producer's argument conversions can be checked against.
fn ad_command(
    state: &mut OpState,
    request_json: &str,
    call: impl FnOnce(&dyn AdService, &str) -> Result<(), shared::services::ServiceError>,
) -> Result<(), JsErrorBox> {
    match ad_service(state) {
        Some(service) => call(&*service, request_json).map_err(JsErrorBox::generic),
        None => Ok(()),
    }
}

#[op2(fast)]
pub fn op_ad_create(state: &mut OpState, #[string] request_json: &str) -> Result<(), JsErrorBox> {
    ad_command(state, request_json, |service, request| {
        service.create_ad(request)
    })
}

#[op2(fast)]
pub fn op_ad_load(state: &mut OpState, #[string] request_json: &str) -> Result<(), JsErrorBox> {
    ad_command(state, request_json, |service, request| {
        service.load_ad(request)
    })
}

#[op2(fast)]
pub fn op_ad_show(state: &mut OpState, #[string] request_json: &str) -> Result<(), JsErrorBox> {
    ad_command(state, request_json, |service, request| {
        service.show_ad(request)
    })
}

#[op2(fast)]
pub fn op_ad_hide(state: &mut OpState, #[string] request_json: &str) -> Result<(), JsErrorBox> {
    ad_command(state, request_json, |service, request| {
        service.hide_ad(request)
    })
}

#[op2(fast)]
pub fn op_ad_update_style(
    state: &mut OpState,
    #[string] request_json: &str,
) -> Result<(), JsErrorBox> {
    ad_command(state, request_json, |service, request| {
        service.update_ad_style(request)
    })
}

#[op2(fast)]
pub fn op_ad_destroy(state: &mut OpState, #[string] request_json: &str) -> Result<(), JsErrorBox> {
    ad_command(state, request_json, |service, request| {
        service.destroy_ad(request)
    })
}

main_runtime_extension!(
    host_v8_ad,
    {
        deps = [host_v8_base],
        ops = [
            op_ad_is_supported,
            op_ad_create,
            op_ad_load,
            op_ad_show,
            op_ad_hide,
            op_ad_update_style,
            op_ad_destroy,
        ],
    },
    esm_entry_point = "ext:host_v8_ad/99_global_scope.js",
    esm = [
        dir "src/ad",
        "01_ad.js",
        "99_global_scope.js",
    ],
);

pub fn ad_extensions() -> Vec<Extension> {
    vec![host_v8_ad::init()]
}

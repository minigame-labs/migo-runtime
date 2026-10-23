//! The host's updates of the content package: `getUpdateManager`.

use deno_core::{Extension, OpState, op2};
use deno_error::JsErrorBox;
use shared::op_state::HostOpState;

/// Whether the host offers updates -- and so will report its check.
#[op2(fast)]
pub fn op_update_available(state: &mut OpState) -> bool {
    let host = state.borrow::<HostOpState>();
    host.device_services
        .as_ref()
        .is_some_and(|services| services.update().is_some())
}

/// Restart the game on the version the host downloaded.
#[op2(fast)]
pub fn op_apply_update(state: &mut OpState) -> Result<(), JsErrorBox> {
    let host = state.borrow::<HostOpState>();
    match host
        .device_services
        .as_ref()
        .and_then(|services| services.update())
    {
        Some(update) => update.apply_update().map_err(JsErrorBox::generic),
        None => Err(JsErrorBox::generic("applyUpdate:fail not supported")),
    }
}

main_runtime_extension!(
    host_v8_update,
    {
        ops = [op_update_available, op_apply_update],
    },
    esm_entry_point = "ext:host_v8_update/99_global_scope.js",
    esm = [
        dir "src/update",
        "02_update_mgr.js",
        "99_global_scope.js",
    ],
);

pub fn update_extensions() -> Vec<Extension> {
    vec![host_v8_update::init()]
}

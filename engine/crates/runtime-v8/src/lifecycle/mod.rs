//! App lifecycle extension: show/hide events, launch options, restart/exit.

use deno_core::{Extension, OpState, extension, op2};
use deno_error::JsErrorBox;
use shared::op_state::HostOpState;
use shared::protocol::host_cmd::HostCommand;

/// `launch_query`: the query the restarted game is launched with, or `None` for the one it was last shown with
/// (`HostCommand::Restart`).
#[op2]
pub fn op_restart_mini_program(
    state: &mut OpState,
    #[string] launch_query: Option<String>,
) -> Result<(), JsErrorBox> {
    let host = state.borrow::<HostOpState>();
    host.host_tx
        .try_send(HostCommand::Restart { launch_query })
        .map_err(|e| JsErrorBox::generic(format!("restartMiniProgram:fail {e}")))
}

#[op2(fast)]
pub fn op_exit_mini_program(state: &mut OpState) -> Result<(), JsErrorBox> {
    let host = state.borrow::<HostOpState>();
    host.host_tx
        .try_send(HostCommand::Shutdown)
        .map_err(|e| JsErrorBox::generic(format!("exitMiniProgram:fail {e}")))
}

extension!(host_v8_lifecycle,
    deps = [host_v8_base],
    ops = [
        op_restart_mini_program,
        op_exit_mini_program,
    ],
    esm = [
        dir "src/lifecycle",
        "01_lifecycle.js",
        "02_restart_exit.js",
    ]
);

pub fn lifecycle_extensions() -> Vec<Extension> {
    vec![host_v8_lifecycle::init()]
}

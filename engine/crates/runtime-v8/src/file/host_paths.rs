//! Paths content names in a request to the host, made into paths the host can
//! open: `migo_services::host_paths`, over this runtime's sandbox. The rules and
//! which fields are files are there, shared with the external session's host.

use std::cell::RefCell;
use std::rc::Rc;

use deno_core::OpState;
use deno_core::serde_json::Value;
use deno_error::JsErrorBox;
use migo_services::host_paths::{self, Files, Sandbox};
use shared::op_state::HostOpState;
use shared::services::host_files::Step;

pub(crate) use migo_services::host_paths::Remote;

use crate::io_state::IoSchedulerState;

pub(crate) fn sandbox(state: &Rc<RefCell<OpState>>) -> Sandbox {
    sandbox_of(&state.borrow())
}

/// The game's sandbox as this runtime holds it, for a synchronous op.
pub(crate) fn sandbox_of(state: &OpState) -> Sandbox {
    let host = state.borrow::<HostOpState>();
    Sandbox {
        host_id: host.id,
        scheduler: state.borrow::<IoSchedulerState>().0.clone(),
        vfs: host.vfs.clone(),
        mount_table: host.mount_table.clone(),
        policy: host.network_policy.clone(),
    }
}

/// `request_json` with every path `files` names replaced by a real path the host
/// can open; a package entry copied out is held until the request's result.
pub(crate) async fn export_paths(
    state: &Rc<RefCell<OpState>>,
    request_json: &str,
    files: Files,
) -> Result<String, JsErrorBox> {
    host_paths::export_paths(&sandbox(state), request_json, files)
        .await
        .map_err(|error| JsErrorBox::generic(error.message))
}

/// `message`'s paths, for a message no result answers: what is copied out is
/// held for the rest of the session.
pub(crate) async fn export_paths_for_session(
    state: &Rc<RefCell<OpState>>,
    message: &mut Value,
    fields: &[&[Step]],
    remote: Remote,
) -> Result<(), JsErrorBox> {
    host_paths::export_paths_for_session(&sandbox(state), message, fields, remote)
        .await
        .map_err(|error| JsErrorBox::generic(error.message))
}

/// A menu share's reply with its images exported for the session; one that is
/// not content's file is dropped.
pub(crate) async fn export_menu_share(state: &Rc<RefCell<OpState>>, reply: &mut Value) {
    host_paths::export_menu_share(&sandbox(state), reply).await;
}

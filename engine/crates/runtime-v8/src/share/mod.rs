//! Share APIs: showShareMenu, updateShareMenu, onShareAppMessage, shareAppMessage,
//! shareMessageToFriend, showShareImageMenu.
//!
//! The three requests the host answers are each their own op (Mode C): sharing,
//! sharing to one friend, and the image share sheet are different host flows
//! with different results, and folding them into one op left the host unable
//! to tell them apart.
//!
//! Each names an image content chose, and the host is handed the real file behind
//! it, resolved through the sandbox (`crate::file::host_paths`) -- never content's
//! string, which could name any file the host's process can read. The ops are
//! eager: an image with a file of its own needs no wait, so the request leaves in
//! the call's own tick.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use deno_core::{Extension, OpState, op2};
use deno_error::JsErrorBox;
use shared::op_state::HostOpState;
use shared::services::ShareService;
use shared::services::host_files::Step;

use crate::file::host_paths::{Remote, export_paths};

/// `shareAppMessage`'s image, which may also be a network image.
const SHARE_APP_MESSAGE_FILES: &[&[Step]] = &[&[Step::Key("imageUrl")]];
/// `shareMessageToFriend`'s image, which may not.
const SHARE_MESSAGE_TO_FRIEND_FILES: &[&[Step]] = &[&[Step::Key("imageUrl")]];
/// `showShareImageMenu`'s image, a local file.
const SHOW_SHARE_IMAGE_MENU_FILES: &[&[Step]] = &[&[Step::Key("path")]];

fn share_service(
    state: &Rc<RefCell<OpState>>,
    err_msg: &'static str,
) -> Result<Arc<dyn ShareService>, JsErrorBox> {
    state
        .borrow()
        .borrow::<HostOpState>()
        .device_services
        .as_ref()
        .and_then(|services| services.share())
        .ok_or_else(|| JsErrorBox::generic(err_msg))
}

#[op2]
pub async fn op_share_app_message(
    state: Rc<RefCell<OpState>>,
    #[string] request_json: String,
) -> Result<(), JsErrorBox> {
    let service = share_service(&state, "shareAppMessage:fail not supported")?;
    let request = export_paths(
        &state,
        &request_json,
        SHARE_APP_MESSAGE_FILES,
        Remote::Allowed,
    )
    .await?;
    service
        .share_app_message(&request)
        .map_err(JsErrorBox::generic)
}

#[op2]
pub async fn op_share_message_to_friend(
    state: Rc<RefCell<OpState>>,
    #[string] request_json: String,
) -> Result<(), JsErrorBox> {
    let service = share_service(&state, "shareMessageToFriend:fail not supported")?;
    let request = export_paths(
        &state,
        &request_json,
        SHARE_MESSAGE_TO_FRIEND_FILES,
        Remote::Refused,
    )
    .await?;
    service
        .share_message_to_friend(&request)
        .map_err(JsErrorBox::generic)
}

#[op2]
pub async fn op_show_share_image_menu(
    state: Rc<RefCell<OpState>>,
    #[string] request_json: String,
) -> Result<(), JsErrorBox> {
    let service = share_service(&state, "showShareImageMenu:fail not supported")?;
    let request = export_paths(
        &state,
        &request_json,
        SHOW_SHARE_IMAGE_MENU_FILES,
        Remote::Refused,
    )
    .await?;
    service
        .show_share_image_menu(&request)
        .map_err(JsErrorBox::generic)
}

deno_core::extension!(
    host_v8_share,
    deps = [host_v8_base],
    ops = [
        op_share_app_message,
        op_share_message_to_friend,
        op_show_share_image_menu,
    ],
    esm_entry_point = "ext:host_v8_share/99_global_scope.js",
    esm = [
        dir "src/share",
        "01_share.js",
        "99_global_scope.js",
    ],
);

pub fn share_extensions() -> Vec<Extension> {
    vec![host_v8_share::init()]
}

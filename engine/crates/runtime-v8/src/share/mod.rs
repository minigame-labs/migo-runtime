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

use deno_core::serde_json::{self, Value};

use crate::file::host_paths::{self, export_paths};
use migo_services::host_paths as files;

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
    let request = export_paths(&state, &request_json, files::SHARE_APP_MESSAGE).await?;
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
    let request = export_paths(&state, &request_json, files::SHARE_MESSAGE_TO_FRIEND).await?;
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
    let request = export_paths(&state, &request_json, files::SHOW_SHARE_IMAGE_MENU).await?;
    service
        .show_share_image_menu(&request)
        .map_err(JsErrorBox::generic)
}

/// Share the game's last finished recording through the host, with the share
/// options content set. The video is the engine's own file, never a path
/// content named; `bgm` is resolved through the sandbox. What it is refused
/// for before the host is asked (`migo_services::game_recorder`) the
/// recorder's module (`05_game_recorder.js`) answers in the reference's codes.
#[op2]
pub async fn op_share_game_recording(
    state: Rc<RefCell<OpState>>,
    #[string] request_json: String,
) -> Result<(), JsErrorBox> {
    let service = share_service(&state, "operateGameRecorderVideo:fail not supported")?;
    let paths = state.borrow().borrow::<HostOpState>().game_paths.clone();
    let request = migo_services::game_recorder::share_request(
        &host_paths::sandbox(&state),
        paths.as_deref(),
        &request_json,
    )
    .await
    .map_err(|error| JsErrorBox::generic(error.message))?;
    service
        .share_game_recording(&request)
        .map_err(JsErrorBox::generic)
}

/// Tell the host's menu what it offers now.
#[op2(fast)]
pub fn op_share_set_menu(state: &mut OpState, #[string] json: &str) -> Result<(), JsErrorBox> {
    state
        .borrow::<HostOpState>()
        .device_services
        .as_ref()
        .and_then(|services| services.share())
        .ok_or_else(|| JsErrorBox::generic("share menu: not supported"))?
        .set_share_menu(json)
        .map_err(JsErrorBox::generic)
}

/// Answer the share the player picked from the host's menu. No result follows
/// a reply, so what it copies out of the package is held for the session; an
/// image that is not one of content's files is dropped, and the host shares
/// with its own default.
#[op2]
pub async fn op_share_menu_reply(
    state: Rc<RefCell<OpState>>,
    #[string] reply_json: String,
) -> Result<(), JsErrorBox> {
    let service = share_service(&state, "share menu: not supported")?;
    let mut reply: Value = serde_json::from_str(&reply_json)
        .map_err(|e| JsErrorBox::generic(format!("malformed reply: {e}")))?;
    host_paths::export_menu_share(&state, &mut reply).await;
    service
        .menu_share_reply(&reply.to_string())
        .map_err(JsErrorBox::generic)
}

main_runtime_extension!(
    host_v8_share,
    {
        deps = [host_v8_base],
        ops = [
            op_share_app_message,
            op_share_message_to_friend,
            op_show_share_image_menu,
            op_share_game_recording,
            op_share_set_menu,
            op_share_menu_reply,
        ],
    },
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

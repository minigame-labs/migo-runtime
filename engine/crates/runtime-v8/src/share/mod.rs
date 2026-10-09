//! Share APIs: showShareMenu, updateShareMenu, onShareAppMessage, shareAppMessage,
//! shareMessageToFriend, showShareImageMenu.
//!
//! The three requests the host answers are each their own op (Mode C): sharing,
//! sharing to one friend, and the image share sheet are different host flows
//! with different results, and folding them into one op left the host unable
//! to tell them apart. The host can also trigger share via
//! `_internalTriggerShareAppMessage`.

use deno_core::{Extension, OpState, op2};
use deno_error::JsErrorBox;
use shared::op_state::HostOpState;

/// Generate a Mode C share op that forwards its request to the host's
/// [`ShareService`](shared::services::ShareService).
macro_rules! share_op {
    ($op_name:ident, $method:ident, $api:literal) => {
        #[op2(fast)]
        pub fn $op_name(
            state: &mut OpState,
            #[string] options_json: String,
        ) -> Result<(), JsErrorBox> {
            let host = state.borrow::<HostOpState>();
            match host
                .device_services
                .as_ref()
                .and_then(|services| services.share())
            {
                Some(svc) => svc.$method(&options_json).map_err(JsErrorBox::generic),
                None => Err(JsErrorBox::generic(concat!($api, ":fail not supported"))),
            }
        }
    };
}

share_op!(op_share_app_message, share_app_message, "shareAppMessage");
share_op!(
    op_share_message_to_friend,
    share_message_to_friend,
    "shareMessageToFriend"
);
share_op!(
    op_show_share_image_menu,
    show_share_image_menu,
    "showShareImageMenu"
);

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

//! UI interaction ops and ESM modules.
//!
//! This module provides cross-platform UI interaction APIs (toast, modal,
//! loading, action sheet) using trait-based services injected via
//! `HostOpState.device_services`.

use deno_core::{Extension, OpState, op2};
use deno_error::JsErrorBox;
use shared::op_state::HostOpState;

// ==================== Toast Ops ====================

#[op2(fast)]
pub fn op_show_toast(state: &mut OpState, #[string] json: String) -> Result<(), JsErrorBox> {
    let host = state.borrow::<HostOpState>();
    if let Some(ref services) = host.device_services {
        if let Some(interaction) = services.interaction() {
            return interaction.show_toast(&json).map_err(JsErrorBox::generic);
        }
    }
    Err(JsErrorBox::generic("showToast:fail not supported"))
}

#[op2(fast)]
pub fn op_hide_toast(state: &mut OpState) -> Result<(), JsErrorBox> {
    let host = state.borrow::<HostOpState>();
    if let Some(ref services) = host.device_services {
        if let Some(interaction) = services.interaction() {
            return interaction.hide_toast().map_err(JsErrorBox::generic);
        }
    }
    Err(JsErrorBox::generic("hideToast:fail not supported"))
}

// ==================== Modal Ops ====================

#[op2(fast)]
pub fn op_show_modal(state: &mut OpState, #[string] json: String) -> Result<(), JsErrorBox> {
    let host = state.borrow::<HostOpState>();
    if let Some(ref services) = host.device_services {
        if let Some(interaction) = services.interaction() {
            return interaction.show_modal(&json).map_err(JsErrorBox::generic);
        }
    }
    Err(JsErrorBox::generic("showModal:fail not supported"))
}

// ==================== Loading Ops ====================

#[op2(fast)]
pub fn op_show_loading(state: &mut OpState, #[string] json: String) -> Result<(), JsErrorBox> {
    let host = state.borrow::<HostOpState>();
    if let Some(ref services) = host.device_services {
        if let Some(interaction) = services.interaction() {
            return interaction.show_loading(&json).map_err(JsErrorBox::generic);
        }
    }
    Err(JsErrorBox::generic("showLoading:fail not supported"))
}

#[op2(fast)]
pub fn op_hide_loading(state: &mut OpState) -> Result<(), JsErrorBox> {
    let host = state.borrow::<HostOpState>();
    if let Some(ref services) = host.device_services {
        if let Some(interaction) = services.interaction() {
            return interaction.hide_loading().map_err(JsErrorBox::generic);
        }
    }
    Err(JsErrorBox::generic("hideLoading:fail not supported"))
}

// ==================== Action Sheet Ops ====================

#[op2(fast)]
pub fn op_show_action_sheet(state: &mut OpState, #[string] json: String) -> Result<(), JsErrorBox> {
    let host = state.borrow::<HostOpState>();
    if let Some(ref services) = host.device_services {
        if let Some(interaction) = services.interaction() {
            return interaction
                .show_action_sheet(&json)
                .map_err(JsErrorBox::generic);
        }
    }
    Err(JsErrorBox::generic("showActionSheet:fail not supported"))
}

// ==================== Menu Button Ops ====================

#[op2]
#[string]
pub fn op_get_menu_button_rect(state: &mut OpState) -> Result<String, JsErrorBox> {
    let host = state.borrow::<HostOpState>();
    if let Some(ref services) = host.device_services {
        if let Some(sys_info) = services.system_info() {
            return sys_info
                .get_menu_button_bounding_client_rect_json()
                .map_err(JsErrorBox::generic);
        }
    }
    Err(JsErrorBox::generic(
        "getMenuButtonBoundingClientRect:fail not supported",
    ))
}

// ==================== Extension Definition ====================

// ==================== Window (desktop) ====================

/// The desktop window service, or `err_msg` when the host has none.
fn window(
    state: &OpState,
    err_msg: &'static str,
) -> Result<std::sync::Arc<dyn shared::services::WindowService>, JsErrorBox> {
    state
        .borrow::<HostOpState>()
        .device_services
        .as_ref()
        .and_then(|services| services.window())
        .ok_or_else(|| JsErrorBox::generic(err_msg))
}

#[op2(fast)]
pub fn op_set_window_size(
    state: &mut OpState,
    #[string] request_json: &str,
) -> Result<(), JsErrorBox> {
    window(state, "setWindowSize:fail not supported")?
        .set_window_size(request_json)
        .map_err(JsErrorBox::generic)
}

/// `setCursor(path, x, y)`: whether the cursor was taken -- a keyword, or an
/// image file the game can read, handed to a host with a window. The host loads
/// the image; `false` is the answer where there is no window to point at.
#[op2(fast)]
pub fn op_set_cursor(state: &mut OpState, #[string] path: &str, x: f64, y: f64) -> bool {
    let Ok(service) = window(state, "setCursor:fail not supported") else {
        return false;
    };
    let sandbox = crate::file::host_paths::sandbox_of(state);
    if !state.has::<migo_services::cursor::CursorCopies>() {
        state.put(migo_services::cursor::CursorCopies::default());
    }
    let copies = state.borrow::<migo_services::cursor::CursorCopies>();
    let Some(request) = migo_services::cursor::request(&sandbox, copies, path, x, y) else {
        return false;
    };
    service.set_cursor(&request).is_ok()
}

#[op2(fast)]
pub fn op_request_pointer_lock(state: &mut OpState) -> Result<(), JsErrorBox> {
    window(state, "requestPointerLock:fail not supported")?
        .request_pointer_lock()
        .map_err(JsErrorBox::generic)
}

#[op2(fast)]
pub fn op_exit_pointer_lock(state: &mut OpState) -> Result<(), JsErrorBox> {
    window(state, "exitPointerLock:fail not supported")?
        .exit_pointer_lock()
        .map_err(JsErrorBox::generic)
}

deno_core::extension!(
    host_v8_ui,
    deps = [host_v8_base],
    ops = [
        op_show_toast,
        op_hide_toast,
        op_show_modal,
        op_show_loading,
        op_hide_loading,
        op_show_action_sheet,
        op_get_menu_button_rect,
        op_set_window_size,
        op_set_cursor,
        op_request_pointer_lock,
        op_exit_pointer_lock,
    ],
    esm_entry_point = "ext:host_v8_ui/99_global_scope.js",
    esm = [
        dir "src/ui",
        "01_interaction.js",
        "02_buttons.js",
        "04_window.js",
        "05_button_layer.js",
        "99_global_scope.js",
    ]
);

pub fn ui_extensions() -> Vec<Extension> {
    vec![host_v8_ui::init()]
}

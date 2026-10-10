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

/// The CSS cursor keywords `setCursor` passes through by name; anything else is
/// a path.
const CURSOR_KEYWORDS: &[&str] = &[
    "default",
    "auto",
    "none",
    "context-menu",
    "help",
    "pointer",
    "progress",
    "wait",
    "cell",
    "crosshair",
    "text",
    "vertical-text",
    "alias",
    "copy",
    "move",
    "no-drop",
    "not-allowed",
    "grab",
    "grabbing",
    "all-scroll",
    "col-resize",
    "row-resize",
    "n-resize",
    "e-resize",
    "s-resize",
    "w-resize",
    "ne-resize",
    "nw-resize",
    "se-resize",
    "sw-resize",
    "ew-resize",
    "ns-resize",
    "nesw-resize",
    "nwse-resize",
    "zoom-in",
    "zoom-out",
];

/// The copies made of package entries named as cursors, one per entry for the
/// session: a game switching between a few cursors names the same few files,
/// and a fresh copy for every switch would grow without bound.
#[derive(Default)]
struct CursorCopies(std::collections::HashMap<String, String>);

/// The real file a cursor path names, or `None` when it names none the game can
/// read.
fn cursor_file(state: &mut OpState, path: &str) -> Option<String> {
    let (host_id, vfs, mount_table) = {
        let host = state.borrow::<HostOpState>();
        (host.id, host.vfs.clone(), host.mount_table.clone())
    };
    match migo_services::fs::resolve_path_vfs(
        vfs.as_deref(),
        mount_table.as_deref(),
        path,
        shared::vfs::FileOp::Read,
    )
    .ok()?
    {
        migo_services::fs::ResolvedPath::Filesystem(file) => std::fs::metadata(&file)
            .is_ok_and(|meta| meta.is_file())
            .then_some(file),
        migo_services::fs::ResolvedPath::Pack { virtual_path } => {
            if let Some(copy) = state
                .try_borrow::<CursorCopies>()
                .and_then(|copies| copies.0.get(&virtual_path))
            {
                return Some(copy.clone());
            }
            let suffix = std::path::Path::new(&virtual_path)
                .extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| format!(".{ext}"))
                .unwrap_or_default();
            let scheduler = state
                .borrow::<crate::io_state::IoSchedulerState>()
                .0
                .clone();
            // A cursor is a few kilobytes: copied here, in the call that names it.
            let copy = migo_services::fs::materialize_pack_to_temp_checked(
                &scheduler,
                mount_table.as_deref(),
                &virtual_path,
                &suffix,
            )
            .ok()?;
            if !shared::services::host_files::hold_export(
                host_id,
                0,
                std::path::PathBuf::from(&copy),
            ) {
                let _ = std::fs::remove_file(&copy);
                return None;
            }
            if !state.has::<CursorCopies>() {
                state.put(CursorCopies::default());
            }
            state
                .borrow_mut::<CursorCopies>()
                .0
                .insert(virtual_path, copy.clone());
            Some(copy)
        }
    }
}

/// `setCursor(path, x, y)`: whether the cursor was taken -- a keyword, or an
/// image file the game can read, handed to a host with a window. The host loads
/// the image; `false` is the answer where there is no window to point at.
#[op2(fast)]
pub fn op_set_cursor(state: &mut OpState, #[string] path: &str, x: f64, y: f64) -> bool {
    let Ok(service) = window(state, "setCursor:fail not supported") else {
        return false;
    };
    let request = if CURSOR_KEYWORDS.contains(&path) {
        deno_core::serde_json::json!({ "keyword": path })
    } else {
        let Some(file) = cursor_file(state, path) else {
            return false;
        };
        let finite = |value: f64| if value.is_finite() { value } else { 0.0 };
        deno_core::serde_json::json!({ "path": file, "x": finite(x), "y": finite(y) })
    };
    service.set_cursor(&request.to_string()).is_ok()
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
        "03_page_manager.js",
        "04_window.js",
        "99_global_scope.js",
    ]
);

pub fn ui_extensions() -> Vec<Extension> {
    vec![host_v8_ui::init()]
}

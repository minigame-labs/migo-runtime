use std::sync::Arc;

use deno_core::{OpState, op2};
use tracing::{error, info};

use shared::{
    font_registration::{build_font_registration_request, resolve_font_src_path},
    op_state::{CanvasOpState, HostOpState},
    protocol::{render_cmd::RenderCommand, send_render_with_resp_sync},
};

/// `pub(super)` so `context2d`'s deadline-class test can cover every sync op
/// name in one list — see `each_sync_op_name_still_selects_the_deadline_its_op_needs`.
pub(super) const OP_LOAD_FONT: &str = "load_font";
pub(super) const OP_GET_TEXT_LINE_HEIGHT: &str = "get_text_line_height";

/// Load a custom font file and register it globally.
///
/// Resolves the path relative to the game's code directory, reads the font
/// bytes, sends them to the render thread for registration in both the global
/// font store and all existing canvas FontManagers.
///
/// Returns the font family key on success, or empty string on failure.
#[op2]
#[string]
pub(crate) fn op_load_font(
    state: &mut OpState,
    #[string] path: String,
    #[string] family: Option<String>,
) -> String {
    // Resolve font path with VFS support (/code, /user, /cache, /tmp).
    let resolved = {
        let host = state.borrow::<HostOpState>();
        let code_dir = host.code_dir.as_deref().unwrap_or("");
        match resolve_font_src_path(code_dir, host.vfs.as_deref(), &path) {
            Ok(p) => p,
            Err(e) => {
                error!("op_load_font: failed to resolve '{}': {}", path, e);
                return String::new();
            }
        }
    };

    // Read font file bytes.
    let bytes = match std::fs::read(&resolved) {
        Ok(b) => b,
        Err(e) => {
            error!("op_load_font: failed to read '{}': {}", resolved, e);
            return String::new();
        }
    };

    if bytes.is_empty() {
        error!("op_load_font: font file is empty: {}", resolved);
        return String::new();
    }

    let request = build_font_registration_request(&path, family.as_deref());

    let bytes = Arc::new(bytes);
    let aliases = Arc::new(request.aliases.clone());

    // Send to render thread for registration.
    let ctx = state.borrow::<CanvasOpState>();
    match send_render_with_resp_sync(ctx, OP_LOAD_FONT, |resp| RenderCommand::LoadFont {
        family: request.family.clone(),
        aliases: aliases.clone(),
        bytes: bytes.clone(),
        resp,
    }) {
        Ok(family) => {
            info!(
                "op_load_font: loaded '{}' as '{}' with aliases {:?}",
                path, family, request.aliases
            );
            family
        }
        Err(e) => {
            error!("op_load_font: render thread error: {}", e);
            String::new()
        }
    }
}

/// Measure the line height of text with the given font configuration.
///
/// Parameters are parsed from the JS object: fontStyle, fontWeight, fontSize, fontFamily.
/// Returns the line height in pixels (ascender - descender), or fontSize * 1.2 as fallback.
#[op2(fast)]
pub(crate) fn op_get_text_line_height(
    state: &mut OpState,
    #[string] font_family: String,
    font_size: f64,
    bold: bool,
    italic: bool,
) -> f64 {
    let fs = font_size as f32;
    let ctx = state.borrow::<CanvasOpState>();
    match send_render_with_resp_sync(ctx, OP_GET_TEXT_LINE_HEIGHT, |resp| {
        RenderCommand::GetTextLineHeight {
            font_family,
            font_size: fs,
            bold,
            italic,
            resp,
        }
    }) {
        Ok(height) => height as f64,
        Err(e) => {
            error!("op_get_text_line_height: render thread error: {}", e);
            font_size * 1.2
        }
    }
}

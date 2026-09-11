//! GL state-change deduplication predicates and update helpers.
//!
//! `StateTracker` is intentionally *stateless* — it takes a mutable
//! reference to the per-canvas [`crate::canvas::CanvasGLState`] and
//! reports "is this GL call redundant?" / records the new value after
//! the driver call.  Keeping this GL-free makes the dedup logic unit-
//! testable without an EGL context (see `tests` module at the bottom)
//! and also prevents the tracker from diverging from the actual GL
//! state — every mutation goes through one of the helpers below.
//!
//! Pattern used by the WebGL handler:
//!
//! ```ignore
//! if st::update_use_program(&mut cm.gl_state[cid], program_id) {
//!     unsafe { gl.use_program(Some(handle)) };
//! }
//! ```
//!
//! The `update_*` helper returns `true` when a real GL call must be
//! issued (the tracked value differs, or we have no prior value yet)
//! and `false` when the call is redundant.

use glow::{self};

use crate::canvas::{BlendEquation, BlendFactors, CanvasGLState, MAX_UNIFORM_CACHE};
use shared::protocol::render_cmd::{BufferId, ProgramId, VaoId};

/// Pass a "must issue" decision through the `state_changes` diagnostic.
///
/// **Every `update_*` in this module returns through here, and that is what
/// makes the counter mean something.** `state_changes` was plumbed end to end —
/// accumulator field, forwarder, atomic aggregation, debug overlay — and never
/// incremented from anywhere, so it read zero on every frame of every build.
/// Zero is the opposite of the truth on this path: state calls outnumber draws
/// by an order of magnitude, which is why this whole module exists.
///
/// A `true` from an `update_*` means the caller issues the driver call —
/// checked at all 37 call sites in [`crate::renderergl`], including the four
/// that bind the result to a local first — so this counts driver state calls,
/// not merely shadow mutations. It rides along with a `gl.*` call that costs
/// hundreds of nanoseconds, so a thread-local increment on that branch is
/// noise; the deduped branch, which is the hot one, does not touch it.
///
/// With it live, `state_changes / draw_calls` is readable per frame, which is
/// what any question about dedup effectiveness or draw-call batching needs and
/// nobody could ask before.
///
/// **The counter cannot over-count, structurally.** Every `update_*` leaves by
/// `return false` on its deduped path, before reaching here, so this is
/// unreachable for a call that does not issue — the `must_issue` branch is
/// belt-and-braces rather than the thing doing the work. Found while trying to
/// mutate the counter into over-counting and failing: making this increment
/// unconditionally changed no test, because the dedup path never arrives. The
/// one shape that *can* over-count is wrapping a convenience setter alongside
/// the `_separate` form it delegates to, and
/// `a_delegating_setter_is_counted_once_not_twice` covers that.
#[inline]
fn issue_if(must_issue: bool) -> bool {
    if must_issue {
        crate::render_diagnostics::bump_state_change();
    }
    must_issue
}

// ============================================================================
// Program / uniforms
// ============================================================================

/// Returns `true` if `glUseProgram(new)` must actually be issued.
///
/// The tracker treats "unknown" (no previous program) as "must issue".
///
/// **This deliberately leaves `uniform_cache` alone.** A uniform's value is
/// state of the *program object* (GLES 3.0 §2.11.6, WebGL 1.0 §5.14.10), not
/// of the context, so `glUseProgram` cannot change it and every cached entry
/// stays true across a switch. An earlier revision dropped the outgoing
/// program's entries here, which cost an O(cache) walk plus a heap free per
/// switch and left nothing cached for the workload the dedup exists for —
/// a frame that cycles between two or three programs. Pinned by
/// `cycling_between_programs_keeps_each_programs_uniform_cache`.
///
/// The event that *does* invalidate these entries is a successful re-link;
/// see [`invalidate_program_uniforms`].
pub(crate) fn update_use_program(state: &mut CanvasGLState, new: ProgramId) -> bool {
    if state.current_program == Some(new) {
        return false;
    }
    state.current_program = Some(new);
    issue_if(true)
}

/// Forget every cached uniform value for `program`.
///
/// **The one event that makes these entries wrong.** A successful
/// `glLinkProgram` — and `glProgramBinary`, which GLES 3.0 §2.11.4 defines to
/// behave as a successful link — allocates fresh uniform storage and
/// initialises it, so every value the driver held is gone. Leave the shadow in
/// place and the content's next upload of an unchanged value gets deduped
/// against a driver that is now holding zero: the uniform silently stays at
/// its initial value and the draw paints with it. Static camera plus a re-link
/// is enough, and re-linking is not exotic — Pixi v8 sorts attributes and
/// re-links, which is why `ProgramMeta::attrib_bindings` exists.
///
/// Also the right call on delete: program names come from the client, so a
/// recycled name would otherwise inherit the dead program's entries.
pub(crate) fn invalidate_program_uniforms(state: &mut CanvasGLState, program: ProgramId) {
    state.uniform_cache.retain(|(prog, _), _| *prog != program);
}

/// Forget everything this canvas shadowed about a program that has been
/// deleted.
///
/// Separate from [`invalidate_program_uniforms`] because deletion also drops
/// the *binding*, and a re-link does not: GLES 3.0 §2.11.4 installs the new
/// executable under the same name, so `current_program` still names the
/// re-linked program and clearing it there would buy a redundant
/// `glUseProgram` per re-link.
///
/// Clearing `current_program` matters because program names are chosen by the
/// client. A name that is deleted and reused names a different program object,
/// and a shadow still claiming it is current dedups the `glUseProgram` that
/// would install it — so the draw runs under whatever program the driver
/// actually has bound.
pub(crate) fn forget_deleted_program(state: &mut CanvasGLState, program: ProgramId) {
    if state.current_program == Some(program) {
        state.current_program = None;
    }
    invalidate_program_uniforms(state, program);
}

/// Hash-and-compare dedup for a single-location uniform upload.
///
/// Existing scalar-only callers use this wrapper; ranged setters must call
/// [`update_uniform_range`] with the number of consecutive locations they
/// write. Keeping the scalar wrapper avoids making unrelated state-tracker
/// tests and helpers repeat a constant while ensuring renderer array paths
/// carry their actual range.
pub(crate) fn update_uniform(
    state: &mut CanvasGLState,
    program: ProgramId,
    location: u32,
    value_bytes: &[u8],
) -> bool {
    update_uniform_range(state, program, location, 1, value_bytes)
}

/// Hash-and-compare dedup for a uniform upload spanning `location_count`
/// consecutive GL uniform locations.
pub(crate) fn update_uniform_range(
    state: &mut CanvasGLState,
    program: ProgramId,
    location: u32,
    location_count: u32,
    value_bytes: &[u8],
) -> bool {
    issue_if(super::uniform_cache::update(
        &mut state.uniform_cache,
        MAX_UNIFORM_CACHE,
        program,
        location,
        location_count,
        value_bytes,
    ))
}

// ============================================================================
// Buffers + VAOs
// ============================================================================

/// Track `glBindBuffer(target, buf)`.  Dedups all WebGL 1 / 2
/// generic targets: ARRAY_BUFFER, ELEMENT_ARRAY_BUFFER,
/// UNIFORM_BUFFER, PIXEL_UNPACK_BUFFER, PIXEL_PACK_BUFFER,
/// COPY_READ_BUFFER, COPY_WRITE_BUFFER, TRANSFORM_FEEDBACK_BUFFER.
///
/// Returns `true` when the caller must issue the underlying
/// `glBindBuffer`, `false` when the binding already matches.  Any
/// target we don't track is conservatively forwarded (returns
/// `true`) rather than silently skipped.
pub(crate) fn update_bind_buffer(
    state: &mut CanvasGLState,
    target: u32,
    new: Option<BufferId>,
) -> bool {
    const GL_ARRAY_BUFFER: u32 = 0x8892;
    const GL_ELEMENT_ARRAY_BUFFER: u32 = 0x8893;
    const GL_UNIFORM_BUFFER: u32 = 0x8A11;
    const GL_PIXEL_UNPACK_BUFFER: u32 = 0x88EC;
    const GL_PIXEL_PACK_BUFFER: u32 = 0x88EB;
    const GL_COPY_READ_BUFFER: u32 = 0x8F36;
    const GL_COPY_WRITE_BUFFER: u32 = 0x8F37;
    const GL_TRANSFORM_FEEDBACK_BUFFER: u32 = 0x8C8E;
    let new_opt = Some(new);
    // Macro keeps the per-target slot pattern uniform: match the
    // previously-bound value, store the new one, and bubble up a
    // "must issue" flag.  Inlined so this stays a branchless
    // match plus a single comparison in the common case.
    macro_rules! dedup_slot {
        ($slot:ident) => {{
            if state.$slot == new_opt {
                false
            } else {
                state.$slot = new_opt;
                issue_if(true)
            }
        }};
    }
    match target {
        GL_ARRAY_BUFFER => dedup_slot!(bound_array_buffer),
        GL_ELEMENT_ARRAY_BUFFER => dedup_slot!(bound_element_array_buffer),
        GL_UNIFORM_BUFFER => dedup_slot!(bound_uniform_buffer),
        GL_PIXEL_UNPACK_BUFFER => dedup_slot!(bound_pixel_unpack_buffer),
        GL_PIXEL_PACK_BUFFER => dedup_slot!(bound_pixel_pack_buffer),
        GL_COPY_READ_BUFFER => dedup_slot!(bound_copy_read_buffer),
        GL_COPY_WRITE_BUFFER => dedup_slot!(bound_copy_write_buffer),
        GL_TRANSFORM_FEEDBACK_BUFFER => dedup_slot!(bound_transform_feedback_buffer),
        _ => issue_if(true),
    }
}

/// Track `glBindBufferBase(target, index, buf)` (WebGL 2).
/// Dedups per-index UNIFORM_BUFFER indexed bindings.  A full
/// `(buffer, 0, 0)` range entry is stored so that a subsequent
/// `bindBufferBase` (full-buffer semantics) matches but a
/// `bindBufferRange` with a different offset/size re-binds.
///
/// Returns `true` when the GL call must be issued.
pub(crate) fn update_bind_buffer_base(
    state: &mut CanvasGLState,
    target: u32,
    index: u32,
    buffer: Option<BufferId>,
) -> bool {
    const GL_UNIFORM_BUFFER: u32 = 0x8A11;
    // TRANSFORM_FEEDBACK_BUFFER (0x8C8E) also has indexed binding,
    // but it pairs with an in-progress transform-feedback object
    // and re-binding it mid-feedback is already guarded by the GL
    // driver; we stay conservative and don't dedup it here.
    if target != GL_UNIFORM_BUFFER {
        return issue_if(true);
    }
    issue_if(
        state
            .bound_uniform_buffer_indexed
            .update(index, (buffer, 0, 0)),
    )
}

/// Track `glBindBufferRange(target, index, buf, offset, size)`
/// (WebGL 2).  Dedup key includes offset and size so partial
/// re-binds never coalesce with full bindings.
pub(crate) fn update_bind_buffer_range(
    state: &mut CanvasGLState,
    target: u32,
    index: u32,
    buffer: Option<BufferId>,
    offset: i32,
    size: i32,
) -> bool {
    const GL_UNIFORM_BUFFER: u32 = 0x8A11;
    if target != GL_UNIFORM_BUFFER {
        return issue_if(true);
    }
    issue_if(
        state
            .bound_uniform_buffer_indexed
            .update(index, (buffer, offset, size)),
    )
}

pub(crate) fn update_bind_vertex_array(state: &mut CanvasGLState, new: Option<VaoId>) -> bool {
    let new = new.unwrap_or(0);
    if state.bound_vao == Some(new) {
        return false;
    }
    state.bound_vao = Some(new);
    // Binding a new VAO changes the ARRAY_BUFFER binding semantically,
    // but the ELEMENT_ARRAY_BUFFER binding is stored *inside* the VAO —
    // forget our cached values to stay correct.
    state.bound_array_buffer = None;
    state.bound_element_array_buffer = None;
    issue_if(true)
}

// ============================================================================
// Textures
// ============================================================================

pub(crate) fn update_active_texture(state: &mut CanvasGLState, unit: u32) -> bool {
    if state.active_texture_unit == Some(unit) {
        return false;
    }
    state.active_texture_unit = Some(unit);
    issue_if(true)
}

/// Dedup `glBindTexture(TEXTURE_2D, tex)` scoped to the currently
/// active texture unit.  Callers without explicit active-unit tracking
/// must treat this as a pessimistic update (return `true`).
pub(crate) fn update_bind_texture_2d(state: &mut CanvasGLState, tex: Option<u32>) -> bool {
    let unit = match state.active_texture_unit {
        Some(u) => u,
        None => return issue_if(true),
    };
    issue_if(state.bound_texture_2d.bind(unit, tex))
}

// ============================================================================
// Viewport
// ============================================================================

/// Update the viewport fingerprint, returning `true` iff the driver
/// call must be issued.  Viewport is set on every frame by many
/// engines (often with the same values) — without dedup we pay an
/// unconditional GL round-trip per frame per canvas.
#[inline]
pub(crate) fn update_viewport(
    state: &mut CanvasGLState,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
) -> bool {
    let new = (x, y, width, height);
    if state.viewport == Some(new) {
        return false;
    }
    state.viewport = Some(new);
    issue_if(true)
}

// ============================================================================
// Blend state
// ============================================================================

/// No [`issue_if`] here on purpose: the `_separate` form it delegates to
/// already counts. Wrapping both would report two driver calls for one.
pub(crate) fn update_blend_func(state: &mut CanvasGLState, src: u32, dst: u32) -> bool {
    update_blend_func_separate(state, src, dst, src, dst)
}

pub(crate) fn update_blend_func_separate(
    state: &mut CanvasGLState,
    src_rgb: u32,
    dst_rgb: u32,
    src_alpha: u32,
    dst_alpha: u32,
) -> bool {
    let new = BlendFactors {
        src_rgb,
        dst_rgb,
        src_alpha,
        dst_alpha,
    };
    if state.blend_factors == Some(new) {
        return false;
    }
    state.blend_factors = Some(new);
    issue_if(true)
}

/// Counted by the `_separate` form it delegates to; see
/// [`update_blend_func`].
pub(crate) fn update_blend_equation(state: &mut CanvasGLState, mode: u32) -> bool {
    update_blend_equation_separate(state, mode, mode)
}

pub(crate) fn update_blend_equation_separate(
    state: &mut CanvasGLState,
    mode_rgb: u32,
    mode_alpha: u32,
) -> bool {
    let new = BlendEquation {
        mode_rgb,
        mode_alpha,
    };
    if state.blend_equation == Some(new) {
        return false;
    }
    state.blend_equation = Some(new);
    issue_if(true)
}

pub(crate) fn update_blend_color(
    state: &mut CanvasGLState,
    r: f32,
    g: f32,
    b: f32,
    a: f32,
) -> bool {
    let new = (r, g, b, a);
    if state.blend_color == Some(new) {
        return false;
    }
    state.blend_color = Some(new);
    issue_if(true)
}

// ============================================================================
// Depth / cull / front-face / misc scalars
// ============================================================================

pub(crate) fn update_depth_func(state: &mut CanvasGLState, func: u32) -> bool {
    if state.depth_func == Some(func) {
        return false;
    }
    state.depth_func = Some(func);
    issue_if(true)
}

pub(crate) fn update_depth_mask(state: &mut CanvasGLState, flag: bool) -> bool {
    if state.depth_mask == Some(flag) {
        return false;
    }
    state.depth_mask = Some(flag);
    issue_if(true)
}

pub(crate) fn update_depth_range(state: &mut CanvasGLState, near: f32, far: f32) -> bool {
    let new = (near, far);
    if state.depth_range == Some(new) {
        return false;
    }
    state.depth_range = Some(new);
    issue_if(true)
}

// ============================================================================
// Stencil state
// ============================================================================

/// Stencil-state dedup:  same (func, ref, mask) against the same face
/// is a no-op on the driver but a real round-trip without this.
/// `face` is `glow::FRONT`, `glow::BACK`, or `glow::FRONT_AND_BACK`;
/// we track per-face separately for the `_separate` calls and
/// duplicate the write when `FRONT_AND_BACK` is the face.
#[inline]
pub(crate) fn update_stencil_func(
    state: &mut CanvasGLState,
    face: u32,
    func: u32,
    ref_: i32,
    mask: u32,
) -> bool {
    issue_if(state.stencil_func.update(face, (func, ref_, mask)))
}

#[inline]
pub(crate) fn update_stencil_op(
    state: &mut CanvasGLState,
    face: u32,
    sfail: u32,
    dpfail: u32,
    dppass: u32,
) -> bool {
    issue_if(state.stencil_op.update(face, (sfail, dpfail, dppass)))
}

#[inline]
pub(crate) fn update_stencil_mask(state: &mut CanvasGLState, face: u32, mask: u32) -> bool {
    issue_if(state.stencil_mask.update(face, mask))
}

// ============================================================================
// Pixel-storei
// ============================================================================

/// Many engines call `pixelStorei` repeatedly with identical `(pname,
/// param)` tuples between texture uploads.  A single HashMap keyed by
/// `pname` covers every pname the driver accepts; unknown pnames fall
/// back to "update and issue" every call (same as before).
#[inline]
pub(crate) fn update_pixel_store_i32(state: &mut CanvasGLState, pname: u32, param: i32) -> bool {
    issue_if(state.pixel_store_i32.update(pname, param))
}

pub(crate) fn update_cull_face(state: &mut CanvasGLState, mode: u32) -> bool {
    if state.cull_face == Some(mode) {
        return false;
    }
    state.cull_face = Some(mode);
    issue_if(true)
}

pub(crate) fn update_front_face(state: &mut CanvasGLState, mode: u32) -> bool {
    if state.front_face == Some(mode) {
        return false;
    }
    state.front_face = Some(mode);
    issue_if(true)
}

/// Dedup `glScissor`, and promote the tracked state when the test is on.
///
/// **This was deliberately absent, and what unblocked it was making the engine's
/// own scissor writes visible to this shadow.** The `GLCmd::Scissor` arm in
/// `renderergl/handler.rs` carried a note explaining why deduping here was
/// unsafe: `dirty_region::apply_scissor` re-pointed the driver's box on the
/// present path for every partial-damage Canvas2D batch, and its partner
/// blanket-disabled the test afterwards — both behind this tracker's back. The
/// classic shape follows: shadow says A, engine set B, content re-asserts A, the
/// dedup eats it, and every later draw is clipped to B. Silent, and wrong pixels
/// rather than a slow frame.
///
/// Those two now go through `ScissorBorrow` and write `last_scissor_rect` from
/// the same computation that feeds the driver — see
/// `dirty_region::the_reported_box_is_always_the_box_the_driver_holds`, which
/// pins the one arm where "what we told the driver" and "what the driver holds"
/// differ (a disable does not reset the box). The blit path never touches the
/// box at all, only the enable bit, and it restores that from what it read. So
/// every writer of the driver's box now updates this field, which is the
/// precondition the note asked for.
pub(crate) fn update_scissor(
    state: &mut CanvasGLState,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
) -> bool {
    let rect = (x, y, width, height);
    if state.last_scissor_rect == Some(rect) {
        return false;
    }
    state.last_scissor_rect = Some(rect);
    // GL retains the box whether or not the test is enabled, so the rect is
    // recorded either way; `ScissorState` only gains the numbers when the test
    // is on, which is what the damage classifier reads.
    if !matches!(state.scissor, crate::ScissorState::Disabled) {
        state.scissor = crate::ScissorState::Enabled {
            x,
            y,
            width,
            height,
        };
    }
    issue_if(true)
}

pub(crate) fn update_line_width(state: &mut CanvasGLState, width: f32) -> bool {
    if state.line_width == Some(width) {
        return false;
    }
    state.line_width = Some(width);
    issue_if(true)
}

pub(crate) fn update_polygon_offset(state: &mut CanvasGLState, factor: f32, units: f32) -> bool {
    let new = (factor, units);
    if state.polygon_offset == Some(new) {
        return false;
    }
    state.polygon_offset = Some(new);
    issue_if(true)
}

// ============================================================================
// Enable / disable capabilities
// ============================================================================

// Skia (Canvas2D Ganesh) shares the GL context with WebGL and toggles
// `GL_STENCIL_TEST` internally without going through this tracker, so
// our shadow drifts. cc.Mask round-avatars get rendered as triangles
// when our dedup skips the next `glEnable` because the shadow says it
// is already on. Always issue real GL calls for STENCIL_TEST.

/// `glEnable(cap)` — returns true if a real call is required.
#[inline]
pub(crate) fn update_enable(state: &mut CanvasGLState, cap: u32) -> bool {
    let changed = state.capabilities.enable(cap);
    issue_if(changed || cap == glow::STENCIL_TEST)
}

/// `glDisable(cap)` — returns true if a real call is required.
#[inline]
pub(crate) fn update_disable(state: &mut CanvasGLState, cap: u32) -> bool {
    let changed = state.capabilities.disable(cap);
    issue_if(changed || cap == glow::STENCIL_TEST)
}

// ============================================================================
// Framebuffer / renderbuffer binding
// ============================================================================

/// `glBindFramebuffer(target, fb)`.  `fb = 0` (driver) == `None` shadow
/// (default FBO).  Returns `true` if the GL call must be issued.
///
/// `FRAMEBUFFER` changes both binding points; READ and DRAW change one each.
/// Only client IDs belong here. Temporary engine bindings must restore actual
/// native handles without entering this client shadow.
pub(crate) fn update_bind_framebuffer(
    state: &mut CanvasGLState,
    target: u32,
    fb: Option<u32>,
) -> bool {
    issue_if(state.bound_framebuffer.update(target, fb))
}

/// Record that the *engine* re-pointed this canvas at its default framebuffer,
/// outside any `bindFramebuffer` the content issued.
///
/// **This has to happen wherever the engine re-points the driver, and not doing
/// it put a render-to-texture pass on the screen.** The shadow is keyed on the
/// user-facing framebuffer name, so content holding its own FBO leaves
/// `Some(name)` here; the engine then re-points the driver at the DrawingBuffer
/// (an EGL switch, the post-swap restore after the blit) and the shadow still
/// claims the content's FBO. The content's next `bindFramebuffer(sameName)` is
/// deduped against that stale claim, never reaches the driver, and the pass draws
/// wherever the engine last pointed. Measured on `scripts/fixtures/rtt-probe`,
/// which presented its render-target colour full-screen on every frame.
///
/// Recording `None` rather than clearing the map is the difference between free
/// and one redundant driver call per frame: `None` *is* the user-facing name for
/// the default framebuffer, so the Cocos-style `bindFramebuffer(FRAMEBUFFER, 0)`
/// every frame — the redundancy this dedup exists for — stays deduped.
///
/// All three targets, because binding `FRAMEBUFFER` sets the draw *and* read
/// bindings, and the blit this most often follows clobbered the read target too.
pub(crate) fn record_default_framebuffer_bind(state: &mut CanvasGLState) {
    state.bound_framebuffer.set_all(None);
    state.draws_to_default_fbo = true;
}

/// `glBindRenderbuffer(RENDERBUFFER, rb)` dedup.  Only one target
/// (`GL_RENDERBUFFER`) exists in GLES; tracked with a single slot.
pub(crate) fn update_bind_renderbuffer(state: &mut CanvasGLState, rb: Option<u32>) -> bool {
    match state.bound_renderbuffer {
        Some(shadow) if shadow == rb => false,
        _ => {
            state.bound_renderbuffer = Some(rb);
            issue_if(true)
        }
    }
}

/// `glColorMask(r, g, b, a)`.
pub(crate) fn update_color_mask(
    state: &mut CanvasGLState,
    r: bool,
    g: bool,
    b: bool,
    a: bool,
) -> bool {
    let new = (r, g, b, a);
    if state.color_mask == new {
        return false;
    }
    state.color_mask = new;
    issue_if(true)
}

// ============================================================================
// Vertex attribute array state
// ============================================================================

/// `glEnableVertexAttribArray(index)`.  Returns `true` when the index
/// isn't already tracked as enabled.
pub(crate) fn update_enable_vertex_attrib(state: &mut CanvasGLState, index: u32) -> bool {
    // Enable state is per-VAO: scope the shadow by the bound VAO so that
    // re-enabling the same index on a different VAO still hits the driver.
    let vao = state.bound_vao.unwrap_or(0);
    issue_if(state.vertex_attribs.enable(vao, index))
}

/// `glDisableVertexAttribArray(index)`.  Returns `true` when the index
/// was previously tracked as enabled *for the currently bound VAO*.
pub(crate) fn update_disable_vertex_attrib(state: &mut CanvasGLState, index: u32) -> bool {
    let vao = state.bound_vao.unwrap_or(0);
    issue_if(state.vertex_attribs.disable(vao, index))
}

/// `glVertexAttribPointer(index, size, type, normalized, stride, offset)`.
///
/// WebGL's most-called non-draw call: Cocos Creator 2.x issues it 8+
/// times per sprite in the no-VAO code path.  Fingerprints the full
/// argument tuple PLUS the bound VAO PLUS the bound `ARRAY_BUFFER`
/// and skips the driver call only when every dimension matches the
/// previously cached entry for `(index)` (scoped by VAO + buffer in
/// the fingerprint).
///
/// Why `array_buffer` is in the fingerprint: WebGL 1.0 §5.14.10 and
/// GLES 3.0 §2.9.5 both say `vertexAttribPointer` captures the
/// currently bound `ARRAY_BUFFER` as the buffer source.  Skipping
/// a call just because `(size,type,...)` repeat — ignoring that
/// the current buffer is different — paints the next draw from the
/// WRONG vertex stream.  Static review caught this as a P0 bug.
pub(crate) fn update_vertex_attrib_pointer(
    state: &mut CanvasGLState,
    index: u32,
    size: i32,
    type_: u32,
    normalized: bool,
    stride: i32,
    offset: i32,
) -> bool {
    let vao = state.bound_vao.unwrap_or(0);
    let fp = crate::canvas::VertexAttribPointerFp {
        size,
        type_,
        normalized,
        stride,
        offset,
        // The inner `Option<u32>` of `bound_array_buffer` is
        // `None` when no buffer has ever been bound (tracker
        // never saw a call) vs `Some(None)` for "known: no
        // buffer".  Both cases collapse to `None` here — we
        // conservatively force re-issue in the "never observed"
        // state by tracking None explicitly in the fingerprint.
        array_buffer: state.bound_array_buffer.and_then(|b| b),
    };
    issue_if(state.vertex_attribs.update_pointer(vao, index, fp))
}

/// `glVertexAttribDivisor(index, divisor)` dedup for WebGL 2 /
/// instanced_arrays.  A cheap keyed-shadow check.
pub(crate) fn update_vertex_attrib_divisor(
    state: &mut CanvasGLState,
    index: u32,
    divisor: u32,
) -> bool {
    // Divisor is per-VAO: scope the shadow by the bound VAO.
    let vao = state.bound_vao.unwrap_or(0);
    issue_if(state.vertex_attribs.update_divisor(vao, index, divisor))
}

/// Test-only helper: construct a fresh state as the baseline for tests.
#[cfg(test)]
pub(crate) fn fresh_state() -> CanvasGLState {
    CanvasGLState::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------------------------------------------------------------------
    // Scissor dedup — see `update_scissor` for why this arrived late
    // ---------------------------------------------------------------------

    #[test]
    fn scissor_first_call_issues_and_a_repeat_is_deduped() {
        let mut s = fresh_state();
        assert!(update_scissor(&mut s, 10, 20, 300, 400));
        assert!(
            !update_scissor(&mut s, 10, 20, 300, 400),
            "the same box was issued twice"
        );
        // Any component differing must reach the driver.
        assert!(update_scissor(&mut s, 11, 20, 300, 400));
        assert!(update_scissor(&mut s, 11, 21, 300, 400));
        assert!(update_scissor(&mut s, 11, 21, 301, 400));
        assert!(update_scissor(&mut s, 11, 21, 301, 401));
        assert!(!update_scissor(&mut s, 11, 21, 301, 401));
    }

    /// GL retains the scissor box whether or not the test is enabled, so the
    /// rect is tracked either way — but `ScissorState` only carries the numbers
    /// when the test is on, because that is the form the damage classifier
    /// reads. A `glScissor` while disabled must not make the state claim the
    /// test is enabled.
    #[test]
    fn scissor_while_disabled_records_the_rect_without_enabling() {
        let mut s = fresh_state();
        s.scissor = crate::ScissorState::Disabled;

        assert!(update_scissor(&mut s, 1, 2, 3, 4));
        assert_eq!(s.last_scissor_rect, Some((1, 2, 3, 4)));
        assert_eq!(
            s.scissor,
            crate::ScissorState::Disabled,
            "a glScissor call while the test is off was read as enabling it"
        );

        // Still deduped on the rect, since the driver holds it.
        assert!(!update_scissor(&mut s, 1, 2, 3, 4));
    }

    /// The `EnabledUnknownRect` state exists because a game can enable the test
    /// before ever calling `glScissor`. The first explicit call must promote it
    /// to a known rect, or the damage classifier keeps falling back to the
    /// viewport for a box it could have had exactly.
    #[test]
    fn scissor_promotes_an_unknown_rect_to_a_known_one() {
        let mut s = fresh_state();
        s.scissor = crate::ScissorState::EnabledUnknownRect;

        assert!(update_scissor(&mut s, 5, 6, 7, 8));
        assert_eq!(
            s.scissor,
            crate::ScissorState::Enabled {
                x: 5,
                y: 6,
                width: 7,
                height: 8
            },
            "an explicit scissor call did not promote EnabledUnknownRect"
        );
    }

    // ---------------------------------------------------------------------
    // Program dedup
    // ---------------------------------------------------------------------
    #[test]
    fn use_program_first_call_issues() {
        let mut s = fresh_state();
        assert!(update_use_program(&mut s, 7));
    }

    #[test]
    fn use_program_same_id_deduped() {
        let mut s = fresh_state();
        assert!(update_use_program(&mut s, 7));
        assert!(!update_use_program(&mut s, 7));
        assert!(!update_use_program(&mut s, 7));
    }

    #[test]
    fn use_program_different_id_reissues() {
        let mut s = fresh_state();
        assert!(update_use_program(&mut s, 1));
        assert!(update_use_program(&mut s, 2));
        assert!(!update_use_program(&mut s, 2));
        assert!(update_use_program(&mut s, 1));
    }

    /// A uniform's value belongs to the program object, not to the context
    /// (GLES 3.0 §2.11.6), so `glUseProgram` does not disturb it and neither
    /// may the shadow. Cycling between two programs per frame — what the
    /// dedup exists for — must leave both programs' entries intact.
    #[test]
    fn cycling_between_programs_keeps_each_programs_uniform_cache() {
        let mut s = fresh_state();
        let bytes = [1u8, 2, 3];

        update_use_program(&mut s, 1);
        assert!(update_uniform(&mut s, 1, 10, &bytes));
        update_use_program(&mut s, 2);
        assert!(update_uniform(&mut s, 2, 10, &bytes));
        update_use_program(&mut s, 1);

        assert!(
            !update_uniform(&mut s, 1, 10, &bytes),
            "program 1's cached uniform was dropped when program 2 became \
             current, so every frame that cycles programs re-uploads every \
             uniform it already deduped"
        );
        update_use_program(&mut s, 2);
        assert!(
            !update_uniform(&mut s, 2, 10, &bytes),
            "program 2's entry was dropped on the way back to program 1"
        );
    }

    // ---- Re-link is the event that invalidates a uniform shadow -----------
    //
    // GLES 3.0 §2.11.4: a successful `LinkProgram` gives the program fresh
    // uniform storage, initialised — so the driver no longer holds whatever we
    // cached. Skipping the re-upload of an unchanged value then leaves the
    // uniform at zero, with no GL error and nothing in a log.

    /// The defect. Without the invalidation the second upload of the *same*
    /// bytes is deduped against a driver that was just reset.
    #[test]
    fn a_relink_makes_the_next_identical_uniform_upload_reach_the_driver() {
        let mut s = fresh_state();
        let prog: ProgramId = 7;
        let mvp = [0u8; 64];

        update_use_program(&mut s, prog);
        assert!(update_uniform(&mut s, prog, 3, &mvp));
        assert!(
            !update_uniform(&mut s, prog, 3, &mvp),
            "fixture must be deduping, or the test below proves nothing"
        );

        invalidate_program_uniforms(&mut s, prog);

        assert!(
            update_uniform(&mut s, prog, 3, &mvp),
            "the driver reset this uniform to zero on re-link, so re-uploading \
             the same value MUST reach GL — deduping it paints with zero"
        );
    }

    /// Scoped to the program that was re-linked. Sweeping the whole table
    /// would satisfy the test above while re-uploading every other program's
    /// uniforms, which is the cost the dedup exists to remove.
    #[test]
    fn a_relink_leaves_other_programs_uniform_shadows_alone() {
        let mut s = fresh_state();
        let bytes = [4u8, 5, 6, 7];

        assert!(update_uniform(&mut s, 1, 2, &bytes));
        assert!(update_uniform(&mut s, 9, 2, &bytes));

        invalidate_program_uniforms(&mut s, 1);

        assert!(
            update_uniform(&mut s, 1, 2, &bytes),
            "the re-linked program's entry survived"
        );
        assert!(
            !update_uniform(&mut s, 9, 2, &bytes),
            "an unrelated program's entry was dropped by a re-link that cannot \
             have touched its uniforms"
        );
    }

    /// A re-link installs the new executable under the same name, so the
    /// binding is unchanged. Clearing it would cost a redundant `glUseProgram`
    /// on every re-link.
    #[test]
    fn a_relink_leaves_the_program_binding_shadow_intact() {
        let mut s = fresh_state();
        assert!(update_use_program(&mut s, 7));

        invalidate_program_uniforms(&mut s, 7);

        assert!(
            !update_use_program(&mut s, 7),
            "the re-link cleared the binding shadow, so the content's next \
             useProgram of the still-current program reaches the driver for nothing"
        );
    }

    /// Deletion is the other half: the name is free for the client to reuse,
    /// and a shadow still claiming it is current dedups away the `glUseProgram`
    /// that would install whatever program the name now refers to.
    #[test]
    fn deleting_the_current_program_clears_the_binding_shadow() {
        let mut s = fresh_state();
        let bytes = [3u8, 3];
        assert!(update_use_program(&mut s, 7));
        assert!(update_uniform(&mut s, 7, 1, &bytes));

        forget_deleted_program(&mut s, 7);

        assert!(
            update_use_program(&mut s, 7),
            "a reused program name was deduped against the deleted program's \
             binding, so the draw runs under whatever the driver still has bound"
        );
        assert!(
            update_uniform(&mut s, 7, 1, &bytes),
            "a reused program name inherited the deleted program's uniform cache"
        );
    }

    /// Deleting a program that is not current must not disturb the binding —
    /// games delete unused programs mid-scene.
    #[test]
    fn deleting_a_non_current_program_leaves_the_binding_shadow_alone() {
        let mut s = fresh_state();
        assert!(update_use_program(&mut s, 7));

        forget_deleted_program(&mut s, 9);

        assert!(
            !update_use_program(&mut s, 7),
            "deleting an unrelated program invalidated the current binding"
        );
    }

    /// Every location of the re-linked program, not just the one a test
    /// happens to name: the driver reset all of them.
    #[test]
    fn a_relink_invalidates_every_location_of_that_program() {
        let mut s = fresh_state();
        let bytes = [1u8];
        for loc in 0..8u32 {
            assert!(update_uniform(&mut s, 5, loc, &bytes));
        }

        invalidate_program_uniforms(&mut s, 5);

        for loc in 0..8u32 {
            assert!(
                update_uniform(&mut s, 5, loc, &bytes),
                "location {loc} was left cached after a re-link"
            );
        }
    }

    // ---------------------------------------------------------------------
    // Uniform dedup
    // ---------------------------------------------------------------------
    #[test]
    fn uniform_identical_bytes_dedup() {
        let mut s = fresh_state();
        let data = [0u8, 1, 2, 3];
        assert!(update_uniform(&mut s, 1, 5, &data));
        assert!(!update_uniform(&mut s, 1, 5, &data));
    }

    #[test]
    fn uniform_different_bytes_reissue() {
        let mut s = fresh_state();
        assert!(update_uniform(&mut s, 1, 5, &[1u8, 2]));
        assert!(update_uniform(&mut s, 1, 5, &[1u8, 3]));
    }

    #[test]
    fn changed_uniform_reuses_cached_value_allocation() {
        let mut s = fresh_state();
        assert!(update_uniform(&mut s, 1, 5, &[1u8, 2, 3, 4]));
        let first_allocation = s.uniform_cache.get(&(1, 5)).unwrap().as_ptr();

        assert!(update_uniform(&mut s, 1, 5, &[5u8, 6, 7, 8]));
        let second_allocation = s.uniform_cache.get(&(1, 5)).unwrap().as_ptr();

        assert_eq!(
            second_allocation, first_allocation,
            "same-sized changes should update the existing cache buffer"
        );
    }

    #[test]
    fn uniform_different_locations_tracked_independently() {
        let mut s = fresh_state();
        assert!(update_uniform(&mut s, 1, 5, &[1u8]));
        assert!(update_uniform(&mut s, 1, 6, &[1u8]));
        assert!(!update_uniform(&mut s, 1, 5, &[1u8]));
        assert!(!update_uniform(&mut s, 1, 6, &[1u8]));
    }

    #[test]
    fn uniform_different_programs_tracked_independently() {
        let mut s = fresh_state();
        assert!(update_uniform(&mut s, 1, 5, &[9u8]));
        assert!(update_uniform(&mut s, 2, 5, &[9u8]));
        assert!(!update_uniform(&mut s, 1, 5, &[9u8]));
    }

    #[test]
    fn uniform_cache_bounded_by_max_entries() {
        let mut s = fresh_state();
        for i in 0..(MAX_UNIFORM_CACHE * 2) {
            update_uniform(&mut s, 1, i as u32, &[0u8]);
        }
        assert!(s.uniform_cache.len() <= MAX_UNIFORM_CACHE);
    }

    // ---------------------------------------------------------------------
    // Buffers
    // ---------------------------------------------------------------------
    #[test]
    fn bind_buffer_array_dedup() {
        let mut s = fresh_state();
        assert!(update_bind_buffer(&mut s, 0x8892, Some(1)));
        assert!(!update_bind_buffer(&mut s, 0x8892, Some(1)));
        assert!(update_bind_buffer(&mut s, 0x8892, Some(2)));
    }

    #[test]
    fn bind_buffer_unbind_tracked() {
        let mut s = fresh_state();
        update_bind_buffer(&mut s, 0x8892, Some(1));
        assert!(update_bind_buffer(&mut s, 0x8892, None));
        assert!(!update_bind_buffer(&mut s, 0x8892, None));
    }

    #[test]
    fn bind_buffer_uniform_target_is_deduped() {
        // UNIFORM_BUFFER is a tracked WebGL 2 generic target: the first bind
        // reaches GL and an identical repeat is suppressed.
        let mut s = fresh_state();
        assert!(update_bind_buffer(&mut s, 0x8A11, Some(1)));
        assert!(!update_bind_buffer(&mut s, 0x8A11, Some(1)));
    }

    #[test]
    fn bind_vertex_array_forgets_buffer_bindings() {
        let mut s = fresh_state();
        update_bind_buffer(&mut s, 0x8892, Some(1));
        assert!(update_bind_vertex_array(&mut s, Some(5)));
        // After VAO change, ARRAY_BUFFER is unknown; next bind must issue.
        assert!(update_bind_buffer(&mut s, 0x8892, Some(1)));
    }

    // ---------------------------------------------------------------------
    // Textures
    // ---------------------------------------------------------------------
    #[test]
    fn active_texture_dedup() {
        let mut s = fresh_state();
        assert!(update_active_texture(&mut s, 0x84C0));
        assert!(!update_active_texture(&mut s, 0x84C0));
    }

    #[test]
    fn bind_texture_scoped_to_active_unit() {
        let mut s = fresh_state();
        update_active_texture(&mut s, 0x84C0);
        assert!(update_bind_texture_2d(&mut s, Some(1)));
        assert!(!update_bind_texture_2d(&mut s, Some(1)));
        // Switch unit; now binding 1 on a different unit must still issue.
        update_active_texture(&mut s, 0x84C1);
        assert!(update_bind_texture_2d(&mut s, Some(1)));
    }

    #[test]
    fn bind_texture_without_active_unit_pessimistic() {
        // If we never tracked an active unit, we can't safely dedup.
        let mut s = fresh_state();
        assert!(update_bind_texture_2d(&mut s, Some(1)));
        assert!(update_bind_texture_2d(&mut s, Some(1)));
    }

    // ---------------------------------------------------------------------
    // Blend / depth / cull / …
    // ---------------------------------------------------------------------
    #[test]
    fn blend_func_dedup() {
        let mut s = fresh_state();
        assert!(update_blend_func(&mut s, 1, 2));
        assert!(!update_blend_func(&mut s, 1, 2));
        assert!(update_blend_func(&mut s, 1, 3));
    }

    #[test]
    fn blend_func_separate_differs_from_plain_blend_func() {
        let mut s = fresh_state();
        update_blend_func(&mut s, 1, 2);
        // separate(1,2,1,2) matches plain blend_func; separate(1,2,3,4) doesn't.
        assert!(!update_blend_func_separate(&mut s, 1, 2, 1, 2));
        assert!(update_blend_func_separate(&mut s, 1, 2, 3, 4));
    }

    #[test]
    fn blend_equation_dedup() {
        let mut s = fresh_state();
        assert!(update_blend_equation(&mut s, 0x8006));
        assert!(!update_blend_equation(&mut s, 0x8006));
    }

    #[test]
    fn depth_func_dedup() {
        let mut s = fresh_state();
        assert!(update_depth_func(&mut s, 0x0203));
        assert!(!update_depth_func(&mut s, 0x0203));
        assert!(update_depth_func(&mut s, 0x0201));
    }

    #[test]
    fn depth_mask_boolean_round_trip() {
        let mut s = fresh_state();
        assert!(update_depth_mask(&mut s, true));
        assert!(!update_depth_mask(&mut s, true));
        assert!(update_depth_mask(&mut s, false));
    }

    #[test]
    fn cull_and_front_face_tracked_separately() {
        let mut s = fresh_state();
        assert!(update_cull_face(&mut s, 0x0405));
        assert!(!update_cull_face(&mut s, 0x0405));
        // front_face change must not affect cull_face cache.
        assert!(update_front_face(&mut s, 0x0900));
        assert!(!update_cull_face(&mut s, 0x0405));
    }

    // ---------------------------------------------------------------------
    // Enable / disable
    // ---------------------------------------------------------------------
    #[test]
    fn enable_then_enable_deduped() {
        let mut s = fresh_state();
        assert!(update_enable(&mut s, 0x0B71));
        assert!(!update_enable(&mut s, 0x0B71));
    }

    #[test]
    fn disable_after_enable_emits_once() {
        let mut s = fresh_state();
        update_enable(&mut s, 0x0B71);
        assert!(update_disable(&mut s, 0x0B71));
        assert!(!update_disable(&mut s, 0x0B71));
    }

    #[test]
    fn re_enable_after_disable_emits() {
        let mut s = fresh_state();
        update_enable(&mut s, 0x0B71);
        update_disable(&mut s, 0x0B71);
        assert!(update_enable(&mut s, 0x0B71));
    }

    // ---------------------------------------------------------------------
    // Cocos-shaped workload: simulate N draw calls that only differ in one
    // uniform, asserting the dedup ratio is >= 90%.  This is the "call
    // count" TDD guarantee promised by the Phase 8 plan.
    // ---------------------------------------------------------------------
    #[test]
    fn cocos_like_workload_deduplicates_most_calls() {
        let mut s = fresh_state();
        let mut issued = 0u32;

        // Typical Cocos 2.x inner loop over 100 sprites sharing one program:
        //   useProgram, bindBuffer(VBO), bindBuffer(EBO),
        //   bindTexture, activeTexture, uniform4f(u_color), drawElements.
        for i in 0..100 {
            if update_use_program(&mut s, 1) {
                issued += 1;
            }
            if update_bind_buffer(&mut s, 0x8892, Some(10)) {
                issued += 1;
            }
            if update_bind_buffer(&mut s, 0x8893, Some(11)) {
                issued += 1;
            }
            if update_active_texture(&mut s, 0x84C0) {
                issued += 1;
            }
            if update_bind_texture_2d(&mut s, Some(42)) {
                issued += 1;
            }
            // One uniform changes per iteration (e.g. the sprite's tint).
            let tint = [i as u8, 0, 0, 255];
            if update_uniform(&mut s, 1, 5, &tint) {
                issued += 1;
            }
        }

        // Of 600 possible redundant calls, only the very first iteration's
        // five state-setup calls are "real", plus 100 uniform uploads → 105.
        assert_eq!(
            issued, 105,
            "expected 105 real GL calls for the 600 logical calls, \
             got {issued} (dedup is broken)"
        );
    }

    // ---------------------------------------------------------------------
    // The `state_changes` diagnostic
    //
    // It was plumbed end to end and incremented from nowhere, so it read zero
    // on every frame of every build — on a path where state calls outnumber
    // draws by an order of magnitude. These pin that it now equals the number
    // of driver state calls the tracker admits, which is what makes
    // `state_changes / draw_calls` readable and any question about dedup
    // effectiveness or draw-call batching answerable.
    //
    // Serialised on one lock: the diagnostics sink is a process-wide
    // thread-local and `install` replaces it, so two of these running
    // concurrently in the same test binary would each see the other's bumps.
    // That is the mistake a previous round made with a shared executor, and it
    // reads as a flaky count rather than as a shared-state bug.
    // ---------------------------------------------------------------------

    static DIAGNOSTICS_SINK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Run `body` against a fresh diagnostics sink and return the
    /// `state_changes` it published.
    fn state_changes_during(body: impl FnOnce()) -> u32 {
        use std::sync::atomic::Ordering;
        let _serialise = DIAGNOSTICS_SINK.lock().unwrap_or_else(|e| e.into_inner());
        crate::render_diagnostics::uninstall_for_tests();
        let stats = std::sync::Arc::new(shared::stats::DebugStats::default());
        crate::render_diagnostics::install(stats.clone());
        body();
        crate::render_diagnostics::flush_frame();
        let observed = stats.state_changes.load(Ordering::Relaxed);
        crate::render_diagnostics::uninstall_for_tests();
        observed
    }

    /// The counter equals the calls that reach the driver, not the calls the
    /// content made. Deduped calls must not appear or the number says nothing
    /// about what the driver saw.
    #[test]
    fn the_state_change_counter_counts_driver_calls_not_content_calls() {
        let observed = state_changes_during(|| {
            let mut s = fresh_state();
            // Six content calls; three distinct states. `update_viewport`,
            // `update_depth_func` and `update_cull_face` each issue once and
            // dedup once.
            for _ in 0..2 {
                update_viewport(&mut s, 0, 0, 1080, 1920);
                update_depth_func(&mut s, glow::LESS);
                update_cull_face(&mut s, glow::BACK);
            }
        });

        assert_eq!(
            observed, 3,
            "expected 3 driver calls for 6 content calls; a counter that reads \
             6 is counting content calls, and one that reads 0 is not wired up"
        );
    }

    /// A fully-deduped frame must report zero — that is the claim the dedup
    /// layer exists to make, and the counter is how anyone sees it.
    #[test]
    fn a_fully_deduped_frame_reports_no_state_changes() {
        let mut s = fresh_state();
        // Establish every shadow outside the measurement.
        update_viewport(&mut s, 0, 0, 1080, 1920);
        update_use_program(&mut s, 1);
        update_enable(&mut s, glow::BLEND);
        update_bind_buffer(&mut s, glow::ARRAY_BUFFER, Some(7));

        let observed = state_changes_during(|| {
            for _ in 0..50 {
                update_viewport(&mut s, 0, 0, 1080, 1920);
                update_use_program(&mut s, 1);
                update_bind_buffer(&mut s, glow::ARRAY_BUFFER, Some(7));
            }
        });

        assert_eq!(
            observed, 0,
            "a frame that changed nothing reported {observed} driver state calls"
        );
    }

    /// `glEnable(STENCIL_TEST)` is deliberately never deduped — Skia toggles it
    /// outside this tracker. So it issues every time, and the counter has to say
    /// so rather than report the shadow's opinion.
    #[test]
    fn the_never_deduped_capability_is_counted_every_time() {
        let observed = state_changes_during(|| {
            let mut s = fresh_state();
            for _ in 0..4 {
                update_enable(&mut s, glow::STENCIL_TEST);
            }
        });

        assert_eq!(
            observed, 4,
            "STENCIL_TEST reaches the driver on every call, so all four must be \
             counted"
        );
    }

    /// The convenience forms delegate to their `_separate` counterparts, which
    /// already count. One content call must not report two driver calls.
    #[test]
    fn a_delegating_setter_is_counted_once_not_twice() {
        let observed = state_changes_during(|| {
            let mut s = fresh_state();
            update_blend_func(&mut s, glow::SRC_ALPHA, glow::ONE_MINUS_SRC_ALPHA);
            update_blend_equation(&mut s, glow::FUNC_ADD);
        });

        assert_eq!(
            observed, 2,
            "two content calls reported {observed} driver calls — the delegating \
             form is counting alongside the one it delegates to"
        );
    }

    // ---------------------------------------------------------------------
    // The per-VAO vertex-attribute shadow
    //
    // The ten tests above pin the dedup semantics and passed unchanged across
    // the move from three `(vao, index)`-keyed hash containers to one record
    // per VAO — which is what makes that move behaviour-preserving. What they
    // do not cover is the shadow's own shape: how far it tracks, how much it
    // allocates, and what deleting a VAO drops. That is here.
    // ---------------------------------------------------------------------

    /// **The property that makes this layout free rather than a trade.** Slots
    /// are grown to the highest index the content touches, so a geometry using
    /// three attributes costs three slots, not a fixed sixteen or thirty-two.
    /// A fixed array would be marginally faster and cost +171% shadow bytes at
    /// this attribute count.
    #[test]
    fn the_attribute_slots_are_sized_to_what_the_content_touches() {
        let mut s = fresh_state();
        for index in 0..3u32 {
            assert!(update_enable_vertex_attrib(&mut s, index));
            assert!(update_vertex_attrib_pointer(
                &mut s,
                index,
                3,
                glow::FLOAT,
                false,
                32,
                0
            ));
        }

        assert_eq!(
            s.vertex_attribs.tracked_slots(0),
            3,
            "three attributes in use should cost three slots"
        );

        // Reaching a higher index grows to exactly that index, not to the cap.
        assert!(update_vertex_attrib_pointer(
            &mut s,
            5,
            3,
            glow::FLOAT,
            false,
            32,
            0
        ));
        assert_eq!(s.vertex_attribs.tracked_slots(0), 6);
    }

    /// One record per VAO the content actually binds — the outer table grows the
    /// same way the slots do.
    #[test]
    fn only_the_vaos_the_content_binds_are_tracked() {
        let mut s = fresh_state();
        assert_eq!(s.vertex_attribs.tracked_vaos(), 0);
        for vao in [0u32, 4, 9] {
            update_bind_vertex_array(&mut s, Some(vao));
            assert!(update_enable_vertex_attrib(&mut s, 0));
        }
        assert_eq!(s.vertex_attribs.tracked_vaos(), 3);
        // Re-binding a VAO already seen does not add another record.
        update_bind_vertex_array(&mut s, Some(4));
        assert!(!update_enable_vertex_attrib(&mut s, 0));
        assert_eq!(s.vertex_attribs.tracked_vaos(), 3);
    }

    /// An attribute index past what any GLES 3 device exposes is forwarded
    /// rather than tracked — that is also what stops a content-chosen index
    /// from sizing the slot vector.
    #[test]
    fn an_attribute_index_past_the_tracked_range_is_forwarded_every_time() {
        let mut s = fresh_state();
        const PAST_END: u32 = 32;

        assert!(update_enable_vertex_attrib(&mut s, PAST_END));
        assert!(
            update_enable_vertex_attrib(&mut s, PAST_END),
            "an untracked index must forward rather than dedup"
        );
        assert!(update_vertex_attrib_pointer(
            &mut s,
            PAST_END,
            3,
            glow::FLOAT,
            false,
            32,
            0
        ));
        assert!(update_vertex_attrib_pointer(
            &mut s,
            PAST_END,
            3,
            glow::FLOAT,
            false,
            32,
            0
        ));
        assert!(update_vertex_attrib_divisor(&mut s, PAST_END, 1));
        assert!(update_vertex_attrib_divisor(&mut s, PAST_END, 1));
        assert!(
            update_disable_vertex_attrib(&mut s, PAST_END),
            "disable of an untracked index must forward too"
        );
        assert_eq!(
            s.vertex_attribs.tracked_slots(0),
            0,
            "an out-of-range index sized the slot vector"
        );
    }

    #[test]
    fn the_last_tracked_attribute_index_still_dedups() {
        let mut s = fresh_state();
        assert!(update_enable_vertex_attrib(&mut s, 31));
        assert!(!update_enable_vertex_attrib(&mut s, 31));
        assert!(update_vertex_attrib_pointer(
            &mut s,
            31,
            3,
            glow::FLOAT,
            false,
            32,
            0
        ));
        assert!(!update_vertex_attrib_pointer(
            &mut s,
            31,
            3,
            glow::FLOAT,
            false,
            32,
            0
        ));
    }

    /// Deleting a VAO must drop its shadow and nothing else. VAO names come
    /// from the client, so a reused name that inherited the dead object's
    /// layout would dedup away the `vertexAttribPointer` the new one needs, and
    /// the draw would read the wrong vertex stream.
    #[test]
    fn deleting_a_vao_drops_its_shadow_and_leaves_the_others() {
        let mut s = fresh_state();
        s.bound_array_buffer = Some(Some(7));
        for vao in [1u32, 2] {
            update_bind_vertex_array(&mut s, Some(vao));
            s.bound_array_buffer = Some(Some(7));
            assert!(update_enable_vertex_attrib(&mut s, 0));
            assert!(update_vertex_attrib_pointer(
                &mut s,
                0,
                3,
                glow::FLOAT,
                false,
                32,
                0
            ));
        }

        s.vertex_attribs.forget_vao(1);

        update_bind_vertex_array(&mut s, Some(1));
        s.bound_array_buffer = Some(Some(7));
        assert!(
            update_enable_vertex_attrib(&mut s, 0),
            "the deleted VAO's enable shadow survived, so a reused name draws \
             with a disabled attribute"
        );
        assert!(
            update_vertex_attrib_pointer(&mut s, 0, 3, glow::FLOAT, false, 32, 0),
            "the deleted VAO's layout shadow survived, so a reused name draws \
             from whatever stream the dead object pointed at"
        );

        update_bind_vertex_array(&mut s, Some(2));
        s.bound_array_buffer = Some(Some(7));
        assert!(
            !update_enable_vertex_attrib(&mut s, 0),
            "an unrelated VAO's shadow was dropped by the delete, costing it a \
             redundant re-enable"
        );
        assert!(!update_vertex_attrib_pointer(
            &mut s,
            0,
            3,
            glow::FLOAT,
            false,
            32,
            0
        ));
    }

    /// The Skia boundary forgets the state but keeps the buffers.
    ///
    /// **Asserted with the allocation probe rather than by counting records**,
    /// because counting proved nothing: a first draft checked
    /// `tracked_vaos() == 1` *after* the re-issues that follow the boundary, and
    /// those recreate the record — so a `forget_all` that dropped every
    /// allocation satisfied it too. The claim is about the heap, so the
    /// instrument has to be the heap.
    ///
    /// One iteration is one frame of a game that crosses the boundary once per
    /// frame, which is what a Canvas2D overlay on a WebGL scene does. A
    /// `forget_all` that dropped the records would re-buy one slot vector per
    /// VAO here, forever, on the render thread.
    #[test]
    fn crossing_the_skia_boundary_every_frame_never_reaches_the_heap() {
        let mut s = fresh_state();

        migo_alloc_probe::assert_no_steady_state_allocation(
            migo_alloc_probe::Burst {
                path: "state_tracker: vertex-attribute shadow across a per-frame Skia boundary",
                warmup: 4,
                measured: 64,
            },
            |_| {
                let mut issued = 0u32;
                for vao in 0..4u32 {
                    update_bind_vertex_array(&mut s, Some(vao));
                    s.bound_array_buffer = Some(Some(10 + vao));
                    for index in 0..5u32 {
                        if update_enable_vertex_attrib(&mut s, index) {
                            issued += 1;
                        }
                        if update_vertex_attrib_pointer(
                            &mut s,
                            index,
                            3,
                            glow::FLOAT,
                            false,
                            32,
                            (index * 12) as i32,
                        ) {
                            issued += 1;
                        }
                    }
                }
                // The boundary: Skia ran, so everything the shadow claimed is
                // now unknown.
                s.invalidate_after_external_gl_use();
                issued
            },
        );
    }

    /// The other half of the boundary contract: after it, every setter must
    /// re-issue. A `forget_all` that kept the allocations *and* the state would
    /// pass the burst above while painting with whatever Skia left bound.
    #[test]
    fn the_skia_boundary_makes_every_attribute_setter_reissue() {
        let mut s = fresh_state();
        s.bound_array_buffer = Some(Some(7));
        assert!(update_enable_vertex_attrib(&mut s, 2));
        assert!(update_vertex_attrib_pointer(
            &mut s,
            2,
            3,
            glow::FLOAT,
            false,
            32,
            0
        ));
        assert!(update_vertex_attrib_divisor(&mut s, 2, 1));

        s.invalidate_after_external_gl_use();
        s.bound_array_buffer = Some(Some(7));

        assert!(update_enable_vertex_attrib(&mut s, 2));
        assert!(update_vertex_attrib_pointer(
            &mut s,
            2,
            3,
            glow::FLOAT,
            false,
            32,
            0
        ));
        assert!(update_vertex_attrib_divisor(&mut s, 2, 1));
    }

    /// Section 7.3's steady-state requirement on the attribute path: a settled
    /// frame re-asserting the same layout must not reach the heap. The slot
    /// vectors are bought once, when the content first touches each index.
    #[test]
    fn a_steady_state_vertex_attribute_frame_never_reaches_the_heap() {
        let mut s = fresh_state();

        migo_alloc_probe::assert_no_steady_state_allocation(
            migo_alloc_probe::Burst {
                path: "state_tracker: per-command vertex-attribute dedup across four VAOs",
                warmup: 4,
                measured: 64,
            },
            |_| {
                let mut issued = 0u32;
                for vao in 0..4u32 {
                    update_bind_vertex_array(&mut s, Some(vao));
                    s.bound_array_buffer = Some(Some(10 + vao));
                    for index in 0..5u32 {
                        if update_enable_vertex_attrib(&mut s, index) {
                            issued += 1;
                        }
                        if update_vertex_attrib_pointer(
                            &mut s,
                            index,
                            3,
                            glow::FLOAT,
                            false,
                            32,
                            (index * 12) as i32,
                        ) {
                            issued += 1;
                        }
                        if update_vertex_attrib_divisor(&mut s, index, 0) {
                            issued += 1;
                        }
                    }
                }
                issued
            },
        );
    }

    // ---------------------------------------------------------------------
    // Boundaries of the right-sized shadows
    //
    // The hash maps these replaced accepted any `u32` as a key, so they would
    // happily dedup a GL enum that is not a texture unit, a cull face, a
    // framebuffer target, or a toggleable capability. The array- and
    // bitmask-backed shadows recognise only the values the spec fixes, and
    // forward everything else. That is the safe direction — an unrecognised
    // enum is a `GL_INVALID_ENUM` the driver rejects, so the only cost is that
    // an already-failing call is not deduped — but it *is* a change, so it is
    // written down here.
    // ---------------------------------------------------------------------

    #[test]
    fn a_texture_unit_past_the_tracked_range_is_forwarded_every_time() {
        let mut s = fresh_state();
        // TEXTURE0 + 32: beyond the 32 units GLES 3.0 guarantees.
        update_active_texture(&mut s, glow::TEXTURE0 + 32);
        assert!(update_bind_texture_2d(&mut s, Some(1)));
        assert!(
            update_bind_texture_2d(&mut s, Some(1)),
            "an untracked unit must forward rather than dedup against a slot \
             that does not exist"
        );
    }

    #[test]
    fn the_last_tracked_texture_unit_still_dedups() {
        let mut s = fresh_state();
        update_active_texture(&mut s, glow::TEXTURE0 + 31);
        assert!(update_bind_texture_2d(&mut s, Some(1)));
        assert!(!update_bind_texture_2d(&mut s, Some(1)));
    }

    /// `bindTexture(target, 0)` reaches the tracker as `Some(0)`, which is a
    /// different request from `None`. Collapsing them would make the shadow
    /// answer one with the other.
    #[test]
    fn binding_texture_zero_is_distinct_from_unbinding() {
        let mut s = fresh_state();
        update_active_texture(&mut s, glow::TEXTURE0);
        assert!(update_bind_texture_2d(&mut s, Some(0)));
        assert!(!update_bind_texture_2d(&mut s, Some(0)));
        assert!(
            update_bind_texture_2d(&mut s, None),
            "None was deduped against Some(0)"
        );
    }

    #[test]
    fn an_unknown_capability_is_forwarded_every_time() {
        let mut s = fresh_state();
        // Not a toggleable WebGL capability — the driver answers INVALID_ENUM.
        const NOT_A_CAP: u32 = 0x1234;
        assert!(update_enable(&mut s, NOT_A_CAP));
        assert!(update_enable(&mut s, NOT_A_CAP));
        assert!(update_disable(&mut s, NOT_A_CAP));
    }

    /// Every capability WebGL exposes has to be tracked, or a real toggle goes
    /// undeduped. Enumerated rather than spot-checked so adding a WebGL 2 cap
    /// to the enum list without adding it to the shadow shows up here.
    #[test]
    fn every_toggleable_webgl_capability_is_tracked() {
        for cap in [
            glow::BLEND,
            glow::CULL_FACE,
            glow::DEPTH_TEST,
            glow::DITHER,
            glow::POLYGON_OFFSET_FILL,
            glow::SAMPLE_ALPHA_TO_COVERAGE,
            glow::SAMPLE_COVERAGE,
            glow::SCISSOR_TEST,
            glow::RASTERIZER_DISCARD,
        ] {
            let mut s = fresh_state();
            assert!(update_enable(&mut s, cap), "cap {cap:#x} first enable");
            assert!(
                !update_enable(&mut s, cap),
                "cap {cap:#x} is not being tracked, so every redundant enable \
                 reaches the driver"
            );
            assert!(update_disable(&mut s, cap), "cap {cap:#x} first disable");
            assert!(
                !update_disable(&mut s, cap),
                "cap {cap:#x} redundant disable"
            );
        }
    }

    /// STENCIL_TEST is the documented exception: Skia toggles it behind the
    /// tracker's back, so it must never be deduped. Kept as its own test
    /// because the bitmask rewrite moved where that decision is made.
    #[test]
    fn stencil_test_is_never_deduped() {
        let mut s = fresh_state();
        assert!(update_enable(&mut s, glow::STENCIL_TEST));
        assert!(
            update_enable(&mut s, glow::STENCIL_TEST),
            "a redundant glEnable(STENCIL_TEST) was deduped — Skia toggles this \
             outside the tracker, so the shadow cannot be trusted for it"
        );
        assert!(update_disable(&mut s, glow::STENCIL_TEST));
        assert!(update_disable(&mut s, glow::STENCIL_TEST));
    }

    #[test]
    fn an_unknown_framebuffer_target_is_forwarded_every_time() {
        let mut s = fresh_state();
        const NOT_A_TARGET: u32 = 0x4321;
        assert!(update_bind_framebuffer(&mut s, NOT_A_TARGET, Some(3)));
        assert!(update_bind_framebuffer(&mut s, NOT_A_TARGET, Some(3)));
    }

    /// Every `pixelStorei` parameter WebGL exposes has to be tracked, or a real
    /// upload knob goes undeduped. Enumerated rather than spot-checked, so
    /// adding one to the spec list without adding it to the shadow shows up.
    #[test]
    fn every_webgl_pixel_store_parameter_is_tracked() {
        // The two WebGL-only pnames have no `glow` constant.
        const UNPACK_FLIP_Y_WEBGL: u32 = 0x9240;
        const UNPACK_PREMULTIPLY_ALPHA_WEBGL: u32 = 0x9241;
        const UNPACK_COLORSPACE_CONVERSION_WEBGL: u32 = 0x9243;
        for pname in [
            glow::PACK_ALIGNMENT,
            glow::UNPACK_ALIGNMENT,
            UNPACK_FLIP_Y_WEBGL,
            UNPACK_PREMULTIPLY_ALPHA_WEBGL,
            UNPACK_COLORSPACE_CONVERSION_WEBGL,
            glow::UNPACK_ROW_LENGTH,
            glow::UNPACK_SKIP_PIXELS,
            glow::UNPACK_SKIP_ROWS,
            glow::UNPACK_IMAGE_HEIGHT,
            glow::UNPACK_SKIP_IMAGES,
            glow::PACK_ROW_LENGTH,
            glow::PACK_SKIP_PIXELS,
            glow::PACK_SKIP_ROWS,
        ] {
            let mut s = fresh_state();
            assert!(
                update_pixel_store_i32(&mut s, pname, 4),
                "pname {pname:#x} first set"
            );
            assert!(
                !update_pixel_store_i32(&mut s, pname, 4),
                "pname {pname:#x} is not being tracked, so every redundant \
                 pixelStorei reaches the driver"
            );
            assert!(
                update_pixel_store_i32(&mut s, pname, 1),
                "pname {pname:#x} value change"
            );
        }
    }

    /// Zero is a legal `param` *and* the array's initial content, so the shadow
    /// needs an observed bit rather than a value comparison alone. Without it,
    /// the first `pixelStorei(pname, 0)` a game issues to undo an earlier
    /// setting would be deduped and the driver would keep the old value.
    #[test]
    fn a_first_pixel_store_of_zero_still_reaches_the_driver() {
        let mut s = fresh_state();
        assert!(
            update_pixel_store_i32(&mut s, glow::UNPACK_ALIGNMENT, 0),
            "an unobserved pname was deduped against the array's zero initialiser"
        );
        assert!(!update_pixel_store_i32(&mut s, glow::UNPACK_ALIGNMENT, 0));
    }

    #[test]
    fn an_unknown_pixel_store_parameter_is_forwarded_every_time() {
        let mut s = fresh_state();
        const NOT_A_PNAME: u32 = 0x4323;
        assert!(update_pixel_store_i32(&mut s, NOT_A_PNAME, 1));
        assert!(update_pixel_store_i32(&mut s, NOT_A_PNAME, 1));
    }

    /// Indexed UBO bindings: each index is its own slot, and a
    /// `bindBufferRange` window must never coalesce with the
    /// `bindBufferBase` that records `(buffer, 0, 0)`.
    #[test]
    fn indexed_uniform_buffer_bindings_are_tracked_per_index_and_window() {
        const GL_UNIFORM_BUFFER: u32 = 0x8A11;
        let mut s = fresh_state();

        assert!(update_bind_buffer_base(
            &mut s,
            GL_UNIFORM_BUFFER,
            0,
            Some(7)
        ));
        assert!(!update_bind_buffer_base(
            &mut s,
            GL_UNIFORM_BUFFER,
            0,
            Some(7)
        ));
        // A different index is a different slot.
        assert!(update_bind_buffer_base(
            &mut s,
            GL_UNIFORM_BUFFER,
            5,
            Some(7)
        ));
        // Same buffer and index, but a window rather than the whole buffer.
        assert!(
            update_bind_buffer_range(&mut s, GL_UNIFORM_BUFFER, 0, Some(7), 256, 64),
            "a range binding was deduped against a base binding of the same buffer"
        );
        assert!(!update_bind_buffer_range(
            &mut s,
            GL_UNIFORM_BUFFER,
            0,
            Some(7),
            256,
            64
        ));
        // And back to the whole buffer must re-issue.
        assert!(update_bind_buffer_base(
            &mut s,
            GL_UNIFORM_BUFFER,
            0,
            Some(7)
        ));
    }

    #[test]
    fn the_last_tracked_uniform_buffer_binding_dedups_and_past_it_forwards() {
        const GL_UNIFORM_BUFFER: u32 = 0x8A11;
        let mut s = fresh_state();
        assert!(update_bind_buffer_base(
            &mut s,
            GL_UNIFORM_BUFFER,
            31,
            Some(1)
        ));
        assert!(!update_bind_buffer_base(
            &mut s,
            GL_UNIFORM_BUFFER,
            31,
            Some(1)
        ));
        // Past the tracked range: forwarded, never deduped.
        assert!(update_bind_buffer_base(
            &mut s,
            GL_UNIFORM_BUFFER,
            32,
            Some(1)
        ));
        assert!(update_bind_buffer_base(
            &mut s,
            GL_UNIFORM_BUFFER,
            32,
            Some(1)
        ));
    }

    #[test]
    fn an_unknown_stencil_face_is_forwarded_every_time() {
        let mut s = fresh_state();
        const NOT_A_FACE: u32 = 0x4322;
        assert!(update_stencil_mask(&mut s, NOT_A_FACE, 0xFF));
        assert!(update_stencil_mask(&mut s, NOT_A_FACE, 0xFF));
    }

    /// Deleting a texture unbinds it from every unit (GLES 3.0 §3.8.14), and
    /// the sweep must reach every unit that named it while leaving the others
    /// deduped — a blanket reset would cost a rebind on every unit.
    #[test]
    fn forgetting_a_deleted_texture_touches_only_the_units_that_named_it() {
        let mut s = fresh_state();
        update_active_texture(&mut s, glow::TEXTURE0);
        assert!(update_bind_texture_2d(&mut s, Some(7)));
        update_active_texture(&mut s, glow::TEXTURE1);
        assert!(update_bind_texture_2d(&mut s, Some(9)));
        update_active_texture(&mut s, glow::TEXTURE2);
        assert!(update_bind_texture_2d(&mut s, Some(7)));

        s.bound_texture_2d.forget_texture(7);

        update_active_texture(&mut s, glow::TEXTURE0);
        assert!(
            update_bind_texture_2d(&mut s, Some(7)),
            "unit 0 still claimed the deleted texture"
        );
        update_active_texture(&mut s, glow::TEXTURE2);
        assert!(
            update_bind_texture_2d(&mut s, Some(7)),
            "unit 2 still claimed the deleted texture"
        );
        update_active_texture(&mut s, glow::TEXTURE1);
        assert!(
            !update_bind_texture_2d(&mut s, Some(9)),
            "unit 1 named a different texture and was reset anyway, costing a \
             redundant rebind"
        );
    }

    // ---------------------------------------------------------------------
    // Multi-program workload: the shape uniform dedup exists for, and the
    // shape it did not survive.
    //
    // Attaching invalidation to `glUseProgram` did not merely weaken the
    // dedup on this shape, it removed it: a switch retained only the incoming
    // program's entries, so cycling A→B→C→A emptied the table before every
    // round and *every* unchanged uniform re-issued. Measured on a fixture
    // this size: 3600 of 3600 logical uploads reached the driver.
    //
    // Asserting the issued count rather than "it dedups somewhere" is what
    // makes this bite — the per-call dedup tests above all passed while the
    // table was being emptied between them.
    // ---------------------------------------------------------------------

    /// Sets `uniforms_per_draw` unchanged uniforms under each of
    /// `programs` programs, `rounds` times, cycling programs every round.
    /// Returns `(logical calls, calls the tracker would issue)`.
    ///
    /// Takes the state by reference so a caller can measure a *steady-state*
    /// frame. A frame's shadow outlives the frame; building a fresh one per
    /// iteration measures the cold-start cost of eleven empty hash tables
    /// instead of the per-command path.
    fn run_program_cycling_frame(
        s: &mut CanvasGLState,
        programs: ProgramId,
        rounds: usize,
        uniforms_per_draw: u32,
    ) -> (u32, u32) {
        let payload = [7u8; 64];
        let mut issued = 0u32;
        let mut logical = 0u32;
        for _ in 0..rounds {
            for p in 1..=programs {
                update_use_program(s, p);
                for loc in 0..uniforms_per_draw {
                    logical += 1;
                    if update_uniform(s, p, loc, &payload) {
                        issued += 1;
                    }
                }
            }
        }
        (logical, issued)
    }

    fn program_cycling_frame(
        programs: ProgramId,
        rounds: usize,
        uniforms_per_draw: u32,
    ) -> (u32, u32) {
        let mut s = fresh_state();
        run_program_cycling_frame(&mut s, programs, rounds, uniforms_per_draw)
    }

    #[test]
    fn a_frame_cycling_three_programs_uploads_each_unchanged_uniform_once() {
        const PROGRAMS: ProgramId = 3;
        const ROUNDS: usize = 100;
        const PER_DRAW: u32 = 12;

        let (logical, issued) = program_cycling_frame(PROGRAMS, ROUNDS, PER_DRAW);

        assert_eq!(logical, ROUNDS as u32 * PROGRAMS * PER_DRAW);
        // Only the first round establishes the shadow; rounds 2..N are free.
        assert_eq!(
            issued,
            PROGRAMS * PER_DRAW,
            "expected {} real uploads ({logical} logical), got {issued} — the \
             uniform shadow is being discarded between program switches, which \
             costs one driver call per uniform per draw for every scene with \
             more than one material",
            PROGRAMS * PER_DRAW
        );
    }

    /// The dedup must not depend on how many programs the frame cycles
    /// through — a scene gets more materials, not fewer.
    #[test]
    fn the_multi_program_dedup_holds_as_the_material_count_grows() {
        for programs in [1u32, 2, 3, 8, 16] {
            let (_, issued) = program_cycling_frame(programs, 20, 8);
            assert_eq!(
                issued,
                programs * 8,
                "{programs} programs: expected {} uploads, got {issued}",
                programs * 8
            );
        }
    }

    /// Section 7.3's steady-state requirement, on the per-command dedup path.
    ///
    /// **This is the assertion the defect could not have passed.** Attaching
    /// uniform invalidation to `glUseProgram` dropped the outgoing program's
    /// entries on every switch, and each dropped entry is a `Vec<u8>` release
    /// plus a fresh allocation the next time that uniform is uploaded — so a
    /// frame that cycles three programs churned the heap in proportion to its
    /// draw calls, on the thread running the game. With invalidation moved to
    /// re-link, a settled frame touches the heap zero times: every upload is a
    /// `get_mut` and a byte compare.
    ///
    /// One iteration is one frame. The warmup covers the first round, which
    /// legitimately allocates — that is where each `(program, location)` gets
    /// its cache entry.
    #[test]
    fn a_steady_state_program_cycling_frame_never_reaches_the_heap() {
        const PROGRAMS: ProgramId = 3;
        const ROUNDS: usize = 20;
        const PER_DRAW: u32 = 12;

        let mut s = fresh_state();

        migo_alloc_probe::assert_no_steady_state_allocation(
            migo_alloc_probe::Burst {
                path: "state_tracker: per-command dedup for a frame cycling three programs",
                warmup: 4,
                measured: 64,
            },
            |_| {
                let (logical, issued) =
                    run_program_cycling_frame(&mut s, PROGRAMS, ROUNDS, PER_DRAW);
                debug_assert_eq!(logical, ROUNDS as u32 * PROGRAMS * PER_DRAW);
                issued
            },
        );
    }

    /// The same requirement for the state families whose containers were
    /// right-sized. A settled frame of binds and toggles must not reach the
    /// heap either — the hash maps these replaced could rehash mid-frame the
    /// first time a scene touched a new texture unit or capability.
    #[test]
    fn a_steady_state_bind_and_toggle_frame_never_reaches_the_heap() {
        let mut s = fresh_state();

        migo_alloc_probe::assert_no_steady_state_allocation(
            migo_alloc_probe::Burst {
                path: "state_tracker: per-command dedup for binds, toggles and stencil state",
                warmup: 4,
                measured: 64,
            },
            |iteration| {
                let mut issued = 0u32;
                for batch in 0..64u32 {
                    if update_active_texture(&mut s, glow::TEXTURE0 + (batch & 3)) {
                        issued += 1;
                    }
                    if update_bind_texture_2d(&mut s, Some(40 + (batch & 7))) {
                        issued += 1;
                    }
                    if update_enable(&mut s, glow::BLEND) {
                        issued += 1;
                    }
                    if update_disable(&mut s, glow::CULL_FACE) {
                        issued += 1;
                    }
                    if update_bind_framebuffer(&mut s, glow::FRAMEBUFFER, None) {
                        issued += 1;
                    }
                    if update_stencil_func(
                        &mut s,
                        glow::FRONT_AND_BACK,
                        glow::EQUAL,
                        (iteration & 3) as i32,
                        0xFF,
                    ) {
                        issued += 1;
                    }
                    if update_viewport(&mut s, 0, 0, 1080, 1920) {
                        issued += 1;
                    }
                }
                issued
            },
        );
    }

    /// Steady-state cost of the per-command shadow bookkeeping for the frame
    /// above, driver calls excluded. Direction-only assertion so the number is
    /// informative without being machine-dependent.
    ///
    /// Run with:
    /// `cargo test --release -p migo-graphics --lib bench_ -- --ignored --nocapture`
    #[test]
    #[ignore = "timing benchmark; run explicitly with --ignored"]
    fn bench_program_cycling_frame_shadow_cost() {
        const PROGRAMS: ProgramId = 3;
        const ROUNDS: usize = 100;
        const PER_DRAW: u32 = 12;
        const ITERS: usize = 2_000;

        // One long-lived shadow, as a running game has: frame N+1 sees the
        // table frame N left behind.
        let mut s = fresh_state();
        for _ in 0..50 {
            std::hint::black_box(run_program_cycling_frame(
                &mut s, PROGRAMS, ROUNDS, PER_DRAW,
            ));
        }

        let start = std::time::Instant::now();
        for _ in 0..ITERS {
            std::hint::black_box(run_program_cycling_frame(
                &mut s, PROGRAMS, ROUNDS, PER_DRAW,
            ));
        }
        let per_frame = start.elapsed().as_nanos() as f64 / ITERS as f64;

        let logical = ROUNDS as f64 * PROGRAMS as f64 * PER_DRAW as f64;
        println!(
            "steady-state program-cycling frame ({PROGRAMS} programs x {ROUNDS} \
             rounds x {PER_DRAW} uniforms = {logical:.0} logical uploads, all \
             deduped):\n  {per_frame:.0} ns/frame of shadow bookkeeping \
             ({:.2} ns per logical upload, {:.3}% of a 16.67 ms frame)",
            per_frame / logical,
            per_frame / 16_666_000.0 * 100.0
        );

        // A frame that only maintains a shadow must stay far under the frame
        // budget; this catches an accidental O(n) walk per call, which is the
        // defect that was here.
        assert!(
            per_frame < 16_666_000.0 / 10.0,
            "shadow bookkeeping alone took {per_frame:.0} ns, over a tenth of a \
             frame budget — something on this path is superlinear"
        );
    }

    // ---------------------------------------------------------------------
    // Sanity: fresh_state's extended dedup fields are all "unknown", so
    // every first setter call goes through.
    // ---------------------------------------------------------------------
    #[test]
    fn fresh_state_emits_first_call_for_every_setter() {
        let mut s = fresh_state();
        assert!(update_blend_func(&mut s, 1, 2));
        assert!(update_blend_equation(&mut s, 0x8006));
        assert!(update_blend_color(&mut s, 0.5, 0.5, 0.5, 1.0));
        assert!(update_depth_func(&mut s, 0x0203));
        assert!(update_depth_mask(&mut s, true));
        assert!(update_depth_range(&mut s, 0.0, 1.0));
        assert!(update_cull_face(&mut s, 0x0405));
        assert!(update_front_face(&mut s, 0x0900));
        assert!(update_line_width(&mut s, 1.0));
        assert!(update_polygon_offset(&mut s, 0.0, 0.0));
    }

    // ---------------------------------------------------------------------
    // P11 dedup expansion — per-target framebuffer / renderbuffer,
    // vertex-attribute enable/pointer/divisor, and color mask.
    // ---------------------------------------------------------------------

    #[test]
    fn framebuffer_combined_bind_repairs_each_separate_binding() {
        for target in [glow::READ_FRAMEBUFFER, glow::DRAW_FRAMEBUFFER] {
            let mut s = fresh_state();
            assert!(update_bind_framebuffer(&mut s, glow::FRAMEBUFFER, Some(7)));
            assert!(update_bind_framebuffer(&mut s, target, Some(9)));
            assert!(
                update_bind_framebuffer(&mut s, glow::FRAMEBUFFER, Some(7)),
                "FRAMEBUFFER must restore the separate target changed to 9"
            );
            assert_eq!(
                s.bound_framebuffer.get(glow::READ_FRAMEBUFFER),
                Some(Some(7))
            );
            assert_eq!(
                s.bound_framebuffer.get(glow::DRAW_FRAMEBUFFER),
                Some(Some(7))
            );
        }
    }

    #[test]
    fn framebuffer_separate_bind_after_combined_change_is_not_deduped() {
        for target in [glow::READ_FRAMEBUFFER, glow::DRAW_FRAMEBUFFER] {
            let mut s = fresh_state();
            assert!(update_bind_framebuffer(&mut s, target, Some(7)));
            assert!(update_bind_framebuffer(&mut s, glow::FRAMEBUFFER, Some(9)));
            assert!(
                update_bind_framebuffer(&mut s, target, Some(7)),
                "FRAMEBUFFER changed this target, so the bind is required"
            );
        }
    }

    #[test]
    fn framebuffer_alias_queries_and_partial_knowledge_follow_binding_points() {
        let mut s = fresh_state();
        assert!(update_bind_framebuffer(
            &mut s,
            glow::READ_FRAMEBUFFER,
            Some(7)
        ));
        assert_eq!(s.bound_framebuffer.get(glow::FRAMEBUFFER), None);
        assert!(update_bind_framebuffer(&mut s, glow::FRAMEBUFFER, Some(7)));
        assert!(update_bind_framebuffer(
            &mut s,
            glow::READ_FRAMEBUFFER,
            Some(9)
        ));
        assert_eq!(s.bound_framebuffer.get(glow::FRAMEBUFFER), Some(Some(7)));
        assert!(update_bind_framebuffer(
            &mut s,
            glow::DRAW_FRAMEBUFFER,
            Some(9)
        ));
        assert_eq!(s.bound_framebuffer.get(glow::FRAMEBUFFER), Some(Some(9)));
        assert!(!update_bind_framebuffer(&mut s, glow::FRAMEBUFFER, Some(9)));
        s.bound_framebuffer.forget_all();
        assert_eq!(s.bound_framebuffer.get(glow::FRAMEBUFFER), None);
        assert_eq!(s.bound_framebuffer.get(glow::READ_FRAMEBUFFER), None);
        assert!(update_bind_framebuffer(&mut s, glow::FRAMEBUFFER, None));
        assert!(!update_bind_framebuffer(
            &mut s,
            glow::READ_FRAMEBUFFER,
            None
        ));
    }

    #[test]
    fn bind_framebuffer_first_call_issues_then_deduped() {
        let mut s = fresh_state();
        assert!(update_bind_framebuffer(&mut s, glow::FRAMEBUFFER, Some(7)));
        assert!(!update_bind_framebuffer(&mut s, glow::FRAMEBUFFER, Some(7)));
        // The combined bind already established both binding points.
        assert!(!update_bind_framebuffer(
            &mut s,
            glow::DRAW_FRAMEBUFFER,
            Some(7)
        ));
        // Rebinding default FBO (0 / None) is a real call after a
        // named FBO was bound.
        assert!(update_bind_framebuffer(&mut s, glow::FRAMEBUFFER, None));
    }

    // ---- The engine's own re-points, and why the shadow has to hear about them.
    //
    // Measured before these existed: `scripts/fixtures/rtt-probe` binds its own
    // framebuffer, lets a canvas switch happen, re-binds the same framebuffer, and
    // clears red. It presented red full-screen on every frame — the re-bind was
    // deduped against a shadow the engine had already invalidated by re-pointing
    // the driver at the DrawingBuffer, so the render-to-texture clear landed on the
    // screen.

    /// The defect itself. `Some(7)` is still in the shadow when the engine
    /// re-points, so without a record the content's identical re-bind looks
    /// redundant and never reaches the driver.
    #[test]
    fn a_content_rebind_reaches_the_driver_after_the_engine_repoints_to_the_default() {
        let mut s = fresh_state();
        assert!(update_bind_framebuffer(&mut s, glow::FRAMEBUFFER, Some(7)));

        record_default_framebuffer_bind(&mut s);

        assert!(
            update_bind_framebuffer(&mut s, glow::FRAMEBUFFER, Some(7)),
            "the re-bind was deduped against a shadow the engine had made stale, \
             so the content would render wherever the engine last pointed"
        );
    }

    /// The control, and the reason the record is `None` rather than a `clear()`.
    /// `None` is the user-facing name for the default framebuffer, so the
    /// Cocos-style `bindFramebuffer(FRAMEBUFFER, 0)` every frame — the exact
    /// redundancy this dedup was built for — stays deduped and the fix is free.
    /// Clearing the map would satisfy the test above while costing a driver call
    /// per frame for every game that does not render to texture at all.
    #[test]
    fn a_content_bind_of_the_default_is_still_deduped_after_the_engine_repoints() {
        let mut s = fresh_state();
        assert!(update_bind_framebuffer(&mut s, glow::FRAMEBUFFER, None));

        record_default_framebuffer_bind(&mut s);

        assert!(!update_bind_framebuffer(&mut s, glow::FRAMEBUFFER, None));
    }

    /// Binding `FRAMEBUFFER` moves the draw *and* read bindings, and the site this
    /// most often follows is the swap-time blit, which bound
    /// `READ=DrawingBuffer, DRAW=0`. A record that covered only `FRAMEBUFFER`
    /// would leave a WebGL 2 content re-bind of either separate target deduped
    /// against the blit's leftovers.
    #[test]
    fn the_engine_repoint_covers_the_separate_draw_and_read_targets_too() {
        let mut s = fresh_state();
        assert!(update_bind_framebuffer(
            &mut s,
            glow::DRAW_FRAMEBUFFER,
            Some(7)
        ));
        assert!(update_bind_framebuffer(
            &mut s,
            glow::READ_FRAMEBUFFER,
            Some(9)
        ));

        record_default_framebuffer_bind(&mut s);

        assert!(update_bind_framebuffer(
            &mut s,
            glow::DRAW_FRAMEBUFFER,
            Some(7)
        ));
        assert!(update_bind_framebuffer(
            &mut s,
            glow::READ_FRAMEBUFFER,
            Some(9)
        ));
    }

    /// Damage tracking asks `draws_to_default_fbo` whether a draw can dirty the
    /// window. After the engine re-points, it can — leaving the flag false would
    /// have the frame's clear counted as invisible and the region never declared
    /// to the compositor, so the stale pixels stay on screen.
    #[test]
    fn the_engine_repoint_makes_the_canvas_draw_to_the_default_framebuffer_again() {
        let mut s = fresh_state();
        s.draws_to_default_fbo = false;

        record_default_framebuffer_bind(&mut s);

        assert!(s.draws_to_default_fbo);
    }

    #[test]
    fn bind_renderbuffer_dedups_repeated_binds() {
        let mut s = fresh_state();
        assert!(update_bind_renderbuffer(&mut s, Some(3)));
        assert!(!update_bind_renderbuffer(&mut s, Some(3)));
        assert!(update_bind_renderbuffer(&mut s, Some(4)));
        assert!(update_bind_renderbuffer(&mut s, None));
    }

    #[test]
    fn color_mask_dedups_identical_tuples() {
        let mut s = fresh_state();
        // Initial state is (true,true,true,true); an identical call
        // must dedup on first touch.
        assert!(!update_color_mask(&mut s, true, true, true, true));
        assert!(update_color_mask(&mut s, false, false, false, false));
        assert!(!update_color_mask(&mut s, false, false, false, false));
    }

    #[test]
    fn enable_and_disable_vertex_attrib_are_idempotent() {
        let mut s = fresh_state();
        assert!(update_enable_vertex_attrib(&mut s, 0));
        assert!(!update_enable_vertex_attrib(&mut s, 0));
        assert!(update_disable_vertex_attrib(&mut s, 0));
        assert!(!update_disable_vertex_attrib(&mut s, 0));
        // Different index.
        assert!(update_enable_vertex_attrib(&mut s, 1));
        assert!(!update_enable_vertex_attrib(&mut s, 1));
    }

    #[test]
    fn enable_vertex_attrib_reissues_after_vao_change() {
        // The enabled/disabled state of a vertex-attribute array lives
        // INSIDE the bound VAO (GLES 3.0 §6.2 / WebGL 2).  Enabling attrib
        // 0 on VAO 0, then binding VAO 2 (whose attrib 0 starts DISABLED)
        // and enabling attrib 0 again MUST hit the driver — otherwise the
        // newly-bound VAO draws with a disabled attribute (constant/zero
        // vertex data), producing degenerate geometry that renders nothing.
        // Regression test for the "draws with vertex attributes render
        // nothing on real devices, but clear + gl_VertexID draws work" bug.
        let mut s = fresh_state();
        assert!(update_enable_vertex_attrib(&mut s, 0)); // VAO 0: issue
        assert!(!update_enable_vertex_attrib(&mut s, 0)); // VAO 0: dedup
        update_bind_vertex_array(&mut s, Some(2));
        assert!(update_enable_vertex_attrib(&mut s, 0)); // VAO 2: MUST re-issue
        assert!(!update_enable_vertex_attrib(&mut s, 0)); // VAO 2: dedup
        // Back to VAO 0 — it still has attrib 0 enabled, so this dedups
        // (per-VAO scoping, not a blunt clear-on-bind).
        update_bind_vertex_array(&mut s, Some(0));
        assert!(!update_enable_vertex_attrib(&mut s, 0));
        // Disable is likewise VAO-scoped: disabling attrib 0 on VAO 0
        // must not report a change for VAO 2 (already independent).
        assert!(update_disable_vertex_attrib(&mut s, 0)); // VAO 0: was enabled
        update_bind_vertex_array(&mut s, Some(2));
        assert!(update_disable_vertex_attrib(&mut s, 0)); // VAO 2: was enabled
    }

    #[test]
    fn vertex_attrib_pointer_dedups_identical_layout() {
        let mut s = fresh_state();
        // First call always issues.
        assert!(update_vertex_attrib_pointer(
            &mut s,
            0,
            4,
            glow::FLOAT,
            false,
            32,
            0
        ));
        // Identical repeat — deduped.
        assert!(!update_vertex_attrib_pointer(
            &mut s,
            0,
            4,
            glow::FLOAT,
            false,
            32,
            0
        ));
        // Different offset → re-issue.
        assert!(update_vertex_attrib_pointer(
            &mut s,
            0,
            4,
            glow::FLOAT,
            false,
            32,
            16
        ));
        // Different index → re-issue (tracked per-index).
        assert!(update_vertex_attrib_pointer(
            &mut s,
            1,
            4,
            glow::FLOAT,
            false,
            32,
            0
        ));
    }

    #[test]
    fn vertex_attrib_pointer_reissues_after_vao_change() {
        // VAO capture semantics: the same (index, layout) after a
        // `bindVertexArray(2)` MUST hit the driver, even though the
        // layout tuple matches what was set for VAO 0.  If we ever
        // forget the VAO key, sprite batches switching VAOs silently
        // lose their vertex layout.
        let mut s = fresh_state();
        // Bind an ARRAY_BUFFER so the fingerprint has a concrete
        // buffer component — otherwise the two calls below also
        // differ in `array_buffer`, which would hide the VAO bug
        // behind the new buffer fingerprint.
        s.bound_array_buffer = Some(Some(99));
        assert!(update_vertex_attrib_pointer(
            &mut s,
            0,
            4,
            glow::FLOAT,
            false,
            32,
            0
        ));
        s.bound_vao = Some(2);
        assert!(update_vertex_attrib_pointer(
            &mut s,
            0,
            4,
            glow::FLOAT,
            false,
            32,
            0
        ));
        assert!(!update_vertex_attrib_pointer(
            &mut s,
            0,
            4,
            glow::FLOAT,
            false,
            32,
            0
        ));
    }

    // ---- ARRAY_BUFFER fingerprint (P0-3 regression) -----------------
    //
    // WebGL spec: `vertexAttribPointer` captures the currently-bound
    // `ARRAY_BUFFER`.  Two calls with identical (size, type, stride,
    // offset) but different bound buffers must both reach the driver
    // — otherwise the next draw reads from the wrong vertex stream
    // and the game paints corrupted geometry.
    //
    // These tests pin the fingerprint down so nobody accidentally
    // drops `array_buffer` again.

    #[test]
    fn vertex_attrib_pointer_reissues_when_array_buffer_changes() {
        let mut s = fresh_state();
        // Establish baseline with buffer A.
        s.bound_array_buffer = Some(Some(42));
        assert!(update_vertex_attrib_pointer(
            &mut s,
            0,
            4,
            glow::FLOAT,
            false,
            32,
            0
        ));
        // Switch ARRAY_BUFFER to buffer B — same layout args, but
        // a different buffer must force the driver call.
        s.bound_array_buffer = Some(Some(43));
        assert!(
            update_vertex_attrib_pointer(&mut s, 0, 4, glow::FLOAT, false, 32, 0),
            "switching ARRAY_BUFFER with identical pointer args MUST re-issue"
        );
        // Same buffer, same args — NOW the dedup should fire.
        assert!(!update_vertex_attrib_pointer(
            &mut s,
            0,
            4,
            glow::FLOAT,
            false,
            32,
            0
        ));
    }

    #[test]
    fn vertex_attrib_pointer_reissues_when_buffer_goes_back_and_forth() {
        // Real Cocos 2.x workload: ping-pong between two VBOs
        // (positions vs. UVs) on the same attribute index across
        // sprite batches.  Each ping-pong must re-issue even
        // though the layout tuple is identical.
        let mut s = fresh_state();
        s.bound_array_buffer = Some(Some(1));
        assert!(update_vertex_attrib_pointer(
            &mut s,
            0,
            2,
            glow::FLOAT,
            false,
            8,
            0
        ));
        s.bound_array_buffer = Some(Some(2));
        assert!(update_vertex_attrib_pointer(
            &mut s,
            0,
            2,
            glow::FLOAT,
            false,
            8,
            0
        ));
        s.bound_array_buffer = Some(Some(1));
        assert!(update_vertex_attrib_pointer(
            &mut s,
            0,
            2,
            glow::FLOAT,
            false,
            8,
            0
        ));
        s.bound_array_buffer = Some(Some(1));
        assert!(!update_vertex_attrib_pointer(
            &mut s,
            0,
            2,
            glow::FLOAT,
            false,
            8,
            0
        ));
    }

    #[test]
    fn vertex_attrib_pointer_keys_by_vao_scope_across_buffer_switches() {
        // Four distinct (VAO, ARRAY_BUFFER) combinations.  VAOs get
        // independent slots in the shadow; the `array_buffer`
        // component inside the fingerprint forces re-issue whenever
        // the bound buffer changes.  Re-visiting a *prior* buffer
        // on the same (VAO, index) re-issues — we don't retain
        // per-(VAO, index, buffer) history because Cocos-style
        // workloads overwhelmingly set the buffer once per VAO
        // scope and keep it, and the storage cost of retaining N
        // buffers per slot would dwarf the saved calls.
        //
        // Cross-VAO isolation IS preserved: switching to VAO=2 and
        // back to VAO=1 with the originally-bound buffer DOES
        // dedup, because VAO=1's slot was never touched while
        // VAO=2 was active.
        let mut s = fresh_state();
        s.bound_vao = Some(1);
        s.bound_array_buffer = Some(Some(10));
        assert!(update_vertex_attrib_pointer(
            &mut s,
            0,
            4,
            glow::FLOAT,
            false,
            16,
            0
        ));
        // Same VAO, different buffer → overwrites VAO 1's slot.
        s.bound_array_buffer = Some(Some(11));
        assert!(update_vertex_attrib_pointer(
            &mut s,
            0,
            4,
            glow::FLOAT,
            false,
            16,
            0
        ));
        // Switch to VAO 2, buffer 10.
        s.bound_vao = Some(2);
        s.bound_array_buffer = Some(Some(10));
        assert!(update_vertex_attrib_pointer(
            &mut s,
            0,
            4,
            glow::FLOAT,
            false,
            16,
            0
        ));
        // Switch back to VAO 1 WITHOUT touching its slot — the
        // previous (vao=1, buffer=11) fp is still cached, so
        // re-applying with buffer=11 dedups.
        s.bound_vao = Some(1);
        s.bound_array_buffer = Some(Some(11));
        assert!(!update_vertex_attrib_pointer(
            &mut s,
            0,
            4,
            glow::FLOAT,
            false,
            16,
            0
        ));
        // But buffer=10 on VAO 1 was overwritten by the earlier
        // buffer=11 set, so it must re-issue now.
        s.bound_array_buffer = Some(Some(10));
        assert!(update_vertex_attrib_pointer(
            &mut s,
            0,
            4,
            glow::FLOAT,
            false,
            16,
            0
        ));
    }

    #[test]
    fn vertex_attrib_divisor_dedups_per_index() {
        let mut s = fresh_state();
        assert!(update_vertex_attrib_divisor(&mut s, 0, 1));
        assert!(!update_vertex_attrib_divisor(&mut s, 0, 1));
        assert!(update_vertex_attrib_divisor(&mut s, 0, 0));
        assert!(update_vertex_attrib_divisor(&mut s, 1, 1));
    }

    #[test]
    fn vertex_attrib_divisor_reissues_after_vao_change() {
        // Divisor is per-VAO too: the same (index, divisor) after binding
        // a different VAO must hit the driver, or instanced draws on the
        // new VAO inherit the wrong divisor.
        let mut s = fresh_state();
        assert!(update_vertex_attrib_divisor(&mut s, 0, 1)); // VAO 0: issue
        assert!(!update_vertex_attrib_divisor(&mut s, 0, 1)); // VAO 0: dedup
        update_bind_vertex_array(&mut s, Some(2));
        assert!(update_vertex_attrib_divisor(&mut s, 0, 1)); // VAO 2: re-issue
        assert!(!update_vertex_attrib_divisor(&mut s, 0, 1)); // VAO 2: dedup
    }

    #[test]
    fn stencil_func_dedup_per_face() {
        let mut s = fresh_state();
        assert!(update_stencil_func(
            &mut s,
            glow::FRONT,
            glow::EQUAL,
            0,
            0xFF
        ));
        assert!(!update_stencil_func(
            &mut s,
            glow::FRONT,
            glow::EQUAL,
            0,
            0xFF
        ));
        // BACK face has no fingerprint yet — must re-issue.
        assert!(update_stencil_func(
            &mut s,
            glow::BACK,
            glow::EQUAL,
            0,
            0xFF
        ));
        // FRONT_AND_BACK dedups only when BOTH faces already match.
        assert!(!update_stencil_func(
            &mut s,
            glow::FRONT_AND_BACK,
            glow::EQUAL,
            0,
            0xFF
        ));
    }

    #[test]
    fn stencil_op_front_and_back_dedup() {
        let mut s = fresh_state();
        assert!(update_stencil_op(
            &mut s,
            glow::FRONT_AND_BACK,
            glow::KEEP,
            glow::KEEP,
            glow::REPLACE
        ));
        // Identical FRONT_AND_BACK dedups.
        assert!(!update_stencil_op(
            &mut s,
            glow::FRONT_AND_BACK,
            glow::KEEP,
            glow::KEEP,
            glow::REPLACE
        ));
        // Single-face with identical fp dedups too.
        assert!(!update_stencil_op(
            &mut s,
            glow::FRONT,
            glow::KEEP,
            glow::KEEP,
            glow::REPLACE
        ));
    }

    #[test]
    fn stencil_mask_separate_front_back_tracked_independently() {
        let mut s = fresh_state();
        assert!(update_stencil_mask(&mut s, glow::FRONT, 0x0F));
        assert!(!update_stencil_mask(&mut s, glow::FRONT, 0x0F));
        assert!(update_stencil_mask(&mut s, glow::BACK, 0x0F));
        // FRONT_AND_BACK with matching values on both — dedups.
        assert!(!update_stencil_mask(&mut s, glow::FRONT_AND_BACK, 0x0F));
        // FRONT_AND_BACK with a new value — re-issues.
        assert!(update_stencil_mask(&mut s, glow::FRONT_AND_BACK, 0xF0));
    }

    #[test]
    fn pixel_store_i32_dedups_per_pname() {
        let mut s = fresh_state();
        assert!(update_pixel_store_i32(&mut s, glow::UNPACK_ALIGNMENT, 4));
        assert!(!update_pixel_store_i32(&mut s, glow::UNPACK_ALIGNMENT, 4));
        assert!(update_pixel_store_i32(&mut s, glow::UNPACK_ALIGNMENT, 1));
        // Different pname does not collide.
        assert!(update_pixel_store_i32(&mut s, glow::PACK_ALIGNMENT, 1));
    }

    #[test]
    fn depth_range_dedups_and_reissues_after_external() {
        let mut s = fresh_state();
        assert!(update_depth_range(&mut s, 0.0, 1.0));
        assert!(!update_depth_range(&mut s, 0.0, 1.0));
        assert!(update_depth_range(&mut s, 0.1, 0.9));
        s.invalidate_after_external_gl_use();
        assert!(update_depth_range(&mut s, 0.1, 0.9));
    }

    #[test]
    fn stencil_state_is_cleared_on_external_gl_use() {
        let mut s = fresh_state();
        let _ = update_stencil_func(&mut s, glow::FRONT, glow::EQUAL, 0, 0xFF);
        let _ = update_stencil_op(&mut s, glow::FRONT, glow::KEEP, glow::KEEP, glow::REPLACE);
        let _ = update_stencil_mask(&mut s, glow::FRONT, 0xFF);
        let _ = update_pixel_store_i32(&mut s, glow::UNPACK_ALIGNMENT, 4);
        s.invalidate_after_external_gl_use();
        // All four families re-issue on next call.
        assert!(update_stencil_func(
            &mut s,
            glow::FRONT,
            glow::EQUAL,
            0,
            0xFF
        ));
        assert!(update_stencil_op(
            &mut s,
            glow::FRONT,
            glow::KEEP,
            glow::KEEP,
            glow::REPLACE
        ));
        assert!(update_stencil_mask(&mut s, glow::FRONT, 0xFF));
        assert!(update_pixel_store_i32(&mut s, glow::UNPACK_ALIGNMENT, 4));
    }

    #[test]
    fn viewport_dedups_identical_values() {
        let mut s = fresh_state();
        assert!(
            update_viewport(&mut s, 0, 0, 800, 600),
            "first call with unknown previous state must issue"
        );
        assert!(
            !update_viewport(&mut s, 0, 0, 800, 600),
            "identical viewport must dedup"
        );
        assert!(
            update_viewport(&mut s, 0, 0, 1024, 600),
            "width change must re-issue"
        );
        assert!(
            update_viewport(&mut s, 10, 0, 1024, 600),
            "origin change must re-issue"
        );
    }

    #[test]
    fn viewport_reissues_after_external_gl_use() {
        let mut s = fresh_state();
        let _ = update_viewport(&mut s, 0, 0, 800, 600);
        s.invalidate_after_external_gl_use();
        // After the Skia boundary, viewport may have been mutated
        // by Ganesh; the shadow is cleared so the next call
        // re-issues.
        assert!(update_viewport(&mut s, 0, 0, 800, 600));
    }

    #[test]
    fn invalidate_after_external_gl_use_preserves_uniform_cache() {
        // Regression from the 2026-04 rendering review: an earlier
        // revision called `self.uniform_cache.clear()` inside
        // `invalidate_after_external_gl_use`, contradicting the doc
        // comment and nuking the entire uniform dedup table every
        // Skia ↔ WebGL boundary crossing.  Uniforms live on GL
        // program objects, not on the shared context state Skia
        // touches, so the cache MUST survive.
        //
        // If this test ever fails, profile the next production
        // frame before re-adding the clear — it will re-issue every
        // `glUniform*` call the app already deduped, which is the
        // single dominant GL op category on shader-heavy workloads.
        let mut s = fresh_state();
        let prog: ProgramId = 7;
        let loc: u32 = 3;
        let bytes = [1u8, 2, 3, 4];
        assert!(update_uniform(&mut s, prog, loc, &bytes));
        // Second call with identical bytes — dedup must fire.
        assert!(!update_uniform(&mut s, prog, loc, &bytes));

        s.invalidate_after_external_gl_use();

        // After invalidation the uniform dedup entry MUST remain,
        // so the next identical call still dedups.
        assert!(
            !update_uniform(&mut s, prog, loc, &bytes),
            "uniform_cache was wiped by invalidate_after_external_gl_use"
        );
    }

    #[test]
    fn invalidate_after_external_gl_use_clears_p11_shadow() {
        let mut s = fresh_state();
        let _ = update_bind_framebuffer(&mut s, glow::FRAMEBUFFER, Some(7));
        let _ = update_enable_vertex_attrib(&mut s, 3);
        let _ = update_vertex_attrib_pointer(&mut s, 3, 4, glow::FLOAT, false, 32, 0);
        let _ = update_vertex_attrib_divisor(&mut s, 3, 1);

        s.invalidate_after_external_gl_use();

        // Every setter must re-issue after invalidation — that's the
        // whole point of marking state stale around a Skia batch.
        assert!(update_bind_framebuffer(&mut s, glow::FRAMEBUFFER, Some(7)));
        assert!(update_enable_vertex_attrib(&mut s, 3));
        assert!(update_vertex_attrib_pointer(
            &mut s,
            3,
            4,
            glow::FLOAT,
            false,
            32,
            0
        ));
        assert!(update_vertex_attrib_divisor(&mut s, 3, 1));
    }
}

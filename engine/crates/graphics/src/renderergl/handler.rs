use glow::{HasContext, NativeUniformLocation};
use shared::{
    error::{EngineError, EngineResult, ErrorCode},
    protocol::render_cmd::{
        CanvasId, CompressedImageData, GLCmd, PixelUnpackSource, ProgramId, ShaderType,
        checked_readback_byte_len, webgl_readback_bytes_per_pixel,
    },
};
use smallvec::SmallVec;
use std::collections::HashMap;

#[cfg(test)]
use crate::CanvasGLState;
use crate::CanvasManager;
use crate::ScissorState;
use crate::backend::gl::state_tracker as st;
use crate::backend::gl::unpack_convert;
use crate::canvas::gl_object::GlObject;
use crate::damage_effect::DamageEffect;
use crate::renderergl::link_queue::{self, DrainCause};
use crate::shader_cache::LinkDescriptor;
use crate::webgl_gpu_budget::GpuAllocationError;

#[inline]
fn ee(code: ErrorCode, detail: impl Into<String>) -> EngineError {
    EngineError::from_detail(code, detail)
}

#[inline]
fn gpu_allocation_error(error: GpuAllocationError) -> EngineError {
    let code = match error {
        GpuAllocationError::InvalidEnum | GpuAllocationError::InvalidValue => {
            ErrorCode::InvalidArgument
        }
        GpuAllocationError::InvalidOperation => ErrorCode::InvalidOperation,
        GpuAllocationError::OutOfMemory => ErrorCode::OutOfMemory,
    };
    ee(code, format!("WebGL GPU storage rejected: {error:?}"))
}

/// An upload whose bytes are fewer than the driver reads with the pixel-store state `layout` -- or of a (format, type)
/// pair GL has no upload of -- is refused before the driver is asked: the driver reads what the layout says from the
/// pointer it is given, past the end of a short slice, and on the Performance+ lane the bytes come from another
/// process. See [`unpack_convert::upload_bytes`].
fn refuse_short_upload(
    bytes: &[u8],
    width: i32,
    height: i32,
    depth: i32,
    format: u32,
    type_: u32,
    layout: &unpack_convert::UnpackLayout,
) -> EngineResult<()> {
    match unpack_convert::upload_bytes(width, height, depth, format, type_, layout) {
        Some(needed) if bytes.len() >= needed => Ok(()),
        Some(needed) => Err(ee(
            ErrorCode::InvalidOperation,
            format!(
                "upload of {} bytes where its rows read {needed}",
                bytes.len()
            ),
        )),
        None => Err(ee(
            ErrorCode::InvalidOperation,
            format!("no upload of format {format:#x}, type {type_:#x} at {width}x{height}x{depth}"),
        )),
    }
}

/// GL reads an upload's data argument as an offset into the bound PIXEL_UNPACK_BUFFER, or -- none bound -- as an
/// address. An upload that names an offset is refused unless a buffer is bound: the driver would otherwise read host
/// memory at that address, and on the Performance+ lane the offset comes from another process. The driver is asked,
/// not the binding shadow, which forgets the binding across Skia's use of the context; only these uploads ask.
fn refuse_offset_without_unpack_buffer(gl: &glow::Context) -> EngineResult<()> {
    // SAFETY: a state query on the current context.
    if unsafe { gl.get_parameter_i32(glow::PIXEL_UNPACK_BUFFER_BINDING) } != 0 {
        return Ok(());
    }
    Err(ee(
        ErrorCode::InvalidOperation,
        "upload from a PIXEL_UNPACK_BUFFER offset with no buffer bound",
    ))
}

/// What an upload hands the driver: the call's bytes, refused when the driver would read past them
/// ([`refuse_short_upload`]), or an offset into the PIXEL_UNPACK_BUFFER, refused when none is bound
/// ([`refuse_offset_without_unpack_buffer`]). The upload's context must be current.
#[allow(clippy::too_many_arguments)]
fn unpack_data<'a>(
    gl: &glow::Context,
    source: &'a PixelUnpackSource,
    width: i32,
    height: i32,
    depth: i32,
    format: u32,
    type_: u32,
    layout: &unpack_convert::UnpackLayout,
) -> EngineResult<glow::PixelUnpackData<'a>> {
    match source {
        PixelUnpackSource::Bytes(bytes) => {
            refuse_short_upload(bytes, width, height, depth, format, type_, layout)?;
            Ok(glow::PixelUnpackData::Slice(Some(bytes)))
        }
        PixelUnpackSource::BufferOffset(offset) => {
            refuse_offset_without_unpack_buffer(gl)?;
            Ok(glow::PixelUnpackData::BufferOffset(*offset))
        }
    }
}

#[inline]
fn to_native_uniform_location(location: Option<u32>) -> Option<NativeUniformLocation> {
    location.map(NativeUniformLocation)
}

/// Identity after logical/physical coordinate unification.
/// Kept as a wrapper so the Scissor call site reads clearly.
#[inline]
fn logical_to_physical_i32(_cm: &CanvasManager, v: i32) -> i32 {
    v
}

pub(crate) struct RendererGL {
    /// Transform-feedback state is link input, not merely post-link metadata.
    tf_descriptors: HashMap<ProgramId, (Vec<String>, u32)>,
}

/// Per-location-range uniform value-dedup.
///
/// `location_count` is the number of consecutive GL uniform locations
/// touched by this setter. Arrays can be addressed through their base or an
/// element location, so the state tracker invalidates overlapping cached
/// ranges before comparing bytes. Matrix arrays consume one location per
/// matrix column, not merely one location per matrix value.
///
/// Returns `true` when the driver call must actually be issued
/// (first set, value changed, or we don't know the current program yet).
/// Returns `false` when the byte-identical value is already live, in
/// which case the caller should skip the `glUniform*` call.
#[inline]
fn should_issue_uniform(
    cm: &mut CanvasManager,
    canvas_id: CanvasId,
    location: Option<u32>,
    location_count: u32,
    bytes: &[u8],
) -> bool {
    // `uniform(null, …)` is a GL no-op already; skip without even
    // checking state to avoid polluting the cache with location=0.
    let Some(loc) = location else {
        return false;
    };
    let state = cm.gl_state.entry(canvas_id).or_default();
    // If we have never seen a `useProgram`, we cannot scope the cache
    // key safely (locations collide across programs).  Issue the call
    // and let a later useProgram install the cache.
    let Some(program) = state.current_program else {
        return true;
    };
    st::update_uniform_range(state, program, loc, location_count, bytes)
}

/// Build a scratch buffer `[transpose_byte, matrix_bytes...]` for
/// matrix-uniform dedup.  Returned slice's lifetime is the caller's
/// scratch SmallVec. One mat4 plus the transpose byte fits inline.
#[inline]
fn mat_uniform_bytes<'a>(
    scratch: &'a mut SmallVec<[u8; 65]>,
    transpose: bool,
    data: &[f32],
) -> &'a [u8] {
    scratch.clear();
    scratch.push(transpose as u8);
    scratch.extend_from_slice(bytemuck::cast_slice::<f32, u8>(data));
    scratch.as_slice()
}

impl RendererGL {
    pub(crate) fn new() -> Self {
        Self {
            tf_descriptors: HashMap::new(),
        }
    }

    fn maybe_log_draw_state(
        &mut self,
        _gl: &glow::Context,
        _canvas_id: CanvasId,
        _mode: u32,
        _count: i32,
    ) {
    }

    #[inline]
    fn bind_for_contextless_gl(&mut self, cm: &mut CanvasManager) -> EngineResult<CanvasId> {
        cm.ensure_any_canvas_current()
    }

    fn current_owner_canvas(cm: &CanvasManager) -> Option<CanvasId> {
        cm.current_canvas_id()
    }

    /// Read `LINK_STATUS` for deferred programs.  `target` is `Some` for a
    /// content query: unrelated links remain queued for a later batch.
    pub(crate) fn drain_pending_links(
        cm: &mut CanvasManager,
        gl: &glow::Context,
        cause: DrainCause,
    ) {
        Self::drain_pending_links_for(cm, gl, cause, None);
    }

    fn drain_pending_link(cm: &mut CanvasManager, gl: &glow::Context, program_id: ProgramId) {
        Self::drain_pending_links_for(cm, gl, DrainCause::ContentAsked, Some(program_id));
    }

    fn drain_pending_links_for(
        cm: &mut CanvasManager,
        gl: &glow::Context,
        cause: DrainCause,
        target: Option<ProgramId>,
    ) {
        if cm.pending_links.is_empty() {
            return;
        }
        let parallel = cm.device_caps.has_parallel_shader_compile;
        let queue = std::mem::take(&mut cm.pending_links);
        let mut still_pending = Vec::with_capacity(queue.len());

        for program_id in queue {
            if target.is_some_and(|wanted| wanted != program_id) {
                still_pending.push(program_id);
                continue;
            }
            let Some(meta) = cm.programs.get(&program_id) else {
                continue;
            };
            if meta.deleted || !meta.link_pending {
                continue;
            }
            let Some(ph) = meta.gl_handle else {
                continue;
            };

            let completion_done = parallel.then(|| unsafe {
                gl.get_program_parameter_i32(ph, link_queue::COMPLETION_STATUS_KHR) != 0
            });
            if link_queue::probe(cause, completion_done) == link_queue::LinkProbe::LeavePending {
                still_pending.push(program_id);
                continue;
            }

            let (vsrc, fsrc) = Self::get_program_shader_sources(cm, meta);
            let attrib_key = Self::attribute_key(&meta.attrib_bindings);
            let link_ok = unsafe { gl.get_program_link_status(ph) };
            if link_ok {
                let (tf_varyings, tf_buffer_mode) = Self::query_tf_descriptor(gl, ph);
                if let (Some(cache), Some(vs), Some(fs)) =
                    (&cm.shader_cache, vsrc.as_deref(), fsrc.as_deref())
                {
                    let descriptor = LinkDescriptor {
                        vertex_src: vs,
                        fragment_src: fs,
                        attrib_key: &attrib_key,
                        tf_varyings: &tf_varyings,
                        tf_buffer_mode,
                    };
                    cache.save(gl, ph, &descriptor);
                }
            }
            if let Some(meta) = cm.programs.get_mut(&program_id) {
                meta.link_pending = false;
            }
        }
        cm.pending_links = still_pending;
    }

    fn attribute_key(bindings: &[(u32, String)]) -> String {
        let mut normalized = bindings.to_vec();
        normalized.sort_by(|(index_a, name_a), (index_b, name_b)| {
            name_a.cmp(name_b).then(index_a.cmp(index_b))
        });
        normalized
            .iter()
            .map(|(index, name)| format!("{name}={index};"))
            .collect()
    }

    fn query_tf_descriptor(
        gl: &glow::Context,
        program: glow::NativeProgram,
    ) -> (Vec<String>, Option<u32>) {
        let count =
            unsafe { gl.get_program_parameter_i32(program, glow::TRANSFORM_FEEDBACK_VARYINGS) };
        let varyings = if count > 0 {
            (0..count as u32)
                .filter_map(|index| unsafe {
                    gl.get_transform_feedback_varying(program, index)
                        .map(|v| v.name)
                })
                .collect()
        } else {
            Vec::new()
        };
        let mode = unsafe {
            gl.get_program_parameter_i32(program, glow::TRANSFORM_FEEDBACK_BUFFER_MODE) as u32
        };
        (varyings, Some(mode))
    }

    /// Look up the vertex and fragment shader sources for a program's
    /// attached shaders.  Used as cache key for shader binary caching.
    fn get_program_shader_sources(
        cm: &CanvasManager,
        meta: &crate::canvas::ProgramMeta,
    ) -> (Option<String>, Option<String>) {
        let mut vertex_src = None;
        let mut fragment_src = None;
        for sid in &meta.attached_shaders {
            if let Some(smeta) = cm.shaders.get(sid) {
                if smeta.gl_shader_type == glow::VERTEX_SHADER {
                    vertex_src = smeta.source.clone();
                } else if smeta.gl_shader_type == glow::FRAGMENT_SHADER {
                    fragment_src = smeta.source.clone();
                }
            }
        }
        (vertex_src, fragment_src)
    }

    /// Compute the DamageEffect for a draw call (drawArrays/drawElements).
    /// Uses viewport ∩ scissor when scissor is enabled.
    fn damage_for_draw(cm: &CanvasManager, canvas_id: CanvasId) -> DamageEffect {
        let onscreen = CanvasId::from(1u32);
        if canvas_id != onscreen {
            return DamageEffect::NoDamage;
        }
        let state = match cm.gl_state.get(&canvas_id) {
            Some(s) => s,
            None => return DamageEffect::FullSurface,
        };
        draw_damage_effect(state.draws_to_default_fbo, state.viewport, state.scissor)
    }

    /// Compute the DamageEffect for a glClear call.
    /// Only color buffer clears produce visible damage. Depth/stencil-only
    /// clears are invisible to the compositor and return NoDamage.
    fn damage_for_clear(cm: &CanvasManager, canvas_id: CanvasId, bit_field: u32) -> DamageEffect {
        let onscreen = CanvasId::from(1u32);
        let state = cm.gl_state.get(&canvas_id);
        let is_onscreen_default_fbo = if canvas_id != onscreen {
            false
        } else {
            state.map_or(true, |s| s.draws_to_default_fbo)
        };
        let scissor = state.map_or(ScissorState::Disabled, |s| s.scissor);
        let color_mask = state.map_or((true, true, true, true), |s| s.color_mask);

        clear_damage_effect(bit_field, is_onscreen_default_fbo, scissor, color_mask)
    }

    /// The damage a `clearBuffer*` does: that of the `clear` of the same buffers (see [`clear_buffer_bits`]).
    fn damage_for_clear_buffer(
        cm: &CanvasManager,
        canvas_id: CanvasId,
        buffer: u32,
        drawbuffer: i32,
    ) -> DamageEffect {
        Self::damage_for_clear(cm, canvas_id, clear_buffer_bits(buffer, drawbuffer))
    }

    /// Process a single GL command.
    ///
    /// PERF: Per-command `make_current_needed` overhead.
    /// Every per-canvas GL command calls `cm.make_current_needed(canvas_id)`.
    /// In the common single-canvas case this is a cheap `BoundContext`
    /// enum comparison (already O(1) short-circuit when the canvas is
    /// already current).  In multi-canvas scenarios, consecutive commands
    /// targeting the same canvas also short-circuit after the first call.
    /// The only real EGL cost (`eglMakeCurrent`) is paid on actual canvas
    /// switches, which are rare within a single batch.
    pub(crate) fn handle_command(
        &mut self,
        cm: &mut CanvasManager,
        gl: &glow::Context,
        cmd: GLCmd,
    ) -> EngineResult<DamageEffect> {
        if let Some(canvas_id) = cmd.touches_canvas() {
            cm.ensure_gl_initial_state(canvas_id)?;
        }
        match cmd {
            // ---------- Per-canvas stateful calls ----------
            GLCmd::Viewport {
                canvas_id,
                x,
                y,
                width,
                height,
            } => {
                cm.make_current_needed(canvas_id)?;
                // Dedup against the shadow state: many engines set
                // the same viewport every frame (or even every draw
                // if a sub-system is over-cautious).  The GL call is
                // not free — it's one of the handful of driver round
                // trips that can't be batched with anything else.
                // Values are in physical (buffer) pixels — no DPR
                // scaling, matching browser WebGL semantics.
                let entry = cm.gl_state.entry(canvas_id).or_default();
                if st::update_viewport(entry, x, y, width as i32, height as i32) {
                    unsafe { gl.viewport(x, y, width as i32, height as i32) };
                }
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::Clear {
                canvas_id,
                bit_field,
            } => {
                cm.make_current_needed(canvas_id)?;
                unsafe { gl.clear(bit_field) };
                Ok(Self::damage_for_clear(cm, canvas_id, bit_field))
            }

            GLCmd::DebugLoseContext { canvas_id: _ } => {
                // Debug trigger (MIGO_debug_gpu_reset): arm a one-shot
                // simulated reset; the next check_graphics_reset_status() poll
                // drives the real loss -> recovery pipeline.
                cm.request_simulated_reset();
                Ok(DamageEffect::NoDamage)
            }

            // `gl.flush()` / `gl.finish()`, and the end of a task that made a fence: the context's commands go to the
            // GPU. Nothing else submits a context the content only draws into offscreen, so on a driver that batches
            // (ANGLE on Metal) a fence was never submitted and never signalled.
            GLCmd::Flush { canvas_id } => {
                cm.make_current_needed(canvas_id)?;
                unsafe { gl.flush() };
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::SampleCoverage {
                canvas_id,
                value,
                invert,
            } => {
                cm.make_current_needed(canvas_id)?;
                unsafe { gl.sample_coverage(value, invert) };
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::ClearColor {
                canvas_id,
                r,
                g,
                b,
                a,
            } => {
                cm.make_current_needed(canvas_id)?;
                unsafe { gl.clear_color(r, g, b, a) };
                Ok(DamageEffect::NoDamage)
            }

            // ---------- clearBuffer* (WebGL 2) ----------
            // The draw-buffer-specific clears. Like `clear`, they honour the scissor and the write masks, and one that
            // lands in the onscreen default framebuffer changes what is shown; only COLOR in draw buffer 0 does
            // (a default framebuffer has no other colour buffer, and depth / stencil are never presented).
            GLCmd::ClearBufferfv {
                canvas_id,
                buffer,
                drawbuffer,
                value,
            } => {
                cm.make_current_needed(canvas_id)?;
                let components = if buffer == glow::COLOR { 4 } else { 1 };
                unsafe {
                    gl.clear_buffer_f32_slice(buffer, drawbuffer as u32, &value[..components])
                };
                Ok(Self::damage_for_clear_buffer(
                    cm, canvas_id, buffer, drawbuffer,
                ))
            }
            GLCmd::ClearBufferiv {
                canvas_id,
                buffer,
                drawbuffer,
                value,
            } => {
                cm.make_current_needed(canvas_id)?;
                let components = if buffer == glow::COLOR { 4 } else { 1 };
                unsafe {
                    gl.clear_buffer_i32_slice(buffer, drawbuffer as u32, &value[..components])
                };
                Ok(Self::damage_for_clear_buffer(
                    cm, canvas_id, buffer, drawbuffer,
                ))
            }
            GLCmd::ClearBufferuiv {
                canvas_id,
                buffer,
                drawbuffer,
                value,
            } => {
                cm.make_current_needed(canvas_id)?;
                unsafe { gl.clear_buffer_u32_slice(buffer, drawbuffer as u32, &value) };
                Ok(Self::damage_for_clear_buffer(
                    cm, canvas_id, buffer, drawbuffer,
                ))
            }
            GLCmd::ClearBufferfi {
                canvas_id,
                depth,
                stencil,
            } => {
                cm.make_current_needed(canvas_id)?;
                unsafe { gl.clear_buffer_depth_stencil(glow::DEPTH_STENCIL, 0, depth, stencil) };
                Ok(Self::damage_for_clear_buffer(
                    cm,
                    canvas_id,
                    glow::DEPTH_STENCIL,
                    0,
                ))
            }

            // ---------- Copies from the read framebuffer and between buffers ----------
            // `copyTexImage2D` (re)defines a level, so it is charged to the GPU budget as a `texImage2D` of the same
            // internal format and size is; the sub copies and the buffer copy write storage that already exists.
            GLCmd::CopyTexImage2D {
                canvas_id,
                target,
                level,
                internalformat,
                x,
                y,
                width,
                height,
            } => {
                cm.make_current_needed(canvas_id)?;
                let prepared = cm
                    .webgl_gpu_budget
                    .prepare_copy_tex_image_2d(
                        canvas_id,
                        target,
                        level,
                        internalformat,
                        width,
                        height,
                    )
                    .map_err(gpu_allocation_error)?;
                unsafe {
                    gl.copy_tex_image_2d(target, level, internalformat, x, y, width, height, 0)
                };
                cm.webgl_gpu_budget.commit(prepared);
                Ok(DamageEffect::NoDamage)
            }
            GLCmd::CopyTexSubImage2D {
                canvas_id,
                target,
                level,
                xoffset,
                yoffset,
                x,
                y,
                width,
                height,
            } => {
                cm.make_current_needed(canvas_id)?;
                unsafe {
                    gl.copy_tex_sub_image_2d(target, level, xoffset, yoffset, x, y, width, height)
                };
                Ok(DamageEffect::NoDamage)
            }
            GLCmd::CopyTexSubImage3D {
                canvas_id,
                target,
                level,
                xoffset,
                yoffset,
                zoffset,
                x,
                y,
                width,
                height,
            } => {
                cm.make_current_needed(canvas_id)?;
                unsafe {
                    gl.copy_tex_sub_image_3d(
                        target, level, xoffset, yoffset, zoffset, x, y, width, height,
                    )
                };
                Ok(DamageEffect::NoDamage)
            }
            GLCmd::CopyBufferSubData {
                canvas_id,
                read_target,
                write_target,
                read_offset,
                write_offset,
                size,
            } => {
                cm.make_current_needed(canvas_id)?;
                unsafe {
                    gl.copy_buffer_sub_data(
                        read_target,
                        write_target,
                        read_offset,
                        write_offset,
                        size,
                    )
                };
                Ok(DamageEffect::NoDamage)
            }

            // ---------- Program (stateful) ----------
            GLCmd::UseProgram {
                canvas_id,
                program_id,
            } => {
                cm.make_current_needed(canvas_id)?;
                // `useProgram(null)` crosses as program 0: it is the call that unbinds the current program (a
                // draw after it is INVALID_OPERATION). Phaser makes it while it sets its context up; it was an
                // error here -- "program not found: 0" -- and the previous program stayed bound.
                if program_id == 0 {
                    let entry = cm.gl_state.entry(canvas_id).or_default();
                    if entry.current_program.take().is_some() {
                        unsafe { gl.use_program(None) };
                    }
                    return Ok(DamageEffect::NoDamage);
                }
                let meta = cm.programs.get(&program_id).ok_or_else(|| {
                    ee(
                        ErrorCode::NotFound,
                        format!("program not found: {program_id:?}"),
                    )
                })?;
                cm.check_owner(meta.owner_canvas, canvas_id, "program")?;

                if meta.deleted {
                    shared::bail!(
                        ErrorCode::InvalidOperation,
                        "use_program on deleted program"
                    );
                }

                if let Some(ph) = meta.gl_handle {
                    let entry = cm.gl_state.entry(canvas_id).or_default();
                    if st::update_use_program(entry, program_id) {
                        unsafe { gl.use_program(Some(ph)) };
                    }
                }
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::GetAttribLocation {
                canvas_id,
                program_id,
                name,
                resp,
            } => {
                cm.make_current_needed(canvas_id)?;
                let meta = cm.programs.get(&program_id).ok_or_else(|| {
                    ee(
                        ErrorCode::NotFound,
                        format!("program not found: {program_id:?}"),
                    )
                })?;
                if let Err(e) = cm.check_owner(meta.owner_canvas, canvas_id, "program") {
                    let _ = resp.send(Err(e));
                    return Ok(DamageEffect::NoDamage);
                }
                if meta.deleted {
                    let _ = resp.send(Ok(None));
                    return Ok(DamageEffect::NoDamage);
                }

                if let Some(ph) = meta.gl_handle {
                    unsafe {
                        let loc = gl.get_attrib_location(ph, &name);
                        let _ = resp.send(Ok(loc));
                    }
                } else {
                    let _ = resp.send(Ok(None));
                }
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::GetActiveAttrib {
                canvas_id,
                program_id,
                index,
                resp,
            } => {
                cm.make_current_needed(canvas_id)?;
                let meta = cm.programs.get(&program_id).ok_or_else(|| {
                    ee(
                        ErrorCode::NotFound,
                        format!("program not found: {program_id:?}"),
                    )
                })?;
                if let Err(e) = cm.check_owner(meta.owner_canvas, canvas_id, "program") {
                    let _ = resp.send(Err(e));
                    return Ok(DamageEffect::NoDamage);
                }
                if meta.deleted {
                    let _ = resp.send(Ok(None));
                    return Ok(DamageEffect::NoDamage);
                }

                if let Some(ph) = meta.gl_handle {
                    unsafe {
                        let info = gl
                            .get_active_attribute(ph, index)
                            .map(|it| (it.name, it.size, it.atype));
                        let _ = resp.send(Ok(info));
                    }
                } else {
                    let _ = resp.send(Ok(None));
                }
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::GetActiveUniform {
                canvas_id,
                program_id,
                index,
                resp,
            } => {
                cm.make_current_needed(canvas_id)?;
                let meta = cm.programs.get(&program_id).ok_or_else(|| {
                    ee(
                        ErrorCode::NotFound,
                        format!("program not found: {program_id:?}"),
                    )
                })?;
                if let Err(e) = cm.check_owner(meta.owner_canvas, canvas_id, "program") {
                    let _ = resp.send(Err(e));
                    return Ok(DamageEffect::NoDamage);
                }
                if meta.deleted {
                    let _ = resp.send(Ok(None));
                    return Ok(DamageEffect::NoDamage);
                }

                if let Some(ph) = meta.gl_handle {
                    unsafe {
                        let info = gl
                            .get_active_uniform(ph, index)
                            .map(|it| (it.name, it.size, it.utype));
                        let _ = resp.send(Ok(info));
                    }
                } else {
                    let _ = resp.send(Ok(None));
                }
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::EnableVertexAttribArray { canvas_id, index } => {
                cm.make_current_needed(canvas_id)?;
                let state = cm.gl_state.entry(canvas_id).or_default();
                if st::update_enable_vertex_attrib(state, index) {
                    unsafe { gl.enable_vertex_attrib_array(index) };
                }
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::VertexAttribPointer {
                canvas_id,
                index,
                size,
                type_,
                normalized,
                stride,
                offset,
            } => {
                cm.make_current_needed(canvas_id)?;
                let state = cm.gl_state.entry(canvas_id).or_default();
                if st::update_vertex_attrib_pointer(
                    state, index, size, type_, normalized, stride, offset,
                ) {
                    // Hot path: Cocos can issue this hundreds/thousands of
                    // times per second after scene switches.  A plain `trace!`
                    // produced multi-megabyte logcat floods when TRACE was
                    // enabled for render debugging.  Keep a sampled trace so
                    // operator can still confirm the state tracker is active.
                    shared::trace_rate_limited!(
                        std::time::Duration::from_secs(1),
                        "VertexAttribPointer(sampled): canvas={:?}, index={}, size={}, type={}, norm={}, stride={}, offset={}",
                        canvas_id,
                        index,
                        size,
                        type_,
                        normalized,
                        stride,
                        offset
                    );
                    unsafe {
                        gl.vertex_attrib_pointer_f32(
                            index, size, type_, normalized, stride, offset,
                        );
                    }
                }
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::VertexAttrib4f {
                canvas_id,
                index,
                x,
                y,
                z,
                w,
            } => {
                cm.make_current_needed(canvas_id)?;
                unsafe { gl.vertex_attrib_4_f32(index, x, y, z, w) };
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::VertexAttribI4i {
                canvas_id,
                index,
                x,
                y,
                z,
                w,
            } => {
                cm.make_current_needed(canvas_id)?;
                unsafe { gl.vertex_attrib_4_i32(index, x, y, z, w) };
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::VertexAttribI4ui {
                canvas_id,
                index,
                x,
                y,
                z,
                w,
            } => {
                cm.make_current_needed(canvas_id)?;
                unsafe { gl.vertex_attrib_4_u32(index, x, y, z, w) };
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::VertexAttribIPointer {
                canvas_id,
                index,
                size,
                type_,
                stride,
                offset,
            } => {
                cm.make_current_needed(canvas_id)?;
                let state = cm.gl_state.entry(canvas_id).or_default();
                if st::update_vertex_attrib_ipointer(state, index, size, type_, stride, offset) {
                    unsafe {
                        gl.vertex_attrib_pointer_i32(index, size, type_, stride, offset);
                    }
                }
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::GetUniformLocation {
                canvas_id,
                program_id,
                name,
                resp,
            } => {
                cm.make_current_needed(canvas_id)?;
                let meta = cm.programs.get(&program_id).ok_or_else(|| {
                    ee(
                        ErrorCode::NotFound,
                        format!("program not found: {program_id:?}"),
                    )
                })?;
                if let Err(e) = cm.check_owner(meta.owner_canvas, canvas_id, "program") {
                    let _ = resp.send(Err(e));
                    return Ok(DamageEffect::NoDamage);
                }
                if meta.deleted {
                    let _ = resp.send(Ok(None));
                    return Ok(DamageEffect::NoDamage);
                }

                if let Some(ph) = meta.gl_handle {
                    unsafe {
                        let loc = gl.get_uniform_location(ph, &name);
                        let raw = loc.map(|l| l.0); // NativeUniformLocation(pub u32)
                        let _ = resp.send(Ok(raw));
                    }
                } else {
                    let _ = resp.send(Ok(None));
                }
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::Uniform3f {
                canvas_id,
                location,
                x,
                y,
                z,
            } => {
                cm.make_current_needed(canvas_id)?;
                let v = [x, y, z];
                if should_issue_uniform(cm, canvas_id, location, 1, bytemuck::bytes_of(&v)) {
                    unsafe {
                        gl.uniform_3_f32(to_native_uniform_location(location).as_ref(), x, y, z)
                    };
                }
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::UniformMatrix3fv {
                canvas_id,
                location,
                transpose,
                value,
            } => {
                cm.make_current_needed(canvas_id)?;
                let mut scratch = SmallVec::<[u8; 65]>::new();
                let bytes = mat_uniform_bytes(&mut scratch, transpose, &value);
                if should_issue_uniform(
                    cm,
                    canvas_id,
                    location,
                    (value.len() / 9 * 3) as u32,
                    bytes,
                ) {
                    unsafe {
                        gl.uniform_matrix_3_f32_slice(
                            to_native_uniform_location(location).as_ref(),
                            transpose,
                            &value,
                        )
                    };
                }
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::DrawArrays {
                canvas_id,
                mode,
                first,
                count,
            } => {
                cm.make_current_needed(canvas_id)?;
                self.maybe_log_draw_state(gl, canvas_id, mode, count);
                unsafe { gl.draw_arrays(mode, first, count) };
                crate::render_diagnostics::bump_draw_call_shaped(
                    crate::render_frame_state::DrawShape {
                        mode,
                        elements: false,
                        start: first,
                        len: count,
                    },
                );
                Ok(Self::damage_for_draw(cm, canvas_id))
            }

            GLCmd::DrawElements {
                canvas_id,
                mode,
                count,
                index_type,
                offset,
            } => {
                cm.make_current_needed(canvas_id)?;
                self.maybe_log_draw_state(gl, canvas_id, mode, count);
                unsafe { gl.draw_elements(mode, count, index_type, offset) };
                crate::render_diagnostics::bump_draw_call_shaped(
                    crate::render_frame_state::DrawShape {
                        mode,
                        elements: true,
                        // `offset` is a *byte* offset into the index buffer, but
                        // `count` is a number of indices. Contiguity has to be
                        // tested in one unit, so convert: a draw of 6 shorts
                        // ending at byte 12 is continued by one starting at
                        // byte 12, which is index 6.
                        start: crate::render_frame_state::indices_from_byte_offset(
                            offset, index_type,
                        ),
                        len: count,
                    },
                );
                Ok(Self::damage_for_draw(cm, canvas_id))
            }

            // ---------- Buffers (stateful) ----------
            GLCmd::BindBuffer {
                canvas_id,
                target,
                buffer,
            } => {
                cm.make_current_needed(canvas_id)?;
                // Validate resource BEFORE dedup check — errors must not be swallowed.
                let native = if let Some(id) = buffer {
                    let meta = cm.buffers.get(&id).ok_or_else(|| {
                        ee(ErrorCode::NotFound, format!("buffer not found: {id:?}"))
                    })?;
                    cm.check_owner(meta.owner_canvas, canvas_id, "buffer")?;
                    if meta.deleted {
                        shared::bail!(ErrorCode::InvalidOperation, "bind_buffer on deleted buffer");
                    }
                    meta.gl_handle
                } else {
                    None
                };
                // State deduplication — skip GL call if already bound. Updated
                // AFTER validation so invalid binds never pollute state.
                // Covers every WebGL2 buffer target (UNIFORM_BUFFER,
                // PIXEL_UNPACK_BUFFER, COPY_READ_BUFFER, ...), not just the
                // two GLES2-era ones a prior inline version tracked.
                if st::update_bind_buffer(cm.gl_state.entry(canvas_id).or_default(), target, buffer)
                {
                    unsafe { gl.bind_buffer(target, native) };
                }
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::BufferData {
                canvas_id,
                target,
                size,
                data,
                usage,
            } => {
                cm.make_current_needed(canvas_id)?;
                let bound_buffer = cm.gl_state.get(&canvas_id).and_then(|state| {
                    match target {
                        glow::ARRAY_BUFFER => state.bound_array_buffer,
                        glow::ELEMENT_ARRAY_BUFFER => state.bound_element_array_buffer,
                        glow::UNIFORM_BUFFER => state.bound_uniform_buffer,
                        glow::PIXEL_UNPACK_BUFFER => state.bound_pixel_unpack_buffer,
                        glow::PIXEL_PACK_BUFFER => state.bound_pixel_pack_buffer,
                        glow::COPY_READ_BUFFER => state.bound_copy_read_buffer,
                        glow::COPY_WRITE_BUFFER => state.bound_copy_write_buffer,
                        glow::TRANSFORM_FEEDBACK_BUFFER => state.bound_transform_feedback_buffer,
                        _ => None,
                    }
                    .flatten()
                });
                let prepared = bound_buffer
                    .filter(|_| size >= 0 || data.is_some())
                    .map(|buffer| {
                        let bytes = data
                            .as_ref()
                            .map_or_else(|| u64::try_from(size).unwrap_or(0), |v| v.len() as u64);
                        cm.webgl_gpu_budget
                            .prepare_buffer_data(canvas_id, buffer, bytes)
                            .map_err(gpu_allocation_error)
                    })
                    .transpose()?;
                unsafe {
                    if let Some(data) = data {
                        if data.is_empty() {
                            gl.buffer_data_size(target, 0, usage);
                        } else {
                            gl.buffer_data_u8_slice(target, &data, usage);
                        }
                    } else {
                        gl.buffer_data_size(target, size, usage);
                    }
                }
                if let Some(prepared) = prepared {
                    cm.webgl_gpu_budget.commit(prepared);
                }
                Ok(DamageEffect::NoDamage)
            }

            // ---------- Context-less-ish calls (need some current context) ----------
            // Program
            GLCmd::CreateProgram {
                canvas_id,
                client_id,
            } => {
                cm.make_current_needed(canvas_id)?;
                let owner = Self::current_owner_canvas(cm);

                unsafe {
                    match gl.create_program() {
                        Ok(p) => {
                            // Hint that we may retrieve the binary later for caching.
                            // Without this, many drivers (notably Mali) return empty
                            // from glGetProgramBinary.
                            cm.set_program_binary_hint(p);
                            cm.programs.insert(
                                client_id,
                                crate::canvas::ProgramMeta {
                                    gl_handle: Some(p),
                                    owner_canvas: owner,
                                    deleted: false,
                                    attached_shaders: Vec::new(),
                                    attrib_bindings: Vec::new(),
                                    link_pending: false,
                                },
                            );
                        }
                        Err(e) => {
                            tracing::error!("gl.create_program failed for id {client_id}: {e:?}");
                        }
                    }
                }
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::LinkProgram { program_id } => {
                let _ = self.bind_for_contextless_gl(cm)?;
                if let Some(meta) = cm.programs.get(&program_id) {
                    if !meta.deleted {
                        if let Some(ph) = meta.gl_handle {
                            let (vsrc, fsrc) = Self::get_program_shader_sources(cm, meta);
                            let attrib_key = Self::attribute_key(&meta.attrib_bindings);
                            let (tf_varyings, tf_buffer_mode) = self
                                .tf_descriptors
                                .get(&program_id)
                                .map(|(varyings, mode)| (varyings.as_slice(), Some(*mode)))
                                .unwrap_or((&[], Some(glow::INTERLEAVED_ATTRIBS)));
                            let cache_hit = match (&cm.shader_cache, &vsrc, &fsrc) {
                                (Some(cache), Some(vs), Some(fs)) => {
                                    let descriptor = LinkDescriptor {
                                        vertex_src: vs,
                                        fragment_src: fs,
                                        attrib_key: &attrib_key,
                                        tf_varyings,
                                        tf_buffer_mode,
                                    };
                                    match cache.load(&descriptor) {
                                        Some((format, buffer)) => {
                                            let prog_binary =
                                                glow::ProgramBinary { format, buffer };
                                            unsafe { gl.program_binary(ph, &prog_binary) };
                                            unsafe { gl.get_program_link_status(ph) }
                                        }
                                        None => false,
                                    }
                                }
                                _ => false,
                            };

                            if !cache_hit {
                                unsafe { gl.link_program(ph) };
                                // Deliberately NOT reading LINK_STATUS here.
                                // That read is what blocks on the driver's
                                // compile, and reading it now serialises a
                                // startup burst of links into one-at-a-time.
                                // The queue is drained at the end of the batch
                                // (or earlier if the content asks), which is
                                // where the cache save happens instead.
                                // See `renderergl::link_queue`.
                                cm.mark_link_pending(program_id);
                                if !shared::feature_policy::is_enabled(
                                    shared::feature_policy::FeatureKey::WebglParallelShaderCompile,
                                ) {
                                    // Kill switch: resolve immediately, which
                                    // restores the old one-link-at-a-time
                                    // behaviour exactly, for a driver where
                                    // deferring turns out to misbehave.
                                    Self::drain_pending_link(cm, gl, program_id);
                                }
                            }
                        }
                    }
                }
                // The link reset this program's uniforms driver-side, so drop
                // what we shadowed for it — see
                // `state_tracker::invalidate_program_uniforms`. Both branches
                // above reset them: GLES 3.0 §2.11.4 defines `ProgramBinary` to
                // behave as a successful link, and a *failed* re-link discards
                // the previous executable rather than restoring it, so there is
                // no outcome where the old values survive.
                //
                // Every canvas, because programs are shared across the EGL
                // share group (ES 3.0 Appendix C.1) — the canvas that re-links
                // need not be the one that cached the uniform. Same reasoning
                // as the `DeleteTexture` sweep below.
                for state in cm.gl_state.values_mut() {
                    st::invalidate_program_uniforms(state, program_id);
                }
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::BindAttribLocation {
                program_id,
                index,
                name,
            } => {
                let _ = self.bind_for_contextless_gl(cm)?;
                if let Some(meta) = cm.programs.get_mut(&program_id) {
                    if !meta.deleted {
                        if let Some(ph) = meta.gl_handle {
                            unsafe { gl.bind_attrib_location(ph, index, &name) };
                        }
                        // Keep one binding per attribute name.  Different names
                        // sharing an index are distinct link inputs.
                        meta.attrib_bindings
                            .retain(|(_, existing)| existing != &name);
                        meta.attrib_bindings.push((index, name));
                    }
                }
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::GetProgramParameter {
                program_id,
                pname,
                resp,
            } => {
                let _ = self.bind_for_contextless_gl(cm)?;
                let Some(meta) = cm.programs.get(&program_id) else {
                    let _ = resp.send(Err(ee(
                        ErrorCode::NotFound,
                        format!("program not found: {program_id:?}"),
                    )));
                    return Ok(DamageEffect::NoDamage);
                };

                if meta.deleted {
                    let _ = resp.send(Ok(0));
                    return Ok(DamageEffect::NoDamage);
                }

                let Some(ph) = meta.gl_handle else {
                    let _ = resp.send(Ok(0));
                    return Ok(DamageEffect::NoDamage);
                };

                if pname == glow::LINK_STATUS || pname == glow::INFO_LOG_LENGTH {
                    // The answer is only available by stalling, so this is the
                    // moment the deferred read has to happen -- and the cache
                    // save that rides along with it.
                    Self::drain_pending_link(cm, gl, program_id);
                }

                let v: i32 = unsafe {
                    match pname {
                        glow::LINK_STATUS => {
                            if gl.get_program_link_status(ph) {
                                1
                            } else {
                                0
                            }
                        }
                        glow::INFO_LOG_LENGTH => gl.get_program_info_log(ph).len() as i32,
                        _ => gl.get_program_parameter_i32(ph, pname),
                    }
                };

                let _ = resp.send(Ok(v));
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::GetProgramInfoLog { program_id, resp } => {
                let _ = self.bind_for_contextless_gl(cm)?;
                // An info log is only meaningful once the link has finished.
                Self::drain_pending_link(cm, gl, program_id);
                if let Some(meta) = cm.programs.get(&program_id) {
                    if meta.deleted {
                        let _ = resp.send(Ok(None));
                        return Ok(DamageEffect::NoDamage);
                    }
                    if let Some(ph) = meta.gl_handle {
                        unsafe {
                            let log = gl.get_program_info_log(ph);
                            let _ = resp.send(Ok(Some(log)));
                        }
                    } else {
                        let _ = resp.send(Ok(None));
                    }
                } else {
                    let _ = resp.send(Err(ee(
                        ErrorCode::NotFound,
                        format!("program not found: {program_id:?}"),
                    )));
                }
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::DeleteProgram { program_id } => {
                self.tf_descriptors.remove(&program_id);
                cm.forget_pending_link(program_id);
                if let Some(mut meta) = cm.programs.remove(&program_id) {
                    // Drop the binding shadow and this program's cached uniform
                    // values, so a client that reuses the name does not inherit
                    // the dead program's dedup state.
                    //
                    // Every canvas, not `meta.owner_canvas`: a program created
                    // on the resource context has `owner_canvas == None`, which
                    // reached no canvas at all, and programs are shared across
                    // the EGL share group anyway so any canvas in the group may
                    // have made this one current.
                    for state in cm.gl_state.values_mut() {
                        st::forget_deleted_program(state, program_id);
                    }
                    meta.deleted = true;
                    if let Some(ph) = meta.gl_handle {
                        cm.delete_gl_object(GlObject::Program(ph))?;
                    }
                }
                Ok(DamageEffect::NoDamage)
            }

            // Shader
            GLCmd::CreateShader {
                canvas_id,
                client_id,
                shader_type,
            } => {
                cm.make_current_needed(canvas_id)?;
                let owner = Self::current_owner_canvas(cm);

                let gl_ty = match shader_type {
                    ShaderType::Vertex => glow::VERTEX_SHADER,
                    ShaderType::Fragment => glow::FRAGMENT_SHADER,
                };

                unsafe {
                    match gl.create_shader(gl_ty) {
                        Ok(s) => {
                            cm.shaders.insert(
                                client_id,
                                crate::canvas::ShaderMeta {
                                    gl_handle: Some(s),
                                    owner_canvas: owner,
                                    shader_type,
                                    gl_shader_type: gl_ty,
                                    deleted: false,
                                    source_len: 0,
                                    source: None,
                                },
                            );
                        }
                        Err(e) => {
                            tracing::error!("gl.create_shader failed for id {client_id}: {e:?}");
                        }
                    }
                }
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::ShaderSource {
                shader_id,
                source,
                resp,
            } => {
                let _ = self.bind_for_contextless_gl(cm)?;
                let Some(meta) = cm.shaders.get_mut(&shader_id) else {
                    if let Some(r) = resp {
                        r.send(Err(ee(
                            ErrorCode::NotFound,
                            format!("shader not found: {shader_id:?}"),
                        )));
                    }
                    return Ok(DamageEffect::NoDamage);
                };

                if meta.deleted {
                    if let Some(r) = resp {
                        r.send(Err(ee(
                            ErrorCode::InvalidOperation,
                            "shader already deleted",
                        )));
                    }
                    return Ok(DamageEffect::NoDamage);
                }

                if let Some(sh) = meta.gl_handle {
                    meta.source_len = source.len();
                    meta.source = Some(source.clone());
                    unsafe { gl.shader_source(sh, &source) };
                    if let Some(r) = resp {
                        r.send(Ok(()));
                    }
                } else if let Some(r) = resp {
                    r.send(Err(ee(
                        ErrorCode::InvalidOperation,
                        "shader handle missing",
                    )));
                }
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::CompileShader { shader_id } => {
                let _ = self.bind_for_contextless_gl(cm)?;
                if let Some(meta) = cm.shaders.get(&shader_id) {
                    if !meta.deleted {
                        if let Some(sh) = meta.gl_handle {
                            unsafe { gl.compile_shader(sh) };
                        }
                    }
                }
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::AttachShader {
                program_id,
                shader_id,
                resp,
            } => {
                let _ = self.bind_for_contextless_gl(cm)?;
                let p = cm.programs.get(&program_id).ok_or_else(|| {
                    ee(
                        ErrorCode::NotFound,
                        format!("program not found: {program_id:?}"),
                    )
                })?;
                let s = cm.shaders.get(&shader_id).ok_or_else(|| {
                    ee(
                        ErrorCode::NotFound,
                        format!("shader not found: {shader_id:?}"),
                    )
                })?;

                if p.deleted || s.deleted {
                    if let Some(r) = resp {
                        r.send(Err(ee(
                            ErrorCode::InvalidOperation,
                            "program/shader deleted",
                        )));
                    }
                    return Ok(DamageEffect::NoDamage);
                }

                // WebGL-ish: must belong to same owner canvas
                if p.owner_canvas != s.owner_canvas {
                    if let Some(r) = resp {
                        r.send(Err(ee(
                            ErrorCode::InvalidOperation,
                            "attach shader across different contexts",
                        )));
                    }
                    return Ok(DamageEffect::NoDamage);
                }

                if let (Some(ph), Some(sh)) = (p.gl_handle, s.gl_handle) {
                    unsafe { gl.attach_shader(ph, sh) };
                    // Track attachment for shader cache key lookup at link time.
                    if let Some(pm) = cm.programs.get_mut(&program_id) {
                        if !pm.attached_shaders.contains(&shader_id) {
                            pm.attached_shaders.push(shader_id);
                        }
                    }
                    if let Some(r) = resp {
                        r.send(Ok(()));
                    }
                } else if let Some(r) = resp {
                    r.send(Err(ee(
                        ErrorCode::InvalidOperation,
                        "program/shader handle missing",
                    )));
                }
                Ok(DamageEffect::NoDamage)
            }

            // The facade refuses a shader that is not attached; one that the render side does not know (its program or
            // shader was never created, or is gone) is nothing to detach.
            GLCmd::DetachShader {
                program_id,
                shader_id,
            } => {
                let _ = self.bind_for_contextless_gl(cm)?;
                let handles = cm
                    .programs
                    .get(&program_id)
                    .and_then(|p| p.gl_handle)
                    .zip(cm.shaders.get(&shader_id).and_then(|s| s.gl_handle));
                if let Some((ph, sh)) = handles {
                    unsafe { gl.detach_shader(ph, sh) };
                }
                // The shader-cache key at link time is the sources attached now.
                if let Some(pm) = cm.programs.get_mut(&program_id) {
                    pm.attached_shaders
                        .retain(|attached| *attached != shader_id);
                }
                Ok(DamageEffect::NoDamage)
            }
            GLCmd::ValidateProgram { program_id } => {
                let _ = self.bind_for_contextless_gl(cm)?;
                if let Some(ph) = cm.programs.get(&program_id).and_then(|p| p.gl_handle) {
                    unsafe { gl.validate_program(ph) };
                }
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::GetShaderParameter {
                shader_id,
                pname,
                resp,
            } => {
                let _ = self.bind_for_contextless_gl(cm)?;
                let Some(meta) = cm.shaders.get(&shader_id) else {
                    let _ = resp.send(Err(ee(
                        ErrorCode::NotFound,
                        format!("shader not found: {shader_id:?}"),
                    )));
                    return Ok(DamageEffect::NoDamage);
                };

                if meta.deleted {
                    let _ = resp.send(Ok(0));
                    return Ok(DamageEffect::NoDamage);
                }

                let Some(sh) = meta.gl_handle else {
                    let _ = resp.send(Ok(0));
                    return Ok(DamageEffect::NoDamage);
                };

                let v: i32 = match pname {
                    glow::COMPILE_STATUS => unsafe {
                        if gl.get_shader_compile_status(sh) {
                            1
                        } else {
                            0
                        }
                    },
                    glow::SHADER_TYPE => meta.gl_shader_type as i32,
                    glow::DELETE_STATUS => {
                        if meta.deleted {
                            1
                        } else {
                            0
                        }
                    }
                    glow::INFO_LOG_LENGTH => unsafe { gl.get_shader_info_log(sh).len() as i32 },
                    glow::SHADER_SOURCE_LENGTH => meta.source_len as i32,

                    _ => {
                        let _ = resp.send(Err(ee(
                            ErrorCode::InvalidArgument,
                            format!("GetShaderParameter: unsupported pname=0x{pname:04x}"),
                        )));
                        return Ok(DamageEffect::NoDamage);
                    }
                };

                let _ = resp.send(Ok(v));
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::GetShaderInfoLog { shader_id, resp } => {
                let _ = self.bind_for_contextless_gl(cm)?;
                if let Some(meta) = cm.shaders.get(&shader_id) {
                    if meta.deleted {
                        let _ = resp.send(Ok(None));
                        return Ok(DamageEffect::NoDamage);
                    }
                    if let Some(sh) = meta.gl_handle {
                        unsafe {
                            let log = gl.get_shader_info_log(sh);
                            let _ = resp.send(Ok(Some(log)));
                        }
                    } else {
                        let _ = resp.send(Ok(None));
                    }
                }
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::DeleteShader { shader_id } => {
                if let Some(mut meta) = cm.shaders.remove(&shader_id) {
                    meta.deleted = true;
                    if let Some(sh) = meta.gl_handle {
                        cm.delete_gl_object(GlObject::Shader(sh))?;
                    }
                }
                Ok(DamageEffect::NoDamage)
            }

            // Buffers
            GLCmd::CreateBuffer {
                canvas_id,
                client_id,
            } => {
                cm.make_current_needed(canvas_id)?;
                let owner = Self::current_owner_canvas(cm);

                unsafe {
                    match gl.create_buffer() {
                        Ok(buf) => {
                            let owner = owner.ok_or_else(|| {
                                ee(
                                    ErrorCode::InvalidOperation,
                                    "WebGL buffer has no owning context",
                                )
                            })?;
                            if let Err(error) = cm.webgl_gpu_budget.create_buffer(owner, client_id)
                            {
                                gl.delete_buffer(buf);
                                return Err(gpu_allocation_error(error));
                            }
                            cm.buffers.insert(
                                client_id,
                                crate::canvas::BufferMeta {
                                    gl_handle: Some(buf),
                                    owner_canvas: Some(owner),
                                    deleted: false,
                                },
                            );
                        }
                        Err(e) => {
                            tracing::error!("gl.create_buffer failed for id {client_id}: {e:?}");
                        }
                    }
                }
                Ok(DamageEffect::NoDamage)
            }

            // ========== Phase 1A: GL State ==========
            GLCmd::Enable { canvas_id, cap } => {
                cm.make_current_needed(canvas_id)?;
                // Issue the GL call only if this cap was not already known-enabled.
                // Scissor state still needs updating on the first real Enable so
                // the damage tracker sees a valid ScissorState::Enabled.
                let should_issue =
                    st::update_enable(cm.gl_state.entry(canvas_id).or_default(), cap);
                if should_issue {
                    unsafe { gl.enable(cap) };
                }
                if cap == glow::SCISSOR_TEST {
                    let s = cm.gl_state.entry(canvas_id).or_default();
                    s.scissor = match s.last_scissor_rect {
                        Some((x, y, w, h)) => ScissorState::Enabled {
                            x,
                            y,
                            width: w,
                            height: h,
                        },
                        None => ScissorState::EnabledUnknownRect,
                    };
                }
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::Disable { canvas_id, cap } => {
                cm.make_current_needed(canvas_id)?;
                let should_issue =
                    st::update_disable(cm.gl_state.entry(canvas_id).or_default(), cap);
                if should_issue {
                    unsafe { gl.disable(cap) };
                }
                if cap == glow::SCISSOR_TEST {
                    cm.gl_state.entry(canvas_id).or_default().scissor = ScissorState::Disabled;
                }
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::GetParameter {
                canvas_id,
                pname,
                resp,
            } => {
                cm.make_current_needed(canvas_id)?;
                let json = unsafe {
                    match pname {
                        // String params: VENDOR / RENDERER / VERSION / SHADING_LANGUAGE_VERSION
                        glow::VENDOR
                        | glow::RENDERER
                        | glow::VERSION
                        | glow::SHADING_LANGUAGE_VERSION => {
                            let val = gl.get_parameter_string(pname);
                            format!("\"{}\"", val)
                        }
                        // Boolean params
                        glow::DEPTH_WRITEMASK
                        | glow::SAMPLE_COVERAGE_INVERT
                        | glow::DITHER
                        | glow::BLEND
                        | glow::CULL_FACE
                        | glow::DEPTH_TEST
                        | glow::POLYGON_OFFSET_FILL
                        | glow::SAMPLE_ALPHA_TO_COVERAGE
                        | glow::SAMPLE_COVERAGE
                        | glow::SCISSOR_TEST
                        | glow::STENCIL_TEST => {
                            let val = gl.get_parameter_i32(pname) != 0;
                            if val {
                                "true".to_string()
                            } else {
                                "false".to_string()
                            }
                        }
                        // Float params
                        glow::DEPTH_CLEAR_VALUE
                        | glow::LINE_WIDTH
                        | glow::POLYGON_OFFSET_FACTOR
                        | glow::POLYGON_OFFSET_UNITS
                        | glow::SAMPLE_COVERAGE_VALUE => {
                            let val = gl.get_parameter_f32(pname);
                            format!("{}", val)
                        }
                        // Float32Array[4] params
                        glow::COLOR_CLEAR_VALUE | glow::BLEND_COLOR => {
                            let mut buf = [0f32; 4];
                            gl.get_parameter_f32_slice(pname, &mut buf);
                            format!("[{},{},{},{}]", buf[0], buf[1], buf[2], buf[3])
                        }
                        // Int32Array[4] params
                        glow::VIEWPORT | glow::SCISSOR_BOX => {
                            let mut buf = [0i32; 4];
                            gl.get_parameter_i32_slice(pname, &mut buf);
                            format!("[{},{},{},{}]", buf[0], buf[1], buf[2], buf[3])
                        }
                        // Boolean[4] param
                        glow::COLOR_WRITEMASK => {
                            let mut buf = [0i32; 4];
                            gl.get_parameter_i32_slice(pname, &mut buf);
                            format!(
                                "[{},{},{},{}]",
                                buf[0] != 0,
                                buf[1] != 0,
                                buf[2] != 0,
                                buf[3] != 0
                            )
                        }
                        // Float32Array[2] params
                        glow::DEPTH_RANGE
                        | glow::ALIASED_LINE_WIDTH_RANGE
                        | glow::ALIASED_POINT_SIZE_RANGE => {
                            let mut buf = [0f32; 2];
                            gl.get_parameter_f32_slice(pname, &mut buf);
                            format!("[{},{}]", buf[0], buf[1])
                        }
                        // Default: integer params (MAX_TEXTURE_SIZE, MAX_VERTEX_ATTRIBS, etc.)
                        _ => {
                            let val = gl.get_parameter_i32(pname);
                            format!("{}", val)
                        }
                    }
                };
                let _ = resp.send(Ok(json));
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::GetState {
                canvas_id,
                query,
                pname,
                extra,
                name,
                resp,
            } => {
                cm.make_current_needed(canvas_id)?;
                let json = match query {
                    shared::protocol::render_cmd::gl_state::INTERNALFORMAT_SAMPLES => {
                        internalformat_samples_json(gl, pname, extra)
                    }
                    shared::protocol::render_cmd::gl_state::FRAMEBUFFER_ATTACHMENT_PARAMETER => {
                        // The queried pname travels as decimal text in `name`: three numbers do not fit the
                        // two a state query has.
                        let queried = name.trim().parse::<u32>().unwrap_or(0);
                        unsafe {
                            gl.get_framebuffer_attachment_parameter_i32(pname, extra, queried)
                        }
                        .to_string()
                    }
                    shared::protocol::render_cmd::gl_state::ACTIVE_UNIFORM_BLOCK_NAME
                    | shared::protocol::render_cmd::gl_state::ACTIVE_UNIFORM_BLOCK_PARAMETER
                    | shared::protocol::render_cmd::gl_state::UNIFORM_INDICES
                    | shared::protocol::render_cmd::gl_state::ACTIVE_UNIFORMS_PARAMETER
                    | shared::protocol::render_cmd::gl_state::UNIFORM_VALUE
                    | shared::protocol::render_cmd::gl_state::FRAG_DATA_LOCATION => {
                        // `pname` is the program's resource id; a program that is gone or never got a GL handle has
                        // nothing to say, which is the specification's INVALID_OPERATION.
                        match cm.programs.get(&pname).and_then(|meta| meta.gl_handle) {
                            Some(handle) => {
                                super::program_state::answer(gl, handle, query, extra, &name)
                            }
                            None => "{\"e\":1282}".to_string(),
                        }
                    }
                    // The wire refuses a number it does not know before it gets here.
                    _ => "null".to_string(),
                };
                let _ = resp.send(Ok(json));
                Ok(DamageEffect::NoDamage)
            }

            // ========== Phase 1B: Textures ==========
            GLCmd::CreateTexture {
                canvas_id,
                client_id,
            } => {
                cm.make_current_needed(canvas_id)?;
                let owner = Self::current_owner_canvas(cm);
                unsafe {
                    match gl.create_texture() {
                        Ok(tex) => {
                            let owner = owner.ok_or_else(|| {
                                ee(
                                    ErrorCode::InvalidOperation,
                                    "WebGL texture has no owning context",
                                )
                            })?;
                            if let Err(error) = cm.webgl_gpu_budget.create_texture(owner, client_id)
                            {
                                gl.delete_texture(tex);
                                return Err(gpu_allocation_error(error));
                            }
                            cm.textures.insert(
                                client_id,
                                crate::canvas::TextureMeta {
                                    gl_handle: Some(tex),
                                    owner_canvas: Some(owner),
                                    deleted: false,
                                },
                            );
                        }
                        Err(e) => {
                            tracing::error!("gl.create_texture failed for id {client_id}: {e:?}")
                        }
                    }
                }
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::DeleteTexture { texture_id } => {
                if let Some(meta) = cm.textures.remove(&texture_id) {
                    // Invalidate dedup state: per GL spec, deleting a texture
                    // implicitly unbinds it from all units.  Clear matching
                    // entries so the next BindTexture with the same ID isn't
                    // incorrectly deduped.
                    for state in cm.gl_state.values_mut() {
                        state.bound_texture_2d.forget_texture(texture_id);
                    }
                    if let Some(h) = meta.gl_handle {
                        cm.delete_gl_object(GlObject::Texture(h))?;
                    }
                    cm.webgl_gpu_budget.delete_texture(texture_id);
                }
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::BindTexture {
                canvas_id,
                target,
                texture,
            } => {
                cm.make_current_needed(canvas_id)?;
                // Validate resource BEFORE dedup — errors must not be swallowed.
                let native = if let Some(id) = texture {
                    let meta = cm.textures.get(&id).ok_or_else(|| {
                        ee(ErrorCode::NotFound, format!("texture not found: {id:?}"))
                    })?;
                    if meta.deleted {
                        shared::bail!(
                            ErrorCode::InvalidOperation,
                            "bind_texture on deleted texture"
                        );
                    }
                    meta.gl_handle
                } else {
                    None
                };
                // Per-unit state deduplication for TEXTURE_2D.
                // Updated AFTER validation so invalid binds never pollute state.
                if target != glow::TEXTURE_2D
                    || st::update_bind_texture_2d(
                        cm.gl_state.entry(canvas_id).or_default(),
                        texture,
                    )
                {
                    unsafe { gl.bind_texture(target, native) };
                }
                cm.webgl_gpu_budget.bind_texture(canvas_id, target, texture);
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::ActiveTexture { canvas_id, unit } => {
                cm.make_current_needed(canvas_id)?;
                if !crate::webgl_gpu_budget::is_valid_texture_unit(unit) {
                    // Preserve GL's INVALID_ENUM behavior without poisoning
                    // either state mirror with a unit the driver rejected.
                    unsafe { gl.active_texture(unit) };
                    return Ok(DamageEffect::NoDamage);
                }
                if st::update_active_texture(cm.gl_state.entry(canvas_id).or_default(), unit) {
                    unsafe { gl.active_texture(unit) };
                }
                cm.webgl_gpu_budget.active_texture(canvas_id, unit);
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::TexImage2D {
                canvas_id,
                target,
                level,
                internalformat,
                width,
                height,
                border,
                format,
                type_,
                data,
            } => {
                cm.make_current_needed(canvas_id)?;
                let pixels = match &data {
                    None => glow::PixelUnpackData::Slice(None),
                    Some(source) => unpack_data(
                        gl,
                        source,
                        width,
                        height,
                        1,
                        format,
                        type_,
                        &cm.unpack_layout(canvas_id, false),
                    )?,
                };
                let prepared = cm
                    .webgl_gpu_budget
                    .prepare_tex_image_2d(
                        canvas_id,
                        target,
                        level,
                        internalformat,
                        width,
                        height,
                        border,
                        format,
                        type_,
                    )
                    .map_err(gpu_allocation_error)?;
                // `UNPACK_FLIP_Y_WEBGL` / `UNPACK_PREMULTIPLY_ALPHA_WEBGL` are applied to the bytes here, with the
                // layout the driver reads them with; a borrow when neither is on.
                let converted = match (&pixels, cm.unpack_conversion(canvas_id)) {
                    (glow::PixelUnpackData::Slice(Some(bytes)), Some(state)) => Some(
                        unpack_convert::convert_upload(bytes, width, height, format, type_, &state),
                    ),
                    _ => None,
                };
                let pixels = match &converted {
                    Some(bytes) => glow::PixelUnpackData::Slice(Some(bytes)),
                    None => pixels,
                };
                // Use PBO for large uploads (> 64 KB) to avoid GPU pipeline stalls.
                if let glow::PixelUnpackData::Slice(Some(bytes)) = pixels {
                    if bytes.len() > 65536 {
                        if let Some(pool) = cm.pbo_pool_mut() {
                            if pool.is_pbo_supported() {
                                let effect = Self::tex_image_2d_pbo(
                                    cm,
                                    gl,
                                    target,
                                    level,
                                    internalformat,
                                    width,
                                    height,
                                    border,
                                    format,
                                    type_,
                                    bytes,
                                )?;
                                cm.webgl_gpu_budget.commit(prepared);
                                return Ok(effect);
                            }
                        }
                    }
                }
                unsafe {
                    gl.tex_image_2d(
                        target,
                        level,
                        internalformat,
                        width,
                        height,
                        border,
                        format,
                        type_,
                        pixels,
                    );
                }
                cm.webgl_gpu_budget.commit(prepared);
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::TexImage2DFromShared {
                canvas_id,
                target,
                level,
                internalformat,
                format,
                type_,
                source_shared_id,
                src_width,
                src_height,
            } => {
                cm.make_current_needed(canvas_id)?;
                let prepared = cm
                    .webgl_gpu_budget
                    .prepare_tex_image_2d(
                        canvas_id,
                        target,
                        level,
                        internalformat,
                        src_width,
                        src_height,
                        0,
                        format,
                        type_,
                    )
                    .map_err(gpu_allocation_error)?;
                cm.tex_image_2d_from_shared(
                    canvas_id,
                    target,
                    level,
                    internalformat,
                    source_shared_id,
                    src_width,
                    src_height,
                )?;
                cm.webgl_gpu_budget.commit(prepared);
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::TexImage2DFromSnapshot {
                canvas_id,
                target,
                level,
                internalformat,
                format,
                type_,
                snapshot_id,
            } => {
                cm.make_current_needed(canvas_id)?;
                // The record carries the `format` and `type` the content passed when the facade knows them, and
                // zeros when it does not (the form of `texImage2D(canvas)` that has no `format`/`type` of its
                // own to forward). A snapshot is RGBA8 pixels, so for an unsized internal format the format is
                // the internal format and the type is UNSIGNED_BYTE -- what `TexImage2DFromCanvas2D` already
                // assumes below. Passing the zeros on made the budget refuse the upload (an unsized format must
                // equal its format), and every canvas drawn and uploaded in the same frame -- every dynamic
                // text sprite -- became an empty texture, drawn opaque black.
                let (format, type_) = canvas_source_upload_format(internalformat, format, type_);
                let prepared = cm
                    .canvas2d_snapshot_dimensions(snapshot_id)
                    .map(|(width, height)| {
                        cm.webgl_gpu_budget
                            .prepare_tex_image_2d(
                                canvas_id,
                                target,
                                level,
                                internalformat,
                                width as i32,
                                height as i32,
                                0,
                                format,
                                type_,
                            )
                            .map_err(gpu_allocation_error)
                    })
                    .transpose()?;
                cm.tex_image_2d_from_canvas2d_snapshot(
                    canvas_id,
                    target,
                    level,
                    internalformat,
                    snapshot_id,
                )?;
                if let Some(prepared) = prepared {
                    cm.webgl_gpu_budget.commit(prepared);
                }
                crate::render_diagnostics::bump_canvas2d_snapshot_upload();
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::TexImage2DFromTextCache {
                canvas_id,
                target,
                level,
                internalformat,
                key,
            } => {
                cm.make_current_needed(canvas_id)?;
                let prepared = cm
                    .webgl_gpu_budget
                    .prepare_tex_image_2d(
                        canvas_id,
                        target,
                        level,
                        internalformat,
                        key.canvas_w as i32,
                        key.canvas_h as i32,
                        0,
                        internalformat as u32,
                        glow::UNSIGNED_BYTE,
                    )
                    .map_err(gpu_allocation_error)?;
                let used = cm.tex_image_2d_from_text_cache(
                    canvas_id,
                    target,
                    level,
                    internalformat,
                    &key,
                )?;
                if used {
                    cm.webgl_gpu_budget.commit(prepared);
                    crate::render_diagnostics::hit_text_cache();
                    crate::render_diagnostics::bump_canvas2d_snapshot_upload();
                } else {
                    crate::render_diagnostics::miss_text_cache();
                    tracing::warn!(
                        "TexImage2DFromTextCache: entry missing at execution time \
                         (pin / eviction race?); destination texture unchanged"
                    );
                }
                Ok(DamageEffect::NoDamage)
            }
            GLCmd::TexImage2DFromCanvas2D {
                canvas_id,
                target,
                level,
                internalformat,
                canvas_2d_id,
                x,
                y,
                width,
                height,
            } => {
                cm.make_current_needed(canvas_id)?;
                let prepared = if width == 0 || height == 0 {
                    None
                } else {
                    Some(
                        cm.webgl_gpu_budget
                            .prepare_tex_image_2d(
                                canvas_id,
                                target,
                                level,
                                internalformat,
                                width as i32,
                                height as i32,
                                0,
                                internalformat as u32,
                                glow::UNSIGNED_BYTE,
                            )
                            .map_err(gpu_allocation_error)?,
                    )
                };
                cm.tex_image_2d_from_canvas2d_direct(
                    canvas_id,
                    target,
                    level,
                    internalformat,
                    canvas_2d_id,
                    x,
                    y,
                    width,
                    height,
                )?;
                if let Some(prepared) = prepared {
                    cm.webgl_gpu_budget.commit(prepared);
                }
                crate::render_diagnostics::bump_canvas2d_snapshot_upload();
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::TexSubImage2DFromCanvas2D {
                canvas_id,
                target,
                level,
                xoffset,
                yoffset,
                canvas_2d_id,
                x,
                y,
                width,
                height,
            } => {
                cm.tex_sub_image_2d_from_canvas2d_direct(
                    canvas_id,
                    target,
                    level,
                    xoffset,
                    yoffset,
                    canvas_2d_id,
                    x,
                    y,
                    width,
                    height,
                )?;
                crate::render_diagnostics::bump_canvas2d_snapshot_upload();
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::TexSubImage2DFromSnapshot {
                canvas_id,
                target,
                level,
                xoffset,
                yoffset,
                format,
                type_,
                snapshot_id,
            } => {
                cm.tex_sub_image_2d_from_canvas2d_snapshot(
                    canvas_id,
                    target,
                    level,
                    xoffset,
                    yoffset,
                    (format, type_),
                    snapshot_id,
                )?;
                crate::render_diagnostics::bump_canvas2d_snapshot_upload();
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::TexSubImage2D {
                canvas_id,
                target,
                level,
                xoffset,
                yoffset,
                width,
                height,
                format,
                type_,
                data,
            } => {
                cm.make_current_needed(canvas_id)?;
                let pixels = unpack_data(
                    gl,
                    &data,
                    width,
                    height,
                    1,
                    format,
                    type_,
                    &cm.unpack_layout(canvas_id, false),
                )?;
                // As in `TexImage2D`: the unpack flags are applied here.
                let converted = match (&pixels, cm.unpack_conversion(canvas_id)) {
                    (glow::PixelUnpackData::Slice(Some(bytes)), Some(state)) => Some(
                        unpack_convert::convert_upload(bytes, width, height, format, type_, &state),
                    ),
                    _ => None,
                };
                let pixels = match &converted {
                    Some(bytes) => glow::PixelUnpackData::Slice(Some(bytes)),
                    None => pixels,
                };
                // Use PBO for large sub-image uploads (> 64 KB).
                if let glow::PixelUnpackData::Slice(Some(bytes)) = pixels {
                    if bytes.len() > 65536 {
                        if let Some(pool) = cm.pbo_pool_mut() {
                            if pool.is_pbo_supported() {
                                return Self::tex_sub_image_2d_pbo(
                                    cm, gl, target, level, xoffset, yoffset, width, height, format,
                                    type_, bytes,
                                );
                            }
                        }
                    }
                }
                unsafe {
                    gl.tex_sub_image_2d(
                        target, level, xoffset, yoffset, width, height, format, type_, pixels,
                    );
                }
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::TexParameteri {
                canvas_id,
                target,
                pname,
                param,
            } => {
                cm.make_current_needed(canvas_id)?;
                unsafe { gl.tex_parameter_i32(target, pname, param) };
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::TexParameterf {
                canvas_id,
                target,
                pname,
                param,
            } => {
                cm.make_current_needed(canvas_id)?;
                unsafe { gl.tex_parameter_f32(target, pname, param) };
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::GenerateMipmap { canvas_id, target } => {
                cm.make_current_needed(canvas_id)?;
                let prepared = cm
                    .webgl_gpu_budget
                    .prepare_generate_mipmap(canvas_id, target)
                    .map_err(gpu_allocation_error)?;
                unsafe { gl.generate_mipmap(target) };
                cm.webgl_gpu_budget.commit(prepared);
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::PixelStorei {
                canvas_id,
                pname,
                param,
            } => {
                cm.make_current_needed(canvas_id)?;
                let entry = cm.gl_state.entry(canvas_id).or_default();
                // The WebGL-only ones are state the engine reads when it builds a texture from an
                // image; the driver has no such parameter.
                if st::update_pixel_store_i32(entry, pname, param)
                    && !crate::canvas::is_webgl_only_pixel_store(pname)
                {
                    unsafe { gl.pixel_store_i32(pname, param) };
                }
                Ok(DamageEffect::NoDamage)
            }

            // ---------- Compressed uploads, 2D and 3D ----------
            // The bytes are the call's own, or a range of the bound PIXEL_UNPACK_BUFFER (WebGL 2's other overload).
            // A level the call defines is charged to the GPU budget at the call's image size.
            GLCmd::CompressedTexImage2D {
                canvas_id,
                target,
                level,
                internalformat,
                width,
                height,
                border,
                data,
            } => {
                cm.make_current_needed(canvas_id)?;
                if matches!(data, CompressedImageData::UnpackBuffer { .. }) {
                    refuse_offset_without_unpack_buffer(gl)?;
                }
                let prepared = cm
                    .webgl_gpu_budget
                    .prepare_compressed_tex_image_2d(
                        canvas_id,
                        target,
                        level,
                        internalformat,
                        width,
                        height,
                        border,
                        data.image_size() as u32,
                    )
                    .map_err(gpu_allocation_error)?;
                match &data {
                    CompressedImageData::Bytes(bytes) => unsafe {
                        gl.compressed_tex_image_2d(
                            target,
                            level,
                            internalformat as i32,
                            width,
                            height,
                            border,
                            bytes.len() as i32,
                            bytes,
                        )
                    },
                    CompressedImageData::UnpackBuffer { offset, size } => {
                        let entry = cm.unpack_buffer_compressed_image()?;
                        // SAFETY: a PIXEL_UNPACK_BUFFER is bound for this overload, so GL reads `data` as an offset.
                        unsafe {
                            (entry.tex_image_2d)(
                                target,
                                level,
                                internalformat,
                                width,
                                height,
                                border,
                                *size,
                                *offset as usize as *const std::ffi::c_void,
                            )
                        }
                    }
                }
                cm.webgl_gpu_budget.commit(prepared);
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::CompressedTexSubImage2D {
                canvas_id,
                target,
                level,
                xoffset,
                yoffset,
                width,
                height,
                format,
                data,
            } => {
                cm.make_current_needed(canvas_id)?;
                if matches!(data, CompressedImageData::UnpackBuffer { .. }) {
                    refuse_offset_without_unpack_buffer(gl)?;
                }
                unsafe {
                    gl.compressed_tex_sub_image_2d(
                        target,
                        level,
                        xoffset,
                        yoffset,
                        width,
                        height,
                        format,
                        compressed_unpack_data(&data),
                    );
                }
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::CompressedTexImage3D {
                canvas_id,
                target,
                level,
                internalformat,
                width,
                height,
                depth,
                border,
                data,
            } => {
                cm.make_current_needed(canvas_id)?;
                if matches!(data, CompressedImageData::UnpackBuffer { .. }) {
                    refuse_offset_without_unpack_buffer(gl)?;
                }
                let prepared = cm
                    .webgl_gpu_budget
                    .prepare_compressed_tex_image_3d(
                        canvas_id,
                        target,
                        level,
                        width,
                        height,
                        depth,
                        border,
                        data.image_size() as u32,
                    )
                    .map_err(gpu_allocation_error)?;
                match &data {
                    CompressedImageData::Bytes(bytes) => unsafe {
                        gl.compressed_tex_image_3d(
                            target,
                            level,
                            internalformat as i32,
                            width,
                            height,
                            depth,
                            border,
                            bytes.len() as i32,
                            bytes,
                        )
                    },
                    CompressedImageData::UnpackBuffer { offset, size } => {
                        let entry = cm.unpack_buffer_compressed_image()?;
                        // SAFETY: as for the 2D call.
                        unsafe {
                            (entry.tex_image_3d)(
                                target,
                                level,
                                internalformat,
                                width,
                                height,
                                depth,
                                border,
                                *size,
                                *offset as usize as *const std::ffi::c_void,
                            )
                        }
                    }
                }
                cm.webgl_gpu_budget.commit(prepared);
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::CompressedTexSubImage3D {
                canvas_id,
                target,
                level,
                xoffset,
                yoffset,
                zoffset,
                width,
                height,
                depth,
                format,
                data,
            } => {
                cm.make_current_needed(canvas_id)?;
                if matches!(data, CompressedImageData::UnpackBuffer { .. }) {
                    refuse_offset_without_unpack_buffer(gl)?;
                }
                unsafe {
                    gl.compressed_tex_sub_image_3d(
                        target,
                        level,
                        xoffset,
                        yoffset,
                        zoffset,
                        width,
                        height,
                        depth,
                        format,
                        compressed_unpack_data(&data),
                    );
                }
                Ok(DamageEffect::NoDamage)
            }

            // ========== Phase 1C: Buffer & Vertex Extensions ==========
            GLCmd::BufferSubData {
                canvas_id,
                target,
                offset,
                data,
            } => {
                cm.make_current_needed(canvas_id)?;
                unsafe { gl.buffer_sub_data_u8_slice(target, offset, &data) };
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::DisableVertexAttribArray { canvas_id, index } => {
                cm.make_current_needed(canvas_id)?;
                let state = cm.gl_state.entry(canvas_id).or_default();
                if st::update_disable_vertex_attrib(state, index) {
                    unsafe { gl.disable_vertex_attrib_array(index) };
                }
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::ClearDepth { canvas_id, depth } => {
                cm.make_current_needed(canvas_id)?;
                unsafe { gl.clear_depth_f32(depth) };
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::ClearStencil { canvas_id, s } => {
                cm.make_current_needed(canvas_id)?;
                unsafe { gl.clear_stencil(s) };
                Ok(DamageEffect::NoDamage)
            }

            // ========== Phase 2A: Blend/Depth/Stencil/Cull ==========
            GLCmd::BlendFunc {
                canvas_id,
                sfactor,
                dfactor,
            } => {
                cm.make_current_needed(canvas_id)?;
                if st::update_blend_func(
                    cm.gl_state.entry(canvas_id).or_default(),
                    sfactor,
                    dfactor,
                ) {
                    unsafe { gl.blend_func(sfactor, dfactor) };
                }
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::BlendFuncSeparate {
                canvas_id,
                src_rgb,
                dst_rgb,
                src_alpha,
                dst_alpha,
            } => {
                cm.make_current_needed(canvas_id)?;
                if st::update_blend_func_separate(
                    cm.gl_state.entry(canvas_id).or_default(),
                    src_rgb,
                    dst_rgb,
                    src_alpha,
                    dst_alpha,
                ) {
                    unsafe { gl.blend_func_separate(src_rgb, dst_rgb, src_alpha, dst_alpha) };
                }
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::BlendEquation { canvas_id, mode } => {
                cm.make_current_needed(canvas_id)?;
                if st::update_blend_equation(cm.gl_state.entry(canvas_id).or_default(), mode) {
                    unsafe { gl.blend_equation(mode) };
                }
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::BlendEquationSeparate {
                canvas_id,
                mode_rgb,
                mode_alpha,
            } => {
                cm.make_current_needed(canvas_id)?;
                let entry = cm.gl_state.entry(canvas_id).or_default();
                if st::update_blend_equation_separate(entry, mode_rgb, mode_alpha) {
                    unsafe { gl.blend_equation_separate(mode_rgb, mode_alpha) };
                }
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::BlendColor {
                canvas_id,
                r,
                g,
                b,
                a,
            } => {
                cm.make_current_needed(canvas_id)?;
                if st::update_blend_color(cm.gl_state.entry(canvas_id).or_default(), r, g, b, a) {
                    unsafe { gl.blend_color(r, g, b, a) };
                }
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::DepthFunc { canvas_id, func } => {
                cm.make_current_needed(canvas_id)?;
                if st::update_depth_func(cm.gl_state.entry(canvas_id).or_default(), func) {
                    unsafe { gl.depth_func(func) };
                }
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::DepthMask { canvas_id, flag } => {
                cm.make_current_needed(canvas_id)?;
                if st::update_depth_mask(cm.gl_state.entry(canvas_id).or_default(), flag) {
                    unsafe { gl.depth_mask(flag) };
                }
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::DepthRange {
                canvas_id,
                near,
                far,
            } => {
                cm.make_current_needed(canvas_id)?;
                let entry = cm.gl_state.entry(canvas_id).or_default();
                if st::update_depth_range(entry, near, far) {
                    unsafe { gl.depth_range_f32(near, far) };
                }
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::StencilFunc {
                canvas_id,
                func,
                ref_,
                mask,
            } => {
                cm.make_current_needed(canvas_id)?;
                let entry = cm.gl_state.entry(canvas_id).or_default();
                if st::update_stencil_func(entry, glow::FRONT_AND_BACK, func, ref_, mask) {
                    unsafe { gl.stencil_func(func, ref_, mask) };
                }
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::StencilFuncSeparate {
                canvas_id,
                face,
                func,
                ref_,
                mask,
            } => {
                cm.make_current_needed(canvas_id)?;
                let entry = cm.gl_state.entry(canvas_id).or_default();
                if st::update_stencil_func(entry, face, func, ref_, mask) {
                    unsafe { gl.stencil_func_separate(face, func, ref_, mask) };
                }
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::StencilOp {
                canvas_id,
                fail,
                zfail,
                zpass,
            } => {
                cm.make_current_needed(canvas_id)?;
                let entry = cm.gl_state.entry(canvas_id).or_default();
                if st::update_stencil_op(entry, glow::FRONT_AND_BACK, fail, zfail, zpass) {
                    unsafe { gl.stencil_op(fail, zfail, zpass) };
                }
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::StencilOpSeparate {
                canvas_id,
                face,
                fail,
                zfail,
                zpass,
            } => {
                cm.make_current_needed(canvas_id)?;
                let entry = cm.gl_state.entry(canvas_id).or_default();
                if st::update_stencil_op(entry, face, fail, zfail, zpass) {
                    unsafe { gl.stencil_op_separate(face, fail, zfail, zpass) };
                }
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::StencilMask { canvas_id, mask } => {
                cm.make_current_needed(canvas_id)?;
                let entry = cm.gl_state.entry(canvas_id).or_default();
                if st::update_stencil_mask(entry, glow::FRONT_AND_BACK, mask) {
                    unsafe { gl.stencil_mask(mask) };
                }
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::StencilMaskSeparate {
                canvas_id,
                face,
                mask,
            } => {
                cm.make_current_needed(canvas_id)?;
                let entry = cm.gl_state.entry(canvas_id).or_default();
                if st::update_stencil_mask(entry, face, mask) {
                    unsafe { gl.stencil_mask_separate(face, mask) };
                }
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::CullFace { canvas_id, mode } => {
                cm.make_current_needed(canvas_id)?;
                if st::update_cull_face(cm.gl_state.entry(canvas_id).or_default(), mode) {
                    unsafe { gl.cull_face(mode) };
                }
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::FrontFace { canvas_id, mode } => {
                cm.make_current_needed(canvas_id)?;
                if st::update_front_face(cm.gl_state.entry(canvas_id).or_default(), mode) {
                    unsafe { gl.front_face(mode) };
                }
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::ColorMask {
                canvas_id,
                r,
                g,
                b,
                a,
            } => {
                cm.make_current_needed(canvas_id)?;
                let state = cm.gl_state.entry(canvas_id).or_default();
                if st::update_color_mask(state, r, g, b, a) {
                    unsafe { gl.color_mask(r, g, b, a) };
                }
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::Scissor {
                canvas_id,
                x,
                y,
                width,
                height,
            } => {
                cm.make_current_needed(canvas_id)?;
                let px = logical_to_physical_i32(cm, x);
                let py = logical_to_physical_i32(cm, y);
                let pw = logical_to_physical_i32(cm, width);
                let ph = logical_to_physical_i32(cm, height);
                // Deduped, which it was not before, and the note that used to
                // sit here explained why: the engine re-pointed the driver's box
                // behind this tracker's back on the present path, so a shadow
                // hit could skip a call the driver needed and clip every later
                // draw to the engine's rect. Silent wrong pixels.
                //
                // What changed is that `dirty_region::apply_scissor` and its
                // restore now go through `ScissorBorrow` and write
                // `last_scissor_rect` from the same computation that feeds the
                // driver, and the blit only ever touches the enable bit — which
                // it restores from what it read. Every writer of the driver's box
                // is now visible here. See `state_tracker::update_scissor`.
                if st::update_scissor(cm.gl_state.entry(canvas_id).or_default(), px, py, pw, ph) {
                    unsafe { gl.scissor(px, py, pw, ph) };
                }
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::LineWidth { canvas_id, width } => {
                cm.make_current_needed(canvas_id)?;
                if st::update_line_width(cm.gl_state.entry(canvas_id).or_default(), width) {
                    unsafe { gl.line_width(width) };
                }
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::PolygonOffset {
                canvas_id,
                factor,
                units,
            } => {
                cm.make_current_needed(canvas_id)?;
                if st::update_polygon_offset(
                    cm.gl_state.entry(canvas_id).or_default(),
                    factor,
                    units,
                ) {
                    unsafe { gl.polygon_offset(factor, units) };
                }
                Ok(DamageEffect::NoDamage)
            }

            // ========== Phase 2B: Uniform Variants ==========
            GLCmd::Uniform1i {
                canvas_id,
                location,
                x,
            } => {
                cm.make_current_needed(canvas_id)?;
                if should_issue_uniform(cm, canvas_id, location, 1, bytemuck::bytes_of(&x)) {
                    unsafe { gl.uniform_1_i32(to_native_uniform_location(location).as_ref(), x) };
                }
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::Uniform1f {
                canvas_id,
                location,
                x,
            } => {
                cm.make_current_needed(canvas_id)?;
                if should_issue_uniform(cm, canvas_id, location, 1, bytemuck::bytes_of(&x)) {
                    unsafe { gl.uniform_1_f32(to_native_uniform_location(location).as_ref(), x) };
                }
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::Uniform2f {
                canvas_id,
                location,
                x,
                y,
            } => {
                cm.make_current_needed(canvas_id)?;
                let v = [x, y];
                if should_issue_uniform(cm, canvas_id, location, 1, bytemuck::bytes_of(&v)) {
                    unsafe {
                        gl.uniform_2_f32(to_native_uniform_location(location).as_ref(), x, y)
                    };
                }
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::Uniform4f {
                canvas_id,
                location,
                x,
                y,
                z,
                w,
            } => {
                cm.make_current_needed(canvas_id)?;
                let v = [x, y, z, w];
                if should_issue_uniform(cm, canvas_id, location, 1, bytemuck::bytes_of(&v)) {
                    unsafe {
                        gl.uniform_4_f32(to_native_uniform_location(location).as_ref(), x, y, z, w)
                    };
                }
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::Uniform1iv {
                canvas_id,
                location,
                value,
            } => {
                cm.make_current_needed(canvas_id)?;
                if should_issue_uniform(
                    cm,
                    canvas_id,
                    location,
                    value.len() as u32,
                    bytemuck::cast_slice::<i32, u8>(&value),
                ) {
                    unsafe {
                        gl.uniform_1_i32_slice(
                            to_native_uniform_location(location).as_ref(),
                            &value,
                        )
                    };
                }
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::Uniform1fv {
                canvas_id,
                location,
                value,
            } => {
                cm.make_current_needed(canvas_id)?;
                if should_issue_uniform(
                    cm,
                    canvas_id,
                    location,
                    value.len() as u32,
                    bytemuck::cast_slice::<f32, u8>(&value),
                ) {
                    unsafe {
                        gl.uniform_1_f32_slice(
                            to_native_uniform_location(location).as_ref(),
                            &value,
                        )
                    };
                }
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::Uniform2iv {
                canvas_id,
                location,
                value,
            } => {
                cm.make_current_needed(canvas_id)?;
                if should_issue_uniform(
                    cm,
                    canvas_id,
                    location,
                    (value.len() / 2) as u32,
                    bytemuck::cast_slice::<i32, u8>(&value),
                ) {
                    unsafe {
                        gl.uniform_2_i32_slice(
                            to_native_uniform_location(location).as_ref(),
                            &value,
                        )
                    };
                }
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::Uniform2fv {
                canvas_id,
                location,
                value,
            } => {
                cm.make_current_needed(canvas_id)?;
                if should_issue_uniform(
                    cm,
                    canvas_id,
                    location,
                    (value.len() / 2) as u32,
                    bytemuck::cast_slice::<f32, u8>(&value),
                ) {
                    unsafe {
                        gl.uniform_2_f32_slice(
                            to_native_uniform_location(location).as_ref(),
                            &value,
                        )
                    };
                }
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::Uniform3iv {
                canvas_id,
                location,
                value,
            } => {
                cm.make_current_needed(canvas_id)?;
                if should_issue_uniform(
                    cm,
                    canvas_id,
                    location,
                    (value.len() / 3) as u32,
                    bytemuck::cast_slice::<i32, u8>(&value),
                ) {
                    unsafe {
                        gl.uniform_3_i32_slice(
                            to_native_uniform_location(location).as_ref(),
                            &value,
                        )
                    };
                }
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::Uniform3fv {
                canvas_id,
                location,
                value,
            } => {
                cm.make_current_needed(canvas_id)?;
                if should_issue_uniform(
                    cm,
                    canvas_id,
                    location,
                    (value.len() / 3) as u32,
                    bytemuck::cast_slice::<f32, u8>(&value),
                ) {
                    unsafe {
                        gl.uniform_3_f32_slice(
                            to_native_uniform_location(location).as_ref(),
                            &value,
                        )
                    };
                }
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::Uniform4iv {
                canvas_id,
                location,
                value,
            } => {
                cm.make_current_needed(canvas_id)?;
                if should_issue_uniform(
                    cm,
                    canvas_id,
                    location,
                    (value.len() / 4) as u32,
                    bytemuck::cast_slice::<i32, u8>(&value),
                ) {
                    unsafe {
                        gl.uniform_4_i32_slice(
                            to_native_uniform_location(location).as_ref(),
                            &value,
                        )
                    };
                }
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::Uniform4fv {
                canvas_id,
                location,
                value,
            } => {
                cm.make_current_needed(canvas_id)?;
                if should_issue_uniform(
                    cm,
                    canvas_id,
                    location,
                    (value.len() / 4) as u32,
                    bytemuck::cast_slice::<f32, u8>(&value),
                ) {
                    unsafe {
                        gl.uniform_4_f32_slice(
                            to_native_uniform_location(location).as_ref(),
                            &value,
                        )
                    };
                }
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::UniformMatrix2fv {
                canvas_id,
                location,
                transpose,
                value,
            } => {
                cm.make_current_needed(canvas_id)?;
                let mut scratch = SmallVec::<[u8; 65]>::new();
                let bytes = mat_uniform_bytes(&mut scratch, transpose, &value);
                if should_issue_uniform(
                    cm,
                    canvas_id,
                    location,
                    (value.len() / 4 * 2) as u32,
                    bytes,
                ) {
                    unsafe {
                        gl.uniform_matrix_2_f32_slice(
                            to_native_uniform_location(location).as_ref(),
                            transpose,
                            &value,
                        )
                    };
                }
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::UniformMatrix4fv {
                canvas_id,
                location,
                transpose,
                value,
            } => {
                cm.make_current_needed(canvas_id)?;
                let mut scratch = SmallVec::<[u8; 65]>::new();
                let bytes = mat_uniform_bytes(&mut scratch, transpose, &value);
                if should_issue_uniform(
                    cm,
                    canvas_id,
                    location,
                    (value.len() / 16 * 4) as u32,
                    bytes,
                ) {
                    unsafe {
                        gl.uniform_matrix_4_f32_slice(
                            to_native_uniform_location(location).as_ref(),
                            transpose,
                            &value,
                        )
                    };
                }
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::Uniform1uiv {
                canvas_id,
                location,
                value,
            } => {
                cm.make_current_needed(canvas_id)?;
                if should_issue_uniform(
                    cm,
                    canvas_id,
                    location,
                    (value.len() / 1) as u32,
                    bytemuck::cast_slice::<u32, u8>(&value),
                ) {
                    unsafe {
                        gl.uniform_1_u32_slice(
                            to_native_uniform_location(location).as_ref(),
                            &value,
                        )
                    };
                }
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::Uniform2uiv {
                canvas_id,
                location,
                value,
            } => {
                cm.make_current_needed(canvas_id)?;
                if should_issue_uniform(
                    cm,
                    canvas_id,
                    location,
                    (value.len() / 2) as u32,
                    bytemuck::cast_slice::<u32, u8>(&value),
                ) {
                    unsafe {
                        gl.uniform_2_u32_slice(
                            to_native_uniform_location(location).as_ref(),
                            &value,
                        )
                    };
                }
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::Uniform3uiv {
                canvas_id,
                location,
                value,
            } => {
                cm.make_current_needed(canvas_id)?;
                if should_issue_uniform(
                    cm,
                    canvas_id,
                    location,
                    (value.len() / 3) as u32,
                    bytemuck::cast_slice::<u32, u8>(&value),
                ) {
                    unsafe {
                        gl.uniform_3_u32_slice(
                            to_native_uniform_location(location).as_ref(),
                            &value,
                        )
                    };
                }
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::Uniform4uiv {
                canvas_id,
                location,
                value,
            } => {
                cm.make_current_needed(canvas_id)?;
                if should_issue_uniform(
                    cm,
                    canvas_id,
                    location,
                    (value.len() / 4) as u32,
                    bytemuck::cast_slice::<u32, u8>(&value),
                ) {
                    unsafe {
                        gl.uniform_4_u32_slice(
                            to_native_uniform_location(location).as_ref(),
                            &value,
                        )
                    };
                }
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::UniformMatrix2x3fv {
                canvas_id,
                location,
                transpose,
                value,
            } => {
                cm.make_current_needed(canvas_id)?;
                let mut scratch = SmallVec::<[u8; 65]>::new();
                let bytes = mat_uniform_bytes(&mut scratch, transpose, &value);
                if should_issue_uniform(
                    cm,
                    canvas_id,
                    location,
                    (value.len() / 6 * 2) as u32,
                    bytes,
                ) {
                    unsafe {
                        gl.uniform_matrix_2x3_f32_slice(
                            to_native_uniform_location(location).as_ref(),
                            transpose,
                            &value,
                        )
                    };
                }
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::UniformMatrix2x4fv {
                canvas_id,
                location,
                transpose,
                value,
            } => {
                cm.make_current_needed(canvas_id)?;
                let mut scratch = SmallVec::<[u8; 65]>::new();
                let bytes = mat_uniform_bytes(&mut scratch, transpose, &value);
                if should_issue_uniform(
                    cm,
                    canvas_id,
                    location,
                    (value.len() / 8 * 2) as u32,
                    bytes,
                ) {
                    unsafe {
                        gl.uniform_matrix_2x4_f32_slice(
                            to_native_uniform_location(location).as_ref(),
                            transpose,
                            &value,
                        )
                    };
                }
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::UniformMatrix3x2fv {
                canvas_id,
                location,
                transpose,
                value,
            } => {
                cm.make_current_needed(canvas_id)?;
                let mut scratch = SmallVec::<[u8; 65]>::new();
                let bytes = mat_uniform_bytes(&mut scratch, transpose, &value);
                if should_issue_uniform(
                    cm,
                    canvas_id,
                    location,
                    (value.len() / 6 * 3) as u32,
                    bytes,
                ) {
                    unsafe {
                        gl.uniform_matrix_3x2_f32_slice(
                            to_native_uniform_location(location).as_ref(),
                            transpose,
                            &value,
                        )
                    };
                }
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::UniformMatrix3x4fv {
                canvas_id,
                location,
                transpose,
                value,
            } => {
                cm.make_current_needed(canvas_id)?;
                let mut scratch = SmallVec::<[u8; 65]>::new();
                let bytes = mat_uniform_bytes(&mut scratch, transpose, &value);
                if should_issue_uniform(
                    cm,
                    canvas_id,
                    location,
                    (value.len() / 12 * 3) as u32,
                    bytes,
                ) {
                    unsafe {
                        gl.uniform_matrix_3x4_f32_slice(
                            to_native_uniform_location(location).as_ref(),
                            transpose,
                            &value,
                        )
                    };
                }
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::UniformMatrix4x2fv {
                canvas_id,
                location,
                transpose,
                value,
            } => {
                cm.make_current_needed(canvas_id)?;
                let mut scratch = SmallVec::<[u8; 65]>::new();
                let bytes = mat_uniform_bytes(&mut scratch, transpose, &value);
                if should_issue_uniform(
                    cm,
                    canvas_id,
                    location,
                    (value.len() / 8 * 4) as u32,
                    bytes,
                ) {
                    unsafe {
                        gl.uniform_matrix_4x2_f32_slice(
                            to_native_uniform_location(location).as_ref(),
                            transpose,
                            &value,
                        )
                    };
                }
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::UniformMatrix4x3fv {
                canvas_id,
                location,
                transpose,
                value,
            } => {
                cm.make_current_needed(canvas_id)?;
                let mut scratch = SmallVec::<[u8; 65]>::new();
                let bytes = mat_uniform_bytes(&mut scratch, transpose, &value);
                if should_issue_uniform(
                    cm,
                    canvas_id,
                    location,
                    (value.len() / 12 * 4) as u32,
                    bytes,
                ) {
                    unsafe {
                        gl.uniform_matrix_4x3_f32_slice(
                            to_native_uniform_location(location).as_ref(),
                            transpose,
                            &value,
                        )
                    };
                }
                Ok(DamageEffect::NoDamage)
            }

            // ========== Phase 3A: Framebuffer/Renderbuffer ==========
            GLCmd::CreateFramebuffer {
                canvas_id,
                client_id,
            } => {
                cm.make_current_needed(canvas_id)?;
                unsafe {
                    match gl.create_framebuffer() {
                        Ok(fb) => {
                            cm.framebuffers.insert(
                                client_id,
                                crate::canvas::FramebufferMeta {
                                    gl_handle: Some(fb),
                                    owner: canvas_id,
                                    deleted: false,
                                },
                            );
                        }
                        Err(e) => tracing::error!(
                            "gl.create_framebuffer failed for id {client_id}: {e:?}"
                        ),
                    }
                }
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::DeleteFramebuffer { framebuffer_id } => {
                // Deleting the framebuffer a context has bound leaves it on its default framebuffer, which GL's own
                // revert to name 0 is not for a canvas drawing into a DrawingBuffer: see the state tracker.
                if let Some(owner) = cm.framebuffers.get(&framebuffer_id).map(|meta| meta.owner) {
                    let name = u32::from(framebuffer_id);
                    if cm
                        .gl_state
                        .get(&owner)
                        .is_some_and(|state| st::binds_framebuffer(state, name))
                    {
                        cm.make_current_needed(owner)?;
                        let default = cm.get_drawing_buffer_fbo(owner);
                        st::rebind_default_over_deleted_framebuffer(
                            gl,
                            cm.gl_state.entry(owner).or_default(),
                            name,
                            default,
                        );
                    }
                }
                let object = cm
                    .framebuffers
                    .get_mut(&framebuffer_id)
                    .and_then(|meta| meta.take_for_delete());
                cm.framebuffers.remove(&framebuffer_id);
                if let Some(object) = object {
                    cm.delete_gl_object(object)?;
                }
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::BindFramebuffer {
                canvas_id,
                target,
                framebuffer,
            } => {
                cm.make_current_needed(canvas_id)?;
                let is_default = framebuffer.is_none();
                let native = if let Some(id) = framebuffer {
                    let meta = cm.framebuffers.get(&id).ok_or_else(|| {
                        ee(
                            ErrorCode::NotFound,
                            format!("framebuffer not found: {id:?}"),
                        )
                    })?;
                    if meta.deleted {
                        shared::bail!(
                            ErrorCode::InvalidOperation,
                            "bind_framebuffer on deleted framebuffer"
                        );
                    }
                    meta.gl_handle
                } else {
                    // "Default framebuffer" — redirect to DrawingBuffer if present.
                    cm.get_drawing_buffer_fbo(canvas_id)
                };
                // Dedup: skip the driver call if the same FBO is
                // already bound on this target.  Cocos Creator 2.x
                // issues `bindFramebuffer(FRAMEBUFFER, 0)` + the
                // real FBO bind every frame which is classic
                // "already-there" redundancy.
                let state = cm.gl_state.entry(canvas_id).or_default();
                // Shadow value: native `None` = default FBO (0 or
                // DrawingBuffer), native `Some(h)` = custom FBO.
                // We key the shadow on the user-facing framebuffer
                // id (framebuffer.map(Into::into)) rather than the
                // native handle so shadow survives FBO handle
                // recycling and multi-target semantics.
                let shadow_val = framebuffer.map(|id| u32::from(id));
                if st::update_bind_framebuffer(state, target, shadow_val) {
                    unsafe { gl.bind_framebuffer(target, native) };
                }
                // Track whether the draw target is the default framebuffer.
                // FRAMEBUFFER and DRAW_FRAMEBUFFER both affect the draw binding.
                if target == glow::FRAMEBUFFER || target == glow::DRAW_FRAMEBUFFER {
                    cm.gl_state
                        .entry(canvas_id)
                        .or_default()
                        .draws_to_default_fbo = is_default;
                }
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::FramebufferTexture2D {
                canvas_id,
                target,
                attachment,
                textarget,
                texture,
                level,
            } => {
                cm.make_current_needed(canvas_id)?;
                // WebGL spec: modifying the default framebuffer is INVALID_OPERATION.
                if cm.is_drawing_buffer_bound(canvas_id, gl, target) {
                    shared::bail!(
                        ErrorCode::InvalidOperation,
                        "framebufferTexture2D on default framebuffer"
                    );
                }
                let tex_handle = if let Some(id) = texture {
                    let meta = cm.textures.get(&id).ok_or_else(|| {
                        ee(ErrorCode::NotFound, format!("texture not found: {id:?}"))
                    })?;
                    if meta.deleted {
                        shared::bail!(
                            ErrorCode::InvalidOperation,
                            "framebufferTexture2D on deleted texture"
                        );
                    }
                    meta.gl_handle
                } else {
                    None
                };
                unsafe {
                    gl.framebuffer_texture_2d(target, attachment, textarget, tex_handle, level)
                };
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::FramebufferTextureLayer {
                canvas_id,
                target,
                attachment,
                texture,
                level,
                layer,
            } => {
                cm.make_current_needed(canvas_id)?;
                // WebGL spec: modifying the default framebuffer is INVALID_OPERATION.
                if cm.is_drawing_buffer_bound(canvas_id, gl, target) {
                    shared::bail!(
                        ErrorCode::InvalidOperation,
                        "framebufferTextureLayer on default framebuffer"
                    );
                }
                let tex_handle = if let Some(id) = texture {
                    let meta = cm.textures.get(&id).ok_or_else(|| {
                        ee(ErrorCode::NotFound, format!("texture not found: {id:?}"))
                    })?;
                    if meta.deleted {
                        shared::bail!(
                            ErrorCode::InvalidOperation,
                            "framebufferTextureLayer on deleted texture"
                        );
                    }
                    meta.gl_handle
                } else {
                    None
                };
                unsafe {
                    gl.framebuffer_texture_layer(target, attachment, tex_handle, level, layer)
                };
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::FramebufferRenderbuffer {
                canvas_id,
                target,
                attachment,
                renderbuffertarget,
                renderbuffer,
            } => {
                cm.make_current_needed(canvas_id)?;
                // WebGL spec: modifying the default framebuffer is INVALID_OPERATION.
                if cm.is_drawing_buffer_bound(canvas_id, gl, target) {
                    shared::bail!(
                        ErrorCode::InvalidOperation,
                        "framebufferRenderbuffer on default framebuffer"
                    );
                }
                let rb_handle = if let Some(id) = renderbuffer {
                    let meta = cm.renderbuffers.get(&id).ok_or_else(|| {
                        ee(
                            ErrorCode::NotFound,
                            format!("renderbuffer not found: {id:?}"),
                        )
                    })?;
                    if meta.deleted {
                        shared::bail!(
                            ErrorCode::InvalidOperation,
                            "framebufferRenderbuffer on deleted renderbuffer"
                        );
                    }
                    meta.gl_handle
                } else {
                    None
                };
                unsafe {
                    gl.framebuffer_renderbuffer(target, attachment, renderbuffertarget, rb_handle)
                };
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::CheckFramebufferStatus {
                canvas_id,
                target,
                resp,
            } => {
                cm.make_current_needed(canvas_id)?;
                let status = unsafe { gl.check_framebuffer_status(target) };
                let _ = resp.send(Ok(status));
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::CreateRenderbuffer {
                canvas_id,
                client_id,
            } => {
                cm.make_current_needed(canvas_id)?;
                let owner = Self::current_owner_canvas(cm);
                unsafe {
                    match gl.create_renderbuffer() {
                        Ok(rb) => {
                            let owner = owner.ok_or_else(|| {
                                ee(
                                    ErrorCode::InvalidOperation,
                                    "WebGL renderbuffer has no owning context",
                                )
                            })?;
                            if let Err(error) =
                                cm.webgl_gpu_budget.create_renderbuffer(owner, client_id)
                            {
                                gl.delete_renderbuffer(rb);
                                return Err(gpu_allocation_error(error));
                            }
                            cm.renderbuffers.insert(
                                client_id,
                                crate::canvas::RenderbufferMeta {
                                    gl_handle: Some(rb),
                                    owner_canvas: Some(owner),
                                    deleted: false,
                                },
                            );
                        }
                        Err(e) => tracing::error!(
                            "gl.create_renderbuffer failed for id {client_id}: {e:?}"
                        ),
                    }
                }
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::DeleteRenderbuffer { renderbuffer_id } => {
                if let Some(meta) = cm.renderbuffers.remove(&renderbuffer_id) {
                    if let Some(h) = meta.gl_handle {
                        cm.delete_gl_object(GlObject::Renderbuffer(h))?;
                    }
                    cm.webgl_gpu_budget.delete_renderbuffer(renderbuffer_id);
                }
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::DeleteBuffer { buffer_id } => {
                cm.webgl_gpu_budget.delete_buffer(buffer_id);
                if let Some(meta) = cm.buffers.remove(&buffer_id) {
                    if let Some(h) = meta.gl_handle {
                        cm.delete_gl_object(GlObject::Buffer(h))?;
                    }
                }
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::BindRenderbuffer {
                canvas_id,
                target,
                renderbuffer,
            } => {
                cm.make_current_needed(canvas_id)?;
                let native = if let Some(id) = renderbuffer {
                    let meta = cm.renderbuffers.get(&id).ok_or_else(|| {
                        ee(
                            ErrorCode::NotFound,
                            format!("renderbuffer not found: {id:?}"),
                        )
                    })?;
                    if meta.deleted {
                        shared::bail!(
                            ErrorCode::InvalidOperation,
                            "bind_renderbuffer on deleted renderbuffer"
                        );
                    }
                    meta.gl_handle
                } else {
                    None
                };
                let state = cm.gl_state.entry(canvas_id).or_default();
                let shadow_val = renderbuffer.map(|id| u32::from(id));
                if st::update_bind_renderbuffer(state, shadow_val) {
                    unsafe { gl.bind_renderbuffer(target, native) };
                }
                cm.webgl_gpu_budget
                    .bind_renderbuffer(canvas_id, target, renderbuffer);
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::RenderbufferStorage {
                canvas_id,
                target,
                internalformat,
                width,
                height,
            } => {
                cm.make_current_needed(canvas_id)?;
                let prepared = cm
                    .webgl_gpu_budget
                    .prepare_renderbuffer_storage(
                        canvas_id,
                        target,
                        internalformat,
                        width,
                        height,
                        1,
                    )
                    .map_err(gpu_allocation_error)?;
                unsafe { gl.renderbuffer_storage(target, internalformat, width, height) };
                cm.webgl_gpu_budget.commit(prepared);
                Ok(DamageEffect::NoDamage)
            }

            // ========== Phase 3B: Misc ==========
            GLCmd::ReadPixels {
                canvas_id,
                x,
                y,
                width,
                height,
                format,
                type_,
                destination_byte_length,
                resp,
            } => {
                if width < 0 || height < 0 {
                    resp.err_code(ErrorCode::InvalidArgument);
                    return Ok(DamageEffect::NoDamage);
                }
                let Some(bytes_per_pixel) = webgl_readback_bytes_per_pixel(format, type_) else {
                    resp.send(Err(
                        crate::backend::gl::readback::invalid_readback_format_type_error(
                            format, type_,
                        ),
                    ));
                    return Ok(DamageEffect::NoDamage);
                };
                let Some(_) = checked_readback_byte_len(width, height, bytes_per_pixel) else {
                    resp.err_code(ErrorCode::OutOfMemory);
                    return Ok(DamageEffect::NoDamage);
                };
                cm.make_current_needed(canvas_id)?;

                resp.send(crate::backend::gl::readback::read_webgl_pixels(
                    gl,
                    x,
                    y,
                    width,
                    height,
                    format,
                    type_,
                    destination_byte_length,
                    || {
                        if canvas_id == CanvasId::from(1u32) {
                            cm.signal_default_fbo_readback()?;
                        }
                        Ok(())
                    },
                ));
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::ReadPixelsToBuffer {
                canvas_id,
                x,
                y,
                width,
                height,
                format,
                type_,
                offset,
                resp,
            } => {
                if width < 0 || height < 0 || offset < 0 {
                    resp.err_code(ErrorCode::InvalidArgument);
                    return Ok(DamageEffect::NoDamage);
                }
                cm.make_current_needed(canvas_id)?;
                resp.send(crate::backend::gl::readback::read_webgl_pixels_to_buffer(
                    gl, x, y, width, height, format, type_, offset,
                ));
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::GetBufferSubData {
                canvas_id,
                target,
                offset,
                size,
                resp,
            } => {
                cm.make_current_needed(canvas_id)?;
                resp.send(crate::backend::gl::readback::read_webgl_buffer_range(
                    gl, target, offset, size,
                ));
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::Hint {
                canvas_id,
                target,
                mode,
            } => {
                cm.make_current_needed(canvas_id)?;
                unsafe { gl.hint(target, mode) };
                Ok(DamageEffect::NoDamage)
            }

            // ================================================================
            // WebGL 2.0 / GLES 3.0 commands.
            //
            // The state tracker added in Phase 8 layers on top of this; for
            // now each command is a thin translation into `glow` with the
            // same `make_current_needed` discipline as the WebGL 1 path.
            // ================================================================
            GLCmd::CreateVertexArray {
                canvas_id,
                client_id,
            } => {
                cm.make_current_needed(canvas_id)?;
                let handle = unsafe { gl.create_vertex_array() }.ok();
                cm.vaos.insert(
                    client_id,
                    crate::canvas::VaoMeta {
                        gl_handle: handle,
                        owner: canvas_id,
                        deleted: false,
                    },
                );
                Ok(DamageEffect::NoDamage)
            }
            GLCmd::DeleteVertexArray { vao } => {
                let object = cm.vaos.remove(&vao).and_then(|meta| {
                    meta.gl_handle.map(|handle| GlObject::VertexArray {
                        handle,
                        owner: meta.owner,
                    })
                });
                // Drop this VAO's vertex-attribute shadow. VAO names come from
                // the client, so a reused name would otherwise inherit the dead
                // object's layout and dedup away the `vertexAttribPointer` the
                // new one needs — a draw reading the wrong vertex stream, with
                // no GL error.
                for state in cm.gl_state.values_mut() {
                    state.vertex_attribs.forget_vao(vao);
                }
                if let Some(object) = object {
                    cm.delete_gl_object(object)?;
                }
                Ok(DamageEffect::NoDamage)
            }
            GLCmd::BindVertexArray { canvas_id, vao } => {
                cm.make_current_needed(canvas_id)?;
                let handle = vao.and_then(|id| cm.vaos.get(&id).and_then(|m| m.gl_handle));
                if st::update_bind_vertex_array(cm.gl_state.entry(canvas_id).or_default(), vao) {
                    unsafe { gl.bind_vertex_array(handle) };
                }
                Ok(DamageEffect::NoDamage)
            }
            GLCmd::VertexAttribDivisor {
                canvas_id,
                index,
                divisor,
            } => {
                cm.make_current_needed(canvas_id)?;
                let state = cm.gl_state.entry(canvas_id).or_default();
                if st::update_vertex_attrib_divisor(state, index, divisor) {
                    unsafe { gl.vertex_attrib_divisor(index, divisor) };
                }
                Ok(DamageEffect::NoDamage)
            }
            GLCmd::DrawArraysInstanced {
                canvas_id,
                mode,
                first,
                count,
                instance_count,
            } => {
                cm.make_current_needed(canvas_id)?;
                unsafe { gl.draw_arrays_instanced(mode, first, count, instance_count) };
                crate::render_diagnostics::bump_draw_call();
                Ok(Self::damage_for_draw(cm, canvas_id))
            }
            GLCmd::DrawElementsInstanced {
                canvas_id,
                mode,
                count,
                index_type,
                offset,
                instance_count,
            } => {
                cm.make_current_needed(canvas_id)?;
                unsafe {
                    gl.draw_elements_instanced(mode, count, index_type, offset, instance_count)
                };
                crate::render_diagnostics::bump_draw_call();
                Ok(Self::damage_for_draw(cm, canvas_id))
            }

            GLCmd::GetUniformBlockIndex {
                program_id,
                name,
                resp,
            } => {
                let _ = self.bind_for_contextless_gl(cm)?;
                let meta = cm.programs.get(&program_id).ok_or_else(|| {
                    ee(
                        ErrorCode::NotFound,
                        format!("program not found: {program_id}"),
                    )
                })?;
                let handle = meta
                    .gl_handle
                    .ok_or_else(|| ee(ErrorCode::InvalidOperation, "program has no GL handle"))?;
                let idx = unsafe { gl.get_uniform_block_index(handle, &name) }.unwrap_or(u32::MAX);
                resp.ok(idx);
                Ok(DamageEffect::NoDamage)
            }
            GLCmd::UniformBlockBinding {
                program_id,
                uniform_block_index,
                uniform_block_binding,
            } => {
                let _ = self.bind_for_contextless_gl(cm)?;
                let meta = cm.programs.get(&program_id).ok_or_else(|| {
                    ee(
                        ErrorCode::NotFound,
                        format!("program not found: {program_id}"),
                    )
                })?;
                if let Some(handle) = meta.gl_handle {
                    unsafe {
                        gl.uniform_block_binding(handle, uniform_block_index, uniform_block_binding)
                    };
                }
                Ok(DamageEffect::NoDamage)
            }
            GLCmd::BindBufferBase {
                canvas_id,
                target,
                index,
                buffer,
            } => {
                cm.make_current_needed(canvas_id)?;
                let handle = buffer.and_then(|id| cm.buffers.get(&id).and_then(|m| m.gl_handle));
                let state = cm.gl_state.entry(canvas_id).or_default();
                if st::update_bind_buffer_base(state, target, index, buffer) {
                    unsafe { gl.bind_buffer_base(target, index, handle) };
                }
                Ok(DamageEffect::NoDamage)
            }
            GLCmd::BindBufferRange {
                canvas_id,
                target,
                index,
                buffer,
                offset,
                size,
            } => {
                cm.make_current_needed(canvas_id)?;
                let handle = buffer.and_then(|id| cm.buffers.get(&id).and_then(|m| m.gl_handle));
                let state = cm.gl_state.entry(canvas_id).or_default();
                if st::update_bind_buffer_range(state, target, index, buffer, offset, size) {
                    unsafe { gl.bind_buffer_range(target, index, handle, offset, size) };
                }
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::TexStorage2D {
                canvas_id,
                target,
                levels,
                internal_format,
                width,
                height,
            } => {
                cm.make_current_needed(canvas_id)?;
                let prepared = cm
                    .webgl_gpu_budget
                    .prepare_tex_storage_2d(
                        canvas_id,
                        target,
                        levels,
                        internal_format,
                        width,
                        height,
                    )
                    .map_err(gpu_allocation_error)?;
                unsafe {
                    gl.tex_storage_2d(target, levels, internal_format, width, height);
                }
                cm.webgl_gpu_budget.commit(prepared);
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::BlitFramebuffer {
                canvas_id,
                src_x0,
                src_y0,
                src_x1,
                src_y1,
                dst_x0,
                dst_y0,
                dst_x1,
                dst_y1,
                mask,
                filter,
            } => {
                cm.make_current_needed(canvas_id)?;
                unsafe {
                    gl.blit_framebuffer(
                        src_x0, src_y0, src_x1, src_y1, dst_x0, dst_y0, dst_x1, dst_y1, mask,
                        filter,
                    );
                }
                // Conservative: any blit touching the onscreen framebuffer
                // counts as full-surface damage.  Phase 8 can look at the
                // currently-bound FBO to refine this.
                Ok(Self::damage_for_draw(cm, canvas_id))
            }
            GLCmd::InvalidateFramebuffer {
                canvas_id,
                target,
                mut attachments,
            } => {
                cm.make_current_needed(canvas_id)?;
                if cm.is_drawing_buffer_bound(canvas_id, gl, target) {
                    attachments
                        .iter_mut()
                        .for_each(|a| *a = drawing_buffer_attachment(*a));
                }
                unsafe { gl.invalidate_framebuffer(target, &attachments) };
                Ok(DamageEffect::NoDamage)
            }
            GLCmd::InvalidateSubFramebuffer {
                canvas_id,
                target,
                mut attachments,
                x,
                y,
                width,
                height,
            } => {
                cm.make_current_needed(canvas_id)?;
                if cm.is_drawing_buffer_bound(canvas_id, gl, target) {
                    attachments
                        .iter_mut()
                        .for_each(|a| *a = drawing_buffer_attachment(*a));
                }
                unsafe { gl.invalidate_sub_framebuffer(target, &attachments, x, y, width, height) };
                Ok(DamageEffect::NoDamage)
            }
            GLCmd::RenderbufferStorageMultisample {
                canvas_id,
                target,
                samples,
                internal_format,
                width,
                height,
            } => {
                cm.make_current_needed(canvas_id)?;
                let prepared = cm
                    .webgl_gpu_budget
                    .prepare_renderbuffer_storage(
                        canvas_id,
                        target,
                        internal_format,
                        width,
                        height,
                        samples,
                    )
                    .map_err(gpu_allocation_error)?;
                unsafe {
                    gl.renderbuffer_storage_multisample(
                        target,
                        samples,
                        internal_format,
                        width,
                        height,
                    )
                };
                cm.webgl_gpu_budget.commit(prepared);
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::CreateSampler {
                canvas_id,
                client_id,
            } => {
                cm.make_current_needed(canvas_id)?;
                let handle = unsafe { gl.create_sampler() }.ok();
                cm.samplers.insert(
                    client_id,
                    crate::canvas::SamplerMeta {
                        gl_handle: handle,
                        owner_canvas: Some(canvas_id),
                        deleted: false,
                    },
                );
                Ok(DamageEffect::NoDamage)
            }
            GLCmd::DeleteSampler { sampler } => {
                let handle = cm.samplers.remove(&sampler).and_then(|meta| meta.gl_handle);
                if let Some(h) = handle {
                    cm.delete_gl_object(GlObject::Sampler(h))?;
                }
                Ok(DamageEffect::NoDamage)
            }
            GLCmd::BindSampler {
                canvas_id,
                unit,
                sampler,
            } => {
                cm.make_current_needed(canvas_id)?;
                let handle = sampler.and_then(|id| cm.samplers.get(&id).and_then(|m| m.gl_handle));
                unsafe { gl.bind_sampler(unit, handle) };
                Ok(DamageEffect::NoDamage)
            }
            GLCmd::SamplerParameteri {
                sampler,
                pname,
                param,
            } => {
                let _ = self.bind_for_contextless_gl(cm)?;
                if let Some(h) = cm.samplers.get(&sampler).and_then(|m| m.gl_handle) {
                    unsafe { gl.sampler_parameter_i32(h, pname, param) };
                }
                Ok(DamageEffect::NoDamage)
            }
            GLCmd::SamplerParameterf {
                sampler,
                pname,
                param,
            } => {
                let _ = self.bind_for_contextless_gl(cm)?;
                if let Some(h) = cm.samplers.get(&sampler).and_then(|m| m.gl_handle) {
                    unsafe { gl.sampler_parameter_f32(h, pname, param) };
                }
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::FenceSync {
                canvas_id,
                client_id,
                condition,
                flags,
            } => {
                cm.make_current_needed(canvas_id)?;
                let handle = unsafe { gl.fence_sync(condition, flags) }.ok();
                cm.syncs.insert(
                    client_id,
                    crate::canvas::SyncMeta {
                        gl_handle: handle,
                        owner_canvas: Some(canvas_id),
                        deleted: false,
                    },
                );
                Ok(DamageEffect::NoDamage)
            }
            // The GL server waits for the fence before it runs what follows; a sync that was never made or is gone is
            // nothing to wait for (the facade refuses a deleted one before anything is queued).
            GLCmd::WaitSync { canvas_id, sync } => {
                cm.make_current_needed(canvas_id)?;
                if let Some(handle) = cm.syncs.get(&sync).and_then(|meta| meta.gl_handle) {
                    unsafe { gl.wait_sync(handle, 0, glow::TIMEOUT_IGNORED) };
                }
                Ok(DamageEffect::NoDamage)
            }
            GLCmd::DeleteSync { sync } => {
                let handle = cm.syncs.remove(&sync).and_then(|meta| meta.gl_handle);
                if let Some(h) = handle {
                    cm.delete_gl_object(GlObject::Sync(h))?;
                }
                Ok(DamageEffect::NoDamage)
            }
            GLCmd::ClientWaitSync { sync, flags, resp } => {
                // clientWaitSync must run on the owning context; rebind if
                // we're not already there.
                let meta = cm.syncs.get(&sync).cloned();
                let status: u32 = if let Some(meta) = meta {
                    if let Some(owner) = meta.owner_canvas {
                        cm.make_current_needed(owner)?;
                    } else {
                        let _ = self.bind_for_contextless_gl(cm)?;
                    }
                    if let Some(h) = meta.gl_handle {
                        // Zero, whatever the producer asked for, and the reason is
                        // that this thread is not the producer's to spend.
                        //
                        // `MAX_CLIENT_WAIT_TIMEOUT_WEBGL` is zero for this context, so
                        // a conforming producer only ever asks for zero and the shim
                        // rejects anything else with INVALID_OPERATION. But on the
                        // external-frame lane the producer is content JavaScript in
                        // another process: it composes this command itself, and the
                        // envelope validator checks framing rather than GL semantics.
                        // A large `timeout_ns` arriving here would block the render
                        // thread inside the driver -- a thread shared by every canvas
                        // and by the frame loop -- for as long as content asked.
                        //
                        // The previous comment argued for preserving the full 64-bit
                        // range because `glow`'s `client_wait_sync` takes `i32` and
                        // would silently clamp above ~2.147 s. That argument was about
                        // not misrepresenting a wait, and it stops applying once the
                        // answer is that there is no wait to misrepresent: polling is
                        // the whole of the contract, and `client_wait_sync_u64` is kept
                        // because it is the path that takes the value without narrowing
                        // it, which is still what a zero should travel through.
                        cm.client_wait_sync_u64(h.0 as *const std::ffi::c_void, flags, 0)
                    } else {
                        glow::WAIT_FAILED
                    }
                } else {
                    glow::WAIT_FAILED
                };
                resp.ok(status);
                Ok(DamageEffect::NoDamage)
            }

            GLCmd::DrawBuffers { canvas_id, buffers } => {
                cm.make_current_needed(canvas_id)?;
                unsafe { gl.draw_buffers(&buffers) };
                Ok(DamageEffect::NoDamage)
            }
            GLCmd::ReadBuffer { canvas_id, src } => {
                cm.make_current_needed(canvas_id)?;
                unsafe { gl.read_buffer(src) };
                Ok(DamageEffect::NoDamage)
            }

            // ---- WebGL 2 Query objects ----
            GLCmd::CreateQuery {
                canvas_id,
                client_id,
            } => {
                cm.make_current_needed(canvas_id)?;
                let handle = unsafe { gl.create_query().ok() };
                cm.queries.insert(
                    client_id,
                    crate::canvas::QueryMeta {
                        gl_handle: handle,
                        owner: canvas_id,
                        deleted: false,
                    },
                );
                Ok(DamageEffect::NoDamage)
            }
            GLCmd::DeleteQuery { query } => {
                let object = cm.queries.remove(&query).and_then(|meta| {
                    meta.gl_handle.map(|handle| GlObject::Query {
                        handle,
                        owner: meta.owner,
                    })
                });
                if let Some(object) = object {
                    cm.delete_gl_object(object)?;
                }
                Ok(DamageEffect::NoDamage)
            }
            GLCmd::BeginQuery {
                canvas_id,
                target,
                query,
            } => {
                cm.make_current_needed(canvas_id)?;
                let handle = cm.queries.get(&query).and_then(|m| m.gl_handle);
                if let Some(h) = handle {
                    unsafe { gl.begin_query(target, h) };
                }
                Ok(DamageEffect::NoDamage)
            }
            GLCmd::EndQuery { canvas_id, target } => {
                cm.make_current_needed(canvas_id)?;
                unsafe { gl.end_query(target) };
                Ok(DamageEffect::NoDamage)
            }
            GLCmd::GetQueryParameter { query, pname, resp } => {
                let meta = cm.queries.get(&query).cloned();
                let result: u32 = if let Some(meta) = meta {
                    cm.make_current_needed(meta.owner)?;
                    match meta.gl_handle {
                        Some(h) => unsafe { gl.get_query_parameter_u32(h, pname) },
                        None => 0,
                    }
                } else {
                    0
                };
                resp.ok(result);
                Ok(DamageEffect::NoDamage)
            }

            // ---- WebGL 2 Transform Feedback ----
            GLCmd::CreateTransformFeedback {
                canvas_id,
                client_id,
            } => {
                cm.make_current_needed(canvas_id)?;
                let handle = unsafe { gl.create_transform_feedback().ok() };
                cm.transform_feedbacks.insert(
                    client_id,
                    crate::canvas::TransformFeedbackMeta {
                        gl_handle: handle,
                        owner: canvas_id,
                        deleted: false,
                    },
                );
                Ok(DamageEffect::NoDamage)
            }
            GLCmd::DeleteTransformFeedback { tf } => {
                let object = cm.transform_feedbacks.remove(&tf).and_then(|meta| {
                    meta.gl_handle.map(|handle| GlObject::TransformFeedback {
                        handle,
                        owner: meta.owner,
                    })
                });
                if let Some(object) = object {
                    cm.delete_gl_object(object)?;
                }
                Ok(DamageEffect::NoDamage)
            }
            GLCmd::BindTransformFeedback {
                canvas_id,
                target,
                tf,
            } => {
                cm.make_current_needed(canvas_id)?;
                let handle =
                    tf.and_then(|id| cm.transform_feedbacks.get(&id).and_then(|m| m.gl_handle));
                unsafe { gl.bind_transform_feedback(target, handle) };
                Ok(DamageEffect::NoDamage)
            }
            GLCmd::BeginTransformFeedback {
                canvas_id,
                primitive_mode,
            } => {
                cm.make_current_needed(canvas_id)?;
                unsafe { gl.begin_transform_feedback(primitive_mode) };
                Ok(DamageEffect::NoDamage)
            }
            GLCmd::EndTransformFeedback { canvas_id } => {
                cm.make_current_needed(canvas_id)?;
                unsafe { gl.end_transform_feedback() };
                Ok(DamageEffect::NoDamage)
            }
            GLCmd::PauseTransformFeedback { canvas_id } => {
                cm.make_current_needed(canvas_id)?;
                unsafe { gl.pause_transform_feedback() };
                Ok(DamageEffect::NoDamage)
            }
            GLCmd::ResumeTransformFeedback { canvas_id } => {
                cm.make_current_needed(canvas_id)?;
                unsafe { gl.resume_transform_feedback() };
                Ok(DamageEffect::NoDamage)
            }
            GLCmd::GetTransformFeedbackVarying {
                program,
                index,
                resp,
            } => {
                let owner = cm.programs.get(&program).and_then(|m| m.owner_canvas);
                let handle = cm.programs.get(&program).and_then(|m| m.gl_handle);
                let result = if let Some(handle) = handle {
                    if let Some(owner) = owner {
                        cm.make_current_needed(owner)?;
                    }
                    unsafe {
                        gl.get_transform_feedback_varying(handle, index)
                            .map(|info| (info.name, info.size, info.tftype))
                    }
                } else {
                    None
                };
                resp.ok(result);
                Ok(DamageEffect::NoDamage)
            }
            GLCmd::TransformFeedbackVaryings {
                canvas_id,
                program,
                varyings,
                buffer_mode,
            } => {
                cm.make_current_needed(canvas_id)?;
                let handle = cm.programs.get(&program).and_then(|m| m.gl_handle);
                if let Some(h) = handle {
                    let refs: Vec<&str> = varyings.iter().map(|s| s.as_str()).collect();
                    unsafe { gl.transform_feedback_varyings(h, &refs, buffer_mode) };
                }
                // Preserve the exact ordered names and mode for the pre-link
                // cache descriptor.  Unlike the linked query, this is the
                // caller's immutable link input.
                self.tf_descriptors.insert(program, (varyings, buffer_mode));
                Ok(DamageEffect::NoDamage)
            }

            // ---- WebGL 2 3D texture uploads ----
            GLCmd::TexImage3D {
                canvas_id,
                target,
                level,
                internal_format,
                width,
                height,
                depth,
                border,
                format,
                ty,
                data,
            } => {
                cm.make_current_needed(canvas_id)?;
                let pixels = match &data {
                    None => glow::PixelUnpackData::Slice(None),
                    Some(source) => unpack_data(
                        gl,
                        source,
                        width,
                        height,
                        depth,
                        format,
                        ty,
                        &cm.unpack_layout(canvas_id, true),
                    )?,
                };
                let prepared = cm
                    .webgl_gpu_budget
                    .prepare_tex_image_3d(
                        canvas_id,
                        target,
                        level,
                        internal_format,
                        width,
                        height,
                        depth,
                        border,
                        format,
                        ty,
                    )
                    .map_err(gpu_allocation_error)?;
                unsafe {
                    gl.tex_image_3d(
                        target,
                        level,
                        internal_format,
                        width,
                        height,
                        depth,
                        border,
                        format,
                        ty,
                        pixels,
                    );
                }
                cm.webgl_gpu_budget.commit(prepared);
                Ok(DamageEffect::NoDamage)
            }
            GLCmd::TexSubImage3D {
                canvas_id,
                target,
                level,
                xoffset,
                yoffset,
                zoffset,
                width,
                height,
                depth,
                format,
                ty,
                data,
            } => {
                cm.make_current_needed(canvas_id)?;
                let pixels = unpack_data(
                    gl,
                    &data,
                    width,
                    height,
                    depth,
                    format,
                    ty,
                    &cm.unpack_layout(canvas_id, true),
                )?;
                unsafe {
                    gl.tex_sub_image_3d(
                        target, level, xoffset, yoffset, zoffset, width, height, depth, format, ty,
                        pixels,
                    );
                }
                Ok(DamageEffect::NoDamage)
            }
            GLCmd::TexStorage3D {
                canvas_id,
                target,
                levels,
                internal_format,
                width,
                height,
                depth,
            } => {
                cm.make_current_needed(canvas_id)?;
                let prepared = cm
                    .webgl_gpu_budget
                    .prepare_tex_storage_3d(
                        canvas_id,
                        target,
                        levels,
                        internal_format,
                        width,
                        height,
                        depth,
                    )
                    .map_err(gpu_allocation_error)?;
                unsafe {
                    gl.tex_storage_3d(target, levels, internal_format, width, height, depth);
                }
                cm.webgl_gpu_budget.commit(prepared);
                Ok(DamageEffect::NoDamage)
            }

            _ => {
                shared::bail!(
                    ErrorCode::NotImplemented,
                    "GL command not covered by RendererGL"
                );
            }
        }
    }

    /// Upload texture data via PBO for async DMA transfer.
    fn tex_image_2d_pbo(
        cm: &mut CanvasManager,
        gl: &glow::Context,
        target: u32,
        level: i32,
        internalformat: i32,
        width: i32,
        height: i32,
        border: i32,
        format: u32,
        type_: u32,
        data: &[u8],
    ) -> EngineResult<DamageEffect> {
        let pool = cm
            .pbo_pool_mut()
            .ok_or_else(|| ee(ErrorCode::RenderBackendError, "PBO pool not available"))?;
        let pbo = pool
            .acquire(gl, data.len())
            .ok_or_else(|| ee(ErrorCode::RenderBackendError, "PBO acquire failed"))?;
        unsafe {
            gl.bind_buffer(glow::PIXEL_UNPACK_BUFFER, Some(pbo));
            gl.buffer_data_u8_slice(glow::PIXEL_UNPACK_BUFFER, data, glow::STREAM_DRAW);
            gl.tex_image_2d(
                target,
                level,
                internalformat,
                width,
                height,
                border,
                format,
                type_,
                glow::PixelUnpackData::BufferOffset(0),
            );
            gl.bind_buffer(glow::PIXEL_UNPACK_BUFFER, None);
        }
        let pool = cm.pbo_pool_mut().unwrap();
        pool.release(gl, pbo, data.len());
        Ok(DamageEffect::NoDamage)
    }

    /// Upload sub-image data via PBO for async DMA transfer.
    fn tex_sub_image_2d_pbo(
        cm: &mut CanvasManager,
        gl: &glow::Context,
        target: u32,
        level: i32,
        xoffset: i32,
        yoffset: i32,
        width: i32,
        height: i32,
        format: u32,
        type_: u32,
        data: &[u8],
    ) -> EngineResult<DamageEffect> {
        let pool = cm
            .pbo_pool_mut()
            .ok_or_else(|| ee(ErrorCode::RenderBackendError, "PBO pool not available"))?;
        let pbo = pool
            .acquire(gl, data.len())
            .ok_or_else(|| ee(ErrorCode::RenderBackendError, "PBO acquire failed"))?;
        unsafe {
            gl.bind_buffer(glow::PIXEL_UNPACK_BUFFER, Some(pbo));
            gl.buffer_data_u8_slice(glow::PIXEL_UNPACK_BUFFER, data, glow::STREAM_DRAW);
            gl.tex_sub_image_2d(
                target,
                level,
                xoffset,
                yoffset,
                width,
                height,
                format,
                type_,
                glow::PixelUnpackData::BufferOffset(0),
            );
            gl.bind_buffer(glow::PIXEL_UNPACK_BUFFER, None);
        }
        let pool = cm.pbo_pool_mut().unwrap();
        pool.release(gl, pbo, data.len());
        Ok(DamageEffect::NoDamage)
    }
}

/// Pure-logic draw damage classification, extracted for testability.
/// Uses `viewport ∩ scissor` when scissor test is enabled.
pub(crate) fn draw_damage_effect(
    draws_to_default_fbo: bool,
    viewport: Option<(i32, i32, i32, i32)>,
    scissor: ScissorState,
) -> DamageEffect {
    if !draws_to_default_fbo {
        return DamageEffect::NoDamage;
    }
    let vp = match viewport {
        Some(v) => v,
        None => return DamageEffect::FullSurface,
    };
    let bounds = match scissor {
        ScissorState::Enabled {
            x,
            y,
            width,
            height,
        } => match crate::damage_effect::intersect_rects(vp, (x, y, width, height)) {
            Some(isect) => isect,
            None => return DamageEffect::NoDamage,
        },
        // Unknown rect: the real GL scissor box is the full drawable.
        // Fall back to viewport — conservative, never under-reports.
        ScissorState::EnabledUnknownRect | ScissorState::Disabled => vp,
    };
    DamageEffect::OnscreenRect {
        x: bounds.0,
        y: bounds.1,
        width: bounds.2,
        height: bounds.3,
    }
}

/// A compressed sub-image's data as glow takes it: the bytes, or the range of the bound PIXEL_UNPACK_BUFFER.
fn compressed_unpack_data(data: &CompressedImageData) -> glow::CompressedPixelUnpackData<'_> {
    match data {
        CompressedImageData::Bytes(bytes) => glow::CompressedPixelUnpackData::Slice(bytes),
        CompressedImageData::UnpackBuffer { offset, size } => {
            glow::CompressedPixelUnpackData::BufferRange(
                *offset..offset.saturating_add(*size as u32),
            )
        }
    }
}

/// An attachment named the way the default framebuffer names its buffers (`COLOR`, `DEPTH`, `STENCIL`), as the
/// attachment of the framebuffer object that stands in for it when the canvas draws into a DrawingBuffer. GL takes
/// only the first spelling on framebuffer 0 and only the second on an object, and WebGL content uses the first.
pub(crate) fn drawing_buffer_attachment(attachment: u32) -> u32 {
    match attachment {
        glow::COLOR => glow::COLOR_ATTACHMENT0,
        glow::DEPTH => glow::DEPTH_ATTACHMENT,
        glow::STENCIL => glow::STENCIL_ATTACHMENT,
        other => other,
    }
}

/// The `clear` bits a `clearBuffer*` call is the clear of. COLOR is the colour buffer only in draw buffer 0: that is
/// the one colour buffer a default framebuffer has (WebGL's `drawBuffers` on it takes one entry), and the emulated
/// default framebuffer presents its COLOR_ATTACHMENT0, which GLES 3 lets no other draw buffer index name. A COLOR clear
/// of another draw buffer writes a user framebuffer's attachment or nothing; depth and stencil are never presented.
pub(crate) fn clear_buffer_bits(buffer: u32, drawbuffer: i32) -> u32 {
    match buffer {
        glow::COLOR if drawbuffer == 0 => glow::COLOR_BUFFER_BIT,
        glow::DEPTH => glow::DEPTH_BUFFER_BIT,
        glow::STENCIL => glow::STENCIL_BUFFER_BIT,
        glow::DEPTH_STENCIL => glow::DEPTH_BUFFER_BIT | glow::STENCIL_BUFFER_BIT,
        _ => 0,
    }
}

pub(crate) fn clear_damage_effect(
    bit_field: u32,
    is_onscreen_default_fbo: bool,
    scissor: ScissorState,
    color_mask: (bool, bool, bool, bool),
) -> DamageEffect {
    if bit_field & glow::COLOR_BUFFER_BIT == 0 {
        return DamageEffect::NoDamage;
    }
    let (r, g, b, a) = color_mask;
    if !r && !g && !b && !a {
        return DamageEffect::NoDamage;
    }
    if !is_onscreen_default_fbo {
        return DamageEffect::NoDamage;
    }
    match scissor {
        ScissorState::Enabled {
            x,
            y,
            width,
            height,
        } => DamageEffect::OnscreenRect {
            x,
            y,
            width,
            height,
        },
        // Unknown rect or disabled: can't bound the clear.
        ScissorState::EnabledUnknownRect | ScissorState::Disabled => DamageEffect::FullSurface,
    }
}

/// The `format` and `type` a texture upload from a canvas is accounted under.
///
/// What the content passed when the record carries it; otherwise what a canvas is: 8-bit pixels, in the internal
/// format's own components. A zero is "not forwarded", never a real format (GL has none).
fn canvas_source_upload_format(internalformat: i32, format: u32, type_: u32) -> (u32, u32) {
    (
        if format == 0 {
            internalformat as u32
        } else {
            format
        },
        if type_ == 0 {
            glow::UNSIGNED_BYTE
        } else {
            type_
        },
    )
}

/// The renderable color formats whose components are integers, which ES 3.0 does not multisample.
fn is_integer_color_format(internalformat: u32) -> bool {
    matches!(
        internalformat,
        glow::R8I
            | glow::R8UI
            | glow::R16I
            | glow::R16UI
            | glow::R32I
            | glow::R32UI
            | glow::RG8I
            | glow::RG8UI
            | glow::RG16I
            | glow::RG16UI
            | glow::RG32I
            | glow::RG32UI
            | glow::RGBA8I
            | glow::RGBA8UI
            | glow::RGBA16I
            | glow::RGBA16UI
            | glow::RGBA32I
            | glow::RGBA32UI
            | glow::RGB10_A2UI
    )
}

/// `getInternalformatParameter(target, internalformat, SAMPLES)`: the sample counts the driver supports for a
/// renderbuffer of `internalformat`, as a JSON array sorted descending (the specification's order), empty when the
/// format cannot be multisampled.
///
/// Asked of the driver, not derived from `MAX_SAMPLES`: which formats multisample, and at which counts, is the
/// driver's. `target` is checked by the caller's validation; a target the driver refuses reads back as no counts.
fn internalformat_samples_json(gl: &glow::Context, target: u32, internalformat: u32) -> String {
    // WebGL 2 is OpenGL ES 3.0, where a signed or unsigned integer renderbuffer cannot be multisampled: the
    // specification's answer is no counts. ANGLE reports its ES 3.1 ones whatever the context version.
    if is_integer_color_format(internalformat) {
        return "[]".to_string();
    }
    let mut count = [0i32; 1];
    unsafe {
        gl.get_internal_format_i32_slice(
            target,
            internalformat,
            glow::NUM_SAMPLE_COUNTS,
            &mut count,
        );
    }
    let count = count[0].clamp(0, 64) as usize;
    if count == 0 {
        return "[]".to_string();
    }
    let mut samples = vec![0i32; count];
    unsafe {
        gl.get_internal_format_i32_slice(target, internalformat, glow::SAMPLES, &mut samples);
    }
    // The driver lists them largest first; the specification promises it, so it is not left to the driver.
    samples.sort_unstable_by(|a, b| b.cmp(a));
    samples.dedup();
    let items: Vec<String> = samples.iter().map(i32::to_string).collect();
    format!("[{}]", items.join(","))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The renderer refuses the bytes the driver would read past, by the same layout the facade checks with: rows
    /// padded to UNPACK_ALIGNMENT, UNPACK_ROW_LENGTH in place of the width, the last row unpadded; and a (format,
    /// type) pair with no layout at all.
    #[test]
    fn an_upload_shorter_than_its_layout_or_of_no_layout_is_refused() {
        use unpack_convert::UnpackLayout;
        let packed = UnpackLayout {
            alignment: 4,
            ..Default::default()
        };
        let refused = |bytes: usize, w, h, format, type_, layout: &UnpackLayout| {
            refuse_short_upload(&vec![0; bytes], w, h, 1, format, type_, layout).map_err(|e| e.code)
        };
        let (rgba, rgb, ubyte) = (glow::RGBA, glow::RGB, glow::UNSIGNED_BYTE);
        assert_eq!(
            refused(3, 1, 1, rgba, ubyte, &packed),
            Err(ErrorCode::InvalidOperation)
        );
        assert_eq!(refused(4, 1, 1, rgba, ubyte, &packed), Ok(()));
        assert_eq!(
            refused(20, 3, 2, rgb, ubyte, &packed),
            Err(ErrorCode::InvalidOperation)
        );
        assert_eq!(refused(21, 3, 2, rgb, ubyte, &packed), Ok(()));
        let rows_of_four = UnpackLayout {
            alignment: 4,
            row_length: 4,
            skip_pixels: 1,
            ..Default::default()
        };
        assert_eq!(
            refused(27, 2, 2, rgba, ubyte, &rows_of_four),
            Err(ErrorCode::InvalidOperation)
        );
        assert_eq!(refused(28, 2, 2, rgba, ubyte, &rows_of_four), Ok(()));
        assert_eq!(
            refused(64, 1, 1, rgba, 0x1234, &packed),
            Err(ErrorCode::InvalidOperation),
            "a type with no layout"
        );
    }

    /// An upload naming a PIXEL_UNPACK_BUFFER offset reaches the driver only with a buffer bound: without one GL
    /// would read the offset as an address.
    #[test]
    #[ignore = "requires Mesa surfaceless EGL and GLES3"]
    fn an_upload_from_an_unpack_buffer_offset_needs_a_buffer_bound() {
        let (_scope, gl) = crate::backend::gl::readback_test_gl::native_gles3_context();
        assert_eq!(
            refuse_offset_without_unpack_buffer(&gl).map_err(|e| e.code),
            Err(ErrorCode::InvalidOperation)
        );
        unsafe {
            let buffer = gl.create_buffer().unwrap();
            gl.bind_buffer(glow::PIXEL_UNPACK_BUFFER, Some(buffer));
            assert_eq!(
                refuse_offset_without_unpack_buffer(&gl).map_err(|e| e.code),
                Ok(())
            );
            gl.bind_buffer(glow::PIXEL_UNPACK_BUFFER, None);
            gl.delete_buffer(buffer);
        }
        assert_eq!(
            refuse_offset_without_unpack_buffer(&gl).map_err(|e| e.code),
            Err(ErrorCode::InvalidOperation)
        );
    }

    const COLOR: u32 = glow::COLOR_BUFFER_BIT;
    const DEPTH: u32 = glow::DEPTH_BUFFER_BIT;
    const STENCIL: u32 = glow::STENCIL_BUFFER_BIT;
    const ALL_ON: (bool, bool, bool, bool) = (true, true, true, true);
    const ALL_OFF: (bool, bool, bool, bool) = (false, false, false, false);
    const OFF: ScissorState = ScissorState::Disabled;

    fn on(x: i32, y: i32, w: i32, h: i32) -> ScissorState {
        ScissorState::Enabled {
            x,
            y,
            width: w,
            height: h,
        }
    }

    #[test]
    fn single_mat4_uniform_dedup_scratch_stays_inline() {
        let mut scratch = smallvec::SmallVec::<[u8; 65]>::new();
        let matrix = [1.0f32; 16];
        {
            let bytes = mat_uniform_bytes(&mut scratch, false, &matrix);
            assert_eq!(bytes.len(), 65);
            assert_eq!(bytes[0], 0);
        }
        assert!(!scratch.spilled());
    }

    // ---- clear_damage_effect tests ----

    /// A compressed sub-image's bytes go to GL as a slice; a range of the bound PIXEL_UNPACK_BUFFER as the buffer range
    /// `offset..offset + size`, whose length is the `imageSize` GL is given. A size that would carry the end past 2^32
    /// saturates there rather than wrapping to a short range GL would accept.
    #[test]
    fn a_compressed_sub_image_s_data_is_its_bytes_or_its_buffer_range() {
        let bytes = CompressedImageData::Bytes(vec![1, 2, 3]);
        assert!(matches!(
            compressed_unpack_data(&bytes),
            glow::CompressedPixelUnpackData::Slice(slice) if slice == [1, 2, 3]
        ));
        let range = CompressedImageData::UnpackBuffer {
            offset: 64,
            size: 16,
        };
        assert!(matches!(
            compressed_unpack_data(&range),
            glow::CompressedPixelUnpackData::BufferRange(r) if r == (64..80)
        ));
        let edge = CompressedImageData::UnpackBuffer {
            offset: u32::MAX - 4,
            size: 16,
        };
        assert!(matches!(
            compressed_unpack_data(&edge),
            glow::CompressedPixelUnpackData::BufferRange(r) if r == (u32::MAX - 4..u32::MAX)
        ));
    }

    /// An invalidation of the default framebuffer names COLOR / DEPTH / STENCIL; on the DrawingBuffer that stands in
    /// for it those are COLOR_ATTACHMENT0 / DEPTH_ATTACHMENT / STENCIL_ATTACHMENT, which GL requires of an object. Any
    /// other name -- an object's own attachment points -- passes unchanged.
    #[test]
    fn the_default_framebuffer_s_buffer_names_become_the_drawing_buffer_s_attachments() {
        assert_eq!(
            drawing_buffer_attachment(glow::COLOR),
            glow::COLOR_ATTACHMENT0
        );
        assert_eq!(
            drawing_buffer_attachment(glow::DEPTH),
            glow::DEPTH_ATTACHMENT
        );
        assert_eq!(
            drawing_buffer_attachment(glow::STENCIL),
            glow::STENCIL_ATTACHMENT
        );
        for unchanged in [
            glow::COLOR_ATTACHMENT0,
            glow::COLOR_ATTACHMENT3,
            glow::DEPTH_ATTACHMENT,
            glow::DEPTH_STENCIL_ATTACHMENT,
        ] {
            assert_eq!(drawing_buffer_attachment(unchanged), unchanged);
        }
    }

    /// A `clearBuffer*` damages what the `clear` of the same buffers does: COLOR in draw buffer 0 is the presented
    /// colour buffer (bounded by the scissor, nothing under an all-off colour mask, nothing off the onscreen default
    /// framebuffer); COLOR in any other draw buffer, DEPTH, STENCIL and DEPTH_STENCIL change nothing that is shown.
    #[test]
    fn clear_buffer_damages_what_the_clear_of_the_same_buffers_does() {
        let color0 = clear_buffer_bits(glow::COLOR, 0);
        assert_eq!(color0, COLOR);
        assert_eq!(
            clear_damage_effect(color0, true, OFF, ALL_ON),
            DamageEffect::FullSurface
        );
        assert_eq!(
            clear_damage_effect(color0, true, on(1, 2, 3, 4), ALL_ON),
            DamageEffect::OnscreenRect {
                x: 1,
                y: 2,
                width: 3,
                height: 4
            }
        );
        assert_eq!(
            clear_damage_effect(color0, true, OFF, ALL_OFF),
            DamageEffect::NoDamage
        );
        assert_eq!(
            clear_damage_effect(color0, false, OFF, ALL_ON),
            DamageEffect::NoDamage
        );
        for (buffer, drawbuffer) in [
            (glow::COLOR, 1),
            (glow::COLOR, 7),
            (glow::DEPTH, 0),
            (glow::STENCIL, 0),
            (glow::DEPTH_STENCIL, 0),
        ] {
            assert_eq!(
                clear_damage_effect(clear_buffer_bits(buffer, drawbuffer), true, OFF, ALL_ON),
                DamageEffect::NoDamage,
                "buffer {buffer:#x} draw buffer {drawbuffer}"
            );
        }
        assert_eq!(clear_buffer_bits(glow::DEPTH_STENCIL, 0), DEPTH | STENCIL);
    }

    #[test]
    fn color_clear_with_scissor_produces_onscreen_rect() {
        assert_eq!(
            clear_damage_effect(COLOR, true, on(10, 20, 100, 50), ALL_ON),
            DamageEffect::OnscreenRect {
                x: 10,
                y: 20,
                width: 100,
                height: 50
            }
        );
    }

    #[test]
    fn color_clear_without_scissor_produces_full_surface() {
        assert_eq!(
            clear_damage_effect(COLOR, true, OFF, ALL_ON),
            DamageEffect::FullSurface
        );
    }

    #[test]
    fn depth_only_clear_produces_no_damage() {
        assert_eq!(
            clear_damage_effect(DEPTH, true, OFF, ALL_ON),
            DamageEffect::NoDamage
        );
    }

    #[test]
    fn stencil_only_clear_produces_no_damage() {
        assert_eq!(
            clear_damage_effect(STENCIL, true, OFF, ALL_ON),
            DamageEffect::NoDamage
        );
    }

    #[test]
    fn depth_stencil_clear_produces_no_damage() {
        assert_eq!(
            clear_damage_effect(DEPTH | STENCIL, true, OFF, ALL_ON),
            DamageEffect::NoDamage
        );
    }

    #[test]
    fn color_depth_clear_uses_color_logic() {
        assert_eq!(
            clear_damage_effect(COLOR | DEPTH, true, on(5, 5, 200, 200), ALL_ON),
            DamageEffect::OnscreenRect {
                x: 5,
                y: 5,
                width: 200,
                height: 200
            }
        );
        assert_eq!(
            clear_damage_effect(COLOR | DEPTH, true, OFF, ALL_ON),
            DamageEffect::FullSurface
        );
    }

    #[test]
    fn depth_clear_on_user_fbo_is_no_damage() {
        assert_eq!(
            clear_damage_effect(DEPTH, false, OFF, ALL_ON),
            DamageEffect::NoDamage
        );
    }

    #[test]
    fn color_clear_on_user_fbo_is_no_damage() {
        assert_eq!(
            clear_damage_effect(COLOR, false, OFF, ALL_ON),
            DamageEffect::NoDamage
        );
    }

    #[test]
    fn depth_only_with_scissor_still_no_damage() {
        assert_eq!(
            clear_damage_effect(DEPTH, true, on(0, 0, 100, 100), ALL_ON),
            DamageEffect::NoDamage
        );
    }

    #[test]
    fn color_clear_with_all_mask_off_is_no_damage() {
        assert_eq!(
            clear_damage_effect(COLOR, true, OFF, ALL_OFF),
            DamageEffect::NoDamage
        );
    }

    #[test]
    fn color_clear_with_partial_mask_and_scissor_produces_onscreen_rect() {
        let partial = (true, false, false, false);
        assert_eq!(
            clear_damage_effect(COLOR, true, on(10, 20, 100, 50), partial),
            DamageEffect::OnscreenRect {
                x: 10,
                y: 20,
                width: 100,
                height: 50
            }
        );
    }

    #[test]
    fn color_depth_clear_with_all_mask_off_is_no_damage() {
        assert_eq!(
            clear_damage_effect(COLOR | DEPTH, true, OFF, ALL_OFF),
            DamageEffect::NoDamage
        );
    }

    #[test]
    fn color_depth_clear_with_active_mask_uses_color_logic() {
        let partial = (false, true, false, false);
        assert_eq!(
            clear_damage_effect(COLOR | DEPTH, true, on(0, 0, 50, 50), partial),
            DamageEffect::OnscreenRect {
                x: 0,
                y: 0,
                width: 50,
                height: 50
            }
        );
        assert_eq!(
            clear_damage_effect(COLOR | DEPTH, true, OFF, partial),
            DamageEffect::FullSurface
        );
    }

    #[test]
    fn alpha_only_mask_still_counts_as_visible_damage() {
        let alpha_only = (false, false, false, true);
        assert_eq!(
            clear_damage_effect(COLOR, true, OFF, alpha_only),
            DamageEffect::FullSurface
        );
    }

    // ---- draw_damage_effect tests ----

    #[test]
    fn draw_viewport_only_produces_onscreen_rect() {
        assert_eq!(
            draw_damage_effect(true, Some((0, 0, 800, 600)), OFF),
            DamageEffect::OnscreenRect {
                x: 0,
                y: 0,
                width: 800,
                height: 600
            }
        );
    }

    #[test]
    fn draw_viewport_intersect_scissor_produces_tighter_rect() {
        assert_eq!(
            draw_damage_effect(true, Some((0, 0, 1080, 1920)), on(100, 200, 300, 400)),
            DamageEffect::OnscreenRect {
                x: 100,
                y: 200,
                width: 300,
                height: 400
            }
        );
    }

    #[test]
    fn draw_viewport_scissor_partial_overlap() {
        assert_eq!(
            draw_damage_effect(true, Some((0, 0, 500, 500)), on(300, 300, 500, 500)),
            DamageEffect::OnscreenRect {
                x: 300,
                y: 300,
                width: 200,
                height: 200
            }
        );
    }

    #[test]
    fn draw_empty_intersection_produces_no_damage() {
        assert_eq!(
            draw_damage_effect(true, Some((0, 0, 100, 100)), on(200, 200, 100, 100)),
            DamageEffect::NoDamage
        );
    }

    #[test]
    fn draw_user_fbo_produces_no_damage() {
        assert_eq!(
            draw_damage_effect(false, Some((0, 0, 800, 600)), OFF),
            DamageEffect::NoDamage
        );
    }

    #[test]
    fn draw_no_viewport_produces_full_surface() {
        assert_eq!(
            draw_damage_effect(true, None, OFF),
            DamageEffect::FullSurface
        );
    }

    // ---- ScissorState transition tests ----

    #[test]
    fn default_scissor_state_is_disabled() {
        let state = CanvasGLState::default();
        assert_eq!(state.scissor, ScissorState::Disabled);
        assert_eq!(state.last_scissor_rect, None);
    }

    /// Scissor(rect) then Enable(SCISSOR_TEST): draw uses viewport ∩ rect.
    #[test]
    fn scissor_then_enable_uses_intersection() {
        let mut state = CanvasGLState::default();
        state.viewport = Some((0, 0, 1080, 1920));

        // glScissor(100, 200, 300, 400)
        state.last_scissor_rect = Some((100, 200, 300, 400));
        // glEnable(SCISSOR_TEST) — has an explicit rect
        state.scissor = ScissorState::Enabled {
            x: 100,
            y: 200,
            width: 300,
            height: 400,
        };

        assert_eq!(
            draw_damage_effect(true, state.viewport, state.scissor),
            DamageEffect::OnscreenRect {
                x: 100,
                y: 200,
                width: 300,
                height: 400
            }
        );
    }

    /// Enable(SCISSOR_TEST) then Scissor(rect): draw uses viewport ∩ rect
    /// after the explicit Scissor call.
    #[test]
    fn enable_then_scissor_uses_intersection() {
        let mut state = CanvasGLState::default();
        state.viewport = Some((0, 0, 1080, 1920));

        // glEnable(SCISSOR_TEST) — no prior glScissor → EnabledUnknownRect
        state.scissor = ScissorState::EnabledUnknownRect;

        // Before explicit glScissor: falls back to viewport (conservative).
        assert_eq!(
            draw_damage_effect(true, state.viewport, state.scissor),
            DamageEffect::OnscreenRect {
                x: 0,
                y: 0,
                width: 1080,
                height: 1920
            }
        );

        // glScissor(50, 50, 200, 200) — promotes to Enabled with known rect.
        state.last_scissor_rect = Some((50, 50, 200, 200));
        state.scissor = ScissorState::Enabled {
            x: 50,
            y: 50,
            width: 200,
            height: 200,
        };

        assert_eq!(
            draw_damage_effect(true, state.viewport, state.scissor),
            DamageEffect::OnscreenRect {
                x: 50,
                y: 50,
                width: 200,
                height: 200
            }
        );
    }

    /// Disable(SCISSOR_TEST): draw goes back to viewport-only.
    #[test]
    fn disable_reverts_to_viewport_only() {
        let mut state = CanvasGLState::default();
        state.viewport = Some((0, 0, 1080, 1920));
        state.last_scissor_rect = Some((100, 200, 300, 400));
        state.scissor = ScissorState::Enabled {
            x: 100,
            y: 200,
            width: 300,
            height: 400,
        };

        // glDisable(SCISSOR_TEST)
        state.scissor = ScissorState::Disabled;

        assert_eq!(
            draw_damage_effect(true, state.viewport, state.scissor),
            DamageEffect::OnscreenRect {
                x: 0,
                y: 0,
                width: 1080,
                height: 1920
            }
        );
    }

    /// Enable(SCISSOR_TEST) before any Scissor(...) does NOT produce NoDamage.
    /// This is the exact blocker scenario: the draw IS visible but the
    /// initial scissor box is the full drawable, not a zero rect.
    #[test]
    fn enable_without_prior_scissor_falls_back_to_viewport() {
        // glEnable(SCISSOR_TEST) with no prior glScissor → EnabledUnknownRect.
        assert_eq!(
            draw_damage_effect(
                true,
                Some((0, 0, 800, 600)),
                ScissorState::EnabledUnknownRect
            ),
            DamageEffect::OnscreenRect {
                x: 0,
                y: 0,
                width: 800,
                height: 600
            }
        );
    }

    /// Clear with EnabledUnknownRect falls back to FullSurface (conservative).
    #[test]
    fn clear_with_unknown_scissor_rect_is_full_surface() {
        assert_eq!(
            clear_damage_effect(COLOR, true, ScissorState::EnabledUnknownRect, ALL_ON),
            DamageEffect::FullSurface
        );
    }

    /// After explicit Scissor, re-enable uses the known rect (retained).
    #[test]
    fn re_enable_after_disable_uses_retained_rect() {
        let mut state = CanvasGLState::default();
        state.viewport = Some((0, 0, 1080, 1920));

        // glScissor(100, 100, 200, 200) + Enable + Disable
        state.last_scissor_rect = Some((100, 100, 200, 200));
        state.scissor = ScissorState::Disabled;

        // glEnable(SCISSOR_TEST) again — last_scissor_rect is retained
        let (x, y, w, h) = state.last_scissor_rect.unwrap();
        state.scissor = ScissorState::Enabled {
            x,
            y,
            width: w,
            height: h,
        };

        assert_eq!(
            draw_damage_effect(true, state.viewport, state.scissor),
            DamageEffect::OnscreenRect {
                x: 100,
                y: 100,
                width: 200,
                height: 200
            }
        );
    }

    /// **`UseProgram` validates before it dedups, and that order is the point.**
    ///
    /// The obvious optimisation is to check the shadow first: `update_use_program`
    /// keys on the *client* `program_id`, which is in hand before any lookup, so
    /// a redundant `useProgram(sameProgram)` could return without touching the
    /// `programs` map, without the ownership check, and without the deleted
    /// check. That is a hash lookup and two branches saved per redundant call,
    /// and a game that leans on the dedup makes many.
    ///
    /// It is also wrong. Those checks are the only thing that turns
    /// `useProgram(deletedProgram)` and `useProgram(neverCreated)` into errors.
    /// Skip them when the shadow says "already current" and the same call
    /// silently succeeds or fails depending on what the shadow happens to hold —
    /// a game gets an error the first time and nothing the second.
    ///
    /// So the order is asserted here rather than left to be rediscovered. The
    /// dispatch itself needs an EGL context, so this reads the source: crude, and
    /// chosen over a mock GL whose agreement with a driver would be its own open
    /// question. What is in doubt is not GL behaviour but whether the lookup is
    /// still ahead of the dedup.
    #[test]
    fn use_program_validates_before_it_consults_the_shadow() {
        const SRC: &str = include_str!("handler.rs");
        let arm_start = SRC.find("GLCmd::UseProgram {").expect("the UseProgram arm");
        let arm = &SRC[arm_start..arm_start + 2000];
        let arm_end = arm.find("GLCmd::GetAttribLocation").unwrap_or(arm.len());
        let arm = &arm[..arm_end];

        // `useProgram(null)` is program 0 and unbinds: answered before the lookup, which would call it not found.
        let unbind = arm
            .find("program_id == 0")
            .expect("UseProgram no longer handles useProgram(null)");
        let lookup = arm
            .find("cm.programs.get(&program_id)")
            .expect("UseProgram no longer resolves the program id at all");
        assert!(
            unbind < lookup,
            "useProgram(null) must be answered before the program lookup"
        );
        let owner = arm
            .find("check_owner")
            .expect("UseProgram no longer checks program ownership");
        let deleted = arm
            .find("meta.deleted")
            .expect("UseProgram no longer rejects a deleted program");
        let dedup = arm
            .find("st::update_use_program")
            .expect("UseProgram no longer dedups");

        assert!(
            lookup < dedup && owner < dedup && deleted < dedup,
            "the shadow is now consulted before validation (lookup at {lookup}, \
             owner at {owner}, deleted at {deleted}, dedup at {dedup}). A \
             redundant useProgram on a deleted or missing program would then \
             succeed silently, where the first call errored — see this test's \
             doc for why the saved hash lookup is not worth that"
        );
    }

    /// A snapshot upload is accounted under the format a canvas is, whatever the record forwarded.
    ///
    /// The record carries `format` and `type` as zeros when the facade has none to forward, and the budget refuses an
    /// unsized internal format that does not equal its format: so every `texImage2D(canvas)` of a canvas drawn in
    /// the same frame was rejected and left an empty texture. Phaser's Text drew opaque black boxes.
    #[test]
    fn a_canvas_source_upload_is_accounted_as_the_8_bit_pixels_it_is() {
        assert_eq!(
            canvas_source_upload_format(glow::RGBA as i32, 0, 0),
            (glow::RGBA, glow::UNSIGNED_BYTE)
        );
        assert_eq!(
            canvas_source_upload_format(glow::RGB as i32, 0, 0),
            (glow::RGB, glow::UNSIGNED_BYTE)
        );
        // What the content did pass is kept.
        assert_eq!(
            canvas_source_upload_format(glow::RGBA8 as i32, glow::RGBA, glow::UNSIGNED_BYTE),
            (glow::RGBA, glow::UNSIGNED_BYTE)
        );
        const SRC: &str = include_str!("handler.rs");
        let arm = &SRC[SRC.find("GLCmd::TexImage2DFromSnapshot {").unwrap()..];
        let arm = &arm[..arm.find("GLCmd::TexImage2DFromTextCache").unwrap()];
        let mapped = arm
            .find("canvas_source_upload_format(")
            .expect("the arm maps format and type");
        let prepared = arm
            .find("prepare_tex_image_2d(")
            .expect("the arm accounts the allocation");
        assert!(
            mapped < prepared,
            "format and type are mapped before the budget sees them"
        );
    }

    #[test]
    fn content_query_drains_only_requested_program() {
        const SRC: &str = include_str!("handler.rs");
        let start = SRC
            .find("GLCmd::GetProgramParameter {")
            .expect("GetProgramParameter arm exists");
        let end = SRC[start..]
            .find("GLCmd::GetProgramInfoLog")
            .map(|offset| start + offset)
            .expect("GetProgramInfoLog follows parameter arm");
        let parameter_arm = &SRC[start..end];
        assert!(
            parameter_arm.contains("Self::drain_pending_link(cm, gl, program_id)"),
            "content query must target its own pending program"
        );
        assert!(
            !parameter_arm.contains("Self::drain_pending_links(cm, gl, DrainCause::ContentAsked)"),
            "content query must not drain unrelated pending programs"
        );

        let info_start = SRC
            .find("GLCmd::GetProgramInfoLog {")
            .expect("GetProgramInfoLog arm exists");
        let info_end = SRC[info_start..]
            .find("GLCmd::DeleteProgram")
            .map(|offset| info_start + offset)
            .expect("DeleteProgram follows info-log arm");
        let info_arm = &SRC[info_start..info_end];
        assert!(info_arm.contains("Self::drain_pending_link(cm, gl, program_id)"));
    }
}

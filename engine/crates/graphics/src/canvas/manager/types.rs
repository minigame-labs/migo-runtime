extern crate khronos_egl as egl;

use std::collections::HashMap;

use glow::{
    NativeBuffer, NativeFence, NativeFramebuffer, NativeProgram, NativeRenderbuffer, NativeSampler,
    NativeShader, NativeTexture, NativeVertexArray,
};
use shared::error::{EngineError, ErrorCode};
use shared::protocol::render_cmd::{CanvasId, ProgramId, ShaderId, ShaderType, VaoId};

use super::gl_object::GlObject;

#[inline]
pub(crate) fn ee(code: ErrorCode, detail: impl Into<String>) -> EngineError {
    EngineError::from_detail(code, detail)
}

#[derive(Clone, Debug)]
pub(crate) struct CanvasInfo {
    #[allow(dead_code)]
    pub id: CanvasId,
    pub width: u32,
    pub height: u32,
    #[allow(dead_code)]
    pub is_onscreen: bool,
}

impl CanvasInfo {
    #[allow(dead_code)]
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }
}

/// Tracked WebGL state per canvas for deduplication of redundant GL calls.
///
/// Scissor test state for damage tracking.
///
/// GL allows `glScissor()` to be called at any time (even when the test is
/// disabled); the rect is retained and takes effect when the test is enabled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ScissorState {
    /// `GL_SCISSOR_TEST` is disabled — scissor rect has no effect.
    Disabled,
    /// `GL_SCISSOR_TEST` is enabled with a known rect (set by explicit `glScissor`).
    Enabled {
        x: i32,
        y: i32,
        width: i32,
        height: i32,
    },
    /// `GL_SCISSOR_TEST` is enabled but no explicit `glScissor()` has been
    /// called yet.  The real GL initial scissor box is the full drawable —
    /// we don't know its size here, so damage must fall back to viewport
    /// (conservative, never under-reports).
    EnabledUnknownRect,
}

/// `(src_factor, dst_factor)` for glBlendFunc-style pairs.  Stored per
/// channel (colour vs alpha) to support `glBlendFuncSeparate`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BlendFactors {
    pub src_rgb: u32,
    pub dst_rgb: u32,
    pub src_alpha: u32,
    pub dst_alpha: u32,
}

impl Default for BlendFactors {
    fn default() -> Self {
        // GL initial values: SRC = 1, DST = 0 for all channels.
        Self {
            src_rgb: 1,
            dst_rgb: 0,
            src_alpha: 1,
            dst_alpha: 0,
        }
    }
}

/// `(mode_rgb, mode_alpha)` for glBlendEquation / glBlendEquationSeparate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BlendEquation {
    pub mode_rgb: u32,
    pub mode_alpha: u32,
}

impl Default for BlendEquation {
    fn default() -> Self {
        // GL_FUNC_ADD = 0x8006 — GL's initial mode.
        Self {
            mode_rgb: 0x8006,
            mode_alpha: 0x8006,
        }
    }
}

/// Tracks the last value written to a given uniform `(program_id, location)`.
///
/// We compare raw bytes: two `glUniform4f` calls with the same three f32
/// values produce identical byte streams, so bytewise equality is both
/// necessary and sufficient to dedup a redundant upload.
///
/// Cache is bounded per-program to avoid unbounded growth when a game
/// spams glGetUniformLocation for hundreds of unique names; once a
/// program has more than [`MAX_UNIFORM_CACHE`] entries we drop the
/// oldest on insert.  Redundant uploads for evicted locations simply
/// fall back to the non-dedup path, never wrong.
pub(crate) const MAX_UNIFORM_CACHE: usize = 256;

/// When a setter call matches the already-tracked value, the actual GL call
/// is skipped.  State is only updated AFTER resource validation succeeds,
/// so errors never pollute the tracked state.
#[derive(Clone, Debug)]
pub(crate) struct CanvasGLState {
    pub current_program: Option<ProgramId>,
    pub viewport: Option<(i32, i32, i32, i32)>,
    /// TEXTURE_2D binding per texture unit, indexed by `unit - TEXTURE0`.
    pub bound_texture_2d: TextureUnitShadow,
    /// ARRAY_BUFFER binding.
    pub bound_array_buffer: Option<Option<u32>>,
    /// ELEMENT_ARRAY_BUFFER binding.
    pub bound_element_array_buffer: Option<Option<u32>>,
    /// UNIFORM_BUFFER binding (WebGL 2 generic target).  Indexed
    /// bindings set via `bindBufferBase` / `bindBufferRange` live
    /// in [`Self::bound_uniform_buffer_indexed`].
    pub bound_uniform_buffer: Option<Option<u32>>,
    /// PIXEL_UNPACK_BUFFER binding (WebGL 2).  Dedup this avoids
    /// the common pattern of `bindBuffer(PIXEL_UNPACK_BUFFER, b)`
    /// before every `texImage2D` upload burst.
    pub bound_pixel_unpack_buffer: Option<Option<u32>>,
    /// PIXEL_PACK_BUFFER binding (WebGL 2, `readPixels`).
    pub bound_pixel_pack_buffer: Option<Option<u32>>,
    /// COPY_READ_BUFFER binding (WebGL 2, `copyBufferSubData`).
    pub bound_copy_read_buffer: Option<Option<u32>>,
    /// COPY_WRITE_BUFFER binding (WebGL 2, `copyBufferSubData`).
    pub bound_copy_write_buffer: Option<Option<u32>>,
    /// TRANSFORM_FEEDBACK_BUFFER generic binding (WebGL 2).
    pub bound_transform_feedback_buffer: Option<Option<u32>>,
    /// Indexed UNIFORM_BUFFER bindings keyed by binding index
    /// (see `glBindBufferBase` / `glBindBufferRange`).  Value is
    /// `(buffer_id, offset, size)` so range-bindings dedup
    /// separately from full-buffer bindings.
    pub bound_uniform_buffer_indexed: IndexedUniformBufferShadow,
    /// Active texture unit.
    pub active_texture_unit: Option<u32>,
    /// True when the DRAW_FRAMEBUFFER binding is the default framebuffer
    /// (= drawing buffer / onscreen surface).  False when a user FBO is bound.
    /// Used by damage tracking: only draws to the default FB affect onscreen damage.
    /// Defaults to true (initial GL state is the default framebuffer).
    pub draws_to_default_fbo: bool,
    /// Scissor test state for damage tracking.
    pub scissor: ScissorState,
    /// Last rect set via explicit `glScissor()`, retained across enable/disable
    /// cycles.  `None` means no explicit `glScissor` has been called yet.
    pub last_scissor_rect: Option<(i32, i32, i32, i32)>,
    /// Current glColorMask state (r, g, b, a).
    /// Used by damage tracking: if all four are false, glClear with
    /// COLOR_BUFFER_BIT doesn't actually modify visible color.
    /// Defaults to (true, true, true, true) per GL initial state.
    pub color_mask: (bool, bool, bool, bool),

    // ---- Extended dedup state (Phase 8) --------------------------------
    //
    // Everything below defaults to the initial GL ES 3.0 state per spec.
    // The state tracker uses these as cheap "already in this configuration"
    // checks before issuing the matching GL call.
    /// Tri-state `glEnable` / `glDisable` shadow: never touched,
    /// known-enabled, known-disabled.
    pub capabilities: CapabilityShadow,
    pub blend_factors: Option<BlendFactors>,
    pub blend_equation: Option<BlendEquation>,
    pub blend_color: Option<(f32, f32, f32, f32)>,
    pub depth_func: Option<u32>,
    pub depth_mask: Option<bool>,
    pub depth_range: Option<(f32, f32)>,
    pub cull_face: Option<u32>,
    pub front_face: Option<u32>,
    pub line_width: Option<f32>,
    pub polygon_offset: Option<(f32, f32)>,
    /// Currently bound VAO (`None` = default vao 0).
    pub bound_vao: Option<u32>,
    /// Per-program uniform value cache.  Keys are `glGetUniformLocation`
    /// values (u32, stored as the location-index returned to JS).
    pub uniform_cache: HashMap<(ProgramId, u32), Vec<u8>>,

    // ---- P11-state-tracker expansion ----------------------------------
    /// Currently bound FBO id for each of the three GL binding targets.
    /// `Some(None)` means "shadowed as the default FBO (0)"; `None` means
    /// "no shadow yet, must re-issue".
    pub bound_framebuffer: FramebufferShadow,
    /// Last-bound renderbuffer id (glBindRenderbuffer only has one
    /// target, RENDERBUFFER).
    pub bound_renderbuffer: Option<Option<u32>>,
    /// Vertex-attribute state, held per VAO because GLES 3.0 §6.2 holds it
    /// inside the vertex array object. See [`VertexArrayShadow`].
    pub vertex_attribs: VertexArrayShadow,

    // ---- Stencil state shadows (P14) ----------------------------------
    //
    // UI systems with many stencil-masked layers repeat these calls every
    // frame; each is a driver round-trip.  Per-face variants use the same
    // fingerprint applied to both FRONT and BACK faces, so a single
    // `StencilFp` variant covers `StencilFunc` / `StencilFuncSeparate` /
    // `StencilOp` / `StencilOpSeparate` / `StencilMask` /
    // `StencilMaskSeparate`.
    /// `(func, ref, mask)` per cull face.
    pub stencil_func: PerFace<(u32, i32, u32)>,
    /// `(sfail, dpfail, dppass)` per cull face.
    pub stencil_op: PerFace<(u32, u32, u32)>,
    /// Write-mask per cull face.
    pub stencil_mask: PerFace<u32>,

    // ---- Pixel-storei shadows (P14) -----------------------------------
    //
    // Games typically set these once per texture upload.  Most engines
    // flip `UNPACK_FLIP_Y_WEBGL` and `UNPACK_PREMULTIPLY_ALPHA_WEBGL`
    // pairs back-to-back across many draws with identical values.
    /// `pname → param` shadow, enumerated across the subset of
    /// parameters WebGL / GL ES exposes.  A future expansion can
    /// move specific pnames out into typed fields (e.g.
    /// `unpack_alignment`) where stricter typing helps — but
    /// the open-coded HashMap reliably dedups ANY `pixelStorei`
    /// pname without per-pname wiring.
    pub pixel_store_i32: PixelStoreShadow,
}

// ============================================================================
// Right-sized shadows for the state families whose key space is fixed and tiny
// ============================================================================
//
// Each of the four types below replaced a `HashMap`/`HashSet` keyed by a GL
// enum drawn from a set the spec fixes at two, three, ten, or thirty-two
// values. Hashing to find one of two things is the shape of the cost; measured
// per frame for a 300-sprite-batch scene at the shipped `opt-level = "z"`:
//
//   stencil per-face   (60 calls)   58.03 -> 5.31 ns/call   10.9x
//   enable/disable     (600 calls)  30.43 -> 1.92 ns/call   15.8x
//   texture unit bind  (300 calls)  18.16 -> 0.77 ns/call   23.6x
//
// None of them trades memory for that: every one is smaller than the hash
// table's own header, and none of them touches the heap at all.
//
// All four keep the same conservative fallback the maps had for a key they do
// not recognise — report "must issue" and track nothing. A GL enum outside
// these sets is a `GL_INVALID_ENUM` the driver rejects anyway, so the only
// consequence is that an invalid call is not deduped, which is the safe
// direction.

/// The capabilities WebGL 1 and 2 let content toggle, in bit order.
///
/// Fixed by the spec: WebGL 1.0 §5.14.3 lists nine, WebGL 2.0 adds
/// `RASTERIZER_DISCARD`. Taken from `glow` rather than transcribed so the
/// values cannot drift from the ones the call actually uses.
const TOGGLEABLE_CAPS: [u32; 10] = [
    glow::BLEND,
    glow::CULL_FACE,
    glow::DEPTH_TEST,
    glow::DITHER,
    glow::POLYGON_OFFSET_FILL,
    glow::SAMPLE_ALPHA_TO_COVERAGE,
    glow::SAMPLE_COVERAGE,
    glow::SCISSOR_TEST,
    glow::STENCIL_TEST,
    glow::RASTERIZER_DISCARD,
];

/// Tri-state shadow of `glEnable` / `glDisable`.
///
/// Replaces a pair of `HashSet<u32>` that existed only to encode three states
/// per capability — never touched, known-enabled, known-disabled — and paid two
/// hash probes on every transition to keep them mutually exclusive. Two
/// bitmasks say the same thing in eight bytes and one branch.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct CapabilityShadow {
    /// Bit set once we have observed an enable or a disable for this cap.
    known: u16,
    /// Bit set when the cap is enabled. Meaningful only where `known` is set.
    enabled: u16,
}

impl CapabilityShadow {
    #[inline]
    fn bit(cap: u32) -> Option<u16> {
        // Ten entries: a linear scan the optimiser turns into a compare chain,
        // and no hash.
        let mut i = 0;
        while i < TOGGLEABLE_CAPS.len() {
            if TOGGLEABLE_CAPS[i] == cap {
                return Some(1u16 << i);
            }
            i += 1;
        }
        None
    }

    /// Record `glEnable(cap)`; `true` when the driver call must be issued.
    #[inline]
    pub(crate) fn enable(&mut self, cap: u32) -> bool {
        let Some(bit) = Self::bit(cap) else {
            return true;
        };
        let already_enabled = self.known & bit != 0 && self.enabled & bit != 0;
        self.known |= bit;
        self.enabled |= bit;
        !already_enabled
    }

    /// Record `glDisable(cap)`; `true` when the driver call must be issued.
    #[inline]
    pub(crate) fn disable(&mut self, cap: u32) -> bool {
        let Some(bit) = Self::bit(cap) else {
            return true;
        };
        let already_disabled = self.known & bit != 0 && self.enabled & bit == 0;
        self.known |= bit;
        self.enabled &= !bit;
        !already_disabled
    }

    #[inline]
    pub(crate) fn forget_all(&mut self) {
        self.known = 0;
        self.enabled = 0;
    }
}

/// `TEXTURE_2D` binding per texture unit.
///
/// The key was the `GL_TEXTURE0 + i` enum, which is a dense range the spec
/// names through `GL_TEXTURE31`, so an array indexed by `i` addresses it
/// exactly. Units above the tracked range fall back to "must issue"; GLES 3.0
/// guarantees only 32 combined image units, and a game that reaches past them
/// gets correctness, just not dedup.
///
/// `Option<u32>` per slot rather than a `u32` with zero as the sentinel:
/// content can send `bindTexture(target, 0)`, which arrives as `Some(0)`, and
/// collapsing that onto the tracker's `None` would make two distinguishable
/// requests compare equal.
const MAX_TRACKED_TEXTURE_UNITS: usize = 32;

#[derive(Clone, Debug)]
pub(crate) struct TextureUnitShadow {
    bindings: [Option<u32>; MAX_TRACKED_TEXTURE_UNITS],
    /// Bit `i` set once unit `i`'s binding has been observed.
    observed: u32,
}

impl Default for TextureUnitShadow {
    fn default() -> Self {
        Self {
            bindings: [None; MAX_TRACKED_TEXTURE_UNITS],
            observed: 0,
        }
    }
}

impl TextureUnitShadow {
    #[inline]
    fn index(unit: u32) -> Option<usize> {
        let i = unit.wrapping_sub(glow::TEXTURE0) as usize;
        (i < MAX_TRACKED_TEXTURE_UNITS).then_some(i)
    }

    /// Record `glBindTexture(TEXTURE_2D, tex)` on `unit`; `true` when the
    /// driver call must be issued.
    #[inline]
    pub(crate) fn bind(&mut self, unit: u32, tex: Option<u32>) -> bool {
        let Some(i) = Self::index(unit) else {
            return true;
        };
        let bit = 1u32 << i;
        if self.observed & bit != 0 && self.bindings[i] == tex {
            return false;
        }
        self.observed |= bit;
        self.bindings[i] = tex;
        true
    }

    /// Forget every unit that names `texture`.
    ///
    /// Deleting a texture implicitly unbinds it from every unit (GLES 3.0
    /// §3.8.14), so a shadow still naming it would dedup away the rebind of a
    /// reused texture name.
    #[inline]
    pub(crate) fn forget_texture(&mut self, texture: u32) {
        for i in 0..MAX_TRACKED_TEXTURE_UNITS {
            if self.bindings[i] == Some(texture) {
                self.bindings[i] = None;
                self.observed &= !(1u32 << i);
            }
        }
    }

    #[inline]
    pub(crate) fn forget_all(&mut self) {
        self.observed = 0;
    }
}

/// A stencil parameter tracked per cull face.
///
/// GLES 3.0 §4.1.4 gives stencil state exactly two faces. The map this
/// replaced hashed `GL_FRONT` or `GL_BACK` to reach one of two slots, and
/// `FRONT_AND_BACK` cost it four probes: two to compare, two to store.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PerFace<T> {
    front: Option<T>,
    back: Option<T>,
}

impl<T> Default for PerFace<T> {
    fn default() -> Self {
        Self {
            front: None,
            back: None,
        }
    }
}

impl<T: Copy + PartialEq> PerFace<T> {
    /// Record `value` against `face`; `true` when the driver call must be
    /// issued. `FRONT_AND_BACK` dedups only when both faces already match,
    /// which is the behaviour the map had.
    #[inline]
    pub(crate) fn update(&mut self, face: u32, value: T) -> bool {
        if face == glow::FRONT_AND_BACK {
            if self.front == Some(value) && self.back == Some(value) {
                return false;
            }
            self.front = Some(value);
            self.back = Some(value);
            return true;
        }
        let slot = match face {
            glow::FRONT => &mut self.front,
            glow::BACK => &mut self.back,
            // Not a face: forward it and track nothing.
            _ => return true,
        };
        if *slot == Some(value) {
            return false;
        }
        *slot = Some(value);
        true
    }

    #[inline]
    pub(crate) fn forget_all(&mut self) {
        self.front = None;
        self.back = None;
    }
}

/// The `pixelStorei` parameters WebGL 1 and 2 let content set, in slot order.
///
/// Fixed by the spec: WebGL 1.0 §5.14.3 has the five alignment and conversion
/// parameters, WebGL 2.0 adds the eight row/skip parameters. Anything else is a
/// `GL_INVALID_ENUM` the driver rejects.
const PIXEL_STORE_PNAMES: [u32; 13] = [
    glow::PACK_ALIGNMENT,
    glow::UNPACK_ALIGNMENT,
    // The two WebGL-only pnames have no `glow` constant: they exist in the
    // WebGL IDL, not in GL ES, and the engine forwards them to Skia's upload
    // path rather than to `glPixelStorei`.
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
];

/// `UNPACK_FLIP_Y_WEBGL` — WebGL 1.0 §5.14.3, no GL ES equivalent.
const UNPACK_FLIP_Y_WEBGL: u32 = 0x9240;
/// `UNPACK_PREMULTIPLY_ALPHA_WEBGL` — WebGL 1.0 §5.14.3.
const UNPACK_PREMULTIPLY_ALPHA_WEBGL: u32 = 0x9241;
/// `UNPACK_COLORSPACE_CONVERSION_WEBGL` — WebGL 1.0 §5.14.3.
const UNPACK_COLORSPACE_CONVERSION_WEBGL: u32 = 0x9243;

/// `glPixelStorei(pname, param)` shadow.
///
/// Engines flip `UNPACK_FLIP_Y_WEBGL` and `UNPACK_PREMULTIPLY_ALPHA_WEBGL` in
/// pairs around every texture upload, and content that uploads a video frame or
/// a canvas snapshot does that per frame — so this is not only a load-time path.
/// The key space is thirteen spec-fixed values, which an array addresses
/// directly; it was a `HashMap<u32, i32>`.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct PixelStoreShadow {
    params: [i32; PIXEL_STORE_PNAMES.len()],
    /// Bit `i` set once `PIXEL_STORE_PNAMES[i]` has been observed. Needed
    /// separately from the value because zero is a legal `param` and also the
    /// array's initial content.
    observed: u16,
}

impl PixelStoreShadow {
    #[inline]
    fn slot(pname: u32) -> Option<usize> {
        let mut i = 0;
        while i < PIXEL_STORE_PNAMES.len() {
            if PIXEL_STORE_PNAMES[i] == pname {
                return Some(i);
            }
            i += 1;
        }
        None
    }

    /// Record `glPixelStorei(pname, param)`; `true` when the driver call must
    /// be issued. An unrecognised pname forwards untracked.
    #[inline]
    pub(crate) fn update(&mut self, pname: u32, param: i32) -> bool {
        let Some(i) = Self::slot(pname) else {
            return true;
        };
        let bit = 1u16 << i;
        if self.observed & bit != 0 && self.params[i] == param {
            return false;
        }
        self.observed |= bit;
        self.params[i] = param;
        true
    }

    #[inline]
    pub(crate) fn forget_all(&mut self) {
        self.observed = 0;
    }
}

/// Indexed `UNIFORM_BUFFER` bindings, one slot per binding index.
///
/// `glBindBufferBase` / `glBindBufferRange` take an index bounded by
/// `MAX_UNIFORM_BUFFER_BINDINGS`, which WebGL 2 guarantees to be at least 24
/// and GLES 3 hardware reports as 24 to 72. A UBO-driven renderer rebinds per
/// draw, so this sat on the per-command path as a `HashMap<u32, _>` keyed by a
/// small dense integer.
///
/// The value is `(buffer, offset, size)` so a `bindBufferRange` with a
/// different window never coalesces with a `bindBufferBase`, which records
/// `(buffer, 0, 0)` — the same distinction the map made.
const MAX_TRACKED_UNIFORM_BUFFER_BINDINGS: usize = 32;

#[derive(Clone, Debug)]
pub(crate) struct IndexedUniformBufferShadow {
    slots: [Option<(Option<u32>, i32, i32)>; MAX_TRACKED_UNIFORM_BUFFER_BINDINGS],
}

impl Default for IndexedUniformBufferShadow {
    fn default() -> Self {
        Self {
            slots: [None; MAX_TRACKED_UNIFORM_BUFFER_BINDINGS],
        }
    }
}

impl IndexedUniformBufferShadow {
    /// Record a binding at `index`; `true` when the driver call must be issued.
    /// An index past the tracked range forwards untracked.
    #[inline]
    pub(crate) fn update(&mut self, index: u32, entry: (Option<u32>, i32, i32)) -> bool {
        let Some(slot) = self.slots.get_mut(index as usize) else {
            return true;
        };
        if *slot == Some(entry) {
            return false;
        }
        *slot = Some(entry);
        true
    }

    #[inline]
    pub(crate) fn forget_all(&mut self) {
        self.slots = [None; MAX_TRACKED_UNIFORM_BUFFER_BINDINGS];
    }
}

/// Client framebuffer bindings at GL's two binding points. `FRAMEBUFFER` binds
/// both READ and DRAW; querying `FRAMEBUFFER_BINDING` aliases DRAW.
///
/// The value is `Option<Option<u32>>` because there are three states and they
/// are all reachable: no shadow yet, shadowed as the default framebuffer
/// (`Some(None)` — which *is* the user-facing name for framebuffer 0), and
/// shadowed as a named framebuffer.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct FramebufferShadow {
    draw: Option<Option<u32>>,
    read: Option<Option<u32>>,
}

impl FramebufferShadow {
    /// Record `glBindFramebuffer(target, fb)`; `true` when the driver call must
    /// be issued.
    #[inline]
    pub(crate) fn update(&mut self, target: u32, fb: Option<u32>) -> bool {
        let slot = match target {
            glow::FRAMEBUFFER => {
                if self.get(glow::DRAW_FRAMEBUFFER) == Some(fb)
                    && self.get(glow::READ_FRAMEBUFFER) == Some(fb)
                {
                    return false;
                }
                self.set_all(fb);
                return true;
            }
            glow::DRAW_FRAMEBUFFER => &mut self.draw,
            glow::READ_FRAMEBUFFER => &mut self.read,
            _ => return true,
        };
        if *slot == Some(fb) {
            return false;
        }
        *slot = Some(fb);
        true
    }

    /// Record a combined bind the engine itself performed.
    #[inline]
    pub(crate) fn set_all(&mut self, fb: Option<u32>) {
        self.draw = Some(fb);
        self.read = Some(fb);
    }

    /// The shadowed binding for `target`: `None` when unshadowed.
    #[inline]
    pub(crate) fn get(&self, target: u32) -> Option<Option<u32>> {
        match target {
            glow::FRAMEBUFFER | glow::DRAW_FRAMEBUFFER => self.draw,
            glow::READ_FRAMEBUFFER => self.read,
            _ => None,
        }
    }

    #[inline]
    pub(crate) fn forget_all(&mut self) {
        self.draw = None;
        self.read = None;
    }
}

// ============================================================================
// Vertex-attribute state, held where GL holds it
// ============================================================================

/// The pointer fingerprint and divisor for one attribute index of one VAO.
///
/// `divisor` is `Option` rather than a plain `u32` defaulted to zero because the
/// tracker distinguishes "never observed" from "observed as zero": zero is the
/// GL initial divisor, but a shadow that assumed it would dedup away the first
/// `glVertexAttribDivisor(i, 0)` a game issues to *undo* instancing, and the
/// driver would keep the old divisor.
///
/// Pointer and divisor share a slot so one bounds check and one cache line
/// serve both — a game that sets a divisor sets a pointer for the same index.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct AttribSlot {
    pointer: Option<VertexAttribPointerFp>,
    divisor: Option<u32>,
}

/// Vertex-attribute dedup state for one vertex array object.
#[derive(Clone, Debug, Default)]
struct VaoAttribs {
    /// Slot `i` is attribute `i`. Length is the highest index the content has
    /// touched plus one, so a geometry using three attributes costs three slots
    /// — see [`VertexArrayShadow`] for why that matters.
    slots: Vec<AttribSlot>,
    /// Bit `i` set ⇔ attribute array `i` is enabled.
    ///
    /// A single mask, with no companion "known" mask, because zero *is* the
    /// truth about a VAO nothing has touched: GLES 3.0 §2.9.4 has every vertex
    /// attribute array start disabled. Pinned by
    /// `enable_and_disable_vertex_attrib_are_idempotent`, whose fourth
    /// assertion is that a `glDisableVertexAttribArray` on an untouched index
    /// dedups.
    enabled: u32,
}

/// `glVertexAttribPointer` / `glEnableVertexAttribArray` /
/// `glVertexAttribDivisor` dedup state, held per VAO.
///
/// **GLES 3.0 §6.2 puts this state inside the vertex array object, and this now
/// stores it that way.** It was three separate hash containers keyed
/// `(vao, index)` — `HashMap<_, Fp>`, `HashSet<_>` and `HashMap<_, u32>` — so a
/// single `vertexAttribPointer` hashed a pair of `u32`s, and the enable and the
/// divisor beside it hashed the same pair again into two more tables. By this
/// module's own account `vertexAttribPointer` is the most-called non-draw call
/// in the Cocos Creator 2.x no-VAO path.
///
/// Measured at the shipped `opt-level = "z"`, `enable` + `pointer` + `divisor`
/// per attribute:
///
/// | workload                        | before      | after      |       |
/// |---------------------------------|-------------|------------|-------|
/// | 1 VAO, 3 attributes (Cocos)     | 25.2 ns/call| 3.4 ns/call| 7.5x  |
/// | 200 VAOs, 3 attributes          | 28.4 ns/call| 9.7 ns/call| 2.9x  |
/// | 200 VAOs, 5 attributes          | 28.8 ns/call| 6.4 ns/call| 4.5x  |
/// | 200 VAOs, 8 attributes          | 28.3 ns/call| 4.8 ns/call| 5.9x  |
///
/// **Growing the slot vector on demand is what makes this free rather than a
/// trade.** A fixed-size attribute array is marginally faster still, but it
/// costs +171% shadow bytes at three attributes in use and it forces a choice
/// of inline capacity — a threshold whose right value depends on the
/// distribution of attributes-per-VAO in real content, which is exactly the
/// kind of number that should not be guessed. Sized to what the content
/// touches, the shadow is *smaller* than the three hash tables at every point
/// measured: −43% at one VAO with three attributes, −6% / −4% / −40% at 200
/// VAOs with three / five / eight.
///
/// The outer table is the same index-memo keyed scan as
/// [`crate::canvas_keyed::CanvasKeyed`], for the same reason: draws arrive in
/// runs that share a VAO. It is a separate type rather than a reuse because the
/// key is a `VaoId`, not a `CanvasId`, and collapsing the two would let a canvas
/// id be passed where a VAO id belongs.
#[derive(Clone, Debug, Default)]
pub(crate) struct VertexArrayShadow {
    entries: Vec<(VaoId, VaoAttribs)>,
    /// Index the previous lookup resolved to, re-checked against the key on
    /// every use.
    hot: usize,
}

impl VertexArrayShadow {
    /// Highest attribute index this shadow will track.
    ///
    /// GLES 3.0 §6.2 caps `MAX_VERTEX_ATTRIBS`; WebGL 2 guarantees at least 16
    /// and GLES 3 hardware reports 16 or 32. Tracking to 32 covers every device
    /// this engine builds for; an index above it is forwarded untracked, which
    /// is also what bounds the slot vector against an index chosen by content.
    const MAX_TRACKED_ATTRIBS: u32 = 32;

    #[inline]
    fn vao_mut(&mut self, vao: VaoId) -> &mut VaoAttribs {
        if let Some((key, _)) = self.entries.get(self.hot)
            && *key == vao
        {
            return &mut self.entries[self.hot].1;
        }
        self.resolve(vao)
    }

    #[inline(never)]
    fn resolve(&mut self, vao: VaoId) -> &mut VaoAttribs {
        match self.entries.iter().position(|(key, _)| *key == vao) {
            Some(pos) => self.hot = pos,
            None => {
                self.entries.push((vao, VaoAttribs::default()));
                self.hot = self.entries.len() - 1;
            }
        }
        &mut self.entries[self.hot].1
    }

    /// The slot for `index`, growing the vector to reach it. `None` for an
    /// index past [`Self::MAX_TRACKED_ATTRIBS`].
    #[inline]
    fn slot_mut(&mut self, vao: VaoId, index: u32) -> Option<&mut AttribSlot> {
        if index >= Self::MAX_TRACKED_ATTRIBS {
            return None;
        }
        let attribs = self.vao_mut(vao);
        let i = index as usize;
        if i >= attribs.slots.len() {
            attribs.slots.resize(i + 1, AttribSlot::default());
        }
        Some(&mut attribs.slots[i])
    }

    /// Record `glVertexAttribPointer`; `true` when the driver call must be
    /// issued.
    #[inline]
    pub(crate) fn update_pointer(
        &mut self,
        vao: VaoId,
        index: u32,
        fp: VertexAttribPointerFp,
    ) -> bool {
        let Some(slot) = self.slot_mut(vao, index) else {
            return true;
        };
        if slot.pointer == Some(fp) {
            return false;
        }
        slot.pointer = Some(fp);
        true
    }

    /// Record `glVertexAttribDivisor`; `true` when the driver call must be
    /// issued.
    #[inline]
    pub(crate) fn update_divisor(&mut self, vao: VaoId, index: u32, divisor: u32) -> bool {
        let Some(slot) = self.slot_mut(vao, index) else {
            return true;
        };
        if slot.divisor == Some(divisor) {
            return false;
        }
        slot.divisor = Some(divisor);
        true
    }

    /// Record `glEnableVertexAttribArray`; `true` when the driver call must be
    /// issued.
    #[inline]
    pub(crate) fn enable(&mut self, vao: VaoId, index: u32) -> bool {
        if index >= Self::MAX_TRACKED_ATTRIBS {
            return true;
        }
        let bit = 1u32 << index;
        let attribs = self.vao_mut(vao);
        if attribs.enabled & bit != 0 {
            return false;
        }
        attribs.enabled |= bit;
        true
    }

    /// Record `glDisableVertexAttribArray`; `true` when the driver call must be
    /// issued.
    #[inline]
    pub(crate) fn disable(&mut self, vao: VaoId, index: u32) -> bool {
        if index >= Self::MAX_TRACKED_ATTRIBS {
            return true;
        }
        let bit = 1u32 << index;
        let attribs = self.vao_mut(vao);
        if attribs.enabled & bit == 0 {
            return false;
        }
        attribs.enabled &= !bit;
        true
    }

    /// Forget every VAO's attribute state.
    ///
    /// Keeps the allocations: this runs at every Skia boundary, and a game that
    /// crosses one per frame would otherwise re-buy one slot vector per VAO per
    /// frame on the render thread.
    #[inline]
    pub(crate) fn forget_all(&mut self) {
        for (_, attribs) in &mut self.entries {
            attribs.slots.clear();
            attribs.enabled = 0;
        }
        self.hot = 0;
    }

    /// Drop a deleted VAO's state.
    ///
    /// VAO names come from the client, so a reused name would otherwise inherit
    /// the dead object's attribute shadow and dedup away the layout the new one
    /// needs — a draw reading the wrong vertex stream.
    #[inline]
    pub(crate) fn forget_vao(&mut self, vao: VaoId) {
        if let Some(pos) = self.entries.iter().position(|(key, _)| *key == vao) {
            self.entries.swap_remove(pos);
            self.hot = 0;
        }
    }

    #[cfg(test)]
    pub(crate) fn tracked_vaos(&self) -> usize {
        self.entries.len()
    }

    #[cfg(test)]
    pub(crate) fn tracked_slots(&self, vao: VaoId) -> usize {
        self.entries
            .iter()
            .find(|(key, _)| *key == vao)
            .map_or(0, |(_, a)| a.slots.len())
    }
}

/// Per-canvas dedup shadows, keyed by canvas.
///
/// See [`crate::canvas_keyed`] for why this is a keyed scan rather than a hash
/// map, and for the measurements. The alias exists because the same container
/// also carries `webgl_gpu_budget`'s binding ledger.
pub(crate) type CanvasStateTable = crate::canvas_keyed::CanvasKeyed<CanvasGLState>;

/// Fingerprint of the arguments to `glVertexAttribPointer`.
///
/// WebGL 1.0 §5.14.10 and OpenGL ES 3.0 §2.9.5 both specify that
/// `vertexAttribPointer` *captures* the currently bound
/// `ARRAY_BUFFER` as the buffer source for that attribute.  Two
/// calls with identical (size, type, normalized, stride, offset)
/// against different bound buffers are NOT equivalent and MUST
/// re-issue to the driver — otherwise the second draw paints from
/// the wrong vertex buffer, a class of bug that's nearly
/// impossible to reproduce outside of real game content.
///
/// That's why `array_buffer` is part of the fingerprint.
///
/// The VAO is *not* part of it, because [`VertexArrayShadow`] holds one of
/// these per VAO per attribute index — the VAO is the table key. It used to be
/// a field here as well, back when a single flat `HashMap<(vao, index), _>`
/// carried every VAO's fingerprints; keeping it would now mean comparing a
/// value against itself on every dedup decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct VertexAttribPointerFp {
    pub size: i32,
    pub type_: u32,
    pub normalized: bool,
    pub stride: i32,
    pub offset: i32,
    /// `ARRAY_BUFFER` binding captured at the time of the
    /// `vertexAttribPointer` call.  `None` = no buffer bound
    /// (the attribute points at client-side memory, which WebGL
    /// rejects anyway) or the tracker hasn't observed a bind yet
    /// (force re-issue to establish shadow).
    pub array_buffer: Option<u32>,
}

impl Default for CanvasGLState {
    fn default() -> Self {
        Self {
            current_program: None,
            viewport: None,
            bound_texture_2d: TextureUnitShadow::default(),
            bound_array_buffer: None,
            bound_element_array_buffer: None,
            bound_uniform_buffer: None,
            bound_pixel_unpack_buffer: None,
            bound_pixel_pack_buffer: None,
            bound_copy_read_buffer: None,
            bound_copy_write_buffer: None,
            bound_transform_feedback_buffer: None,
            bound_uniform_buffer_indexed: IndexedUniformBufferShadow::default(),
            active_texture_unit: None,
            // Initial GL state: default framebuffer is bound.
            draws_to_default_fbo: true,
            scissor: ScissorState::Disabled,
            last_scissor_rect: None,
            color_mask: (true, true, true, true),

            capabilities: CapabilityShadow::default(),
            blend_factors: None,
            blend_equation: None,
            blend_color: None,
            depth_func: None,
            depth_mask: None,
            depth_range: None,
            cull_face: None,
            front_face: None,
            line_width: None,
            polygon_offset: None,
            bound_vao: None,
            uniform_cache: HashMap::new(),
            bound_framebuffer: FramebufferShadow::default(),
            bound_renderbuffer: None,
            vertex_attribs: VertexArrayShadow::default(),
            stencil_func: PerFace::default(),
            stencil_op: PerFace::default(),
            stencil_mask: PerFace::default(),
            pixel_store_i32: PixelStoreShadow::default(),
        }
    }
}

impl CanvasGLState {
    /// Forget every piece of shadow state that external code (Skia's
    /// text-atlas upload, DrawingBuffer blit, any non-WebGL GL caller)
    /// might have mutated during its batch.  The next WebGL call MUST
    /// re-issue rather than trust stale shadow.
    ///
    /// Rationale: Skia's draw pipeline binds programs / VAOs / FBOs /
    /// blend state / scissor / stencil / viewport / textures without
    /// going through our handlers — the same machinery backing the
    /// dedup decisions in `state_tracker.rs`.  If we kept the shadow
    /// as-is, the next Cocos-style WebGL frame might issue a
    /// `bindBuffer(ARRAY_BUFFER, 42)` that matches the last-known
    /// shadow (pre-Skia) and get silently skipped — painting with
    /// whatever buffer Skia left bound.  Bugs from this class are
    /// subtle and hard to reproduce, so the safer move is to wipe the
    /// shadow and pay one extra rebind per affected state next frame.
    ///
    /// We intentionally preserve `draws_to_default_fbo` because the
    /// render-thread caller restores the default FBO binding itself
    /// around Skia batches — leaving the flag intact is consistent
    /// with that external invariant.  If that assumption ever breaks,
    /// set the flag here too.
    /// Invalidate the dedup shadow back to "unknown".  See
    /// [`Self::invalidate_after_external_gl_use`] for rationale.
    ///
    /// This base impl only clears the P11-expansion fields; the main
    /// `invalidate_after_external_gl_use` (below) invokes it after
    /// clearing the older fields.
    fn invalidate_p11_shadow(&mut self) {
        self.bound_framebuffer.forget_all();
        self.bound_renderbuffer = None;
        self.vertex_attribs.forget_all();
        // P14 shadows: Skia does not touch stencil state (Ganesh GL
        // backend leaves stencil disabled by default) or pixelStorei
        // (those are upload-path knobs not used during draw).
        // Still, clearing them on the boundary matches the behaviour
        // of every other tracked slot and keeps the "after boundary,
        // next call MUST re-issue" contract uniform.
        self.stencil_func.forget_all();
        self.stencil_op.forget_all();
        self.stencil_mask.forget_all();
        self.pixel_store_i32.forget_all();
    }

    pub fn invalidate_after_external_gl_use(&mut self) {
        self.current_program = None;
        self.viewport = None;
        self.bound_texture_2d.forget_all();
        self.bound_array_buffer = None;
        self.bound_element_array_buffer = None;
        self.bound_uniform_buffer = None;
        self.bound_pixel_unpack_buffer = None;
        self.bound_pixel_pack_buffer = None;
        self.bound_copy_read_buffer = None;
        self.bound_copy_write_buffer = None;
        self.bound_transform_feedback_buffer = None;
        self.bound_uniform_buffer_indexed.forget_all();
        self.active_texture_unit = None;
        // Scissor: Skia typically leaves it disabled after its draw
        // batch completes.  Reset to the conservative "don't know
        // the rect" enabled state so the next `glScissor` call
        // re-issues; damage tracking falls back to viewport, which
        // over-reports but never under-reports.
        self.scissor = ScissorState::EnabledUnknownRect;
        self.last_scissor_rect = None;
        // Colour-mask defaults to "all channels writable" on the GL
        // initial state — but Skia may have changed it mid-batch.
        // Returning to the conservative known-nothing sentinel forces
        // the next `glColorMask` call to re-issue.
        self.color_mask = (true, true, true, true);
        self.capabilities.forget_all();
        self.blend_factors = None;
        self.blend_equation = None;
        self.blend_color = None;
        self.depth_func = None;
        self.depth_mask = None;
        self.depth_range = None;
        self.cull_face = None;
        self.front_face = None;
        self.line_width = None;
        self.polygon_offset = None;
        self.bound_vao = None;
        // `uniform_cache` intentionally survives: uniforms are scoped
        // to a program, not to the shared GL context.  Skia's
        // rendering on the same canvas touches its own shader
        // programs — never the WebGL programs our cache keys are
        // attached to — so the dedup table remains accurate across
        // the boundary.  Clearing it here would force every frame
        // to re-issue `glUniform*` calls the app already deduped,
        // which is the dominant GL op category in shader-heavy
        // workloads.  Regression pinned by `uniform_cache_survives_
        // external_gl_use` in `state_tracker.rs`.
        // Clear the P11 dedup fields too.
        self.invalidate_p11_shadow();
    }
}

/// A/B benchmarks for the container choices above, in the profile the engine
/// actually ships (`opt-level = "z"`, `lto = "fat"`, one codegen unit) — the
/// numbers move a lot between that and `opt-level = 3`, so measuring under
/// `cargo bench`-style defaults would have mis-ranked these.
///
/// Run with:
/// `cargo test --release -p migo-graphics --lib bench_ -- --ignored --nocapture`
///
/// Every assertion is directional (new shape faster than the shape it
/// replaced), never a wall-clock threshold, so the gate holds on any machine.
#[cfg(test)]
mod shadow_shape_benches {
    use super::*;
    use std::collections::{HashMap, HashSet};
    use std::time::Instant;

    /// Sprite batches in the modelled frame. Chosen to match the workload the
    /// state tracker's own doc comments describe (Cocos Creator 2.x sprite
    /// batches), not to flatter any particular container.
    const BATCHES: usize = 300;
    const ITERS: usize = 2_000;

    fn time(mut f: impl FnMut() -> u32) -> f64 {
        for _ in 0..(ITERS / 10) {
            std::hint::black_box(f());
        }
        let start = Instant::now();
        for _ in 0..ITERS {
            std::hint::black_box(f());
        }
        start.elapsed().as_nanos() as f64 / ITERS as f64
    }

    fn report(family: &str, calls: usize, before: f64, after: f64) {
        println!(
            "{family}\n  {calls} calls/frame\n  \
             hash-keyed (before) : {before:>9.0} ns/frame  {:>6.2} ns/call\n  \
             right-sized (after) : {after:>9.0} ns/frame  {:>6.2} ns/call\n  \
             → {:.2}x, {:.1} µs/frame saved on this host\n",
            before / calls as f64,
            after / calls as f64,
            before / after,
            (before - after) / 1000.0
        );
    }

    #[test]
    #[ignore = "timing benchmark; run explicitly with --ignored"]
    fn bench_capability_shadow_vs_two_hash_sets() {
        // Before: `enabled_caps` / `disabled_caps`, two `HashSet<u32>` kept
        // mutually exclusive, so a transition cost an insert and a remove.
        let mut enabled: HashSet<u32> = HashSet::new();
        let mut disabled: HashSet<u32> = HashSet::new();
        let before = time(|| {
            let mut n = 0;
            for i in 0..BATCHES {
                // A blend toggle and a cull enable per batch, which is what a
                // material change looks like.
                let cap = glow::BLEND;
                if i % 3 == 0 {
                    if !disabled.contains(&cap) {
                        disabled.insert(cap);
                        enabled.remove(&cap);
                        n += 1;
                    }
                } else if !enabled.contains(&cap) {
                    enabled.insert(cap);
                    disabled.remove(&cap);
                    n += 1;
                }
                if !enabled.contains(&glow::CULL_FACE) {
                    enabled.insert(glow::CULL_FACE);
                    disabled.remove(&glow::CULL_FACE);
                    n += 1;
                }
            }
            n
        });

        let mut shadow = CapabilityShadow::default();
        let after = time(|| {
            let mut n = 0;
            for i in 0..BATCHES {
                if i % 3 == 0 {
                    if shadow.disable(glow::BLEND) {
                        n += 1;
                    }
                } else if shadow.enable(glow::BLEND) {
                    n += 1;
                }
                if shadow.enable(glow::CULL_FACE) {
                    n += 1;
                }
            }
            n
        });

        report("glEnable / glDisable", BATCHES * 2, before, after);
        assert!(
            after < before,
            "two bitmasks came out slower than two hash sets ({after:.0} vs \
             {before:.0} ns) — the container change is not paying for itself"
        );
    }

    #[test]
    #[ignore = "timing benchmark; run explicitly with --ignored"]
    fn bench_texture_unit_shadow_vs_hash_map() {
        // Before: `HashMap<u32, Option<u32>>` keyed by the `GL_TEXTURE0 + i`
        // enum — a dense range addressed through a hash.
        let mut map: HashMap<u32, Option<u32>> = HashMap::new();
        let before = time(|| {
            let mut n = 0;
            for i in 0..BATCHES {
                let unit = glow::TEXTURE0 + (i % 4) as u32;
                let tex = Some(42 + (i % 8) as u32);
                let entry = map.entry(unit).or_insert(None);
                if *entry != tex {
                    *entry = tex;
                    n += 1;
                }
            }
            n
        });

        let mut shadow = TextureUnitShadow::default();
        let after = time(|| {
            let mut n = 0;
            for i in 0..BATCHES {
                if shadow.bind(glow::TEXTURE0 + (i % 4) as u32, Some(42 + (i % 8) as u32)) {
                    n += 1;
                }
            }
            n
        });

        report("glBindTexture(TEXTURE_2D, …)", BATCHES, before, after);
        assert!(
            after < before,
            "the indexed array came out slower than the hash map ({after:.0} vs \
             {before:.0} ns)"
        );
    }

    #[test]
    #[ignore = "timing benchmark; run explicitly with --ignored"]
    fn bench_per_face_shadow_vs_hash_map() {
        // Before: `HashMap<u32, (u32, i32, u32)>` keyed FRONT / BACK, so a
        // `FRONT_AND_BACK` call cost four probes — two to compare, two to
        // store. A stencil-masked UI runs this per mask.
        const CALLS: usize = 60;
        let mut map: HashMap<u32, (u32, i32, u32)> = HashMap::new();
        let before = time(|| {
            let mut n = 0;
            for i in 0..CALLS {
                let fp = (glow::EQUAL, i as i32 & 3, 0xFF);
                let same = |m: &HashMap<u32, (u32, i32, u32)>, k: u32| m.get(&k) == Some(&fp);
                if !(same(&map, glow::FRONT) && same(&map, glow::BACK)) {
                    map.insert(glow::FRONT, fp);
                    map.insert(glow::BACK, fp);
                    n += 1;
                }
            }
            n
        });

        let mut shadow: PerFace<(u32, i32, u32)> = PerFace::default();
        let after = time(|| {
            let mut n = 0;
            for i in 0..CALLS {
                if shadow.update(glow::FRONT_AND_BACK, (glow::EQUAL, i as i32 & 3, 0xFF)) {
                    n += 1;
                }
            }
            n
        });

        report("glStencilFunc(FRONT_AND_BACK, …)", CALLS, before, after);
        assert!(
            after < before,
            "two Option fields came out slower than a two-entry hash map \
             ({after:.0} vs {before:.0} ns)"
        );
    }

    /// The vertex-attribute shadow: by this module's own account the
    /// most-called non-draw path in the Cocos Creator 2.x no-VAO code path.
    ///
    /// Before: three containers keyed `(vao, index)` — a `HashMap` of
    /// fingerprints, a `HashSet` of enabled indices and a `HashMap` of divisors
    /// — so one attribute cost three hashes of the same pair into three tables.
    ///
    /// Run at four attribute counts and two VAO counts, because the shape of
    /// the answer is what decided the layout: a fixed-size attribute array is
    /// marginally faster than growing on demand but costs +171% shadow bytes at
    /// three attributes in use, and picking its inline capacity would mean
    /// guessing the distribution of attributes-per-VAO in real content. Grown to
    /// what the content touches, the new layout is faster *and* smaller at every
    /// point here.
    #[test]
    #[ignore = "timing benchmark; run explicitly with --ignored"]
    fn bench_vertex_attrib_shadow_vs_three_hash_containers() {
        for (vaos, attribs) in [(1u32, 3u32), (200, 3), (200, 5), (200, 8)] {
            let calls = (vaos * attribs * 3) as usize;

            // Before: the three containers, keyed exactly as they were.
            #[derive(Clone, Copy, PartialEq, Eq)]
            struct KeyedFp {
                size: i32,
                type_: u32,
                normalized: bool,
                stride: i32,
                offset: i32,
                vao: u32,
                array_buffer: Option<u32>,
            }
            let mut pointers: HashMap<(u32, u32), KeyedFp> = HashMap::new();
            let mut enabled: HashSet<(u32, u32)> = HashSet::new();
            let mut divisors: HashMap<(u32, u32), u32> = HashMap::new();
            let before = time(|| {
                let mut n = 0;
                for vao in 0..vaos {
                    for index in 0..attribs {
                        let key = (vao, index);
                        if !enabled.contains(&key) {
                            enabled.insert(key);
                            n += 1;
                        }
                        let fp = KeyedFp {
                            size: 3,
                            type_: glow::FLOAT,
                            normalized: false,
                            stride: 32,
                            offset: (index * 12) as i32,
                            vao,
                            array_buffer: Some(10 + vao),
                        };
                        if pointers.get(&key) != Some(&fp) {
                            pointers.insert(key, fp);
                            n += 1;
                        }
                        if divisors.get(&key).copied() != Some(0) {
                            divisors.insert(key, 0);
                            n += 1;
                        }
                    }
                }
                n
            });

            let mut shadow = VertexArrayShadow::default();
            let after = time(|| {
                let mut n = 0;
                for vao in 0..vaos {
                    for index in 0..attribs {
                        if shadow.enable(vao, index) {
                            n += 1;
                        }
                        let fp = VertexAttribPointerFp {
                            size: 3,
                            type_: glow::FLOAT,
                            normalized: false,
                            stride: 32,
                            offset: (index * 12) as i32,
                            array_buffer: Some(10 + vao),
                        };
                        if shadow.update_pointer(vao, index, fp) {
                            n += 1;
                        }
                        if shadow.update_divisor(vao, index, 0) {
                            n += 1;
                        }
                    }
                }
                n
            });

            report(
                &format!("vertex attributes, {vaos} VAO(s) x {attribs} attributes"),
                calls,
                before,
                after,
            );
            assert!(
                after < before,
                "{vaos} VAOs x {attribs} attributes: the per-VAO shadow came out \
                 slower than the three hash containers ({after:.0} vs {before:.0} ns)"
            );
        }
    }

    /// The GPU budget's binding ledger, which is reached on the same commands
    /// as the dedup shadow but is a *ledger*: `bind_texture` writes on every
    /// call and cannot be skipped, so the container cost is paid in full.
    ///
    /// Before: `HashMap<CanvasId, _>` to find the context, then
    /// `HashMap<(unit, target), Option<TextureId>>` to reach one of 128 slots
    /// whose key space validation had already bounded.
    #[test]
    #[ignore = "timing benchmark; run explicitly with --ignored"]
    fn bench_gpu_budget_binding_ledger_vs_hash_maps() {
        const TARGETS: [u32; 4] = [0x0DE1, 0x8513, 0x806F, 0x8C1A];
        // One activeTexture + one bindTexture per batch, as a material change
        // does.
        const CALLS: usize = BATCHES * 2;

        #[derive(Default)]
        struct HashShape {
            active_texture: u32,
            textures: HashMap<(u32, u32), Option<u32>>,
        }
        let mut before_map: HashMap<CanvasId, HashShape> = HashMap::new();
        let before = time(|| {
            let mut n = 0;
            for i in 0..BATCHES {
                let state = before_map.entry(1).or_default();
                state.active_texture = glow::TEXTURE0 + (i % 4) as u32;
                n += 1;
                let state = before_map.entry(1).or_default();
                let unit = state.active_texture;
                state.textures.insert(
                    (unit, TARGETS[i % TARGETS.len()]),
                    Some(40 + (i % 8) as u32),
                );
                n += 1;
            }
            n
        });

        // After: the shared keyed table plus a flat 128-slot array. Modelled
        // here rather than driven through `WebGlGpuBudget` so the measurement
        // is of the containers and not of the budget arithmetic around them.
        struct FlatShape {
            active_texture: u32,
            slots: [Option<u32>; 128],
        }
        impl Default for FlatShape {
            fn default() -> Self {
                Self {
                    active_texture: glow::TEXTURE0,
                    slots: [None; 128],
                }
            }
        }
        let mut after_table: crate::canvas_keyed::CanvasKeyed<FlatShape> =
            crate::canvas_keyed::CanvasKeyed::default();
        let after = time(|| {
            let mut n = 0;
            for i in 0..BATCHES {
                let state = after_table.entry(1).or_default();
                state.active_texture = glow::TEXTURE0 + (i % 4) as u32;
                n += 1;
                let state = after_table.entry(1).or_default();
                let unit_index = state.active_texture.wrapping_sub(glow::TEXTURE0) as usize;
                let target_index = i % TARGETS.len();
                state.slots[unit_index * TARGETS.len() + target_index] = Some(40 + (i % 8) as u32);
                n += 1;
            }
            n
        });

        report("GPU budget binding ledger", CALLS, before, after);
        assert!(
            after < before,
            "the flat ledger came out slower than the two hash maps ({after:.0} \
             vs {before:.0} ns)"
        );
    }

    #[test]
    #[ignore = "timing benchmark; run explicitly with --ignored"]
    fn bench_canvas_state_table_vs_hash_map() {
        // Before: `HashMap<CanvasId, CanvasGLState>` built `with_capacity(4)`,
        // hashed once per GL state command to find the one canvas a
        // single-canvas game has. 14 state calls per sprite batch.
        const CALLS: usize = BATCHES * 14;

        let mut map: HashMap<CanvasId, CanvasGLState> = HashMap::with_capacity(4);
        let before = time(|| {
            let mut n = 0;
            for _ in 0..CALLS {
                let state = map.entry(1).or_default();
                state.depth_func = Some(glow::LESS);
                n += 1;
            }
            n
        });

        let mut table = CanvasStateTable::default();
        let after = time(|| {
            let mut n = 0;
            for _ in 0..CALLS {
                let state = table.entry(1).or_default();
                state.depth_func = Some(glow::LESS);
                n += 1;
            }
            n
        });

        report("per-command canvas shadow lookup", CALLS, before, after);
        assert!(
            after < before,
            "the keyed scan came out slower than the hash map on a single \
             canvas ({after:.0} vs {before:.0} ns)"
        );
    }

    /// The case a keyed scan could lose: several canvases, switching on every
    /// command, so the memo never hits. It has to stay at least competitive,
    /// or the change trades a common win for an uncommon regression.
    #[test]
    #[ignore = "timing benchmark; run explicitly with --ignored"]
    fn bench_canvas_state_table_when_every_command_switches_canvas() {
        const CALLS: usize = BATCHES * 14;
        for canvases in [2u32, 4] {
            let mut map: HashMap<CanvasId, CanvasGLState> = HashMap::with_capacity(4);
            let before = time(|| {
                let mut n = 0;
                for i in 0..CALLS {
                    map.entry(1 + (i as u32 % canvases)).or_default().depth_func = Some(glow::LESS);
                    n += 1;
                }
                n
            });

            let mut table = CanvasStateTable::default();
            let after = time(|| {
                let mut n = 0;
                for i in 0..CALLS {
                    table
                        .entry(1 + (i as u32 % canvases))
                        .or_default()
                        .depth_func = Some(glow::LESS);
                    n += 1;
                }
                n
            });

            report(
                &format!("canvas shadow lookup, {canvases} canvases alternating"),
                CALLS,
                before,
                after,
            );
            assert!(
                after < before * 1.25,
                "{canvases} alternating canvases: the keyed scan is more than \
                 25% slower than the hash map ({after:.0} vs {before:.0} ns), \
                 so the adversarial case now costs more than the common case \
                 gains"
            );
        }
    }
}

/// The EGL context, Skia context and identity tag shared by every offscreen
/// Canvas2D surface. One of these exists per `CanvasManager`, created lazily.
pub(crate) struct Shared2DContext {
    pub(super) egl: EglContextHandle,
    pub gr_ctx: skia_safe::gpu::DirectContext,
    pub interface: skia_safe::gpu::gl::Interface,
    /// Identity used by `ImageStore` to key its per-context SkImage wrappers.
    /// Shared, like the context it names: canvases on this context can and
    /// should reuse each other's wrappers.
    pub ctx_tag: u32,
}

#[derive(Debug)]
pub(crate) struct ProgramMeta {
    pub gl_handle: Option<NativeProgram>,
    pub owner_canvas: Option<CanvasId>, // None => resource context created
    pub deleted: bool,
    /// Shader IDs attached via `glAttachShader`.  Used by shader cache to
    /// reconstruct the cache key at link time.
    pub attached_shaders: Vec<ShaderId>,
    /// `glBindAttribLocation(program, index, name)` bindings recorded since
    /// the program was created.  These change the linked program's attribute
    /// locations, so they MUST be part of the shader-binary cache key —
    /// otherwise a re-link with new bindings (Pixi v8 sorts attributes and
    /// re-links) loads a stale cached binary with the wrong locations, and
    /// every vertex attribute reads the wrong stream.
    pub attrib_bindings: Vec<(u32, String)>,
    /// `glLinkProgram` has been issued and nobody has read `LINK_STATUS` yet.
    ///
    /// The read is what blocks, so it is deferred until either the content asks
    /// or the batch ends -- see `renderergl::link_queue`. While this is set the
    /// program's binary has not been offered to the shader cache.
    pub link_pending: bool,
}

#[derive(Debug)]
pub(crate) struct ShaderMeta {
    pub gl_handle: Option<NativeShader>,
    pub owner_canvas: Option<CanvasId>,
    #[allow(dead_code)]
    pub shader_type: ShaderType, // Vertex / Fragment (protocol enum)
    pub gl_shader_type: u32, // glow::VERTEX_SHADER / glow::FRAGMENT_SHADER
    pub deleted: bool,
    pub source_len: usize, // cached for SHADER_SOURCE_LENGTH
    /// Shader source cached for shader binary cache key.
    pub source: Option<String>,
}

#[derive(Debug)]
pub(crate) struct BufferMeta {
    pub gl_handle: Option<NativeBuffer>,
    pub owner_canvas: Option<CanvasId>,
    pub deleted: bool,
}

#[derive(Debug)]
pub(crate) struct TextureMeta {
    pub gl_handle: Option<NativeTexture>,
    pub owner_canvas: Option<CanvasId>,
    pub deleted: bool,
}

/// WebGL framebuffer object. A container object, so its name is local to the
/// context that created it and `owner` is what makes deletion correct — see
/// [`super::gl_object`].
#[derive(Debug)]
pub(crate) struct FramebufferMeta {
    pub gl_handle: Option<NativeFramebuffer>,
    pub owner: CanvasId,
    pub deleted: bool,
}

impl FramebufferMeta {
    /// Take this object for deletion, once.
    ///
    /// The handle leaves as a [`GlObject`], which carries the owning canvas with
    /// it, so a caller cannot obtain a bare name to delete from whatever context
    /// happens to be current. That is the whole point: a framebuffer name means a
    /// different object in every context of the share group.
    pub(crate) fn take_for_delete(&mut self) -> Option<GlObject> {
        let handle = self.gl_handle.take()?;
        self.deleted = true;
        Some(GlObject::Framebuffer {
            handle,
            owner: self.owner,
        })
    }
}

#[derive(Debug)]
pub(crate) struct RenderbufferMeta {
    pub gl_handle: Option<NativeRenderbuffer>,
    pub owner_canvas: Option<CanvasId>,
    pub deleted: bool,
}

/// WebGL 2 Vertex Array Object.  The owner canvas is tracked so that
/// destroying a canvas also sweeps its VAOs — VAOs are not shared in
/// the EGL share-group model WebGL uses, which is also why deleting one
/// requires its own context ([`super::gl_object`]).
#[derive(Clone, Debug)]
pub(crate) struct VaoMeta {
    pub gl_handle: Option<NativeVertexArray>,
    pub owner: CanvasId,
    pub deleted: bool,
}

impl VaoMeta {
    /// Take this object for deletion, once. See [`FramebufferMeta::take_for_delete`].
    pub(crate) fn take_for_delete(&mut self) -> Option<GlObject> {
        let handle = self.gl_handle.take()?;
        self.deleted = true;
        Some(GlObject::VertexArray {
            handle,
            owner: self.owner,
        })
    }
}

#[derive(Clone, Debug)]
pub(crate) struct SamplerMeta {
    pub gl_handle: Option<NativeSampler>,
    #[allow(dead_code)]
    pub owner_canvas: Option<CanvasId>,
    pub deleted: bool,
}

/// Fence sync.  Shared across the EGL share group like buffers and textures
/// (ES 3.0 Appendix C.1), so any context of the group may wait on one or delete
/// it; the owner canvas is noted only so `ClientWaitSync` polls on a context that
/// is guaranteed to exist.
#[derive(Clone, Debug)]
pub(crate) struct SyncMeta {
    pub gl_handle: Option<NativeFence>,
    pub owner_canvas: Option<CanvasId>,
    pub deleted: bool,
}

/// WebGL 2 query object.  A container object: owned by one canvas context
/// (queries are not shareable across GL contexts per the spec), so deletion needs
/// that context — see [`super::gl_object`].
#[derive(Clone, Debug)]
pub(crate) struct QueryMeta {
    pub gl_handle: Option<glow::NativeQuery>,
    pub owner: CanvasId,
    pub deleted: bool,
}

impl QueryMeta {
    /// Take this object for deletion, once. See [`FramebufferMeta::take_for_delete`].
    pub(crate) fn take_for_delete(&mut self) -> Option<GlObject> {
        let handle = self.gl_handle.take()?;
        self.deleted = true;
        Some(GlObject::Query {
            handle,
            owner: self.owner,
        })
    }
}

/// WebGL 2 transform feedback object.  Same context-locality as
/// queries; we stash the owner canvas so deletion binds the right
/// context first.
#[derive(Clone, Debug)]
pub(crate) struct TransformFeedbackMeta {
    pub gl_handle: Option<glow::NativeTransformFeedback>,
    pub owner: CanvasId,
    pub deleted: bool,
}

impl TransformFeedbackMeta {
    /// Take this object for deletion, once. See [`FramebufferMeta::take_for_delete`].
    pub(crate) fn take_for_delete(&mut self) -> Option<GlObject> {
        let handle = self.gl_handle.take()?;
        self.deleted = true;
        Some(GlObject::TransformFeedback {
            handle,
            owner: self.owner,
        })
    }
}

#[derive(Clone)]
pub(super) struct EglContextHandle {
    pub ctx: egl::Context,
    /// `None` when the share group is surfaceless; see
    /// `EglInitResult::surfaceless`.
    pub surf: Option<egl::Surface>,
}

#[derive(Clone, Copy)]
pub(super) enum SurfaceKind {
    Window,
    Pbuffer,
}

pub(super) struct CanvasEntry {
    pub info: CanvasInfo,
    /// Actual EGL surface dimensions (physical pixels).
    pub physical_width: u32,
    pub physical_height: u32,
    pub kind: SurfaceKind,
    pub ctx: EglContextHandle,
    /// DrawingBuffer for the onscreen canvas. None for offscreen pbuffer canvases.
    pub drawing_buffer: Option<super::drawing_buffer::DrawingBuffer>,
    /// When true, WebGL renders directly to FBO 0 (window surface) and the
    /// DrawingBuffer blit is skipped.  Currently enabled when there is
    /// exactly one canvas (the onscreen one) with a DrawingBuffer.
    ///
    /// When bypass is active, the window surface content becomes undefined
    /// after `eglSwapBuffers` (per EGL spec). To handle games that read from
    /// the default framebuffer, `CanvasManager::signal_default_fbo_readback()`
    /// permanently disables bypass when such a readback is detected. This
    /// re-routes rendering through the DrawingBuffer which preserves content.
    ///
    /// Detected by a nonempty `ReadPixels` on the onscreen default READ FBO.
    ///
    /// Set by `CanvasManager::evaluate_bypass()` after canvas lifecycle events.
    pub bypass_drawing_buffer: bool,
    /// Mode whose default-FBO mapping is installed in this EGL context.
    /// May lag the requested mode while another context is current.
    pub applied_bypass_drawing_buffer: bool,
}

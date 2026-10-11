//! The engine's overlay: one canvas presented above the content's frame.
//!
//! The native buttons content creates (`createUserInfoButton` and its kind) are
//! views above the game on the platform whose API they are. Migo draws them
//! itself, into a 2D canvas content has no handle on, and names that canvas
//! with `Canvas2DCmd::SetOverlay`. At present time the canvas is composited over
//! the window -- after the content's pixels reach it and before the swap -- so
//! the content's canvas, and everything that reads it (`getImageData`,
//! `readPixels`, a screenshot of the game), never sees the buttons.
//!
//! The window's GL context is the content's WebGL context too, and a present
//! lands between any two of its batches. So the draw here changes nothing the
//! content can observe: every piece of state it touches is read before and put
//! back after ([`SavedState`]). Skia's own work gets away with invalidating the
//! WebGL dedup shadow because it never runs in this context; this does.

use glow::HasContext;

use shared::protocol::render_cmd::CanvasId;

/// Which canvas is the overlay and where its top-left corner sits, in window
/// pixels with the origin at the top left.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Placement {
    pub(crate) canvas: CanvasId,
    pub(crate) x: i32,
    pub(crate) y: i32,
}

/// What a present composites: a copy of the canvas, taken before the frame's
/// damage was declared so the present itself never switches contexts.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Prepared {
    pub(crate) texture: glow::NativeTexture,
    pub(crate) x: i32,
    pub(crate) y: i32,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

/// What a present showed. When the next one would show anything else, the
/// whole surface is repaired: the window's older buffers still hold the old
/// overlay, and only a full repair takes it out of all of them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Presented {
    pub(crate) placement: Placement,
    pub(crate) generation: u64,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

const VERTEX_SHADER: &str = "#version 300 es
uniform vec4 u_rect;
out vec2 v_uv;
void main() {
    vec2 corner = vec2(float(gl_VertexID & 1), float(gl_VertexID >> 1));
    v_uv = corner;
    gl_Position = vec4(mix(u_rect.xy, u_rect.zw, corner), 0.0, 1.0);
}
";

// The copy is premultiplied, top row first: v = 0 is the top of the canvas.
const FRAGMENT_SHADER: &str = "#version 300 es
precision mediump float;
uniform sampler2D u_overlay;
in vec2 v_uv;
out vec4 o_color;
void main() {
    o_color = texture(u_overlay, v_uv);
}
";

/// The program and the (empty) vertex array that draw the overlay, made in the
/// window context they belong to: a vertex array is that context's, not the
/// share group's, so a new window context makes a new one.
pub(crate) struct Compositor {
    context: *mut std::ffi::c_void,
    program: glow::NativeProgram,
    vertex_array: glow::NativeVertexArray,
    rect: glow::NativeUniformLocation,
}

impl Compositor {
    /// Build it in the current context, `context`. `None` when the driver
    /// refuses the program, which is logged once by the caller.
    pub(crate) fn new(gl: &glow::Context, context: *mut std::ffi::c_void) -> Option<Self> {
        unsafe {
            let program = gl.create_program().ok()?;
            let mut shaders = Vec::with_capacity(2);
            for (kind, source) in [
                (glow::VERTEX_SHADER, VERTEX_SHADER),
                (glow::FRAGMENT_SHADER, FRAGMENT_SHADER),
            ] {
                let Ok(shader) = gl.create_shader(kind) else {
                    gl.delete_program(program);
                    return None;
                };
                gl.shader_source(shader, source);
                gl.compile_shader(shader);
                gl.attach_shader(program, shader);
                shaders.push(shader);
            }
            gl.link_program(program);
            for shader in shaders {
                gl.detach_shader(program, shader);
                gl.delete_shader(shader);
            }
            if !gl.get_program_link_status(program) {
                tracing::warn!(
                    "overlay program did not link: {}",
                    gl.get_program_info_log(program)
                );
                gl.delete_program(program);
                return None;
            }
            let rect = gl.get_uniform_location(program, "u_rect");
            let sampler = gl.get_uniform_location(program, "u_overlay");
            let vertex_array = gl.create_vertex_array().ok();
            let (Some(rect), Some(sampler), Some(vertex_array)) = (rect, sampler, vertex_array)
            else {
                gl.delete_program(program);
                return None;
            };
            // The sampler reads unit 0, set once: a uniform is the program's.
            let previous = gl.get_parameter_i32(glow::CURRENT_PROGRAM);
            gl.use_program(Some(program));
            gl.uniform_1_i32(Some(&sampler), 0);
            gl.use_program(native_program(previous));
            Some(Self {
                context,
                program,
                vertex_array,
                rect,
            })
        }
    }

    /// Whether this was made in `context`.
    pub(crate) fn belongs_to(&self, context: *mut std::ffi::c_void) -> bool {
        self.context == context
    }

    /// Composite `overlay` over the window (`DRAW_FRAMEBUFFER` 0, `surface_w` x
    /// `surface_h`), blending its premultiplied pixels over what is there, and
    /// leave every piece of GL state as it found it.
    pub(crate) fn draw(
        &self,
        gl: &glow::Context,
        overlay: &Prepared,
        surface_w: u32,
        surface_h: u32,
    ) {
        if surface_w == 0 || surface_h == 0 || overlay.width == 0 || overlay.height == 0 {
            return;
        }
        let (w, h) = (surface_w as f32, surface_h as f32);
        let left = 2.0 * overlay.x as f32 / w - 1.0;
        let right = 2.0 * (overlay.x as f32 + overlay.width as f32) / w - 1.0;
        let top = 1.0 - 2.0 * overlay.y as f32 / h;
        let bottom = 1.0 - 2.0 * (overlay.y as f32 + overlay.height as f32) / h;
        unsafe {
            let saved = SavedState::capture(gl);
            gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, None);
            gl.viewport(0, 0, surface_w as i32, surface_h as i32);
            for capability in SavedState::DISABLED {
                gl.disable(capability);
            }
            gl.color_mask(true, true, true, true);
            gl.enable(glow::BLEND);
            gl.blend_equation(glow::FUNC_ADD);
            gl.blend_func(glow::ONE, glow::ONE_MINUS_SRC_ALPHA);
            gl.active_texture(glow::TEXTURE0);
            gl.bind_sampler(0, None);
            gl.bind_texture(glow::TEXTURE_2D, Some(overlay.texture));
            // The copy is drawn pixel for pixel; nearest keeps it exact.
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MIN_FILTER,
                glow::NEAREST as i32,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MAG_FILTER,
                glow::NEAREST as i32,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_WRAP_S,
                glow::CLAMP_TO_EDGE as i32,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_WRAP_T,
                glow::CLAMP_TO_EDGE as i32,
            );
            gl.use_program(Some(self.program));
            gl.uniform_4_f32(Some(&self.rect), left, top, right, bottom);
            gl.bind_vertex_array(Some(self.vertex_array));
            // A draw while content's transform feedback runs would be captured
            // by it -- or refused, its varyings not being ours.
            let feedback = saved.transform_feedback_active && !saved.transform_feedback_paused;
            if feedback {
                gl.pause_transform_feedback();
            }
            gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
            if feedback {
                gl.resume_transform_feedback();
            }
            saved.restore(gl);
        }
    }
}

fn native_program(raw: i32) -> Option<glow::NativeProgram> {
    std::num::NonZeroU32::new(raw as u32).map(glow::NativeProgram)
}

/// The GL state the overlay draw touches, as the content left it.
struct SavedState {
    draw_framebuffer: i32,
    viewport: [i32; 4],
    enabled: [bool; 7],
    color_mask: [bool; 4],
    blend: bool,
    blend_func: [i32; 4],
    blend_equation: [i32; 2],
    active_texture: i32,
    texture: i32,
    sampler: i32,
    program: i32,
    vertex_array: i32,
    transform_feedback_active: bool,
    transform_feedback_paused: bool,
}

impl SavedState {
    /// Capabilities the overlay draw turns off, any of which would clip,
    /// discard or test away its fragments.
    const DISABLED: [u32; 7] = [
        glow::SCISSOR_TEST,
        glow::DEPTH_TEST,
        glow::STENCIL_TEST,
        glow::CULL_FACE,
        glow::RASTERIZER_DISCARD,
        glow::SAMPLE_ALPHA_TO_COVERAGE,
        glow::SAMPLE_COVERAGE,
    ];

    unsafe fn capture(gl: &glow::Context) -> Self {
        unsafe {
            let mut viewport = [0; 4];
            gl.get_parameter_i32_slice(glow::VIEWPORT, &mut viewport);
            let color_mask = gl.get_parameter_bool_array::<4>(glow::COLOR_WRITEMASK);
            let active_texture = gl.get_parameter_i32(glow::ACTIVE_TEXTURE);
            // The texture and sampler of unit 0, which the draw binds.
            gl.active_texture(glow::TEXTURE0);
            let texture = gl.get_parameter_i32(glow::TEXTURE_BINDING_2D);
            let sampler = gl.get_parameter_i32(glow::SAMPLER_BINDING);
            Self {
                draw_framebuffer: gl.get_parameter_i32(glow::DRAW_FRAMEBUFFER_BINDING),
                viewport,
                enabled: Self::DISABLED.map(|capability| gl.is_enabled(capability)),
                color_mask,
                blend: gl.is_enabled(glow::BLEND),
                blend_func: [
                    gl.get_parameter_i32(glow::BLEND_SRC_RGB),
                    gl.get_parameter_i32(glow::BLEND_DST_RGB),
                    gl.get_parameter_i32(glow::BLEND_SRC_ALPHA),
                    gl.get_parameter_i32(glow::BLEND_DST_ALPHA),
                ],
                blend_equation: [
                    gl.get_parameter_i32(glow::BLEND_EQUATION_RGB),
                    gl.get_parameter_i32(glow::BLEND_EQUATION_ALPHA),
                ],
                active_texture,
                texture,
                sampler,
                program: gl.get_parameter_i32(glow::CURRENT_PROGRAM),
                vertex_array: gl.get_parameter_i32(glow::VERTEX_ARRAY_BINDING),
                transform_feedback_active: gl.get_parameter_i32(glow::TRANSFORM_FEEDBACK_ACTIVE)
                    != 0,
                transform_feedback_paused: gl.get_parameter_i32(glow::TRANSFORM_FEEDBACK_PAUSED)
                    != 0,
            }
        }
    }

    unsafe fn restore(&self, gl: &glow::Context) {
        unsafe {
            gl.bind_vertex_array(
                std::num::NonZeroU32::new(self.vertex_array as u32).map(glow::NativeVertexArray),
            );
            gl.use_program(native_program(self.program));
            // Unit 0 first, then back to the unit the content had active.
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(
                glow::TEXTURE_2D,
                std::num::NonZeroU32::new(self.texture as u32).map(glow::NativeTexture),
            );
            gl.bind_sampler(
                0,
                std::num::NonZeroU32::new(self.sampler as u32).map(glow::NativeSampler),
            );
            gl.active_texture(self.active_texture as u32);
            gl.blend_equation_separate(
                self.blend_equation[0] as u32,
                self.blend_equation[1] as u32,
            );
            gl.blend_func_separate(
                self.blend_func[0] as u32,
                self.blend_func[1] as u32,
                self.blend_func[2] as u32,
                self.blend_func[3] as u32,
            );
            set_enabled(gl, glow::BLEND, self.blend);
            for (capability, enabled) in Self::DISABLED.iter().zip(self.enabled) {
                set_enabled(gl, *capability, enabled);
            }
            gl.color_mask(
                self.color_mask[0],
                self.color_mask[1],
                self.color_mask[2],
                self.color_mask[3],
            );
            gl.viewport(
                self.viewport[0],
                self.viewport[1],
                self.viewport[2],
                self.viewport[3],
            );
            gl.bind_framebuffer(
                glow::DRAW_FRAMEBUFFER,
                std::num::NonZeroU32::new(self.draw_framebuffer as u32)
                    .map(glow::NativeFramebuffer),
            );
        }
    }
}

unsafe fn set_enabled(gl: &glow::Context, capability: u32, enabled: bool) {
    unsafe {
        if enabled {
            gl.enable(capability);
        } else {
            gl.disable(capability);
        }
    }
}

use deno_core::extension;

mod context2d;
pub(crate) mod decode;
mod font;
pub(crate) mod frame_collector;
/// Re-exported, not defined here any more.
///
/// The structural validator and the WebGL opcode table moved to
/// `migo-frame-wire` so the cross-process frame consumer can validate the same
/// words without linking a JavaScript engine. The `stream` path stays valid for
/// every call site in this crate; `gl` is not re-exported because after the
/// move its only user is a test, and a re-export nothing outside tests reads is
/// an unused import in every shipped build.
pub(crate) use frame_wire::stream;

mod raf;
/// Test-only: the cases asserting this crate's JavaScript encoder agrees with
/// the shared wire-format table.
#[cfg(test)]
mod render_stream_js_agreement;
mod webgl;

use context2d::*;
pub mod error_state;
use error_state::{
    op_webgl_get_context_attributes, op_webgl_get_error, op_webgl_query_compressed_caps,
    op_webgl_record_attributes, op_webgl_record_error, op_webgl_record_out_of_memory,
};
use font::*;
use raf::*;
use webgl::*;

extension!(host_v8_webgl,
    deps = [host_v8_console, host_v8_base],
    ops = [
        op_viewport,
        op_clear,
        op_clear_color,

        op_await_next_frame,
        op_set_preferred_fps,

        op_alloc_gl_resource_id,
        op_webgl_get_error,
        op_webgl_record_error,
        op_webgl_record_out_of_memory,
        op_webgl_get_context_attributes,
        op_webgl_record_attributes,
        op_webgl_query_compressed_caps,
        op_create_program,
        op_use_program,
        op_link_program,
        op_get_program_parameter,
        op_get_program_info_log,
        op_delete_program,

        op_create_shader,
        op_shader_source,
        op_compile_shader,
        op_attach_shader,
        op_delete_shader,
        op_get_shader_parameter,
        op_get_shader_info_log,

        op_draw_arrays,
        op_draw_elements,

        op_get_attrib_location,
        op_bind_attrib_location,
        op_get_active_attrib,
        op_get_active_uniform,
        op_enable_vertex_attrib_array,
        op_vertex_attrib_pointer,

        op_create_buffer,
        op_bind_buffer,
        op_buffer_data,

        op_get_uniform_location,
        op_uniform3f,
        op_uniform_matrix_3fv,
        op_gl_flush,
        op_gl_is_context_lost,
        op_gl_lose_context,

        // GL State
        op_enable,
        op_disable,
        op_get_parameter,

        // Textures
        op_create_texture,
        op_delete_texture,
        op_bind_texture,
        op_active_texture,
        op_tex_image_2d,
        op_tex_image_2d_from_image,
        op_tex_image_2d_from_snapshot,
        op_tex_image_2d_from_canvas2d,
        op_tex_sub_image_2d,
        op_tex_sub_image_2d_from_snapshot,
        op_tex_sub_image_2d_from_canvas2d,
        op_tex_sub_image_2d_from_image,
        op_tex_parameteri,
        op_tex_parameterf,
        op_generate_mipmap,
        op_pixel_storei,
        op_compressed_tex_image_2d,
        op_compressed_tex_sub_image_2d,

        // Buffer & Vertex extensions
        op_buffer_sub_data,
        op_disable_vertex_attrib_array,
        op_clear_depth,
        op_clear_stencil,

        // Blend/Depth/Stencil/Cull State
        op_blend_func,
        op_blend_func_separate,
        op_blend_equation,
        op_blend_equation_separate,
        op_blend_color,
        op_depth_func,
        op_depth_mask,
        op_depth_range,
        op_stencil_func,
        op_stencil_func_separate,
        op_stencil_op,
        op_stencil_op_separate,
        op_stencil_mask,
        op_stencil_mask_separate,
        op_cull_face,
        op_front_face,
        op_color_mask,
        op_scissor,
        op_line_width,
        op_polygon_offset,

        // Uniform variants
        op_uniform1i,
        op_uniform1f,
        op_uniform2f,
        op_uniform4f,
        op_uniform1iv,
        op_uniform1fv,
        op_uniform2iv,
        op_uniform2fv,
        op_uniform3iv,
        op_uniform3fv,
        op_uniform4iv,
        op_uniform4fv,
        op_uniform_matrix_2fv,
        op_uniform_matrix_4fv,

        // Framebuffer/Renderbuffer
        op_create_framebuffer,
        op_delete_framebuffer,
        op_bind_framebuffer,
        op_framebuffer_texture_2d,
        op_framebuffer_renderbuffer,
        op_check_framebuffer_status,
        op_create_renderbuffer,
        op_delete_renderbuffer,
        op_delete_buffer,
        op_bind_renderbuffer,
        op_renderbuffer_storage,

        // Misc
        op_read_pixels,
        op_read_pixels_to_buffer,
        op_hint,

        // 2D Context (sync ops)
        op_create_context_2d,
        op_measure_text,
        op_measure_text_flat,
        op_get_image_data,
        op_capture_canvas2d_snapshot,
        op_capture_canvas2d_snapshot_for_cache,
        op_force_readback_snapshot,

        // Text texture cache
        op_text_cache_peek_pin,
        op_text_cache_unpin,
        op_tex_image_2d_from_text_cache,

        // Frame lifecycle
        op_frame_end_unified,
        op_invalidate,

        // Path methods
        op_begin_path,
        op_close_path,
        op_move_to,
        op_line_to,
        op_quadratic_curve_to,
        op_bezier_curve_to,
        op_arc,
        op_arc_to,
        op_rect,
        op_ellipse,

        // Drawing methods
        op_fill,
        op_stroke,
        op_clip,

        // Rectangle methods
        op_fill_rect,
        op_stroke_rect,
        op_clear_rect,

        // Text methods
        op_fill_text,
        op_stroke_text,

        // Style setters
        op_set_fill_style,
        op_set_stroke_style,
        op_set_line_width,
        op_set_line_cap,
        op_set_line_join,
        op_set_miter_limit,
        op_set_global_alpha,
        op_set_composite_operation,
        op_set_line_dash,
        op_set_line_dash_offset,
        op_set_shadow_blur,
        op_set_shadow_color,
        op_set_shadow_offset_x,
        op_set_shadow_offset_y,
        op_set_fill_style_gradient,
        op_set_stroke_style_gradient,
        op_set_fill_style_pattern,
        op_set_stroke_style_pattern,
        op_set_font,
        op_set_text_align,
        op_set_text_baseline,
        op_set_text_direction,

        // State methods
        op_save,
        op_restore,

        // Transform methods
        op_translate,
        op_rotate,
        op_scale,
        op_set_transform,
        op_reset_transform,

        // Image methods
        op_draw_image,
        op_draw_image_batch,

        // Font methods
        op_load_font,
        op_get_text_line_height,

        // ---- WebGL 2.0 / GLES 3.0 ----
        op_create_vertex_array,
        op_delete_vertex_array,
        op_bind_vertex_array,
        op_vertex_attrib_divisor,
        op_draw_arrays_instanced,
        op_draw_elements_instanced,
        op_get_uniform_block_index,
        op_uniform_block_binding,
        op_bind_buffer_base,
        op_bind_buffer_range,
        op_tex_storage_2d,
        op_blit_framebuffer,
        op_invalidate_framebuffer,
        op_renderbuffer_storage_multisample,
        op_create_sampler,
        op_delete_sampler,
        op_bind_sampler,
        op_sampler_parameteri,
        op_sampler_parameterf,
        op_fence_sync,
        op_delete_sync,
        op_client_wait_sync,
        op_draw_buffers,
        op_read_buffer,
        op_create_query,
        op_delete_query,
        op_begin_query,
        op_end_query,
        op_get_query_parameter,
        op_create_transform_feedback,
        op_delete_transform_feedback,
        op_bind_transform_feedback,
        op_begin_transform_feedback,
        op_end_transform_feedback,
        op_pause_transform_feedback,
        op_resume_transform_feedback,
        op_get_transform_feedback_varying,
        op_transform_feedback_varyings,
        op_tex_image_3d,
        op_tex_sub_image_3d,
        op_tex_storage_3d,
        op_submit_render_stream,
    ],
    esm = [
        dir "src/rendering/webgl",
        "00_render_command_stream.js",
        "01_constants.js",
        "02_2d_context.js",
        "02_webgl_context.js",
        "03_raf.js",
        "04_font.js",
    ],
    state = |state| {
        let host_id = state.borrow::<shared::op_state::HostOpState>().id;
        state.put(GlResourceIdAllocator::new());
        state.put(frame_collector::UnifiedFrameCollector::with_host_id(host_id));
        state.put(error_state::WebGLErrorState::default());
    }
);

pub(super) fn webgl_extensions() -> Vec<deno_core::Extension> {
    vec![host_v8_webgl::init()]
}

pub(super) fn webgl_lazy_extensions() -> Vec<deno_core::Extension> {
    vec![host_v8_webgl::lazy_init()]
}

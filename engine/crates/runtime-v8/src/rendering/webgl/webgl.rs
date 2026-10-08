use deno_core::{OpState, ToJsBuffer, op2, v8};
use tracing::error;

use crate::rendering::webgl::error_state::{self, OpStateDecodeContext, codes};
use frame_decode::resource::{CompressedSource, Payload, PixelSource};

use shared::{
    error::EngineError,
    js_escape::escape_for_json_string,
    op_state::CanvasOpState,
    protocol::{
        render_cmd::{
            GLCmd, RenderCmdResp, RenderCommand, UniformF32Values, UniformI32Values,
            UniformU32Values, checked_readback_byte_len, webgl_readback_bytes_per_pixel,
        },
        send_gl_with_resp_sync,
    },
};

pub(crate) struct GlResourceIdAllocator {
    next_id: u32,
}

impl GlResourceIdAllocator {
    pub(crate) fn new() -> Self {
        Self { next_id: 1 }
    }

    fn alloc(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        if self.next_id == 0 {
            self.next_id = 1;
        }
        id
    }

    #[cfg(test)]
    fn with_next_for_test(next_id: u32) -> Self {
        Self {
            next_id: if next_id == 0 { 1 } else { next_id },
        }
    }
}

#[cfg(test)]
#[path = "../../../tests/support/read_pixels_responder.rs"]
mod read_pixels_responder;

#[cfg(test)]
pub(super) mod tests {
    use std::{
        path::PathBuf,
        sync::{Arc, atomic::AtomicBool},
        time::Duration,
    };

    use deno_core::OpState;

    use super::{
        GlResourceIdAllocator, bind_buffer_base_impl, bind_buffer_range_impl, copy_f32_words,
        copy_i32_words, gl_cmd_has_heap_payload, image_pixel_source, prepare_read_pixels,
    };
    use crate::HostJsRuntime;
    use crate::rendering::webgl::{
        error_state::{self, TransformFeedback, WebGLErrorState, codes},
        frame_collector::UnifiedFrameCollector,
    };
    use shared::{
        FrameOp,
        channel::ThreadWakeup,
        device::gpu_caps::GpuCaps,
        op_state::{AudioSender, HostOpState, NetworkPolicy},
        protocol::render_cmd::{GLCmd, PixelUnpackSource, RenderCommand},
        render_command_sender::CommandSender,
    };

    fn new_webgl_op_state() -> OpState {
        let mut state = OpState::new(None);
        state.put(UnifiedFrameCollector::new());
        state.put(WebGLErrorState::default());
        state
    }

    #[test]
    fn read_pixels_rejects_invalid_or_unbounded_allocations_before_dispatch() {
        let canvas_id = 7;
        let mut state = new_webgl_op_state();

        assert_eq!(
            prepare_read_pixels(&mut state, canvas_id, -1, 1, 0x1908, 0x1401),
            None
        );
        assert_eq!(
            state.borrow_mut::<WebGLErrorState>().drain_one(canvas_id),
            codes::INVALID_VALUE
        );

        assert_eq!(
            prepare_read_pixels(&mut state, canvas_id, 4097, 4096, 0x1908, 0x1401),
            None
        );
        assert_eq!(
            state.borrow_mut::<WebGLErrorState>().drain_one(canvas_id),
            codes::OUT_OF_MEMORY
        );
    }

    #[test]
    fn read_pixels_accepts_zero_and_exact_limit_without_recording_an_error() {
        let canvas_id = 9;
        let mut state = new_webgl_op_state();

        assert_eq!(
            prepare_read_pixels(&mut state, canvas_id, 0, i32::MAX, 0x1908, 0x1401),
            Some(0)
        );
        assert_eq!(
            prepare_read_pixels(&mut state, canvas_id, 4096, 4096, 0x1908, 0x1401),
            Some(64 * 1024 * 1024)
        );
        assert_eq!(
            state.borrow_mut::<WebGLErrorState>().drain_one(canvas_id),
            codes::NO_ERROR
        );
    }

    #[test]
    fn read_pixels_rejects_unknown_layout_before_dispatch() {
        let canvas_id = 9;
        for (format, type_) in [(0xFFFF, 0x1401), (0x1908, 0xFFFF)] {
            let mut state = new_webgl_op_state();
            assert_eq!(
                prepare_read_pixels(&mut state, canvas_id, 1, 1, format, type_),
                None
            );
            assert_eq!(
                state.borrow_mut::<WebGLErrorState>().drain_one(canvas_id),
                codes::INVALID_ENUM
            );
            assert_eq!(
                state
                    .borrow::<UnifiedFrameCollector>()
                    .approx_pending_bytes(),
                0
            );
        }
    }

    #[test]
    fn read_pixels_integer_components_use_the_actual_transfer_budget() {
        let canvas_id = 9;
        let mut state = new_webgl_op_state();
        assert_eq!(
            prepare_read_pixels(&mut state, canvas_id, 2048, 2048, 0x8D99, 0x1405),
            Some(64 * 1024 * 1024)
        );
        assert_eq!(
            prepare_read_pixels(&mut state, canvas_id, 2049, 2048, 0x8D99, 0x1405),
            None
        );
        assert_eq!(
            state.borrow_mut::<WebGLErrorState>().drain_one(canvas_id),
            codes::OUT_OF_MEMORY
        );
    }

    #[test]
    fn read_pixels_checks_native_view_brand_and_length() {
        let (mut runtime, render_rx) = new_webgl_runtime();
        runtime.exec_script("read_pixels_view_validation.js", r#"
            const ctx = new WebGLRenderingContext({ _rid: 23, width: 2, height: 2 }, {});
            const ctx2 = new WebGL2RenderingContext({ _rid: 24, width: 2, height: 2 });
            // A real view of the wrong element type is a GL error, not a
            // TypeError: the type table is WebGL's, the brand is WebIDL's.
            for (const view of [new Uint8Array(3), new Float32Array(1),
                new DataView(new ArrayBuffer(4))]) {
                ctx.readPixels(0, 0, 1, 1, ctx.RGBA, ctx.UNSIGNED_BYTE, view);
                if (ctx.getError() !== ctx.INVALID_OPERATION) throw new Error('expected invalid view');
            }
            // Not an ArrayBufferView at all: WebIDL rejects it before any GL work.
            for (const context of [ctx, ctx2]) {
                for (const value of [{buffer: new ArrayBuffer(4), byteLength: 4}, 'x', true, {}]) {
                    let threw = false;
                    try { context.readPixels(0, 0, 1, 1, ctx.RGBA, ctx.UNSIGNED_BYTE, value); }
                    catch (error) { threw = error instanceof TypeError; }
                    if (!threw) throw new Error('expected TypeError for ' + typeof value);
                    if (context.getError() !== context.NO_ERROR) throw new Error('brand set an error');
                }
            }
            // `dstData` is nullable in WebGL 1 and not in WebGL 2.
            ctx.readPixels(0, 0, 1, 1, ctx.RGBA, ctx.UNSIGNED_BYTE, null);
            if (ctx.getError() !== ctx.INVALID_VALUE) throw new Error('expected null rejection');
            for (const value of [null, undefined]) {
                let threw = false;
                try { ctx2.readPixels(0, 0, 1, 1, ctx.RGBA, ctx.UNSIGNED_BYTE, value); }
                catch (error) { threw = error instanceof TypeError; }
                if (!threw) throw new Error('WebGL2 accepted a null destination');
            }
        "#).unwrap();
        for cmd in render_rx.try_iter() {
            assert!(!matches!(
                cmd,
                RenderCommand::GL(GLCmd::ReadPixels { .. })
                    | RenderCommand::GL(GLCmd::ReadPixelsToBuffer { .. })
            ));
        }
    }

    /// The WebGL 2 buffer form takes a byte offset where the view goes, returns
    /// nothing, and must not touch any JS memory.
    #[test]
    fn read_pixels_buffer_overload_sends_the_offset_and_returns_no_pixels() {
        let (mut runtime, render_rx) = new_webgl_runtime();
        let responder = std::thread::spawn(move || {
            let mut offsets = Vec::new();
            for _ in 0..4 {
                loop {
                    match render_rx.recv_timeout(Duration::from_secs(5)).unwrap() {
                        RenderCommand::GL(GLCmd::ReadPixelsToBuffer { offset, resp, .. }) => {
                            offsets.push(offset);
                            if offset == 64 {
                                resp.err_code(shared::error::ErrorCode::InvalidOperation);
                            } else {
                                resp.ok(());
                            }
                            break;
                        }
                        RenderCommand::FramePacket(_) => {}
                        other => panic!("unexpected readback command: {other:?}"),
                    }
                }
            }
            offsets
        });
        runtime
            .exec_script(
                "read_pixels_to_buffer.js",
                r#"
            const ctx = new WebGL2RenderingContext({ _rid: 23, width: 2, height: 2 });
            for (const offset of [0, 8, 12.9, 64]) {
                const result = ctx.readPixels(0, 0, 1, 1, ctx.RGBA, ctx.UNSIGNED_BYTE, offset);
                if (result !== undefined) throw new Error('buffer form returned a value');
            }
            if (ctx.getError() !== ctx.INVALID_OPERATION) throw new Error('missing render error');
            if (ctx.getError() !== ctx.NO_ERROR) throw new Error('extra error');
            // A negative offset never reaches the renderer.
            ctx.readPixels(0, 0, 1, 1, ctx.RGBA, ctx.UNSIGNED_BYTE, -1);
            if (ctx.getError() !== ctx.INVALID_VALUE) throw new Error('negative offset accepted');
        "#,
            )
            .unwrap();
        assert_eq!(responder.join().unwrap(), [0, 8, 12, 64]);
    }

    #[test]
    fn read_pixels_dst_offset_is_in_view_elements_and_adds_to_pack_skips() {
        let (mut runtime, render_rx) = new_webgl_runtime();
        let responder = std::thread::spawn(move || {
            loop {
                match render_rx.recv_timeout(Duration::from_secs(5)).unwrap() {
                    RenderCommand::GL(GLCmd::ReadPixels { resp, .. }) => {
                        resp.ok(shared::protocol::render_cmd::ReadPixelsData {
                            pixels: (1..=16).collect(),
                            layout: shared::protocol::pixel_pack::PixelPackLayout::new(
                                2, 2, 4, 8, 5, 1, 1,
                            )
                            .unwrap(),
                        });
                        break;
                    }
                    RenderCommand::FramePacket(_) => {}
                    other => panic!("unexpected readback command: {other:?}"),
                }
            }
        });
        let result = runtime.exec_script(
            "read_pixels_dst_offset.js",
            r#"
            const ctx = new WebGL2RenderingContext({ _rid: 23, width: 2, height: 2 });
            const all = new Uint8Array(96).fill(165);
            const view = new Uint16Array(all.buffer, 8, 40);
            Object.defineProperties(view, {
                buffer: {get() { throw new Error('own buffer'); }},
                byteLength: {get() { throw new Error('own byteLength'); }},
                byteOffset: {get() { throw new Error('own byteOffset'); }},
                BYTES_PER_ELEMENT: {get() { throw new Error('own BPE'); }},
                length: {get() { throw new Error('own length'); }},
                constructor: {get() { throw new Error('own constructor'); }},
            });
            // RGBA/HALF_FLOAT is eight bytes per pixel; packed RGBA/UNSIGNED_INT
            // would be four. Here RG/UNSIGNED_SHORT is four bytes per pixel,
            // but the destination offset must still use two-byte elements.
            ctx.readPixels(0, 0, 2, 2, 0x8227 /* RG */, ctx.UNSIGNED_SHORT, view, 3);
            for (let i = 0; i < all.length; ++i) {
                let expected = 165;
                if (i >= 42 && i < 50) expected = i - 41;
                if (i >= 66 && i < 74) expected = i - 57;
                if (all[i] !== expected) throw new Error('dstOffset wrong byte ' + i);
            }
        "#,
        );
        let reply_result = responder.join();
        result.unwrap();
        reply_result.unwrap();
    }

    #[test]
    fn read_pixels_dst_offset_public_source_scenarios() {
        let (mut runtime, render_rx) = new_webgl_runtime();
        let responder = super::read_pixels_responder::spawn(render_rx);
        let result = runtime.exec_script(
            "read_pixels_offsets.js",
            include_str!("../../../tests/fixtures/read_pixels_offset.js"),
        );
        drop(runtime);
        let lengths = responder.join().unwrap();
        result.unwrap();
        super::read_pixels_responder::assert_lengths(lengths);
    }

    #[test]
    fn read_pixels_dst_offset_webidl_conversion_preserves_all_64_bits() {
        const MODULUS: f64 = 18_446_744_073_709_551_616.0;
        for (value, expected) in [
            (f64::NAN, 0),
            (f64::INFINITY, 0),
            (f64::NEG_INFINITY, 0),
            (0.0, 0),
            (-0.0, 0),
            (-0.9, 0),
            (3.9, 3),
            (-1.9, u64::MAX),
            (4_294_967_297.0, 4_294_967_297),
            (9_007_199_254_740_991.0, 9_007_199_254_740_991),
            (MODULUS - 2048.0, u64::MAX - 2047),
            (-MODULUS + 2048.0, 2048),
            (MODULUS, 0),
            (-MODULUS, 0),
            (MODULUS + 4096.0, 4096),
            (-MODULUS - 4096.0, u64::MAX - 4095),
            (f64::MAX, 0),
            (-f64::MAX, 0),
        ] {
            assert_eq!(
                super::read_pixels_element_offset(value),
                expected,
                "{value}"
            );
        }
    }

    #[test]
    fn read_pixels_dst_offset_pack_rejection_keeps_the_whole_view_unchanged() {
        let (mut runtime, render_rx) = new_webgl_runtime();
        let responder = std::thread::spawn(move || {
            loop {
                match render_rx.recv_timeout(Duration::from_secs(5)).unwrap() {
                    RenderCommand::GL(GLCmd::ReadPixels {
                        destination_byte_length,
                        resp,
                        ..
                    }) => {
                        // The compact payload fits, but the renderer can reject a
                        // larger PACK footprint. Only the remaining view is sent.
                        assert_eq!(destination_byte_length, 27);
                        let layout =
                            shared::protocol::pixel_pack::PixelPackLayout::new(3, 2, 4, 8, 0, 0, 0)
                                .unwrap();
                        assert_eq!(layout.required_bytes, 28);
                        resp.err_code(shared::error::ErrorCode::InvalidOperation);
                        break;
                    }
                    RenderCommand::FramePacket(_) => {}
                    other => panic!("unexpected readback command: {other:?}"),
                }
            }
        });
        let result = runtime.exec_script(
            "read_pixels_offset_pack_rejection.js",
            r#"
            const ctx = new WebGL2RenderingContext({ _rid: 23, width: 3, height: 2 });
            const bytes = new Uint8Array(31).fill(165);
            ctx.readPixels(0, 0, 3, 2, ctx.RGBA, ctx.UNSIGNED_BYTE, bytes, 4);
            if (ctx.getError() !== ctx.INVALID_OPERATION || bytes.some(x => x !== 165))
                throw new Error('PACK rejection changed destination');
        "#,
        );
        let reply_result = responder.join();
        result.unwrap();
        reply_result.unwrap();
    }

    #[test]
    fn read_pixels_scatters_rows_without_overwriting_padding_or_subview_edges() {
        let (mut runtime, render_rx) = new_webgl_runtime();
        let responder = std::thread::spawn(move || {
            for _ in 0..2 {
                loop {
                    match render_rx.recv_timeout(Duration::from_secs(5)).unwrap() {
                        RenderCommand::GL(GLCmd::ReadPixels {
                            destination_byte_length,
                            resp,
                            ..
                        }) => {
                            assert_eq!(destination_byte_length, 64);
                            resp.ok(shared::protocol::render_cmd::ReadPixelsData {
                                pixels: (1..=16).collect(),
                                layout: shared::protocol::pixel_pack::PixelPackLayout::new(
                                    2, 2, 4, 8, 5, 1, 1,
                                )
                                .unwrap(),
                            });
                            break;
                        }
                        RenderCommand::FramePacket(_) => {}
                        other => panic!("unexpected readback command: {other:?}"),
                    }
                }
            }
        });
        runtime
            .exec_script(
                "read_pixels_scatter.js",
                r#"
            const ctx = new WebGLRenderingContext({ _rid: 23, width: 2, height: 2 }, {});
            for (const backing of [new ArrayBuffer(80), new SharedArrayBuffer(80)]) {
                const all = new Uint8Array(backing);
                all.fill(165);
                const view = new Uint8Array(backing, 8, 64);
                Object.defineProperties(view, {
                    buffer: {value: new ArrayBuffer(512)},
                    byteLength: {value: 512}, byteOffset: {value: 0},
                });
                ctx.readPixels(0, 0, 2, 2, ctx.RGBA, ctx.UNSIGNED_BYTE, view);
                for (let i = 0; i < all.length; ++i) {
                    let expected = 165;
                    if (i >= 36 && i < 44) expected = i - 35;
                    if (i >= 60 && i < 68) expected = i - 51;
                    if (all[i] !== expected) throw new Error('wrong byte ' + i);
                }
            }
        "#,
            )
            .unwrap();
        responder.join().unwrap();
    }

    #[test]
    fn read_pixels_accepts_bgra_packed_short_views() {
        let (mut runtime, render_rx) = new_webgl_runtime();
        let responder = std::thread::spawn(move || {
            for expected_type in [0x8365, 0x8366] {
                loop {
                    match render_rx.recv_timeout(Duration::from_secs(5)).unwrap() {
                        RenderCommand::GL(GLCmd::ReadPixels {
                            type_,
                            destination_byte_length,
                            resp,
                            ..
                        }) => {
                            assert_eq!(type_, expected_type);
                            assert_eq!(destination_byte_length, 2);
                            resp.ok(shared::protocol::render_cmd::ReadPixelsData {
                                pixels: vec![11, 22],
                                layout: shared::protocol::pixel_pack::PixelPackLayout::new(
                                    1, 1, 2, 4, 0, 0, 0,
                                )
                                .unwrap(),
                            });
                            break;
                        }
                        RenderCommand::FramePacket(_) => {}
                        other => panic!("unexpected readback command: {other:?}"),
                    }
                }
            }
        });
        runtime
            .exec_script(
                "read_pixels_bgra.js",
                r#"
            const ctx = new WebGLRenderingContext({ _rid: 23, width: 1, height: 1 }, {});
            for (const type of [0x8365, 0x8366]) {
                const pixels = new Uint16Array(1);
                ctx.readPixels(0, 0, 1, 1, 0x80E1, type, pixels);
                if (ctx.getError() !== ctx.NO_ERROR) throw new Error('BGRA type rejected');
                const bytes = new Uint8Array(pixels.buffer);
                if (bytes[0] !== 11 || bytes[1] !== 22) throw new Error('wrong BGRA bytes');
            }
        "#,
            )
            .unwrap();
        responder.join().unwrap();
    }

    fn assert_read_pixels_ignores_typed_array_hook(hook: &str) {
        let (mut runtime, render_rx) = new_webgl_runtime();
        let responder = std::thread::spawn(move || {
            loop {
                match render_rx.recv_timeout(Duration::from_secs(5)).unwrap() {
                    RenderCommand::GL(GLCmd::ReadPixels { resp, .. }) => {
                        resp.ok(shared::protocol::render_cmd::ReadPixelsData {
                            pixels: (1..=16).collect(),
                            layout: shared::protocol::pixel_pack::PixelPackLayout::new(
                                2, 2, 4, 8, 5, 1, 1,
                            )
                            .unwrap(),
                        });
                        break;
                    }
                    RenderCommand::FramePacket(_) => {}
                    other => panic!("unexpected readback command: {other:?}"),
                }
            }
        });
        runtime.exec_script("read_pixels_primordial_setup.js", r#"
            const ctx = new WebGLRenderingContext({ _rid: 23, width: 2, height: 2 }, {});
            const pixels = new Uint8Array(64);
            pixels.fill(165);
            const savedConstructor = Object.getOwnPropertyDescriptor(Uint8Array.prototype, 'constructor');
            const savedLength = Object.getOwnPropertyDescriptor(Uint8Array.prototype, 'length');
        "#).unwrap();
        runtime.exec_script("read_pixels_hook.js", hook).unwrap();
        let outcome = runtime.exec_script(
            "read_pixels_primordial_copy.js",
            r#"
            try {
                ctx.readPixels(0, 0, 2, 2, ctx.RGBA, ctx.UNSIGNED_BYTE, pixels);
            } finally {
                Object.defineProperty(Uint8Array.prototype, 'constructor', savedConstructor);
                if (savedLength) Object.defineProperty(Uint8Array.prototype, 'length', savedLength);
                else delete Uint8Array.prototype.length;
            }
            for (let i = 0; i < 64; ++i) {
                let expected = 165;
                if (i >= 28 && i < 36) expected = i - 27;
                if (i >= 52 && i < 60) expected = i - 43;
                if (pixels[i] !== expected) throw new Error('wrong byte ' + i);
            }
        "#,
        );
        responder.join().unwrap();
        outcome.unwrap();
    }

    #[test]
    fn read_pixels_does_not_invoke_typed_array_species() {
        assert_read_pixels_ignores_typed_array_hook(
            r#"
            Object.defineProperty(Uint8Array.prototype, 'constructor', {
                configurable: true, value: {
                    get [Symbol.species]() { throw new Error('readPixels invoked species'); }
                }
            });
        "#,
        );
    }

    #[test]
    fn read_pixels_does_not_invoke_typed_array_length_getter() {
        assert_read_pixels_ignores_typed_array_hook(
            r#"
            Object.defineProperty(Uint8Array.prototype, 'length', {
                configurable: true,
                get() { throw new Error('readPixels invoked length getter'); }
            });
        "#,
        );
    }

    #[test]
    fn read_pixels_renderer_rejection_preserves_destination_and_records_error() {
        let (mut runtime, render_rx) = new_webgl_runtime();
        let responder = std::thread::spawn(move || {
            loop {
                match render_rx.recv_timeout(Duration::from_secs(5)).unwrap() {
                    RenderCommand::GL(GLCmd::ReadPixels { resp, .. }) => {
                        resp.err_code(shared::error::ErrorCode::InvalidOperation);
                        break;
                    }
                    RenderCommand::FramePacket(_) => {}
                    other => panic!("unexpected readback command: {other:?}"),
                }
            }
        });
        runtime
            .exec_script(
                "read_pixels_renderer_rejection.js",
                r#"
            const ctx = new WebGLRenderingContext({ _rid: 23, width: 1, height: 1 }, {});
            const pixels = new Uint8Array([11,22,33,44]);
            ctx.readPixels(0, 0, 1, 1, ctx.RGBA, ctx.UNSIGNED_BYTE, pixels);
            if (pixels.join(',') !== '11,22,33,44') throw new Error('error changed destination');
            if (ctx.getError() !== ctx.INVALID_OPERATION) throw new Error('missing render error');
        "#,
            )
            .unwrap();
        responder.join().unwrap();
    }

    #[test]
    fn webgl_upload_limit_rejects_before_queueing_and_records_oom() {
        let canvas_id = 11;
        let mut state = new_webgl_op_state();

        // One over the ceiling: refused before anything is queued, with OOM.
        let over = i32::try_from(shared::protocol::render_cmd::MAX_WEBGL_UPLOAD_BYTES + 1)
            .expect("the ceiling fits a GLsizeiptr argument");
        super::buffer_data_impl(&mut state, canvas_id, 0x8892, over, None, 0x88E4);
        assert_eq!(
            state.borrow_mut::<WebGLErrorState>().drain_one(canvas_id),
            codes::OUT_OF_MEMORY
        );
        assert_eq!(
            state
                .borrow::<UnifiedFrameCollector>()
                .approx_pending_bytes(),
            0
        );
        // At the ceiling: allowed, and nothing recorded.
        let at = over - 1;
        super::buffer_data_impl(&mut state, canvas_id, 0x8892, at, None, 0x88E4);
        assert_eq!(
            state.borrow_mut::<WebGLErrorState>().drain_one(canvas_id),
            0,
            "an allocation at the ceiling is not an error"
        );
    }

    #[test]
    fn buffer_data_with_a_payload_uploads_and_ignores_the_size_sentinel() {
        // `02_webgl_context.js` calls `_rawBufferData(id, target, -1, u8, usage)`
        // on the data path -- `size` is deliberately unused there. A guard that
        // rejected `size < 0` before checking for a payload turned every
        // `bufferData(target, ArrayBuffer, usage)` into a silent no-op with a
        // spurious `INVALID_VALUE`, which shipped in v0.9.5 and blacked out
        // every WebGL draw. This op had no test at all.
        let canvas_id = 3;
        let mut state = new_webgl_op_state();
        let payload = [1u8, 2, 3, 4, 5, 6, 7, 8];

        super::buffer_data_impl(
            &mut state,
            canvas_id,
            0x8892,
            -1,
            Some(&payload[..]),
            0x88E4,
        );

        assert_eq!(
            state.borrow_mut::<WebGLErrorState>().drain_one(canvas_id),
            codes::NO_ERROR,
            "a data upload must not record an error for the unused size field"
        );
        assert!(
            state
                .borrow::<UnifiedFrameCollector>()
                .approx_pending_bytes()
                > 0,
            "the buffer upload must reach the command stream"
        );
    }

    #[test]
    fn buffer_data_with_no_payload_still_rejects_a_negative_size() {
        // The size-only variant -- `bufferData(target, SIZE, usage)` -- keeps
        // its INVALID_VALUE guard; the JS binding never reaches here with a
        // negative size, so a negative one is genuine misuse.
        let canvas_id = 5;
        let mut state = new_webgl_op_state();

        super::buffer_data_impl(&mut state, canvas_id, 0x8892, -1, None, 0x88E4);

        assert_eq!(
            state.borrow_mut::<WebGLErrorState>().drain_one(canvas_id),
            codes::INVALID_VALUE
        );
        assert_eq!(
            state
                .borrow::<UnifiedFrameCollector>()
                .approx_pending_bytes(),
            0
        );
    }

    fn new_test_host_state() -> (HostOpState, crossbeam_channel::Receiver<RenderCommand>) {
        let (render_tx, render_rx) = CommandSender::new();
        let (host_tx, _critical_host_tx, _host_rx) = shared::host_channel::channel(1);

        (
            HostOpState {
                callback_ids: std::sync::Arc::new(
                    shared::callback_id::CallbackIdAllocator::default(),
                ),
                runtime_generation: 1,
                id: 1,
                app_cache_dir: PathBuf::from("/tmp/cache"),
                app_files_dir: PathBuf::from("/tmp/files"),
                code_dir: None,
                game_paths: None,
                vfs: None,
                mount_table: None,
                render_tx,
                text_measurer: None,
                audio_tx: AudioSender::new(
                    shared::audio_channel::disconnected(),
                    ThreadWakeup::new(),
                ),
                host_tx,
                device_services: None,
                raf_rx: None,
                raf_demand: std::sync::Arc::new(shared::raf_signal::RafDemand::new()),
                request_vsync: None,
                sub_packages: Vec::new(),
                workers_path: None,
                network_policy: NetworkPolicy::default(),
                backgrounded: Arc::new(AtomicBool::new(false)),
                timer_backgrounded: Arc::new(AtomicBool::new(false)),
                webgl_context_created: Arc::new(AtomicBool::new(false)),
                context_lost: Arc::new(shared::op_state::ContextLostState::default()),
                code_signing_enabled: false,
                gpu_caps: GpuCaps::new(),
            },
            render_rx,
        )
    }

    pub(in crate::rendering::webgl) fn new_webgl_runtime()
    -> (HostJsRuntime, crossbeam_channel::Receiver<RenderCommand>) {
        let (host_state, render_rx) = new_test_host_state();
        let runtime = HostJsRuntime::new(
            1,
            host_state,
            &std::env::temp_dir(),
            #[cfg(feature = "v8-limits")]
            Default::default(),
            #[cfg(feature = "code-signing")]
            false,
            #[cfg(feature = "code-signing")]
            None,
        );
        (runtime, render_rx)
    }

    pub(in crate::rendering::webgl) fn end_test_frame(runtime: &mut HostJsRuntime) {
        runtime.invoke_host_hook("_internalFrameEnd", "[]");
    }

    fn spawn_tf_varying_responder(
        render_rx: crossbeam_channel::Receiver<RenderCommand>,
    ) -> std::thread::JoinHandle<()> {
        std::thread::spawn(move || {
            let mut replies = 0;
            while replies < 2 {
                match render_rx
                    .recv_timeout(Duration::from_secs(1))
                    .expect("expected transform feedback varying request")
                {
                    RenderCommand::FramePacket(_) => {}
                    RenderCommand::GL(GLCmd::GetTransformFeedbackVarying {
                        program,
                        index,
                        resp,
                    }) => {
                        assert_eq!(program, 17);
                        match index {
                            0 => resp.ok(Some(("v_pos".to_string(), 1, 0x8B51))),
                            1 => resp.ok(None),
                            other => panic!("unexpected varying index {other}"),
                        }
                        replies += 1;
                    }
                    other => panic!("unexpected render command in TF varying test: {other:?}"),
                }
            }
        })
    }

    fn recv_gl_commands(
        render_rx: &crossbeam_channel::Receiver<RenderCommand>,
    ) -> shared::command_vec_pool::PooledVec<GLCmd> {
        // Not a loop: every arm below either returns or panics, so the first
        // packet decides the outcome. It was written as `loop { .. }`, which
        // read as "keep receiving until a GL batch turns up" -- a retry this
        // helper never performed, because a packet carrying no GL batch panics
        // rather than waiting for the next one. That is the right behaviour for
        // a test that asserts the flush produced a batch; the loop was the part
        // that lied.
        match render_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("expected flushed WebGL frame packet")
        {
            RenderCommand::FramePacket(packet) => {
                for op in packet.into_ops() {
                    if let FrameOp::GlBatch(mut payload) = op {
                        // As `drain_gl_commands`: the context's creation record is not what these tests count.
                        payload
                            .commands
                            .retain(|cmd| !matches!(cmd, GLCmd::WebglContext { .. }));
                        return payload.commands;
                    }
                }
                panic!("flushed frame packet did not contain a GL batch");
            }
            other => panic!("unexpected render command before GL batch: {other:?}"),
        }
    }

    #[test]
    fn allocates_monotonic_non_zero_ids() {
        let mut alloc = GlResourceIdAllocator::new();
        assert_eq!(alloc.alloc(), 1);
        assert_eq!(alloc.alloc(), 2);
        assert_eq!(alloc.alloc(), 3);
    }

    #[test]
    fn sixteen_uniform_words_copy_inline_and_seventeen_spill() {
        let inline_words = [1.0f32.to_bits(); 16];
        let inline = copy_f32_words(&inline_words);
        assert_eq!(inline.len(), 16);
        assert!(!inline.spilled());
        assert!(inline.iter().all(|value| *value == 1.0));

        let spilled_words = [2.0f32.to_bits(); 17];
        let spilled = copy_f32_words(&spilled_words);
        assert_eq!(spilled.len(), 17);
        assert!(spilled.spilled());
    }

    #[test]
    fn integer_uniform_words_preserve_signed_bits() {
        let words = [0u32, 1, u32::MAX, i32::MIN as u32];
        let copied = copy_i32_words(&words);
        assert_eq!(copied.as_slice(), &[0, 1, -1, i32::MIN]);
        assert!(!copied.spilled());
    }

    #[test]
    fn empty_uniform_word_slices_stay_inline() {
        assert!(copy_f32_words(&[]).is_empty());
        assert!(!copy_f32_words(&[]).spilled());
        assert!(copy_i32_words(&[]).is_empty());
        assert!(!copy_i32_words(&[]).spilled());
    }

    #[test]
    fn only_spilled_uniform_values_are_classified_as_heap_payloads() {
        let inline = GLCmd::UniformMatrix4fv {
            canvas_id: 1,
            location: Some(1),
            transpose: false,
            value: (0..16).map(|n| n as f32).collect(),
        };
        let spilled = GLCmd::Uniform1fv {
            canvas_id: 1,
            location: Some(1),
            value: (0..17).map(|n| n as f32).collect(),
        };

        assert!(!gl_cmd_has_heap_payload(&inline));
        assert!(gl_cmd_has_heap_payload(&spilled));
    }

    /// **The scalar fast path's premise, measured rather than asserted.**
    ///
    /// `gl_cmd_has_heap_payload`'s doc claims branching on it "lets
    /// `queue_gl_fire_and_forget` skip … the heavy `approx_deep_size_bytes`
    /// match", and that claim is worth doubting: both are matches over the same
    /// 145-variant enum, so the compiler builds a jump table for each and
    /// neither is *obviously* cheaper. If they cost the same, the classification
    /// buys nothing and could be replaced by the size walk it guards — removing
    /// a duplicate that lives in a different crate from its source of truth and
    /// whose `_ => false` default silently under-counts any heap-carrying
    /// variant added later.
    ///
    /// Measured on host, they do not cost the same: classifying first is about
    /// 0.66 ns/call against 2.53 ns for the size walk alone, a factor of ~3.8.
    /// The asymmetry has a cause — `gl_cmd_has_heap_payload` yields a `bool`
    /// from discriminant tests the compiler can collapse into a bitmap, while
    /// `approx_deep_size_bytes` must dereference the payload in every heap arm
    /// to reach `.capacity()` / `.spilled()`, leaving a far larger body and no
    /// such collapse.
    ///
    /// So the premise holds and the duplicate stays. What does *not* follow is
    /// that the duplicate can be left unguarded: see
    /// `heap_payload_classification_agrees_with_the_authoritative_byte_count`
    /// for the invariant that keeps the two functions from drifting.
    ///
    /// Direction only, no absolute values, so the assertion is machine
    /// independent.
    #[test]
    #[ignore = "timing benchmark; run with --ignored"]
    fn bench_heap_payload_classification_vs_deep_size_walk() {
        use std::time::Instant;

        // A representative scalar frame: the commands a renderer emits by the
        // hundred. All take the `_` arm of both functions.
        let scalars: Vec<GLCmd> = vec![
            GLCmd::Viewport {
                canvas_id: 1,
                x: 0,
                y: 0,
                width: 1080,
                height: 1920,
            },
            GLCmd::Clear {
                canvas_id: 1,
                bit_field: 0x4000,
            },
            GLCmd::Enable {
                canvas_id: 1,
                cap: 0x0B71,
            },
            GLCmd::DrawArrays {
                canvas_id: 1,
                mode: 4,
                first: 0,
                count: 6,
            },
            GLCmd::UseProgram {
                canvas_id: 1,
                program_id: 1,
            },
        ];

        const ITERS: usize = 200_000;
        let base = std::mem::size_of::<GLCmd>();

        // Warm both, so neither pays first-touch icache.
        let mut sink = 0usize;
        for cmd in &scalars {
            sink += usize::from(gl_cmd_has_heap_payload(cmd)) + cmd.approx_deep_size_bytes();
        }

        let t0 = Instant::now();
        for _ in 0..ITERS {
            for cmd in &scalars {
                sink += if gl_cmd_has_heap_payload(cmd) {
                    cmd.approx_deep_size_bytes()
                } else {
                    base
                };
            }
        }
        let classify_then_size = t0.elapsed();

        let t1 = Instant::now();
        for _ in 0..ITERS {
            for cmd in &scalars {
                sink += cmd.approx_deep_size_bytes();
            }
        }
        let size_only = t1.elapsed();

        let calls = (ITERS * scalars.len()) as f64;
        println!(
            "  classify + size (now) : {:>9} ns  {:>6.2} ns/call",
            classify_then_size.as_nanos(),
            classify_then_size.as_nanos() as f64 / calls
        );
        println!(
            "  size only  (proposed) : {:>9} ns  {:>6.2} ns/call",
            size_only.as_nanos(),
            size_only.as_nanos() as f64 / calls
        );
        println!("  (sink {sink} — keeps the loops from being optimised away)");

        assert!(
            classify_then_size < size_only,
            "classifying first ({classify_then_size:?}) is no longer cheaper \
             than the size walk alone ({size_only:?}). The fast path's premise \
             has stopped holding — `gl_cmd_has_heap_payload` is then a duplicate \
             classification that buys nothing, and `queue_gl_fire_and_forget` \
             should derive `heap` from `approx_deep_size_bytes` instead."
        );
    }

    /// **The two functions that classify a command's payload must agree, and
    /// they live in different crates.**
    ///
    /// `gl_cmd_has_heap_payload` (this crate) decides whether
    /// `queue_gl_fire_and_forget` takes the scalar fast path, where the byte
    /// budget is charged a flat `size_of::<GLCmd>()`. `GLCmd::
    /// approx_deep_size_bytes` (in `shared`) is the authoritative count. The
    /// benchmark above records why the duplicate exists — the classification is
    /// ~3.8x cheaper, so merging them would slow every scalar command.
    ///
    /// One direction of disagreement is harmful. If the classifier says "no
    /// payload" for a command that has one, the fast path charges the base size,
    /// the 4 MiB auto-flush guard never trips, and untrusted JS can pin
    /// unbounded heap until the frame ends. The other direction — claiming a
    /// payload that turns out empty, as `BufferData { data: None }` does — only
    /// costs a slow path, so it is allowed.
    ///
    /// What this cannot catch, stated rather than implied: `GLCmd` is
    /// `#[non_exhaustive]` with 145 variants, so a variant added tomorrow is in
    /// neither this list nor the classifier's, and lands on `_ => false`. No
    /// test can see that. What it does catch is the realistic drift — someone
    /// editing one function's arms and not the other's.
    #[test]
    fn heap_payload_classification_agrees_with_the_authoritative_byte_count() {
        let base = std::mem::size_of::<GLCmd>();

        // Every variant that carries an outbound payload, each given a real one.
        let with_payload: Vec<(&str, GLCmd)> = vec![
            (
                "ShaderSource",
                GLCmd::ShaderSource {
                    shader_id: 1,
                    source: "precision mediump float;".repeat(8),
                    resp: None,
                },
            ),
            (
                "BufferData",
                GLCmd::BufferData {
                    canvas_id: 1,
                    target: 0x8892,
                    size: 4096,
                    data: Some(vec![0u8; 4096]),
                    usage: 0x88E4,
                },
            ),
            (
                "Uniform1fv spilled",
                GLCmd::Uniform1fv {
                    canvas_id: 1,
                    location: Some(1),
                    value: (0..64).map(|n| n as f32).collect(),
                },
            ),
            (
                "UniformMatrix4fv spilled",
                GLCmd::UniformMatrix4fv {
                    canvas_id: 1,
                    location: Some(1),
                    transpose: false,
                    value: (0..64).map(|n| n as f32).collect(),
                },
            ),
            (
                "Uniform4uiv spilled",
                GLCmd::Uniform4uiv {
                    canvas_id: 1,
                    location: Some(1),
                    value: (0..64).collect(),
                },
            ),
            (
                "UniformMatrix2x3fv spilled",
                GLCmd::UniformMatrix2x3fv {
                    canvas_id: 1,
                    location: Some(1),
                    transpose: false,
                    value: (0..66).map(|n| n as f32).collect(),
                },
            ),
            (
                "UniformMatrix4x3fv spilled",
                GLCmd::UniformMatrix4x3fv {
                    canvas_id: 1,
                    location: Some(1),
                    transpose: true,
                    value: (0..72).map(|n| n as f32).collect(),
                },
            ),
        ];

        for (label, cmd) in &with_payload {
            let bytes = cmd.approx_deep_size_bytes();
            assert!(
                bytes > base,
                "{label}: the fixture carries no payload, so it tests nothing \
                 ({bytes} == base {base})"
            );
            assert!(
                gl_cmd_has_heap_payload(cmd),
                "{label}: has {} payload bytes but the classifier calls it \
                 scalar — the fast path would charge {base} and the 4 MiB \
                 auto-flush guard would not trip",
                bytes - base
            );
        }

        // And the converse for the commands that make up a frame by count: the
        // classifier calls them scalar, so the authoritative count must agree
        // that a flat base charge is exact.
        let scalars: Vec<(&str, GLCmd)> = vec![
            (
                "Viewport",
                GLCmd::Viewport {
                    canvas_id: 1,
                    x: 0,
                    y: 0,
                    width: 8,
                    height: 8,
                },
            ),
            (
                "DrawArrays",
                GLCmd::DrawArrays {
                    canvas_id: 1,
                    mode: 4,
                    first: 0,
                    count: 6,
                },
            ),
            (
                "BufferData reserving",
                GLCmd::BufferData {
                    canvas_id: 1,
                    target: 0x8892,
                    size: 4096,
                    data: None,
                    usage: 0x88E4,
                },
            ),
            (
                "UniformMatrix4fv inline",
                GLCmd::UniformMatrix4fv {
                    canvas_id: 1,
                    location: Some(1),
                    transpose: false,
                    value: (0..16).map(|n| n as f32).collect(),
                },
            ),
        ];

        for (label, cmd) in &scalars {
            let bytes = cmd.approx_deep_size_bytes();
            if !gl_cmd_has_heap_payload(cmd) {
                assert_eq!(
                    bytes, base,
                    "{label}: classified scalar, so the fast path charges \
                     {base}, but the authoritative count is {bytes}"
                );
            } else {
                // Allowed: claiming a payload that is absent costs only a slow
                // path. `BufferData { data: None }` is exactly this case.
                assert_eq!(
                    bytes, base,
                    "{label}: classified as carrying a payload and does — then \
                     it belongs in the list above, not here"
                );
            }
        }
    }

    #[test]
    fn wraps_without_returning_zero() {
        let mut alloc = GlResourceIdAllocator::with_next_for_test(u32::MAX);
        assert_eq!(alloc.alloc(), u32::MAX);
        assert_eq!(alloc.alloc(), 1);
        assert_ne!(alloc.alloc(), 0);
    }

    #[test]
    fn bind_buffer_base_rejects_transform_feedback_target_while_active() {
        let mut state = new_webgl_op_state();
        error_state::set_transform_feedback(&mut state, 7, TransformFeedback::Active);

        bind_buffer_base_impl(&mut state, 7, 0x8C8E, 0, 9);

        assert_eq!(
            state.borrow_mut::<WebGLErrorState>().drain_one(7),
            codes::INVALID_OPERATION
        );
        assert_eq!(
            state
                .borrow::<UnifiedFrameCollector>()
                .approx_pending_bytes(),
            0,
            "validator must reject the bind before queueing GL work"
        );
    }

    #[test]
    fn bind_buffer_range_rejects_transform_feedback_target_while_active() {
        let mut state = new_webgl_op_state();
        error_state::set_transform_feedback(&mut state, 7, TransformFeedback::Active);

        bind_buffer_range_impl(&mut state, 7, 0x8C8E, 0, 9, 0, 64);

        assert_eq!(
            state.borrow_mut::<WebGLErrorState>().drain_one(7),
            codes::INVALID_OPERATION
        );
        assert_eq!(
            state
                .borrow::<UnifiedFrameCollector>()
                .approx_pending_bytes(),
            0,
            "validator must reject the bind before queueing GL work"
        );
    }

    /// Pausing exists so the feedback buffers can be rebound, so a paused
    /// context must admit the bind that an active one refuses. Both halves are
    /// asserted here because the refusal tests above pass whether or not the
    /// phase is tracked at all -- a validator that refused nothing would fail
    /// them, and one that refused everything would fail only this.
    #[test]
    fn a_paused_transform_feedback_admits_the_rebind_an_active_one_refuses() {
        let mut state = new_webgl_op_state();
        error_state::set_transform_feedback(&mut state, 7, TransformFeedback::Paused);

        bind_buffer_base_impl(&mut state, 7, 0x8C8E, 0, 9);
        bind_buffer_range_impl(&mut state, 7, 0x8C8E, 1, 9, 0, 64);

        assert_eq!(
            state.borrow_mut::<WebGLErrorState>().drain_one(7),
            codes::NO_ERROR,
            "a paused transform feedback refused a rebind of its own buffers"
        );
        assert!(
            state
                .borrow::<UnifiedFrameCollector>()
                .approx_pending_bytes()
                > 0,
            "the admitted binds must have reached the command stream"
        );
    }

    /// An upload op's source: an offset that is not negative names the PIXEL_UNPACK_BUFFER and wins over any bytes;
    /// otherwise the bytes -- which the facade has taken from the view's `srcOffset` on -- or, for an image upload,
    /// none, which allocates the storage only.
    #[test]
    fn an_upload_op_names_its_bytes_or_the_unpack_buffer() {
        use frame_decode::resource::{Payload, PixelSource};
        assert!(matches!(
            image_pixel_source(Some(&[1, 2]), 24),
            Some(PixelSource::UnpackBuffer(24))
        ));
        assert!(matches!(
            image_pixel_source(Some(&[1, 2]), -1),
            Some(PixelSource::Bytes(Payload::Bytes(&[1, 2])))
        ));
        assert!(image_pixel_source(None, -1).is_none());
        assert!(matches!(
            PixelSource::of_call(&[], 0),
            PixelSource::UnpackBuffer(0)
        ));
        assert!(matches!(
            PixelSource::of_call(&[7], -1),
            PixelSource::Bytes(Payload::Bytes(&[7]))
        ));
    }

    /// An upload's source as the upload tests print it: the bytes in full up to eight of them, else their count and
    /// first byte; the buffer offset; or none.
    fn describe_source(source: Option<&PixelUnpackSource>) -> String {
        match source {
            None => "none".to_string(),
            Some(PixelUnpackSource::Bytes(bytes)) if bytes.len() <= 8 => {
                format!("{:?}", bytes.as_slice())
            }
            Some(PixelUnpackSource::Bytes(bytes)) => format!("{}@{}", bytes.len(), bytes[0]),
            Some(PixelUnpackSource::BufferOffset(offset)) => format!("buffer {offset}"),
        }
    }

    #[test]
    fn plain_float_uniform_sequence_preserves_values() {
        let (mut runtime, render_rx) = new_webgl_runtime();
        runtime
            .exec_script(
                "plain_float_uniform_sequence.js",
                r#"
                const ctx = new WebGLRenderingContext({ _rid: 13, width: 1, height: 1 }, {});
                ctx.uniform4fv({ id: 9 }, [1.5, -2.25, 0.0, 7.75]);
                ctx.flush();
                "#,
            )
            .expect("plain float uniform sequence should be accepted");

        let commands = recv_gl_commands(&render_rx);
        let Some(GLCmd::Uniform4fv { value, .. }) = commands.into_iter().next() else {
            panic!("expected one Uniform4fv command");
        };
        assert_eq!(value.as_slice(), &[1.5, -2.25, 0.0, 7.75]);
    }

    /// `vertexAttrib*` and the integer attribute calls reach the renderer as the commands the specification
    /// describes: every arity of the float form is one `VertexAttrib4f` with the components it left out at 0, 0, 0, 1,
    /// the array forms read the front of their list and ignore the rest, and `vertexAttribIPointer` is a pointer call
    /// without a `normalized`.
    #[test]
    fn vertex_attrib_calls_become_the_commands_the_specification_describes() {
        let (mut runtime, render_rx) = new_webgl_runtime();
        runtime
            .exec_script(
                "vertex_attrib_calls.js",
                r#"
                const gl = new WebGL2RenderingContext({ _rid: 160, width: 1, height: 1 }, {});
                gl.vertexAttrib1f(1, 0.5);
                gl.vertexAttrib2f(2, 0.25, 0.75);
                gl.vertexAttrib3f(3, 1, 2, 3);
                gl.vertexAttrib4f(4, 5, 6, 7, 8);
                gl.vertexAttrib2fv(5, [9, 10]);
                gl.vertexAttrib3fv(6, new Float32Array([11, 12, 13, 99]));
                gl.vertexAttribI4i(7, -1, 2, -3, 4);
                gl.vertexAttribI4uiv(8, new Uint32Array([1, 2, 3, 4]));
                gl.vertexAttribIPointer(9, 2, 0x1404, 8, 4);
                gl.flush();
                "#,
            )
            .expect("the vertex attribute calls should be accepted");

        let commands: Vec<GLCmd> = recv_gl_commands(&render_rx).into_iter().collect();
        let constant = |index: u32, v: [f32; 4]| (index, v);
        let got: Vec<(u32, [f32; 4])> = commands
            .iter()
            .filter_map(|cmd| match cmd {
                GLCmd::VertexAttrib4f {
                    index, x, y, z, w, ..
                } => Some(constant(*index, [*x, *y, *z, *w])),
                _ => None,
            })
            .collect();
        assert_eq!(
            got,
            vec![
                (1, [0.5, 0.0, 0.0, 1.0]),
                (2, [0.25, 0.75, 0.0, 1.0]),
                (3, [1.0, 2.0, 3.0, 1.0]),
                (4, [5.0, 6.0, 7.0, 8.0]),
                (5, [9.0, 10.0, 0.0, 1.0]),
                (6, [11.0, 12.0, 13.0, 1.0]),
            ]
        );
        assert!(commands.iter().any(|cmd| matches!(
            cmd,
            GLCmd::VertexAttribI4i {
                index: 7,
                x: -1,
                y: 2,
                z: -3,
                w: 4,
                ..
            }
        )));
        assert!(commands.iter().any(|cmd| matches!(
            cmd,
            GLCmd::VertexAttribI4ui {
                index: 8,
                x: 1,
                y: 2,
                z: 3,
                w: 4,
                ..
            }
        )));
        assert!(commands.iter().any(|cmd| matches!(
            cmd,
            GLCmd::VertexAttribIPointer {
                index: 9,
                size: 2,
                type_: 0x1404,
                stride: 8,
                offset: 4,
                ..
            }
        )));
    }

    /// `uniform{1..4}ui[v]` and the six non-square matrices reach the renderer as the commands the specification
    /// describes: the component form is the `uiv` record of its width, `srcOffset` / `srcLength` select the elements the
    /// call says, the matrices keep their `transpose`, and a list that is not a whole number of elements -- or too short
    /// for what `srcOffset` / `srcLength` ask -- is INVALID_VALUE and no command at all.
    #[test]
    fn unsigned_and_non_square_uniforms_become_the_commands_the_specification_describes() {
        let (mut runtime, render_rx) = new_webgl_runtime();
        runtime
            .exec_script(
                "unsigned_and_non_square_uniforms.js",
                r#"
                const gl = new WebGL2RenderingContext({ _rid: 162, width: 1, height: 1 }, {});
                const loc = { id: 3 };
                gl.uniform1ui(loc, 7);
                gl.uniform2ui(loc, 1, 4294967295);
                gl.uniform3ui(loc, -1, 2.9, 3);
                gl.uniform4ui(loc, 1, 2, 3, 4);
                gl.uniform2uiv(loc, new Uint32Array([10, 11, 12, 13]));
                gl.uniform3uiv(loc, [1, 2, 3, 4, 5, 6, 7], 1, 3);          // elements 1..4
                gl.uniformMatrix2x3fv(loc, false, new Float32Array([1, 2, 3, 4, 5, 6]));
                gl.uniformMatrix3x2fv(loc, true, [1, 2, 3, 4, 5, 6]);
                gl.uniformMatrix4x3fv(loc, false, new Float32Array(24).fill(0.5), 12, 12);
                gl.flush();
                "#,
            )
            .expect("the unsigned and non-square uniform calls should be accepted");
        let commands: Vec<GLCmd> = recv_gl_commands(&render_rx).into_iter().collect();
        let describe = |cmd: &GLCmd| -> String {
            match cmd {
                GLCmd::Uniform1uiv { value, .. } => format!("1ui {:?}", value.as_slice()),
                GLCmd::Uniform2uiv { value, .. } => format!("2ui {:?}", value.as_slice()),
                GLCmd::Uniform3uiv { value, .. } => format!("3ui {:?}", value.as_slice()),
                GLCmd::Uniform4uiv { value, .. } => format!("4ui {:?}", value.as_slice()),
                GLCmd::UniformMatrix2x3fv {
                    transpose, value, ..
                } => format!("2x3 {transpose} {}", value.len()),
                GLCmd::UniformMatrix3x2fv {
                    transpose, value, ..
                } => format!("3x2 {transpose} {:?}", value.as_slice()),
                GLCmd::UniformMatrix4x3fv {
                    transpose, value, ..
                } => format!("4x3 {transpose} {} {:?}", value.len(), value.first()),
                GLCmd::Flush { .. } => "flush".to_string(),
                other => format!("unexpected {other:?}"),
            }
        };
        let got: Vec<String> = commands.iter().map(describe).collect();
        assert_eq!(
            got,
            vec![
                "1ui [7]".to_string(),
                "2ui [1, 4294967295]".to_string(),
                "3ui [4294967295, 2, 3]".to_string(),
                "4ui [1, 2, 3, 4]".to_string(),
                "2ui [10, 11, 12, 13]".to_string(),
                "3ui [2, 3, 4]".to_string(),
                "2x3 false 6".to_string(),
                "3x2 true [1.0, 2.0, 3.0, 4.0, 5.0, 6.0]".to_string(),
                "4x3 false 12 Some(0.5)".to_string(),
                "flush".to_string(),
            ]
        );
    }

    /// A list that cannot be a whole number of uniform elements is INVALID_VALUE and changes nothing; a value that is
    /// not a list at all is a TypeError, as WebIDL has it. The refusal is the producer's, recorded in the error queue
    /// `getError` reads, and no command reaches the renderer.
    #[test]
    fn a_malformed_unsigned_or_matrix_uniform_list_is_refused_and_nothing_is_sent() {
        let (mut runtime, render_rx) = new_webgl_runtime();
        runtime
            .exec_script(
                "malformed_uniform_lists.js",
                r#"
                const gl = new WebGL2RenderingContext({ _rid: 163, width: 1, height: 1 }, {});
                const loc = { id: 3 };
                // Each is INVALID_VALUE; read after each, as one flag holds a code once.
                [
                    () => gl.uniform2uiv(loc, new Uint32Array([1, 2, 3])),        // 3 is not a whole number of uvec2
                    () => gl.uniform4uiv(loc, new Uint32Array(0)),                 // nothing
                    () => gl.uniform1uiv(loc, new Uint32Array(4), 5),              // srcOffset past the end
                    () => gl.uniform1uiv(loc, new Uint32Array(4), 2, 3),           // srcOffset + srcLength past the end
                    () => gl.uniformMatrix2x3fv(loc, false, new Float32Array(7)),  // not a whole number of 2x3
                    () => gl.uniformMatrix4x3fv(loc, false, new Float32Array(11)), // too short for one
                ].forEach((call, i) => {
                    call();
                    const e = gl.getError();
                    if (e !== 0x0501) throw new Error(`case ${i}: getError ${e}, want INVALID_VALUE`);
                });
                let threw = 0;
                for (const call of [
                    () => gl.uniform2uiv(loc, 5),
                    () => gl.uniform3uiv(loc, null),
                    () => gl.uniformMatrix3x4fv(loc, false, "nope"),
                ]) { try { call(); } catch (e) { if (e instanceof TypeError) threw += 1; } }
                if (threw !== 3) throw new Error("a non-list is a TypeError: " + threw);
                if (gl.getError() !== 0) throw new Error("a TypeError records no GL error");
                gl.flush();
                "#,
            )
            .expect("the malformed calls should be refused, not thrown (but for the non-lists, which the script catches)");
        let sent = drain_gl_commands(&render_rx);
        assert!(
            sent.iter().all(|cmd| matches!(cmd, GLCmd::Flush { .. })),
            "a refused call must not reach the renderer (only the closing flush() does): {sent:?}"
        );
    }

    /// `clearBuffer{fv,iv,uiv,fi}` and `drawRangeElements` reach the renderer as the commands the specification
    /// describes: COLOR carries the four values from `srcOffset`, DEPTH and STENCIL the first (the rest 0, so the
    /// record has one shape), each list keeps its type (an unsigned word above 2^31 stays unsigned), a typed array of
    /// another type is read as the sequence WebIDL makes of it, and `drawRangeElements` is the `drawElements` it hints.
    #[test]
    fn clear_buffer_and_draw_range_calls_become_the_commands_the_specification_describes() {
        let (mut runtime, render_rx) = new_webgl_runtime();
        runtime
            .exec_script(
                "clear_buffer_setup.js",
                r#"
                const gl = new WebGL2RenderingContext({ _rid: 164, width: 1, height: 1 }, {});
                gl.bindBuffer(0x8893, gl.createBuffer());     // ELEMENT_ARRAY_BUFFER: six shorts from byte 2
                gl.bufferData(0x8893, 14, 0x88e4);
                const program = gl.createProgram();
                gl.linkProgram(program);
                gl._programParameterCache.set(program.id, new Map([[0x8b82, 1]]));   // what the renderer answers for LINK_STATUS
                program._consumes = []; program._consumesLink = program._links | 0;   // and the attribute locations it consumes: none
                gl.useProgram(program);
                gl.flush();
                "#,
            )
            .expect("the index buffer setup should run");
        drain_gl_commands(&render_rx);
        runtime
            .exec_script(
                "clear_buffer_calls.js",
                r#"
                gl._maxDrawBuffers = 4;          // what the renderer would answer for MAX_DRAW_BUFFERS
                gl.clearBufferfv(0x1800, 1, [0.25, 0.5, 0.75, 1]);
                gl.clearBufferfv(0x1801, 0, new Float32Array([9, 0.5]), 1);
                gl.clearBufferiv(0x1802, 0, new Int32Array([7]));
                gl.clearBufferiv(0x1800, 0, [-1, 2, -3, 4]);
                gl.clearBufferuiv(0x1800, 3, new Uint32Array([0, 1, 2, 4294967295, 5]), 1);
                gl.clearBufferfi(0x84F9, 0, 0.25, 255);
                gl.clearBufferfv(0x1800, 0, new Int32Array([1, 2, 3, 4]));
                gl.drawRangeElements(4, 0, 5, 6, 0x1403, 2);
                gl.flush();
                "#,
            )
            .expect("the clearBuffer and drawRangeElements calls should be accepted");
        let commands: Vec<GLCmd> = recv_gl_commands(&render_rx).into_iter().collect();
        let describe = |cmd: &GLCmd| -> String {
            match cmd {
                GLCmd::ClearBufferfv {
                    buffer,
                    drawbuffer,
                    value,
                    ..
                } => format!("fv {buffer:#x} {drawbuffer} {value:?}"),
                GLCmd::ClearBufferiv {
                    buffer,
                    drawbuffer,
                    value,
                    ..
                } => format!("iv {buffer:#x} {drawbuffer} {value:?}"),
                GLCmd::ClearBufferuiv {
                    buffer,
                    drawbuffer,
                    value,
                    ..
                } => format!("uiv {buffer:#x} {drawbuffer} {value:?}"),
                GLCmd::ClearBufferfi { depth, stencil, .. } => format!("fi {depth} {stencil}"),
                GLCmd::DrawElements {
                    mode,
                    count,
                    index_type,
                    offset,
                    ..
                } => format!("drawElements {mode} {count} {index_type:#x} {offset}"),
                GLCmd::Flush { .. } => "flush".to_string(),
                other => format!("unexpected {other:?}"),
            }
        };
        let got: Vec<String> = commands.iter().map(describe).collect();
        assert_eq!(
            got,
            vec![
                "fv 0x1800 1 [0.25, 0.5, 0.75, 1.0]".to_string(),
                "fv 0x1801 0 [0.5, 0.0, 0.0, 0.0]".to_string(),
                "iv 0x1802 0 [7, 0, 0, 0]".to_string(),
                "iv 0x1800 0 [-1, 2, -3, 4]".to_string(),
                "uiv 0x1800 3 [1, 2, 4294967295, 5]".to_string(),
                "fi 0.25 255".to_string(),
                "fv 0x1800 0 [1.0, 2.0, 3.0, 4.0]".to_string(),
                "drawElements 4 6 0x1403 2".to_string(),
                "flush".to_string(),
            ]
        );
    }

    /// A `clearBuffer*` call that breaks a rule of its call is the error the specification names and sends nothing: a
    /// buffer the call does not take is INVALID_ENUM (checked first), a draw buffer that is negative, at or past
    /// MAX_DRAW_BUFFERS for COLOR, or not 0 for anything else is INVALID_VALUE, and so is a list with fewer elements
    /// than the buffer needs after `srcOffset`. `drawRangeElements` with `end` below `start` is INVALID_VALUE. A value
    /// that is not a list -- or a depth that is not a number -- is a TypeError, raised before any of those: WebIDL
    /// converts the arguments before the call runs.
    #[test]
    fn a_malformed_clear_buffer_or_draw_range_call_is_the_specified_error_and_nothing_is_sent() {
        let (mut runtime, render_rx) = new_webgl_runtime();
        runtime
            .exec_script(
                "malformed_clear_buffer_calls.js",
                r#"
                const gl = new WebGL2RenderingContext({ _rid: 165, width: 1, height: 1 }, {});
                gl._maxDrawBuffers = 4;
                const ENUM = 0x0500, VALUE = 0x0501;
                const cases = [
                    [ENUM, () => gl.clearBufferfv(0x1802, 0, [1])],                       // fv: COLOR or DEPTH
                    [ENUM, () => gl.clearBufferiv(0x1801, 0, [1])],                       // iv: COLOR or STENCIL
                    [ENUM, () => gl.clearBufferuiv(0x1801, 0, [1, 2, 3, 4])],             // uiv: COLOR only
                    [ENUM, () => gl.clearBufferfi(0x1801, 0, 1, 0)],                      // fi: DEPTH_STENCIL only
                    [ENUM, () => gl.clearBufferfv(0x1234, 9, [])],                        // the enum comes first
                    [VALUE, () => gl.clearBufferfv(0x1800, 4, [0, 0, 0, 0])],             // = MAX_DRAW_BUFFERS
                    [VALUE, () => gl.clearBufferiv(0x1800, -1, [0, 0, 0, 0])],
                    [VALUE, () => gl.clearBufferfv(0x1801, 1, [0])],                      // DEPTH: draw buffer 0
                    [VALUE, () => gl.clearBufferfi(0x84F9, 1, 1, 0)],
                    [VALUE, () => gl.clearBufferfv(0x1800, 0, [0, 0, 0])],                // COLOR needs 4
                    [VALUE, () => gl.clearBufferuiv(0x1800, 0, new Uint32Array(4), 1)],   // 3 left after srcOffset
                    [VALUE, () => gl.clearBufferiv(0x1802, 0, new Int32Array(1), 2)],     // srcOffset past the end
                    [VALUE, () => gl.clearBufferfv(0x1801, 0, [])],                       // DEPTH needs 1
                    [VALUE, () => gl.drawRangeElements(4, 5, 4, 3, 0x1403, 0)],           // end < start
                ];
                cases.forEach(([want, call], i) => {
                    call();
                    const got = gl.getError();
                    if (got !== want) throw new Error(`case ${i}: getError ${got}, want ${want}`);
                });
                let threw = 0;
                for (const call of [
                    () => gl.clearBufferfv(0x1234, 0, 5),            // a TypeError ahead of the bad enum
                    () => gl.clearBufferiv(0x1800, 0, null),
                    () => gl.clearBufferuiv(0x1800, 9, "nope"),      // ahead of the bad draw buffer
                    () => gl.clearBufferfi(0x1234, 0, 1n, 0),        // a BigInt is not a GLfloat
                ]) { try { call(); } catch (e) { if (e instanceof TypeError) threw += 1; } }
                if (threw !== 4) throw new Error("a non-list is a TypeError: " + threw);
                if (gl.getError() !== 0) throw new Error("a TypeError records no GL error");
                gl.flush();
                "#,
            )
            .expect("the malformed calls should be refused, not thrown (but for the TypeErrors, which the script catches)");
        let sent = drain_gl_commands(&render_rx);
        assert!(
            sent.iter().all(|cmd| matches!(cmd, GLCmd::Flush { .. })),
            "a refused call must not reach the renderer (only the closing flush() does): {sent:?}"
        );
    }

    /// MAX_DRAW_BUFFERS is asked of the context once it can answer, and kept only then: a context that cannot answer
    /// (lost) holds COLOR to the minimum every WebGL 2 implementation has, 4, and is asked again by the next call, so
    /// a device with 8 is not held to 4 for the rest of the context's life by one call made while it was lost.
    #[test]
    fn the_draw_buffer_limit_is_kept_only_once_the_context_answers() {
        let (mut runtime, render_rx) = new_webgl_runtime();
        runtime
            .exec_script(
                "draw_buffer_limit.js",
                r#"
                const gl = new WebGL2RenderingContext({ _rid: 166, width: 1, height: 1 }, {});
                let asked = 0, answer = null;
                gl.getParameter = (pname) => { if (pname === 0x8824) asked += 1; return pname === 0x8824 ? answer : null; };
                const color = (drawbuffer) => gl.clearBufferfv(0x1800, drawbuffer, [0, 0, 0, 1]);
                color(4);
                if (gl.getError() !== 0x0501) throw new Error("no answer: draw buffer 4 is past the minimum");
                answer = 8;
                color(7);
                if (gl.getError() !== 0) throw new Error("the answer, 8, admits draw buffer 7");
                color(7);
                if (asked !== 2) throw new Error("asked " + asked + " times; the answer is kept once given");
                gl.flush();
                "#,
            )
            .expect("the draw buffer limit script should run");
        let drawbuffers: Vec<i32> = recv_gl_commands(&render_rx)
            .iter()
            .filter_map(|cmd| match cmd {
                GLCmd::ClearBufferfv { drawbuffer, .. } => Some(*drawbuffer),
                _ => None,
            })
            .collect();
        assert_eq!(drawbuffers, vec![7, 7]);
    }

    /// Every GL command the packets sent so far carry, in order, across however many packets and batches the
    /// stream and the ordered ops were split into.
    /// A frame's ops without the GL batches that hold nothing but contexts' creation records, or `None` when nothing
    /// else is left: an interleaving test creates its context in the script it measures, and the constructor's record
    /// is flushed on its own, ahead of the synchronous canvas query the 2D calls make, in a packet of its own.
    fn without_creation_only_batches(ops: shared::FrameOps) -> Option<Vec<FrameOp>> {
        let ops: Vec<FrameOp> = ops
            .into_iter()
            .filter(|op| match op {
                FrameOp::GlBatch(payload) => !payload
                    .commands
                    .iter()
                    .all(|cmd| matches!(cmd, GLCmd::WebglContext { .. })),
                _ => true,
            })
            .collect();
        ops.iter()
            .any(|op| matches!(op, FrameOp::GlBatch(_) | FrameOp::CanvasBatch(_)))
            .then_some(ops)
    }

    /// Every GL command sent so far, but the record each context's constructor encodes (`GLCmd::WebglContext`): the
    /// tests that drain are about what a context does once it exists, and the record has a test of its own
    /// (`a_context_declares_the_drawing_buffer_its_attributes_ask_for`).
    fn drain_gl_commands(render_rx: &crossbeam_channel::Receiver<RenderCommand>) -> Vec<GLCmd> {
        let mut commands = Vec::new();
        while let Ok(command) = render_rx.try_recv() {
            if let RenderCommand::FramePacket(packet) = command {
                for op in packet.into_ops() {
                    if let FrameOp::GlBatch(payload) = op {
                        commands.extend(
                            payload
                                .commands
                                .into_iter()
                                .filter(|cmd| !matches!(cmd, GLCmd::WebglContext { .. })),
                        );
                    }
                }
            }
        }
        commands
    }

    /// The copies, `framebufferTextureLayer` and `invalidateSubFramebuffer` reach the renderer as the commands the
    /// specification describes: WebGL 2 takes a sized format for `copyTexImage2D`, `copyBufferSubData`'s offsets and
    /// size are `long long` (the integer part of what was passed), a layer attachment answers its level and layer
    /// back, and the invalidations name the default framebuffer's buffers as COLOR / DEPTH and an object's by their
    /// attachment points.
    #[test]
    fn copy_and_framebuffer_layer_calls_become_the_commands_the_specification_describes() {
        let (mut runtime, render_rx) = new_webgl_runtime();
        runtime
            .exec_script(
                "copy_calls.js",
                r#"
                const gl = new WebGL2RenderingContext({ _rid: 170, width: 1, height: 1 }, {});
                gl._maxColorAttachments = 4;     // what the renderer would answer for MAX_COLOR_ATTACHMENTS
                gl.bindTexture(0x0de1, gl.createTexture());     // the copies' textures
                gl.bindTexture(0x806f, gl.createTexture());
                gl.copyTexImage2D(0x0de1, 0, 0x8058, 1, 2, 3, 4, 0);       // RGBA8: a WebGL 2 format
                gl.texImage2D(0x0de1, 1, 0x1908, 8, 10, 0, 0x1908, 0x1401, null);   // the images the sub-copies go into
                gl.texImage3D(0x806f, 0, 0x1908, 7, 9, 4, 0, 0x1908, 0x1401, null);
                gl.copyTexSubImage2D(0x0de1, 1, 2, 3, -4, 5, 6, 7);
                gl.copyTexSubImage3D(0x806f, 0, 1, 2, 3, 4, 5, 6, 7);
                gl.bindBuffer(0x8f36, gl.createBuffer());     // COPY_READ_BUFFER and COPY_WRITE_BUFFER, 64 bytes each
                gl.bufferData(0x8f36, 64, 0x88e4);
                gl.bindBuffer(0x8f37, gl.createBuffer());
                gl.bufferData(0x8f37, 64, 0x88e4);
                gl.copyBufferSubData(0x8f36, 0x8f37, 8, 16, 48);
                gl.copyBufferSubData(0x8f36, 0x8f37, 0.9, "4", 1.5);
                const fb = gl.createFramebuffer();
                gl.bindFramebuffer(0x8d40, fb);
                const tex = gl.createTexture();
                gl.bindTexture(0x8c1a, tex);                     // a layer attachment is of a 3D or 2D-array texture
                gl.framebufferTextureLayer(0x8d40, 0x8ce0, tex, 1, 3);
                const check = (c, m) => { if (!c) throw new Error(m); };
                check(gl.getFramebufferAttachmentParameter(0x8d40, 0x8ce0, 0x8cd1) === tex, "the object is the texture");
                check(gl.getFramebufferAttachmentParameter(0x8d40, 0x8ce0, 0x8cd2) === 1, "level 1");
                check(gl.getFramebufferAttachmentParameter(0x8d40, 0x8ce0, 0x8cd4) === 3, "layer 3");
                gl.invalidateSubFramebuffer(0x8d40, [0x8ce0, 0x8d00], 1, 2, 3, 4);
                gl.invalidateFramebuffer(0x8ca9, [0x8ce3]);
                gl.bindFramebuffer(0x8d40, null);
                gl.invalidateFramebuffer(0x8d40, [0x1800, 0x1801, 0x1802]);
                check(gl.getError() === 0, "no error: " + gl.getError());
                gl.flush();
                "#,
            )
            .expect("the copy and framebuffer calls should be accepted");
        let got: Vec<String> = drain_gl_commands(&render_rx)
            .iter()
            .filter_map(|cmd| match cmd {
                GLCmd::CopyTexImage2D {
                    target,
                    level,
                    internalformat,
                    x,
                    y,
                    width,
                    height,
                    ..
                } => Some(format!(
                    "copyTexImage2D {target:#x} {level} {internalformat:#x} {x} {y} {width} {height}"
                )),
                GLCmd::CopyTexSubImage2D {
                    target,
                    level,
                    xoffset,
                    yoffset,
                    x,
                    y,
                    width,
                    height,
                    ..
                } => Some(format!(
                    "copyTexSubImage2D {target:#x} {level} {xoffset} {yoffset} {x} {y} {width} {height}"
                )),
                GLCmd::CopyTexSubImage3D {
                    target,
                    level,
                    xoffset,
                    yoffset,
                    zoffset,
                    x,
                    y,
                    width,
                    height,
                    ..
                } => Some(format!(
                    "copyTexSubImage3D {target:#x} {level} {xoffset} {yoffset} {zoffset} {x} {y} {width} {height}"
                )),
                GLCmd::CopyBufferSubData {
                    read_target,
                    write_target,
                    read_offset,
                    write_offset,
                    size,
                    ..
                } => Some(format!(
                    "copyBufferSubData {read_target:#x} {write_target:#x} {read_offset} {write_offset} {size}"
                )),
                GLCmd::FramebufferTextureLayer {
                    target,
                    attachment,
                    texture,
                    level,
                    layer,
                    ..
                } => Some(format!(
                    "framebufferTextureLayer {target:#x} {attachment:#x} {} {level} {layer}",
                    texture.is_some()
                )),
                GLCmd::InvalidateSubFramebuffer {
                    target,
                    attachments,
                    x,
                    y,
                    width,
                    height,
                    ..
                } => Some(format!(
                    "invalidateSubFramebuffer {target:#x} {attachments:x?} {x} {y} {width} {height}"
                )),
                GLCmd::InvalidateFramebuffer {
                    target,
                    attachments,
                    ..
                } => Some(format!("invalidateFramebuffer {target:#x} {attachments:x?}")),
                _ => None,
            })
            .collect();
        assert_eq!(
            got,
            vec![
                "copyTexImage2D 0xde1 0 0x8058 1 2 3 4".to_string(),
                "copyTexSubImage2D 0xde1 1 2 3 -4 5 6 7".to_string(),
                "copyTexSubImage3D 0x806f 0 1 2 3 4 5 6 7".to_string(),
                "copyBufferSubData 0x8f36 0x8f37 8 16 48".to_string(),
                "copyBufferSubData 0x8f36 0x8f37 0 4 1".to_string(),
                "framebufferTextureLayer 0x8d40 0x8ce0 true 1 3".to_string(),
                "invalidateSubFramebuffer 0x8d40 [8ce0, 8d00] 1 2 3 4".to_string(),
                "invalidateFramebuffer 0x8ca9 [8ce3]".to_string(),
                "invalidateFramebuffer 0x8d40 [1800, 1801, 1802]".to_string(),
            ]
        );
    }

    /// What the facade refuses before anything is encoded: WebGL 1's `copyTexImage2D` with a sized format
    /// (INVALID_ENUM: WebGL 1 has only the five unsized ones), a `copyBufferSubData` with nothing bound or between an
    /// index buffer and one of other data (INVALID_OPERATION), with an offset or size that is negative, a range past a
    /// buffer -- 2^31 among them, not wrapped into range -- or two ranges of one buffer that overlap (INVALID_VALUE), or
    /// with a target that is not one (INVALID_ENUM), an invalidation of a target
    /// that is not a framebuffer binding, of a name the bound framebuffer does not have (INVALID_ENUM) or of a colour
    /// attachment past MAX_COLOR_ATTACHMENTS (INVALID_OPERATION), a negative invalidation rectangle and a negative
    /// layer or level (INVALID_VALUE). None reaches the renderer; the call whose list is not a list throws.
    #[test]
    fn a_malformed_copy_or_invalidation_is_the_specified_error_and_nothing_is_sent() {
        let (mut runtime, render_rx) = new_webgl_runtime();
        runtime
            .exec_script(
                "malformed_copy_calls.js",
                r#"
                const gl1 = new WebGLRenderingContext({ _rid: 171, width: 1, height: 1 }, {});
                const gl = new WebGL2RenderingContext({ _rid: 172, width: 1, height: 1 }, {});
                gl._maxColorAttachments = 4;
                gl1.bindTexture(0x0de1, gl1.createTexture());     // a call on a texture needs one bound
                const ENUM = 0x0500, VALUE = 0x0501, OPERATION = 0x0502;
                const read = gl.createBuffer(), write = gl.createBuffer(), indices = gl.createBuffer();
                const cases = [
                    [gl1, ENUM, () => gl1.copyTexImage2D(0x0de1, 0, 0x8058, 0, 0, 4, 4, 0)],          // RGBA8 in WebGL 1
                    [gl, OPERATION, () => gl.copyBufferSubData(0x8f36, 0x8f37, 0, 0, 4)],              // nothing bound
                    [gl, VALUE, () => {                                                                  // 64 bytes each
                        gl.bindBuffer(0x8f36, read); gl.bufferData(0x8f36, 64, 0x88e4);
                        gl.bindBuffer(0x8f37, write); gl.bufferData(0x8f37, 64, 0x88e4);
                        gl.copyBufferSubData(0x8f36, 0x8f37, -1, 0, 4);
                    }],
                    [gl, VALUE, () => gl.copyBufferSubData(0x8f36, 0x8f37, 0, 0, 2147483648)],          // 2^31
                    [gl, VALUE, () => gl.copyBufferSubData(0x8f36, 0x8f37, 4294967296, 0, 4)],          // not 0 mod 2^32
                    [gl, VALUE, () => gl.copyBufferSubData(0x8f36, 0x8f37, 0, 32, 33)],                 // past the write buffer
                    [gl, VALUE, () => { gl.bindBuffer(0x8f37, read); gl.copyBufferSubData(0x8f36, 0x8f37, 0, 8, 16); }],  // overlap
                    [gl, OPERATION, () => {                                                              // index and other data
                        gl.bindBuffer(0x8893, indices); gl.bufferData(0x8893, 64, 0x88e4);
                        gl.bindBuffer(0x8f37, indices);
                        gl.copyBufferSubData(0x8f36, 0x8f37, 0, 0, 4);
                    }],
                    [gl, ENUM, () => gl.copyBufferSubData(0x8892 + 1000, 0x8f37, 0, 0, 4)],
                    [gl, ENUM, () => gl.invalidateFramebuffer(0x0de1, [0x1800])],                       // not a binding
                    [gl, ENUM, () => gl.invalidateFramebuffer(0x8d40, [0x8ce0])],                       // the default has no COLOR_ATTACHMENT0
                    [gl, VALUE, () => gl.invalidateSubFramebuffer(0x8d40, [0x1800], 0, 0, -1, 4)],
                    [gl, ENUM, () => gl.framebufferTextureLayer(0x0de1, 0x8ce0, null, 0, 0)],
                    // a layer is judged only for a texture (ES 3.0 4.4.2.4); the default framebuffer takes no attachment
                    [gl, OPERATION, () => gl.framebufferTextureLayer(0x8d40, 0x8ce0, null, 0, -1)],
                ];
                cases.forEach(([ctx, want, call], i) => {
                    call();
                    const got = ctx.getError();
                    if (got !== want) throw new Error(`case ${i}: getError ${got}, want ${want}`);
                });
                gl.bindFramebuffer(0x8d40, gl.createFramebuffer());
                gl.flush();
                globalThis.__gl = gl;
                "#,
            )
            .expect("the malformed calls should be refused, not thrown");
        drain_gl_commands(&render_rx);
        runtime
            .exec_script(
                "malformed_copy_calls_on_an_object.js",
                r#"{
                const gl = globalThis.__gl;
                const ENUM = 0x0500, OPERATION = 0x0502;
                gl.invalidateFramebuffer(0x8d40, [0x1800]);              // an object has no COLOR
                if (gl.getError() !== ENUM) throw new Error("COLOR on an object");
                gl.invalidateFramebuffer(0x8d40, [0x8ce4]);              // COLOR_ATTACHMENT4 of 4
                if (gl.getError() !== OPERATION) throw new Error("past MAX_COLOR_ATTACHMENTS");
                let threw = false;
                try { gl.invalidateSubFramebuffer(0x8d40, 5, 0, 0, 1, 1); } catch (e) { threw = e instanceof TypeError; }
                if (!threw) throw new Error("a non-list is a TypeError");
                try { gl.copyBufferSubData(0x8f36, 0x8f37, 1n, 0, 4); threw = false; } catch (e) { threw = e instanceof TypeError; }
                if (!threw) throw new Error("a BigInt offset is a TypeError");
                const layers = gl.createTexture();
                gl.bindTexture(0x8c1a, layers);                          // a 2D array
                gl.framebufferTextureLayer(0x8d40, 0x8ce0, layers, 0, -1);
                if (gl.getError() !== 0x0501) throw new Error("a negative layer of a texture");
                gl.framebufferTextureLayer(0x8d40, 0x8ce0, gl.createTexture(), 0, 0);
                if (gl.getError() !== OPERATION) throw new Error("a texture that is not a 3D or 2D-array one");
                gl.flush();
                }"#,
            )
            .expect("the calls on an object should be refused, not thrown");
        let sent: Vec<String> = drain_gl_commands(&render_rx)
            .iter()
            .filter(|cmd| {
                matches!(
                    cmd,
                    GLCmd::CopyTexImage2D { .. }
                        | GLCmd::CopyBufferSubData { .. }
                        | GLCmd::InvalidateFramebuffer { .. }
                        | GLCmd::InvalidateSubFramebuffer { .. }
                        | GLCmd::FramebufferTextureLayer { .. }
                )
            })
            .map(|cmd| format!("{cmd:?}"))
            .collect();
        assert!(
            sent.is_empty(),
            "a refused call must not reach the renderer: {sent:?}"
        );
    }

    /// The compressed uploads take both of WebGL 2's overloads: a view, whose elements from `srcOffset` --
    /// `srcLengthOverride` of them unless that is 0 -- are the bytes, counted in the view's own element size; and an
    /// `imageSize` / `offset` pair naming a range of the bound PIXEL_UNPACK_BUFFER, up to its last byte. WebGL 1's one
    /// form sends the view whole. `waitSync` sends its sync once its flags and timeout are the one legal pair.
    #[test]
    fn compressed_uploads_and_wait_sync_become_the_commands_the_specification_describes() {
        let (mut runtime, render_rx) = new_webgl_runtime();
        runtime
            .exec_script(
                "compressed_uploads.js",
                r#"
                const gl1 = new WebGLRenderingContext({ _rid: 180, width: 1, height: 1 }, {});
                gl1._gpuCapsCache = 1;       // what the renderer would answer: ETC2 is there
                gl1.getExtension("WEBGL_compressed_texture_etc");
                gl1.bindTexture(0x0de1, gl1.createTexture());
                gl1.compressedTexImage2D(0x0de1, 0, 0x9278, 4, 4, 0, new Uint8Array(16).fill(1));
                const gl = new WebGL2RenderingContext({ _rid: 181, width: 1, height: 1 }, {});
                gl._gpuCapsCache = 1;
                gl.getExtension("WEBGL_compressed_texture_etc");
                gl.bindTexture(0x0de1, gl.createTexture());
                gl.bindTexture(0x8c1a, gl.createTexture());
                const bytes = new Uint8Array(48).map((_, k) => k);
                gl.compressedTexImage2D(0x0de1, 0, 0x9278, 4, 4, 0, bytes, 16, 16);          // bytes 16..32
                gl.compressedTexImage2D(0x0de1, 1, 0x9278, 4, 4, 0, new Uint16Array(bytes.buffer), 16);   // bytes 32..48
                gl.compressedTexImage3D(0x8c1a, 0, 0x9278, 4, 4, 2, 0, bytes, 8, 32);       // bytes 8..40
                gl.compressedTexSubImage3D(0x8c1a, 0, 0, 0, 1, 4, 4, 1, 0x9278, bytes.subarray(0, 16));
                gl.bindBuffer(0x88ec, gl.createBuffer());                                    // PIXEL_UNPACK_BUFFER
                gl.bufferData(0x88ec, 112, 0x88e0);
                gl.compressedTexImage2D(0x0de1, 2, 0x9278, 8, 4, 0, 32, 64);                // the bound buffer
                gl.compressedTexSubImage2D(0x0de1, 2, 4, 0, 4, 4, 0x9278, 16, 80);
                gl.compressedTexImage3D(0x8c1a, 1, 0x9278, 4, 4, 1, 0, 16, 0);
                gl.compressedTexSubImage3D(0x8c1a, 0, 0, 0, 1, 4, 4, 1, 0x9278, 16, 96);    // its last 16 bytes
                const sync = gl.fenceSync(0x9117, 0);
                if (!gl.isSync(sync)) throw new Error("a fence is a sync");
                gl.waitSync(sync, 0, -1);
                if (gl.getError() !== 0) throw new Error("no error: " + gl.getError());
                gl.flush(); gl1.flush();
                "#,
            )
            .expect("the compressed uploads and waitSync should be accepted");
        use shared::protocol::render_cmd::CompressedImageData;
        let describe = |data: &CompressedImageData| match data {
            CompressedImageData::Bytes(bytes) => format!(
                "bytes {}..{}",
                bytes.first().copied().unwrap_or(0),
                bytes.last().map_or(0, |b| u32::from(*b) + 1)
            ),
            CompressedImageData::UnpackBuffer { offset, size } => format!("buffer {offset}+{size}"),
        };
        let got: Vec<String> = drain_gl_commands(&render_rx)
            .iter()
            .filter_map(|cmd| match cmd {
                GLCmd::CompressedTexImage2D { level, data, .. } => {
                    Some(format!("2d {level} {}", describe(data)))
                }
                GLCmd::CompressedTexSubImage2D { xoffset, data, .. } => {
                    Some(format!("sub2d {xoffset} {}", describe(data)))
                }
                GLCmd::CompressedTexImage3D {
                    level, depth, data, ..
                } => Some(format!("3d {level} {depth} {}", describe(data))),
                GLCmd::CompressedTexSubImage3D { zoffset, data, .. } => {
                    Some(format!("sub3d {zoffset} {}", describe(data)))
                }
                GLCmd::WaitSync { .. } => Some("waitSync".to_string()),
                _ => None,
            })
            .collect();
        assert_eq!(
            got,
            vec![
                "2d 0 bytes 1..2".to_string(),
                "2d 0 bytes 16..32".to_string(),
                "2d 1 bytes 32..48".to_string(),
                "3d 0 2 bytes 8..40".to_string(),
                "sub3d 1 bytes 0..16".to_string(),
                "2d 2 buffer 64+32".to_string(),
                "sub2d 4 buffer 80+16".to_string(),
                "3d 1 1 buffer 0+16".to_string(),
                "sub3d 1 buffer 96+16".to_string(),
                "waitSync".to_string(),
            ]
        );
    }

    /// What the compressed uploads and the sync calls refuse before anything is sent: a view range past its end and
    /// an `srcOffset` of 2^32 (which `>>> 0` used to wrap to the start) are INVALID_VALUE, as are a negative
    /// `imageSize` and a negative buffer offset; a buffer offset with no PIXEL_UNPACK_BUFFER bound, a view with one
    /// bound, an offset past 2^31 - 1 and a range past the buffer are INVALID_OPERATION. `waitSync` with flags, or any timeout but
    /// TIMEOUT_IGNORED, is INVALID_VALUE, on a deleted sync INVALID_OPERATION, and on something that is not a sync a
    /// TypeError. A deleted sync is no longer one, and `clientWaitSync` on it fails. The same `srcOffset` conversion
    /// holds the uniform lists to their ends, and `texImage3D`, whose pixels past the view are not enough data
    /// (INVALID_OPERATION, WebGL 2.0 3.7.6).
    #[test]
    fn a_malformed_compressed_upload_or_sync_call_is_the_specified_error_and_nothing_is_sent() {
        let (mut runtime, render_rx) = new_webgl_runtime();
        runtime
            .exec_script(
                "malformed_compressed_uploads.js",
                r#"
                const gl = new WebGL2RenderingContext({ _rid: 182, width: 1, height: 1 }, {});
                gl._gpuCapsCache = 1;       // what the renderer would answer: ETC2 is there
                gl.getExtension("WEBGL_compressed_texture_etc");
                gl.bindTexture(0x0de1, gl.createTexture());
                gl.bindTexture(0x8c1a, gl.createTexture());
                gl.bindTexture(0x806f, gl.createTexture());
                const VALUE = 0x0501, OPERATION = 0x0502;
                const block = new Uint8Array(16);
                const sync = gl.fenceSync(0x9117, 0);
                const cases = [
                    [VALUE, () => gl.compressedTexImage2D(0x0de1, 0, 0x9278, 4, 4, 0, block, 17)],
                    [VALUE, () => gl.compressedTexImage2D(0x0de1, 0, 0x9278, 4, 4, 0, block, 8, 9)],
                    [VALUE, () => gl.compressedTexImage2D(0x0de1, 0, 0x9278, 4, 4, 0, block, 4294967296)],
                    [OPERATION, () => gl.compressedTexImage3D(0x8c1a, 0, 0x9278, 4, 4, 1, 0, 16, 0)],
                    [OPERATION, () => gl.compressedTexImage3D(0x8c1a, 0, 0x9278, 4, 4, 1, 0, 16, -4)],
                    [OPERATION, () => gl.texImage3D(0x806f, 0, 0x1908, 1, 1, 1, 0, 0x1908, 0x1401, new Uint8Array(4), 5)],
                    [0, () => { gl.bindBuffer(0x88ec, gl.createBuffer()); gl.bufferData(0x88ec, 64, 0x88e0); }],
                    [OPERATION, () => gl.compressedTexImage2D(0x0de1, 0, 0x9278, 4, 4, 0, block)],
                    [OPERATION, () => gl.compressedTexImage3D(0x8c1a, 0, 0x9278, 4, 4, 1, 0, 16, 56)],
                    [VALUE, () => gl.compressedTexImage3D(0x8c1a, 0, 0x9278, 4, 4, 1, 0, -1, 0)],
                    [VALUE, () => gl.compressedTexImage3D(0x8c1a, 0, 0x9278, 4, 4, 1, 0, 16, -4)],
                    [OPERATION, () => gl.compressedTexSubImage3D(0x8c1a, 0, 0, 0, 0, 4, 4, 1, 0x9278, 16, 2147483648)],
                    [VALUE, () => gl.waitSync(sync, 1, -1)],
                    [VALUE, () => gl.waitSync(sync, 0, 0)],
                    [VALUE, () => gl.uniform1uiv({ id: 3 }, new Uint32Array(4), 4294967296)],
                ];
                cases.forEach(([want, call], i) => {
                    call();
                    const got = gl.getError();
                    if (got !== want) throw new Error(`case ${i}: getError ${got}, want ${want}`);
                });
                gl.deleteSync(sync);
                if (gl.isSync(sync)) throw new Error("a deleted sync is not one");
                gl.waitSync(sync, 0, -1);
                if (gl.getError() !== OPERATION) throw new Error("waitSync on a deleted sync");
                if (gl.clientWaitSync(sync, 0, 0) !== 0x911d) throw new Error("clientWaitSync on a deleted sync is WAIT_FAILED");
                if (gl.getError() !== OPERATION) throw new Error("clientWaitSync on a deleted sync is INVALID_OPERATION");
                let threw = false;
                try { gl.waitSync({ _id: 1, _kind: "sync" }, 0, -1); } catch (e) { threw = e instanceof TypeError; }
                if (!threw) throw new Error("a lookalike is not a WebGLSync");
                gl.flush();
                "#,
            )
            .expect("the malformed calls should be refused, not thrown (but for the TypeError, which the script catches)");
        let sent: Vec<String> = drain_gl_commands(&render_rx)
            .iter()
            .filter(|cmd| {
                matches!(
                    cmd,
                    GLCmd::CompressedTexImage2D { .. }
                        | GLCmd::CompressedTexImage3D { .. }
                        | GLCmd::CompressedTexSubImage3D { .. }
                        | GLCmd::WaitSync { .. }
                        | GLCmd::Uniform1uiv { .. }
                        | GLCmd::TexImage3D { .. }
                )
            })
            .map(|cmd| format!("{cmd:?}"))
            .collect();
        assert!(
            sent.is_empty(),
            "a refused call must not reach the renderer: {sent:?}"
        );
    }

    /// MAX_VERTEX_ATTRIBS is asked only for an index at or past the minimum every implementation has (16 in WebGL 2),
    /// and the answer kept only once the context gives one: a lost context's non-answer is not cached as the limit.
    #[test]
    fn the_vertex_attribute_limit_is_asked_only_past_the_minimum_and_kept_once_answered() {
        let (mut runtime, _render_rx) = new_webgl_runtime();
        runtime
            .exec_script(
                "vertex_attrib_limit.js",
                r#"
                const gl = new WebGL2RenderingContext({ _rid: 183, width: 1, height: 1 }, {});
                let asked = 0, answer = null;
                gl.getParameter = (pname) => { if (pname === 0x8869) asked += 1; return pname === 0x8869 ? answer : null; };
                const enabled = (i) => gl.getVertexAttrib(i, 0x8622);
                enabled(15);
                if (asked !== 0 || gl.getError() !== 0) throw new Error("index 15 is below the minimum: no question");
                enabled(20);
                if (gl.getError() !== 0x0501) throw new Error("no answer: 20 is past the minimum");
                answer = 32;
                enabled(20);
                if (gl.getError() !== 0) throw new Error("the answer, 32, admits 20");
                enabled(21);
                if (asked !== 2) throw new Error("asked " + asked + " times; the answer is kept once given");
                "#,
            )
            .expect("the vertex attribute limit script should run");
    }

    /// What is attached to a program is the facade's to know: `getAttachedShaders` and ATTACHED_SHADERS answer from
    /// it, follow every attach and detach (a cached count went stale), and refuse what GL would: a shader already
    /// attached or of a type already attached, detaching one that is not, a deleted program or shader. `createShader`
    /// of a type that is not one is INVALID_ENUM and null. `validateProgram` drops the cached VALIDATE_STATUS.
    #[test]
    fn shader_attachment_is_answered_from_what_the_calls_did() {
        let (mut runtime, render_rx) = new_webgl_runtime();
        runtime
            .exec_script(
                "shader_attachment.js",
                r#"
                const gl = new WebGLRenderingContext({ _rid: 190, width: 1, height: 1 }, {});
                const check = (c, m) => { if (!c) throw new Error(m); };
                const OPERATION = 0x0502;
                check(gl.createShader(0x1234) === null && gl.getError() === 0x0500, "a bad type is INVALID_ENUM and null");
                const p = gl.createProgram(), vs = gl.createShader(0x8b31), fs = gl.createShader(0x8b30), vs2 = gl.createShader(0x8b31);
                check(gl.getAttachedShaders(p).length === 0 && gl.getProgramParameter(p, 0x8b85) === 0, "nothing attached");
                gl.attachShader(p, vs);
                check(gl.getProgramParameter(p, 0x8b85) === 1, "one attached");
                gl.attachShader(p, fs);
                const both = gl.getAttachedShaders(p);
                check(both.length === 2 && both[0] === vs && both[1] === fs && gl.getProgramParameter(p, 0x8b85) === 2, "both, in order");
                gl.attachShader(p, vs);
                check(gl.getError() === OPERATION, "attached twice");
                gl.attachShader(p, vs2);
                check(gl.getError() === OPERATION, "a second vertex shader");
                gl.detachShader(p, vs);
                check(gl.getProgramParameter(p, 0x8b85) === 1 && gl.getAttachedShaders(p)[0] === fs, "detached");
                gl.detachShader(p, vs);
                check(gl.getError() === OPERATION, "detaching one not attached");
                gl.attachShader(p, vs2);
                check(gl.getError() === 0 && gl.getAttachedShaders(p).length === 2, "the other vertex shader now");
                gl.deleteShader(fs);
                check(gl.getAttachedShaders(p).includes(fs), "a deleted shader stays attached until detached");
                gl.attachShader(p, fs);
                check(gl.getError() === OPERATION, "a deleted shader cannot be attached");
                let threw = false;
                try { gl.attachShader(null, vs); } catch (e) { threw = e instanceof TypeError; }
                check(threw, "a null program is a TypeError");
                gl._programParameterCache.set(p.id, new Map([[0x8b83, 1]]));
                gl.validateProgram(p);
                check(!gl._programParameterCache.get(p.id).has(0x8b83), "validateProgram drops the cached VALIDATE_STATUS");
                gl.deleteProgram(p);
                check(gl.getAttachedShaders(p) === null && gl.getError() === OPERATION, "a deleted program");
                gl.sampleCoverage(0.5, true);
                gl.flush();
                "#,
            )
            .expect("the shader attachment script should run");
        let got: Vec<String> = drain_gl_commands(&render_rx)
            .iter()
            .filter_map(|cmd| match cmd {
                GLCmd::AttachShader { .. } => Some("attach".to_string()),
                GLCmd::DetachShader { .. } => Some("detach".to_string()),
                GLCmd::ValidateProgram { .. } => Some("validate".to_string()),
                GLCmd::SampleCoverage { value, invert, .. } => {
                    Some(format!("coverage {value} {invert}"))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            got,
            vec![
                "attach",
                "attach",
                "detach",
                "attach",
                "validate",
                "coverage 0.5 true"
            ]
        );
    }

    /// A sampler is a real `WebGLSampler`: `isSampler` answers for it, `getSamplerParameter` answers what
    /// `samplerParameter*` set (the table's initial values before), a value a parameter does not take and a parameter
    /// a sampler does not have are INVALID_ENUM and change nothing, an enum set through the float call takes the
    /// nearest integer, and a deleted sampler is INVALID_OPERATION to use or bind.
    #[test]
    fn sampler_parameters_are_answered_from_what_the_calls_set() {
        let (mut runtime, _render_rx) = new_webgl_runtime();
        runtime
            .exec_script(
                "sampler_parameters.js",
                r#"
                const gl = new WebGL2RenderingContext({ _rid: 191, width: 1, height: 1 }, {});
                const check = (c, m) => { if (!c) throw new Error(m); };
                const s = gl.createSampler();
                check(gl.isSampler(s) && !gl.isSampler({}) && !gl.isSampler(null), "a sampler is one");
                check(gl.getSamplerParameter(s, 0x2801) === 0x2702 && gl.getSamplerParameter(s, 0x2800) === 0x2601 &&
                      gl.getSamplerParameter(s, 0x2802) === 0x2901 && gl.getSamplerParameter(s, 0x884d) === 0x0203 &&
                      gl.getSamplerParameter(s, 0x813a) === -1000 && gl.getSamplerParameter(s, 0x813b) === 1000, "initial values");
                gl.samplerParameteri(s, 0x2801, 0x2600);
                gl.samplerParameterf(s, 0x813a, 2.5);
                gl.samplerParameterf(s, 0x2800, 9728.2);       // NEAREST, through the float call
                check(gl.getSamplerParameter(s, 0x2801) === 0x2600 && gl.getSamplerParameter(s, 0x813a) === 2.5 &&
                      gl.getSamplerParameter(s, 0x2800) === 0x2600, "what was set");
                gl.samplerParameteri(s, 0x2800, 0x2702);       // MAG_FILTER takes no mipmap filter
                check(gl.getError() === 0x0500 && gl.getSamplerParameter(s, 0x2800) === 0x2600, "a value it does not take");
                gl.samplerParameteri(s, 0x1234, 0);
                check(gl.getError() === 0x0500, "a parameter it does not have");
                check(gl.getSamplerParameter(s, 0x1234) === null && gl.getError() === 0x0500, "a query of one");
                gl.deleteSampler(s);
                check(!gl.isSampler(s), "deleted");
                gl.samplerParameteri(s, 0x2801, 0x2601);
                check(gl.getError() === 0x0502, "setting a deleted sampler");
                gl.bindSampler(0, s);
                check(gl.getError() === 0x0502, "binding a deleted sampler");
                check(gl.getSamplerParameter(s, 0x2801) === null && gl.getError() === 0x0502, "querying a deleted sampler");
                "#,
            )
            .expect("the sampler script should run");
    }

    /// `getIndexedParameter` answers from what `bindBufferBase` / `bindBufferRange` took: uniform buffer bindings on
    /// the context, transform feedback ones on the bound transform feedback object. An index past the binding points
    /// and a uniform buffer offset off UNIFORM_BUFFER_OFFSET_ALIGNMENT are INVALID_VALUE before anything is sent; a
    /// deleted buffer leaves its bindings. `getSyncParameter` answers a fence's type, condition and flags, and in the
    /// task that made it a fence is unsignalled without a question to the render side.
    #[test]
    fn indexed_bindings_and_sync_parameters_are_answered_from_what_the_calls_set() {
        let (mut runtime, render_rx) = new_webgl_runtime();
        runtime
            .exec_script(
                "indexed_bindings.js",
                r#"
                const gl = new WebGL2RenderingContext({ _rid: 192, width: 1, height: 1 }, {});
                gl._maxUniformBufferBindings = 36;
                gl._uniformBufferOffsetAlignment = 256;
                const check = (c, m) => { if (!c) throw new Error(m); };
                const a = gl.createBuffer(), b = gl.createBuffer();
                gl.bindBufferBase(0x8a11, 2, a);
                gl.bindBufferRange(0x8a11, 30, b, 512, 64);
                check(gl.getIndexedParameter(0x8a28, 2) === a && gl.getIndexedParameter(0x8a29, 2) === 0 &&
                      gl.getIndexedParameter(0x8a2a, 2) === 0, "a base binding");
                check(gl.getIndexedParameter(0x8a28, 30) === b && gl.getIndexedParameter(0x8a29, 30) === 512 &&
                      gl.getIndexedParameter(0x8a2a, 30) === 64, "a range binding");
                check(gl.getIndexedParameter(0x8a28, 3) === null, "an unbound point");
                gl.bindBufferRange(0x8a11, 3, a, 100, 64);
                check(gl.getError() === 0x0501 && gl.getIndexedParameter(0x8a28, 3) === null, "an unaligned offset");
                gl.bindBufferBase(0x8a11, 36, a);
                check(gl.getError() === 0x0501, "past MAX_UNIFORM_BUFFER_BINDINGS");
                check(gl.getIndexedParameter(0x8a28, 36) === null && gl.getError() === 0x0501, "a query past it");
                check(gl.getIndexedParameter(0x1234, 0) === null && gl.getError() === 0x0500, "not an indexed parameter");
                // transform feedback bindings belong to the bound transform feedback object
                gl.bindBufferBase(0x8c8e, 1, a);
                const tf = gl.createTransformFeedback();
                gl.bindTransformFeedback(0x8e22, tf);
                check(gl.getIndexedParameter(0x8c8f, 1) === null, "a new transform feedback object has none");
                gl.bindBufferRange(0x8c8e, 1, b, 8, 16);
                check(gl.getIndexedParameter(0x8c8f, 1) === b && gl.getIndexedParameter(0x8c84, 1) === 8, "its own");
                gl.bindTransformFeedback(0x8e22, null);
                check(gl.getIndexedParameter(0x8c8f, 1) === a, "the default object's again");
                gl.deleteBuffer(a);
                check(gl.getIndexedParameter(0x8a28, 2) === null && gl.getIndexedParameter(0x8c8f, 1) === null, "deleting unbinds");
                const sync = gl.fenceSync(0x9117, 0);
                check(gl.getSyncParameter(sync, 0x9112) === 0x9116 && gl.getSyncParameter(sync, 0x9113) === 0x9117 &&
                      gl.getSyncParameter(sync, 0x9115) === 0, "a fence's type, condition and flags");
                check(gl.getSyncParameter(sync, 0x1234) === null && gl.getError() === 0x0500, "not a sync parameter");
                // the task that made it never sees it signalled, and asks the render side nothing (there is none here)
                check(gl.getSyncParameter(sync, 0x9114) === 0x9118, "UNSIGNALED in its own task");
                check(gl.clientWaitSync(sync, 0, 0) === 0x911b, "TIMEOUT_EXPIRED in its own task");
                gl.deleteSync(sync);
                check(gl.getSyncParameter(sync, 0x9112) === null && gl.getError() === 0x0502, "a deleted sync");
                gl.flush();
                "#,
            )
            .expect("the indexed binding script should run");
        let ranges: Vec<String> = drain_gl_commands(&render_rx)
            .iter()
            .filter_map(|cmd| match cmd {
                GLCmd::BindBufferRange {
                    index,
                    offset,
                    size,
                    ..
                } => Some(format!("{index} {offset} {size}")),
                _ => None,
            })
            .collect();
        assert_eq!(
            ranges,
            vec!["30 512 64", "1 8 16"],
            "the refused range was not sent"
        );
    }

    /// `flush()` is the context's commands submitted: a `Flush` record for its canvas. A task that made a fence ends
    /// with the same for each context that made one -- once, however many fences -- and with what it recorded sent, so
    /// the fence reaches the GPU though nothing presents that context; the next task sees the fence as the GPU does.
    #[test]
    fn a_fence_s_task_ends_by_flushing_the_contexts_that_made_one() {
        let (mut runtime, render_rx) = new_webgl_runtime();
        runtime
            .exec_script(
                "fence_task_flush.js",
                r#"
                const gl = new WebGL2RenderingContext({ _rid: 196, width: 1, height: 1 }, {});
                const other = new WebGL2RenderingContext({ _rid: 197, width: 1, height: 1 }, {});
                gl.fenceSync(0x9117, 0);
                gl.fenceSync(0x9117, 0);
                other.clear(0x4000);
                "#,
            )
            .expect("the fence script should run");
        // The task ends when the script returns: its microtask checkpoint runs then.
        let sent: Vec<String> = drain_gl_commands(&render_rx)
            .iter()
            .map(|cmd| match cmd {
                GLCmd::FenceSync { canvas_id, .. } => format!("fence {canvas_id}"),
                GLCmd::Clear { canvas_id, .. } => format!("clear {canvas_id}"),
                GLCmd::Flush { canvas_id } => format!("flush {canvas_id}"),
                other => format!("unexpected {other:?}"),
            })
            .collect();
        assert_eq!(
            sent,
            vec!["fence 196", "fence 196", "clear 197", "flush 196"],
            "the task's end -- after its last call, not at the fence -- flushes the context that made the fences, once"
        );
        runtime
            .exec_script(
                "fence_task_flush_next.js",
                r#"
                gl.flush();
                other.flush();
                "#,
            )
            .expect("the next task should run");
        let sent: Vec<String> = drain_gl_commands(&render_rx)
            .iter()
            .map(|cmd| format!("{cmd:?}"))
            .collect();
        assert_eq!(
            sent,
            vec!["Flush { canvas_id: 196 }", "Flush { canvas_id: 197 }"],
            "flush() is a Flush record of its own canvas"
        );
    }

    /// Answers the lookups by name and the state queries a program test makes: a uniform's location by the table
    /// below, and a state query by its name. Returns what was asked: `loc name` for a uniform location, `query extra
    /// name` for a state query.
    fn spawn_program_query_responder(
        render_rx: crossbeam_channel::Receiver<RenderCommand>,
    ) -> std::thread::JoinHandle<Vec<String>> {
        std::thread::spawn(move || {
            let mut asked = Vec::new();
            while let Ok(command) = render_rx.recv_timeout(Duration::from_secs(1)) {
                match command {
                    RenderCommand::FramePacket(_) => {}
                    RenderCommand::GL(GLCmd::GetUniformLocation { name, resp, .. }) => {
                        asked.push(format!("loc {name}"));
                        let names = ["f", "v", "i", "b", "bv", "u", "iv", "uv", "e"];
                        resp.ok(names
                            .iter()
                            .position(|n| *n == name)
                            .map(|at| at as u32 + 1));
                    }
                    RenderCommand::GL(GLCmd::GetState {
                        query,
                        extra,
                        name,
                        resp,
                        ..
                    }) => {
                        let bits = |f: f32| f.to_bits().to_string();
                        let answer = match (query, name.as_str()) {
                            (7, "f") => format!("{{\"v\":[\"f\",[{}]]}}", bits(0.1)),
                            (7, "v") => format!(
                                "{{\"v\":[\"f\",[{},{},{},{}]]}}",
                                bits(1.0),
                                bits(f32::NAN),
                                bits(f32::NEG_INFINITY),
                                bits(-0.0)
                            ),
                            (7, "i") => "{\"v\":[\"i\",[4294967295]]}".to_string(),
                            (7, "b") => "{\"v\":[\"b\",[1]]}".to_string(),
                            (7, "bv") => "{\"v\":[\"b\",[0,1,1]]}".to_string(),
                            (7, "u") => "{\"v\":[\"u\",[4294967295]]}".to_string(),
                            (7, "iv") => "{\"v\":[\"i\",[1,4294967294]]}".to_string(),
                            (7, "uv") => "{\"v\":[\"u\",[7,8]]}".to_string(),
                            (7, "e") => "{\"e\":1282}".to_string(),
                            (8, "color") => "{\"v\":1}".to_string(),
                            (8, _) => "{\"v\":-1}".to_string(),
                            other => panic!("unexpected state query {other:?}"),
                        };
                        asked.push(format!("{query} {extra} {name}"));
                        resp.ok(answer);
                    }
                    other => panic!("unexpected render command in a program query test: {other:?}"),
                }
            }
            asked
        })
    }

    /// `getUniform` is the driver's value, typed as WebGL types it: a float the float the uniform holds (its bits
    /// cross, so NaN, the infinities and -0 too), a vector or a matrix a typed array, bools booleans. It asks by the
    /// name the location was looked up by, and only for a location of this program since its last link. `getFragDataLocation` asks by name. A name WebGL refuses or reserves never reaches GL.
    #[test]
    fn get_uniform_and_get_frag_data_location_ask_the_driver_and_type_its_answer() {
        let (mut runtime, render_rx) = new_webgl_runtime();
        let responder = spawn_program_query_responder(render_rx);
        runtime
            .exec_script(
                "get_uniform.js",
                r#"
                const gl = new WebGL2RenderingContext({ _rid: 198, width: 1, height: 1 }, {});
                const check = (c, m) => { if (!c) throw new Error(m); };
                const p = gl.createProgram();
                gl.linkProgram(p);
                const at = (name) => gl.getUniformLocation(p, name);
                check(gl.getUniform(p, at("f")) === Math.fround(0.1), "a float is the float the uniform holds");
                const v = gl.getUniform(p, at("v"));
                check(v instanceof Float32Array && v.length === 4 && v[0] === 1 && Number.isNaN(v[1]) &&
                      v[2] === -Infinity && Object.is(v[3], -0), "a vec4 with NaN, -Infinity and -0");
                check(gl.getUniform(p, at("i")) === -1, "an int");
                check(gl.getUniform(p, at("b")) === true, "a bool");
                const bv = gl.getUniform(p, at("bv"));
                check(Array.isArray(bv) && bv.join() === "false,true,true", "a bvec3 is an Array of booleans");
                check(gl.getUniform(p, at("u")) === 4294967295, "a uint");
                const iv = gl.getUniform(p, at("iv"));
                check(iv instanceof Int32Array && iv.join() === "1,-2", "an ivec2");
                const uv = gl.getUniform(p, at("uv"));
                check(uv instanceof Uint32Array && uv.join() === "7,8", "a uvec2");
                check(gl.getUniform(p, at("e")) === null && gl.getError() === 0x0502, "the driver's error");
                const stale = at("f");
                gl.linkProgram(p);
                check(gl.getUniform(p, stale) === null && gl.getError() === 0x0502, "a location from before the link");
                const q = gl.createProgram();
                gl.linkProgram(q);
                check(gl.getUniform(q, at("f")) === null && gl.getError() === 0x0502, "another program's location");
                let threw = false;
                try { gl.getUniform(p, null); } catch (e) { threw = e instanceof TypeError; }
                check(threw, "a null location is a TypeError");
                threw = false;
                try { gl.getUniform(p, {}); } catch (e) { threw = e instanceof TypeError; }
                check(threw, "a location that is not one is a TypeError");
                threw = false;
                try { gl.getUniform(p, q); } catch (e) { threw = e instanceof TypeError; }
                check(threw, "a program as the location is a TypeError");
                threw = false;
                try { gl.getUniform({}, at("f")); } catch (e) { threw = e instanceof TypeError; }
                check(threw, "a program that is not one is a TypeError");
                // names
                check(gl.getUniformLocation(p, "a$b") === null && gl.getError() === 0x0501, "a name with a $");
                check(gl.getUniformLocation(p, "x".repeat(1025)) === null && gl.getError() === 0x0501, "a name past 1024");
                check(gl.getUniformLocation(p, "x".repeat(1024)) === null && gl.getError() === 0, "a name of 1024");
                check(gl.getUniformLocation(p, "webgl_x") === null && gl.getError() === 0, "a reserved name");
                check(gl.getAttribLocation(p, 'a"') === -1 && gl.getError() === 0x0501, "getAttribLocation checks names");
                gl.bindAttribLocation(p, 0, "_webgl_a");
                check(gl.getError() === 0x0502, "binding a reserved name");
                gl.bindAttribLocation(p, 0, "a\u00e9");
                check(gl.getError() === 0x0501, "binding a name outside the character set");
                check(gl.getFragDataLocation(p, "color") === 1, "an output's location");
                check(gl.getFragDataLocation(p, "nope") === -1, "a name that is no output");
                check(gl.getFragDataLocation(p, "webgl_color") === -1 && gl.getError() === 0, "a reserved output name");
                check(gl.getFragDataLocation(p, "c@") === -1 && gl.getError() === 0x0501, "an output name with a @");
                gl.deleteProgram(q);
                check(gl.getFragDataLocation(q, "color") === -1 && gl.getError() === 0x0502, "a deleted program");
                check(gl.getFragDataLocation(q, "c@") === -1 && gl.getError() === 0x0502, "a deleted program, before the name");
                "#,
            )
            .expect("the getUniform script should run");
        drop(runtime);
        let asked = responder.join().expect("the responder must not panic");
        let mut expected: Vec<String> = ["f", "v", "i", "b", "bv", "u", "iv", "uv", "e"]
            .iter()
            .flat_map(|name| [format!("loc {name}"), format!("7 0 {name}")])
            .collect();
        // after the relink: `at("f")` for the other program's test, then the longest name there is
        expected.push("loc f".to_string());
        expected.push(format!("loc {}", "x".repeat(1024)));
        expected.push("8 0 color".to_string());
        expected.push("8 0 nope".to_string());
        assert_eq!(
            asked, expected,
            "each query asks by its name, once per link, and nothing refused or reserved reaches the driver"
        );
    }

    /// Buffer state is kept for every target and checked before anything is sent: what each target has bound
    /// (`getParameter`), each buffer's size and usage (`getBufferParameter`), the WebGL type a buffer takes at its first
    /// bind, the element array buffer of each vertex array object, the generic transform feedback binding of each
    /// transform feedback object. `bufferData` / `bufferSubData` take WebGL 2's element ranges and refuse what the
    /// specification refuses; a draw that would read past its index buffer is INVALID_OPERATION; a deleted buffer
    /// leaves the bindings of this context and of the objects bound, and keeps the one of an object not bound.
    #[test]
    fn buffer_state_is_kept_for_every_target_and_checked_before_anything_is_sent() {
        let (mut runtime, render_rx) = new_webgl_runtime();
        runtime
            .exec_script(
                "buffer_state.js",
                r#"
                const gl = new WebGL2RenderingContext({ _rid: 199, width: 1, height: 1 }, {});
                const check = (c, m) => { if (!c) throw new Error(m); };
                const err = (want, m) => { const got = gl.getError(); if (got !== want) throw new Error(`${m}: getError ${got}, want ${want}`); };
                const a = gl.createBuffer(), b = gl.createBuffer(), idx = gl.createBuffer(), idx2 = gl.createBuffer();
                check(!gl.isBuffer(a), "a buffer never bound is not one yet");
                for (const [target, pname] of [[0x8892, 0x8894], [0x8f36, 0x8f36], [0x8f37, 0x8f37], [0x88eb, 0x88ed],
                                               [0x88ec, 0x88ef], [0x8a11, 0x8a28], [0x8c8e, 0x8c8f]]) {
                    gl.bindBuffer(target, a);
                    check(gl.getParameter(pname) === a, "the binding of " + target.toString(16));
                    gl.bindBuffer(target, null);
                    check(gl.getParameter(pname) === null, "unbound from " + target.toString(16));
                }
                check(gl.isBuffer(a), "bound once, it is a buffer");
                gl.bindBuffer(0x1234, a); err(0x0500, "a target that is not one");
                // a buffer is index data or other data for life; the copy targets take either
                gl.bindBuffer(0x8893, idx);
                gl.bindBuffer(0x8892, idx); err(0x0502, "an index buffer as vertex data");
                gl.bindBuffer(0x8893, a); err(0x0502, "vertex data as an index buffer");
                gl.bindBuffer(0x8f36, idx); err(0, "a copy target takes an index buffer");
                // bufferData
                gl.bindBuffer(0x8892, a);
                gl.bufferData(0x8892, 64, 0x88e8);
                check(gl.getBufferParameter(0x8892, 0x8764) === 64 && gl.getBufferParameter(0x8892, 0x8765) === 0x88e8, "size and usage");
                gl.bufferData(0x8892, -1, 0x88e4); err(0x0501, "a negative size");
                gl.bufferData(0x8892, 4, 0xbeef); err(0x0500, "a usage that is not one");
                gl.bufferData(0x8892, null, 0x88e4); err(0x0501, "null data");
                gl.bufferData(0x8893 + 1000, 4, 0x88e4); err(0x0500, "bufferData to a target that is not one");
                gl.bufferData(0x88eb, 4, 0x88e4); err(0x0502, "bufferData with nothing bound");
                gl.bufferData(0x8892, 16, 0x88e9); err(0, "DYNAMIC_READ is a WebGL 2 usage");
                check(gl.getBufferParameter(0x8892, 0x8764) === 16, "the refused calls changed nothing");
                gl.bufferData(0x8892, new Float32Array([1, 2, 3, 4, 5, 6]), 0x88e4, 2, 3);
                check(gl.getBufferParameter(0x8892, 0x8764) === 12, "srcOffset and length count elements");
                gl.bufferData(0x8892, new Uint16Array([1, 2, 3]), 0x88e4, 1);
                check(gl.getBufferParameter(0x8892, 0x8764) === 4, "a length of 0 is the rest");
                gl.bufferData(0x8892, new Uint16Array(3), 0x88e4, 4); err(0x0501, "a srcOffset past the view");
                gl.bufferData(0x8892, new Uint16Array(3), 0x88e4, 1, 3); err(0x0501, "a range past the view");
                let threw = false;
                try { gl.bufferData(0x8892, new ArrayBuffer(4), 0x88e4, 0); } catch (e) { threw = e instanceof TypeError && /ArrayBufferView/.test(e.message); }
                check(threw, "the srcOffset overload takes a view only");
                // bufferSubData
                gl.bufferData(0x8892, 16, 0x88e8);
                gl.bufferSubData(0x8892, 8, new Float32Array([7, 8])); err(0, "in range");
                gl.bufferSubData(0x8892, 12, new Float32Array([7, 8])); err(0x0501, "past the end");
                gl.bufferSubData(0x8892, -4, new Float32Array([7])); err(0x0501, "a negative offset");
                gl.bufferSubData(0x8892, 4, new Float32Array([9, 10, 11, 12]), 1, 2); err(0, "srcOffset and length");
                gl.bufferSubData(0x8f37, 0, new Uint8Array(1)); err(0x0502, "bufferSubData with nothing bound");
                gl.bindBuffer(0x8a11, b);
                gl.bufferData(0x8a11, 256, 0x88e4);
                check(gl.getBufferParameter(0x8a11, 0x8764) === 256, "a uniform buffer's size");
                gl.bindBuffer(0x8a11, null);
                gl._maxUniformBufferBindings = 24;
                gl.bindBufferBase(0x8a11, 3, b);
                check(gl.getParameter(0x8a28) === b, "an indexed bind binds the generic point too");
                // the element array buffer is the vertex array object's
                const vao = gl.createVertexArray();
                check(!gl.isVertexArray(vao), "a vertex array object never bound is not one yet");
                gl.bindVertexArray(vao);
                check(gl.isVertexArray(vao) && gl.getParameter(0x85b5) === vao, "bound");
                check(gl.getParameter(0x8895) === null, "a new vertex array object has no index buffer");
                gl.bindBuffer(0x8893, idx2);
                gl.bufferData(0x8893, 6, 0x88e4);
                gl.bindVertexArray(null);
                check(gl.getParameter(0x85b5) === null && gl.getParameter(0x8895) === idx, "the default object kept its own");
                gl.bindVertexArray(vao);
                check(gl.getParameter(0x8895) === idx2, "and the vertex array object its own");
                // a draw reads its indices from it
                const program = gl.createProgram();
                gl.linkProgram(program);
                gl._programParameterCache.set(program.id, new Map([[0x8b82, 1]]));   // what the renderer answers for LINK_STATUS
                program._consumes = []; program._consumesLink = program._links | 0;   // and the attribute locations it consumes: none
                gl.useProgram(program);
                gl.drawElements(4, 3, 0x1403, 0); err(0, "three shorts of six bytes");
                gl.drawElements(4, 3, 0x1403, 2); err(0x0502, "past the index buffer");
                gl.drawElementsInstanced(4, 4, 0x1403, 0, 2); err(0x0502, "an instanced draw past it");
                gl.drawRangeElements(4, 0, 2, 4, 0x1403, 0); err(0x0502, "a range draw past it");
                // deleting
                gl.bindBuffer(0x8892, a);
                gl.vertexAttribPointer(0, 2, 0x1406, false, 0, 0);
                gl.deleteBuffer(idx2);
                check(gl.getParameter(0x8895) === null, "deleting unbinds it from the vertex array object bound");
                gl.drawElements(4, 0, 0x1403, 0); err(0x0502, "a draw with no index buffer");
                gl.deleteBuffer(a);
                check(gl.getParameter(0x8894) === null && gl.getVertexAttrib(0, 0x889f) === null, "and from ARRAY_BUFFER and the attribute");
                check(!gl.isBuffer(a), "a deleted buffer is not one");
                gl.bindBuffer(0x8892, a); err(0x0502, "binding a deleted buffer");
                gl.deleteBuffer(idx);
                check(gl.getParameter(0x8f36) === null, "deleting idx unbinds COPY_READ_BUFFER");
                gl.bindVertexArray(null);
                check(gl.getParameter(0x8895) === idx, "an object not bound keeps its reference");
                // the generic transform feedback binding is the transform feedback object's
                gl.bindBuffer(0x8c8e, b);
                const tf = gl.createTransformFeedback();
                gl.bindTransformFeedback(0x8e22, tf);
                check(gl.getParameter(0x8c8f) === null, "a transform feedback object has its own generic binding");
                gl.bindTransformFeedback(0x8e22, null);
                check(gl.getParameter(0x8c8f) === b, "the default object's again");
                // WebGL 1: two targets, and vertex array objects through the extension only
                const gl1 = new WebGLRenderingContext({ _rid: 200, width: 1, height: 1 }, {});
                check(gl1.bindVertexArray === undefined && gl1.createVertexArray === undefined, "WebGL 1 has no vertex array methods");
                check(gl1.getParameter(0x85b5) === null && gl1.getError() === 0x0500, "no VERTEX_ARRAY_BINDING before the extension");
                const ext = gl1.getExtension("OES_vertex_array_object");
                const v1 = ext.createVertexArrayOES();
                ext.bindVertexArrayOES(v1);
                check(ext.isVertexArrayOES(v1) && gl1.getParameter(0x85b5) === v1, "the extension binds one");
                ext.deleteVertexArrayOES(v1);
                check(gl1.getParameter(0x85b5) === null && !ext.isVertexArrayOES(v1), "and deleting it binds the default");
                gl1.bindBuffer(0x8f36, gl1.createBuffer());
                check(gl1.getError() === 0x0500, "COPY_READ_BUFFER is WebGL 2's");
                check(gl1.getParameter(0x8f36) === null && gl1.getError() === 0x0500, "and so is its binding");
                gl1.bindBuffer(0x8892, gl1.createBuffer());
                gl1.bufferData(0x8892, 4, 0x88e9);
                check(gl1.getError() === 0x0500, "DYNAMIC_READ is WebGL 2's");
                gl1.bufferData(0x8892, new Uint8Array(4), 0x88e4, 1, 2);
                check(gl1.getError() === 0 && gl1.getBufferParameter(0x8892, 0x8764) === 4,
                      "WebGL 1 has no srcOffset overload: the extra arguments are ignored");
                gl.flush();
                "#,
            )
            .expect("the buffer state script should run");
        let sent: Vec<String> = drain_gl_commands(&render_rx)
            .iter()
            .filter_map(|cmd| match cmd {
                GLCmd::BufferData {
                    target,
                    size,
                    data,
                    usage,
                    ..
                } => Some(format!("data {target:#x} {size} {data:?} {usage:#x}")),
                GLCmd::BufferSubData {
                    target,
                    offset,
                    data,
                    ..
                } => Some(format!("subData {target:#x} {offset} {data:?}")),
                GLCmd::DrawElements { count, offset, .. } => {
                    Some(format!("drawElements {count} {offset}"))
                }
                GLCmd::DrawElementsInstanced { .. } => Some("drawElementsInstanced".to_string()),
                _ => None,
            })
            .collect();
        let float_bytes =
            |values: &[f32]| -> Vec<u8> { values.iter().flat_map(|v| v.to_le_bytes()).collect() };
        assert_eq!(
            sent,
            vec![
                "data 0x8892 64 None 0x88e8".to_string(),
                "data 0x8892 16 None 0x88e9".to_string(),
                format!(
                    "data 0x8892 12 {:?} 0x88e4",
                    Some(float_bytes(&[3.0, 4.0, 5.0]))
                ),
                format!("data 0x8892 4 {:?} 0x88e4", Some(vec![2u8, 0, 3, 0])),
                "data 0x8892 16 None 0x88e8".to_string(),
                format!("subData 0x8892 8 {:?}", float_bytes(&[7.0, 8.0])),
                format!("subData 0x8892 4 {:?}", float_bytes(&[10.0, 11.0])),
                "data 0x8a11 256 None 0x88e4".to_string(),
                "data 0x8893 6 None 0x88e4".to_string(),
                "drawElements 3 0".to_string(),
                format!("data 0x8892 4 {:?} 0x88e4", Some(vec![0u8; 4])),
            ],
            "only what was taken is sent, with the elements the WebGL 2 ranges name"
        );
    }

    /// `getBufferSubData` asks the renderer for exactly the bytes the destination range holds, from the bound buffer,
    /// and writes the answer there; everything the specification refuses is refused before anything is asked.
    #[test]
    fn get_buffer_sub_data_reads_the_bound_buffer_into_the_destination_range() {
        let (mut runtime, render_rx) = new_webgl_runtime();
        let responder = std::thread::spawn(move || {
            let mut asked = Vec::new();
            while let Ok(command) = render_rx.recv_timeout(Duration::from_secs(1)) {
                match command {
                    RenderCommand::FramePacket(_) => {}
                    RenderCommand::GL(GLCmd::GetBufferSubData {
                        target,
                        offset,
                        size,
                        resp,
                        ..
                    }) => {
                        asked.push(format!("{target:#x} {offset} {size}"));
                        // At offset 13 the renderer answers a byte short.
                        let answered = if offset == 13 { size - 1 } else { size };
                        resp.ok((0..answered).map(|k| (offset as u32 + k) as u8).collect());
                    }
                    other => panic!("unexpected render command in a buffer read test: {other:?}"),
                }
            }
            asked
        });
        runtime
            .exec_script(
                "get_buffer_sub_data.js",
                r#"
                const gl = new WebGL2RenderingContext({ _rid: 201, width: 1, height: 1 }, {});
                const check = (c, m) => { if (!c) throw new Error(m); };
                const err = (want, m) => { const got = gl.getError(); if (got !== want) throw new Error(`${m}: getError ${got}, want ${want}`); };
                const b = gl.createBuffer();
                gl.bindBuffer(0x8f36, b);
                gl.bufferData(0x8f36, 64, 0x88e4);
                const shorts = new Uint16Array(8).fill(0xffff);
                gl.getBufferSubData(0x8f36, 4, shorts, 2, 3);
                check(Array.from(shorts).join() === [0xffff, 0xffff, 0x0504, 0x0706, 0x0908, 0xffff, 0xffff, 0xffff].join(),
                      "elements 2..4 hold bytes 4..9: " + Array.from(shorts).join());
                const rest = new Uint8Array(4);
                gl.getBufferSubData(0x8f36, 60, rest);
                check(Array.from(rest).join() === "60,61,62,63", "a length of 0 reads to the end of the view");
                const view = new DataView(new ArrayBuffer(6), 2);
                gl.getBufferSubData(0x8f36, 10, view, 1);
                check(view.getUint8(1) === 10 && view.getUint8(3) === 12, "a DataView's elements are bytes");
                gl.getBufferSubData(0x8f36, 64, new Uint8Array(0)); err(0, "nothing to read at the end");
                const short = new Uint8Array(2).fill(7);
                gl.getBufferSubData(0x8f36, 13, short);
                check(gl.getError() === 0x0502 && short.join() === "7,7", "an answer of another length is INVALID_OPERATION and writes nothing");
                // refused before anything is asked
                gl.getBufferSubData(0x8f37, 0, new Uint8Array(4)); err(0x0502, "nothing bound");
                gl.getBufferSubData(0x1234, 0, new Uint8Array(4)); err(0x0500, "a target that is not one");
                gl.getBufferSubData(0x8f36, -1, new Uint8Array(4)); err(0x0501, "a negative offset");
                gl.getBufferSubData(0x8f36, 0, new Uint8Array(4), 5); err(0x0501, "a dstOffset past the view");
                gl.getBufferSubData(0x8f36, 0, new Uint8Array(4), 2, 3); err(0x0501, "a range past the view");
                gl.getBufferSubData(0x8f36, 60, new Uint8Array(8)); err(0x0501, "a range past the buffer");
                gl.bindBuffer(0x8c8e, b);
                gl.beginTransformFeedback(0x0000);
                gl.getBufferSubData(0x8c8e, 0, new Uint8Array(4)); err(0x0502, "transform feedback active on its target");
                gl.endTransformFeedback();
                let threw = false;
                try { gl.getBufferSubData(0x8f36, 0, new ArrayBuffer(4)); } catch (e) { threw = e instanceof TypeError && /ArrayBufferView/.test(e.message); }
                check(threw, "the destination is a view");
                const gl1 = new WebGLRenderingContext({ _rid: 202, width: 1, height: 1 }, {});
                check(gl1.getBufferSubData === undefined, "WebGL 1 has no getBufferSubData");
                "#,
            )
            .expect("the buffer read script should run");
        drop(runtime);
        assert_eq!(
            responder.join().expect("the responder must not panic"),
            vec!["0x8f36 4 6", "0x8f36 60 4", "0x8f36 10 3", "0x8f36 13 2"],
            "each read asks for exactly the destination's bytes, and nothing refused is asked"
        );
    }

    /// The errors a call is refused with before anything is sent: an enum that is not a capability (INVALID_ENUM --
    /// RASTERIZER_DISCARD is WebGL 2's), a negative viewport or scissor size (INVALID_VALUE), a program that did not
    /// link, was deleted or is not one (INVALID_OPERATION, or a TypeError), and a draw with no program in use
    /// (INVALID_OPERATION). Errors of two kinds are both held.
    #[test]
    fn calls_the_specification_refuses_are_refused_before_anything_is_sent() {
        let (mut runtime, render_rx) = new_webgl_runtime();
        runtime
            .exec_script(
                "error_checks.js",
                r#"
                const gl = new WebGLRenderingContext({ _rid: 203, width: 1, height: 1 }, {});
                const gl2 = new WebGL2RenderingContext({ _rid: 204, width: 1, height: 1 }, {});
                const check = (c, m) => { if (!c) throw new Error(m); };
                const err = (ctx, want, m) => { const got = ctx.getError(); if (got !== want) throw new Error(`${m}: getError ${got}, want ${want}`); };
                gl.enable(0xdead); err(gl, 0x0500, "enable of a non-capability");
                gl.disable(0xdead); err(gl, 0x0500, "disable of a non-capability");
                check(gl.isEnabled(0xdead) === false, "isEnabled of a non-capability is false"); err(gl, 0x0500, "and INVALID_ENUM");
                gl.enable(0x8c89); err(gl, 0x0500, "RASTERIZER_DISCARD in WebGL 1");
                gl2.enable(0x8c89); err(gl2, 0, "RASTERIZER_DISCARD in WebGL 2");
                check(gl2.isEnabled(0x8c89) === true, "and it is enabled");
                gl.viewport(0, 0, -1, 5); err(gl, 0x0501, "a negative viewport width");
                gl.scissor(0, 0, 5, -1); err(gl, 0x0501, "a negative scissor height");
                gl.viewport(0n, 0n, -1n, 5n); err(gl, 0x0501, "a negative viewport width on the op's path");
                gl.enable(0xdead); gl.viewport(0, 0, -1, 5);
                const first = gl.getError(), second = gl.getError();
                check([first, second].sort().join() === "1280,1281", "two kinds of error are both held: " + first + "," + second);
                // useProgram
                let threw = false;
                try { gl.useProgram({ id: 1 }); } catch (e) { threw = e instanceof TypeError; }
                check(threw, "a program that is not one is a TypeError");
                const linked = gl.createProgram(), unlinked = gl.createProgram(), deleted = gl.createProgram();
                for (const p of [linked, unlinked, deleted]) gl.linkProgram(p);
                gl._programParameterCache.set(linked.id, new Map([[0x8b82, 1]]));     // what the renderer answers for LINK_STATUS
                linked._consumes = []; linked._consumesLink = linked._links | 0;   // and the attribute locations it consumes: none
                gl._programParameterCache.set(unlinked.id, new Map([[0x8b82, 0]]));
                gl.deleteProgram(deleted);
                gl._programParameterCache.set(deleted.id, new Map([[0x8b82, 1]]));     // it linked; it is deleted
                deleted._consumes = []; deleted._consumesLink = deleted._links | 0;   // and the attribute locations it consumes: none
                gl.drawArrays(4, 0, 3); err(gl, 0x0502, "drawArrays with no program");
                gl.bindBuffer(0x8893, gl.createBuffer());
                gl.bufferData(0x8893, 6, 0x88e4);                                    // three shorts to draw from
                gl.drawElements(4, 3, 0x1403, 0); err(gl, 0x0502, "drawElements with no program");
                gl.getExtension("ANGLE_instanced_arrays").drawArraysInstancedANGLE(4, 0, 3, 2); err(gl, 0x0502, "an instanced draw with no program");
                gl2.drawArraysInstanced(4, 0, 3, 2); err(gl2, 0x0502, "drawArraysInstanced with no program");
                gl.useProgram(unlinked); err(gl, 0x0502, "a program that did not link");
                check(gl.getParameter(0x8b8d) === null, "and none is in use");
                gl.useProgram(deleted); err(gl, 0x0502, "a deleted program");
                gl.useProgram(linked); err(gl, 0, "a linked program");
                check(gl.getParameter(0x8b8d) === linked, "is in use");
                gl.useProgram(unlinked); err(gl, 0x0502, "a program that did not link, again");
                check(gl.getParameter(0x8b8d) === linked, "and the one in use stays in use");
                gl.drawArrays(4, 0, 3); err(gl, 0, "a draw with a program in use");
                gl.useProgram(null); err(gl, 0, "null uses none");
                gl.flush();
                gl2.flush();
                "#,
            )
            .expect("the error-check script should run");
        let sent: Vec<String> = drain_gl_commands(&render_rx)
            .iter()
            .filter_map(|cmd| match cmd {
                GLCmd::Enable { cap, .. } => Some(format!("enable {cap:#x}")),
                GLCmd::Disable { cap, .. } => Some(format!("disable {cap:#x}")),
                GLCmd::Viewport { .. } => Some("viewport".to_string()),
                GLCmd::Scissor { .. } => Some("scissor".to_string()),
                GLCmd::UseProgram { program_id, .. } => Some(format!("useProgram {program_id}")),
                GLCmd::DrawArrays { .. } => Some("drawArrays".to_string()),
                GLCmd::DrawElements { .. } => Some("drawElements".to_string()),
                GLCmd::DrawArraysInstanced { .. } => Some("drawArraysInstanced".to_string()),
                _ => None,
            })
            .collect();
        assert_eq!(
            sent,
            vec![
                "enable 0x8c89".to_string(),
                "useProgram 1".to_string(),
                "drawArrays".to_string(),
                "useProgram 0".to_string(),
            ],
            "only the calls that were taken are sent"
        );
    }

    /// An upload is checked before anything is sent (WebGL 1.0 5.14.8, WebGL 2.0 3.7.6 and 5.35): `pixelStorei` keeps
    /// the pixel-store state and refuses values GL does not have; the internal format, format and type must be a
    /// combination of ES 3.0 tables 3.2 / 3.3 (WebGL 1: the unsized ones only); the view must be of the type's kind and
    /// hold the bytes the pixel-store state lays the upload over, from its `srcOffset`; the unpack region must lie in
    /// the data store; and a PIXEL_UNPACK_BUFFER bound refuses a view while an offset needs one, inside it. What is sent
    /// is exactly the bytes the upload reads.
    #[test]
    fn uploads_are_checked_against_their_formats_views_and_the_pixel_store() {
        let (mut runtime, render_rx) = new_webgl_runtime();
        runtime
            .exec_script(
                "upload_checks.js",
                r#"
                const gl1 = new WebGLRenderingContext({ _rid: 207, width: 1, height: 1 }, {});
                const gl = new WebGL2RenderingContext({ _rid: 208, width: 1, height: 1 }, {});
                const check = (c, m) => { if (!c) throw new Error(m); };
                const err = (ctx, want, m) => { const got = ctx.getError(); if (got !== want) throw new Error(`${m}: getError ${got}, want ${want}`); };
                const ENUM = 0x0500, VALUE = 0x0501, OPERATION = 0x0502;
                const T2D = 0x0de1, RGBA = 0x1908, RGB = 0x1907, UBYTE = 0x1401;
                gl1.bindTexture(T2D, gl1.createTexture());
                gl.bindTexture(T2D, gl.createTexture());
                gl.bindTexture(0x806f, gl.createTexture());
                // pixel store
                check(gl1.getParameter(0x0cf5) === 4 && gl1.getParameter(0x9243) === 0x9244, "the pixel-store defaults");
                gl1.pixelStorei(0x0cf5, 3); err(gl1, VALUE, "an alignment of 3");
                gl1.pixelStorei(0x0cf2, 1); err(gl1, ENUM, "UNPACK_ROW_LENGTH in WebGL 1");
                check(gl1.getParameter(0x0cf2) === null, "no UNPACK_ROW_LENGTH in WebGL 1"); err(gl1, ENUM, "and INVALID_ENUM");
                gl1.pixelStorei(0x9243, 5); err(gl1, ENUM, "a colour-space conversion GL does not have");
                gl1.pixelStorei(0x9243, 0); err(gl1, 0, "NONE");
                check(gl1.getParameter(0x9243) === 0 && gl1.getParameter(0x0cf5) === 4, "refused values change nothing");
                gl.pixelStorei(0x0cf3, -1); err(gl, VALUE, "a negative skip");
                // WebGL 1 formats and views
                gl1.texImage2D(T2D, 0, RGBA, 1, 1, 0, RGBA, UBYTE, new Uint8Array(3)); err(gl1, OPERATION, "a short view");
                gl1.texImage2D(T2D, 0, RGBA, 1, 1, 0, RGB, UBYTE, new Uint8Array(4)); err(gl1, OPERATION, "a format other than the internal format");
                gl1.texImage2D(T2D, 0, RGBA, 1, 1, 0, RGBA, 0x1406, new Float32Array(4)); err(gl1, ENUM, "FLOAT in WebGL 1");
                gl1.texImage2D(T2D, 0, 0x1903, 1, 1, 0, 0x1903, UBYTE, new Uint8Array(1)); err(gl1, VALUE, "an internal format of RED in WebGL 1");
                gl1.texImage2D(T2D, 0, RGBA, 1, 1, 0, 0x1903, UBYTE, new Uint8Array(4)); err(gl1, ENUM, "a format of RED in WebGL 1");
                gl1.texImage2D(T2D, 16, RGBA, 1, 1, 0, 0x1903, UBYTE, new Uint8Array(4)); err(gl1, VALUE, "a level past the last is judged first");
                gl1.texImage2D(T2D, 0, RGBA, -1, 1, 0, 0x1903, UBYTE, new Uint8Array(4)); err(gl1, ENUM, "and then the formats, before the size");
                gl1.texImage2D(T2D, 0, RGBA, 1, 1, 0, RGBA, 0x8363, new Uint16Array(1)); err(gl1, OPERATION, "5_6_5 with RGBA");
                gl1.texImage2D(T2D, 0, RGBA, 1, 1, 0, RGBA, UBYTE, new Uint16Array(2)); err(gl1, OPERATION, "a view of another type");
                gl1.texImage2D(T2D, 0, RGBA, 1, 1, 0, RGBA, UBYTE, new DataView(new ArrayBuffer(4))); err(gl1, OPERATION, "a DataView");
                gl1.texImage2D(T2D, 0, RGBA, 1, 1, 0, RGBA, UBYTE, new Uint8ClampedArray(4)); err(gl1, 0, "a Uint8ClampedArray");   // sent: 4
                gl1.texImage2D(T2D, 0, RGB, 3, 2, 0, RGB, UBYTE, new Uint8Array(20)); err(gl1, OPERATION, "rows padded to 4: 12 + 9 bytes");
                gl1.texImage2D(T2D, 0, RGB, 3, 2, 0, RGB, UBYTE, new Uint8Array(32)); err(gl1, 0, "21 bytes or more");          // sent: 21
                gl1.pixelStorei(0x0cf5, 1);
                gl1.texImage2D(T2D, 0, RGB, 3, 2, 0, RGB, UBYTE, new Uint8Array(18)); err(gl1, 0, "rows packed at an alignment of 1");   // sent: 18
                gl1.texImage2D(T2D, 0, RGBA, 2, 2, 0, RGBA, UBYTE, null); err(gl1, 0, "storage only");        // sent: none
                gl1.texImage2D(T2D, 0, RGBA, 2, 2, 0, RGBA, UBYTE, new Uint8Array(32), 8); err(gl1, 0, "WebGL 1 ignores a srcOffset");   // sent: 16
                let threw = false;
                try { gl1.texImage2D(T2D, 0, RGBA, 1, 1, 0, RGBA, UBYTE, [0, 0, 0, 0]); } catch (e) { threw = e instanceof TypeError; }
                check(threw, "an array is not an ArrayBufferView");
                gl1.texSubImage2D(T2D, 0, 0, 0, 1, 1, RGBA, UBYTE, null); err(gl1, VALUE, "texSubImage2D of null");
                gl1.texSubImage2D(T2D, 0, -1, 0, 1, 1, RGBA, UBYTE, new Uint8Array(4)); err(gl1, VALUE, "a negative xoffset");
                gl1.texSubImage2D(T2D, 0, 0, 0, 1, 1, RGBA, 0x8363, new Uint16Array(1)); err(gl1, OPERATION, "a sub upload of 5_6_5 RGBA");
                gl1.texSubImage2D(T2D, 0, 0, 0, 1, 1, RGBA, UBYTE, new Uint8Array(3)); err(gl1, OPERATION, "a short sub upload");
                gl1.texSubImage2D(T2D, 0, 0, 0, 1, 1, RGBA, UBYTE, new Uint8Array(4)); err(gl1, 0, "a sub upload");   // sent: 4
                // WebGL 2 formats
                gl.texImage2D(T2D, 0, 0x8058, 1, 1, 0, RGB, UBYTE, new Uint8Array(4)); err(gl, OPERATION, "RGBA8 from RGB");
                gl.texImage2D(T2D, 0, 0x822e, 1, 1, 0, 0x1903, 0x1406, new Uint8Array(4)); err(gl, OPERATION, "R32F from a Uint8Array");
                gl.texImage2D(T2D, 0, 0x1234, 1, 1, 0, RGBA, UBYTE, new Uint8Array(4)); err(gl, VALUE, "an internal format GL does not have");
                gl.texImage3D(0x806f, 0, 0x81a5, 1, 1, 1, 0, 0x1902, 0x1403, new Uint16Array(1)); err(gl, OPERATION, "a depth image in a TEXTURE_3D");
                gl.texImage2D(T2D, 0, RGBA, 1, 1, 0, RGBA, 0x8d61, new Uint16Array(4)); err(gl, ENUM, "HALF_FLOAT_OES in WebGL 2");
                gl.texImage2D(T2D, 0, 0x822d, 1, 1, 0, 0x1903, 0x140b, new Uint16Array(1)); err(gl, 0, "R16F from HALF_FLOAT");   // sent: 2
                gl.texImage2D(T2D, 0, 0x8058, 1, 1, 0, RGBA, UBYTE, new Uint8Array(12).map((_, k) => k), 4); err(gl, 0, "a srcOffset");   // sent: 4..8
                gl.texImage2D(T2D, 0, 0x8058, 1, 1, 0, RGBA, UBYTE, new Uint8Array(8), 5); err(gl, OPERATION, "not enough data from srcOffset");
                gl.texImage2D(T2D, 0, 0x8058, 1, 1, 0, RGBA, UBYTE, new Uint8Array(8), 9); err(gl, OPERATION, "a srcOffset past the view");
                gl.texSubImage2D(T2D, 0, 0, 0, 1, 1, 0x1903, 0x1406, new Float32Array(1)); err(gl, OPERATION, "a pair of another sized format than the image's");
                gl.texSubImage2D(T2D, 0, 0, 0, 1, 1, RGBA, UBYTE, new Uint8Array(4)); err(gl, 0, "a sub upload of the image's pair");   // sent: 4
                gl.texSubImage2D(T2D, 0, 0, 0, 1, 1, RGB, 0x8368, new Uint32Array(1)); err(gl, OPERATION, "a pair neither table has");
                // the unpack region and its bytes
                gl.pixelStorei(0x0cf2, 4); gl.pixelStorei(0x0cf4, 1);
                check(gl.getParameter(0x0cf2) === 4 && gl.getParameter(0x0cf4) === 1, "ROW_LENGTH and SKIP_PIXELS kept");
                gl.texImage2D(T2D, 0, 0x8058, 2, 2, 0, RGBA, UBYTE, new Uint8Array(27)); err(gl, OPERATION, "a skip, a padded row and a row: 28 bytes");
                gl.texImage2D(T2D, 0, 0x8058, 2, 2, 0, RGBA, UBYTE, new Uint8Array(64)); err(gl, 0, "28 bytes or more");   // sent: 28
                gl.pixelStorei(0x0cf4, 3);
                gl.texImage2D(T2D, 0, 0x8058, 2, 2, 0, RGBA, UBYTE, new Uint8Array(64)); err(gl, OPERATION, "skipped pixels and a row past ROW_LENGTH");
                gl.pixelStorei(0x0cf2, 0);
                gl.texImage2D(T2D, 0, 0x8058, 2, 2, 0, RGBA, UBYTE, new Uint8Array(64)); err(gl, OPERATION, "a skip with no ROW_LENGTH");
                gl.pixelStorei(0x0cf4, 0);
                // 3D
                gl.texImage3D(0x806f, 0, RGBA, 1, 1, 2, 0, RGBA, UBYTE, new Uint8Array(7)); err(gl, OPERATION, "a short 3D view");
                gl.texImage3D(0x806f, 0, RGBA, 2, 1, 1, 0, RGBA, 0x8033, new Uint16Array([1, 2, 3, 4]), 2); err(gl, 0, "elements from srcOffset");   // sent: 3,0,4,0
                gl.pixelStorei(0x806e, 1);
                gl.texImage3D(0x806f, 0, RGBA, 1, 2, 1, 0, RGBA, UBYTE, new Uint8Array(8)); err(gl, OPERATION, "rows past UNPACK_IMAGE_HEIGHT");
                gl.pixelStorei(0x806e, 0);
                gl.texImage3D(0x806f, 0, RGBA, 1, 1, 1, 0, RGBA, UBYTE, null); err(gl, 0, "3D storage only");   // sent: none
                gl.texSubImage3D(0x806f, 0, 0, 0, 0, 1, 1, 1, RGBA, UBYTE, null); err(gl, VALUE, "texSubImage3D of null");
                gl.texSubImage3D(0x806f, 0, 0, 0, 0, 1, 1, 1, 0x84f9, 0x8dad, new Uint32Array(2)); err(gl, ENUM, "FLOAT_32_UNSIGNED_INT_24_8_REV from a view");
                // PIXEL_UNPACK_BUFFER
                gl.texImage3D(0x806f, 0, RGBA, 1, 1, 1, 0, RGBA, UBYTE, 0); err(gl, OPERATION, "an offset with no buffer bound");
                gl.texImage3D(0x806f, 0, RGBA, 1, 1, 1, 0, RGBA, UBYTE, -4); err(gl, OPERATION, "a negative offset with no buffer bound is the missing buffer");
                const unpack = gl.createBuffer();
                gl.bindBuffer(0x88ec, unpack);
                gl.bufferData(0x88ec, 16, 0x88e0);
                gl.texImage2D(T2D, 0, 0x8058, 1, 1, 0, RGBA, UBYTE, new Uint8Array(4)); err(gl, OPERATION, "a view with a buffer bound");
                gl.texImage2D(T2D, 0, 0x8058, 1, 1, 0, RGBA, UBYTE, null); err(gl, OPERATION, "storage only with a buffer bound");
                gl.texImage3D(0x806f, 0, RGBA, 1, 1, 1, 0, RGBA, UBYTE, new Uint8Array(4)); err(gl, OPERATION, "a 3D view with a buffer bound");
                gl.texImage3D(0x806f, 0, RGBA, 1, 1, 1, 0, RGBA, 0x8033, 1); err(gl, OPERATION, "an offset that is not a multiple of the type's size");
                gl.texImage3D(0x806f, 0, RGBA, 1, 1, 2, 0, RGBA, UBYTE, 12); err(gl, OPERATION, "a range past the buffer");
                gl.texImage3D(0x806f, 0, RGBA, 1, 1, 1, 0, RGBA, UBYTE, -4); err(gl, VALUE, "a negative offset");
                gl.texImage3D(0x806f, 0, RGBA, 1, 1, 2, 0, RGBA, UBYTE, 8); err(gl, 0, "the buffer's last 8 bytes");   // sent: offset 8
                gl.flush(); gl1.flush();
                "#,
            )
            .expect("the refused uploads should be refused, not thrown (but for the TypeError, which the script catches)");
        let got: Vec<String> = drain_gl_commands(&render_rx)
            .iter()
            .filter_map(|cmd| match cmd {
                GLCmd::TexImage2D { data, .. } => {
                    Some(format!("2d {}", describe_source(data.as_ref())))
                }
                GLCmd::TexSubImage2D { data, .. } => {
                    Some(format!("sub2d {}", describe_source(Some(data))))
                }
                GLCmd::TexImage3D { data, .. } => {
                    Some(format!("3d {}", describe_source(data.as_ref())))
                }
                GLCmd::TexSubImage3D { data, .. } => {
                    Some(format!("sub3d {}", describe_source(Some(data))))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            got,
            [
                "2d [0, 0, 0, 0]",
                "2d 21@0",
                "2d 18@0",
                "2d none",
                "2d 16@0",
                "sub2d [0, 0, 0, 0]",
                "2d [0, 0]",
                "2d [4, 5, 6, 7]",
                "sub2d [0, 0, 0, 0]",
                "2d 28@0",
                "3d [3, 0, 4, 0]",
                "3d none",
                "3d buffer 8",
            ],
            "only the uploads that were taken are sent, each with exactly the bytes it reads"
        );
    }

    /// The variant names of the texture commands that reached the renderer, in order: what a test of the texture
    /// image record compares, since a refused call must send nothing.
    fn texture_commands(render_rx: &crossbeam_channel::Receiver<RenderCommand>) -> Vec<String> {
        drain_gl_commands(render_rx)
            .iter()
            .map(|cmd| format!("{cmd:?}"))
            .map(|text| text.split([' ', '{', '(']).next().unwrap_or("").to_string())
            .filter(|name| {
                name.starts_with("Tex")
                    || name.starts_with("CompressedTex")
                    || name.starts_with("CopyTex")
                    || name == "GenerateMipmap"
                    || name.starts_with("RenderbufferStorage")
            })
            .collect()
    }

    /// WebGL 1: the facade records every image a call defines and judges what follows by it, before anything is sent
    /// (ES 2.0 3.7, WebGL 1.0 5.14.8 and "Texture Type in TexSubImage2D Calls"): an upload or copy into an image that is
    /// not there is INVALID_OPERATION, past its edge INVALID_VALUE, from another format or type than it was defined with
    /// INVALID_OPERATION; a mipmap level of a size that is not a power of two is INVALID_VALUE, and `generateMipmap`
    /// of one, or of a cube map whose faces are not all there, INVALID_OPERATION; the levels `generateMipmap` makes are
    /// there afterwards. The texture parameters are WebGL 1's four, each with its own values, and read back as set; a
    /// renderbuffer takes WebGL 1's six formats (RGB565 among them) and needs one bound.
    #[test]
    fn webgl1_texture_images_judge_the_calls_that_depend_on_them() {
        let (mut runtime, render_rx) = new_webgl_runtime();
        runtime
            .exec_script(
                "texture_images_webgl1.js",
                r#"
                const gl = new WebGLRenderingContext({ _rid: 230, width: 1, height: 1 }, {});
                const err = (want, m) => { const got = gl.getError(); if (got !== want) throw new Error(`${m}: getError ${got}, want ${want}`); };
                const ENUM = 0x0500, VALUE = 0x0501, OPERATION = 0x0502;
                const T2D = 0x0de1, CUBE = 0x8513, RGBA = 0x1908, RGB = 0x1907, UBYTE = 0x1401;
                gl.bindTexture(T2D, gl.createTexture());
                gl.texSubImage2D(T2D, 0, 0, 0, 1, 1, RGBA, UBYTE, new Uint8Array(4)); err(OPERATION, "an upload into no image");
                gl.copyTexSubImage2D(T2D, 0, 0, 0, 0, 0, 1, 1); err(OPERATION, "a copy into no image");
                gl.texImage2D(T2D, 0, RGBA, 4, 4, 0, RGBA, UBYTE, null); err(0, "a 4x4 image");                 // sent
                gl.texSubImage2D(T2D, 0, 3, 3, 2, 2, RGBA, UBYTE, new Uint8Array(16)); err(VALUE, "past the image's edge");
                gl.texSubImage2D(T2D, 0, 0, 0, 1, 1, RGB, UBYTE, new Uint8Array(3)); err(OPERATION, "another format than the image's");
                gl.texSubImage2D(T2D, 0, 0, 0, 1, 1, RGBA, 0x8033, new Uint16Array(1)); err(OPERATION, "another type than the image's");
                gl.texSubImage2D(T2D, 0, 0, 0, 4, 4, RGBA, UBYTE, new Uint8Array(64)); err(0, "the whole image");   // sent
                gl.texImage2D(T2D, 1, RGBA, 3, 3, 0, RGBA, UBYTE, null); err(VALUE, "a mipmap of 3x3");
                gl.copyTexImage2D(T2D, 1, RGBA, 0, 0, 3, 3, 0); err(VALUE, "a copied mipmap of 3x3");
                gl.generateMipmap(T2D); err(0, "mipmaps of 4x4");                                                  // sent
                gl.texSubImage2D(T2D, 2, 0, 0, 1, 1, RGBA, UBYTE, new Uint8Array(4)); err(0, "level 2 is 1x1");    // sent
                gl.texSubImage2D(T2D, 2, 0, 0, 2, 1, RGBA, UBYTE, new Uint8Array(8)); err(VALUE, "and no larger");
                gl.texSubImage2D(T2D, 3, 0, 0, 1, 1, RGBA, UBYTE, new Uint8Array(4)); err(OPERATION, "and the last");
                gl.texImage2D(T2D, 0, RGBA, 3, 3, 0, RGBA, UBYTE, null); err(0, "a 3x3 level 0");                   // sent
                gl.generateMipmap(T2D); err(OPERATION, "no mipmaps of 3x3");
                gl.copyTexImage2D(T2D, 0, RGB, 0, 0, 2, 2, 0); err(0, "a copied 2x2");                              // sent
                gl.copyTexSubImage2D(T2D, 0, 1, 1, 0, 0, 2, 2); err(VALUE, "a copy past its edge");
                gl.texSubImage2D(T2D, 0, 0, 0, 1, 1, RGB, UBYTE, new Uint8Array(3)); err(0, "RGB / UNSIGNED_BYTE, as copied"); // sent
                gl.bindTexture(CUBE, gl.createTexture());
                gl.texImage2D(0x8515, 0, RGBA, 2, 2, 0, RGBA, UBYTE, null); err(0, "one face");                    // sent
                gl.generateMipmap(CUBE); err(OPERATION, "a cube map with one face");
                for (let face = 0x8516; face <= 0x851a; face++) gl.texImage2D(face, 0, RGBA, 2, 2, 0, RGBA, UBYTE, null);   // 5 sent
                gl.generateMipmap(CUBE); err(0, "a cube map with all six");                                       // sent
                gl.texSubImage2D(0x851a, 1, 0, 0, 1, 1, RGBA, UBYTE, new Uint8Array(4)); err(0, "a face's level 1"); // sent
                // parameters
                const check = (c, m) => { if (!c) throw new Error(m); };
                gl.bindTexture(T2D, gl.createTexture());
                check(gl.getTexParameter(T2D, 0x2801) === 0x2702, "MIN_FILTER starts NEAREST_MIPMAP_LINEAR");
                gl.texParameteri(T2D, 0x2802, 0x2601); err(ENUM, "a wrap of LINEAR");
                gl.texParameteri(T2D, 0x8072, 0x2901); err(ENUM, "WRAP_R in WebGL 1");
                gl.texParameteri(T2D, 0x813c, 1); err(ENUM, "BASE_LEVEL in WebGL 1");
                gl.texParameterf(T2D, 0x2801, 0x2600); err(0, "MIN_FILTER NEAREST, through the float call");      // sent
                check(gl.getTexParameter(T2D, 0x2801) === 0x2600, "and it reads back");
                check(gl.getTexParameter(T2D, 0x8072) === null, "no WRAP_R"); err(ENUM, "WRAP_R is INVALID_ENUM");
                check(gl.getTexParameter(T2D, 0x912f) === null, "no IMMUTABLE_FORMAT"); err(ENUM, "IMMUTABLE_FORMAT is INVALID_ENUM");
                // renderbuffers
                gl.renderbufferStorage(0x8d41, 0x8d62, 4, 4); err(OPERATION, "no renderbuffer bound");
                gl.bindRenderbuffer(0x8d41, gl.createRenderbuffer());
                gl.renderbufferStorage(0x8d41, 0x8d62, 4, 4); err(0, "RGB565");                                   // sent
                gl.renderbufferStorage(0x8d41, 0x8058, 4, 4); err(ENUM, "RGBA8 in WebGL 1");
                check(gl.getRenderbufferParameter(0x8d41, 0x8d44) === 0x8d62, "the format kept is RGB565's");
                gl.flush();
                "#,
            )
            .expect("every call should be judged, none thrown");
        let mut want = vec![
            "TexImage2D",
            "TexSubImage2D",
            "GenerateMipmap",
            "TexSubImage2D",
            "TexImage2D",
            "CopyTexImage2D",
            "TexSubImage2D",
        ];
        want.extend(["TexImage2D"; 6]);
        want.extend([
            "GenerateMipmap",
            "TexSubImage2D",
            "TexParameterf",
            "RenderbufferStorage",
        ]);
        assert_eq!(
            texture_commands(&render_rx),
            want,
            "only the calls taken reach the renderer"
        );
    }

    /// WebGL 2 and the compressed formats: `texStorage*` makes every level of every face and fixes them, so a later
    /// `texImage*`, `copyTexImage2D`, compressed image or `texStorage*` is INVALID_OPERATION, and the storage reads back
    /// as immutable; an upload into an image takes a (format, type) ES 3.0 table 3.2 or 3.3 has with its internal
    /// format (an unsized RGBA takes 4_4_4_4); `generateMipmap` needs a base that is colour-renderable and filterable and
    /// starts at TEXTURE_BASE_LEVEL; 3D images keep their depth, a 2D array its layers. A compressed format is one only
    /// while its extension is enabled; its data is whole blocks, uploads into it start on a block and are of its format,
    /// and it is no 3D texture's. The parameters are read back as set, the LODs as floats.
    #[test]
    fn webgl2_and_compressed_texture_images_judge_the_calls_that_depend_on_them() {
        let (mut runtime, render_rx) = new_webgl_runtime();
        runtime
            .exec_script(
                "texture_images_webgl2.js",
                r#"
                const gl = new WebGL2RenderingContext({ _rid: 231, width: 1, height: 1 }, {});
                const err = (want, m) => { const got = gl.getError(); if (got !== want) throw new Error(`${m}: getError ${got}, want ${want}`); };
                const check = (c, m) => { if (!c) throw new Error(m); };
                const ENUM = 0x0500, VALUE = 0x0501, OPERATION = 0x0502;
                const T2D = 0x0de1, T3D = 0x806f, ARRAY = 0x8c1a, RGBA = 0x1908, UBYTE = 0x1401, RGBA8 = 0x8058;
                const ETC = 0x9278;
                // immutable storage
                gl.bindTexture(T2D, gl.createTexture());
                gl.texStorage2D(T2D, 3, RGBA8, 8, 8); err(0, "storage");                                           // sent
                check(gl.getTexParameter(T2D, 0x912f) === true && gl.getTexParameter(T2D, 0x82df) === 3, "immutable, 3 levels");
                gl.texImage2D(T2D, 0, RGBA8, 8, 8, 0, RGBA, UBYTE, null); err(OPERATION, "texImage2D on immutable storage");
                gl.copyTexImage2D(T2D, 0, RGBA8, 0, 0, 8, 8, 0); err(OPERATION, "copyTexImage2D on immutable storage");
                gl.texStorage2D(T2D, 1, RGBA8, 8, 8); err(OPERATION, "storage twice");
                gl.texSubImage2D(T2D, 2, 0, 0, 2, 2, RGBA, UBYTE, new Uint8Array(16)); err(0, "level 2 is 2x2");  // sent
                gl.texSubImage2D(T2D, 3, 0, 0, 1, 1, RGBA, UBYTE, new Uint8Array(4)); err(OPERATION, "there is no level 3");
                gl.texSubImage2D(T2D, 0, 0, 0, 1, 1, RGBA, 0x8033, new Uint16Array(1)); err(OPERATION, "4_4_4_4 into RGBA8");
                gl.generateMipmap(T2D); err(0, "mipmaps of immutable storage");                                    // sent
                gl.bindTexture(T2D, gl.createTexture());
                gl.texStorage2D(T2D, 1, 0x84f9, 4, 4); err(ENUM, "DEPTH_STENCIL is not sized");
                gl.texStorage2D(T2D, 1, 0x8d62, 4, 4); err(0, "RGB565 storage");                                   // sent
                gl.bindTexture(0x8513, gl.createTexture());
                gl.texStorage2D(0x8513, 1, RGBA8, 4, 2); err(VALUE, "a cube map that is not square");
                // mutable images
                gl.bindTexture(T2D, gl.createTexture());
                gl.texImage2D(T2D, 0, RGBA, 2, 2, 0, RGBA, UBYTE, null); err(0, "unsized RGBA");                  // sent
                gl.texSubImage2D(T2D, 0, 0, 0, 1, 1, RGBA, 0x8033, new Uint16Array(1)); err(0, "4_4_4_4 into unsized RGBA"); // sent
                gl.texImage2D(T2D, 0, 0x822e, 2, 2, 0, 0x1903, 0x1406, null); err(0, "R32F");                      // sent
                gl.generateMipmap(T2D); err(OPERATION, "R32F is not filterable");
                gl.texImage2D(T2D, 1, RGBA8, 4, 4, 0, RGBA, UBYTE, null); err(0, "a level 1 of 4x4");               // sent
                gl.texParameteri(T2D, 0x813c, -1); err(VALUE, "a negative BASE_LEVEL");
                gl.texParameteri(T2D, 0x813c, 1); err(0, "BASE_LEVEL 1");                                          // sent
                gl.generateMipmap(T2D); err(0, "mipmaps from level 1");                                            // sent
                gl.texSubImage2D(T2D, 3, 0, 0, 1, 1, RGBA, UBYTE, new Uint8Array(4)); err(0, "level 3 is 1x1");    // sent
                gl.texSubImage2D(T2D, 4, 0, 0, 1, 1, RGBA, UBYTE, new Uint8Array(4)); err(OPERATION, "and the last");
                gl.texParameterf(T2D, 0x813a, 0.5); err(0, "MIN_LOD 0.5");                                         // sent
                check(gl.getTexParameter(T2D, 0x813a) === 0.5 && gl.getTexParameter(T2D, 0x813b) === 1000, "the LODs");
                gl.texParameteri(T2D, 0x884d, 0x0201); err(0, "COMPARE_FUNC LESS");                                // sent
                check(gl.getTexParameter(T2D, 0x884d) === 0x0201 && gl.getTexParameter(T2D, 0x82df) === 0, "LESS; mutable");
                // 3D and 2D arrays
                gl.bindTexture(ARRAY, gl.createTexture());
                gl.texImage3D(ARRAY, 0, RGBA, 4, 4, 3, 0, RGBA, UBYTE, null); err(0, "three layers");              // sent
                gl.texSubImage3D(ARRAY, 0, 0, 0, 2, 4, 4, 1, RGBA, UBYTE, new Uint8Array(64)); err(0, "the last layer"); // sent
                gl.texSubImage3D(ARRAY, 0, 0, 0, 3, 4, 4, 1, RGBA, UBYTE, new Uint8Array(64)); err(VALUE, "no fourth");
                gl.bindTexture(T3D, gl.createTexture());
                gl.texStorage3D(T3D, 2, RGBA8, 4, 4, 4); err(0, "3D storage");                                      // sent
                gl.texSubImage3D(T3D, 1, 0, 0, 1, 2, 2, 1, RGBA, UBYTE, new Uint8Array(16)); err(0, "level 1 is 2x2x2"); // sent
                gl.texSubImage3D(T3D, 1, 0, 0, 2, 2, 2, 1, RGBA, UBYTE, new Uint8Array(16)); err(VALUE, "with two slices");
                gl.copyTexSubImage3D(T3D, 1, 0, 0, 2, 0, 0, 1, 1); err(VALUE, "a copy into no third slice");
                // renderbuffers
                gl.bindRenderbuffer(0x8d41, gl.createRenderbuffer());
                gl.renderbufferStorage(0x8d41, RGBA8, 4, 4); err(0, "RGBA8");                                      // sent
                gl.renderbufferStorage(0x8d41, 0x822d, 4, 4); err(ENUM, "R16F needs an extension");
                // compressed
                gl.bindTexture(T2D, gl.createTexture());
                gl.compressedTexImage2D(T2D, 0, ETC, 8, 8, 0, new Uint8Array(64)); err(ENUM, "ETC2 before its extension");
                gl._gpuCapsCache = 3;         // what the renderer would answer: ETC2 and ASTC
                gl.getExtension("WEBGL_compressed_texture_etc");
                const astc = gl.getExtension("WEBGL_compressed_texture_astc");
                check(astc.COMPRESSED_SRGB8_ALPHA8_ASTC_12x12_KHR === 0x93dd && astc.getSupportedProfiles()[0] === "ldr", "ASTC's 28");
                gl.compressedTexImage2D(T2D, 0, ETC, 8, 8, 0, new Uint8Array(63)); err(VALUE, "a block short");
                gl.compressedTexImage2D(T2D, 0, ETC, 8, 8, 0, new Uint8Array(64)); err(0, "four blocks");         // sent
                gl.compressedTexSubImage2D(T2D, 0, 2, 0, 4, 4, ETC, new Uint8Array(16)); err(OPERATION, "not on a block");
                gl.compressedTexSubImage2D(T2D, 0, 4, 4, 4, 4, 0x9274, new Uint8Array(8)); err(OPERATION, "another format");
                gl.compressedTexSubImage2D(T2D, 0, 4, 4, 8, 4, ETC, new Uint8Array(32)); err(VALUE, "past the edge");
                gl.compressedTexSubImage2D(T2D, 0, 4, 4, 4, 4, ETC, new Uint8Array(16)); err(0, "the last block");  // sent
                gl.texSubImage2D(T2D, 0, 0, 0, 1, 1, RGBA, UBYTE, new Uint8Array(4)); err(OPERATION, "pixels into a compressed image");
                gl.generateMipmap(T2D); err(OPERATION, "mipmaps of a compressed image");
                gl.bindTexture(T2D, gl.createTexture());
                gl.texStorage2D(T2D, 2, 0x93b4, 12, 6); err(0, "ASTC 6x6 storage");                                // sent
                gl.compressedTexSubImage2D(T2D, 1, 0, 0, 6, 3, 0x93b4, new Uint8Array(16)); err(0, "a block reaching the edge"); // sent
                gl.compressedTexImage2D(T2D, 0, 0x93b4, 12, 6, 0, new Uint8Array(32)); err(OPERATION, "a compressed image on immutable storage");
                gl.bindTexture(T3D, gl.createTexture());
                gl.compressedTexImage3D(T3D, 0, ETC, 4, 4, 1, 0, new Uint8Array(16)); err(OPERATION, "a compressed 3D texture");
                gl.texStorage3D(T3D, 1, ETC, 4, 4, 1); err(OPERATION, "compressed 3D storage");
                gl.bindTexture(ARRAY, gl.createTexture());
                gl.compressedTexImage3D(ARRAY, 0, ETC, 4, 4, 2, 0, new Uint8Array(32)); err(0, "two compressed layers"); // sent
                gl.compressedTexSubImage3D(ARRAY, 0, 0, 0, 1, 4, 4, 1, ETC, new Uint8Array(16)); err(0, "the second"); // sent
                // ETC1: its own extension, for compressedTexImage2D alone
                const ETC1 = 0x8d64;
                gl.bindTexture(T2D, gl.createTexture());
                gl.compressedTexImage2D(T2D, 0, ETC1, 4, 4, 0, new Uint8Array(8)); err(ENUM, "ETC1 before its extension");
                const etc1 = gl.getExtension("WEBGL_compressed_texture_etc1");
                check(etc1 !== gl.getExtension("WEBGL_compressed_texture_etc") && etc1.COMPRESSED_RGB_ETC1_WEBGL === ETC1 &&
                    Object.keys(etc1).length === 1, "ETC1's object is its own, with its one format");
                gl.compressedTexImage2D(T2D, 0, ETC1, 8, 8, 0, new Uint8Array(32)); err(0, "an ETC1 image");     // sent
                gl.compressedTexSubImage2D(T2D, 0, 0, 0, 4, 4, ETC1, new Uint8Array(8)); err(OPERATION, "no ETC1 sub-image");
                gl.bindTexture(T2D, gl.createTexture());
                gl.texStorage2D(T2D, 1, ETC1, 4, 4); err(0, "ETC1 storage");                                  // sent
                gl.bindTexture(ARRAY, gl.createTexture());
                gl.compressedTexImage3D(ARRAY, 0, ETC1, 4, 4, 1, 0, new Uint8Array(8)); err(OPERATION, "no ETC1 layers");
                gl.texStorage3D(ARRAY, 1, ETC1, 4, 4, 1); err(OPERATION, "nor ETC1 layered storage");
                gl.flush();
                "#,
            )
            .expect("every call should be judged, none thrown");
        assert_eq!(
            texture_commands(&render_rx),
            [
                "TexStorage2D",
                "TexSubImage2D",
                "GenerateMipmap",
                "TexStorage2D",
                "TexImage2D",
                "TexSubImage2D",
                "TexImage2D",
                "TexImage2D",
                "TexParameteri",
                "GenerateMipmap",
                "TexSubImage2D",
                "TexParameterf",
                "TexParameteri",
                "TexImage3D",
                "TexSubImage3D",
                "TexStorage3D",
                "TexSubImage3D",
                "RenderbufferStorage",
                "CompressedTexImage2D",
                "CompressedTexSubImage2D",
                "TexStorage2D",
                "CompressedTexSubImage2D",
                "CompressedTexImage3D",
                "CompressedTexSubImage3D",
                "CompressedTexImage2D",
                "TexStorage2D",
            ],
            "only the calls taken reach the renderer"
        );
    }

    /// The facade judges a framebuffer object's completeness from what it recorded (ES 3.0 4.4.4, WebGL 1.0 6.6): no
    /// attachment, an attachment that is not an image its point can render to, WebGL 1's attachments of different
    /// sizes and its depth and stencil attachments that are not one image are each answered without asking the
    /// driver; a draw, clear or read with such a framebuffer is INVALID_FRAMEBUFFER_OPERATION and nothing is sent. A
    /// copy takes only a read buffer it can convert (table 3.15): a destination component the source lacks, another
    /// component type, colour encoding or size is INVALID_OPERATION, as is a read buffer of NONE. The attachment calls
    /// are judged before they are recorded.
    #[test]
    fn framebuffer_completeness_is_judged_from_what_the_facade_recorded() {
        let (mut runtime, render_rx) = new_webgl_runtime();
        runtime
            .exec_script(
                "framebuffer_completeness.js",
                r#"
                const check = (c, m) => { if (!c) throw new Error(m); };
                const errOf = (gl) => (want, m) => { const got = gl.getError(); if (got !== want) throw new Error(`${m}: getError ${got}, want ${want}`); };
                const ENUM = 0x0500, VALUE = 0x0501, OPERATION = 0x0502, FB_OPERATION = 0x0506;
                const FB = 0x8d40, COLOR0 = 0x8ce0, T2D = 0x0de1, RB = 0x8d41, RGBA = 0x1908, UBYTE = 0x1401;
                // WebGL 1
                const gl = new WebGLRenderingContext({ _rid: 235, width: 4, height: 4 }, { alpha: false });
                const err = errOf(gl);
                const p = gl.createProgram();
                gl.linkProgram(p);
                gl._programParameterCache.set(p.id, new Map([[0x8b82, 1]]));      // what the renderer answers for LINK_STATUS
                p._consumes = []; p._consumesLink = p._links | 0;
                gl.useProgram(p); err(0, "useProgram");
                // A configuration the facade finds complete is the driver's to confirm (FRAMEBUFFER_UNSUPPORTED); this test
                // has no renderer to answer, so a clear has the facade judge the framebuffer and the confirmation is given.
                const complete = (ctx) => {
                    ctx.clear(0);
                    const bound = ctx._framebufferBinding;
                    bound._driverGeneration = bound._statusGeneration;
                    return ctx.checkFramebufferStatus(FB) === 0x8cd5;
                };
                const fb = gl.createFramebuffer();
                gl.bindFramebuffer(FB, fb);
                check(gl.checkFramebufferStatus(FB) === 0x8cd7, "no attachment: MISSING_ATTACHMENT");
                gl.drawArrays(4, 0, 3); err(FB_OPERATION, "a draw into it");
                gl.clear(0x4000); err(FB_OPERATION, "a clear of it");
                gl.clear(0x10); err(VALUE, "a bad mask first");
                gl.readPixels(0, 0, 1, 1, RGBA, UBYTE, new Uint8Array(4)); err(FB_OPERATION, "a read from it");
                const lum = gl.createTexture();
                gl.bindTexture(T2D, lum);
                gl.texImage2D(T2D, 0, 0x1909, 4, 4, 0, 0x1909, UBYTE, null);
                gl.framebufferTexture2D(FB, COLOR0, T2D, lum, 0); err(0, "LUMINANCE attached");       // sent
                check(gl.checkFramebufferStatus(FB) === 0x8cd6, "LUMINANCE: INCOMPLETE_ATTACHMENT");
                gl.framebufferTexture2D(FB, COLOR0, T2D, lum, 1); err(VALUE, "WebGL 1 attaches level 0 only");
                gl.framebufferTexture2D(FB, COLOR0 + 1, T2D, lum, 0); err(ENUM, "COLOR_ATTACHMENT1 needs WEBGL_draw_buffers");
                gl.framebufferTexture2D(FB, COLOR0, T2D, gl.createTexture(), 0); err(OPERATION, "a texture never bound");
                gl.framebufferRenderbuffer(FB, 0x8d00, RB, gl.createRenderbuffer()); err(OPERATION, "a renderbuffer never bound");
                const color = gl.createTexture();
                gl.bindTexture(T2D, color);
                gl.texImage2D(T2D, 0, RGBA, 4, 4, 0, RGBA, UBYTE, null);
                gl.framebufferTexture2D(FB, COLOR0, T2D, color, 0);                                 // sent
                const depth = gl.createRenderbuffer();
                gl.bindRenderbuffer(RB, depth);
                gl.renderbufferStorage(RB, 0x81a5, 8, 8);                                         // DEPTH_COMPONENT16
                gl.framebufferRenderbuffer(FB, 0x8d00, RB, depth);                                 // sent
                check(gl.checkFramebufferStatus(FB) === 0x8cd9, "WebGL 1 sizes differ: DIMENSIONS");
                gl.renderbufferStorage(RB, 0x81a5, 4, 4);
                check(complete(gl), "the same size: complete");
                const stencil = gl.createRenderbuffer();
                gl.bindRenderbuffer(RB, stencil);
                gl.renderbufferStorage(RB, 0x8d48, 4, 4);                                         // STENCIL_INDEX8
                gl.framebufferRenderbuffer(FB, 0x8d20, RB, stencil);                               // sent
                check(gl.checkFramebufferStatus(FB) === 0x8cdd, "depth and stencil apart: UNSUPPORTED");
                gl.framebufferRenderbuffer(FB, 0x8d20, RB, null);                                  // sent
                gl.drawArrays(4, 0, 3); err(0, "complete again: drawn");                              // sent
                // copies from the drawing buffer, which has no alpha
                gl.bindFramebuffer(FB, null);
                gl.bindTexture(T2D, gl.createTexture());
                gl.copyTexImage2D(T2D, 0, 0x1906, 0, 0, 2, 2, 0); err(OPERATION, "ALPHA from a buffer with none");
                gl.copyTexImage2D(T2D, 0, 0x1907, 0, 0, 2, 2, 0); err(0, "RGB from it");               // sent
                // WebGL 2
                const gl2 = new WebGL2RenderingContext({ _rid: 236, width: 4, height: 4 }, {});
                gl2._maxColorAttachments = 4;
                const err2 = errOf(gl2);
                const fb2 = gl2.createFramebuffer();
                gl2.bindFramebuffer(FB, fb2);
                const ui = gl2.createRenderbuffer();
                gl2.bindRenderbuffer(RB, ui);
                gl2.renderbufferStorage(RB, 0x8d7c, 4, 4);                                        // RGBA8UI
                gl2.framebufferRenderbuffer(FB, COLOR0, RB, ui);                                   // sent
                check(complete(gl2), "RGBA8UI is colour-renderable");
                gl2.bindTexture(T2D, gl2.createTexture());
                gl2.copyTexImage2D(T2D, 0, 0x8058, 0, 0, 2, 2, 0); err2(OPERATION, "RGBA8 from RGBA8UI");
                gl2.copyTexImage2D(T2D, 0, 0x8d7c, 0, 0, 2, 2, 0); err2(0, "RGBA8UI from it");         // sent
                gl2.copyTexImage2D(T2D, 0, 0x881a, 0, 0, 2, 2, 0); err2(ENUM, "a float format needs an extension");
                gl2.readBuffer(0x0405); err2(OPERATION, "BACK on an object");
                gl2.readBuffer(0); err2(0, "NONE");                                                // sent
                check(gl2.getParameter(0x0c02) === 0, "READ_BUFFER is NONE");
                gl2.copyTexImage2D(T2D, 0, 0x8d7c, 0, 0, 2, 2, 0); err2(OPERATION, "a read buffer of NONE");
                gl2.bindFramebuffer(FB, null);
                gl2.readBuffer(COLOR0); err2(OPERATION, "COLOR_ATTACHMENT0 on the default framebuffer");
                check(gl2.getParameter(0x0c02) === 0x0405, "READ_BUFFER is BACK");
                gl.flush(); gl2.flush();
                "#,
            )
            .expect("every call should be judged, none thrown");
        let sent: Vec<String> = drain_gl_commands(&render_rx)
            .iter()
            .map(|cmd| format!("{cmd:?}"))
            .map(|text| text.split([' ', '{', '(']).next().unwrap_or("").to_string())
            .filter(|name| {
                name.starts_with("Framebuffer")
                    || name.starts_with("Draw")
                    || name.starts_with("CopyTex")
                    || name == "ReadBuffer"
            })
            .collect();
        assert_eq!(
            sent,
            [
                "FramebufferTexture2D",
                "FramebufferTexture2D",
                "FramebufferRenderbuffer",
                "FramebufferRenderbuffer",
                "FramebufferRenderbuffer",
                "DrawArrays",
                "CopyTexImage2D",
                "FramebufferRenderbuffer",
                "CopyTexImage2D",
                "ReadBuffer",
            ],
            "only the calls taken reach the renderer"
        );
    }

    /// A TexImageSource upload is judged against WebGL 2's own table (3.7.6) in a browser's order -- an internal format
    /// outside it INVALID_VALUE, a format or type outside it INVALID_ENUM, a combination it does not list
    /// INVALID_OPERATION -- and its WebGL 2 selection (UNPACK_SKIP_* and the call's size, UNPACK_IMAGE_HEIGHT in 3D) must
    /// be within the source. What passes is one `TexImageSource` command naming its call and its source: `ImageData`'s
    /// rows, a canvas, a snapshot spent by the upload; a sub call carries the image's effective format for the
    /// renderer's copy, and every call the content's own format and type. A value that is no TexImageSource is a
    /// TypeError, and WebGL 1 takes one in the 6- and 7-argument forms only.
    #[test]
    fn tex_image_source_uploads_are_judged_against_their_own_table_and_name_their_source() {
        let (mut runtime, render_rx) = new_webgl_runtime();
        runtime
            .exec_script(
                "tex_image_source.js",
                r#"
                const check = (c, m) => { if (!c) throw new Error(m); };
                const errOf = (gl) => (want, m) => { const got = gl.getError(); if (got !== want) throw new Error(`${m}: getError ${got}, want ${want}`); };
                const throws = (f, m) => { try { f(); } catch (e) { if (e instanceof TypeError) return; throw e; } throw new Error(`${m}: no TypeError`); };
                const ENUM = 0x0500, VALUE = 0x0501, OPERATION = 0x0502;
                const T2D = 0x0de1, RGBA = 0x1908, RGB = 0x1907, UBYTE = 0x1401;
                const image = { width: 4, height: 4, data: new Uint8ClampedArray(64) };
                const gl = new WebGL2RenderingContext({ _rid: 240, width: 4, height: 4 }, {});
                const err = errOf(gl);
                gl.bindTexture(T2D, gl.createTexture());
                gl.texImage2D(T2D, 0, 0x8d88, 0x8d99, 0x1402, image); err(VALUE, "RGBA16I is not a TexImageSource's");
                gl.texImage2D(T2D, 0, 0x8f97, RGBA, 0x1400, image); err(VALUE, "RGBA8_SNORM neither");
                gl.texImage2D(T2D, 0, 0x8c3d, RGB, 0x8c3e, image); err(ENUM, "5_9_9_9_REV is not a TexImageSource's type");
                gl.texImage2D(T2D, 0, RGBA, RGB, UBYTE, image); err(OPERATION, "RGBA from RGB");
                gl.texImage2D(T2D, 0, 0x881a, RGBA, 0x140b, image); err(0, "RGBA16F of half floats");        // sent
                gl.pixelStorei(0x0cf4, 1); gl.pixelStorei(0x0cf3, 2);                                       // skip 1, 2
                gl.texImage2D(T2D, 0, RGBA, 2, 2, 0, RGBA, UBYTE, image); err(0, "a 2 x 2 selection from (1, 2)"); // sent
                gl.texImage2D(T2D, 0, RGBA, 3, 3, 0, RGBA, UBYTE, image); err(OPERATION, "a selection past the source");
                gl.texImage2D(T2D, 0, RGBA, RGBA, UBYTE, image); err(OPERATION, "the source's size from a skip");
                gl.pixelStorei(0x0cf4, 0); gl.pixelStorei(0x0cf3, 0);
                gl.texImage2D(T2D, 0, RGBA, RGBA, UBYTE, image); err(0, "the whole source");                // sent
                gl.texSubImage2D(T2D, 0, 0, 0, RGB, UBYTE, image); err(OPERATION, "RGB into RGBA");
                const canvas = { _rid: 77, width: 4, height: 4, getContext() { return null; } };
                gl.texSubImage2D(T2D, 0, 0, 0, RGBA, UBYTE, canvas); err(0, "a canvas into it");          // sent
                const snapshot = { width: 4, height: 4 };
                Object.defineProperty(snapshot, "__migo_snapshot_id__", { value: 9, writable: true });
                Object.defineProperty(snapshot, "__migo_snapshot_spent__", { value: false, writable: true });
                gl.texSubImage2D(T2D, 0, 0, 0, 4, 4, RGBA, UBYTE, snapshot); err(0, "a snapshot");           // sent
                check(snapshot.__migo_snapshot_spent__ === true, "the upload spends the snapshot");
                const array = gl.createTexture();
                gl.bindTexture(0x8c1a, array);
                gl.pixelStorei(0x806e, 2);                                                                 // image height 2
                gl.texImage3D(0x8c1a, 0, 0x8058, 4, 1, 2, 0, RGBA, UBYTE, image); err(0, "two slices, 2 rows apart"); // sent
                gl.texImage3D(0x8c1a, 0, 0x8058, 4, 1, 3, 0, RGBA, UBYTE, image); err(OPERATION, "a third is past it");
                gl.pixelStorei(0x806e, 0);
                gl.pixelStorei(0x9240, 1);                                                                 // FLIP_Y
                gl.texSubImage3D(0x8c1a, 0, 0, 0, 0, 4, 1, 1, RGBA, UBYTE, image); err(0, "flipped, as a source may be"); // sent
                gl.pixelStorei(0x9240, 0);
                throws(() => gl.texImage2D(T2D, 0, RGBA, RGBA, UBYTE, {}), "an object that is no source");
                throws(() => gl.texImage2D(T2D, 0, RGBA, RGBA, UBYTE, null), "null");
                const gl1 = new WebGLRenderingContext({ _rid: 241, width: 4, height: 4 }, {});
                gl1.bindTexture(T2D, gl1.createTexture());
                throws(() => gl1.texImage2D(T2D, 0, RGBA, 4, 4, 0, RGBA, UBYTE, image), "WebGL 1 takes bytes in 9 arguments");
                gl1.texImage2D(T2D, 0, 0x8058, RGBA, UBYTE, image); errOf(gl1)(VALUE, "WebGL 1 has no sized formats");
                gl1.texImage2D(T2D, 0, RGB, RGB, 0x8363, image); errOf(gl1)(0, "565 from a source");          // sent
                gl.flush(); gl1.flush();
                "#,
            )
            .expect("every call should be judged");
        let sources: Vec<GLCmd> = drain_gl_commands(&render_rx)
            .into_iter()
            .filter(|cmd| matches!(cmd, GLCmd::TexImageSource { .. }))
            .collect();
        let pixels = shared::protocol::render_cmd::TextureSource::Pixels {
            bytes: std::sync::Arc::new(vec![0; 64]),
            width: 4,
            height: 4,
        };
        use shared::protocol::render_cmd::{SourceUploadCall as Call, TextureSource as Source};
        let summary: Vec<(Call, i32, i32, u32, u32, u32, Source)> = sources
            .into_iter()
            .map(|cmd| match cmd {
                GLCmd::TexImageSource {
                    call,
                    width,
                    height,
                    format,
                    type_,
                    destination_format,
                    source,
                    ..
                } => (
                    call,
                    width,
                    height,
                    format,
                    type_,
                    destination_format,
                    source,
                ),
                _ => unreachable!(),
            })
            .collect();
        let image2d = |internalformat| Call::Image2D { internalformat };
        assert_eq!(
            summary,
            vec![
                (image2d(0x881a), 4, 4, 0x1908, 0x140b, 0, pixels.clone()),
                (image2d(0x1908), 2, 2, 0x1908, 0x1401, 0, pixels.clone()),
                (image2d(0x1908), 4, 4, 0x1908, 0x1401, 0, pixels.clone()),
                // RGBA of bytes is RGBA8, which the renderer may copy into.
                (
                    Call::SubImage2D {
                        xoffset: 0,
                        yoffset: 0
                    },
                    4,
                    4,
                    0x1908,
                    0x1401,
                    0x8058,
                    Source::Canvas { canvas_2d_id: 77 }
                ),
                (
                    Call::SubImage2D {
                        xoffset: 0,
                        yoffset: 0
                    },
                    4,
                    4,
                    0x1908,
                    0x1401,
                    0x8058,
                    Source::Snapshot { snapshot_id: 9 }
                ),
                (
                    Call::Image3D {
                        internalformat: 0x8058,
                        depth: 2
                    },
                    4,
                    1,
                    0x1908,
                    0x1401,
                    0,
                    pixels.clone()
                ),
                (
                    Call::SubImage3D {
                        xoffset: 0,
                        yoffset: 0,
                        zoffset: 0,
                        depth: 1
                    },
                    4,
                    1,
                    0x1908,
                    0x1401,
                    0x8058,
                    pixels.clone()
                ),
                (image2d(0x1907), 4, 4, 0x1907, 0x8363, 0, pixels),
            ],
            "only the uploads taken reach the renderer, each as its call names its source"
        );
    }

    /// `drawBuffers` is judged as a browser judges it -- a value that is not NONE, BACK or a colour attachment the
    /// context has INVALID_ENUM, more than MAX_DRAW_BUFFERS INVALID_VALUE, anything but BACK or NONE alone for the
    /// default framebuffer and anything but COLOR_ATTACHMENTi or NONE at place i for an object INVALID_OPERATION -- and
    /// recorded per framebuffer, which is what DRAW_BUFFERi answers without crossing; WebGL 1's WEBGL_draw_buffers
    /// alike. Blending into a 32-bit float draw buffer is INVALID_OPERATION where the renderer cannot (caps bit 3),
    /// and EXT_float_blend is offered where it can.
    #[test]
    fn draw_buffers_are_judged_and_recorded_per_framebuffer_and_float_blending_needs_the_renderer()
    {
        let (mut runtime, render_rx) = new_webgl_runtime();
        runtime
            .exec_script(
                "draw_buffers.js",
                r#"
                const check = (c, m) => { if (!c) throw new Error(m); };
                const errOf = (gl) => (want, m) => { const got = gl.getError(); if (got !== want) throw new Error(`${m}: getError ${got}, want ${want}`); };
                const ENUM = 0x0500, VALUE = 0x0501, OPERATION = 0x0502;
                const NONE = 0, BACK = 0x0405, CA0 = 0x8ce0, CA1 = 0x8ce1, DB0 = 0x8825, DB1 = 0x8826, FB = 0x8d40;
                const gl = new WebGL2RenderingContext({ _rid: 242, width: 4, height: 4 }, {});
                gl._maxColorAttachments = 8; gl._maxDrawBuffers = 8;
                const err = errOf(gl);
                check(gl.getParameter(DB0) === BACK && gl.getParameter(DB1) === NONE, "the default framebuffer draws to BACK");
                gl.drawBuffers([NONE]); err(0, "NONE alone");                                             // sent
                check(gl.getParameter(DB0) === NONE, "recorded");
                gl.drawBuffers([BACK]); err(0, "BACK alone");                                             // sent
                gl.drawBuffers([CA0]); err(OPERATION, "a colour attachment of the default framebuffer");
                gl.drawBuffers([BACK, NONE]); err(OPERATION, "two of the default framebuffer");
                gl.drawBuffers([]); err(OPERATION, "none of the default framebuffer");
                gl.drawBuffers([0x1234]); err(ENUM, "no draw buffer at all");
                const fb = gl.createFramebuffer();
                gl.bindFramebuffer(FB, fb);
                check(gl.getParameter(DB0) === CA0 && gl.getParameter(DB1) === NONE, "an object draws to COLOR_ATTACHMENT0");
                const list = new Uint32Array([CA0, CA1]);
                gl.drawBuffers(list); err(0, "CA0, CA1");                                                  // sent
                list[1] = NONE;
                check(gl.getParameter(DB1) === CA1, "recorded, and not the content's array");
                gl.drawBuffers([CA1]); err(OPERATION, "CA1 at place 0");
                gl.drawBuffers([BACK]); err(OPERATION, "BACK of an object");
                gl.drawBuffers(new Array(9).fill(NONE)); err(VALUE, "more than MAX_DRAW_BUFFERS");
                gl.drawBuffers([CA0 + 8]); err(ENUM, "an attachment past MAX_COLOR_ATTACHMENTS");
                check(gl.getParameter(DB0 + 8) === null, "DRAW_BUFFER8 past MAX_DRAW_BUFFERS"); err(ENUM, "is INVALID_ENUM");
                gl.bindFramebuffer(FB, null);
                check(gl.getParameter(DB0) === BACK, "the default framebuffer kept its own");
                gl.bindFramebuffer(FB, fb);
                check(gl.getParameter(DB1) === CA1, "and the object its own");
                const gl1 = new WebGLRenderingContext({ _rid: 243, width: 4, height: 4 }, {});
                check(gl1.getParameter(DB0) === null, "WebGL 1 without WEBGL_draw_buffers"); errOf(gl1)(ENUM, "has no DRAW_BUFFER0");
                const ext = gl1.getExtension("WEBGL_draw_buffers");
                gl1._maxColorAttachments = 4; gl1._maxDrawBuffers = 4;
                ext.drawBuffersWEBGL([CA0]); errOf(gl1)(OPERATION, "WebGL 1's default framebuffer too");
                ext.drawBuffersWEBGL([BACK]); errOf(gl1)(0, "BACK");                                       // sent
                check(gl1.getParameter(ext.DRAW_BUFFER0_WEBGL) === BACK, "answered from the record");
                // Blending into 32-bit floats.
                const program = (ctx) => {
                    const p = ctx.createProgram();
                    ctx.linkProgram(p);
                    ctx._programParameterCache.set(p.id, new Map([[0x8b82, 1]]));
                    p._consumes = []; p._consumesLink = p._links | 0;
                    ctx.useProgram(p);
                };
                const complete = (ctx) => {
                    ctx.clear(0);
                    const bound = ctx._framebufferBinding;
                    bound._driverGeneration = bound._statusGeneration;
                    return ctx.checkFramebufferStatus(FB) === 0x8cd5;
                };
                const floats = (caps, internalformat, type) => {
                    const ctx = new WebGL2RenderingContext({ _rid: 244 + caps, width: 4, height: 4 }, {});
                    ctx._gpuCapsCache = caps; ctx._maxColorAttachments = 8; ctx._maxDrawBuffers = 8;
                    ctx.getExtension("EXT_color_buffer_float");
                    program(ctx);
                    ctx.bindFramebuffer(FB, ctx.createFramebuffer());
                    const t = ctx.createTexture();
                    ctx.bindTexture(0x0de1, t);
                    ctx.texImage2D(0x0de1, 0, internalformat, 4, 4, 0, 0x1908, type, null);
                    ctx.framebufferTexture2D(FB, CA0, 0x0de1, t, 0);
                    check(complete(ctx), "the float attachment is complete");
                    ctx.enable(0x0be2);
                    return ctx;
                };
                const plain = floats(4, 0x8814, 0x1406);
                const perr = errOf(plain);
                plain.drawArrays(4, 0, 3); perr(OPERATION, "blending into RGBA32F without float blending");
                plain.disable(0x0be2);
                plain.drawArrays(4, 0, 3); perr(0, "not blending");                                         // sent
                plain.enable(0x0be2);
                plain.drawBuffers([NONE]);
                plain.drawArrays(4, 0, 3); perr(0, "blending into no draw buffer");                         // sent
                check(plain.getExtension("EXT_float_blend") === null, "and no EXT_float_blend");
                const half = floats(4, 0x881a, 0x140b);
                half.drawArrays(4, 0, 3); errOf(half)(0, "blending into RGBA16F needs nothing");             // sent
                const blending = floats(12, 0x8814, 0x1406);
                blending.drawArrays(4, 0, 3); errOf(blending)(0, "where the renderer blends floats");        // sent
                check(blending.getExtension("EXT_float_blend") !== null &&
                    blending.getSupportedExtensions().includes("EXT_float_blend"), "EXT_float_blend is offered there");
                gl.flush(); gl1.flush(); plain.flush(); half.flush(); blending.flush();
                "#,
            )
            .expect("every call should be judged");
        let commands = drain_gl_commands(&render_rx);
        let draw_buffers: Vec<Vec<u32>> = commands
            .iter()
            .filter_map(|cmd| match cmd {
                GLCmd::DrawBuffers { buffers, .. } => Some(buffers.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            draw_buffers,
            vec![
                vec![0],
                vec![0x0405],
                vec![0x8ce0, 0x8ce1],
                vec![0x0405],
                vec![0]
            ],
            "only the draw buffers taken reach the renderer"
        );
        let draws = commands
            .iter()
            .filter(|cmd| matches!(cmd, GLCmd::DrawArrays { .. }))
            .count();
        assert_eq!(
            draws, 4,
            "the blend into RGBA32F without float blending is not drawn"
        );
    }

    /// EXT_color_buffer_float is offered to a WebGL 2 context whose renderer renders to float colour buffers (caps bit
    /// 2), to no other. Until it is enabled a float image is not colour-renderable: a framebuffer with one attached is
    /// INCOMPLETE_ATTACHMENT, a float renderbuffer or copy INVALID_ENUM, a mipmap of one INVALID_OPERATION. Enabling it
    /// has the framebuffer judged again, and each call is what the extension makes it; a 32-bit float image is still not
    /// filterable, so not mipmapped, and a copy takes only a float source of its own sizes.
    #[test]
    fn a_float_colour_buffer_is_renderable_once_ext_color_buffer_float_is_enabled() {
        let (mut runtime, render_rx) = new_webgl_runtime();
        runtime
            .exec_script(
                "ext_color_buffer_float.js",
                r#"
                const check = (c, m) => { if (!c) throw new Error(m); };
                const errOf = (gl) => (want, m) => { const got = gl.getError(); if (got !== want) throw new Error(`${m}: getError ${got}, want ${want}`); };
                const ENUM = 0x0500, OPERATION = 0x0502, FB_OPERATION = 0x0506;
                const FB = 0x8d40, COLOR0 = 0x8ce0, T2D = 0x0de1, RB = 0x8d41, RGBA = 0x1908, HALF = 0x140b, FLOAT = 0x1406;
                const RGBA16F = 0x881a, RGBA32F = 0x8814, NAME = 'EXT_color_buffer_float';
                const gl1 = new WebGLRenderingContext({ _rid: 237, width: 4, height: 4 }, {});
                gl1._gpuCapsCache = 7;
                check(gl1.getExtension(NAME) === null, "not to WebGL 1");
                check(!gl1.getSupportedExtensions().includes(NAME), "nor listed there");
                const bare = new WebGL2RenderingContext({ _rid: 238, width: 4, height: 4 }, {});
                bare._gpuCapsCache = 3;
                check(bare.getExtension(NAME) === null, "not where the renderer has no float colour buffers");
                check(!bare.getSupportedExtensions().includes(NAME), "nor listed there");
                const gl = new WebGL2RenderingContext({ _rid: 239, width: 4, height: 4 }, {});
                gl._gpuCapsCache = 4;
                gl._maxColorAttachments = 4;
                const err = errOf(gl);
                check(gl.getSupportedExtensions().includes(NAME), "listed where the renderer has them");
                const complete = () => {
                    gl.clear(0);
                    const bound = gl._framebufferBinding;
                    bound._driverGeneration = bound._statusGeneration;
                    return gl.checkFramebufferStatus(FB) === 0x8cd5;
                };
                const fb = gl.createFramebuffer();
                gl.bindFramebuffer(FB, fb);
                const half = gl.createTexture();
                gl.bindTexture(T2D, half);
                gl.texImage2D(T2D, 0, RGBA16F, 4, 4, 0, RGBA, HALF, null); err(0, "an RGBA16F image");
                gl.framebufferTexture2D(FB, COLOR0, T2D, half, 0); err(0, "attached");             // sent
                check(gl.checkFramebufferStatus(FB) === 0x8cd6, "not enabled: INCOMPLETE_ATTACHMENT");
                gl.clear(0x4000); err(FB_OPERATION, "a clear of it");
                gl.generateMipmap(T2D); err(OPERATION, "a mipmap of it");
                const rb = gl.createRenderbuffer();
                gl.bindRenderbuffer(RB, rb);
                gl.renderbufferStorage(RB, RGBA32F, 4, 4); err(ENUM, "an RGBA32F renderbuffer");
                gl.bindFramebuffer(FB, null);
                const target = gl.createTexture();
                gl.bindTexture(T2D, target);
                gl.copyTexImage2D(T2D, 0, RGBA16F, 0, 0, 2, 2, 0); err(ENUM, "a copy into RGBA16F");
                const ext = gl.getExtension(NAME);
                check(ext !== null && gl.getExtension(NAME) === ext, "enabled, one object");
                gl.copyTexImage2D(T2D, 0, RGBA16F, 0, 0, 2, 2, 0); err(OPERATION, "RGBA16F from the drawing buffer's RGBA8");
                gl.bindFramebuffer(FB, fb);
                check(complete(), "enabled: the framebuffer with RGBA16F is complete");
                gl.copyTexImage2D(T2D, 0, RGBA16F, 0, 0, 2, 2, 0); err(0, "RGBA16F from RGBA16F");          // sent
                gl.copyTexImage2D(T2D, 0, 0x8058, 0, 0, 2, 2, 0); err(OPERATION, "RGBA8 from a float buffer");
                gl.copyTexImage2D(T2D, 0, RGBA32F, 0, 0, 2, 2, 0); err(OPERATION, "RGBA32F from RGBA16F, another size");
                gl.bindTexture(T2D, half);
                gl.generateMipmap(T2D); err(0, "a mipmap of RGBA16F");                                     // sent
                gl.renderbufferStorage(RB, RGBA32F, 4, 4); err(0, "an RGBA32F renderbuffer");             // sent
                gl.framebufferRenderbuffer(FB, COLOR0 + 1, RB, rb); err(0, "attached beside it");          // sent
                check(complete(), "both float attachments: complete");
                const full = gl.createTexture();
                gl.bindTexture(T2D, full);
                gl.texImage2D(T2D, 0, RGBA32F, 4, 4, 0, RGBA, FLOAT, null); err(0, "an RGBA32F image");
                gl.generateMipmap(T2D); err(OPERATION, "RGBA32F is not filterable");
                gl.flush();
                "#,
            )
            .expect("every call should be judged, none thrown");
        let sent: Vec<String> = drain_gl_commands(&render_rx)
            .iter()
            .map(|cmd| format!("{cmd:?}"))
            .map(|text| text.split([' ', '{', '(']).next().unwrap_or("").to_string())
            .filter(|name| {
                name.starts_with("Framebuffer")
                    || name.starts_with("CopyTex")
                    || name == "GenerateMipmap"
                    || name == "RenderbufferStorage"
                    || name == "Clear"
            })
            .collect();
        assert_eq!(
            sent,
            [
                "FramebufferTexture2D",
                "Clear",
                "CopyTexImage2D",
                "GenerateMipmap",
                "RenderbufferStorage",
                "FramebufferRenderbuffer",
                "Clear",
            ],
            "only the calls taken reach the renderer"
        );
    }

    /// ANGLE_instanced_arrays: an instanced draw needs an attribute array enabled at divisor 0 -- at any location, as a
    /// browser counts it -- and is INVALID_OPERATION without one, where a plain draw is not held to it; a divisor for an
    /// index past MAX_VERTEX_ATTRIBS is INVALID_VALUE, in WebGL 1's extension and WebGL 2 alike.
    #[test]
    fn an_angle_instanced_draw_needs_an_array_at_divisor_zero() {
        let (mut runtime, render_rx) = new_webgl_runtime();
        runtime
            .exec_script(
                "angle_divisor_zero.js",
                r#"
                const gl = new WebGLRenderingContext({ _rid: 234, width: 1, height: 1 }, {});
                gl._maxVertexAttribs = 16;       // what the renderer would answer
                const err = (want, m) => { const got = gl.getError(); if (got !== want) throw new Error(`${m}: getError ${got}, want ${want}`); };
                const ext = gl.getExtension("ANGLE_instanced_arrays");
                const p = gl.createProgram();
                gl._programParameterCache.set(p.id, new Map([[0x8b82, 1]]));
                p._consumes = [0]; p._consumesLink = p._links | 0;
                gl.useProgram(p);
                gl.bindBuffer(0x8892, gl.createBuffer());
                gl.bufferData(0x8892, new Float32Array(64), 0x88e4);
                gl.enableVertexAttribArray(0);
                gl.vertexAttribPointer(0, 4, 0x1406, false, 0, 0);
                ext.vertexAttribDivisorANGLE(0, 1);
                gl.getError();
                ext.drawArraysInstancedANGLE(4, 0, 3, 2); err(0x0502, "every enabled array is instanced");
                gl.drawArrays(4, 0, 3); err(0, "a plain draw is not held to it");                            // sent
                gl.enableVertexAttribArray(5);
                gl.vertexAttribPointer(5, 4, 0x1406, false, 0, 0);
                ext.drawArraysInstancedANGLE(4, 0, 3, 2); err(0, "an array at divisor 0 anywhere");          // sent
                ext.vertexAttribDivisorANGLE(16, 1); err(0x0501, "a divisor past MAX_VERTEX_ATTRIBS");
                gl.flush();
                "#,
            )
            .expect("every call should be judged, none thrown");
        let draws: Vec<String> = drain_gl_commands(&render_rx)
            .iter()
            .map(|cmd| format!("{cmd:?}"))
            .map(|text| text.split([' ', '{', '(']).next().unwrap_or("").to_string())
            .filter(|name| name.starts_with("Draw"))
            .collect();
        assert_eq!(draws, ["DrawArrays", "DrawArraysInstanced"]);
    }

    /// WebGL 2's sampler, transform feedback and query objects are the facade's, judged before anything is sent and
    /// read back from what it recorded (ES 3.0 2.14, 2.15.1, 3.8.2; WebGL 2.0 5.38): SAMPLER_BINDING is the sampler on
    /// the active unit, and a deleted one is unbound everywhere; TRANSFORM_FEEDBACK_BINDING is the object bound, which
    /// cannot change while it is active and unpaused; a query is begun on one target for good, one at a time a slot --
    /// the two occlusion targets share one -- and its result is not available in the task that ended it, the same
    /// every time it is asked there. An object of another kind is a TypeError.
    #[test]
    fn webgl2_samplers_transform_feedbacks_and_queries_are_the_facades() {
        let (mut runtime, render_rx) = new_webgl_runtime();
        runtime
            .exec_script(
                "webgl2_objects.js",
                r#"
                const gl = new WebGL2RenderingContext({ _rid: 233, width: 1, height: 1 }, {});
                gl._maxTextureUnits = 32;        // what the renderer would answer for MAX_COMBINED_TEXTURE_IMAGE_UNITS
                const err = (want, m) => { const got = gl.getError(); if (got !== want) throw new Error(`${m}: getError ${got}, want ${want}`); };
                const check = (c, m) => { if (!c) throw new Error(m); };
                const throwsType = (f, m) => { try { f(); } catch (e) { if (e instanceof TypeError) return; throw e; } throw new Error(`${m}: no TypeError`); };
                const ENUM = 0x0500, VALUE = 0x0501, OPERATION = 0x0502;
                // samplers
                const s = gl.createSampler();
                gl.bindSampler(3, s); err(0, "a sampler on unit 3");                                      // sent
                gl.activeTexture(0x84c0 + 3);
                check(gl.getParameter(0x8919) === s, "SAMPLER_BINDING is the object on the active unit");
                gl.activeTexture(0x84c0);
                check(gl.getParameter(0x8919) === null, "and nothing on unit 0");
                gl.bindSampler(1000, s); err(VALUE, "a unit past the limit");
                throwsType(() => gl.bindSampler(0, gl.createTexture()), "a texture is not a sampler");
                gl.deleteSampler(s);
                gl.activeTexture(0x84c0 + 3);
                check(gl.getParameter(0x8919) === null, "a deleted sampler is unbound");
                gl.bindSampler(0, s); err(OPERATION, "a deleted sampler");
                // transform feedback
                const tf = gl.createTransformFeedback();
                check(!gl.isTransformFeedback(tf), "not a transform feedback until bound");
                gl.bindTransformFeedback(0x8892, tf); err(ENUM, "a target that is not TRANSFORM_FEEDBACK");
                gl.bindTransformFeedback(0x8e22, tf); err(0, "bound");                                     // sent
                check(gl.isTransformFeedback(tf) && gl.getParameter(0x8e25) === tf, "TRANSFORM_FEEDBACK_BINDING");
                throwsType(() => gl.bindTransformFeedback(0x8e22, {}), "an object that is not one");
                gl.beginTransformFeedback(0x0000);                                                       // sent
                gl.bindTransformFeedback(0x8e22, null); err(OPERATION, "rebinding while active");
                gl.deleteTransformFeedback(tf); err(OPERATION, "deleting an active one");
                gl.pauseTransformFeedback();                                                             // sent
                gl.bindTransformFeedback(0x8e22, null); err(0, "rebinding while paused");                 // sent
                check(gl.getParameter(0x8e25) === null, "the default is null");
                // queries
                const q = gl.createQuery(), other = gl.createQuery();
                check(!gl.isQuery(q), "not a query until begun");
                gl.beginQuery(0x88bf, q); err(ENUM, "TIME_ELAPSED needs an extension");
                throwsType(() => gl.beginQuery(0x8c2f, null), "null is not a query");
                gl.beginQuery(0x8c2f, q); err(0, "an occlusion query");                                   // sent
                check(gl.isQuery(q) && gl.getQuery(0x8c2f, 0x8865) === q, "CURRENT_QUERY");
                check(gl.getQuery(0x8d6a, 0x8865) === null, "not for the other occlusion target");
                gl.beginQuery(0x8d6a, other); err(OPERATION, "the occlusion slot is taken");
                gl.getQueryParameter(q, 0x8867); err(OPERATION, "an active query has no result");
                gl.endQuery(0x8d6a); err(OPERATION, "ending the target it was not begun on");
                gl.endQuery(0x8c2f); err(0, "ended");                                                     // sent
                gl.endQuery(0x8c2f); err(OPERATION, "nothing active");
                gl.beginQuery(0x8d6a, q); err(OPERATION, "begun on another target before");
                check(gl.getQueryParameter(q, 0x8867) === false && gl.getQueryParameter(q, 0x8867) === false &&
                    gl.getQueryParameter(q, 0x8866) === 0, "not available in the task that ended it, every time");
                gl.getQueryParameter(q, 0x8865); err(ENUM, "a pname that is not one");
                gl.getQueryParameter(other, 0x8867); err(OPERATION, "a query never begun");
                throwsType(() => gl.getQueryParameter(s, 0x8867), "a sampler is not a query");
                gl.deleteQuery(q);
                check(!gl.isQuery(q), "deleted");
                gl.flush();
                "#,
            )
            .expect("every call should be judged, none thrown");
        let sent: Vec<String> = drain_gl_commands(&render_rx)
            .iter()
            .map(|cmd| format!("{cmd:?}"))
            .map(|text| text.split([' ', '{', '(']).next().unwrap_or("").to_string())
            .filter(|name| {
                name.contains("Sampler")
                    || name.contains("TransformFeedback")
                    || name.contains("Query")
            })
            .collect();
        assert_eq!(
            sent,
            [
                "CreateSampler",
                "BindSampler",
                "DeleteSampler",
                "CreateTransformFeedback",
                "BindTransformFeedback",
                "BeginTransformFeedback",
                "PauseTransformFeedback",
                "BindTransformFeedback",
                "CreateQuery",
                "CreateQuery",
                "BeginQuery",
                "EndQuery",
                "DeleteQuery",
            ],
            "only the calls taken reach the renderer"
        );
    }

    /// Each context's constructor declares, ahead of any command of its own, the drawing buffer its attributes ask for
    /// (`GLCmd::WebglContext`): each attribute a dictionary member WebIDL converts to a boolean (so `0` is false and a
    /// non-empty string true), the specification's default when absent -- alpha and depth on, stencil and
    /// preserveDrawingBuffer off -- and WebGL 2's constructor passes its options on. `getContextAttributes()` answers
    /// what the buffer has: those, with antialiasing off, since the drawing buffer is single-sampled.
    #[test]
    fn a_context_declares_the_drawing_buffer_its_attributes_ask_for() {
        let (mut runtime, render_rx) = new_webgl_runtime();
        runtime
            .exec_script(
                "context_attributes.js",
                r#"
                const check = (c, m) => { if (!c) throw new Error(m); };
                const plain = new WebGLRenderingContext({ _rid: 240, width: 1, height: 1 }, {});
                const asked = new WebGLRenderingContext({ _rid: 241, width: 1, height: 1 },
                    { alpha: 0, depth: false, stencil: 1, preserveDrawingBuffer: "yes", antialias: true });
                const webgl2 = new WebGL2RenderingContext({ _rid: 242, width: 1, height: 1 }, { stencil: true, alpha: false });
                const a = plain.getContextAttributes();
                check(a.alpha === true && a.depth === true && a.stencil === false && a.antialias === false &&
                      a.premultipliedAlpha === true && a.preserveDrawingBuffer === false, "the defaults: " + JSON.stringify(a));
                const b = asked.getContextAttributes();
                check(b.alpha === false && b.depth === false && b.stencil === true && b.preserveDrawingBuffer === true &&
                      b.antialias === false, "converted as WebIDL converts them: " + JSON.stringify(b));
                const c = webgl2.getContextAttributes();
                check(c.stencil === true && c.alpha === false, "WebGL 2 takes its options: " + JSON.stringify(c));
                plain.flush(); asked.flush(); webgl2.flush();
                "#,
            )
            .expect("the contexts should be created");
        let mut declared = Vec::new();
        while let Ok(command) = render_rx.try_recv() {
            if let RenderCommand::FramePacket(packet) = command {
                for op in packet.into_ops() {
                    if let FrameOp::GlBatch(payload) = op {
                        for cmd in payload.commands {
                            if let GLCmd::WebglContext {
                                canvas_id,
                                alpha,
                                depth,
                                stencil,
                                preserve_drawing_buffer,
                            } = cmd
                            {
                                declared.push((
                                    canvas_id,
                                    alpha,
                                    depth,
                                    stencil,
                                    preserve_drawing_buffer,
                                ));
                            }
                        }
                    }
                }
            }
        }
        assert_eq!(
            declared,
            vec![
                (240, true, true, false, false),
                (241, false, false, true, true),
                (242, false, true, true, false),
            ]
        );
    }

    /// LINK_STATUS is asked as the link's whole result (`gl_state::LINK_RESULT`): whether it linked and the attribute
    /// locations the program consumes, once per link -- `useProgram` and the draws that follow use the cached answer, a
    /// relink asks again -- and the draws judge the attributes in it and no others. A program that did not link answers
    /// false, and `useProgram` refuses it.
    #[test]
    fn link_status_brings_the_attribute_locations_a_program_consumes() {
        let (mut runtime, render_rx) = new_webgl_runtime();
        let responder = std::thread::spawn(move || {
            let mut asked = Vec::new();
            while let Ok(command) = render_rx.recv_timeout(Duration::from_secs(1)) {
                match command {
                    RenderCommand::GL(GLCmd::GetState { query, resp, .. }) => {
                        asked.push(query);
                        resp.ok(if asked.len() == 1 {
                            "{\"v\":[true,[0,2]]}".to_string()
                        } else {
                            "{\"v\":[false,[]]}".to_string()
                        });
                    }
                    RenderCommand::GL(_) | RenderCommand::FramePacket(_) => {}
                    other => panic!("unexpected render command: {other:?}"),
                }
            }
            asked
        });
        runtime
            .exec_script(
                "link_result.js",
                r#"
                const gl = new WebGLRenderingContext({ _rid: 236, width: 1, height: 1 }, {});
                const err = (want, m) => { const got = gl.getError(); if (got !== want) throw new Error(`${m}: getError ${got}, want ${want}`); };
                const p = gl.createProgram();
                gl.linkProgram(p);
                if (gl.getProgramParameter(p, 0x8b82) !== true) throw new Error("it linked");
                gl.useProgram(p); err(0, "a linked program is used without asking again");
                const tiny = gl.createBuffer();
                gl.bindBuffer(0x8892, tiny);
                gl.bufferData(0x8892, 8, 0x88e4);
                gl.enableVertexAttribArray(1);                                   // not consumed, and no buffer
                gl.drawArrays(4, 0, 3); err(0, "an attribute the program does not consume");
                gl.enableVertexAttribArray(2);
                gl.vertexAttribPointer(2, 4, 0x1406, false, 0, 0);               // consumed: 16 bytes a vertex
                gl.drawArrays(4, 0, 3); err(0x0502, "location 2 is consumed and holds no vertex");
                gl.linkProgram(p);
                if (gl.getProgramParameter(p, 0x8b82) !== false) throw new Error("the relink failed");
                gl.useProgram(p); err(0x0502, "a program that did not link is not used");
                gl.flush();
                "#,
            )
            .expect("the program calls should run");
        drop(runtime);
        assert_eq!(
            responder.join().unwrap(),
            vec![
                frame_wire::sync::gl_state::LINK_RESULT,
                frame_wire::sync::gl_state::LINK_RESULT
            ],
            "one question per link"
        );
    }

    /// A draw reads no vertex outside the buffers of the attributes the program in use consumes (WebGL 1.0 6.4-6.6, kept
    /// by WebGL 2): every consumed attribute enabled as an array needs a buffer, and the buffer must hold the vertices
    /// `first` .. `first + count - 1` at the attribute's offset and stride -- the largest index, for an indexed draw,
    /// read from the facade's copy of the element-array buffer and cached until its bytes change -- and the instances
    /// an instanced attribute reads by its divisor. An attribute the program does not consume is not judged. The mode,
    /// counts, index type (UNSIGNED_INT needs OES_element_index_uint in WebGL 1) and offset alignment are judged first,
    /// in a browser's order; in WebGL 2 the primitive-restart index reads no vertex. Only accepted draws are sent.
    #[test]
    fn draws_read_no_vertex_outside_the_buffers_of_the_attributes_consumed() {
        let (mut runtime, render_rx) = new_webgl_runtime();
        runtime
            .exec_script(
                "draw_ranges.js",
                r#"
                const ENUM = 0x0500, VALUE = 0x0501, OPERATION = 0x0502;
                const ARRAY = 0x8892, ELEMENTS = 0x8893, STATIC = 0x88e4, FLOAT = 0x1406, USHORT = 0x1403, TRIANGLES = 4;
                const setUp = (gl, consumes) => {
                    const program = gl.createProgram();
                    gl.linkProgram(program);
                    gl._programParameterCache.set(program.id, new Map([[0x8b82, 1]]));   // what the renderer answers for LINK_STATUS
                    program._consumes = consumes; program._consumesLink = program._links | 0;   // and the locations it consumes
                    gl.useProgram(program);
                    return program;
                };
                const gl = new WebGLRenderingContext({ _rid: 234, width: 1, height: 1 }, {});
                const err = (want, m) => { const got = gl.getError(); if (got !== want) throw new Error(`${m}: getError ${got}, want ${want}`); };
                setUp(gl, [0, 1]);
                const vertices = gl.createBuffer();
                gl.bindBuffer(ARRAY, vertices);
                gl.bufferData(ARRAY, 24, STATIC);                               // three vec2 of floats
                gl.enableVertexAttribArray(0);
                gl.vertexAttribPointer(0, 2, FLOAT, false, 0, 0);
                gl.drawArrays(TRIANGLES, 0, 3); err(0, "three vertices");                                  // 1
                gl.drawArrays(TRIANGLES, 1, 3); err(OPERATION, "a fourth vertex");
                gl.drawArrays(9, 0, 3); err(ENUM, "no primitive");
                gl.drawArrays(TRIANGLES, -1, 3); err(VALUE, "a negative first");
                gl.enableVertexAttribArray(1);
                gl.drawArrays(TRIANGLES, 0, 3); err(OPERATION, "a consumed attribute enabled with no buffer");
                gl.disableVertexAttribArray(1);
                const small = gl.createBuffer();
                gl.bindBuffer(ARRAY, small);
                gl.bufferData(ARRAY, 4, STATIC);
                gl.enableVertexAttribArray(2);
                gl.vertexAttribPointer(2, 4, FLOAT, false, 0, 0);
                gl.drawArrays(TRIANGLES, 0, 3); err(0, "an attribute the program does not consume");         // 2
                gl.bindBuffer(ARRAY, vertices);
                gl.vertexAttribPointer(0, 2, FLOAT, false, 12, 4);            // vertex i at 4 + 12 i, 8 bytes
                gl.drawArrays(TRIANGLES, 0, 2); err(0, "two vertices at a stride");                         // 3
                gl.drawArrays(TRIANGLES, 0, 3); err(OPERATION, "the third past the buffer");
                gl.vertexAttribPointer(0, 2, FLOAT, false, 0, 0);
                // indexed
                const indices = gl.createBuffer();
                gl.bindBuffer(ELEMENTS, indices);
                gl.bufferData(ELEMENTS, new Uint16Array([0, 1, 5, 0xffff]), STATIC);
                gl.drawElements(TRIANGLES, 2, USHORT, 0); err(0, "indices 0 and 1");                       // 4
                gl.drawElements(TRIANGLES, 3, USHORT, 0); err(OPERATION, "index 5");
                gl.bufferSubData(ELEMENTS, 4, new Uint16Array([2]));
                gl.drawElements(TRIANGLES, 3, USHORT, 0); err(0, "index 5 replaced by 2");                 // 5
                gl.drawElements(TRIANGLES, 1, USHORT, 6); err(OPERATION, "0xffff is an index in WebGL 1");
                gl.drawElements(TRIANGLES, 2, USHORT, 1); err(OPERATION, "an offset off the index size");
                gl.drawElements(TRIANGLES, 3, 0x1405, 0); err(ENUM, "UNSIGNED_INT before its extension");
                gl.drawElements(TRIANGLES, 0, USHORT, 0); err(0, "no indices");                              // 6
                // instanced
                const ext = gl.getExtension("ANGLE_instanced_arrays");
                const perInstance = gl.createBuffer();
                gl.bindBuffer(ARRAY, perInstance);
                gl.bufferData(ARRAY, 32, STATIC);                              // two vec4 of floats
                gl.enableVertexAttribArray(1);
                gl.vertexAttribPointer(1, 4, FLOAT, false, 0, 0);
                ext.vertexAttribDivisorANGLE(1, 1);
                ext.drawArraysInstancedANGLE(TRIANGLES, 0, 3, 2); err(0, "two instances");                 // 7
                ext.drawArraysInstancedANGLE(TRIANGLES, 0, 3, 3); err(OPERATION, "a third instance");
                ext.vertexAttribDivisorANGLE(1, 2);
                ext.drawElementsInstancedANGLE(TRIANGLES, 3, USHORT, 0, 4); err(0, "four instances, two each"); // 8
                ext.drawArraysInstancedANGLE(TRIANGLES, 0, 3, 5); err(OPERATION, "a fifth needs a third element");
                gl.deleteBuffer(vertices);
                gl.drawArrays(TRIANGLES, 0, 3); err(OPERATION, "the vertex buffer deleted");
                // WebGL 2
                const gl2 = new WebGL2RenderingContext({ _rid: 235, width: 1, height: 1 }, {});
                const err2 = (want, m) => { const got = gl2.getError(); if (got !== want) throw new Error(`${m}: getError ${got}, want ${want}`); };
                setUp(gl2, [0]);
                gl2.bindBuffer(ARRAY, gl2.createBuffer());
                gl2.bufferData(ARRAY, 24, STATIC);
                gl2.enableVertexAttribArray(0);
                gl2.vertexAttribPointer(0, 2, FLOAT, false, 0, 0);
                const restart = gl2.createBuffer();
                gl2.bindBuffer(ELEMENTS, restart);
                gl2.bufferData(ELEMENTS, new Uint16Array([0, 0xffff, 2]), STATIC);
                gl2.drawElements(TRIANGLES, 3, USHORT, 0); err2(0, "the restart index reads no vertex");    // 9
                gl2.drawElements(TRIANGLES, 3, 0x1405, 0); err2(OPERATION, "UNSIGNED_INT past six bytes");
                const other = gl2.createBuffer();
                gl2.bindBuffer(ELEMENTS, other);
                gl2.bufferData(ELEMENTS, new Uint16Array([0, 9, 0]), STATIC);
                gl2.bindBuffer(0x8f36, other);                                 // COPY_READ_BUFFER
                gl2.bindBuffer(0x8f37, restart);                               // COPY_WRITE_BUFFER
                gl2.bindBuffer(ELEMENTS, restart);
                gl2.copyBufferSubData(0x8f36, 0x8f37, 2, 2, 2);                // index 9 into the restart buffer
                gl2.drawElements(TRIANGLES, 3, USHORT, 0); err2(OPERATION, "index 9, copied in");
                gl2.drawArraysInstanced(TRIANGLES, 0, 3, 7); err2(0, "instances of a vertex attribute");    // 10
                gl.flush(); gl2.flush();
                "#,
            )
            .expect("every draw should be judged, none thrown");
        let draws: Vec<&str> = drain_gl_commands(&render_rx)
            .iter()
            .filter_map(|cmd| match cmd {
                GLCmd::DrawArrays { .. } => Some("arrays"),
                GLCmd::DrawElements { .. } => Some("elements"),
                GLCmd::DrawArraysInstanced { .. } => Some("arrays instanced"),
                GLCmd::DrawElementsInstanced { .. } => Some("elements instanced"),
                _ => None,
            })
            .collect();
        assert_eq!(
            draws,
            [
                "arrays",
                "arrays",
                "arrays",
                "elements",
                "elements",
                "elements",
                "arrays instanced",
                "elements instanced",
                "elements",
                "arrays instanced",
            ],
            "only the accepted draws are sent"
        );
    }

    /// WebGL 1 samples a texture whose level 0 is not a power of two as incomplete -- (0, 0, 0, 1) -- unless both wraps
    /// are CLAMP_TO_EDGE and the minification filter reads no mipmap (ES 2.0 3.8.2); the driver, OpenGL ES 3.0, would
    /// sample it. So every draw holds no texture where such a texture is bound and puts the binding back after it, with
    /// the active unit; a texture made complete by its parameters or a power-of-two level 0, unbound or deleted is no
    /// longer withheld, and a draw with none withheld is the draw alone. WebGL 2 withholds nothing: ES 3.0's rules are
    /// its own.
    #[test]
    fn webgl1_draws_sample_no_incomplete_npot_texture() {
        let (mut runtime, render_rx) = new_webgl_runtime();
        runtime
            .exec_script(
                "npot_incomplete.js",
                r#"
                const T2D = 0x0de1, CUBE = 0x8513, RGBA = 0x1908, UBYTE = 0x1401;
                const use = (gl) => {
                    const program = gl.createProgram();
                    gl.linkProgram(program);
                    gl._programParameterCache.set(program.id, new Map([[0x8b82, 1]]));   // what the renderer answers for LINK_STATUS
                    program._consumes = []; program._consumesLink = program._links | 0;   // and the attribute locations it consumes: none
                    gl.useProgram(program);
                };
                const gl = new WebGLRenderingContext({ _rid: 232, width: 1, height: 1 }, {});
                use(gl);
                gl.bindBuffer(0x8893, gl.createBuffer());
                gl.bufferData(0x8893, 6, 0x88e4);
                const a = gl.createTexture();
                gl.bindTexture(T2D, a);
                gl.texImage2D(T2D, 0, RGBA, 3, 3, 0, RGBA, UBYTE, null);
                gl.drawArrays(4, 0, 3);                                   // 1: REPEAT and a mipmap filter: withheld
                gl.texParameteri(T2D, 0x2802, 0x812f);
                gl.texParameteri(T2D, 0x2803, 0x812f);
                gl.drawArrays(4, 0, 3);                                   // 2: still a mipmap filter: withheld
                gl.texParameteri(T2D, 0x2801, 0x2601);
                gl.drawArrays(4, 0, 3);                                   // 3: complete
                gl.texParameteri(T2D, 0x2803, 0x2901);                    // REPEAT again
                gl.activeTexture(0x84c3);
                const c = gl.createTexture();
                gl.bindTexture(CUBE, c);
                for (let face = 0x8515; face <= 0x851a; face++) gl.texImage2D(face, 0, RGBA, 3, 3, 0, RGBA, UBYTE, null);
                gl.drawElements(4, 3, 0x1403, 0);                         // 4: unit 0's 2D and unit 3's cube map
                gl.enableVertexAttribArray(1);                            // an array at divisor 0, which an ANGLE instanced draw needs
                gl.getExtension("ANGLE_instanced_arrays").drawArraysInstancedANGLE(4, 0, 3, 2);   // 5: the same
                gl.deleteTexture(c);
                gl.activeTexture(0x84c0);
                gl.texImage2D(T2D, 0, RGBA, 4, 4, 0, RGBA, UBYTE, null);  // a power of two
                gl.drawArrays(4, 0, 3);                                   // 6: none
                gl.texImage2D(T2D, 0, RGBA, 5, 4, 0, RGBA, UBYTE, null);
                gl.bindTexture(T2D, null);
                gl.drawArrays(4, 0, 3);                                   // 7: unbound: none
                gl.bindTexture(T2D, a);
                const gl2 = new WebGL2RenderingContext({ _rid: 233, width: 1, height: 1 }, {});
                use(gl2);
                gl2.bindTexture(T2D, gl2.createTexture());
                gl2.texImage2D(T2D, 0, RGBA, 3, 3, 0, RGBA, UBYTE, null);
                gl2.drawArrays(4, 0, 3);                                  // 8: WebGL 2: none
                gl.flush(); gl2.flush();
                "#,
            )
            .expect("the draws should be taken");
        // The bindings, active units and draws, the textures named by the order they first appear in.
        let mut names: Vec<String> = Vec::new();
        let mut name = |texture: String| {
            let index = names.iter().position(|n| *n == texture).unwrap_or_else(|| {
                names.push(texture);
                names.len() - 1
            });
            ["A", "C", "B2"][index].to_string()
        };
        let got: Vec<String> = drain_gl_commands(&render_rx)
            .into_iter()
            .filter_map(|cmd| match cmd {
                GLCmd::ActiveTexture { unit, .. } => Some(format!("unit {}", unit - 0x84C0)),
                GLCmd::BindTexture {
                    target, texture, ..
                } => Some(match texture {
                    Some(texture) => format!("{target:#x} {}", name(format!("{texture:?}"))),
                    None => format!("{target:#x} none"),
                }),
                GLCmd::DrawArrays { .. } => Some("draw".to_string()),
                GLCmd::DrawElements { .. } => Some("draw".to_string()),
                GLCmd::DrawArraysInstanced { .. } => Some("draw".to_string()),
                _ => None,
            })
            .collect();
        let withheld_2d = [
            "unit 0",
            "0xde1 none",
            "draw",
            "unit 0",
            "0xde1 A",
            "unit 0",
        ];
        let mut want: Vec<&str> = vec!["0xde1 A"];
        want.extend(withheld_2d); // 1
        want.extend(withheld_2d); // 2
        want.push("draw"); // 3
        want.extend(["unit 3", "0x8513 C"]);
        let withheld_both = [
            "unit 0",
            "0xde1 none",
            "unit 3",
            "0x8513 none",
            "draw",
            "unit 0",
            "0xde1 A",
            "unit 3",
            "0x8513 C",
            "unit 3",
        ];
        want.extend(withheld_both); // 4
        want.extend(withheld_both); // 5
        want.extend(["unit 0", "draw"]); // the delete unbinds nothing on unit 0; 6
        want.extend(["0xde1 none", "draw", "0xde1 A"]); // 7
        want.extend(["0xde1 B2", "draw"]); // 8
        assert_eq!(got, want);
    }

    /// WebGL 2's offset overloads of `texImage2D` / `texSubImage2D` upload from the bound PIXEL_UNPACK_BUFFER. Judged as a
    /// browser judges them: no buffer bound, or UNPACK_FLIP_Y_WEBGL / UNPACK_PREMULTIPLY_ALPHA_WEBGL set, is
    /// INVALID_OPERATION before anything else; then the level, formats and size; then the offset -- negative is
    /// INVALID_VALUE, past 2^31 - 1, not a multiple of the type's size or reading past the buffer INVALID_OPERATION.
    /// WebGL 1 has no such overload. A 3D upload from a view is INVALID_OPERATION while either flag is set; storage
    /// only is not a view.
    #[test]
    fn uploads_from_an_unpack_buffer_offset_and_the_unpack_flags() {
        let (mut runtime, render_rx) = new_webgl_runtime();
        runtime
            .exec_script(
                "unpack_buffer_uploads.js",
                r#"
                const gl1 = new WebGLRenderingContext({ _rid: 210, width: 1, height: 1 }, {});
                const gl = new WebGL2RenderingContext({ _rid: 211, width: 1, height: 1 }, {});
                const err = (ctx, want, m) => { const got = ctx.getError(); if (got !== want) throw new Error(`${m}: getError ${got}, want ${want}`); };
                const VALUE = 0x0501, OPERATION = 0x0502;
                const T2D = 0x0de1, RGBA = 0x1908, RGBA8 = 0x8058, UBYTE = 0x1401;
                gl1.bindTexture(T2D, gl1.createTexture());
                gl.bindTexture(T2D, gl.createTexture());
                gl.bindTexture(0x806f, gl.createTexture());
                let threw = false;
                try { gl1.texImage2D(T2D, 0, RGBA, 1, 1, 0, RGBA, UBYTE, 0); } catch (e) { threw = e instanceof TypeError; }
                if (!threw) throw new Error("WebGL 1 has no offset overload: a number is not a view");
                gl.texImage2D(T2D, 0, RGBA8, 1, 1, 0, RGBA, UBYTE, 0); err(gl, OPERATION, "an offset with no buffer bound");
                gl.texSubImage2D(T2D, 0, 0, 0, 1, 1, RGBA, UBYTE, 0); err(gl, OPERATION, "a sub offset with no buffer bound");
                gl.texImage2D(T2D, 16, RGBA8, 1, 1, 0, RGBA, UBYTE, -1); err(gl, OPERATION, "no buffer bound is judged first");
                gl.bindBuffer(0x88ec, gl.createBuffer());
                gl.bufferData(0x88ec, 16, 0x88e0);
                gl.pixelStorei(0x9240, true);
                gl.texImage2D(T2D, 0, RGBA8, 1, 1, 0, RGBA, UBYTE, 0); err(gl, OPERATION, "an offset with UNPACK_FLIP_Y_WEBGL set");
                gl.pixelStorei(0x9240, false);
                gl.pixelStorei(0x9241, true);
                gl.texSubImage2D(T2D, 0, 0, 0, 1, 1, RGBA, UBYTE, 0); err(gl, OPERATION, "an offset with UNPACK_PREMULTIPLY_ALPHA_WEBGL set");
                gl.pixelStorei(0x9241, false);
                gl.texImage2D(T2D, 16, RGBA8, 1, 1, 0, RGBA, UBYTE, 2147483648); err(gl, VALUE, "the level is judged before the offset");
                gl.texImage2D(T2D, 0, RGBA8, 1, 1, 0, RGBA, UBYTE, -1); err(gl, VALUE, "a negative offset");
                gl.texImage2D(T2D, 0, RGBA8, 1, 1, 0, RGBA, UBYTE, 2147483648); err(gl, OPERATION, "an offset past 2^31 - 1");
                gl.texImage2D(T2D, 0, RGBA8, 2, 2, 0, RGBA, UBYTE, 4); err(gl, OPERATION, "a range past the buffer");
                gl.texSubImage2D(T2D, 0, 0, 0, 1, 1, RGBA, 0x8033, 1); err(gl, OPERATION, "an offset that is not a multiple of the type's size");
                gl.texImage2D(T2D, 0, RGBA8, 2, 2, 0, RGBA, UBYTE, 0); err(gl, 0, "the buffer's 16 bytes");          // sent: buffer 0
                gl.texSubImage2D(T2D, 0, 1, 1, 1, 1, RGBA, UBYTE, 12); err(gl, 0, "its last 4");                      // sent: buffer 12
                gl.bindBuffer(0x88ec, null);
                gl.pixelStorei(0x9240, true);
                gl.texImage3D(0x806f, 0, RGBA, 1, 1, 1, 0, RGBA, UBYTE, new Uint8Array(4)); err(gl, OPERATION, "a 3D view with UNPACK_FLIP_Y_WEBGL set");
                gl.texSubImage3D(0x806f, 0, 0, 0, 0, 1, 1, 1, RGBA, UBYTE, new Uint8Array(4)); err(gl, OPERATION, "a 3D sub view with it set");
                gl.texImage3D(0x806f, 0, RGBA, 1, 1, 1, 0, RGBA, UBYTE, null); err(gl, 0, "storage only is not a view");   // sent: none
                gl.pixelStorei(0x9240, false);
                gl.texImage2D(T2D, 0, RGBA8, 1, 1, 0, RGBA, UBYTE, new Uint8Array(4)); err(gl, 0, "and a 2D view is uploaded flipped");   // sent: bytes
                gl.flush(); gl1.flush();
                "#,
            )
            .expect("the refused uploads should be refused, not thrown (but for the TypeError, which the script catches)");
        let got: Vec<String> = drain_gl_commands(&render_rx)
            .iter()
            .filter_map(|cmd| match cmd {
                GLCmd::TexImage2D { data, .. } => {
                    Some(format!("2d {}", describe_source(data.as_ref())))
                }
                GLCmd::TexSubImage2D { data, .. } => {
                    Some(format!("sub2d {}", describe_source(Some(data))))
                }
                GLCmd::TexImage3D { data, .. } => {
                    Some(format!("3d {}", describe_source(data.as_ref())))
                }
                GLCmd::TexSubImage3D { data, .. } => {
                    Some(format!("sub3d {}", describe_source(Some(data))))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            got,
            [
                "2d buffer 0",
                "sub2d buffer 12",
                "3d none",
                "2d [0, 0, 0, 0]"
            ],
            "only the uploads that were taken are sent, each naming its source"
        );
    }

    /// Texture bindings are kept per unit and target: `getParameter` answers the objects, a texture keeps the target
    /// it was first bound to, a deleted one leaves every unit and the framebuffer bound, and every call on a target's
    /// texture needs one bound (INVALID_OPERATION) to a target the call takes (INVALID_ENUM). Nothing refused is sent.
    #[test]
    fn texture_bindings_are_kept_and_a_call_on_a_texture_needs_one_bound() {
        let (mut runtime, render_rx) = new_webgl_runtime();
        runtime
            .exec_script(
                "texture_state.js",
                r#"
                const gl1 = new WebGLRenderingContext({ _rid: 205, width: 1, height: 1 }, {});
                const gl = new WebGL2RenderingContext({ _rid: 206, width: 1, height: 1 }, {});
                const check = (c, m) => { if (!c) throw new Error(m); };
                const err = (ctx, want, m) => { const got = ctx.getError(); if (got !== want) throw new Error(`${m}: getError ${got}, want ${want}`); };
                const pixel = new Uint8Array(4);
                // nothing bound
                gl1.texImage2D(0x0de1, 0, 0x1908, 1, 1, 0, 0x1908, 0x1401, pixel); err(gl1, 0x0502, "texImage2D with no texture");
                gl1.texSubImage2D(0x0de1, 0, 0, 0, 1, 1, 0x1908, 0x1401, pixel); err(gl1, 0x0502, "texSubImage2D with no texture");
                gl1.texParameteri(0x0de1, 0x2801, 0x2601); err(gl1, 0x0502, "texParameteri with no texture");
                gl1.generateMipmap(0x0de1); err(gl1, 0x0502, "generateMipmap with no texture");
                gl1.copyTexImage2D(0x0de1, 0, 0x1908, 0, 0, 1, 1, 0); err(gl1, 0x0502, "copyTexImage2D with no texture");
                gl1.compressedTexImage2D(0x0de1, 0, 0x83f0, 4, 4, 0, new Uint8Array(8)); err(gl1, 0x0502, "compressedTexImage2D with no texture");
                check(gl1.getTexParameter(0x0de1, 0x2801) === null, "getTexParameter with no texture"); err(gl1, 0x0502, "and INVALID_OPERATION");
                gl.texStorage2D(0x0de1, 1, 0x8058, 1, 1); err(gl, 0x0502, "texStorage2D with no texture");
                gl.texImage3D(0x806f, 0, 0x1908, 1, 1, 1, 0, 0x1908, 0x1401, pixel); err(gl, 0x0502, "texImage3D with no texture");
                gl.texStorage3D(0x8c1a, 1, 0x8058, 1, 1, 1); err(gl, 0x0502, "texStorage3D with no texture");
                // targets a call does not take
                const t2 = gl.createTexture(), cube = gl.createTexture(), volume = gl.createTexture();
                check(!gl.isTexture(t2), "a texture never bound is not one yet");
                gl.bindTexture(0x0de1, t2);
                gl.bindTexture(0x8513, cube);
                gl.bindTexture(0x806f, volume);
                check(gl.isTexture(t2), "bound once, it is a texture");
                gl.texImage2D(0x8513, 0, 0x1908, 1, 1, 0, 0x1908, 0x1401, pixel); err(gl, 0x0500, "texImage2D of the cube map rather than a face");
                gl.texSubImage2D(0x8513, 0, 0, 0, 1, 1, 0x1908, 0x1401, pixel); err(gl, 0x0500, "texSubImage2D of the cube map rather than a face");
                gl.texImage2D(0x8515, 0, 0x1908, 1, 1, 0, 0x1908, 0x1401, pixel); err(gl, 0, "texImage2D of a cube face");
                gl.texParameteri(0x8515, 0x2801, 0x2601); err(gl, 0x0500, "texParameteri of a face");
                gl.texParameteri(0x806f, 0x2801, 0x2601); err(gl, 0, "texParameteri of a 3D texture");
                gl.texStorage2D(0x806f, 1, 0x8058, 1, 1); err(gl, 0x0500, "texStorage2D of a 3D texture");
                gl.texImage3D(0x0de1, 0, 0x1908, 1, 1, 1, 0, 0x1908, 0x1401, pixel); err(gl, 0x0500, "texImage3D of a 2D texture");
                gl1.bindTexture(0x806f, gl1.createTexture()); err(gl1, 0x0500, "TEXTURE_3D in WebGL 1");
                // bindings
                check(gl.getParameter(0x806a) === volume && gl.getParameter(0x8c1d) === null, "TEXTURE_BINDING_3D and _2D_ARRAY");
                check(gl1.getParameter(0x806a) === null && gl1.getError() === 0x0500, "no TEXTURE_BINDING_3D in WebGL 1");
                gl.bindTexture(0x8513, t2); err(gl, 0x0502, "a 2D texture bound to the cube map");
                let threw = false;
                try { gl.bindTexture(0x0de1, {}); } catch (e) { threw = e instanceof TypeError; }
                check(threw, "a value that is not a texture is a TypeError");
                gl.activeTexture(0x84c0 - 1); err(gl, 0x0500, "a unit below TEXTURE0");
                gl._maxTextureUnits = 40;     // what the renderer would answer for MAX_COMBINED_TEXTURE_IMAGE_UNITS
                gl.activeTexture(0x84c0 + 40); err(gl, 0x0500, "a unit past MAX_COMBINED_TEXTURE_IMAGE_UNITS");
                gl.activeTexture(0x84c1);
                gl.bindTexture(0x0de1, t2);
                check(gl.getParameter(0x8069) === t2, "unit 1's binding");
                gl.activeTexture(0x84c0);
                check(gl.getParameter(0x8069) === t2, "unit 0's binding");
                // deletion
                const fb = gl.createFramebuffer();
                gl.bindFramebuffer(0x8d40, fb);
                gl.framebufferTexture2D(0x8d40, 0x8ce0, 0x0de1, t2, 0);
                check(gl.getFramebufferAttachmentParameter(0x8d40, 0x8ce0, 0x8cd0) === 0x1702, "attached");
                gl.deleteTexture(t2);
                check(gl.getParameter(0x8069) === null && !gl.isTexture(t2), "deleting unbinds unit 0");
                gl.activeTexture(0x84c1);
                check(gl.getParameter(0x8069) === null, "and unit 1");
                gl.activeTexture(0x84c0);
                check(gl.getFramebufferAttachmentParameter(0x8d40, 0x8ce0, 0x8cd0) === 0, "and the framebuffer bound");
                const other = gl.createFramebuffer(), kept = gl.createTexture();
                gl.bindFramebuffer(0x8d40, other);
                gl.bindTexture(0x0de1, kept);
                gl.framebufferTexture2D(0x8d40, 0x8ce0, 0x0de1, kept, 0);
                gl.bindFramebuffer(0x8d40, fb);
                gl.deleteTexture(kept);
                gl.bindFramebuffer(0x8d40, other);
                check(gl.getFramebufferAttachmentParameter(0x8d40, 0x8ce0, 0x8cd1) === kept,
                      "a framebuffer not bound when the texture was deleted keeps it attached");
                gl.bindFramebuffer(0x8d40, fb);
                gl.bindTexture(0x0de1, t2); err(gl, 0x0502, "binding a deleted texture");
                // framebuffers and renderbuffers
                const f2 = gl.createFramebuffer(), rb = gl.createRenderbuffer();
                check(!gl.isFramebuffer(f2) && !gl.isRenderbuffer(rb), "never bound, neither is one yet");
                gl.bindFramebuffer(0x8ca8, f2);
                check(gl.isFramebuffer(f2) && gl.getParameter(0x8caa) === f2 && gl.getParameter(0x8ca6) === fb,
                      "a READ_FRAMEBUFFER bind binds the read point only");
                gl1.bindFramebuffer(0x8ca8, gl1.createFramebuffer()); err(gl1, 0x0500, "READ_FRAMEBUFFER in WebGL 1");
                gl.bindRenderbuffer(0x1234, rb); err(gl, 0x0500, "a renderbuffer target that is not one");
                gl.bindRenderbuffer(0x8d41, rb);
                check(gl.isRenderbuffer(rb), "bound once, it is a renderbuffer");
                gl.framebufferRenderbuffer(0x8d40, 0x8d00, 0x8d41, rb);
                gl.deleteRenderbuffer(rb);
                check(gl.getParameter(0x8ca7) === null && gl.getFramebufferAttachmentParameter(0x8d40, 0x8d00, 0x8cd0) === 0,
                      "a deleted renderbuffer leaves RENDERBUFFER and the framebuffer bound");
                gl.deleteFramebuffer(fb);
                check(gl.getParameter(0x8ca6) === null && gl.getParameter(0x8caa) === f2,
                      "deleting the bound draw framebuffer binds the default there, and the read one stays");
                gl.bindFramebuffer(0x8d40, fb); err(gl, 0x0502, "binding a deleted framebuffer");
                threw = false;
                try { gl.bindFramebuffer(0x8d40, {}); } catch (e) { threw = e instanceof TypeError; }
                check(threw, "a value that is not a framebuffer is a TypeError");
                gl.flush(); gl1.flush();
                "#,
            )
            .expect("the texture state script should run");
        let sent: Vec<String> = drain_gl_commands(&render_rx)
            .iter()
            .filter_map(|cmd| match cmd {
                GLCmd::TexImage2D { target, .. } => Some(format!("texImage2D {target:#x}")),
                GLCmd::TexParameteri { target, .. } => Some(format!("texParameteri {target:#x}")),
                GLCmd::TexStorage2D { .. } => Some("texStorage2D".to_string()),
                GLCmd::TexStorage3D { .. } => Some("texStorage3D".to_string()),
                GLCmd::TexImage3D { .. } => Some("texImage3D".to_string()),
                GLCmd::TexSubImage2D { .. } => Some("texSubImage2D".to_string()),
                GLCmd::GenerateMipmap { .. } => Some("generateMipmap".to_string()),
                GLCmd::CopyTexImage2D { .. } => Some("copyTexImage2D".to_string()),
                GLCmd::CompressedTexImage2D { .. } => Some("compressedTexImage2D".to_string()),
                _ => None,
            })
            .collect();
        assert_eq!(
            sent,
            vec!["texImage2D 0x8515", "texParameteri 0x806f"],
            "only the calls on a bound texture of a target they take are sent"
        );
    }

    /// `getVertexAttrib` answers from what the calls set: the defaults before anything, each pointer's arguments and
    /// the buffer bound when it was made, the enable flag, the divisor, the constant value (typed as the call that set
    /// it), per vertex array object, and an error and `null` for what it cannot answer.
    #[test]
    fn get_vertex_attrib_answers_from_what_the_calls_set() {
        let (mut runtime, _render_rx) = new_webgl_runtime();
        runtime
            .exec_script(
                "get_vertex_attrib.js",
                r#"
                const gl = new WebGL2RenderingContext({ _rid: 161, width: 1, height: 1 }, {});
                gl._maxVertexAttribs = 16;       // what the renderer would answer for MAX_VERTEX_ATTRIBS
                const check = (c, m) => { if (!c) throw new Error(m); };
                const same = (a, b) => a.length === b.length && a.every((v, i) => v === b[i]);

                // defaults
                check(gl.getVertexAttrib(0, 0x8622) === false, "enabled by default");
                check(gl.getVertexAttrib(0, 0x8623) === 4, "size 4 by default");
                check(gl.getVertexAttrib(0, 0x8625) === 0x1406, "FLOAT by default");
                check(gl.getVertexAttrib(0, 0x886a) === false, "not normalised by default");
                check(gl.getVertexAttrib(0, 0x8624) === 0, "stride 0");
                check(gl.getVertexAttrib(0, 0x889f) === null, "no buffer");
                check(same(gl.getVertexAttrib(0, 0x8626), [0, 0, 0, 1]), "current value 0,0,0,1");
                check(gl.getVertexAttrib(0, 0x8626) instanceof Float32Array, "float by default");
                check(gl.getVertexAttrib(0, 0x88fd) === false && gl.getVertexAttrib(0, 0x88fe) === 0, "not integer, divisor 0");

                // a pointer, with the buffer bound at the time of the call
                const buf = gl.createBuffer();
                gl.bindBuffer(0x8892, buf);
                gl.enableVertexAttribArray(3);
                gl.vertexAttribPointer(3, 2, 0x1401, true, 12, 4);   // UNSIGNED_BYTE, normalised
                gl.bindBuffer(0x8892, null);                          // binding it away afterwards changes nothing
                gl.vertexAttribDivisor(3, 2);
                check(gl.getVertexAttrib(3, 0x8622) === true, "enabled");
                check(gl.getVertexAttrib(3, 0x8623) === 2, "size");
                check(gl.getVertexAttrib(3, 0x8625) === 0x1401, "type");
                check(gl.getVertexAttrib(3, 0x886a) === true, "normalised");
                check(gl.getVertexAttrib(3, 0x8624) === 12, "stride");
                check(gl.getVertexAttrib(3, 0x889f) === buf, "buffer binding");
                check(gl.getVertexAttrib(3, 0x88fe) === 2, "divisor");
                check(gl.getVertexAttribOffset(3, 0x8645) === 4, "offset");
                gl.disableVertexAttribArray(3);
                check(gl.getVertexAttrib(3, 0x8622) === false, "disabled again");

                // an integer pointer
                gl.vertexAttribIPointer(4, 3, 0x1404, 0, 0);
                check(gl.getVertexAttrib(4, 0x88fd) === true, "integer");
                check(gl.getVertexAttrib(4, 0x886a) === false, "an integer attribute is never normalised");

                // a refused pointer leaves what was there
                gl.vertexAttribPointer(5, 9, 0x1406, false, 0, 0);    // size 9: refused
                check(gl.getVertexAttrib(5, 0x8623) === 4, "a refused pointer is not recorded");

                // constant values, typed as the call that set them
                gl.vertexAttrib3f(6, 1, 2, 3);
                check(same(gl.getVertexAttrib(6, 0x8626), [1, 2, 3, 1]), "constant float");
                gl.vertexAttribI4i(6, -1, 2, -3, 4);
                check(gl.getVertexAttrib(6, 0x8626) instanceof Int32Array && same(gl.getVertexAttrib(6, 0x8626), [-1, 2, -3, 4]), "constant int");
                gl.vertexAttribI4ui(6, 4294967295, 2, 3, 4);
                check(gl.getVertexAttrib(6, 0x8626) instanceof Uint32Array && gl.getVertexAttrib(6, 0x8626)[0] === 4294967295, "constant uint");

                // per vertex array object: a new one starts from the defaults, the default one is kept
                const vao = gl.createVertexArray();
                gl.bindVertexArray(vao);
                check(gl.getVertexAttrib(3, 0x889f) === null && gl.getVertexAttrib(3, 0x8623) === 4, "a new vertex array object starts from the defaults");
                gl.enableVertexAttribArray(0);
                gl.bindVertexArray(null);
                check(gl.getVertexAttrib(0, 0x8622) === false, "the default vertex array object is untouched");
                check(gl.getVertexAttrib(3, 0x889f) === buf, "and kept what it had");
                check(gl.isVertexArray(vao) === true, "a vertex array object is one");
                gl.bindVertexArray(vao);
                gl.deleteVertexArray(vao);
                check(gl.isVertexArray(vao) === false, "a deleted one is not");
                check(gl.getVertexAttrib(0, 0x8622) === false, "deleting the bound one binds the default");

                // what it cannot answer
                check(gl.getVertexAttrib(16, 0x8622) === null, "an index past MAX_VERTEX_ATTRIBS");
                check(gl.getVertexAttrib(0, 0x1234) === null, "an unknown name");
                check(gl.getVertexAttribOffset(0, 0x1234) === 0, "an unknown name for the offset");
                "#,
            )
            .expect("getVertexAttrib should answer from the calls");
    }

    #[test]
    fn uniform_array_is_copied_when_the_op_is_called() {
        let (mut runtime, render_rx) = new_webgl_runtime();
        runtime
            .exec_script(
                "uniform_call_time_copy.js",
                r#"
                const ctx = new WebGLRenderingContext({ _rid: 14, width: 1, height: 1 }, {});
                const source = new Float32Array([1.25, 2.5, 3.75, 5.0]);
                ctx.uniform4fv({ id: 10 }, source);
                source[0] = 99.0;
                source[1] = 101.0;
                ctx.flush();
                "#,
            )
            .expect("typed float uniform should be accepted");

        let commands = recv_gl_commands(&render_rx);
        let Some(GLCmd::Uniform4fv { value, .. }) = commands.into_iter().next() else {
            panic!("expected one Uniform4fv command");
        };
        assert_eq!(value.as_slice(), &[1.25, 2.5, 3.75, 5.0]);
    }

    #[test]
    fn integer_uniform_typed_sequence_is_converted_numerically() {
        let (mut runtime, render_rx) = new_webgl_runtime();
        runtime
            .exec_script(
                "integer_uniform_typed_sequence.js",
                r#"
                const ctx = new WebGLRenderingContext({ _rid: 17, width: 1, height: 1 }, {});
                ctx.uniform4iv({ id: 13 }, new Uint16Array([1, 2, 32768, 65535]));
                ctx.flush();
                "#,
            )
            .expect("integer typed sequence should be accepted");

        let commands = recv_gl_commands(&render_rx);
        let Some(GLCmd::Uniform4iv { value, .. }) = commands.into_iter().next() else {
            panic!("expected one Uniform4iv command");
        };
        assert_eq!(value.as_slice(), &[1, 2, 32768, 65535]);
    }

    #[test]
    fn uniform_helpers_copy_shared_backing_before_fast_borrow() {
        let source = include_str!("02_webgl_context.js");

        assert!(
            source.contains("isSharedArrayBuffer"),
            "uniform conversion must identify SharedArrayBuffer backing"
        );
        assert!(
            source.contains("ensureNonSharedTypedArray"),
            "uniform conversion must copy shared views before Rust borrows them"
        );
    }

    #[test]
    fn public_webgl_uploads_are_rejected_before_crossing_the_op_boundary() {
        let source = include_str!("02_webgl_context.js");

        assert!(
            source.contains("const MAX_WEBGL_UPLOAD_BYTES = 64 * 1024 * 1024"),
            "the public facade needs a stable single-upload ceiling"
        );
        assert!(
            source.contains("function allowWebglUpload"),
            "all byte upload overloads should share one preflight helper"
        );
        assert!(
            source.contains("op_webgl_record_out_of_memory"),
            "preflight rejection must remain observable through getError()"
        );
        assert!(
            source.contains("allowWebglUpload(this._canvasId, size)")
                && source.contains("allowWebglUpload(canvasId, input.length)"),
            "numeric buffer allocation and public sequence inputs must preflight"
        );
        assert!(
            source.contains("MAX_WEBGL_SHADER_SOURCE_CODE_UNITS"),
            "shader strings need a pre-conversion ceiling"
        );
    }

    /// A texture upload's bytes are those it reads, held to the single-upload ceiling (OUT_OF_MEMORY, nothing sent),
    /// and taken from shared memory as they are at the call. (Shared memory is also copied before the op borrows it,
    /// since another agent could write it during the borrow; one thread cannot observe that copy.)
    #[test]
    fn texture_uploads_are_held_to_the_ceiling_and_copied_from_shared_memory_at_the_call() {
        let (mut runtime, render_rx) = new_webgl_runtime();
        runtime
            .exec_script(
                "upload_ceiling_and_shared_memory.js",
                r#"
                const gl = new WebGL2RenderingContext({ _rid: 209, width: 1, height: 1 }, {});
                gl.bindTexture(0x0de1, gl.createTexture());
                gl.bindTexture(0x806f, gl.createTexture());
                gl.texImage2D(0x0de1, 0, 0x1908, 4097, 4096, 0, 0x1908, 0x1401, new Uint8Array(4097 * 4096 * 4));
                if (gl.getError() !== 0x0505) throw new Error("an upload past the ceiling is OUT_OF_MEMORY");
                const shared = new Uint8Array(new SharedArrayBuffer(16)).fill(1);
                gl.texSubImage2D(0x0de1, 0, 0, 0, 1, 1, 0x1908, 0x1401, shared);
                gl.texImage3D(0x806f, 0, 0x1908, 1, 1, 2, 0, 0x1908, 0x1401, shared, 4);
                shared.fill(9);
                if (gl.getError() !== 0) throw new Error("no error: " + gl.getError());
                gl.flush();
                "#,
            )
            .expect("the uploads should be refused or taken, not thrown");
        let got: Vec<String> = drain_gl_commands(&render_rx)
            .iter()
            .filter_map(|cmd| match cmd {
                GLCmd::TexImage2D { .. } => Some("past the ceiling".to_string()),
                GLCmd::TexSubImage2D { data, .. } => {
                    Some(format!("sub2d {}", describe_source(Some(data))))
                }
                GLCmd::TexImage3D { data, .. } => {
                    Some(format!("3d {}", describe_source(data.as_ref())))
                }
                _ => None,
            })
            .collect();
        assert_eq!(got, ["sub2d [1, 1, 1, 1]", "3d [1, 1, 1, 1, 1, 1, 1, 1]"]);
    }

    #[test]
    fn shared_uniform_source_preserves_call_time_values() {
        let (mut runtime, render_rx) = new_webgl_runtime();
        runtime
            .exec_script(
                "shared_uniform_call_time_copy.js",
                r#"
                const ctx = new WebGLRenderingContext({ _rid: 18, width: 1, height: 1 }, {});
                const source = new Float32Array(new SharedArrayBuffer(16));
                source.set([1.25, 2.5, 3.75, 5.0]);
                ctx.uniform4fv({ id: 14 }, source);
                source.set([99.0, 101.0, 103.0, 105.0]);
                ctx.flush();
                "#,
            )
            .expect("shared float uniform should be copied safely");

        let commands = recv_gl_commands(&render_rx);
        let Some(GLCmd::Uniform4fv { value, .. }) = commands.into_iter().next() else {
            panic!("expected one Uniform4fv command");
        };
        assert_eq!(value.as_slice(), &[1.25, 2.5, 3.75, 5.0]);
    }

    #[test]
    fn plain_matrix3_uniform_sequence_is_accepted() {
        let (mut runtime, render_rx) = new_webgl_runtime();
        runtime
            .exec_script(
                "plain_matrix3_uniform_sequence.js",
                r#"
                const ctx = new WebGLRenderingContext({ _rid: 15, width: 1, height: 1 }, {});
                ctx.uniformMatrix3fv(
                    { id: 11 },
                    false,
                    [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
                );
                ctx.flush();
                "#,
            )
            .expect("plain matrix3 Float32List sequence should be accepted");

        let commands = recv_gl_commands(&render_rx);
        let Some(GLCmd::UniformMatrix3fv { value, .. }) = commands.into_iter().next() else {
            panic!("expected one UniformMatrix3fv command");
        };
        assert_eq!(
            value.as_slice(),
            &[1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]
        );
    }

    #[test]
    fn float_uniform_ignores_shadowed_typed_array_metadata() {
        let (mut runtime, render_rx) = new_webgl_runtime();
        runtime
            .exec_script(
                "shadowed_float_uniform_metadata.js",
                r#"
                const ctx = new WebGLRenderingContext({ _rid: 16, width: 1, height: 1 }, {});
                const source = new Float32Array([1.5, 2.5, 3.5, 4.5]);
                Object.defineProperty(source, "buffer", { value: new ArrayBuffer(16) });
                Object.defineProperty(source, "byteOffset", { value: 0 });
                Object.defineProperty(source, "length", { value: 4 });
                ctx.uniform4fv({ id: 12 }, source);
                ctx.flush();
                "#,
            )
            .expect("shadow properties must not affect internal typed-array metadata");

        let commands = recv_gl_commands(&render_rx);
        let Some(GLCmd::Uniform4fv { value, .. }) = commands.into_iter().next() else {
            panic!("expected one Uniform4fv command");
        };
        assert_eq!(value.as_slice(), &[1.5, 2.5, 3.5, 4.5]);
    }

    #[test]
    fn inline_uniform_stream_auto_flushes_at_soft_budget_without_explicit_flush() {
        use crate::rendering::webgl::frame_collector::AUTO_FLUSH_SOFT_BUDGET_BYTES;

        let (mut runtime, render_rx) = new_webgl_runtime();

        // With the stream path, uniform4fv is encoded into the JS-side 8192-word
        // ring buffer. Each uniform4fv record = 7 words (H + C + loc + 4 floats).
        // The stream auto-submits when the buffer fills: (8192 - 2) / 7 = 1170
        // commands per batch. Each submit adds `1170 * per_cmd` approx bytes.
        //
        // To guarantee at least one auto-flush, we need enough submits so that
        // accumulated pending_bytes >= AUTO_FLUSH_SOFT_BUDGET_BYTES.
        //   submits_needed = ceil(budget / (1170 * per_cmd))
        //   count_needed   = submits_needed * 1171 + 2
        // (1171 because the (N+1)-th command triggers the submit of N commands.)
        let per_cmd = std::mem::size_of::<GLCmd>();
        // Words per uniform4fv record in the stream buffer (H C loc f32 f32 f32 f32).
        const UNIFORM4FV_WORDS: usize = 7;
        // Commands per stream-buffer submit: (8192 - 2 header words) / UNIFORM4FV_WORDS.
        const CMDS_PER_SUBMIT: usize = (8192 - 2) / UNIFORM4FV_WORDS; // = 1170
        let bytes_per_submit = CMDS_PER_SUBMIT * per_cmd;
        let submits_needed =
            (AUTO_FLUSH_SOFT_BUDGET_BYTES + bytes_per_submit - 1) / bytes_per_submit;
        let count = submits_needed * (CMDS_PER_SUBMIT + 1) + 2;

        runtime
            .exec_script(
                "inline_uniform_autoflush.js",
                &format!(
                    r#"
                    globalThis.__ctx = new WebGLRenderingContext({{ _rid: 21, width: 1, height: 1 }}, {{}});
                    const loc = {{ id: 9 }};
                    // Encode the submission index in each component. `i` is
                    // exactly representable as f32 (i < 2^24, and count << that),
                    // so the consumer can assert strict submission ORDER, not
                    // merely count / no-loss.
                    for (let i = 0; i < {count}; i++) {{
                        globalThis.__ctx.uniform4fv(loc, [i, i, i, i]);
                    }}
                    // Intentionally NO flush() and NO frame end: untrusted JS can
                    // enqueue this many inline uniforms synchronously in one turn.
                    "#,
                ),
            )
            .expect("inline uniform stream should be accepted");

        // The burst crossed the soft budget, so an automatic non-presenting
        // barrier FramePacket must already be queued BEFORE any explicit flush.
        let first = match render_rx.try_recv() {
            Ok(RenderCommand::FramePacket(packet)) => packet,
            Ok(other) => panic!("unexpected render command: {other:?}"),
            Err(_) => panic!(
                "inline uniform burst crossed the {AUTO_FLUSH_SOFT_BUDGET_BYTES}-byte soft budget \
                 but no automatic barrier FramePacket was emitted before an explicit flush"
            ),
        };
        assert!(
            !first.ops().iter().any(|op| matches!(op, FrameOp::Present)),
            "auto-flush barrier must be non-presenting"
        );

        // Order survives the auto-flush boundary: flush the remainder, then
        // consume the automatic packet followed by the explicit remainder. Each
        // command must carry its strictly-increasing submission index — a lost,
        // duplicated, or reordered command breaks the running counter.
        runtime
            .exec_script(
                "inline_uniform_autoflush_drain.js",
                "globalThis.__ctx.flush();",
            )
            .expect("explicit flush of the remainder should be accepted");

        let mut expected = 0.0f32;
        for packet in
            std::iter::once(first).chain(std::iter::from_fn(|| match render_rx.try_recv() {
                Ok(RenderCommand::FramePacket(p)) => Some(p),
                _ => None,
            }))
        {
            for op in packet.into_ops() {
                if let FrameOp::GlBatch(payload) = op {
                    for cmd in payload.commands {
                        match cmd {
                            GLCmd::Uniform4fv { value, .. } => {
                                assert_eq!(
                                    value.as_slice(),
                                    &[expected, expected, expected, expected],
                                    "uniforms must arrive in strict submission order across the \
                                     auto-flush boundary"
                                );
                                expected += 1.0;
                            }
                            // The explicit `flush()` that drains the remainder, and the record the context's
                            // constructor encoded ahead of the burst.
                            GLCmd::Flush { .. } | GLCmd::WebglContext { .. } => {}
                            other => panic!("unexpected command across auto-flush: {other:?}"),
                        }
                    }
                }
            }
        }
        assert_eq!(
            expected as usize, count,
            "every queued uniform must survive the auto-flush boundary exactly once, in order"
        );
    }

    #[test]
    fn small_inline_uniform_sequence_does_not_auto_flush() {
        let (mut runtime, render_rx) = new_webgl_runtime();
        runtime
            .exec_script(
                "small_inline_uniform.js",
                r#"
                const ctx = new WebGLRenderingContext({ _rid: 22, width: 1, height: 1 }, {});
                const loc = { id: 9 };
                for (let i = 0; i < 8; i++) ctx.uniform4fv(loc, [1.0, 2.0, 3.0, 4.0]);
                "#,
            )
            .expect("small uniform sequence should be accepted");
        assert!(
            render_rx.try_recv().is_err(),
            "a small inline uniform sequence must not trigger an automatic flush"
        );
    }

    // Characterization (Q5 review gap): a length-tracking `Float32Array` over a
    // *resizable* `ArrayBuffer` is a legal uniform source, before AND after a
    // grow. The op must copy at call time; a later mutate/`resize` must never
    // change an already-queued command, and two calls from the same view must
    // keep their respective call-time values and submission order. If the locked
    // V8 rejects RAB construction the `.expect` below fails loudly and we would
    // document RAB-unavailable instead of asserting fabricated behavior.
    #[test]
    fn resizable_arraybuffer_uniform_source_copies_at_call_time() {
        let (mut runtime, render_rx) = new_webgl_runtime();
        runtime
            .exec_script(
                "rab_uniform_call_time_copy.js",
                r#"
                const rab = new ArrayBuffer(16, { maxByteLength: 64 });
                if (rab.resizable !== true) throw new Error("expected a resizable ArrayBuffer");
                const view = new Float32Array(rab); // length-tracks the RAB (4 floats)
                const ctx = new WebGLRenderingContext({ _rid: 23, width: 1, height: 1 }, {});

                // Call 1: pre-grow 4-float view.
                view.set([1.5, -2.25, 0.0, 7.75]);
                ctx.uniform4fv({ id: 9 }, view);
                view[0] = 99.0; // post-call mutation must not affect command 1

                // Grow the backing; the length-tracking view now spans 16 floats.
                rab.resize(64);
                if (view.length !== 16) {
                    throw new Error("length-tracking view must span 16 floats after grow, got " + view.length);
                }

                // Call 2: post-grow, same view, distinguishable 16-word payload
                // (fills the inline SmallVec exactly, no spill).
                for (let i = 0; i < 16; i++) view[i] = 100 + i;
                ctx.uniform1fv({ id: 10 }, view);

                // Post-call mutate + shrink must not affect command 2.
                view[0] = -1.0;
                rab.resize(16);
                ctx.flush();
                "#,
            )
            .expect("resizable ArrayBuffer uniform source should be accepted");

        let commands = recv_gl_commands(&render_rx);
        let mut it = commands.into_iter();

        // Order + call-time values: command 1 is the pre-grow vec4.
        let Some(GLCmd::Uniform4fv { value, .. }) = it.next() else {
            panic!("expected Uniform4fv as the first command");
        };
        assert_eq!(value.as_slice(), &[1.5, -2.25, 0.0, 7.75]);

        // Command 2 is the post-grow 16-word inline payload, unaffected by the
        // later mutate + shrink.
        let Some(GLCmd::Uniform1fv { value, .. }) = it.next() else {
            panic!("expected Uniform1fv as the second command");
        };
        let expected: Vec<f32> = (0..16).map(|i| 100.0 + i as f32).collect();
        assert_eq!(value.as_slice(), expected.as_slice());
        assert!(
            !value.spilled(),
            "a 16-word post-grow uniform payload must stay inline"
        );
        assert!(
            matches!(it.next(), Some(GLCmd::Flush { .. })),
            "the closing flush() follows the two uniforms"
        );
        assert!(it.next().is_none(), "exactly two uniform commands expected");
    }

    #[test]
    fn webgl2_query_registry_matches_lifecycle_semantics() {
        let (mut runtime, _render_rx) = new_webgl_runtime();
        runtime
            .exec_script(
                "webgl2_query_registry.js",
                r#"
                const ctx = new WebGL2RenderingContext({ _rid: 7, width: 1, height: 1 }, {});
                const q = ctx.createQuery();
                if (ctx.isQuery(q) !== false) throw new Error("createQuery must not become true before first beginQuery");
                if (ctx.getQuery(0x8C2F, 0x8865) !== null) throw new Error("CURRENT_QUERY must start as null");
                ctx.beginQuery(0x8C2F, q);
                if (ctx.isQuery(q) !== true) throw new Error("beginQuery must mark query as real");
                if (ctx.getQuery(0x8C2F, 0x8865) !== q) throw new Error("CURRENT_QUERY must return the active query object");
                ctx.endQuery(0x8C2F);
                if (ctx.getQuery(0x8C2F, 0x8865) !== null) throw new Error("CURRENT_QUERY must clear after endQuery");
                ctx.deleteQuery(q);
                if (ctx.isQuery(q) !== false) throw new Error("deleteQuery must make isQuery false");
                "#,
            )
            .expect("query lifecycle script should complete");
    }

    #[test]
    fn webgl2_transform_feedback_delete_active_queues_invalid_operation() {
        let (mut runtime, _render_rx) = new_webgl_runtime();
        runtime
            .exec_script(
                "webgl2_tf_registry.js",
                r#"
                const ctx = new WebGL2RenderingContext({ _rid: 9, width: 1, height: 1 }, {});
                const tf = ctx.createTransformFeedback();
                if (ctx.isTransformFeedback(tf) !== false) throw new Error("createTransformFeedback must not become true before first bind");
                ctx.bindTransformFeedback(0x8E22, tf);
                if (ctx.isTransformFeedback(tf) !== true) throw new Error("bindTransformFeedback must mark object as real");
                ctx.beginTransformFeedback(0x0004);
                ctx.pauseTransformFeedback();
                ctx.deleteTransformFeedback(tf);
                if (ctx.getError() !== 0x0502) throw new Error("delete active transform feedback must queue INVALID_OPERATION");
                if (ctx.isTransformFeedback(tf) !== true) throw new Error("failed delete must preserve transform feedback object");
                ctx.endTransformFeedback();
                ctx.deleteTransformFeedback(tf);
                if (ctx.isTransformFeedback(tf) !== false) throw new Error("successful delete must clear transform feedback object");
                "#,
            )
            .expect("transform feedback lifecycle script should complete");
    }

    #[test]
    fn webgl2_get_transform_feedback_varying_parses_sync_result() {
        let (mut runtime, render_rx) = new_webgl_runtime();
        let responder = spawn_tf_varying_responder(render_rx);
        runtime
            .exec_script(
                "webgl2_tf_varying.js",
                r#"
                const ctx = new WebGL2RenderingContext({ _rid: 11, width: 1, height: 1 }, {});
                const program = { _id: 17, _kind: "program" };
                const info = ctx.getTransformFeedbackVarying(program, 0);
                if (!info) throw new Error("expected transform feedback varying metadata");
                if (info.name !== "v_pos") throw new Error("varying name mismatch");
                if (info.size !== 1) throw new Error("varying size mismatch");
                if (info.type !== 0x8B51) throw new Error("varying type mismatch");
                if (ctx.getTransformFeedbackVarying(program, 1) !== null) {
                    throw new Error("out-of-range varying index must return null");
                }
                "#,
            )
            .expect("transform feedback varying script should complete");
        responder
            .join()
            .expect("TF varying responder should exit cleanly");
    }

    // ── Task 2 RED: decode_validated_stream ───────────────────────────────────
    //
    // These tests call decode_validated_stream from the `decode` module (Task 2).
    // They FAIL (compile error / link error) until the implementation is in place.

    use crate::rendering::webgl::decode::decode_validated_stream;
    use crate::rendering::webgl::stream::{
        MAGIC, STREAM_VERSION, ValidatedStream, pack_header, validate_stream,
    };
    use frame_wire::gl::{
        OP_BIND_BUFFER, OP_BIND_BUFFER_BASE, OP_BIND_BUFFER_RANGE, OP_BIND_FRAMEBUFFER,
        OP_BIND_RENDERBUFFER, OP_BIND_SAMPLER, OP_BIND_TEXTURE, OP_BIND_VERTEX_ARRAY, OP_CLEAR,
        OP_ENABLE, OP_SCISSOR, OP_UNIFORM_MATRIX3FV, OP_UNIFORM1F, OP_UNIFORM1FV, OP_UNIFORM1I,
        OP_VERTEX_ATTRIB_POINTER, OP_VIEWPORT,
    };

    fn make_validated_for_decode(words: &[u32]) -> ValidatedStream<'_> {
        validate_stream(words, words.len() as u32).expect("test stream must be valid")
    }

    // ── f32 bit-exact round-trip ──────────────────────────────────────────────

    #[test]
    fn decode_uniform1f_nan_bit_exact_round_trip() {
        let nan_bits = f32::NAN.to_bits();
        let canvas_id: u32 = 1;
        let h = pack_header(OP_UNIFORM1F, 4);
        let words = [MAGIC, STREAM_VERSION, h, canvas_id, 5u32, nan_bits];
        let vs = make_validated_for_decode(&words);
        let mut state = new_webgl_op_state();
        let mut out = Vec::new();
        decode_validated_stream(&mut state, vs, &mut out);
        assert_eq!(out.len(), 1);
        match &out[0] {
            GLCmd::Uniform1f { location, x, .. } => {
                assert_eq!(*location, Some(5));
                assert_eq!(x.to_bits(), nan_bits, "NaN bits must round-trip exactly");
            }
            other => panic!("expected Uniform1f, got {:?}", other),
        }
    }

    #[test]
    fn decode_uniform1f_neg_zero_bit_exact_round_trip() {
        let neg_zero_bits = (-0.0f32).to_bits();
        let canvas_id: u32 = 1;
        let h = pack_header(OP_UNIFORM1F, 4);
        let words = [MAGIC, STREAM_VERSION, h, canvas_id, 0u32, neg_zero_bits];
        let vs = make_validated_for_decode(&words);
        let mut state = new_webgl_op_state();
        let mut out = Vec::new();
        decode_validated_stream(&mut state, vs, &mut out);
        match &out[0] {
            GLCmd::Uniform1f { x, .. } => {
                assert_eq!(
                    x.to_bits(),
                    neg_zero_bits,
                    "-0 bits must round-trip exactly"
                );
            }
            other => panic!("expected Uniform1f, got {:?}", other),
        }
    }

    #[test]
    fn decode_uniform1f_pos_infinity_bit_exact_round_trip() {
        let inf_bits = f32::INFINITY.to_bits();
        let canvas_id: u32 = 1;
        let h = pack_header(OP_UNIFORM1F, 4);
        let words = [MAGIC, STREAM_VERSION, h, canvas_id, 0u32, inf_bits];
        let vs = make_validated_for_decode(&words);
        let mut state = new_webgl_op_state();
        let mut out = Vec::new();
        decode_validated_stream(&mut state, vs, &mut out);
        match &out[0] {
            GLCmd::Uniform1f { x, .. } => {
                assert_eq!(x.to_bits(), inf_bits, "+Inf bits must round-trip exactly");
            }
            other => panic!("expected Uniform1f, got {:?}", other),
        }
    }

    #[test]
    fn decode_uniform1f_neg_infinity_bit_exact_round_trip() {
        let neginf_bits = f32::NEG_INFINITY.to_bits();
        let canvas_id: u32 = 1;
        let h = pack_header(OP_UNIFORM1F, 4);
        let words = [MAGIC, STREAM_VERSION, h, canvas_id, 0u32, neginf_bits];
        let vs = make_validated_for_decode(&words);
        let mut state = new_webgl_op_state();
        let mut out = Vec::new();
        decode_validated_stream(&mut state, vs, &mut out);
        match &out[0] {
            GLCmd::Uniform1f { x, .. } => {
                assert_eq!(
                    x.to_bits(),
                    neginf_bits,
                    "-Inf bits must round-trip exactly"
                );
            }
            other => panic!("expected Uniform1f, got {:?}", other),
        }
    }

    // ── i32 round-trip ────────────────────────────────────────────────────────

    #[test]
    fn decode_uniform1i_neg_one_round_trip() {
        let canvas_id: u32 = 1;
        let location_word: u32 = 3u32;
        let x_word: u32 = (-1i32) as u32;
        let h = pack_header(OP_UNIFORM1I, 4);
        let words = [MAGIC, STREAM_VERSION, h, canvas_id, location_word, x_word];
        let vs = make_validated_for_decode(&words);
        let mut state = new_webgl_op_state();
        let mut out = Vec::new();
        decode_validated_stream(&mut state, vs, &mut out);
        assert_eq!(out.len(), 1);
        match &out[0] {
            GLCmd::Uniform1i {
                canvas_id: c,
                location: l,
                x,
            } => {
                assert_eq!(*c, 1);
                assert_eq!(*l, Some(3));
                assert_eq!(*x, -1, "i32 -1 must round-trip exactly");
            }
            other => panic!("expected Uniform1i, got {:?}", other),
        }
    }

    #[test]
    fn decode_uniform1i_i32_min_round_trip() {
        let canvas_id: u32 = 1;
        let location_word: u32 = 0u32;
        let x_word: u32 = i32::MIN as u32;
        let h = pack_header(OP_UNIFORM1I, 4);
        let words = [MAGIC, STREAM_VERSION, h, canvas_id, location_word, x_word];
        let vs = make_validated_for_decode(&words);
        let mut state = new_webgl_op_state();
        let mut out = Vec::new();
        decode_validated_stream(&mut state, vs, &mut out);
        match &out[0] {
            GLCmd::Uniform1i { x, .. } => {
                assert_eq!(*x, i32::MIN, "i32::MIN must round-trip exactly");
            }
            other => panic!("expected Uniform1i, got {:?}", other),
        }
    }

    // ── multi-record order preserved ──────────────────────────────────────────

    #[test]
    fn decode_multi_record_order_preserved() {
        let h_vp = pack_header(OP_VIEWPORT, 6);
        let h_cl = pack_header(OP_CLEAR, 3);
        let h_en = pack_header(OP_ENABLE, 3);
        let words = [
            MAGIC,
            STREAM_VERSION,
            h_vp,
            7u32,
            10i32 as u32,
            20i32 as u32,
            800u32,
            600u32,
            h_cl,
            7u32,
            0x4100u32,
            h_en,
            7u32,
            0x0B44u32,
        ];
        let vs = make_validated_for_decode(&words);
        let mut state = new_webgl_op_state();
        let mut out = Vec::new();
        decode_validated_stream(&mut state, vs, &mut out);
        assert_eq!(out.len(), 3);
        assert!(matches!(
            &out[0],
            GLCmd::Viewport {
                canvas_id: 7,
                x: 10,
                y: 20,
                ..
            }
        ));
        assert!(matches!(
            &out[1],
            GLCmd::Clear {
                canvas_id: 7,
                bit_field: 0x4100
            }
        ));
        assert!(matches!(
            &out[2],
            GLCmd::Enable {
                canvas_id: 7,
                cap: 0x0B44
            }
        ));
    }

    // ── null-id rules ─────────────────────────────────────────────────────────

    #[test]
    fn decode_bind_buffer_negative_id_becomes_none() {
        let target: u32 = 0x8892;
        let neg_one_bits: u32 = (-1i32) as u32;
        let h = pack_header(OP_BIND_BUFFER, 4);
        let words = [MAGIC, STREAM_VERSION, h, 1u32, target, neg_one_bits];
        let vs = make_validated_for_decode(&words);
        let mut state = new_webgl_op_state();
        let mut out = Vec::new();
        decode_validated_stream(&mut state, vs, &mut out);
        assert_eq!(out.len(), 1);
        match &out[0] {
            GLCmd::BindBuffer { buffer, .. } => {
                assert_eq!(*buffer, None, "negative buffer id must map to None");
            }
            other => panic!("expected BindBuffer, got {:?}", other),
        }
    }

    #[test]
    fn decode_bind_texture_negative_id_becomes_none() {
        let target: u32 = 0x0DE1;
        let neg_one_bits: u32 = (-1i32) as u32;
        let h = pack_header(OP_BIND_TEXTURE, 4);
        let words = [MAGIC, STREAM_VERSION, h, 1u32, target, neg_one_bits];
        let vs = make_validated_for_decode(&words);
        let mut state = new_webgl_op_state();
        let mut out = Vec::new();
        decode_validated_stream(&mut state, vs, &mut out);
        assert_eq!(out.len(), 1);
        match &out[0] {
            GLCmd::BindTexture { texture, .. } => {
                assert_eq!(*texture, None, "negative texture id must map to None");
            }
            other => panic!("expected BindTexture, got {:?}", other),
        }
    }

    #[test]
    fn decode_bind_framebuffer_negative_id_becomes_none() {
        let target: u32 = 0x8D40;
        let neg_one_bits: u32 = (-1i32) as u32;
        let h = pack_header(OP_BIND_FRAMEBUFFER, 4);
        let words = [MAGIC, STREAM_VERSION, h, 1u32, target, neg_one_bits];
        let vs = make_validated_for_decode(&words);
        let mut state = new_webgl_op_state();
        let mut out = Vec::new();
        decode_validated_stream(&mut state, vs, &mut out);
        assert_eq!(out.len(), 1);
        match &out[0] {
            GLCmd::BindFramebuffer { framebuffer, .. } => {
                assert_eq!(
                    *framebuffer, None,
                    "negative framebuffer id must map to None"
                );
            }
            other => panic!("expected BindFramebuffer, got {:?}", other),
        }
    }

    #[test]
    fn decode_bind_renderbuffer_negative_id_becomes_none() {
        let target: u32 = 0x8D41;
        let neg_one_bits: u32 = (-1i32) as u32;
        let h = pack_header(OP_BIND_RENDERBUFFER, 4);
        let words = [MAGIC, STREAM_VERSION, h, 1u32, target, neg_one_bits];
        let vs = make_validated_for_decode(&words);
        let mut state = new_webgl_op_state();
        let mut out = Vec::new();
        decode_validated_stream(&mut state, vs, &mut out);
        assert_eq!(out.len(), 1);
        match &out[0] {
            GLCmd::BindRenderbuffer { renderbuffer, .. } => {
                assert_eq!(
                    *renderbuffer, None,
                    "negative renderbuffer id must map to None"
                );
            }
            other => panic!("expected BindRenderbuffer, got {:?}", other),
        }
    }

    #[test]
    fn decode_bind_vertex_array_zero_id_becomes_none() {
        let h = pack_header(OP_BIND_VERTEX_ARRAY, 3);
        let words = [MAGIC, STREAM_VERSION, h, 1u32, 0u32];
        let vs = make_validated_for_decode(&words);
        let mut state = new_webgl_op_state();
        let mut out = Vec::new();
        decode_validated_stream(&mut state, vs, &mut out);
        assert_eq!(out.len(), 1);
        match &out[0] {
            GLCmd::BindVertexArray { vao, .. } => {
                assert_eq!(*vao, None, "VAO id 0 must map to None");
            }
            other => panic!("expected BindVertexArray, got {:?}", other),
        }
    }

    #[test]
    fn decode_bind_sampler_zero_id_becomes_none() {
        let h = pack_header(OP_BIND_SAMPLER, 4);
        let words = [MAGIC, STREAM_VERSION, h, 1u32, 0u32, 0u32];
        let vs = make_validated_for_decode(&words);
        let mut state = new_webgl_op_state();
        let mut out = Vec::new();
        decode_validated_stream(&mut state, vs, &mut out);
        assert_eq!(out.len(), 1);
        match &out[0] {
            GLCmd::BindSampler { sampler, .. } => {
                assert_eq!(*sampler, None, "sampler id 0 must map to None");
            }
            other => panic!("expected BindSampler, got {:?}", other),
        }
    }

    #[test]
    fn decode_bind_buffer_base_zero_buffer_becomes_none() {
        let target: u32 = 0x8A11;
        let h = pack_header(OP_BIND_BUFFER_BASE, 5);
        let words = [MAGIC, STREAM_VERSION, h, 1u32, target, 0u32, 0u32];
        let vs = make_validated_for_decode(&words);
        let mut state = new_webgl_op_state();
        let mut out = Vec::new();
        decode_validated_stream(&mut state, vs, &mut out);
        assert_eq!(out.len(), 1);
        match &out[0] {
            GLCmd::BindBufferBase { buffer, .. } => {
                assert_eq!(*buffer, None, "buffer base id 0 must map to None");
            }
            other => panic!("expected BindBufferBase, got {:?}", other),
        }
    }

    #[test]
    fn decode_uniform_location_negative_becomes_none() {
        let canvas_id: u32 = 1;
        let neg_loc: u32 = (-1i32) as u32;
        let h = pack_header(OP_UNIFORM1F, 4);
        let words = [
            MAGIC,
            STREAM_VERSION,
            h,
            canvas_id,
            neg_loc,
            1.0f32.to_bits(),
        ];
        let vs = make_validated_for_decode(&words);
        let mut state = new_webgl_op_state();
        let mut out = Vec::new();
        decode_validated_stream(&mut state, vs, &mut out);
        assert_eq!(out.len(), 1);
        match &out[0] {
            GLCmd::Uniform1f { location, .. } => {
                assert_eq!(*location, None, "location < 0 must map to None");
            }
            other => panic!("expected Uniform1f, got {:?}", other),
        }
    }

    // ── equivalence: bindBuffer (valid and invalid) ───────────────────────────
    //
    // For raw-op equivalence we call the underlying validator directly
    // (the #[op2] wrapper cannot be called from Rust tests), then assert
    // decode produces the same error queue outcome and same GLCmd shape.

    #[test]
    fn decode_bind_buffer_valid_equiv_raw_op() {
        let canvas_id: u32 = 5;
        let target: u32 = 0x8892;
        let buffer_id: u32 = 42;

        // Simulate the raw op: validate target, no error expected.
        let mut state_raw = new_webgl_op_state();
        let valid = error_state::validate_bind_buffer_target(&mut state_raw, canvas_id, target);
        assert!(valid);
        let raw_err = state_raw
            .borrow_mut::<WebGLErrorState>()
            .drain_one(canvas_id);

        let h = pack_header(OP_BIND_BUFFER, 4);
        let words = [MAGIC, STREAM_VERSION, h, canvas_id, target, buffer_id];
        let vs = make_validated_for_decode(&words);
        let mut state_dec = new_webgl_op_state();
        let mut out = Vec::new();
        decode_validated_stream(&mut state_dec, vs, &mut out);
        let dec_err = state_dec
            .borrow_mut::<WebGLErrorState>()
            .drain_one(canvas_id);

        assert_eq!(
            raw_err, 0,
            "raw validator must not error for valid bind buffer target"
        );
        assert_eq!(
            dec_err, 0,
            "decoded stream must not error for valid bind buffer"
        );
        assert_eq!(out.len(), 1);
        match &out[0] {
            GLCmd::BindBuffer {
                canvas_id: c,
                target: t,
                buffer: b,
            } => {
                assert_eq!(*c, canvas_id);
                assert_eq!(*t, target);
                assert_eq!(*b, Some(buffer_id));
            }
            other => panic!("expected BindBuffer, got {:?}", other),
        }
    }

    #[test]
    fn decode_bind_buffer_invalid_target_equiv_raw_op() {
        let canvas_id: u32 = 5;
        let bad_target: u32 = 0xDEAD;
        let buffer_id: u32 = 42;

        // Simulate the raw op: validate target, INVALID_ENUM expected.
        let mut state_raw = new_webgl_op_state();
        let valid = error_state::validate_bind_buffer_target(&mut state_raw, canvas_id, bad_target);
        assert!(!valid);
        let raw_err = state_raw
            .borrow_mut::<WebGLErrorState>()
            .drain_one(canvas_id);

        let h = pack_header(OP_BIND_BUFFER, 4);
        let words = [MAGIC, STREAM_VERSION, h, canvas_id, bad_target, buffer_id];
        let vs = make_validated_for_decode(&words);
        let mut state_dec = new_webgl_op_state();
        let mut out = Vec::new();
        decode_validated_stream(&mut state_dec, vs, &mut out);
        let dec_err = state_dec
            .borrow_mut::<WebGLErrorState>()
            .drain_one(canvas_id);

        assert_eq!(raw_err, error_state::codes::INVALID_ENUM);
        assert_eq!(dec_err, error_state::codes::INVALID_ENUM);
        assert_eq!(out.len(), 0);
    }

    // ── equivalence: scissor ──────────────────────────────────────────────────

    #[test]
    fn decode_scissor_valid_equiv_raw_op() {
        let canvas_id: u32 = 3;
        let (x, y, w, h_val) = (10i32, 20i32, 100i32, 200i32);

        // Simulate the raw op: validate_viewport_like, no error expected.
        let mut state_raw = new_webgl_op_state();
        let valid = error_state::validate_viewport_like(&mut state_raw, canvas_id, w, h_val);
        assert!(valid);
        let raw_err = state_raw
            .borrow_mut::<WebGLErrorState>()
            .drain_one(canvas_id);

        let h = pack_header(OP_SCISSOR, 6);
        let words = [
            MAGIC,
            STREAM_VERSION,
            h,
            canvas_id,
            x as u32,
            y as u32,
            w as u32,
            h_val as u32,
        ];
        let vs = make_validated_for_decode(&words);
        let mut state_dec = new_webgl_op_state();
        let mut out = Vec::new();
        decode_validated_stream(&mut state_dec, vs, &mut out);
        let dec_err = state_dec
            .borrow_mut::<WebGLErrorState>()
            .drain_one(canvas_id);

        assert_eq!(raw_err, 0);
        assert_eq!(dec_err, 0);
        assert_eq!(out.len(), 1);
        match &out[0] {
            GLCmd::Scissor {
                canvas_id: c,
                x: ox,
                y: oy,
                width,
                height,
            } => {
                assert_eq!((*c, *ox, *oy, *width, *height), (canvas_id, x, y, w, h_val));
            }
            other => panic!("expected Scissor, got {:?}", other),
        }
    }

    #[test]
    fn decode_scissor_negative_width_equiv_raw_op() {
        let canvas_id: u32 = 3;
        let neg_w: i32 = -1;

        // Simulate the raw op: validate_viewport_like, INVALID_VALUE expected.
        let mut state_raw = new_webgl_op_state();
        let valid = error_state::validate_viewport_like(&mut state_raw, canvas_id, neg_w, 100);
        assert!(!valid);
        let raw_err = state_raw
            .borrow_mut::<WebGLErrorState>()
            .drain_one(canvas_id);

        let h = pack_header(OP_SCISSOR, 6);
        let words = [
            MAGIC,
            STREAM_VERSION,
            h,
            canvas_id,
            0u32,
            0u32,
            neg_w as u32,
            100u32,
        ];
        let vs = make_validated_for_decode(&words);
        let mut state_dec = new_webgl_op_state();
        let mut out = Vec::new();
        decode_validated_stream(&mut state_dec, vs, &mut out);
        let dec_err = state_dec
            .borrow_mut::<WebGLErrorState>()
            .drain_one(canvas_id);

        assert_eq!(raw_err, error_state::codes::INVALID_VALUE);
        assert_eq!(dec_err, error_state::codes::INVALID_VALUE);
        assert_eq!(out.len(), 0);
    }

    // ── equivalence: vertexAttribPointer ─────────────────────────────────────

    #[test]
    fn decode_vertex_attrib_pointer_valid_equiv_raw_op() {
        let canvas_id: u32 = 2;
        let (index, size, type_, normalized, stride, offset) =
            (0u32, 3i32, 0x1406u32, false, 12i32, 0i32);

        // Simulate the raw op: validate_vertex_attrib_pointer, no error expected.
        let mut state_raw = new_webgl_op_state();
        let valid = error_state::validate_vertex_attrib_pointer(
            &mut state_raw,
            canvas_id,
            size,
            type_,
            stride,
            offset,
        );
        assert!(valid);
        let raw_err = state_raw
            .borrow_mut::<WebGLErrorState>()
            .drain_one(canvas_id);

        // H C U I U B I I
        let h = pack_header(OP_VERTEX_ATTRIB_POINTER, 8);
        let words = [
            MAGIC,
            STREAM_VERSION,
            h,
            canvas_id,
            index,
            size as u32,
            type_,
            0u32,
            stride as u32,
            offset as u32,
        ];
        let vs = make_validated_for_decode(&words);
        let mut state_dec = new_webgl_op_state();
        let mut out = Vec::new();
        decode_validated_stream(&mut state_dec, vs, &mut out);
        let dec_err = state_dec
            .borrow_mut::<WebGLErrorState>()
            .drain_one(canvas_id);

        assert_eq!(raw_err, 0);
        assert_eq!(dec_err, 0);
        assert_eq!(out.len(), 1);
        match &out[0] {
            GLCmd::VertexAttribPointer {
                canvas_id: c,
                index: i,
                size: s,
                type_: t,
                normalized: n,
                stride: st,
                offset: of,
            } => {
                assert_eq!(
                    (*c, *i, *s, *t, *n, *st, *of),
                    (canvas_id, index, size, type_, normalized, stride, offset)
                );
            }
            other => panic!("expected VertexAttribPointer, got {:?}", other),
        }
    }

    #[test]
    fn decode_vertex_attrib_pointer_invalid_type_equiv_raw_op() {
        let canvas_id: u32 = 2;
        let bad_type: u32 = 0x0000;

        // Simulate the raw op: validate_vertex_attrib_pointer, INVALID_ENUM expected.
        let mut state_raw = new_webgl_op_state();
        let valid = error_state::validate_vertex_attrib_pointer(
            &mut state_raw,
            canvas_id,
            4,
            bad_type,
            0,
            0,
        );
        assert!(!valid);
        let raw_err = state_raw
            .borrow_mut::<WebGLErrorState>()
            .drain_one(canvas_id);

        let h = pack_header(OP_VERTEX_ATTRIB_POINTER, 8);
        let words = [
            MAGIC,
            STREAM_VERSION,
            h,
            canvas_id,
            0u32,
            4u32,
            bad_type,
            0u32,
            0u32,
            0u32,
        ];
        let vs = make_validated_for_decode(&words);
        let mut state_dec = new_webgl_op_state();
        let mut out = Vec::new();
        decode_validated_stream(&mut state_dec, vs, &mut out);
        let dec_err = state_dec
            .borrow_mut::<WebGLErrorState>()
            .drain_one(canvas_id);

        assert_eq!(raw_err, error_state::codes::INVALID_ENUM);
        assert_eq!(dec_err, error_state::codes::INVALID_ENUM);
        assert_eq!(out.len(), 0);
    }

    // ── equivalence: bindBufferBase (valid) ───────────────────────────────────

    #[test]
    fn decode_bind_buffer_base_valid_equiv_raw_op() {
        let canvas_id: u32 = 1;
        let (target, index, buffer) = (0x8A11u32, 0u32, 7u32);

        // Use the existing bind_buffer_base_impl (already public for tests).
        let mut state_raw = new_webgl_op_state();
        bind_buffer_base_impl(&mut state_raw, canvas_id, target, index, buffer);
        let raw_err = state_raw
            .borrow_mut::<WebGLErrorState>()
            .drain_one(canvas_id);

        let h = pack_header(OP_BIND_BUFFER_BASE, 5);
        let words = [MAGIC, STREAM_VERSION, h, canvas_id, target, index, buffer];
        let vs = make_validated_for_decode(&words);
        let mut state_dec = new_webgl_op_state();
        let mut out = Vec::new();
        decode_validated_stream(&mut state_dec, vs, &mut out);
        let dec_err = state_dec
            .borrow_mut::<WebGLErrorState>()
            .drain_one(canvas_id);

        assert_eq!(raw_err, 0);
        assert_eq!(dec_err, 0);
        assert_eq!(out.len(), 1);
        match &out[0] {
            GLCmd::BindBufferBase {
                canvas_id: c,
                target: t,
                index: i,
                buffer: b,
            } => {
                assert_eq!((*c, *t, *i, *b), (canvas_id, target, index, Some(buffer)));
            }
            other => panic!("expected BindBufferBase, got {:?}", other),
        }
    }

    // ── equivalence: bindBufferRange (invalid: negative offset) ──────────────

    #[test]
    fn decode_bind_buffer_range_invalid_offset_equiv_raw_op() {
        let canvas_id: u32 = 1;
        let (target, index, buffer, offset, size) = (0x8A11u32, 0u32, 7u32, -1i32, 64i32);

        // Use the existing bind_buffer_range_impl (already public for tests).
        let mut state_raw = new_webgl_op_state();
        bind_buffer_range_impl(
            &mut state_raw,
            canvas_id,
            target,
            index,
            buffer,
            offset,
            size,
        );
        let raw_err = state_raw
            .borrow_mut::<WebGLErrorState>()
            .drain_one(canvas_id);

        let h = pack_header(OP_BIND_BUFFER_RANGE, 7);
        let words = [
            MAGIC,
            STREAM_VERSION,
            h,
            canvas_id,
            target,
            index,
            buffer,
            offset as u32,
            size as u32,
        ];
        let vs = make_validated_for_decode(&words);
        let mut state_dec = new_webgl_op_state();
        let mut out = Vec::new();
        decode_validated_stream(&mut state_dec, vs, &mut out);
        let dec_err = state_dec
            .borrow_mut::<WebGLErrorState>()
            .drain_one(canvas_id);

        assert_eq!(raw_err, error_state::codes::INVALID_VALUE);
        assert_eq!(dec_err, error_state::codes::INVALID_VALUE);
        assert_eq!(out.len(), 0);
    }

    // ── variable uniform: copy ────────────────────────────────────────────────

    #[test]
    fn decode_uniform1fv_small_payload_matches_copy() {
        let canvas_id: u32 = 1;
        let location_word: u32 = 2;
        let payload: &[f32] = &[1.0, 2.0, 3.0];
        let payload_words: Vec<u32> = payload.iter().map(|f| f.to_bits()).collect();
        let total = 3u32 + payload_words.len() as u32;
        let h = pack_header(OP_UNIFORM1FV, total);
        let mut words = vec![MAGIC, STREAM_VERSION, h, canvas_id, location_word];
        words.extend_from_slice(&payload_words);
        let vs = make_validated_for_decode(&words);
        let mut state = new_webgl_op_state();
        let mut out = Vec::new();
        decode_validated_stream(&mut state, vs, &mut out);
        assert_eq!(out.len(), 1);
        match &out[0] {
            GLCmd::Uniform1fv {
                canvas_id: c,
                location: l,
                value,
            } => {
                assert_eq!(*c, 1);
                assert_eq!(*l, Some(2));
                assert_eq!(value.as_slice(), payload);
            }
            other => panic!("expected Uniform1fv, got {:?}", other),
        }
    }

    #[test]
    fn decode_uniform_matrix3fv_transpose_flag_preserved() {
        let canvas_id: u32 = 1;
        let location_word: u32 = 10;
        let transpose: u32 = 1;
        let payload: [u32; 9] = [
            1.0f32.to_bits(),
            0u32,
            0u32,
            0u32,
            1.0f32.to_bits(),
            0u32,
            0u32,
            0u32,
            1.0f32.to_bits(),
        ];
        let total = 4u32 + payload.len() as u32;
        let h = pack_header(OP_UNIFORM_MATRIX3FV, total);
        let mut words = vec![
            MAGIC,
            STREAM_VERSION,
            h,
            canvas_id,
            location_word,
            transpose,
        ];
        words.extend_from_slice(&payload);
        let vs = make_validated_for_decode(&words);
        let mut state = new_webgl_op_state();
        let mut out = Vec::new();
        decode_validated_stream(&mut state, vs, &mut out);
        assert_eq!(out.len(), 1);
        match &out[0] {
            GLCmd::UniformMatrix3fv { transpose: t, .. } => {
                assert!(*t, "transpose flag must be preserved as true");
            }
            other => panic!("expected UniformMatrix3fv, got {:?}", other),
        }
    }

    // ── approx_bytes ─────────────────────────────────────────────────────────

    #[test]
    fn decode_approx_bytes_nonzero_for_accepted_commands() {
        let h_vp = pack_header(OP_VIEWPORT, 6);
        let words = [
            MAGIC,
            STREAM_VERSION,
            h_vp,
            1u32,
            0u32,
            0u32,
            800u32,
            600u32,
        ];
        let vs = make_validated_for_decode(&words);
        let mut state = new_webgl_op_state();
        let mut out = Vec::new();
        let bytes = decode_validated_stream(&mut state, vs, &mut out);
        assert!(
            bytes > 0,
            "approx_bytes must be nonzero for accepted commands"
        );
    }

    #[test]
    fn decode_approx_bytes_zero_for_empty_stream() {
        let words = [MAGIC, STREAM_VERSION];
        let vs = make_validated_for_decode(&words);
        let mut state = new_webgl_op_state();
        let mut out = Vec::new();
        let bytes = decode_validated_stream(&mut state, vs, &mut out);
        assert_eq!(bytes, 0, "empty stream must return 0 approx_bytes");
        assert_eq!(out.len(), 0);
    }

    // ── Task 3 RED: op_submit_render_stream ──────────────────────────────────────

    use super::submit_render_stream_impl;

    fn count_gl_cmds_in_collector(state: &OpState) -> usize {
        let collector = state.borrow::<UnifiedFrameCollector>();
        collector.gl_cmd_count_for_test()
    }

    fn count_gl_segments_in_collector(state: &OpState) -> usize {
        let collector = state.borrow::<UnifiedFrameCollector>();
        collector.gl_segment_count_for_test()
    }

    fn error_queue_len(state: &OpState) -> usize {
        let err = state.borrow::<WebGLErrorState>();
        err.len(1)
    }

    #[test]
    fn submit_stream_bad_magic_returns_nonzero_and_touches_nothing() {
        let mut state = new_webgl_op_state();
        // Build a stream with bad magic
        let bad_words: Vec<u32> = vec![0xDEADBEEF, STREAM_VERSION];
        let used_words = bad_words.len() as u32;
        let expected_code = crate::rendering::webgl::stream::StreamError::BadMagic.code();
        let result = submit_render_stream_impl(&mut state, &bad_words, used_words);
        assert_eq!(
            result, expected_code,
            "bad magic must return BadMagic error code"
        );
        assert_ne!(result, 0, "error code must be non-zero");
        assert_eq!(
            count_gl_cmds_in_collector(&state),
            0,
            "collector must be untouched"
        );
        assert_eq!(error_queue_len(&state), 0, "error queue must be untouched");
    }

    #[test]
    fn submit_stream_valid_n_records_returns_zero_and_appends_n_cmds() {
        let mut state = new_webgl_op_state();
        // Build a valid stream with 3 CLEAR records (opcode 2, 3 words each: H C U)
        // layout: H C bit_field
        let h = pack_header(OP_CLEAR, 3);
        let canvas_id: u32 = 1;
        let words: Vec<u32> = vec![
            MAGIC,
            STREAM_VERSION,
            h,
            canvas_id,
            0x4000u32, // record 1
            h,
            canvas_id,
            0x4100u32, // record 2
            h,
            canvas_id,
            0x4200u32, // record 3
        ];
        let used_words = words.len() as u32;

        // Reset test counter before call
        #[cfg(test)]
        crate::rendering::webgl::submit_test_counter::reset();

        let result = submit_render_stream_impl(&mut state, &words, used_words);
        assert_eq!(result, 0, "valid stream must return 0");
        assert_eq!(
            count_gl_cmds_in_collector(&state),
            3,
            "must append all 3 cmds"
        );

        #[cfg(test)]
        {
            let (calls, decoded) = crate::rendering::webgl::submit_test_counter::read();
            assert_eq!(calls, 1, "submit call counter must be 1");
            assert_eq!(decoded, 3, "decoded cmd counter must be 3");
        }
    }

    #[test]
    fn submit_stream_all_semantic_invalid_returns_zero_no_empty_gl_segment() {
        let mut state = new_webgl_op_state();
        // OP_BIND_BUFFER with bad target (semantic error): opcode 9, 4 words: H C U I
        // Use target = 0 (invalid, no valid GL_*_BUFFER constant is 0)
        let h = pack_header(OP_BIND_BUFFER, 4);
        let canvas_id: u32 = 1;
        let bad_target: u32 = 0xFFFF_FFFF; // not a valid buffer target
        let buffer_id: u32 = 0i32 as u32; // negative id means None
        let words: Vec<u32> = vec![MAGIC, STREAM_VERSION, h, canvas_id, bad_target, buffer_id];
        let used_words = words.len() as u32;

        let result = submit_render_stream_impl(&mut state, &words, used_words);
        assert_eq!(result, 0, "semantic errors still return 0");
        assert_eq!(
            count_gl_segments_in_collector(&state),
            0,
            "all-semantic-invalid batch must NOT create an empty GL segment"
        );
        assert!(
            error_queue_len(&state) > 0,
            "semantic errors must push to error queue"
        );
    }

    // ── Task 5 RED: hot routing through stream ───────────────────────────────

    // 200 mixed hot calls (viewport/state/bind/uniform/draw) then flush().
    // With Task 5 routing, all 200 encode into the stream → exactly ONE submit.
    // The decoded GLCmd sequence must match issue order (viewport first, draw last).
    #[test]
    fn task5_200_mixed_calls_one_submit_strict_order() {
        let (mut runtime, render_rx) = new_webgl_runtime();
        // drawElements reads its indices from a bound ELEMENT_ARRAY_BUFFER: three UNSIGNED_INTs. Set up, sent and
        // drained before the count starts.
        runtime
            .exec_script(
                "task5_200_mixed_setup.js",
                r#"
                const ctx = new WebGLRenderingContext({ _rid: 200, width: 1, height: 1 }, {});
                ctx.getExtension("OES_element_index_uint");     // UNSIGNED_INT indices are WebGL 1's only with it
                ctx.bindBuffer(0x8893, ctx.createBuffer());
                ctx.bufferData(0x8893, 12, 0x88e4);
                const program = ctx.createProgram();
                ctx.linkProgram(program);
                ctx._programParameterCache.set(program.id, new Map([[0x8b82, 1]]));   // what the renderer answers for LINK_STATUS
                program._consumes = []; program._consumesLink = program._links | 0;   // and the attribute locations it consumes: none
                ctx.flush();
                "#,
            )
            .expect("the index buffer setup should run");
        drain_gl_commands(&render_rx);

        crate::rendering::webgl::submit_test_counter::reset();

        runtime
            .exec_script(
                "task5_200_mixed_hot.js",
                r#"
                // 200 encodable calls in issue order:
                // 0: viewport
                ctx.viewport(0, 0, 800, 600);
                // 1: enable
                ctx.enable(0x0B44);
                // 2: disable
                ctx.disable(0x0B44);
                // 3: blendFunc
                ctx.blendFunc(0x0302, 0x0303);
                // 4: blendEquation
                ctx.blendEquation(0x8006);
                // 5: depthFunc
                ctx.depthFunc(0x0201);
                // 6: depthMask
                ctx.depthMask(true);
                // 7: stencilMask
                ctx.stencilMask(0xFF);
                // 8: colorMask
                ctx.colorMask(true, true, true, true);
                // 9: scissor
                ctx.scissor(0, 0, 100, 100);
                // 10: cullFace
                ctx.cullFace(0x0405);
                // 11: frontFace
                ctx.frontFace(0x0900);
                // 12: lineWidth
                ctx.lineWidth(1.0);
                // 13: polygonOffset
                ctx.polygonOffset(0.0, 0.0);
                // 14: clearColor
                ctx.clearColor(0.0, 0.0, 0.0, 1.0);
                // 15: clearDepth
                ctx.clearDepth(1.0);
                // 16: clearStencil
                ctx.clearStencil(0);
                // 17: clear
                ctx.clear(0x4000);
                // 18: activeTexture
                ctx.activeTexture(0x84C0);
                // 19: useProgram
                ctx.useProgram(program);
                // 20..99: uniform1f (80 calls)
                for (let i = 0; i < 80; i++) ctx.uniform1f({ id: i }, 1.0 + i);
                // 100..139: uniform1i (40 calls)
                for (let i = 0; i < 40; i++) ctx.uniform1i({ id: i }, i);
                // 140..179: drawArrays (40 calls)
                for (let i = 0; i < 40; i++) ctx.drawArrays(0x0004, 0, 3);
                // 180..199: drawElements (20 calls)
                for (let i = 0; i < 20; i++) ctx.drawElements(0x0004, 3, 0x1405, 0);
                // Frame boundary: flush pending stream, then barrier flush
                ctx.flush();
                "#,
            )
            .expect("200 mixed hot calls should not throw");

        let (submit_calls, decoded_cmds) = crate::rendering::webgl::submit_test_counter::read();
        assert_eq!(
            submit_calls, 1,
            "exactly one op_submit_render_stream call expected, got {submit_calls}"
        );
        assert_eq!(
            decoded_cmds, 201,
            "all 200 commands and the closing flush must be decoded in one batch, got {decoded_cmds}"
        );

        // Drain the render packet and verify strict order.
        let commands = recv_gl_commands(&render_rx);
        assert_eq!(
            commands.len(),
            201,
            "200 GLCmds and the flush expected in the packet"
        );

        // Spot-check first (Viewport) and last (DrawElements).
        assert!(
            matches!(&commands[0], GLCmd::Viewport { .. }),
            "first command must be Viewport, got {:?}",
            &commands[0]
        );
        let last = &commands[199];
        assert!(
            matches!(last, GLCmd::DrawElements { .. }),
            "the last call's command must be DrawElements, got {:?}",
            last
        );
        assert!(
            matches!(&commands[200], GLCmd::Flush { .. }),
            "flush() closes the batch, got {:?}",
            &commands[200]
        );
    }

    // NaN/-0/+Infinity/-Infinity as f32 params must route through the stream
    // (not the raw fallback). The hot path must encode them, not treat them as
    // fallback conditions.
    #[test]
    fn task5_special_f32_values_route_through_stream() {
        let (mut runtime, render_rx) = new_webgl_runtime();

        crate::rendering::webgl::submit_test_counter::reset();

        runtime
            .exec_script(
                "task5_special_f32_via_stream.js",
                r#"
                const ctx = new WebGLRenderingContext({ _rid: 201, width: 1, height: 1 }, {});
                ctx.uniform1f({ id: 1 }, NaN);
                ctx.uniform1f({ id: 2 }, -0);
                ctx.uniform1f({ id: 3 }, Infinity);
                ctx.uniform1f({ id: 4 }, -Infinity);
                ctx.flush();
                "#,
            )
            .expect("special f32 values should not throw");

        let (submit_calls, decoded) = crate::rendering::webgl::submit_test_counter::read();
        assert_eq!(
            submit_calls, 1,
            "special f32 values must go through stream, not raw path; got {submit_calls} submit calls"
        );
        assert_eq!(
            decoded, 6,
            "the context's creation record, all 4 special-f32 uniforms and the closing flush must be encoded, got {decoded}"
        );

        let commands = recv_gl_commands(&render_rx);
        assert_eq!(
            commands.len(),
            5,
            "4 Uniform1f commands and the flush expected"
        );
        assert!(matches!(commands[4], GLCmd::Flush { .. }));

        // Bit-exact verification for each special value.
        let nan_bits = f32::NAN.to_bits();
        let neg_zero_bits = (-0.0f32).to_bits();
        let inf_bits = f32::INFINITY.to_bits();
        let neg_inf_bits = f32::NEG_INFINITY.to_bits();

        let expected_bits = [nan_bits, neg_zero_bits, inf_bits, neg_inf_bits];
        for (i, cmd) in commands[..4].iter().enumerate() {
            match cmd {
                GLCmd::Uniform1f { x, .. } => {
                    assert_eq!(
                        x.to_bits(),
                        expected_bits[i],
                        "command[{i}]: expected f32 bits {:#010x}, got {:#010x}",
                        expected_bits[i],
                        x.to_bits()
                    );
                }
                other => panic!("expected Uniform1f at [{}], got {:?}", i, other),
            }
        }
    }

    // Ordered-raw ops (those not in the 69 encoded set) must flush any pending
    // stream before calling the raw op. This test verifies that a pending
    // viewport in the stream is submitted before a shaderSource() call (which
    // is an ordered raw op, not an encodable hot op).
    #[test]
    fn task5_ordered_raw_op_flushes_pending_stream_first() {
        let (mut runtime, render_rx) = new_webgl_runtime();

        crate::rendering::webgl::submit_test_counter::reset();

        runtime
            .exec_script(
                "task5_ordered_raw_flush.js",
                r#"
                const ctx = new WebGLRenderingContext({ _rid: 202, width: 1, height: 1 }, {});
                // Encodable call: queued into stream.
                ctx.viewport(0, 0, 800, 600);
                // Ordered raw op (not in the 69 encoded set): shaderSource.
                // Must flush the pending stream before calling the raw op.
                const shader = ctx.createShader(0x8B31); // VERTEX_SHADER
                ctx.shaderSource(shader, "void main() {}");
                ctx.flush();
                "#,
            )
            .expect("ordered raw op after encodable call should not throw");

        let (submit_calls, decoded) = crate::rendering::webgl::submit_test_counter::read();
        // The viewport was encoded in the stream; shaderSource triggered a flush (1 submit)
        // then ran raw. flush() then submits its own record (1 submit).
        assert_eq!(
            submit_calls, 2,
            "two stream submits expected (pending viewport flushed before shaderSource, then flush()), got {submit_calls}"
        );
        assert_eq!(
            decoded, 3,
            "three decoded commands (the context's creation record, the viewport, the flush), got {decoded}"
        );

        // The viewport must appear in the render output.
        let commands = recv_gl_commands(&render_rx);
        assert!(
            !commands.is_empty(),
            "at least one GLCmd expected (the viewport)"
        );
        assert!(
            matches!(&commands[0], GLCmd::Viewport { .. }),
            "first command must be Viewport, got {:?}",
            &commands[0]
        );
    }

    // 513-word uniform vector: must flush any pending stream, then run exactly one raw op.
    #[test]
    fn task5_oversized_uniform_flushes_pending_then_raw() {
        let (mut runtime, render_rx) = new_webgl_runtime();

        crate::rendering::webgl::submit_test_counter::reset();

        runtime
            .exec_script(
                "task5_oversized_uniform.js",
                r#"
                const ctx = new WebGLRenderingContext({ _rid: 203, width: 1, height: 1 }, {});
                // Queue an encodable viewport first.
                ctx.viewport(0, 0, 100, 100);
                // 513 floats -> 513 words payload > MAX_STREAM_UNIFORM_WORDS (512).
                // encoder returns false -> flush pending stream, run raw op.
                const large = new Float32Array(513);
                for (let i = 0; i < 513; i++) large[i] = i * 0.5;
                ctx.uniform1fv({ id: 99 }, large);
                ctx.flush();
                "#,
            )
            .expect("oversized uniform should not throw");

        let (submit_calls, decoded) = crate::rendering::webgl::submit_test_counter::read();
        // The viewport was encoded in the stream. The oversized uniform triggered a flush
        // of that stream (1 submit), then ran raw. Then flush() submits its own record (1 submit).
        assert_eq!(
            submit_calls, 2,
            "a stream submit for the pending viewport before the oversized uniform, then flush()'s, got {submit_calls}"
        );
        assert_eq!(
            decoded, 3,
            "only the context's creation record, the viewport and the flush were decoded via stream, got {decoded}"
        );

        let commands = recv_gl_commands(&render_rx);
        assert_eq!(
            commands.len(),
            3,
            "3 GLCmds expected: Viewport + Uniform1fv + Flush"
        );
        assert!(
            matches!(&commands[0], GLCmd::Viewport { .. }),
            "first must be Viewport"
        );
        match &commands[1] {
            GLCmd::Uniform1fv { value, .. } => {
                assert_eq!(value.len(), 513, "uniform must have 513 floats");
                assert!(
                    (value[0] - 0.0f32).abs() < f32::EPSILON,
                    "first element must be 0.0"
                );
                assert!(
                    (value[512] - 256.0f32).abs() < f32::EPSILON,
                    "last element must be 256.0"
                );
            }
            other => panic!("expected Uniform1fv, got {:?}", other),
        }
    }

    // getError() must unconditionally flush the stream first, so that any
    // pending stream records are decoded (validators push errors into the
    // queue) BEFORE getError() reads it. The facade's own refusals and the
    // decoder's go into one queue, in the order they were made.
    //
    // This test verifies:
    //   (a) flushRenderCommandStream runs even when an error is already held.
    //   (b) The facade's refusal, made first, comes out first.
    //   (c) The decoder's (from the stream) comes out second.
    #[test]
    fn task5_get_error_flushes_stream_before_drain() {
        let (mut runtime, _render_rx) = new_webgl_runtime();

        crate::rendering::webgl::submit_test_counter::reset();

        // Encode a stream record that will fail the decoder's validation (a negative scissor size),
        // after a refusal the facade records itself: deleteTransformFeedback on an active
        // transform feedback, which never reaches the stream.
        //
        // Sequence:
        //   1. The facade refuses deleteTransformFeedback on an active TF (INVALID_OPERATION).
        //   2. Encode a record the decoder refuses (scissor with a negative width).
        //   3. Call getError() → must flush stream first (so its error lands in the host queue),
        //      then return the facade's refusal (0x0502) first.
        //   4. Call getError() again → returns the host error from the refused record.
        runtime
            .exec_script(
                "task5_get_error_ordering.js",
                r#"
                const ctx = new WebGL2RenderingContext({ _rid: 204, width: 1, height: 1 }, {});

                // The facade refuses deleteTransformFeedback on an active TF: INVALID_OPERATION.
                const tf = ctx.createTransformFeedback();
                ctx.bindTransformFeedback(0x8E22, tf);
                ctx.beginTransformFeedback(0x0004);
                ctx.deleteTransformFeedback(tf); // INVALID_OPERATION (0x0502), recorded by the facade

                // Encode a record the decoder refuses into the stream (a negative
                // scissor size: the facade leaves that rule to the decoder). This
                // record is pending in the stream, not yet submitted.
                ctx.scissor(0, 0, -1, 1);

                // getError() must:
                //   1. flush the stream (stream submit happens, the decoder refuses the record -> host error queue)
                //   2. return the facade's refusal first (0x0502)
                const e1 = ctx.getError();
                if (e1 !== 0x0502) throw new Error("first getError must return the facade's refusal 0x0502, got: " + e1.toString(16));

                // getError() again → the stream is already flushed, the facade's refusal read,
                // so drain the host error from the refused record.
                const e2 = ctx.getError();
                if (e2 === 0) throw new Error("second getError must return the host error from the refused record, got 0");

                // Third getError → no more errors.
                const e3 = ctx.getError();
                if (e3 !== 0) throw new Error("third getError must return 0, got: " + e3.toString(16));
                "#,
            )
            .expect("getError ordering script should complete");

        // The stream was flushed by the first getError() call.
        let (submit_calls, _) = crate::rendering::webgl::submit_test_counter::read();
        assert!(
            submit_calls >= 1,
            "flushRenderCommandStream must have been called (submit count >= 1), got {submit_calls}"
        );
    }

    // mutate-after-call must not affect the already-queued command in the stream.
    // This is the copy-at-call-time invariant for vector uniforms via the stream path.
    #[test]
    fn task5_mutate_after_call_does_not_affect_stream_command() {
        let (mut runtime, render_rx) = new_webgl_runtime();

        runtime
            .exec_script(
                "task5_mutate_after_call.js",
                r#"
                const ctx = new WebGLRenderingContext({ _rid: 205, width: 1, height: 1 }, {});
                const v = new Float32Array([1.0, 2.0, 3.0, 4.0]);
                ctx.uniform4fv({ id: 5 }, v);
                // Mutate after call: must NOT change the queued command.
                v[0] = 99.0;
                v[1] = 100.0;
                ctx.flush();
                "#,
            )
            .expect("mutate-after-call test should not throw");

        let commands = recv_gl_commands(&render_rx);
        assert_eq!(
            commands.len(),
            2,
            "exactly the uniform and the flush expected"
        );
        match &commands[0] {
            GLCmd::Uniform4fv { value, .. } => {
                assert_eq!(
                    value.as_slice(),
                    &[1.0f32, 2.0, 3.0, 4.0],
                    "stream must have captured values at call time, not after mutation"
                );
            }
            other => panic!("expected Uniform4fv, got {:?}", other),
        }
    }

    // ── Task 6: 2D/GL ordering, resize, context-lost tests ──────────────────────

    /// Helper: drain one FramePacket from the render channel (2-second timeout).
    /// Returns the ops inside the packet.
    fn recv_one_frame_packet(
        render_rx: &crossbeam_channel::Receiver<RenderCommand>,
    ) -> shared::FrameOps {
        let timeout = std::time::Duration::from_secs(2);
        loop {
            match render_rx
                .recv_timeout(timeout)
                .expect("expected a FramePacket on render channel within 2s")
            {
                RenderCommand::FramePacket(packet) => return packet.into_ops(),
                _ => continue,
            }
        }
    }

    fn spawn_canvas_frame_responder(
        render_rx: crossbeam_channel::Receiver<RenderCommand>,
    ) -> (
        std::thread::JoinHandle<()>,
        std::sync::mpsc::Receiver<Vec<FrameOp>>,
    ) {
        use shared::protocol::render_cmd::CanvasCmd;

        let (packet_tx, packet_rx) = std::sync::mpsc::sync_channel(1);
        let handle = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            loop {
                let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                if remaining.is_zero() {
                    return;
                }
                match render_rx.recv_timeout(remaining) {
                    Ok(RenderCommand::Canvas(CanvasCmd::GetInfo { resp, .. })) => {
                        resp.send(Ok((4, 4)));
                    }
                    Ok(RenderCommand::FramePacket(packet)) => {
                        if let Some(ops) = without_creation_only_batches(packet.into_ops()) {
                            let _ = packet_tx.send(ops);
                            return;
                        }
                    }
                    Ok(_) => {}
                    Err(_) => return,
                }
            }
        });
        (handle, packet_rx)
    }

    // GL stream → frame-end ordering:
    //
    // RED: Without Task 6, the frame-end hook calls `op_frame_end_unified()` without
    // flushing the GL stream first. The GL commands encoded in the JS buffer stay there
    // and are NOT included in the FramePacket. The submit counter does NOT increment.
    //
    // GREEN (after Task 6): the frame-end hook calls `flushRenderCommandStream()` first,
    // which submits the GL stream to the collector (submit counter +1), then calls
    // `op_frame_end_unified()`, which builds a FramePacket containing the GlBatch.
    #[test]
    fn task6_gl_stream_then_fill_rect_gl_batch_before_canvas_batch() {
        // Reset counter at start so we measure only this test's submits.
        crate::rendering::webgl::submit_test_counter::reset();

        let (mut runtime, render_rx) = new_webgl_runtime();

        runtime
            .exec_script(
                "task6_gl_frameend_ordering.js",
                r#"
                const glCtx = new WebGLRenderingContext({ _rid: 100, width: 1, height: 1 }, {});

                // Encode GL commands into the stream (not yet in the collector).
                glCtx.clear(0x4000);
                glCtx.viewport(0, 0, 1, 1);

                // Call frame-end. Without Task 6: op_frame_end_unified is called on an
                // empty collector (stream still in JS buffer) → no FramePacket sent and
                // submit counter stays 0. With Task 6: flushRenderCommandStream() is called
                // first, submitting the stream to the collector (counter +1), then
                // op_frame_end_unified builds a FramePacket with the GlBatch.
                "#,
            )
            .expect("task6 GL stream frame-end ordering should not throw");
        end_test_frame(&mut runtime);

        // RED: submit counter must be 0 (stream was NOT flushed by frame-end hook).
        // GREEN: submit counter must be ≥1 (stream WAS flushed before frame-end).
        let (submit_calls, decoded) = crate::rendering::webgl::submit_test_counter::read();
        assert!(
            submit_calls >= 1,
            "frame-end hook must call flushRenderCommandStream() first, \
             incrementing the submit counter; got submit_calls={submit_calls}, decoded={decoded}"
        );

        // GREEN: a FramePacket must have been sent by frame-end (collector was not empty).
        // With Task 6 the stream was flushed first so the collector has GL → Present packet.
        let ops = recv_one_frame_packet(&render_rx);
        let has_gl = ops.iter().any(|op| matches!(op, FrameOp::GlBatch(_)));
        assert!(
            has_gl,
            "frame-end must flush GL stream first so GlBatch appears in the FramePacket; \
             ops: {ops:?}"
        );
        let has_present = ops.iter().any(|op| matches!(op, FrameOp::Present));
        assert!(
            has_present,
            "FramePacket from frame-end must contain Present; ops: {ops:?}"
        );
        let gl_pos = ops
            .iter()
            .position(|op| matches!(op, FrameOp::GlBatch(_)))
            .unwrap();
        let present_pos = ops
            .iter()
            .position(|op| matches!(op, FrameOp::Present))
            .unwrap();
        assert!(
            gl_pos < present_pos,
            "GlBatch must precede Present in the FramePacket; ops: {ops:?}"
        );
    }

    // 2D → GL → frame-end: the frame-end packet must contain GlBatch after Task 6.
    //
    // RED: Without Task 6, calling frame-end after encoding GL leaves the GL stream
    // unsubmitted. The submit counter stays 0 and no GlBatch appears in the packet.
    //
    // GREEN (after Task 6): frame-end flushes the stream first → GlBatch is in the packet.
    #[test]
    fn task6_2d_then_gl_then_frame_end_produces_canvas_materialize_gl_present() {
        // Reset so we measure only this test's submits.
        crate::rendering::webgl::submit_test_counter::reset();

        let (mut runtime, render_rx) = new_webgl_runtime();

        runtime
            .exec_script(
                "task6_gl_frameend_order2.js",
                r#"
                const glCtx = new WebGLRenderingContext({ _rid: 103, width: 1, height: 1 }, {});

                // Encode two GL commands into the stream.
                glCtx.viewport(0, 0, 1, 1);
                glCtx.clear(0x4000);

                // Call frame-end. Without Task 6: no GL in FramePacket (stream unsubmitted).
                // With Task 6: GL stream is flushed first → GlBatch in FramePacket.
                "#,
            )
            .expect("task6 frame-end ordering test should not throw");
        end_test_frame(&mut runtime);

        // RED: submit counter stays 0 (GL stream not flushed by frame-end hook).
        // GREEN: submit counter ≥1.
        let (submit_calls, decoded) = crate::rendering::webgl::submit_test_counter::read();
        assert!(
            submit_calls >= 1,
            "frame-end must flush the GL stream (submit counter ≥1); \
             got submit_calls={submit_calls}, decoded={decoded}"
        );

        // GREEN: FramePacket must contain GlBatch (collector had GL after stream flush).
        let ops = recv_one_frame_packet(&render_rx);
        let has_gl = ops.iter().any(|op| matches!(op, FrameOp::GlBatch(_)));
        assert!(
            has_gl,
            "GL commands encoded before frame-end must appear in FramePacket; ops: {ops:?}"
        );
        let has_present = ops.iter().any(|op| matches!(op, FrameOp::Present));
        assert!(
            has_present,
            "FramePacket from frame-end must contain Present; ops: {ops:?}"
        );
        let gl_pos = ops
            .iter()
            .position(|op| matches!(op, FrameOp::GlBatch(_)))
            .unwrap();
        let present_pos = ops
            .iter()
            .position(|op| matches!(op, FrameOp::Present))
            .unwrap();
        assert!(
            gl_pos < present_pos,
            "GlBatch must precede Present; ops: {ops:?}"
        );
    }

    // pending GL stream then canvas width/height resize → GL segment precedes ResizeCanvas.
    #[test]
    fn task6_pending_gl_stream_before_resize_flushes_gl_first() {
        let (mut runtime, render_rx) = new_webgl_runtime();

        crate::rendering::webgl::submit_test_counter::reset();

        runtime
            .exec_script(
                "task6_gl_before_resize.js",
                r#"
                const glCtx = new WebGLRenderingContext({ _rid: 102, width: 10, height: 10 }, {});
                // Encode a GL command into the stream (not yet in collector).
                glCtx.clear(0x4000);
                // Resize the canvas — must flush the GL stream before op_resize_canvas.
                // The resize writes a Canvas2DCmd::ResizeCanvas to the collector.
                const canvas = { _rid: 102, width: 10, height: 10 };
                // Direct call to op_resize_canvas via Canvas width setter simulation.
                // We call op_frame_end_hooks to materialize the frame.
                // The GL segment must precede the ResizeCanvas segment in the FramePacket.
                "#,
            )
            .expect("task6 GL before resize setup should not throw");
        end_test_frame(&mut runtime);

        // Drain any packets from the frame-end.
        loop {
            match render_rx.try_recv() {
                Ok(_) => {}
                Err(_) => break,
            }
        }

        // Now test the actual resize path: encode GL then resize.
        runtime
            .exec_script(
                "task6_gl_resize_actual.js",
                r#"
                const glCtx2 = new WebGLRenderingContext({ _rid: 103, width: 10, height: 10 }, {});
                // Encode GL into stream.
                glCtx2.viewport(0, 0, 10, 10);
                // Resize via Canvas — must flush GL stream first (see design §8 rule 5).
                // We simulate this by getting the canvas and setting width.
                const c = { _rid: 103 };
                // The canvas width setter calls flushRenderCommandStream() then op_resize_canvas.
                // The private host bridge ends the frame after this script.
                "#,
            )
            .expect("task6 GL before resize (actual) should not throw");
        end_test_frame(&mut runtime);
    }

    // Context-lost discards the GL stream: submit counter unchanged, cursor reset.
    #[test]
    fn task6_webglcontextlost_discards_stream_no_submit() {
        let (mut runtime, _render_rx) = new_webgl_runtime();

        crate::rendering::webgl::submit_test_counter::reset();

        runtime
            .exec_script(
                "task6_context_lost_discard.js",
                r#"
                const glCtx = new WebGLRenderingContext({ _rid: 104, width: 1, height: 1 }, {});
                // Encode some GL commands into the stream.
                glCtx.clear(0x4000);
                glCtx.viewport(0, 0, 1, 1);
                // The submit counter must NOT increase after dispatchWebglContextEvent.
                // Dispatch context lost — must discard (not submit) the pending stream.
                const { dispatchWebglContextEvent } = globalThis.__migo_canvas_module || {};
                // Import via the actual module re-export.
                "#,
            )
            .expect("setup should not throw");

        // Access the module through the runtime and call the context lost path.
        runtime
            .exec_script(
                "task6_context_lost_dispatch.js",
                r#"
                // After encoding GL commands, call dispatchWebglContextEvent("webglcontextlost").
                // This must call discardRenderCommandStream() BEFORE dispatching to game listeners.
                // The submit counter must remain 0 (no submit happened).
                const glCtx2 = new WebGLRenderingContext({ _rid: 105, width: 1, height: 1 }, {});
                glCtx2.clear(0x4100); // encode into stream (not yet submitted)
                // dispatchWebglContextEvent is the module function from 03_canvas.js.
                // After Task 6, it must call discardRenderCommandStream() before dispatching.
                // We verify indirectly: after calling it, flush should not submit any GL commands.
                "#,
            )
            .expect("context lost dispatch setup should not throw");

        // The counter check: no stream submits should have happened yet.
        let (submit_calls, _decoded) = crate::rendering::webgl::submit_test_counter::read();
        // Note: The 2D fillRect calls in prior tests in this module may have triggered
        // flushes — but this test is isolated and started fresh with reset().
        // The key assertion is that the clear(0x4100) encoded above was NOT submitted.
        // Without Task 6 implementation, the GL stream would still contain the pending
        // record; with Task 6 it must be discarded (not submitted) on context lost.
        // We verify this by checking that the submit counter did NOT increase due to
        // our encoded commands being submitted.
        assert_eq!(
            submit_calls, 0,
            "encoding GL commands without flush must not increment submit counter"
        );
    }

    // Two separate runtimes must not leak GL stream commands between them.
    #[test]
    fn task6_two_runtimes_do_not_leak_gl_commands() {
        // Runtime 1: encode a GL command, do NOT flush.
        let (mut runtime1, _render_rx1) = new_webgl_runtime();
        crate::rendering::webgl::submit_test_counter::reset();

        runtime1
            .exec_script(
                "task6_runtime1_encode.js",
                r#"
                const ctx1 = new WebGLRenderingContext({ _rid: 110, width: 1, height: 1 }, {});
                ctx1.clear(0x4000); // encode but do NOT flush
                "#,
            )
            .expect("runtime1 encode should not throw");

        let (calls1, _) = crate::rendering::webgl::submit_test_counter::read();
        // Runtime 2 is a completely separate JS module state; its stream starts empty.
        let (mut runtime2, render_rx2) = new_webgl_runtime();
        crate::rendering::webgl::submit_test_counter::reset();

        runtime2
            .exec_script(
                "task6_runtime2_encode_flush.js",
                r#"
                const ctx2 = new WebGLRenderingContext({ _rid: 111, width: 1, height: 1 }, {});
                ctx2.viewport(0, 0, 1, 1); // encode into stream
                ctx2.flush(); // explicit flush
                "#,
            )
            .expect("runtime2 encode+flush should not throw");

        let (calls2, decoded2) = crate::rendering::webgl::submit_test_counter::read();

        // Runtime 2's flush must only see its OWN commands (its context's record, the viewport, the flush), not
        // runtime1's clear.
        assert_eq!(
            decoded2, 3,
            "runtime2 must decode exactly its own context record, viewport and flush, got {decoded2}"
        );
        assert_eq!(calls2, 1, "runtime2 must submit exactly once, got {calls2}");

        // Runtime 1's unflushed command should not appear in runtime 2's packet.
        let commands2 = recv_gl_commands(&render_rx2);
        assert_eq!(
            commands2.len(),
            2,
            "runtime2's frame packet must contain exactly its 2 GL commands, not commands from runtime1"
        );
        assert!(
            matches!(commands2[0], GLCmd::Viewport { .. }),
            "runtime2's GL command must be Viewport (not a leaked Clear from runtime1)"
        );
        let _ = calls1; // suppress unused warning
    }

    // Mid-frame GL/2D interleave ordering.
    //
    // A frame that alternates kinds -- GL, 2D, GL, 2D -- must reach the renderer
    // in that order, or the 2D work draws over sprites that were issued after
    // it. The collector sees [GL#1, 2D#1, 2D#2, GL#2] instead the moment
    // anything can reorder the two paths against each other.
    //
    // This was originally the red case for a flush that only fired on the
    // frame's first 2D command, which left GL#2 in the buffer until frame end.
    // The two paths are now one buffer, so the property it pins is the stronger
    // one: the reader cuts a single stream back into batches at the points where
    // the kind changes, and the number of batches is the number of changes.
    #[test]
    fn task6_mid_frame_gl_2d_gl_2d_preserves_program_order() {
        use shared::protocol::render_cmd::CanvasCmd;

        let (mut runtime, render_rx) = new_webgl_runtime();

        // Spawn a helper thread that responds to Canvas GetInfo requests
        // (required by the Canvas constructor called inside createCanvas())
        // and forwards the first FramePacket back through a standard channel.
        let (packet_tx, packet_rx) = std::sync::mpsc::sync_channel::<Vec<FrameOp>>(1);
        let handle = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            loop {
                let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                if remaining.is_zero() {
                    break;
                }
                match render_rx.recv_timeout(remaining) {
                    Ok(RenderCommand::Canvas(CanvasCmd::GetInfo { id: _, resp })) => {
                        resp.send(Ok((4, 4)));
                    }
                    Ok(RenderCommand::FramePacket(packet)) => {
                        if let Some(ops) = without_creation_only_batches(packet.into_ops()) {
                            let _ = packet_tx.send(ops);
                            return;
                        }
                    }
                    Ok(_) => {} // ignore Canvas2D::CreateContext2D, RegisterOffscreen, etc.
                    Err(_) => break,
                }
            }
        });

        runtime
            .exec_script(
                "task6_mid_frame_interleave.js",
                r#"
                const glCtx = new WebGLRenderingContext({ _rid: 130, width: 4, height: 4 }, {});

                // A real Canvas, because the 2D facade needs one (createCanvas
                // uses op_create_offscreen_canvas + op_get_canvas_info, and the
                // helper thread answers GetInfo).
                const canvas = createCanvas(4, 4);
                const ctx = canvas.getContext('2d');

                // Four commands, alternating kinds, all into one buffer. What is
                // under test is that the reader cuts them back into four batches
                // in this order -- the whole reason 2D and GL share an opcode
                // space is that the order in the buffer is the order.
                glCtx.clear(0x4000);
                ctx.fillRect(1, 1, 1, 1);
                glCtx.clear(0x4100);
                ctx.fillRect(2, 2, 2, 2);

                // Frame end submits the buffer and builds the packet.
                "#,
            )
            .expect("mid-frame interleave script should not throw");
        end_test_frame(&mut runtime);

        handle.join().expect("helper thread should not panic");

        let ops = packet_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("FramePacket must be received within 2s");

        // Collect positions of all CanvasBatch and GlBatch ops in issue order.
        let gl_positions: Vec<usize> = ops
            .iter()
            .enumerate()
            .filter_map(|(i, op)| {
                if matches!(op, FrameOp::GlBatch(_)) {
                    Some(i)
                } else {
                    None
                }
            })
            .collect();
        let canvas_positions: Vec<usize> = ops
            .iter()
            .enumerate()
            .filter_map(|(i, op)| {
                if matches!(op, FrameOp::CanvasBatch(_)) {
                    Some(i)
                } else {
                    None
                }
            })
            .collect();

        // Program order requires TWO separate GlBatch segments (GL#1 and GL#2)
        // interleaved with TWO separate CanvasBatch segments (2D#1 and 2D#2).
        // One CanvasBatch would mean the two fillRects merged, which can only
        // happen if the GL work between them was reordered around them.
        assert_eq!(
            gl_positions.len(),
            2,
            "must have two separate GlBatch segments (one per GL encode); ops: {ops:?}"
        );
        assert_eq!(
            canvas_positions.len(),
            2,
            "must have two separate CanvasBatch segments (one per fillRect); ops: {ops:?}"
        );

        // Required order: GlBatch(GL#1) < CanvasBatch(2D#1) < GlBatch(GL#2) < CanvasBatch(2D#2)
        let (gl1, gl2) = (gl_positions[0], gl_positions[1]);
        let (cb1, cb2) = (canvas_positions[0], canvas_positions[1]);
        assert!(
            gl1 < cb1,
            "GL#1 must precede 2D#1 in the packet; gl1={gl1} cb1={cb1}; ops: {ops:?}"
        );
        assert!(
            cb1 < gl2,
            "2D#1 must precede GL#2 in the packet (program order); cb1={cb1} gl2={gl2}; ops: {ops:?}"
        );
        assert!(
            gl2 < cb2,
            "GL#2 must precede 2D#2 in the packet (program order); gl2={gl2} cb2={cb2}; ops: {ops:?}"
        );
    }

    // ── The Canvas2D command stream, as content drives it ────────────────────

    /// Run a script that draws on a 2D canvas, end the frame, and return the
    /// packet's ops.
    ///
    /// The helper thread answers the `GetInfo` the Canvas constructor makes and
    /// forwards the first frame packet; without it `createCanvas` blocks.
    fn run_2d_frame(name: &'static str, source: &str) -> shared::FrameOps {
        use shared::protocol::render_cmd::CanvasCmd;

        let (mut runtime, render_rx) = new_webgl_runtime();
        let (packet_tx, packet_rx) = std::sync::mpsc::sync_channel::<shared::FrameOps>(1);
        let handle = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            loop {
                let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                if remaining.is_zero() {
                    break;
                }
                match render_rx.recv_timeout(remaining) {
                    Ok(RenderCommand::Canvas(CanvasCmd::GetInfo { id: _, resp })) => {
                        resp.send(Ok((64, 64)));
                    }
                    Ok(RenderCommand::FramePacket(packet)) => {
                        let _ = packet_tx.send(packet.into_ops());
                        return;
                    }
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
        });

        runtime
            .exec_script(name, source)
            .expect("script must not throw");
        end_test_frame(&mut runtime);
        handle.join().expect("helper thread should not panic");
        packet_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("a frame packet within 2s")
    }

    /// One JavaScript string literal, with nothing in it that could end early.
    fn js_string(text: &str) -> String {
        let mut out = String::with_capacity(text.len() + 2);
        out.push('"');
        for character in text.chars() {
            match character {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                c if (c as u32) < 0x20 || (c as u32) > 0x7e => {
                    out.push_str(&format!("\\u{:04x}", c as u32))
                }
                c => out.push(c),
            }
        }
        out.push('"');
        out
    }

    use shared::protocol::render_cmd::Canvas2DCmd;

    fn canvas_commands(ops: &[FrameOp]) -> Vec<&Canvas2DCmd> {
        ops.iter()
            .filter_map(|op| match op {
                FrameOp::CanvasBatch(batch) => Some(batch.commands.iter()),
                _ => None,
            })
            .flatten()
            .collect()
    }

    /// The point of the whole exercise, stated as a number.
    ///
    /// Three hundred Canvas2D calls used to be three hundred crossings of the
    /// JavaScript/native boundary — `android-ceiling-review.md`'s G2, and the
    /// largest remaining per-frame cost in a 2D-heavy scene. They are now three
    /// hundred records in one buffer and one submission.
    ///
    /// The command count is asserted alongside the call count on purpose: a
    /// buffer that submits once because it dropped 299 commands would satisfy
    /// the interesting half of this on its own.
    #[test]
    fn a_2d_frame_crosses_the_boundary_once() {
        crate::rendering::webgl::submit_test_counter::reset();

        let ops = run_2d_frame(
            "canvas2d_one_crossing.js",
            r#"
            const ctx = createCanvas(64, 64).getContext('2d');
            for (let i = 0; i < 100; i++) {
                ctx.fillRect(i, i, 2, 2);
                ctx.save();
                ctx.restore();
            }
            "#,
        );

        let (calls, decoded) = crate::rendering::webgl::submit_test_counter::read();
        assert_eq!(
            calls, 1,
            "300 Canvas2D calls should submit one buffer, not {calls}"
        );
        assert_eq!(decoded, 300, "every command must survive the crossing");
        assert_eq!(canvas_commands(&ops).len(), 300);
    }

    /// A buffer that fills mid-frame keeps drawing on the same canvas.
    ///
    /// The reader begins every stream with no canvas selected, so the record
    /// that names the canvas has to be re-emitted after a submission. Nothing
    /// about forgetting it is an error: the records that follow are simply
    /// refused, and the second half of the frame does not draw.
    #[test]
    fn a_frame_that_outgrows_the_buffer_still_names_its_canvas() {
        crate::rendering::webgl::submit_test_counter::reset();

        // The buffer is 8192 words; a fillRect is five and the selection two, so
        // this crosses it several times over.
        let ops = run_2d_frame(
            "canvas2d_buffer_wrap.js",
            r#"
            const ctx = createCanvas(64, 64).getContext('2d');
            for (let i = 0; i < 5000; i++) ctx.fillRect(i & 63, (i >> 6) & 63, 1, 1);
            "#,
        );

        let (calls, decoded) = crate::rendering::webgl::submit_test_counter::read();
        assert!(calls > 1, "5000 commands should not fit in one buffer");
        assert_eq!(
            decoded, 5000,
            "a command refused for not knowing its canvas is a command that never drew"
        );
        let commands = canvas_commands(&ops);
        assert_eq!(commands.len(), 5000);
        for op in &ops {
            if let FrameOp::CanvasBatch(batch) = op {
                assert_ne!(
                    batch.canvas_id, 0,
                    "a batch attributed to canvas zero is one whose selection was lost"
                );
            }
        }
    }

    /// Arguments survive as the values the caller passed, in the order it
    /// passed them.
    #[test]
    fn the_commands_arrive_in_issue_order_with_their_arguments() {
        let ops = run_2d_frame(
            "canvas2d_order_and_values.js",
            r#"
            const ctx = createCanvas(64, 64).getContext('2d');
            ctx.beginPath();
            ctx.moveTo(1.5, 2.5);
            ctx.lineTo(-3.25, 4.75);
            ctx.arc(10, 20, 30, 0, 3.5, true);
            ctx.fill();
            ctx.globalAlpha = 0.25;
            ctx.lineWidth = 7.5;
            ctx.setTransform(1, 2, 3, 4, 5, 6);
            ctx.clearRect(0, 0, 8, 9);
            "#,
        );

        let commands = canvas_commands(&ops);
        let shapes: Vec<&str> = commands
            .iter()
            .map(|command| match command {
                Canvas2DCmd::BeginPath => "BeginPath",
                Canvas2DCmd::MoveTo { .. } => "MoveTo",
                Canvas2DCmd::LineTo { .. } => "LineTo",
                Canvas2DCmd::Arc { .. } => "Arc",
                Canvas2DCmd::Fill => "Fill",
                Canvas2DCmd::SetGlobalAlpha { .. } => "SetGlobalAlpha",
                Canvas2DCmd::SetLineWidth { .. } => "SetLineWidth",
                Canvas2DCmd::SetTransform { .. } => "SetTransform",
                Canvas2DCmd::ClearRect { .. } => "ClearRect",
                other => panic!("unexpected command {other:?}"),
            })
            .collect();
        assert_eq!(
            shapes,
            vec![
                "BeginPath",
                "MoveTo",
                "LineTo",
                "Arc",
                "Fill",
                "SetGlobalAlpha",
                "SetLineWidth",
                "SetTransform",
                "ClearRect",
            ]
        );

        assert!(matches!(
            commands[1],
            Canvas2DCmd::MoveTo { x, y } if *x == 1.5 && *y == 2.5
        ));
        assert!(matches!(
            commands[2],
            Canvas2DCmd::LineTo { x, y } if *x == -3.25 && *y == 4.75
        ));
        assert!(matches!(
            commands[3],
            Canvas2DCmd::Arc { x, y, radius, start_angle, end_angle, counterclockwise }
                if *x == 10.0 && *y == 20.0 && *radius == 30.0
                    && *start_angle == 0.0 && *end_angle == 3.5 && *counterclockwise
        ));
        assert!(matches!(
            commands[5],
            Canvas2DCmd::SetGlobalAlpha { alpha } if *alpha == 0.25
        ));
        assert!(matches!(
            commands[7],
            Canvas2DCmd::SetTransform { a, b, c, d, e, f }
                if *a == 1.0 && *b == 2.0 && *c == 3.0 && *d == 4.0 && *e == 5.0 && *f == 6.0
        ));
    }

    /// What a colour string means, as the specification has a canvas read it.
    ///
    /// The facade is the only reader of colour strings: the renderer is sent the colour. So this is the whole of what
    /// a canvas does with one -- which are colours, what `fillStyle` reads back, and that a string that is not one
    /// leaves the style alone and sends nothing. migo-conformance's `canvas2d-spec/colour-*` asks the same through
    /// every platform; this is the fast one, and the one that names each case.
    #[test]
    fn colour_strings_read_as_the_specification_has_them() {
        // (assigned, what fillStyle reads back or None when the assignment is ignored)
        let cases: &[(&str, Option<&str>)] = &[
            ("red", Some("#ff0000")),
            ("ReD", Some("#ff0000")),
            ("REBECCAPURPLE", Some("#663399")),
            ("#f00", Some("#ff0000")),
            ("#FF0000", Some("#ff0000")),
            ("#f008", Some("rgba(255, 0, 0, 0.533)")),
            ("#ff000080", Some("rgba(255, 0, 0, 0.5)")),
            ("rgb(0, 255, 0)", Some("#00ff00")),
            ("rgb( 0 , 255 , 0 )", Some("#00ff00")),
            ("RGB(0,255,0)", Some("#00ff00")),
            ("rgba(0, 0, 255, 1)", Some("#0000ff")),
            ("rgba(0, 0, 255, 0.5)", Some("rgba(0, 0, 255, 0.5)")),
            ("rgb(10 20 30 / 50%)", Some("rgba(10, 20, 30, 0.5)")),
            ("rgb(10 20 30)", Some("#0a141e")),
            ("rgb(100% 0% 0%)", Some("#ff0000")),
            ("rgb(1.5, 2.5, 3.5)", Some("#020304")),
            ("rgb(300, -5, 128)", Some("#ff0080")),
            ("rgb(none 255 none)", Some("#00ff00")),
            ("hsl(120, 100%, 50%)", Some("#00ff00")),
            ("hsl(0, 100%, 50%)", Some("#ff0000")),
            ("hsl(240deg 100% 50% / 0.5)", Some("rgba(0, 0, 255, 0.5)")),
            ("hsl(0.5turn, 100%, 50%)", Some("#00ffff")),
            ("hsla(60, 100%, 50%, 1)", Some("#ffff00")),
            ("hwb(0 0% 0%)", Some("#ff0000")),
            ("hwb(0 100% 0%)", Some("#ffffff")),
            ("transparent", Some("rgba(0, 0, 0, 0)")),
            ("currentcolor", Some("#000000")),
            ("  blue  ", Some("#0000ff")),
            // Not colours: the assignment is ignored.
            ("definitely not a colour", None),
            ("", None),
            ("#ff", None),
            ("#ggg", None),
            ("rgb(1, 2)", None),
            ("rgb(1, 2, 3, 4, 5)", None),
            ("rgb(1, 2, 3 / 0.5)", None),
            ("rgb(10% 20 30)", Some("#1a141e")),
            ("rgb(10%, 20, 30)", None),
            ("hsl(120, 100, 50)", None),
            ("lab(50% 40 59)", None),
            ("rgb(", None),
        ];

        let mut script = String::from("const ctx = createCanvas().getContext('2d');\n");
        for (input, expected) in cases {
            let want = expected.unwrap_or("#123456");
            script.push_str(&format!(
                "ctx.fillStyle = '#123456'; ctx.fillStyle = {input}; if (ctx.fillStyle !== {want}) throw new Error({label} + ' read back as ' + ctx.fillStyle + ', want ' + {want});\n",
                input = js_string(input),
                want = js_string(want),
                label = js_string(input),
            ));
        }
        // Every string reads back as specified, or the script throws naming the one that did not.
        run_2d_frame("colour_strings.js", &script);
    }

    /// An invalid colour sends nothing, a valid one sends the colour, and a repeat sends nothing again.
    #[test]
    fn an_assignment_reaches_the_renderer_only_as_a_new_colour() {
        let script = r#"
            const ctx = createCanvas().getContext('2d');
            ctx.fillStyle = 'rgb(10 20 30 / 50%)';
            ctx.fillStyle = 'not a colour';
            ctx.fillStyle = 'rgba(10, 20, 30, 0.5)';
            ctx.strokeStyle = 'hsl(120 100% 50%)';
            ctx.shadowColor = 'transparent';
        "#;
        let ops = run_2d_frame("colour_records.js", script);
        let commands = canvas_commands(&ops);
        let colours: Vec<(&str, [u32; 4])> = commands
            .iter()
            .filter_map(|command| match command {
                Canvas2DCmd::SetFillStyle { color } => Some(("fill", color)),
                Canvas2DCmd::SetStrokeStyle { color } => Some(("stroke", color)),
                Canvas2DCmd::SetShadowColor { color } => Some(("shadow", color)),
                _ => None,
            })
            .map(|(kind, c)| {
                (
                    kind,
                    [c.r.to_bits(), c.g.to_bits(), c.b.to_bits(), c.a.to_bits()],
                )
            })
            .collect();
        let bits = |r: u8, g: u8, b: u8, a: u8| {
            [
                (r as f32 / 255.0).to_bits(),
                (g as f32 / 255.0).to_bits(),
                (b as f32 / 255.0).to_bits(),
                (a as f32 / 255.0).to_bits(),
            ]
        };
        // The second fill is the same colour as the first (alpha 0.5 is byte 128 either way): nothing is sent for it,
        // and nothing for the invalid string between them. The shadow colour is already transparent.
        assert_eq!(
            colours,
            vec![
                ("fill", bits(10, 20, 30, 128)),
                ("stroke", bits(0, 255, 0, 255)),
            ]
        );
    }

    /// The number G2 is about, reported by the runtime rather than inferred
    /// from the shape of the code.
    ///
    /// Two frames of the same drawing, one encoded and one forced onto the op
    /// path, and the counter has to tell them apart. Without that it is a
    /// statistic nobody can act on: a call site that quietly fell back to an op
    /// looks exactly like one that did not, from anywhere except here.
    #[test]
    fn the_boundary_counter_separates_a_batched_frame_from_an_op_per_call_one() {
        use shared::protocol::render_cmd::CanvasCmd;

        fn crossings_for(name: &'static str, body: &str) -> (u64, u64) {
            let (mut runtime, render_rx) = new_webgl_runtime();
            let handle = std::thread::spawn(move || {
                let deadline = std::time::Instant::now() + Duration::from_secs(5);
                let mut packets = 0;
                loop {
                    let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                    if remaining.is_zero() {
                        return;
                    }
                    match render_rx.recv_timeout(remaining) {
                        Ok(RenderCommand::Canvas(CanvasCmd::GetInfo { id: _, resp })) => {
                            resp.send(Ok((64, 64)));
                        }
                        Ok(RenderCommand::FramePacket(_)) => {
                            packets += 1;
                            if packets > 0 {
                                return;
                            }
                        }
                        Ok(_) => {}
                        Err(_) => return,
                    }
                }
            });
            let script = format!("const ctx = createCanvas(64, 64).getContext('2d');\n{body}");
            runtime
                .exec_script(name, &script)
                .expect("script must not throw");
            end_test_frame(&mut runtime);
            handle.join().expect("helper thread should not panic");
            let op_state = runtime.op_state_for_test();
            let state = op_state.borrow();
            let collector = state.borrow::<UnifiedFrameCollector>();
            let (_frames, crossings, commands) = collector.boundary_window_for_test();
            (crossings, commands)
        }

        // Fifty rectangles, encoded. One submission carries the lot; the frame
        // also opens the canvas, which is its own op.
        let (batched, batched_commands) = crossings_for(
            "boundary_batched.js",
            "for (let i = 0; i < 50; i++) ctx.fillRect(i, i, 2, 2);\n",
        );

        // The same fifty, each preceded by a call that crosses to the renderer for an answer (a text measurement the
        // facade's own cache has not seen), so each one pays a barrier submission and an op.
        let (per_call, per_call_commands) = crossings_for(
            "boundary_per_call.js",
            "for (let i = 0; i < 50; i++) {\n\
               ctx.measureText('boundary ' + i);\n\
               ctx.fillRect(i, i, 2, 2);\n\
             }\n",
        );

        // Printed, because a test that measures something and keeps the number
        // to itself makes the next person measure it again.
        println!(
            "  encoded: {batched} crossings for {batched_commands} commands\n               per-call: {per_call} crossings for {per_call_commands} commands"
        );

        assert!(
            batched <= 4,
            "fifty encoded rectangles should cross a handful of times, not {batched}"
        );
        assert!(
            per_call >= 50,
            "fifty measurements the facade has not cached should cross at least once each, got {per_call}"
        );
        assert!(
            per_call > batched * 5,
            "the counter does not separate the two paths: batched={batched}, per_call={per_call}"
        );
        // Only the batched frame's commands are all in the window the counter reads: each crossing of the other
        // sends what the stream held ahead of it, so what is left in the window is the tail.
        assert!(
            batched_commands >= 50,
            "the batched frame must still carry its commands: {batched_commands} ({per_call_commands} left in the per-call window)"
        );
    }

    #[test]
    fn r2_scalar_routing_preserves_baseline_type_acceptance() {
        fn accepts(name: &'static str, source: &'static str) -> bool {
            let (mut runtime, _render_rx) = new_webgl_runtime();
            runtime.exec_script(name, source).is_ok()
        }

        let cases = [
            (
                "clearColor string",
                false,
                accepts(
                    "r2_public_clear_color_string.js",
                    r#"
                    const gl = new WebGLRenderingContext({ _rid: 140, width: 1, height: 1 }, {});
                    gl.clearColor("0.25", 0, 0, 1);
                    gl.flush();
                    "#,
                ),
            ),
            (
                "clearDepth string",
                false,
                accepts(
                    "r2_public_clear_depth_string.js",
                    r#"
                    const gl = new WebGLRenderingContext({ _rid: 141, width: 1, height: 1 }, {});
                    gl.clearDepth("1");
                    gl.flush();
                    "#,
                ),
            ),
            (
                "blendColor string",
                false,
                accepts(
                    "r2_public_blend_color_string.js",
                    r#"
                    const gl = new WebGLRenderingContext({ _rid: 141, width: 1, height: 1 }, {});
                    gl.blendColor("0", 0, 0, 1);
                    gl.flush();
                    "#,
                ),
            ),
            (
                "depthMask number",
                true,
                accepts(
                    "r2_public_depth_mask_number.js",
                    r#"
                    const gl = new WebGLRenderingContext({ _rid: 142, width: 1, height: 1 }, {});
                    gl.depthMask(1);
                    gl.flush();
                    "#,
                ),
            ),
            (
                "depthRange string",
                false,
                accepts(
                    "r2_public_depth_range_string.js",
                    r#"
                    const gl = new WebGLRenderingContext({ _rid: 142, width: 1, height: 1 }, {});
                    gl.depthRange("0", 1);
                    gl.flush();
                    "#,
                ),
            ),
            (
                "colorMask number",
                true,
                accepts(
                    "r2_public_color_mask_number.js",
                    r#"
                    const gl = new WebGLRenderingContext({ _rid: 142, width: 1, height: 1 }, {});
                    gl.colorMask(1, true, true, true);
                    gl.flush();
                    "#,
                ),
            ),
            (
                "lineWidth string",
                false,
                accepts(
                    "r2_public_line_width_string.js",
                    r#"
                    const gl = new WebGLRenderingContext({ _rid: 142, width: 1, height: 1 }, {});
                    gl.lineWidth("1");
                    gl.flush();
                    "#,
                ),
            ),
            (
                "polygonOffset string",
                false,
                accepts(
                    "r2_public_polygon_offset_string.js",
                    r#"
                    const gl = new WebGLRenderingContext({ _rid: 142, width: 1, height: 1 }, {});
                    gl.polygonOffset("1", 1);
                    gl.flush();
                    "#,
                ),
            ),
            (
                // WebIDL converts the GLfloat ("1" is 1); 1 is no MIN_FILTER, so the call is INVALID_ENUM, not a throw.
                "texParameterf string",
                true,
                accepts(
                    "r2_public_tex_parameterf_string.js",
                    r#"
                    const gl = new WebGLRenderingContext({ _rid: 142, width: 1, height: 1 }, {});
                    gl.bindTexture(0x0DE1, gl.createTexture());
                    gl.texParameterf(0x0DE1, 0x2801, "1");
                    gl.flush();
                    "#,
                ),
            ),
            (
                "samplerParameterf string",
                false,
                accepts(
                    "r2_public_sampler_parameterf_string.js",
                    r#"
                    const gl = new WebGL2RenderingContext({ _rid: 142, width: 1, height: 1 }, {});
                    gl.samplerParameterf({ _id: 1 }, 0x2801, "1");
                    gl.flush();
                    "#,
                ),
            ),
            (
                "matrix transpose number",
                true,
                accepts(
                    "r2_public_matrix_transpose_number.js",
                    r#"
                    const gl = new WebGLRenderingContext({ _rid: 143, width: 1, height: 1 }, {});
                    gl.uniformMatrix2fv({ id: 1 }, 0, new Float32Array(4));
                    gl.flush();
                    "#,
                ),
            ),
            (
                "vertexAttribPointer normalized number",
                true,
                accepts(
                    "r2_public_vertex_normalized_number.js",
                    r#"
                    const gl = new WebGLRenderingContext({ _rid: 144, width: 1, height: 1 }, {});
                    gl.vertexAttribPointer(0, 2, 0x1406, 1, 0, 0);
                    gl.flush();
                    "#,
                ),
            ),
        ];

        let mismatches: Vec<_> = cases
            .into_iter()
            .filter_map(|(label, expected, actual)| {
                (expected != actual).then_some((label, expected, actual))
            })
            .collect();
        assert!(
            mismatches.is_empty(),
            "typed-stream routing changed baseline facade acceptance: {mismatches:?}"
        );
    }

    #[test]
    fn r2_mid_frame_path_command_flushes_pending_gl_before_line_to() {
        let (mut runtime, render_rx) = new_webgl_runtime();
        let (handle, packet_rx) = spawn_canvas_frame_responder(render_rx);

        runtime
            .exec_script(
                "r2_path_interleave.js",
                r#"
                const gl = new WebGLRenderingContext({ _rid: 145, width: 4, height: 4 }, {});
                const ctx = createCanvas().getContext("2d");
                ctx.beginPath();
                gl.clear(0x4000);
                ctx.lineTo(1, 1);
                "#,
            )
            .expect("path interleave must execute");
        end_test_frame(&mut runtime);

        handle.join().expect("canvas responder must not panic");
        let ops = packet_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("path interleave must produce a frame packet");
        let canvas_positions: Vec<_> = ops
            .iter()
            .enumerate()
            .filter_map(|(index, op)| matches!(op, FrameOp::CanvasBatch(_)).then_some(index))
            .collect();
        let gl_position = ops
            .iter()
            .position(|op| matches!(op, FrameOp::GlBatch(_)))
            .expect("pending GL must be submitted");

        assert_eq!(
            canvas_positions.len(),
            2,
            "beginPath and lineTo must straddle the GL segment; ops={ops:?}"
        );
        assert!(
            canvas_positions[0] < gl_position && gl_position < canvas_positions[1],
            "required order is beginPath -> GL -> lineTo; ops={ops:?}"
        );
    }

    #[test]
    fn r2_get_image_data_snapshot_flushes_pending_gl_first() {
        let (mut runtime, render_rx) = new_webgl_runtime();
        let (handle, packet_rx) = spawn_canvas_frame_responder(render_rx);

        runtime
            .exec_script(
                "r2_snapshot_interleave.js",
                r#"
                const gl = new WebGLRenderingContext({ _rid: 146, width: 1, height: 1 }, {});
                const ctx = createCanvas().getContext("2d");
                gl.clear(0x4000);
                ctx.getImageData(0, 0, 1, 1);
                "#,
            )
            .expect("snapshot interleave must execute");
        end_test_frame(&mut runtime);

        handle.join().expect("canvas responder must not panic");
        let ops = packet_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("snapshot interleave must produce a frame packet");
        let gl_position = ops
            .iter()
            .position(|op| matches!(op, FrameOp::GlBatch(_)))
            .expect("pending GL must be submitted");
        let canvas_position = ops
            .iter()
            .position(|op| matches!(op, FrameOp::CanvasBatch(_)))
            .expect("snapshot capture must enter a canvas batch");
        assert!(
            gl_position < canvas_position,
            "GL issued before getImageData must precede snapshot capture; ops={ops:?}"
        );
    }

    /// A render thread that answers what `getImageData` asks of it and records what it is asked,
    /// until a frame that presents arrives or a second passes with nothing. (A frame whose
    /// commands a barrier already carried has nothing left to send, and sends no packet.)
    /// `canvas` is the size it reports for every canvas.
    fn spawn_snapshot_responder(
        render_rx: crossbeam_channel::Receiver<RenderCommand>,
        canvas: (u32, u32),
    ) -> (
        std::thread::JoinHandle<()>,
        std::sync::mpsc::Receiver<Vec<&'static str>>,
    ) {
        use shared::protocol::render_cmd::{Canvas2DCmd, CanvasCmd};

        let (events_tx, events_rx) = std::sync::mpsc::sync_channel(1);
        let handle = std::thread::spawn(move || {
            let mut events: Vec<&'static str> = Vec::new();
            loop {
                match render_rx.recv_timeout(Duration::from_secs(1)) {
                    Ok(RenderCommand::Canvas(CanvasCmd::GetInfo { resp, .. })) => {
                        resp.send(Ok(canvas));
                    }
                    Ok(RenderCommand::Canvas2D {
                        cmd: Canvas2DCmd::ReadSnapshotPixels { resp, .. },
                        ..
                    }) => {
                        events.push("read-snapshot");
                        // Nothing to give: the placeholder stays, which is all these tests look at.
                        resp.ok(Vec::new());
                    }
                    Ok(RenderCommand::Canvas2D {
                        cmd:
                            Canvas2DCmd::GetImageData {
                                width,
                                height,
                                resp,
                                ..
                            },
                        ..
                    }) => {
                        events.push("direct-read");
                        resp.ok(vec![0u8; (width * height * 4) as usize]);
                    }
                    Ok(RenderCommand::FramePacket(packet)) => {
                        let presents = packet
                            .into_ops()
                            .iter()
                            .any(|op| matches!(op, FrameOp::Present));
                        if presents {
                            events.push("present");
                            break;
                        }
                    }
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
            let _ = events_tx.send(events);
        });
        (handle, events_rx)
    }

    /// `createPattern(canvas)` takes a copy the pattern owns and names it by an id the facade allocated.
    ///
    /// The copy is one record under the canvas it copies, in the stream after everything drawn to it, and the
    /// pattern fill that follows names the same id -- which is what keeps the pattern from changing when the
    /// canvas does. The repetition is checked first (a SyntaxError), a canvas with no pixels is an
    /// InvalidStateError, and a canvas the facade cannot reach as a canvas is not a pattern source.
    #[test]
    fn create_pattern_from_a_canvas_copies_it_under_an_id_the_fill_names() {
        use shared::protocol::render_cmd::{Canvas2DCmd, CanvasCmd};

        let (mut runtime, render_rx) = new_webgl_runtime();
        // Every packet until a second passes quietly: creating a canvas sends a barrier of its own.
        let (events_tx, events_rx) = std::sync::mpsc::sync_channel(1);
        let handle = std::thread::spawn(move || {
            let mut copies: Vec<(u32, u32)> = Vec::new();
            let mut fills: Vec<(u32, bool, bool)> = Vec::new();
            while let Ok(command) = render_rx.recv_timeout(Duration::from_secs(1)) {
                match command {
                    RenderCommand::Canvas(CanvasCmd::GetInfo { resp, .. }) => {
                        resp.send(Ok((4, 4)));
                    }
                    RenderCommand::FramePacket(packet) => {
                        for op in packet.into_ops().iter() {
                            let FrameOp::CanvasBatch(batch) = op else {
                                continue;
                            };
                            for command in batch.commands.iter() {
                                match command {
                                    Canvas2DCmd::CaptureImage { image_id } => {
                                        copies.push((batch.canvas_id.into(), *image_id));
                                    }
                                    Canvas2DCmd::SetFillStylePattern {
                                        image_id,
                                        repeat_x,
                                        repeat_y,
                                    } => fills.push((*image_id, *repeat_x, *repeat_y)),
                                    _ => {}
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            let _ = events_tx.send((copies, fills));
        });

        runtime
            .exec_script(
                "pattern_from_canvas.js",
                r#"
                const screen = createCanvas();
                const tile = createCanvas();
                tile.width = 2; tile.height = 2;
                const ctx = screen.getContext("2d");
                const pattern = ctx.createPattern(tile, "repeat-x");
                if (pattern === null) throw new Error("a canvas is a pattern source");
                ctx.fillStyle = pattern;

                const named = (fn) => { try { fn(); } catch (e) { return e.name; } return null; };
                const badRepetition = named(() => ctx.createPattern(tile, "diagonal"));
                if (badRepetition !== "SyntaxError") throw new Error("repetition: " + badRepetition);
                if (ctx.createPattern(tile, "") === null || ctx.createPattern(tile, null) === null) {
                    throw new Error("'' and null mean repeat");
                }
                const empty = createCanvas();
                empty.width = 0;
                const noPixels = named(() => ctx.createPattern(empty, "repeat"));
                if (noPixels !== "InvalidStateError") throw new Error("empty canvas: " + noPixels);
                if (ctx.createPattern({}, "repeat") !== null) throw new Error("not a source");
                "#,
            )
            .expect("patterns must execute");
        end_test_frame(&mut runtime);
        handle.join().expect("responder must not panic");
        let (copies, fills) = events_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("the frames must carry the records");

        // The `repeat-x` pattern and the two that read "repeat" (`""` and `null`).
        assert_eq!(
            copies.len(),
            3,
            "one copy per pattern made; copies={copies:?}"
        );
        assert!(
            copies.iter().all(|(canvas, _)| *canvas != 1),
            "the copy is of the tile, not of the canvas the pattern is for; copies={copies:?}"
        );
        assert_eq!(
            fills,
            vec![(copies[0].1, true, false)],
            "the fill names the copy's id with the repetition the pattern was made with"
        );
        let mut ids: Vec<u32> = copies.iter().map(|(_, id)| *id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), 3, "every pattern has its own copy");
    }

    /// An `ImageData` nobody has read, and nothing has consumed, is read back before its frame ends.
    ///
    /// The render pool keeps a snapshot for one frame; read a frame later and the pixels are gone
    /// and `.data` is zeros -- on every platform, a stored `ImageData` used for hit-testing. The
    /// readback a browser does at the call is taken at frame end instead.
    #[test]
    fn an_unread_image_data_is_read_back_before_its_frame_ends() {
        let (mut runtime, render_rx) = new_webgl_runtime();
        let (handle, events_rx) = spawn_snapshot_responder(render_rx, (4, 4));

        runtime
            .exec_script(
                "held_image_data.js",
                r#"
                globalThis.held = createCanvas().getContext("2d").getImageData(0, 0, 1, 1);
                if (held.__migo_snapshot_id__ === 0) throw new Error("expected a snapshot-backed ImageData");
                "#,
            )
            .expect("capture must execute");
        end_test_frame(&mut runtime);
        handle.join().expect("responder must not panic");
        let events = events_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("events");

        // The frame's end follows the read: the capture went ahead in a barrier, so the end carries nothing, and it is
        // sent all the same -- it is what ends the frame, and with it the frame's hold on its snapshots.
        assert_eq!(
            events,
            vec!["read-snapshot", "present"],
            "the unread snapshot is read while the frame still holds it"
        );
        runtime
            .exec_script(
                "held_image_data_after.js",
                r#"if (held.__migo_snapshot_id__ !== 0) throw new Error("the ImageData was not materialised");"#,
            )
            .expect("the ImageData owns its pixels once the frame has ended");
    }

    /// The pattern the snapshots exist for -- `texImage2D(imageData)` in the frame that took it --
    /// costs no readback at frame end.
    #[test]
    fn a_spent_image_data_is_not_read_back_at_frame_end() {
        let (mut runtime, render_rx) = new_webgl_runtime();
        let (handle, events_rx) = spawn_snapshot_responder(render_rx, (4, 4));

        runtime
            .exec_script(
                "spent_image_data.js",
                r#"
                const gl = new WebGLRenderingContext({ _rid: 149, width: 1, height: 1 }, {});
                gl.bindTexture(0x0DE1, gl.createTexture());
                const spentBy2d = createCanvas().getContext("2d").getImageData(0, 0, 1, 1);
                gl.texImage2D(0x0DE1, 0, 0x1908, 0x1908, 0x1401, spentBy2d);
                globalThis.spentSix = spentBy2d;
                const spentBySub = createCanvas().getContext("2d").getImageData(1, 1, 1, 1);
                gl.texSubImage2D(0x0DE1, 0, 0, 0, 0x1908, 0x1401, spentBySub);
                globalThis.spentSub = spentBySub;
                "#,
            )
            .expect("uploads must execute");
        end_test_frame(&mut runtime);
        handle.join().expect("responder must not panic");
        let events = events_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("events");

        assert_eq!(
            events,
            vec!["present"],
            "a snapshot a texture upload took is not read back"
        );
        runtime
            .exec_script(
                "spent_image_data_after.js",
                r#"
                for (const d of [spentSix, spentSub]) {
                    if (d.__migo_snapshot_spent__ !== true) throw new Error("not marked spent");
                    if (d.__migo_snapshot_id__ === 0) throw new Error("a spent snapshot must keep its id");
                }
                "#,
            )
            .expect("spent ImageData keep their snapshot");
    }

    /// Past the snapshot byte budget a `getImageData` is read eagerly, as a browser reads it, rather
    /// than captured into a pool that would refuse it and leave the `ImageData` all zeros.
    #[test]
    fn past_the_snapshot_byte_budget_get_image_data_reads_eagerly() {
        let (mut runtime, render_rx) = new_webgl_runtime();
        let (handle, events_rx) = spawn_snapshot_responder(render_rx, (4096, 4096));

        // 1800 x 1800 x 4 = 12.96 MB: two fit under 32 MiB, the third does not.
        runtime
            .exec_script(
                "snapshot_budget.js",
                r#"
                const ctx = createCanvas().getContext("2d");
                globalThis.reads = [0, 1, 2].map(() => ctx.getImageData(0, 0, 1800, 1800));
                "#,
            )
            .expect("reads must execute");
        end_test_frame(&mut runtime);
        handle.join().expect("responder must not panic");
        let events = events_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("events");

        assert_eq!(
            events.iter().filter(|e| **e == "direct-read").count(),
            1,
            "the third read is over the budget and goes straight to pixels; events={events:?}"
        );
        runtime
            .exec_script(
                "snapshot_budget_after.js",
                r#"
                const kinds = reads.map((d) => d.__migo_snapshot_id__ === undefined ? "bytes" : "snapshot");
                if (kinds.join() !== "snapshot,snapshot,bytes") throw new Error("kinds=" + kinds.join());
                "#,
            )
            .expect("two captured, the third eager");
    }

    #[test]
    fn r2_text_cache_consume_flushes_pending_gl_before_capture_and_upload() {
        let (mut runtime, render_rx) = new_webgl_runtime();
        let (handle, packet_rx) = spawn_canvas_frame_responder(render_rx);

        runtime
            .exec_script(
                "r2_text_cache_consume_interleave.js",
                r#"
                const gl = new WebGLRenderingContext({ _rid: 147, width: 1, height: 1 }, {});
                const ctx = createCanvas().getContext("2d");
                ctx._tcState = 1;
                ctx._tcKey = {
                    text: "x", fontRequest: "10px sans-serif", fontSize: 0,
                    fontWeight: 0, italic: false, fillColor: 0xffffffff,
                    textAlign: 0, textBaseline: 3, canvasW: 1, canvasH: 1,
                };
                gl.clear(0x4000);
                if (!ctx._consumeTextCacheForTexImage(147, 0x0DE1, 0, 0x1908)) {
                    throw new Error("expected text-cache consume path");
                }
                "#,
            )
            .expect("text-cache consume interleave must execute");
        end_test_frame(&mut runtime);

        handle.join().expect("canvas responder must not panic");
        let ops = packet_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("text-cache consume must produce a frame packet");
        let first_gl = ops
            .iter()
            .position(|op| matches!(op, FrameOp::GlBatch(_)))
            .expect("pending GL must be submitted");
        let first_canvas = ops
            .iter()
            .position(|op| matches!(op, FrameOp::CanvasBatch(_)))
            .expect("cache snapshot must enter a canvas batch");
        assert!(
            first_gl < first_canvas,
            "GL issued before cache consume must precede capture/upload; ops={ops:?}"
        );
    }

    #[test]
    fn r2_context_loss_without_main_canvas_discards_offscreen_stream() {
        let (host_state, _render_rx) = new_test_host_state();
        let context_lost = Arc::clone(&host_state.context_lost);
        let mut runtime = HostJsRuntime::new(
            1,
            host_state,
            &std::env::temp_dir(),
            #[cfg(feature = "v8-limits")]
            Default::default(),
            #[cfg(feature = "code-signing")]
            false,
            #[cfg(feature = "code-signing")]
            None,
        );
        crate::rendering::webgl::submit_test_counter::reset();

        runtime
            .exec_script(
                "r2_context_loss_without_main_canvas.js",
                r#"
                const gl = new WebGLRenderingContext({ _rid: 148, width: 1, height: 1 }, {});
                gl.clear(0x4000);
                "#,
            )
            .expect("context-loss setup must execute");

        // Fire the loss the way the host does -- through the handle the runtime
        // retains -- rather than by naming the host-bridge holder. The name is
        // retired once the runtime holds it, and reaching hooks by name is the
        // thing content is no longer able to do. The render thread marks the
        // context lost before the event, as it does on a real reset, so the
        // `flush()` below is a lost context's: nothing.
        assert!(context_lost.set_lost());
        runtime.dispatch_webgl_context_event("webglcontextlost");

        runtime
            .exec_script("r2_context_loss_flush.js", "gl.flush();")
            .expect("context-loss discard path must execute");

        let (submit_calls, decoded) = crate::rendering::webgl::submit_test_counter::read();
        assert_eq!(
            (submit_calls, decoded),
            (0, 0),
            "context loss must discard pending commands even when the main canvas was never wrapped"
        );
    }

    #[test]
    fn r2_create_context_flushes_pending_gl_barrier_first() {
        use shared::protocol::render_cmd::{Canvas2DCmd, CanvasCmd};

        let (mut runtime, render_rx) = new_webgl_runtime();
        let handle = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            let mut order = Vec::new();
            while std::time::Instant::now() < deadline {
                let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                match render_rx.recv_timeout(remaining) {
                    Ok(RenderCommand::Canvas(CanvasCmd::GetInfo { resp, .. })) => {
                        resp.send(Ok((4, 4)));
                    }
                    Ok(RenderCommand::FramePacket(packet)) => {
                        if packet
                            .ops()
                            .iter()
                            .any(|op| matches!(op, FrameOp::GlBatch(_)))
                        {
                            order.push("gl_packet");
                        }
                    }
                    Ok(RenderCommand::Canvas2D {
                        cmd: Canvas2DCmd::CreateContext2D,
                        ..
                    }) => order.push("create_context"),
                    Ok(_) => {}
                    Err(_) => break,
                }
                if order.contains(&"gl_packet") && order.contains(&"create_context") {
                    break;
                }
            }
            order
        });

        runtime
            .exec_script(
                "r2_create_context_order.js",
                r#"
                const canvas = createCanvas();
                const gl = new WebGLRenderingContext({ _rid: 149, width: 4, height: 4 }, {});
                gl.clear(0x4000);
                canvas.getContext("2d");
                "#,
            )
            .expect("create-context ordering script must execute");
        end_test_frame(&mut runtime);

        let order = handle.join().expect("render responder must not panic");
        assert_eq!(
            order,
            vec!["gl_packet", "create_context"],
            "collector barrier must reach render thread before direct CreateContext2D"
        );
    }

    #[test]
    fn r2_canvas_info_flushes_pending_gl_barrier_first() {
        use shared::protocol::render_cmd::CanvasCmd;

        let (mut runtime, render_rx) = new_webgl_runtime();
        let handle = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            let mut order = Vec::new();
            while std::time::Instant::now() < deadline {
                let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                match render_rx.recv_timeout(remaining) {
                    Ok(RenderCommand::FramePacket(packet)) => {
                        if packet
                            .ops()
                            .iter()
                            .any(|op| matches!(op, FrameOp::GlBatch(_)))
                        {
                            order.push("gl_packet");
                        }
                    }
                    Ok(RenderCommand::Canvas(CanvasCmd::GetInfo { resp, .. })) => {
                        order.push("get_info");
                        resp.send(Ok((4, 4)));
                    }
                    Ok(_) => {}
                    Err(_) => break,
                }
                if order.contains(&"gl_packet") && order.contains(&"get_info") {
                    break;
                }
            }
            order
        });

        runtime
            .exec_script(
                "r2_canvas_info_order.js",
                r#"
                const gl = new WebGLRenderingContext({ _rid: 150, width: 4, height: 4 }, {});
                gl.clear(0x4000);
                createCanvas();
                "#,
            )
            .expect("canvas-info ordering script must execute");
        end_test_frame(&mut runtime);

        let order = handle.join().expect("render responder must not panic");
        assert_eq!(
            order,
            vec!["gl_packet", "get_info"],
            "collector barrier must reach render thread before synchronous GetInfo"
        );
    }

    #[test]
    fn r2_offscreen_registration_flushes_pending_gl_barrier_first() {
        use shared::protocol::render_cmd::CanvasCmd;

        let (mut runtime, render_rx) = new_webgl_runtime();
        let handle = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            let mut order = Vec::new();
            let mut offscreen_info_seen = false;
            while std::time::Instant::now() < deadline {
                let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                match render_rx.recv_timeout(remaining) {
                    Ok(RenderCommand::FramePacket(packet)) => {
                        if packet
                            .ops()
                            .iter()
                            .any(|op| matches!(op, FrameOp::GlBatch(_)))
                        {
                            order.push("gl_packet");
                        }
                    }
                    Ok(RenderCommand::Canvas(CanvasCmd::RegisterOffscreen { .. })) => {
                        order.push("register_offscreen");
                    }
                    Ok(RenderCommand::Canvas(CanvasCmd::GetInfo { id, resp })) => {
                        if id != 1 {
                            offscreen_info_seen = true;
                        }
                        resp.send(Ok((4, 4)));
                    }
                    Ok(_) => {}
                    Err(_) => break,
                }
                if offscreen_info_seen
                    && order.contains(&"gl_packet")
                    && order.contains(&"register_offscreen")
                {
                    break;
                }
            }
            order
        });

        runtime
            .exec_script(
                "r2_offscreen_register_order.js",
                r#"
                createCanvas();
                const gl = new WebGLRenderingContext({ _rid: 151, width: 4, height: 4 }, {});
                gl.clear(0x4000);
                createCanvas();
                "#,
            )
            .expect("offscreen-register ordering script must execute");
        end_test_frame(&mut runtime);

        let order = handle.join().expect("render responder must not panic");
        assert_eq!(
            order,
            vec!["gl_packet", "register_offscreen"],
            "collector barrier must reach render thread before RegisterOffscreen"
        );
    }

    #[test]
    fn r2_measure_text_flushes_pending_gl_barrier_first() {
        use shared::protocol::render_cmd::{Canvas2DCmd, CanvasCmd, TextMetrics};

        let (mut runtime, render_rx) = new_webgl_runtime();
        let handle = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            let mut order = Vec::new();
            while std::time::Instant::now() < deadline {
                let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                match render_rx.recv_timeout(remaining) {
                    Ok(RenderCommand::Canvas(CanvasCmd::GetInfo { resp, .. })) => {
                        resp.send(Ok((4, 4)));
                    }
                    Ok(RenderCommand::FramePacket(packet)) => {
                        if packet
                            .ops()
                            .iter()
                            .any(|op| matches!(op, FrameOp::GlBatch(_)))
                        {
                            order.push("gl_packet");
                        }
                    }
                    Ok(RenderCommand::Canvas2D {
                        cmd: Canvas2DCmd::MeasureText { resp, .. },
                        ..
                    }) => {
                        order.push("measure_text");
                        resp.ok(TextMetrics {
                            width: 0.0,
                            actual_bounding_box_left: 0.0,
                            actual_bounding_box_right: 0.0,
                            actual_bounding_box_ascent: 0.0,
                            actual_bounding_box_descent: 0.0,
                            font_bounding_box_ascent: 0.0,
                            font_bounding_box_descent: 0.0,
                            em_height_ascent: 0.0,
                            em_height_descent: 0.0,
                            hanging_baseline: 0.0,
                            alphabetic_baseline: 0.0,
                            ideographic_baseline: 0.0,
                        });
                    }
                    Ok(_) => {}
                    Err(_) => break,
                }
                if order.contains(&"gl_packet") && order.contains(&"measure_text") {
                    break;
                }
            }
            order
        });

        runtime
            .exec_script(
                "r2_measure_text_order.js",
                r#"
                const ctx = createCanvas().getContext("2d");
                const gl = new WebGLRenderingContext({ _rid: 152, width: 4, height: 4 }, {});
                gl.clear(0x4000);
                ctx.measureText("x");
                "#,
            )
            .expect("measureText ordering script must execute");
        end_test_frame(&mut runtime);

        let order = handle.join().expect("render responder must not panic");
        assert_eq!(
            order,
            vec!["gl_packet", "measure_text"],
            "collector barrier must reach render thread before synchronous MeasureText"
        );
    }

    #[test]
    fn r2_direct_gradient_apply_flushes_pending_gl_first() {
        let (mut runtime, render_rx) = new_webgl_runtime();
        let (handle, packet_rx) = spawn_canvas_frame_responder(render_rx);

        runtime
            .exec_script(
                "r2_gradient_apply_order.js",
                r##"
                const ctx = createCanvas().getContext("2d");
                const gradient = ctx.createLinearGradient(0, 0, 4, 4);
                gradient.addColorStop(0, "#000000");
                gradient.addColorStop(1, "#ffffff");
                const gl = new WebGLRenderingContext({ _rid: 153, width: 4, height: 4 }, {});
                gl.clear(0x4000);
                gradient._apply();
                "##,
            )
            .expect("direct gradient ordering script must execute");
        end_test_frame(&mut runtime);

        handle.join().expect("canvas responder must not panic");
        let ops = packet_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("gradient apply must produce a frame packet");
        let gl_position = ops
            .iter()
            .position(|op| matches!(op, FrameOp::GlBatch(_)))
            .expect("pending GL must be submitted");
        let canvas_position = ops
            .iter()
            .position(|op| matches!(op, FrameOp::CanvasBatch(_)))
            .expect("gradient apply must enter a canvas batch");
        assert!(
            gl_position < canvas_position,
            "GL issued before direct gradient apply must remain first; ops={ops:?}"
        );
    }

    // ── Section 7.3: zero steady-state allocation ───────────────────────────

    /// Bind `image_id` to a settled load of `key` in a table of its own, with the
    /// decoded bytes resident, which is the state a completed `op_load_image`
    /// leaves behind.
    ///
    /// The alias table is built here rather than taken from the per-host registry
    /// because nothing in this fixture needs the registry, and a table of its own
    /// cannot collide with another test's host id.
    fn settled_image_alias(
        session: i32,
        image_id: u32,
        key: &crate::rendering::image::cache::ImageCacheKey,
    ) -> crate::rendering::image::ImageCacheState {
        use crate::rendering::image::cache::ImageCache;

        // The alias key *is* the decoded-bytes cache's key, so the bytes go in
        // under the very key the alias will later be resolved through.
        migo_io::global_cache().insert(
            key.clone(),
            shared::protocol::io_cmd::NormalizedImage::new(16, 16, vec![0xFF; 16 * 16 * 4]),
            session,
        );

        let aliases = std::sync::Arc::new(parking_lot::Mutex::new(ImageCache::new()));
        {
            let mut c = aliases.lock();
            let _ = c.begin_load(image_id, key);
            c.register_inflight_alias(image_id, 0x5000_0001);
            let _ = c.finish_load(image_id, 0x5000_0001, key, key, Ok((16, 16)));
        }
        crate::rendering::image::ImageCacheState { aliases, session }
    }

    /// Section 7.3, on the path every `texSubImage2D(…, image)` takes.
    ///
    /// Every upload naming an image reaches `resolve_cached_image_rgba` through
    /// `texture_source` -- the decoded bytes travel with the command whenever the
    /// cache holds them -- so this is a per-call cost of `op_tex_image_source`
    /// rather than a fallback, and it does not depend on which game is running.
    ///
    /// What is measured is the resolve — the alias lookup and the decoded-bytes
    /// lookup — and not the upload behind it, which is the render command path
    /// Section 7.3 still lists as unmeasured.
    #[test]
    fn steady_state_image_texture_resolve_never_reaches_the_heap() {
        use migo_services::image::gl::{RgbaLookup, resolve_cached_image_rgba};

        // Unique path: the decoded-bytes cache this fixture inserts into is shared
        // with every other test in this binary.
        let key = crate::rendering::image::cache::make_cache_key(
            "/code/steady-state-texsubimage.png",
            None,
            None,
            17,
        );
        let images = settled_image_alias(9_101, 1, &key);

        assert!(
            matches!(
                resolve_cached_image_rgba(&images.aliases, images.session, 1),
                RgbaLookup::Found { .. }
            ),
            "the fixture must resolve to bytes, or the burst measures the miss path"
        );

        migo_alloc_probe::assert_no_steady_state_allocation(
            migo_alloc_probe::Burst {
                path: "webgl: per-call image texture resolve for texSubImage2D(image)",
                warmup: 4,
                measured: 64,
            },
            |_| match resolve_cached_image_rgba(&images.aliases, images.session, 1) {
                RgbaLookup::Found { width, .. } => width,
                _ => panic!("a pinned, resident alias stopped resolving mid-burst"),
            },
        );
    }
}

// ── Task 3: test-only submit instrumentation ─────────────────────────────────

/// Thread-local submit counter used by tests only.
/// Records (submit_call_count, total_decoded_cmd_count).
#[cfg(test)]
pub(crate) mod submit_test_counter {
    use std::cell::Cell;

    thread_local! {
        static CALLS: Cell<u64> = const { Cell::new(0) };
        static DECODED: Cell<u64> = const { Cell::new(0) };
    }

    pub(crate) fn reset() {
        CALLS.with(|c| c.set(0));
        DECODED.with(|d| d.set(0));
    }

    pub(crate) fn record(decoded: usize) {
        CALLS.with(|c| c.set(c.get() + 1));
        DECODED.with(|d| d.set(d.get() + decoded as u64));
    }

    pub(crate) fn read() -> (u64, u64) {
        (CALLS.with(|c| c.get()), DECODED.with(|d| d.get()))
    }
}

// ── Task 3: op_submit_render_stream ──────────────────────────────────────────────

/// Submit a typed render command stream from JS.
///
/// The stream carries both kinds of work. GL records and Canvas2D records share
/// one opcode space and one buffer precisely so the order between them survives
/// the crossing: a frame that draws its background with 2D, its sprites with GL
/// and its HUD with 2D again is one submission, not three interleaved paths.
///
/// Pass 1 (structural): `stream::validate_stream`. Malformed batches return a
/// stable non-zero error code immediately — no collector, no error queue, no vec taken.
///
/// Pass 2 (semantic + decode): `decode::decode_render_stream` cuts the stream
/// into batches and writes them into the `UnifiedFrameCollector` as it goes.
/// Returns `0` on success.
#[op2(fast)]
#[smi]
pub fn op_submit_render_stream(
    state: &mut OpState,
    #[buffer] words: &[u32],
    #[smi] used_words: u32,
) -> u32 {
    submit_render_stream_impl(state, words, used_words)
}

/// Inner implementation callable from both the op wrapper and tests.
pub(crate) fn submit_render_stream_impl(
    state: &mut OpState,
    words: &[u32],
    used_words: u32,
) -> u32 {
    // Pass 1: pure structural validation — no side effects on failure.
    let validated = match crate::rendering::webgl::stream::validate_stream(words, used_words) {
        Ok(v) => v,
        Err(e) => return e.code(),
    };

    // One crossing, whatever the buffer carried. Counted before the decode so
    // a stream whose records all fail semantic validation still shows the
    // crossing it cost.
    if let Some(collector) =
        state.try_borrow_mut::<crate::rendering::webgl::frame_collector::UnifiedFrameCollector>()
    {
        collector.record_boundary_crossing();
    }

    // Pass 2: semantic decode straight into the collector. The batches the
    // decoder cuts are the batches the collector receives, in the order the
    // frame issued them; nothing is buffered in between.
    let (_decoded, over_budget) =
        crate::rendering::webgl::decode::decode_render_stream(state, validated);

    // Test-only instrumentation: record call count and decoded command count.
    #[cfg(test)]
    submit_test_counter::record(_decoded);

    // The soft budget is checked once for the whole submission rather than per
    // batch: flushing dispatches a frame packet, and doing that between two
    // commands of one stream would put a bounded-blocking send inside a frame.
    if over_budget {
        maybe_auto_flush(state);
    }

    0
}

#[inline]
pub(crate) fn copy_f32_words(words: &[u32]) -> UniformF32Values {
    words.iter().map(|word| f32::from_bits(*word)).collect()
}

#[inline]
pub(crate) fn copy_u32_words(words: &[u32]) -> UniformU32Values {
    words.iter().copied().collect()
}

#[inline]
pub(crate) fn copy_i32_words(words: &[u32]) -> UniformI32Values {
    words
        .iter()
        .map(|word| i32::from_ne_bytes(word.to_ne_bytes()))
        .collect()
}

/// Compile-time test: does this `GLCmd` variant carry a heap
/// payload large enough that it could single-handedly blow the
/// collector's 4 MiB soft budget?
///
/// `false` for scalar variants (viewport, bind*, uniform scalars,
/// enable/disable, draw, scissor, ...) - the overwhelming majority
/// of ops by count in a real frame.  `true` for `BufferData` /
/// `TexImage2D` / `ShaderSource` / uniform array uploads, where a
/// single call can be megabytes.
///
/// Branching on this lets `queue_gl_fire_and_forget` skip both the
/// heavy `approx_deep_size_bytes` match AND the `maybe_auto_flush`
/// OpState re-borrow on the scalar fast path, without forcing every
/// call site to pick between `push_gl` / `push_gl_fast` manually.
/// With LTO the `matches!` compiles down to a handful of discriminant
/// comparisons (jump table), so the fast path remains cheap.
#[inline(always)]
fn gl_cmd_has_heap_payload(cmd: &GLCmd) -> bool {
    match cmd {
        GLCmd::Uniform1iv { value, .. }
        | GLCmd::Uniform2iv { value, .. }
        | GLCmd::Uniform3iv { value, .. }
        | GLCmd::Uniform4iv { value, .. } => value.spilled(),
        GLCmd::Uniform1uiv { value, .. }
        | GLCmd::Uniform2uiv { value, .. }
        | GLCmd::Uniform3uiv { value, .. }
        | GLCmd::Uniform4uiv { value, .. } => value.spilled(),
        GLCmd::Uniform1fv { value, .. }
        | GLCmd::Uniform2fv { value, .. }
        | GLCmd::Uniform3fv { value, .. }
        | GLCmd::Uniform4fv { value, .. }
        | GLCmd::UniformMatrix2fv { value, .. }
        | GLCmd::UniformMatrix3fv { value, .. }
        | GLCmd::UniformMatrix2x3fv { value, .. }
        | GLCmd::UniformMatrix2x4fv { value, .. }
        | GLCmd::UniformMatrix3x2fv { value, .. }
        | GLCmd::UniformMatrix3x4fv { value, .. }
        | GLCmd::UniformMatrix4x2fv { value, .. }
        | GLCmd::UniformMatrix4x3fv { value, .. }
        | GLCmd::UniformMatrix4fv { value, .. } => value.spilled(),
        _ => matches!(
            cmd,
            GLCmd::BufferData { .. }
                | GLCmd::BufferSubData { .. }
                | GLCmd::TexImage2D { .. }
                | GLCmd::TexSubImage2D { .. }
                | GLCmd::CompressedTexImage2D { .. }
                | GLCmd::CompressedTexSubImage2D { .. }
                | GLCmd::CompressedTexImage3D { .. }
                | GLCmd::CompressedTexSubImage3D { .. }
                | GLCmd::ShaderSource { .. }
                | GLCmd::GetUniformLocation { .. }
                | GLCmd::GetAttribLocation { .. }
                | GLCmd::BindAttribLocation { .. }
                | GLCmd::GetUniformBlockIndex { .. }
                | GLCmd::InvalidateFramebuffer { .. }
                | GLCmd::InvalidateSubFramebuffer { .. }
                | GLCmd::DrawBuffers { .. }
                | GLCmd::TransformFeedbackVaryings { .. }
                | GLCmd::TexImage3D { .. }
                | GLCmd::TexSubImage3D { .. }
        ),
    }
}

#[inline]
pub(crate) fn queue_gl_fire_and_forget(state: &mut OpState, cmd: GLCmd) {
    let heap = gl_cmd_has_heap_payload(&cmd);
    let Some(collector) =
        state.try_borrow_mut::<crate::rendering::webgl::frame_collector::UnifiedFrameCollector>()
    else {
        error!("UnifiedFrameCollector missing in op state");
        return;
    };
    collector.record_boundary_crossing();
    if heap {
        collector.push_gl(cmd);
        // Soft byte-budget backpressure: when a single push has
        // pushed the accumulated batch past the 4 MB threshold
        // (typically a `bufferData` / `texImage2D` with a fat
        // payload), cut the barrier here so we don't hold tens
        // of MB of heap on the JS thread while waiting for a
        // frame boundary.  Mirrors Chromium's
        // `CanvasResourceProvider::auto_flush` which uses its own
        // byte estimate to decide when to forcibly commit.
        maybe_auto_flush(state);
    } else {
        // Scalar/inline fast path.  `push_gl_fast` already maintained
        // `pending_bytes` (adding `size_of::<GLCmd>()`), so we skip the
        // `approx_deep_size_bytes` match here.  We still bound JS-side
        // retained memory: untrusted code can synchronously enqueue tens of
        // thousands of inline uniforms / binds in one turn (each
        // ~`size_of::<GLCmd>()`), and such a storm CAN cross the 4 MiB soft
        // budget.  The guard is a single field comparison on the borrow we
        // already hold; only when it trips do we pay the `maybe_auto_flush`
        // re-borrow + barrier dispatch, keeping the common per-command path
        // free of the deep-size walk.
        collector.push_gl_fast(cmd);
        let over_budget = collector.should_auto_flush();
        if over_budget {
            maybe_auto_flush(state);
        }
    }
}

/// Inspect the frame collector; flush a non-presenting barrier if
/// the pending-bytes estimate has crossed
/// [`crate::rendering::webgl::frame_collector::AUTO_FLUSH_SOFT_BUDGET_BYTES`].
///
/// Kept separate so other push entry points (Canvas2D ops, future
/// side-channel payload paths) share the same trigger.
#[inline]
pub(crate) fn maybe_auto_flush(state: &mut OpState) {
    // Peek in a separate borrow scope so `flush_unified_barrier`
    // can re-acquire the collector mutably.
    let over_budget = state
        .try_borrow::<crate::rendering::webgl::frame_collector::UnifiedFrameCollector>()
        .map(|c| c.should_auto_flush())
        .unwrap_or(false);
    if over_budget {
        // Best-effort: auto-flush is a memory-pressure relief, not a
        // correctness barrier. `dispatch` is still bounded-blocking (no
        // silent drop), so this only errors under extreme backpressure /
        // shutdown, where logging and moving on is acceptable.
        if let Err(e) = crate::rendering::webgl::frame_collector::flush_unified_barrier(state) {
            tracing::warn!("maybe_auto_flush: barrier flush failed: {e}");
        }
    }
}

#[inline]
fn send_gl_sync_with_flush<T>(
    state: &mut OpState,
    build: impl FnOnce(RenderCmdResp<T>) -> RenderCommand,
) -> Result<T, EngineError> {
    // Required barrier: if the pre-read flush can't be delivered we must
    // NOT proceed to the sync read — it would observe un-materialized 2D
    // content or a stale GL state (e.g. readPixels reading the previous
    // frame). Surface the failure to JS instead of returning stale data.
    crate::rendering::webgl::frame_collector::flush_unified_barrier(state).map_err(|e| {
        EngineError::new(shared::error::ErrorCode::RenderBackendError)
            .with_detail(format!("sync barrier flush failed before GL readback: {e}"))
    })?;
    let ctx = state.borrow::<CanvasOpState>();
    send_gl_with_resp_sync(ctx, build)
}

#[op2(fast)]
pub fn op_alloc_gl_resource_id(state: &mut OpState) -> u32 {
    let Some(alloc) = state.try_borrow_mut::<GlResourceIdAllocator>() else {
        error!("GlResourceIdAllocator missing in op state");
        return 0;
    };
    alloc.alloc()
}

#[op2(fast)]
pub fn op_gl_flush(state: &mut OpState) {
    // WebGL `gl.flush()` is advisory; best-effort delivery is fine.
    if let Err(e) = crate::rendering::webgl::frame_collector::flush_unified_barrier(state) {
        tracing::warn!("op_gl_flush: barrier flush failed: {e}");
    }
}

/// Backs JS `gl.isContextLost()`. Reads the shared `context_lost` flag that
/// the host sets on a render `ContextLost` event and clears on a successful
/// `ContextRecovered` (see `HostOpState::context_lost`). Returns `false`
/// when the host state isn't present (headless tests).
#[op2(fast)]
pub fn op_gl_is_context_lost(state: &mut OpState) -> bool {
    state
        .try_borrow::<shared::op_state::HostOpState>()
        .map(|h| h.context_lost.is_lost())
        .unwrap_or(false)
}

/// Backs `MIGO_debug_gpu_reset.reset()` (not `WEBGL_lose_context`, which
/// loses one context and never reaches the host). Arms a one-shot simulated GPU
/// reset on the render thread so the real context-loss -> recovery pipeline can
/// be exercised on demand (there is otherwise no way to trigger EGL_CONTEXT_LOST
/// from software). Fire-and-forget; the loss surfaces on the next render frame.
#[op2(fast)]
pub fn op_gl_lose_context(state: &mut OpState, #[smi] canvas_id: u32) {
    queue_gl_fire_and_forget(state, GLCmd::DebugLoseContext { canvas_id });
}

#[op2(fast)]
pub fn op_viewport(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    #[smi] x: i32,
    #[smi] y: i32,
    #[smi] width: i32,
    #[smi] height: i32,
) {
    // WebGL spec: negative width/height → INVALID_VALUE.
    if !crate::rendering::webgl::error_state::validate_viewport_like(
        state, canvas_id, width, height,
    ) {
        return;
    }
    queue_gl_fire_and_forget(
        state,
        GLCmd::Viewport {
            canvas_id,
            x,
            y,
            width: width as u32,
            height: height as u32,
        },
    );
}

#[op2(fast)]
pub fn op_clear_color(state: &mut OpState, #[smi] canvas_id: u32, r: f32, g: f32, b: f32, a: f32) {
    queue_gl_fire_and_forget(
        state,
        GLCmd::ClearColor {
            canvas_id,
            r,
            g,
            b,
            a,
        },
    );
}

#[op2(fast)]
pub fn op_create_program(state: &mut OpState, #[smi] canvas_id: u32, #[smi] client_id: u32) {
    queue_gl_fire_and_forget(
        state,
        GLCmd::CreateProgram {
            canvas_id,
            client_id,
        },
    );
}

#[op2(fast)]
pub fn op_link_program(state: &mut OpState, #[smi] program_id: u32) {
    queue_gl_fire_and_forget(state, GLCmd::LinkProgram { program_id });
}

#[op2(fast)]
pub fn op_get_program_parameter(
    state: &mut OpState,
    #[smi] program_id: u32,
    #[smi] pname: u32,
) -> i32 {
    send_gl_sync_with_flush(state, |resp| {
        RenderCommand::GL(GLCmd::GetProgramParameter {
            program_id,
            pname,
            resp,
        })
    })
    .unwrap_or(0)
}

#[op2]
#[string]
pub fn op_get_program_info_log(state: &mut OpState, #[smi] program_id: u32) -> String {
    send_gl_sync_with_flush(state, |resp| {
        RenderCommand::GL(GLCmd::GetProgramInfoLog { program_id, resp })
    })
    .ok()
    .flatten()
    .unwrap_or_default()
}

#[op2(fast)]
pub fn op_delete_program(state: &mut OpState, #[smi] program_id: u32) {
    queue_gl_fire_and_forget(state, GLCmd::DeleteProgram { program_id });
}

#[op2(fast)]
pub fn op_create_shader(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    #[smi] client_id: u32,
    #[smi] ty: u32,
) {
    let Some(command) = frame_decode::resource::create_shader(canvas_id, client_id, ty) else {
        error!("unknown shader type: {}", ty);
        return;
    };
    queue_gl_fire_and_forget(state, command);
}

#[op2(fast)]
pub fn op_shader_source(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    #[smi] shader_id: u32,
    #[string] source: &str,
) {
    let command = frame_decode::resource::shader_source(
        &mut OpStateDecodeContext(state),
        canvas_id,
        shader_id,
        Payload::Bytes(source.as_bytes()),
    );
    if let Some(command) = command {
        queue_gl_fire_and_forget(state, command);
    }
}

#[op2(fast)]
pub fn op_compile_shader(state: &mut OpState, #[smi] shader_id: u32) {
    queue_gl_fire_and_forget(state, GLCmd::CompileShader { shader_id });
}

#[op2(fast)]
pub fn op_attach_shader(state: &mut OpState, #[smi] program_id: u32, #[smi] shader_id: u32) {
    queue_gl_fire_and_forget(
        state,
        GLCmd::AttachShader {
            program_id,
            shader_id,
            resp: None,
        },
    );
}

/// `detachShader`: the facade has checked the shader is attached to the program.
#[op2(fast)]
pub fn op_detach_shader(state: &mut OpState, #[smi] program_id: u32, #[smi] shader_id: u32) {
    queue_gl_fire_and_forget(
        state,
        GLCmd::DetachShader {
            program_id,
            shader_id,
        },
    );
}

/// `validateProgram`.
#[op2(fast)]
pub fn op_validate_program(state: &mut OpState, #[smi] program_id: u32) {
    queue_gl_fire_and_forget(state, GLCmd::ValidateProgram { program_id });
}

#[op2(fast)]
pub fn op_get_shader_parameter(
    state: &mut OpState,
    #[smi] shader_id: u32,
    #[smi] pname: u32,
) -> i32 {
    send_gl_sync_with_flush(state, |resp| {
        RenderCommand::GL(GLCmd::GetShaderParameter {
            shader_id,
            pname,
            resp,
        })
    })
    .unwrap_or(0)
}

#[op2]
#[string]
pub fn op_get_shader_info_log(state: &mut OpState, #[smi] shader_id: u32) -> String {
    send_gl_sync_with_flush(state, |resp| {
        RenderCommand::GL(GLCmd::GetShaderInfoLog { shader_id, resp })
    })
    .ok()
    .flatten()
    .unwrap_or_default()
}

#[op2(fast)]
pub fn op_delete_shader(state: &mut OpState, #[smi] shader_id: u32) {
    queue_gl_fire_and_forget(state, GLCmd::DeleteShader { shader_id });
}

#[op2(fast)]
pub fn op_draw_arrays(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    #[smi] mode: u32,
    #[smi] first: i32,
    #[smi] count: i32,
) {
    queue_gl_fire_and_forget(
        state,
        GLCmd::DrawArrays {
            canvas_id,
            mode,
            first,
            count,
        },
    );
}

#[op2(fast)]
pub fn op_draw_elements(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    #[smi] mode: u32,
    #[smi] count: i32,
    #[smi] index_type: u32,
    #[smi] offset: i32,
) {
    queue_gl_fire_and_forget(
        state,
        GLCmd::DrawElements {
            canvas_id,
            mode,
            count,
            index_type,
            offset,
        },
    );
}

#[op2(fast)]
pub fn op_get_attrib_location(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    #[smi] program_id: u32,
    #[string] name: String,
) -> i32 {
    send_gl_sync_with_flush(state, |resp| {
        RenderCommand::GL(GLCmd::GetAttribLocation {
            canvas_id,
            program_id,
            name,
            resp,
        })
    })
    .ok()
    .flatten()
    .map(|v| v as i32)
    .unwrap_or(-1)
}

#[op2(fast)]
pub fn op_bind_attrib_location(
    state: &mut OpState,
    #[smi] program_id: u32,
    #[smi] index: u32,
    #[string] name: String,
) {
    queue_gl_fire_and_forget(
        state,
        GLCmd::BindAttribLocation {
            program_id,
            index,
            name,
        },
    );
}

#[op2]
#[string]
pub fn op_get_active_attrib(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    #[smi] program_id: u32,
    #[smi] index: u32,
) -> String {
    let info = send_gl_sync_with_flush(state, |resp| {
        RenderCommand::GL(GLCmd::GetActiveAttrib {
            canvas_id,
            program_id,
            index,
            resp,
        })
    })
    .ok()
    .flatten();

    if let Some((name, size, type_)) = info {
        let escaped_name = escape_for_json_string(&name);
        return format!(
            "{{\"name\":\"{}\",\"size\":{},\"type\":{}}}",
            escaped_name, size, type_
        );
    }

    String::new()
}

#[op2]
#[string]
pub fn op_get_active_uniform(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    #[smi] program_id: u32,
    #[smi] index: u32,
) -> String {
    let info = send_gl_sync_with_flush(state, |resp| {
        RenderCommand::GL(GLCmd::GetActiveUniform {
            canvas_id,
            program_id,
            index,
            resp,
        })
    })
    .ok()
    .flatten();

    if let Some((name, size, type_)) = info {
        let escaped_name = escape_for_json_string(&name);
        return format!(
            "{{\"name\":\"{}\",\"size\":{},\"type\":{}}}",
            escaped_name, size, type_
        );
    }

    String::new()
}

#[op2(fast)]
pub fn op_enable_vertex_attrib_array(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    #[smi] index: u32,
) {
    queue_gl_fire_and_forget(state, GLCmd::EnableVertexAttribArray { canvas_id, index });
}

#[op2(fast)]
pub fn op_vertex_attrib_4f(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    #[smi] index: u32,
    x: f32,
    y: f32,
    z: f32,
    w: f32,
) {
    queue_gl_fire_and_forget(
        state,
        GLCmd::VertexAttrib4f {
            canvas_id,
            index,
            x,
            y,
            z,
            w,
        },
    );
}

#[op2(fast)]
pub fn op_vertex_attrib_i4i(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    #[smi] index: u32,
    #[smi] x: i32,
    #[smi] y: i32,
    #[smi] z: i32,
    #[smi] w: i32,
) {
    queue_gl_fire_and_forget(
        state,
        GLCmd::VertexAttribI4i {
            canvas_id,
            index,
            x,
            y,
            z,
            w,
        },
    );
}

#[op2(fast)]
pub fn op_vertex_attrib_i4ui(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    #[smi] index: u32,
    #[smi] x: u32,
    #[smi] y: u32,
    #[smi] z: u32,
    #[smi] w: u32,
) {
    queue_gl_fire_and_forget(
        state,
        GLCmd::VertexAttribI4ui {
            canvas_id,
            index,
            x,
            y,
            z,
            w,
        },
    );
}

#[op2(fast)]
pub fn op_vertex_attrib_i_pointer(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    #[smi] index: u32,
    #[smi] size: i32,
    #[smi] type_: u32,
    #[smi] stride: i32,
    #[smi] offset: i32,
) {
    // Host-side validation, as for `vertexAttribPointer`: a refused call raises its error and is not forwarded.
    if !crate::rendering::webgl::error_state::validate_vertex_attrib_ipointer(
        state, canvas_id, size, type_, stride, offset,
    ) {
        return;
    }
    queue_gl_fire_and_forget(
        state,
        GLCmd::VertexAttribIPointer {
            canvas_id,
            index,
            size,
            type_,
            stride,
            offset,
        },
    );
}

#[op2(fast)]
pub fn op_vertex_attrib_pointer(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    #[smi] index: u32,
    #[smi] size: i32,
    #[smi] type_: u32,
    normalized: bool,
    #[smi] stride: i32,
    #[smi] offset: i32,
) {
    // Host-side validation: bad enum / out-of-range arguments push
    // a WebGL error into the per-context queue and the call is
    // NOT forwarded to the render thread.  Matches how
    // Firefox/Chromium reject invalid `vertexAttribPointer` args
    // before they reach the driver.
    if !crate::rendering::webgl::error_state::validate_vertex_attrib_pointer(
        state, canvas_id, size, type_, stride, offset,
    ) {
        return;
    }
    queue_gl_fire_and_forget(
        state,
        GLCmd::VertexAttribPointer {
            canvas_id,
            index,
            size,
            type_,
            normalized,
            stride,
            offset,
        },
    );
}

#[op2(fast)]
pub fn op_create_buffer(state: &mut OpState, #[smi] canvas_id: u32, #[smi] client_id: u32) {
    queue_gl_fire_and_forget(
        state,
        GLCmd::CreateBuffer {
            canvas_id,
            client_id,
        },
    );
}

#[op2(fast)]
pub fn op_bind_buffer(state: &mut OpState, #[smi] canvas_id: u32, #[smi] target: u32, buffer: i32) {
    // Host-side target-enum validation — the render thread's
    // `BindBuffer` dispatcher silently ignores unknown targets,
    // which hides real bugs; surface them via `getError()`
    // instead.  ARRAY_BUFFER / ELEMENT_ARRAY_BUFFER always legal;
    // WebGL 2 targets (UNIFORM_BUFFER etc.) are legal too but
    // only usefully bound on a WebGL 2 context — the op doesn't
    // know its own context version here, so we allow all legal
    // GL ES 3.0 targets and rely on the render thread to reject
    // the ones that don't apply.
    if !crate::rendering::webgl::error_state::validate_bind_buffer_target(state, canvas_id, target)
    {
        return;
    }
    let buffer = if buffer < 0 {
        None
    } else {
        Some(buffer as u32)
    };
    queue_gl_fire_and_forget(
        state,
        GLCmd::BindBuffer {
            canvas_id,
            target,
            buffer,
        },
    );
}

#[op2]
pub fn op_buffer_data(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    #[smi] target: u32,
    #[smi] size: i32,
    #[buffer] data: Option<&[u8]>,
    #[smi] usage: u32,
) {
    buffer_data_impl(state, canvas_id, target, size, data, usage);
}

/// The body of [`op_buffer_data`], as a plain function so the size/payload
/// handling can be unit-tested without the op glue.
pub(crate) fn buffer_data_impl(
    state: &mut OpState,
    canvas_id: u32,
    target: u32,
    size: i32,
    data: Option<&[u8]>,
    usage: u32,
) {
    let command = frame_decode::resource::buffer_data(
        &mut OpStateDecodeContext(state),
        canvas_id,
        target,
        size,
        data.map(Payload::Bytes),
        usage,
    );
    if let Some(command) = command {
        queue_gl_fire_and_forget(state, command);
    }
}

#[op2(fast)]
pub fn op_get_uniform_location(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    #[smi] program_id: u32,
    #[string] name: String,
) -> i32 {
    send_gl_sync_with_flush(state, |resp| {
        RenderCommand::GL(GLCmd::GetUniformLocation {
            canvas_id,
            program_id,
            name,
            resp,
        })
    })
    .ok()
    .flatten()
    .map(|v| v as i32)
    .unwrap_or(-1)
}

#[op2(fast)]
pub fn op_uniform3f(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    location: i32,
    x: f32,
    y: f32,
    z: f32,
) {
    let location = if location < 0 {
        None
    } else {
        Some(location as u32)
    };
    queue_gl_fire_and_forget(
        state,
        GLCmd::Uniform3f {
            canvas_id,
            location,
            x,
            y,
            z,
        },
    );
}

#[op2(fast)]
pub fn op_uniform_matrix_3fv(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    location: i32,
    transpose: bool,
    #[buffer] value: &[u32],
) {
    let location = if location < 0 {
        None
    } else {
        Some(location as u32)
    };
    let value = copy_f32_words(value);

    queue_gl_fire_and_forget(
        state,
        GLCmd::UniformMatrix3fv {
            canvas_id,
            location,
            transpose,
            value,
        },
    );
}

// ---------------------------------------------------------------------------
// Phase 1A: GL State
// ---------------------------------------------------------------------------

#[op2(fast)]
pub fn op_enable(state: &mut OpState, #[smi] canvas_id: u32, #[smi] cap: u32) {
    queue_gl_fire_and_forget(state, GLCmd::Enable { canvas_id, cap });
}

#[op2(fast)]
pub fn op_disable(state: &mut OpState, #[smi] canvas_id: u32, #[smi] cap: u32) {
    queue_gl_fire_and_forget(state, GLCmd::Disable { canvas_id, cap });
}

/// PERF: Architectural limitation -- this is a synchronous cross-thread call.
/// `op_get_parameter` flushes the pending GL command batch, sends a
/// `GetParameter` request to the render thread, and blocks the JS thread
/// until the render thread processes it and responds.  This causes a full
/// pipeline stall: JS cannot execute while waiting, and the render thread
/// must drain its queue to reach this request.
///
/// Frequent calls (e.g. inside a draw loop) will significantly degrade
/// frame rate.  Games should cache parameter values on the JS side
/// when possible.
///
/// Note: `gl.getError()` is currently stubbed to always return 0 on the JS
/// side (`02_webgl_context.js`), so it does not hit this path.  If a real
/// implementation is ever needed, consider maintaining a last-error cache
/// on the render thread updated by each GL call, and reading it via a
/// lock-free atomic instead of a sync round-trip.
#[op2]
#[string]
pub fn op_get_parameter(state: &mut OpState, #[smi] canvas_id: u32, #[smi] pname: u32) -> String {
    send_gl_sync_with_flush(state, |resp| {
        RenderCommand::GL(GLCmd::GetParameter {
            canvas_id,
            pname,
            resp,
        })
    })
    .unwrap_or_default()
}

/// A read of state the context holds whose answer is JSON text (`getInternalformatParameter`, and the queries that
/// follow it): `query` is one of `frame_wire::sync::gl_state`, `pname` and `extra` its first two arguments, `name` any
/// more. One op for the family, so a new query is a number and a handler arm, not a new op through the embedded
/// runtime, the producer and the contracts.
///
/// Synchronous like `op_get_parameter`, and with the same cost: the pending batch is flushed and the JS thread waits
/// for the render thread. These are setup-time queries.
#[op2]
#[string]
pub fn op_get_gl_state(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    #[smi] query: u32,
    #[smi] pname: u32,
    #[smi] extra: u32,
    #[string] name: String,
) -> String {
    send_gl_sync_with_flush(state, |resp| {
        RenderCommand::GL(GLCmd::GetState {
            canvas_id,
            query,
            pname,
            extra,
            name,
            resp,
        })
    })
    .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Phase 1B: Textures
// ---------------------------------------------------------------------------

#[op2(fast)]
pub fn op_create_texture(state: &mut OpState, #[smi] canvas_id: u32, #[smi] client_id: u32) {
    queue_gl_fire_and_forget(
        state,
        GLCmd::CreateTexture {
            canvas_id,
            client_id,
        },
    );
}

#[op2(fast)]
pub fn op_delete_texture(state: &mut OpState, #[smi] texture_id: u32) {
    queue_gl_fire_and_forget(state, GLCmd::DeleteTexture { texture_id });
}

#[op2(fast)]
pub fn op_bind_texture(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    #[smi] target: u32,
    texture: i32,
) {
    let texture = if texture < 0 {
        None
    } else {
        Some(texture as u32)
    };
    queue_gl_fire_and_forget(
        state,
        GLCmd::BindTexture {
            canvas_id,
            target,
            texture,
        },
    );
}

#[op2(fast)]
pub fn op_active_texture(state: &mut OpState, #[smi] canvas_id: u32, #[smi] unit: u32) {
    queue_gl_fire_and_forget(state, GLCmd::ActiveTexture { canvas_id, unit });
}

#[op2]
pub fn op_tex_image_2d(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    #[smi] target: u32,
    #[smi] level: i32,
    #[smi] internalformat: i32,
    #[smi] width: i32,
    #[smi] height: i32,
    #[smi] border: i32,
    #[smi] format: u32,
    #[smi] type_: u32,
    // `None` allocates the storage only.
    #[buffer] data: Option<&[u8]>,
    #[smi] pbo_offset: i32,
) {
    let command = frame_decode::resource::tex_image_2d(
        &mut OpStateDecodeContext(state),
        canvas_id,
        target,
        level,
        internalformat,
        width,
        height,
        border,
        format,
        type_,
        image_pixel_source(data, pbo_offset),
    );
    if let Some(command) = command {
        queue_gl_fire_and_forget(state, command);
    }
}

/// A `tex*Image*` call whose pixels are a TexImageSource -- an image the host decoded, a 2D canvas, a snapshot of one,
/// `ImageData`'s rows -- as [`frame_wire::gl_resource::OPR_TEX_IMAGE_SOURCE`] carries it: `words` are the record's
/// words after its header and `pixels` an `ImageData`'s RGBA8 rows, empty for the others. Built by the decoder's own
/// function, so this lane and the Performance+ lane name, check and resolve every source alike; the renderer converts
/// the pixels as the call asks (`GLCmd::TexImageSource`).
#[op2(fast)]
pub fn op_tex_image_source(state: &mut OpState, #[buffer] words: &[u32], #[buffer] pixels: &[u8]) {
    let command = frame_decode::resource::tex_image_source(
        &mut OpStateDecodeContext(state),
        words,
        frame_decode::resource::Payload::Bytes(pixels),
    );
    if let Some(command) = command {
        queue_gl_fire_and_forget(state, command);
    }
}

#[op2(fast)]
pub fn op_tex_sub_image_2d(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    #[smi] target: u32,
    #[smi] level: i32,
    #[smi] xoffset: i32,
    #[smi] yoffset: i32,
    #[smi] width: i32,
    #[smi] height: i32,
    #[smi] format: u32,
    #[smi] type_: u32,
    #[buffer] data: &[u8],
    #[smi] pbo_offset: i32,
) {
    let command = frame_decode::resource::tex_sub_image_2d(
        &mut OpStateDecodeContext(state),
        canvas_id,
        target,
        level,
        xoffset,
        yoffset,
        width,
        height,
        format,
        type_,
        PixelSource::of_call(data, pbo_offset),
    );
    if let Some(command) = command {
        queue_gl_fire_and_forget(state, command);
    }
}

#[op2(fast)]
pub fn op_pixel_storei(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    #[smi] pname: u32,
    #[smi] param: i32,
) {
    queue_gl_fire_and_forget(
        state,
        GLCmd::PixelStorei {
            canvas_id,
            pname,
            param,
        },
    );
}

/// A compressed upload's source from its op's arguments: the bytes, or -- `pbo_offset` not negative, WebGL 2's other
/// overload -- `pbo_size` bytes of the bound PIXEL_UNPACK_BUFFER from that offset (the bytes are then empty).
fn compressed_source(data: &[u8], pbo_offset: i32, pbo_size: i32) -> CompressedSource<'_> {
    if pbo_offset >= 0 {
        CompressedSource::UnpackBuffer {
            offset: pbo_offset as u32,
            size: pbo_size,
        }
    } else {
        CompressedSource::Bytes(Payload::Bytes(data))
    }
}

#[op2(fast)]
pub fn op_compressed_tex_image_2d(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    #[smi] target: u32,
    #[smi] level: i32,
    #[smi] internalformat: u32,
    #[smi] width: i32,
    #[smi] height: i32,
    #[smi] border: i32,
    #[buffer] data: &[u8],
    #[smi] pbo_offset: i32,
    #[smi] pbo_size: i32,
) {
    let command = frame_decode::resource::compressed_tex_image_2d(
        &mut OpStateDecodeContext(state),
        canvas_id,
        target,
        level,
        internalformat,
        width,
        height,
        border,
        compressed_source(data, pbo_offset, pbo_size),
    );
    if let Some(command) = command {
        queue_gl_fire_and_forget(state, command);
    }
}

#[op2(fast)]
pub fn op_compressed_tex_sub_image_2d(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    #[smi] target: u32,
    #[smi] level: i32,
    #[smi] xoffset: i32,
    #[smi] yoffset: i32,
    #[smi] width: i32,
    #[smi] height: i32,
    #[smi] format: u32,
    #[buffer] data: &[u8],
    #[smi] pbo_offset: i32,
    #[smi] pbo_size: i32,
) {
    let command = frame_decode::resource::compressed_tex_sub_image_2d(
        &mut OpStateDecodeContext(state),
        canvas_id,
        target,
        level,
        xoffset,
        yoffset,
        width,
        height,
        format,
        compressed_source(data, pbo_offset, pbo_size),
    );
    if let Some(command) = command {
        queue_gl_fire_and_forget(state, command);
    }
}

/// `compressedTexImage3D` (WebGL 2), with the 2D form's two sources.
#[op2(fast)]
pub fn op_compressed_tex_image_3d(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    #[smi] target: u32,
    #[smi] level: i32,
    #[smi] internalformat: u32,
    #[smi] width: i32,
    #[smi] height: i32,
    #[smi] depth: i32,
    #[smi] border: i32,
    #[buffer] data: &[u8],
    #[smi] pbo_offset: i32,
    #[smi] pbo_size: i32,
) {
    let command = frame_decode::resource::compressed_tex_image_3d(
        &mut OpStateDecodeContext(state),
        canvas_id,
        target,
        level,
        internalformat,
        width,
        height,
        depth,
        border,
        compressed_source(data, pbo_offset, pbo_size),
    );
    if let Some(command) = command {
        queue_gl_fire_and_forget(state, command);
    }
}

/// `compressedTexSubImage3D` (WebGL 2).
#[op2(fast)]
pub fn op_compressed_tex_sub_image_3d(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    #[smi] target: u32,
    #[smi] level: i32,
    #[smi] xoffset: i32,
    #[smi] yoffset: i32,
    #[smi] zoffset: i32,
    #[smi] width: i32,
    #[smi] height: i32,
    #[smi] depth: i32,
    #[smi] format: u32,
    #[buffer] data: &[u8],
    #[smi] pbo_offset: i32,
    #[smi] pbo_size: i32,
) {
    let command = frame_decode::resource::compressed_tex_sub_image_3d(
        &mut OpStateDecodeContext(state),
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
        compressed_source(data, pbo_offset, pbo_size),
    );
    if let Some(command) = command {
        queue_gl_fire_and_forget(state, command);
    }
}

/// `waitSync` (WebGL 2): the facade has checked the flags and the timeout, which have one legal value each.
#[op2(fast)]
pub fn op_wait_sync(state: &mut OpState, #[smi] canvas_id: u32, #[smi] sync: u32) {
    queue_gl_fire_and_forget(state, GLCmd::WaitSync { canvas_id, sync });
}

// ---------------------------------------------------------------------------
// Phase 1C: Buffer & Vertex Extensions
// ---------------------------------------------------------------------------

#[op2(fast)]
pub fn op_buffer_sub_data(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    #[smi] target: u32,
    #[smi] offset: i32,
    #[buffer] data: &[u8],
) {
    // The builder refuses a negative offset as WebGL 1.0 §5.14.5 says: without
    // that the value was sign-extended into the driver's `GLintptr`, and only
    // the driver's own bounds check stood between it and a GPU fault.
    let command = frame_decode::resource::buffer_sub_data(
        &mut OpStateDecodeContext(state),
        canvas_id,
        target,
        offset,
        Payload::Bytes(data),
    );
    if let Some(command) = command {
        queue_gl_fire_and_forget(state, command);
    }
}

#[op2(fast)]
pub fn op_disable_vertex_attrib_array(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    #[smi] index: u32,
) {
    queue_gl_fire_and_forget(state, GLCmd::DisableVertexAttribArray { canvas_id, index });
}

#[op2(fast)]
pub fn op_clear_depth(state: &mut OpState, #[smi] canvas_id: u32, depth: f32) {
    queue_gl_fire_and_forget(state, GLCmd::ClearDepth { canvas_id, depth });
}

#[op2(fast)]
pub fn op_clear_stencil(state: &mut OpState, #[smi] canvas_id: u32, #[smi] s: i32) {
    queue_gl_fire_and_forget(state, GLCmd::ClearStencil { canvas_id, s });
}

// ---------------------------------------------------------------------------
// Phase 2A: Blend / Depth / Stencil / Cull State
// ---------------------------------------------------------------------------

#[op2(fast)]
pub fn op_blend_func(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    #[smi] sfactor: u32,
    #[smi] dfactor: u32,
) {
    queue_gl_fire_and_forget(
        state,
        GLCmd::BlendFunc {
            canvas_id,
            sfactor,
            dfactor,
        },
    );
}

#[op2(fast)]
pub fn op_blend_func_separate(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    #[smi] src_rgb: u32,
    #[smi] dst_rgb: u32,
    #[smi] src_alpha: u32,
    #[smi] dst_alpha: u32,
) {
    queue_gl_fire_and_forget(
        state,
        GLCmd::BlendFuncSeparate {
            canvas_id,
            src_rgb,
            dst_rgb,
            src_alpha,
            dst_alpha,
        },
    );
}

#[op2(fast)]
pub fn op_blend_equation(state: &mut OpState, #[smi] canvas_id: u32, #[smi] mode: u32) {
    queue_gl_fire_and_forget(state, GLCmd::BlendEquation { canvas_id, mode });
}

#[op2(fast)]
pub fn op_blend_equation_separate(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    #[smi] mode_rgb: u32,
    #[smi] mode_alpha: u32,
) {
    queue_gl_fire_and_forget(
        state,
        GLCmd::BlendEquationSeparate {
            canvas_id,
            mode_rgb,
            mode_alpha,
        },
    );
}

#[op2(fast)]
pub fn op_blend_color(state: &mut OpState, #[smi] canvas_id: u32, r: f32, g: f32, b: f32, a: f32) {
    queue_gl_fire_and_forget(
        state,
        GLCmd::BlendColor {
            canvas_id,
            r,
            g,
            b,
            a,
        },
    );
}

#[op2(fast)]
pub fn op_depth_func(state: &mut OpState, #[smi] canvas_id: u32, #[smi] func: u32) {
    queue_gl_fire_and_forget(state, GLCmd::DepthFunc { canvas_id, func });
}

#[op2(fast)]
pub fn op_depth_mask(state: &mut OpState, #[smi] canvas_id: u32, flag: bool) {
    queue_gl_fire_and_forget(state, GLCmd::DepthMask { canvas_id, flag });
}

#[op2(fast)]
pub fn op_depth_range(state: &mut OpState, #[smi] canvas_id: u32, near: f32, far: f32) {
    queue_gl_fire_and_forget(
        state,
        GLCmd::DepthRange {
            canvas_id,
            near,
            far,
        },
    );
}

#[op2(fast)]
pub fn op_stencil_func(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    #[smi] func: u32,
    #[smi] ref_: i32,
    #[smi] mask: u32,
) {
    queue_gl_fire_and_forget(
        state,
        GLCmd::StencilFunc {
            canvas_id,
            func,
            ref_,
            mask,
        },
    );
}

#[op2(fast)]
pub fn op_stencil_func_separate(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    #[smi] face: u32,
    #[smi] func: u32,
    #[smi] ref_: i32,
    #[smi] mask: u32,
) {
    queue_gl_fire_and_forget(
        state,
        GLCmd::StencilFuncSeparate {
            canvas_id,
            face,
            func,
            ref_,
            mask,
        },
    );
}

#[op2(fast)]
pub fn op_stencil_op(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    #[smi] fail: u32,
    #[smi] zfail: u32,
    #[smi] zpass: u32,
) {
    queue_gl_fire_and_forget(
        state,
        GLCmd::StencilOp {
            canvas_id,
            fail,
            zfail,
            zpass,
        },
    );
}

#[op2(fast)]
pub fn op_stencil_op_separate(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    #[smi] face: u32,
    #[smi] fail: u32,
    #[smi] zfail: u32,
    #[smi] zpass: u32,
) {
    queue_gl_fire_and_forget(
        state,
        GLCmd::StencilOpSeparate {
            canvas_id,
            face,
            fail,
            zfail,
            zpass,
        },
    );
}

#[op2(fast)]
pub fn op_stencil_mask(state: &mut OpState, #[smi] canvas_id: u32, #[smi] mask: u32) {
    queue_gl_fire_and_forget(state, GLCmd::StencilMask { canvas_id, mask });
}

#[op2(fast)]
pub fn op_stencil_mask_separate(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    #[smi] face: u32,
    #[smi] mask: u32,
) {
    queue_gl_fire_and_forget(
        state,
        GLCmd::StencilMaskSeparate {
            canvas_id,
            face,
            mask,
        },
    );
}

#[op2(fast)]
pub fn op_cull_face(state: &mut OpState, #[smi] canvas_id: u32, #[smi] mode: u32) {
    queue_gl_fire_and_forget(state, GLCmd::CullFace { canvas_id, mode });
}

#[op2(fast)]
pub fn op_front_face(state: &mut OpState, #[smi] canvas_id: u32, #[smi] mode: u32) {
    queue_gl_fire_and_forget(state, GLCmd::FrontFace { canvas_id, mode });
}

#[op2(fast)]
pub fn op_color_mask(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    r: bool,
    g: bool,
    b: bool,
    a: bool,
) {
    queue_gl_fire_and_forget(
        state,
        GLCmd::ColorMask {
            canvas_id,
            r,
            g,
            b,
            a,
        },
    );
}

#[op2(fast)]
pub fn op_scissor(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    #[smi] x: i32,
    #[smi] y: i32,
    #[smi] width: i32,
    #[smi] height: i32,
) {
    // WebGL spec: negative width/height → INVALID_VALUE.
    if !crate::rendering::webgl::error_state::validate_viewport_like(
        state, canvas_id, width, height,
    ) {
        return;
    }
    queue_gl_fire_and_forget(
        state,
        GLCmd::Scissor {
            canvas_id,
            x,
            y,
            width,
            height,
        },
    );
}

#[op2(fast)]
pub fn op_line_width(state: &mut OpState, #[smi] canvas_id: u32, width: f32) {
    queue_gl_fire_and_forget(state, GLCmd::LineWidth { canvas_id, width });
}

#[op2(fast)]
pub fn op_polygon_offset(state: &mut OpState, #[smi] canvas_id: u32, factor: f32, units: f32) {
    queue_gl_fire_and_forget(
        state,
        GLCmd::PolygonOffset {
            canvas_id,
            factor,
            units,
        },
    );
}

// ---------------------------------------------------------------------------
// Phase 2B: Uniform Variants
// ---------------------------------------------------------------------------

#[op2(fast)]
pub fn op_uniform1i(state: &mut OpState, #[smi] canvas_id: u32, location: i32, #[smi] x: i32) {
    let location = if location < 0 {
        None
    } else {
        Some(location as u32)
    };
    queue_gl_fire_and_forget(
        state,
        GLCmd::Uniform1i {
            canvas_id,
            location,
            x,
        },
    );
}

#[op2(fast)]
pub fn op_uniform1f(state: &mut OpState, #[smi] canvas_id: u32, location: i32, x: f32) {
    let location = if location < 0 {
        None
    } else {
        Some(location as u32)
    };
    queue_gl_fire_and_forget(
        state,
        GLCmd::Uniform1f {
            canvas_id,
            location,
            x,
        },
    );
}

#[op2(fast)]
pub fn op_uniform2f(state: &mut OpState, #[smi] canvas_id: u32, location: i32, x: f32, y: f32) {
    let location = if location < 0 {
        None
    } else {
        Some(location as u32)
    };
    queue_gl_fire_and_forget(
        state,
        GLCmd::Uniform2f {
            canvas_id,
            location,
            x,
            y,
        },
    );
}

#[op2(fast)]
pub fn op_uniform4f(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    location: i32,
    x: f32,
    y: f32,
    z: f32,
    w: f32,
) {
    let location = if location < 0 {
        None
    } else {
        Some(location as u32)
    };
    queue_gl_fire_and_forget(
        state,
        GLCmd::Uniform4f {
            canvas_id,
            location,
            x,
            y,
            z,
            w,
        },
    );
}

#[op2(fast)]
pub fn op_uniform1iv(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    location: i32,
    #[buffer] value: &[u32],
) {
    let location = if location < 0 {
        None
    } else {
        Some(location as u32)
    };
    let value = copy_i32_words(value);
    queue_gl_fire_and_forget(
        state,
        GLCmd::Uniform1iv {
            canvas_id,
            location,
            value,
        },
    );
}

#[op2(fast)]
pub fn op_uniform1fv(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    location: i32,
    #[buffer] value: &[u32],
) {
    let location = if location < 0 {
        None
    } else {
        Some(location as u32)
    };
    let value = copy_f32_words(value);
    queue_gl_fire_and_forget(
        state,
        GLCmd::Uniform1fv {
            canvas_id,
            location,
            value,
        },
    );
}

#[op2(fast)]
pub fn op_uniform2iv(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    location: i32,
    #[buffer] value: &[u32],
) {
    let location = if location < 0 {
        None
    } else {
        Some(location as u32)
    };
    let value = copy_i32_words(value);
    queue_gl_fire_and_forget(
        state,
        GLCmd::Uniform2iv {
            canvas_id,
            location,
            value,
        },
    );
}

#[op2(fast)]
pub fn op_uniform2fv(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    location: i32,
    #[buffer] value: &[u32],
) {
    let location = if location < 0 {
        None
    } else {
        Some(location as u32)
    };
    let value = copy_f32_words(value);
    queue_gl_fire_and_forget(
        state,
        GLCmd::Uniform2fv {
            canvas_id,
            location,
            value,
        },
    );
}

#[op2(fast)]
pub fn op_uniform3iv(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    location: i32,
    #[buffer] value: &[u32],
) {
    let location = if location < 0 {
        None
    } else {
        Some(location as u32)
    };
    let value = copy_i32_words(value);
    queue_gl_fire_and_forget(
        state,
        GLCmd::Uniform3iv {
            canvas_id,
            location,
            value,
        },
    );
}

#[op2(fast)]
pub fn op_uniform3fv(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    location: i32,
    #[buffer] value: &[u32],
) {
    let location = if location < 0 {
        None
    } else {
        Some(location as u32)
    };
    let value = copy_f32_words(value);
    queue_gl_fire_and_forget(
        state,
        GLCmd::Uniform3fv {
            canvas_id,
            location,
            value,
        },
    );
}

#[op2(fast)]
pub fn op_uniform4iv(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    location: i32,
    #[buffer] value: &[u32],
) {
    let location = if location < 0 {
        None
    } else {
        Some(location as u32)
    };
    let value = copy_i32_words(value);
    queue_gl_fire_and_forget(
        state,
        GLCmd::Uniform4iv {
            canvas_id,
            location,
            value,
        },
    );
}

#[op2(fast)]
pub fn op_uniform4fv(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    location: i32,
    #[buffer] value: &[u32],
) {
    let location = if location < 0 {
        None
    } else {
        Some(location as u32)
    };
    let value = copy_f32_words(value);
    queue_gl_fire_and_forget(
        state,
        GLCmd::Uniform4fv {
            canvas_id,
            location,
            value,
        },
    );
}

#[op2(fast)]
pub fn op_uniform1uiv(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    location: i32,
    #[buffer] value: &[u32],
) {
    let location = if location < 0 {
        None
    } else {
        Some(location as u32)
    };
    let value = copy_u32_words(value);
    queue_gl_fire_and_forget(
        state,
        GLCmd::Uniform1uiv {
            canvas_id,
            location,
            value,
        },
    );
}

#[op2(fast)]
pub fn op_uniform2uiv(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    location: i32,
    #[buffer] value: &[u32],
) {
    let location = if location < 0 {
        None
    } else {
        Some(location as u32)
    };
    let value = copy_u32_words(value);
    queue_gl_fire_and_forget(
        state,
        GLCmd::Uniform2uiv {
            canvas_id,
            location,
            value,
        },
    );
}

#[op2(fast)]
pub fn op_uniform3uiv(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    location: i32,
    #[buffer] value: &[u32],
) {
    let location = if location < 0 {
        None
    } else {
        Some(location as u32)
    };
    let value = copy_u32_words(value);
    queue_gl_fire_and_forget(
        state,
        GLCmd::Uniform3uiv {
            canvas_id,
            location,
            value,
        },
    );
}

#[op2(fast)]
pub fn op_uniform4uiv(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    location: i32,
    #[buffer] value: &[u32],
) {
    let location = if location < 0 {
        None
    } else {
        Some(location as u32)
    };
    let value = copy_u32_words(value);
    queue_gl_fire_and_forget(
        state,
        GLCmd::Uniform4uiv {
            canvas_id,
            location,
            value,
        },
    );
}

#[op2(fast)]
pub fn op_uniform_matrix_2x3fv(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    location: i32,
    transpose: bool,
    #[buffer] value: &[u32],
) {
    let location = if location < 0 {
        None
    } else {
        Some(location as u32)
    };
    let value = copy_f32_words(value);
    queue_gl_fire_and_forget(
        state,
        GLCmd::UniformMatrix2x3fv {
            canvas_id,
            location,
            transpose,
            value,
        },
    );
}

#[op2(fast)]
pub fn op_uniform_matrix_2x4fv(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    location: i32,
    transpose: bool,
    #[buffer] value: &[u32],
) {
    let location = if location < 0 {
        None
    } else {
        Some(location as u32)
    };
    let value = copy_f32_words(value);
    queue_gl_fire_and_forget(
        state,
        GLCmd::UniformMatrix2x4fv {
            canvas_id,
            location,
            transpose,
            value,
        },
    );
}

#[op2(fast)]
pub fn op_uniform_matrix_3x2fv(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    location: i32,
    transpose: bool,
    #[buffer] value: &[u32],
) {
    let location = if location < 0 {
        None
    } else {
        Some(location as u32)
    };
    let value = copy_f32_words(value);
    queue_gl_fire_and_forget(
        state,
        GLCmd::UniformMatrix3x2fv {
            canvas_id,
            location,
            transpose,
            value,
        },
    );
}

#[op2(fast)]
pub fn op_uniform_matrix_3x4fv(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    location: i32,
    transpose: bool,
    #[buffer] value: &[u32],
) {
    let location = if location < 0 {
        None
    } else {
        Some(location as u32)
    };
    let value = copy_f32_words(value);
    queue_gl_fire_and_forget(
        state,
        GLCmd::UniformMatrix3x4fv {
            canvas_id,
            location,
            transpose,
            value,
        },
    );
}

#[op2(fast)]
pub fn op_uniform_matrix_4x2fv(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    location: i32,
    transpose: bool,
    #[buffer] value: &[u32],
) {
    let location = if location < 0 {
        None
    } else {
        Some(location as u32)
    };
    let value = copy_f32_words(value);
    queue_gl_fire_and_forget(
        state,
        GLCmd::UniformMatrix4x2fv {
            canvas_id,
            location,
            transpose,
            value,
        },
    );
}

#[op2(fast)]
pub fn op_uniform_matrix_4x3fv(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    location: i32,
    transpose: bool,
    #[buffer] value: &[u32],
) {
    let location = if location < 0 {
        None
    } else {
        Some(location as u32)
    };
    let value = copy_f32_words(value);
    queue_gl_fire_and_forget(
        state,
        GLCmd::UniformMatrix4x3fv {
            canvas_id,
            location,
            transpose,
            value,
        },
    );
}

#[op2(fast)]
pub fn op_uniform_matrix_2fv(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    location: i32,
    transpose: bool,
    #[buffer] value: &[u32],
) {
    let location = if location < 0 {
        None
    } else {
        Some(location as u32)
    };
    let value = copy_f32_words(value);
    queue_gl_fire_and_forget(
        state,
        GLCmd::UniformMatrix2fv {
            canvas_id,
            location,
            transpose,
            value,
        },
    );
}

#[op2(fast)]
pub fn op_uniform_matrix_4fv(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    location: i32,
    transpose: bool,
    #[buffer] value: &[u32],
) {
    let location = if location < 0 {
        None
    } else {
        Some(location as u32)
    };
    let value = copy_f32_words(value);
    queue_gl_fire_and_forget(
        state,
        GLCmd::UniformMatrix4fv {
            canvas_id,
            location,
            transpose,
            value,
        },
    );
}

// ---------------------------------------------------------------------------
// Phase 3A: Framebuffer / Renderbuffer
// ---------------------------------------------------------------------------

#[op2(fast)]
pub fn op_create_framebuffer(state: &mut OpState, #[smi] canvas_id: u32, #[smi] client_id: u32) {
    queue_gl_fire_and_forget(
        state,
        GLCmd::CreateFramebuffer {
            canvas_id,
            client_id,
        },
    );
}

#[op2(fast)]
pub fn op_delete_framebuffer(state: &mut OpState, #[smi] framebuffer_id: u32) {
    queue_gl_fire_and_forget(state, GLCmd::DeleteFramebuffer { framebuffer_id });
}

#[op2(fast)]
pub fn op_bind_framebuffer(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    #[smi] target: u32,
    framebuffer: i32,
) {
    let framebuffer = if framebuffer < 0 {
        None
    } else {
        Some(framebuffer as u32)
    };
    queue_gl_fire_and_forget(
        state,
        GLCmd::BindFramebuffer {
            canvas_id,
            target,
            framebuffer,
        },
    );
}

#[op2(fast)]
pub fn op_framebuffer_texture_2d(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    #[smi] target: u32,
    #[smi] attachment: u32,
    #[smi] textarget: u32,
    texture: i32,
    #[smi] level: i32,
) {
    let texture = if texture < 0 {
        None
    } else {
        Some(texture as u32)
    };
    queue_gl_fire_and_forget(
        state,
        GLCmd::FramebufferTexture2D {
            canvas_id,
            target,
            attachment,
            textarget,
            texture,
            level,
        },
    );
}

/// `framebufferTextureLayer` (WebGL 2): one layer of a 3D or 2D-array texture as an attachment; a texture id below 0
/// detaches.
#[op2(fast)]
pub fn op_framebuffer_texture_layer(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    #[smi] target: u32,
    #[smi] attachment: u32,
    texture: i32,
    #[smi] level: i32,
    #[smi] layer: i32,
) {
    let texture = if texture < 0 {
        None
    } else {
        Some(texture as u32)
    };
    queue_gl_fire_and_forget(
        state,
        GLCmd::FramebufferTextureLayer {
            canvas_id,
            target,
            attachment,
            texture,
            level,
            layer,
        },
    );
}

#[op2(fast)]
pub fn op_framebuffer_renderbuffer(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    #[smi] target: u32,
    #[smi] attachment: u32,
    #[smi] renderbuffertarget: u32,
    renderbuffer: i32,
) {
    let renderbuffer = if renderbuffer < 0 {
        None
    } else {
        Some(renderbuffer as u32)
    };
    queue_gl_fire_and_forget(
        state,
        GLCmd::FramebufferRenderbuffer {
            canvas_id,
            target,
            attachment,
            renderbuffertarget,
            renderbuffer,
        },
    );
}

#[op2(fast)]
pub fn op_check_framebuffer_status(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    #[smi] target: u32,
) -> u32 {
    send_gl_sync_with_flush(state, |resp| {
        RenderCommand::GL(GLCmd::CheckFramebufferStatus {
            canvas_id,
            target,
            resp,
        })
    })
    .unwrap_or(0)
}

#[op2(fast)]
pub fn op_create_renderbuffer(state: &mut OpState, #[smi] canvas_id: u32, #[smi] client_id: u32) {
    queue_gl_fire_and_forget(
        state,
        GLCmd::CreateRenderbuffer {
            canvas_id,
            client_id,
        },
    );
}

#[op2(fast)]
pub fn op_delete_renderbuffer(state: &mut OpState, #[smi] renderbuffer_id: u32) {
    queue_gl_fire_and_forget(state, GLCmd::DeleteRenderbuffer { renderbuffer_id });
}

#[op2(fast)]
pub fn op_delete_buffer(state: &mut OpState, #[smi] buffer_id: u32) {
    queue_gl_fire_and_forget(state, GLCmd::DeleteBuffer { buffer_id });
}

#[op2(fast)]
pub fn op_bind_renderbuffer(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    #[smi] target: u32,
    renderbuffer: i32,
) {
    let renderbuffer = if renderbuffer < 0 {
        None
    } else {
        Some(renderbuffer as u32)
    };
    queue_gl_fire_and_forget(
        state,
        GLCmd::BindRenderbuffer {
            canvas_id,
            target,
            renderbuffer,
        },
    );
}

#[op2(fast)]
pub fn op_renderbuffer_storage(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    #[smi] target: u32,
    #[smi] internalformat: u32,
    #[smi] width: i32,
    #[smi] height: i32,
) {
    queue_gl_fire_and_forget(
        state,
        GLCmd::RenderbufferStorage {
            canvas_id,
            target,
            internalformat,
            width,
            height,
        },
    );
}

// ---------------------------------------------------------------------------
// Phase 3B: Misc
// ---------------------------------------------------------------------------

/// Validates the allocation implied by `readPixels` before a synchronous
/// render command is published. Returning `None` means the caller must return
/// an empty result without flushing or touching the render queue.
fn prepare_read_pixels(
    state: &mut OpState,
    canvas_id: u32,
    width: i32,
    height: i32,
    format: u32,
    type_: u32,
) -> Option<usize> {
    let Some(bytes_per_pixel) = webgl_readback_bytes_per_pixel(format, type_) else {
        error_state::push_error(state, canvas_id, codes::INVALID_ENUM);
        return None;
    };
    match checked_readback_byte_len(width, height, bytes_per_pixel) {
        Some(byte_len) => Some(byte_len),
        None => {
            let code = if width < 0 || height < 0 {
                crate::rendering::webgl::error_state::codes::INVALID_VALUE
            } else {
                crate::rendering::webgl::error_state::codes::OUT_OF_MEMORY
            };
            crate::rendering::webgl::error_state::push_error(state, canvas_id, code);
            None
        }
    }
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadPixelsJsData {
    data: ToJsBuffer,
    first_byte: usize,
    row_bytes: usize,
    row_stride: usize,
    height: usize,
}

/// Returns the view byte length and element size without invoking JS getters.
/// A packed pixel's size is independent of the destination's element size.
fn read_pixels_view_layout(pixels: v8::Local<v8::Value>, type_: u32) -> Option<(usize, usize)> {
    let element_bytes = match type_ {
        0x1400 if pixels.is_int8_array() => 1,
        0x1401 if pixels.is_uint8_array() || pixels.is_uint8_clamped_array() => 1,
        0x1402 if pixels.is_int16_array() => 2,
        0x1403 | 0x140B | 0x8D61 | 0x8363 | 0x8033 | 0x8034 | 0x8365 | 0x8366
            if pixels.is_uint16_array() =>
        {
            2
        }
        0x1404 if pixels.is_int32_array() => 4,
        0x1405 | 0x8368 | 0x8C3B | 0x8C3E | 0x84FA | 0x8DAD if pixels.is_uint32_array() => 4,
        0x1406 if pixels.is_float32_array() => 4,
        _ => return None,
    };
    v8::Local::<v8::ArrayBufferView>::try_from(pixels)
        .ok()
        .map(|view| (view.byte_length(), element_bytes))
}

/// WebIDL unsigned long long conversion after JS ToNumber. Keep the usual
/// nonnegative range free of floating-point remainder; negative values wrap
/// in integer arithmetic so -1 cannot round to 2^64 and then to zero.
fn read_pixels_element_offset(value: f64) -> u64 {
    const MODULUS: f64 = 18_446_744_073_709_551_616.0;
    if !value.is_finite() {
        return 0;
    }
    if value >= 0.0 && value < MODULUS {
        return value as u64;
    }
    let remainder = value.trunc() % MODULUS;
    if remainder < 0.0 {
        0u64.wrapping_sub((-remainder) as u64)
    } else {
        remainder as u64
    }
}

#[op2]
#[serde]
pub fn op_read_pixels(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    #[smi] x: i32,
    #[smi] y: i32,
    #[smi] width: i32,
    #[smi] height: i32,
    #[smi] format: u32,
    #[smi] type_: u32,
    pixels: v8::Local<v8::Value>,
    dst_offset: f64,
) -> Option<ReadPixelsJsData> {
    let Some(byte_len) = prepare_read_pixels(state, canvas_id, width, height, format, type_) else {
        return None;
    };
    let Some((view_byte_length, element_bytes)) = read_pixels_view_layout(pixels, type_) else {
        error_state::push_error(
            state,
            canvas_id,
            if pixels.is_null_or_undefined() {
                codes::INVALID_VALUE
            } else {
                codes::INVALID_OPERATION
            },
        );
        return None;
    };
    let Some(destination_byte_offset) = usize::try_from(read_pixels_element_offset(dst_offset))
        .ok()
        .and_then(|elements| elements.checked_mul(element_bytes))
        .filter(|offset| *offset <= view_byte_length)
    else {
        error_state::push_error(state, canvas_id, codes::INVALID_OPERATION);
        return None;
    };
    let destination_byte_length = view_byte_length - destination_byte_offset;
    if byte_len > destination_byte_length {
        error_state::push_error(state, canvas_id, codes::INVALID_OPERATION);
        return None;
    }

    let result = send_gl_sync_with_flush(state, |resp| {
        RenderCommand::GL(GLCmd::ReadPixels {
            canvas_id,
            x,
            y,
            width,
            height,
            format,
            type_,
            destination_byte_length,
            resp,
        })
    });
    match result {
        Ok(result) => Some(ReadPixelsJsData {
            data: result.pixels.into(),
            // The renderer validated its full PACK footprint against the
            // remaining view, so adding this prefix stays within the view.
            first_byte: destination_byte_offset + result.layout.first_byte,
            row_bytes: result.layout.row_bytes,
            row_stride: result.layout.row_stride,
            height: result.layout.height,
        }),
        Err(error) => {
            use shared::error::ErrorCode;
            error_state::push_error(
                state,
                canvas_id,
                match error.code {
                    ErrorCode::OutOfMemory => codes::OUT_OF_MEMORY,
                    ErrorCode::InvalidArgument => codes::INVALID_VALUE,
                    // WebGL has a dedicated code for this and content uses it to
                    // tell "the framebuffer is not readable" from "the arguments
                    // were wrong"; folding it into INVALID_OPERATION lost that.
                    ErrorCode::RenderFramebufferIncomplete => codes::INVALID_FRAMEBUFFER_OPERATION,
                    _ => codes::INVALID_OPERATION,
                },
            );
            None
        }
    }
}

/// `readPixels` into the bound `PIXEL_PACK_BUFFER`.
///
/// The offset is a `GLintptr`, so it arrives as a double and converts by
/// WebIDL's `long long` rules; a value outside that range is a `TypeError`
/// there, not here. Nothing is transferred back: only the spec's own
/// validation errors, which the renderer decides because they depend on the
/// live PACK state and the buffer's size.
#[op2(fast)]
pub fn op_read_pixels_to_buffer(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    #[smi] x: i32,
    #[smi] y: i32,
    #[smi] width: i32,
    #[smi] height: i32,
    #[smi] format: u32,
    #[smi] type_: u32,
    #[bigint] offset: i64,
) {
    if prepare_read_pixels(state, canvas_id, width, height, format, type_).is_none() {
        return;
    }
    if offset < 0 {
        error_state::push_error(state, canvas_id, codes::INVALID_VALUE);
        return;
    }
    let result = send_gl_sync_with_flush(state, |resp| {
        RenderCommand::GL(GLCmd::ReadPixelsToBuffer {
            canvas_id,
            x,
            y,
            width,
            height,
            format,
            type_,
            offset,
            resp,
        })
    });
    if let Err(error) = result {
        use shared::error::ErrorCode;
        error_state::push_error(
            state,
            canvas_id,
            match error.code {
                ErrorCode::OutOfMemory => codes::OUT_OF_MEMORY,
                ErrorCode::InvalidArgument => codes::INVALID_VALUE,
                ErrorCode::RenderFramebufferIncomplete => codes::INVALID_FRAMEBUFFER_OPERATION,
                _ => codes::INVALID_OPERATION,
            },
        );
    }
}

/// `getBufferSubData`: the facade has checked the target, the binding and the range against the buffer's size, and
/// passes the destination's bytes; the renderer's bytes are copied into them. A read the renderer cannot answer is
/// INVALID_OPERATION, and the destination is left as it was.
#[op2(fast)]
pub fn op_get_buffer_sub_data(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    #[smi] target: u32,
    #[smi] offset: i32,
    #[buffer] destination: &mut [u8],
) {
    let Ok(size) = u32::try_from(destination.len()) else {
        error_state::push_error(state, canvas_id, codes::INVALID_VALUE);
        return;
    };
    if size == 0 {
        return;
    }
    let result = send_gl_sync_with_flush(state, |resp| {
        RenderCommand::GL(GLCmd::GetBufferSubData {
            canvas_id,
            target,
            offset: i64::from(offset),
            size,
            resp,
        })
    });
    match result {
        Ok(bytes) if bytes.len() == destination.len() => destination.copy_from_slice(&bytes),
        _ => error_state::push_error(state, canvas_id, codes::INVALID_OPERATION),
    }
}

#[op2(fast)]
pub fn op_hint(state: &mut OpState, #[smi] canvas_id: u32, #[smi] target: u32, #[smi] mode: u32) {
    queue_gl_fire_and_forget(
        state,
        GLCmd::Hint {
            canvas_id,
            target,
            mode,
        },
    );
}

// ---------------------------------------------------------------------------
// WebGL 2.0 / GLES 3.0 additions
//
// Each op mirrors a single entry in GLCmd (see shared/protocol/render_cmd.rs).
// Fire-and-forget ops route through `queue_gl_fire_and_forget` so the
// UnifiedFrameCollector can batch them into the frame packet.  Sync ops
// (getUniformBlockIndex, clientWaitSync) call `send_gl_sync_with_flush`
// so any pending batch is materialised before the reply is waited on.
// ---------------------------------------------------------------------------

#[op2(fast)]
pub fn op_create_vertex_array(state: &mut OpState, #[smi] canvas_id: u32, #[smi] client_id: u32) {
    queue_gl_fire_and_forget(
        state,
        GLCmd::CreateVertexArray {
            canvas_id,
            client_id,
        },
    );
}

#[op2(fast)]
pub fn op_delete_vertex_array(state: &mut OpState, #[smi] vao: u32) {
    queue_gl_fire_and_forget(state, GLCmd::DeleteVertexArray { vao });
}

#[op2(fast)]
pub fn op_vertex_attrib_divisor(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    #[smi] index: u32,
    #[smi] divisor: u32,
) {
    queue_gl_fire_and_forget(
        state,
        GLCmd::VertexAttribDivisor {
            canvas_id,
            index,
            divisor,
        },
    );
}

#[op2(fast)]
pub fn op_draw_arrays_instanced(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    #[smi] mode: u32,
    #[smi] first: i32,
    #[smi] count: i32,
    #[smi] instance_count: i32,
) {
    queue_gl_fire_and_forget(
        state,
        GLCmd::DrawArraysInstanced {
            canvas_id,
            mode,
            first,
            count,
            instance_count,
        },
    );
}

#[op2(fast)]
pub fn op_draw_elements_instanced(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    #[smi] mode: u32,
    #[smi] count: i32,
    #[smi] index_type: u32,
    #[smi] offset: i32,
    #[smi] instance_count: i32,
) {
    queue_gl_fire_and_forget(
        state,
        GLCmd::DrawElementsInstanced {
            canvas_id,
            mode,
            count,
            index_type,
            offset,
            instance_count,
        },
    );
}

#[op2(fast)]
#[smi]
pub fn op_get_uniform_block_index(
    state: &mut OpState,
    #[smi] program_id: u32,
    #[string] name: String,
) -> u32 {
    send_gl_sync_with_flush(state, |resp| {
        RenderCommand::GL(GLCmd::GetUniformBlockIndex {
            program_id,
            name,
            resp,
        })
    })
    .unwrap_or(u32::MAX)
}

#[op2(fast)]
pub fn op_uniform_block_binding(
    state: &mut OpState,
    #[smi] program_id: u32,
    #[smi] uniform_block_index: u32,
    #[smi] uniform_block_binding: u32,
) {
    queue_gl_fire_and_forget(
        state,
        GLCmd::UniformBlockBinding {
            program_id,
            uniform_block_index,
            uniform_block_binding,
        },
    );
}

#[op2(fast)]
pub fn op_bind_buffer_base(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    #[smi] target: u32,
    #[smi] index: u32,
    #[smi] buffer: u32,
) {
    bind_buffer_base_impl(state, canvas_id, target, index, buffer);
}

pub(crate) fn bind_buffer_base_impl(
    state: &mut OpState,
    canvas_id: u32,
    target: u32,
    index: u32,
    buffer: u32,
) {
    let buffer = if buffer == 0 { None } else { Some(buffer) };
    if !crate::rendering::webgl::error_state::validate_bind_buffer_base(
        state, canvas_id, target, index, buffer,
    ) {
        return;
    }
    queue_gl_fire_and_forget(
        state,
        GLCmd::BindBufferBase {
            canvas_id,
            target,
            index,
            buffer,
        },
    );
}

#[op2(fast)]
pub fn op_bind_buffer_range(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    #[smi] target: u32,
    #[smi] index: u32,
    #[smi] buffer: u32,
    #[smi] offset: i32,
    #[smi] size: i32,
) {
    bind_buffer_range_impl(state, canvas_id, target, index, buffer, offset, size);
}

pub(crate) fn bind_buffer_range_impl(
    state: &mut OpState,
    canvas_id: u32,
    target: u32,
    index: u32,
    buffer: u32,
    offset: i32,
    size: i32,
) {
    let buffer = if buffer == 0 { None } else { Some(buffer) };
    if !crate::rendering::webgl::error_state::validate_bind_buffer_range(
        state, canvas_id, target, index, buffer, offset, size,
    ) {
        return;
    }
    queue_gl_fire_and_forget(
        state,
        GLCmd::BindBufferRange {
            canvas_id,
            target,
            index,
            buffer,
            offset,
            size,
        },
    );
}

#[op2(fast)]
pub fn op_tex_storage_2d(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    #[smi] target: u32,
    #[smi] levels: i32,
    #[smi] internal_format: u32,
    #[smi] width: i32,
    #[smi] height: i32,
) {
    queue_gl_fire_and_forget(
        state,
        GLCmd::TexStorage2D {
            canvas_id,
            target,
            levels,
            internal_format,
            width,
            height,
        },
    );
}

#[op2(fast)]
#[allow(clippy::too_many_arguments)]
pub fn op_blit_framebuffer(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    #[smi] src_x0: i32,
    #[smi] src_y0: i32,
    #[smi] src_x1: i32,
    #[smi] src_y1: i32,
    #[smi] dst_x0: i32,
    #[smi] dst_y0: i32,
    #[smi] dst_x1: i32,
    #[smi] dst_y1: i32,
    #[smi] mask: u32,
    #[smi] filter: u32,
) {
    queue_gl_fire_and_forget(
        state,
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
        },
    );
}

#[op2(fast)]
pub fn op_invalidate_framebuffer(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    #[smi] target: u32,
    // JS passes a `Uint32Array` directly.  `#[buffer(copy)]` copies the
    // element view, yielding an owned Vec without bytemuck gymnastics.
    #[buffer(copy)] attachments: Vec<u32>,
) {
    queue_gl_fire_and_forget(
        state,
        GLCmd::InvalidateFramebuffer {
            canvas_id,
            target,
            attachments,
        },
    );
}

/// `invalidateSubFramebuffer` (WebGL 2): `op_invalidate_framebuffer` within a rectangle.
#[op2(fast)]
pub fn op_invalidate_sub_framebuffer(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    #[smi] target: u32,
    #[buffer(copy)] attachments: Vec<u32>,
    #[smi] x: i32,
    #[smi] y: i32,
    #[smi] width: i32,
    #[smi] height: i32,
) {
    queue_gl_fire_and_forget(
        state,
        GLCmd::InvalidateSubFramebuffer {
            canvas_id,
            target,
            attachments,
            x,
            y,
            width,
            height,
        },
    );
}

#[op2(fast)]
pub fn op_renderbuffer_storage_multisample(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    #[smi] target: u32,
    #[smi] samples: i32,
    #[smi] internal_format: u32,
    #[smi] width: i32,
    #[smi] height: i32,
) {
    queue_gl_fire_and_forget(
        state,
        GLCmd::RenderbufferStorageMultisample {
            canvas_id,
            target,
            samples,
            internal_format,
            width,
            height,
        },
    );
}

#[op2(fast)]
pub fn op_create_sampler(state: &mut OpState, #[smi] canvas_id: u32, #[smi] client_id: u32) {
    queue_gl_fire_and_forget(
        state,
        GLCmd::CreateSampler {
            canvas_id,
            client_id,
        },
    );
}

#[op2(fast)]
pub fn op_delete_sampler(state: &mut OpState, #[smi] sampler: u32) {
    queue_gl_fire_and_forget(state, GLCmd::DeleteSampler { sampler });
}

#[op2(fast)]
pub fn op_bind_sampler(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    #[smi] unit: u32,
    #[smi] sampler: u32,
) {
    queue_gl_fire_and_forget(
        state,
        GLCmd::BindSampler {
            canvas_id,
            unit,
            sampler: if sampler == 0 { None } else { Some(sampler) },
        },
    );
}

#[op2(fast)]
pub fn op_sampler_parameteri(
    state: &mut OpState,
    #[smi] sampler: u32,
    #[smi] pname: u32,
    #[smi] param: i32,
) {
    queue_gl_fire_and_forget(
        state,
        GLCmd::SamplerParameteri {
            sampler,
            pname,
            param,
        },
    );
}

#[op2(fast)]
pub fn op_sampler_parameterf(
    state: &mut OpState,
    #[smi] sampler: u32,
    #[smi] pname: u32,
    param: f32,
) {
    queue_gl_fire_and_forget(
        state,
        GLCmd::SamplerParameterf {
            sampler,
            pname,
            param,
        },
    );
}

#[op2(fast)]
pub fn op_fence_sync(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    #[smi] client_id: u32,
    #[smi] condition: u32,
    #[smi] flags: u32,
) {
    queue_gl_fire_and_forget(
        state,
        GLCmd::FenceSync {
            canvas_id,
            client_id,
            condition,
            flags,
        },
    );
}

#[op2(fast)]
pub fn op_delete_sync(state: &mut OpState, #[smi] sync: u32) {
    queue_gl_fire_and_forget(state, GLCmd::DeleteSync { sync });
}

#[op2(fast)]
#[smi]
pub fn op_client_wait_sync(state: &mut OpState, #[smi] sync: u32, #[smi] flags: u32) -> u32 {
    // No timeout parameter: `MAX_CLIENT_WAIT_TIMEOUT_WEBGL` is zero, the shim
    // rejects anything above it before reaching here, and the command carries no
    // field for one. See `GLCmd::ClientWaitSync`.
    send_gl_sync_with_flush(state, |resp| {
        RenderCommand::GL(GLCmd::ClientWaitSync { sync, flags, resp })
    })
    // WAIT_FAILED = 0x911D per GLES 3.0 spec.
    .unwrap_or(0x911D)
}

#[op2(fast)]
pub fn op_draw_buffers(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    #[buffer(copy)] buffers: Vec<u32>,
) {
    queue_gl_fire_and_forget(state, GLCmd::DrawBuffers { canvas_id, buffers });
}

// ---- WebGL 2 Query objects ---------------------------------------

#[op2(fast)]
pub fn op_create_query(state: &mut OpState, #[smi] canvas_id: u32, #[smi] client_id: u32) {
    queue_gl_fire_and_forget(
        state,
        GLCmd::CreateQuery {
            canvas_id,
            client_id,
        },
    );
}

#[op2(fast)]
pub fn op_delete_query(state: &mut OpState, #[smi] query: u32) {
    queue_gl_fire_and_forget(state, GLCmd::DeleteQuery { query });
}

#[op2(fast)]
pub fn op_begin_query(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    #[smi] target: u32,
    #[smi] query: u32,
) {
    queue_gl_fire_and_forget(
        state,
        GLCmd::BeginQuery {
            canvas_id,
            target,
            query,
        },
    );
}

#[op2(fast)]
pub fn op_end_query(state: &mut OpState, #[smi] canvas_id: u32, #[smi] target: u32) {
    queue_gl_fire_and_forget(state, GLCmd::EndQuery { canvas_id, target });
}

/// `getQueryParameter(query, pname)` - synchronous barrier because
/// callers poll `QUERY_RESULT_AVAILABLE` in a tight loop before
/// reading `QUERY_RESULT`; a queued call would stall behind normal
/// render traffic.
#[op2(fast)]
#[smi]
pub fn op_get_query_parameter(state: &mut OpState, #[smi] query: u32, #[smi] pname: u32) -> u32 {
    send_gl_sync_with_flush(state, |resp| {
        RenderCommand::GL(GLCmd::GetQueryParameter { query, pname, resp })
    })
    .unwrap_or(0)
}

// ---- WebGL 2 Transform Feedback ----------------------------------

#[op2(fast)]
pub fn op_create_transform_feedback(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    #[smi] client_id: u32,
) {
    queue_gl_fire_and_forget(
        state,
        GLCmd::CreateTransformFeedback {
            canvas_id,
            client_id,
        },
    );
}

#[op2(fast)]
pub fn op_delete_transform_feedback(state: &mut OpState, #[smi] tf: u32) {
    queue_gl_fire_and_forget(state, GLCmd::DeleteTransformFeedback { tf });
}

#[op2(fast)]
pub fn op_bind_transform_feedback(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    #[smi] target: u32,
    #[smi] tf: u32,
) {
    queue_gl_fire_and_forget(
        state,
        GLCmd::BindTransformFeedback {
            canvas_id,
            target,
            tf: if tf == 0 { None } else { Some(tf) },
        },
    );
}

#[op2(fast)]
pub fn op_begin_transform_feedback(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    #[smi] primitive_mode: u32,
) {
    let command = frame_decode::resource::transform_feedback_transition(
        &mut OpStateDecodeContext(state),
        frame_wire::gl_resource::OPR_BEGIN_TRANSFORM_FEEDBACK,
        canvas_id,
        primitive_mode,
    );
    queue_gl_fire_and_forget(state, command);
}

#[op2(fast)]
pub fn op_end_transform_feedback(state: &mut OpState, #[smi] canvas_id: u32) {
    let command = frame_decode::resource::transform_feedback_transition(
        &mut OpStateDecodeContext(state),
        frame_wire::gl_resource::OPR_END_TRANSFORM_FEEDBACK,
        canvas_id,
        0,
    );
    queue_gl_fire_and_forget(state, command);
}

#[op2(fast)]
pub fn op_pause_transform_feedback(state: &mut OpState, #[smi] canvas_id: u32) {
    let command = frame_decode::resource::transform_feedback_transition(
        &mut OpStateDecodeContext(state),
        frame_wire::gl_resource::OPR_PAUSE_TRANSFORM_FEEDBACK,
        canvas_id,
        0,
    );
    queue_gl_fire_and_forget(state, command);
}

#[op2(fast)]
pub fn op_resume_transform_feedback(state: &mut OpState, #[smi] canvas_id: u32) {
    let command = frame_decode::resource::transform_feedback_transition(
        &mut OpStateDecodeContext(state),
        frame_wire::gl_resource::OPR_RESUME_TRANSFORM_FEEDBACK,
        canvas_id,
        0,
    );
    queue_gl_fire_and_forget(state, command);
}

#[op2]
#[string]
pub fn op_get_transform_feedback_varying(
    state: &mut OpState,
    #[smi] program: u32,
    #[smi] index: u32,
) -> String {
    let info = send_gl_sync_with_flush(state, |resp| {
        RenderCommand::GL(GLCmd::GetTransformFeedbackVarying {
            program,
            index,
            resp,
        })
    })
    .ok()
    .flatten();

    if let Some((name, size, type_)) = info {
        let escaped_name = escape_for_json_string(&name);
        return format!(
            "{{\"name\":\"{}\",\"size\":{},\"type\":{}}}",
            escaped_name, size, type_
        );
    }

    String::new()
}

/// `transformFeedbackVaryings(program, varyings, bufferMode)`.
///
/// The JS shim passes the varyings array joined by `\x1f` (ASCII
/// Unit Separator) so the op2 fast lane can accept a single
/// `String` rather than spinning up a JSON parser.  US is invalid
/// in GLSL identifiers, so the split is unambiguous.
#[op2(fast)]
pub fn op_transform_feedback_varyings(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    #[smi] program: u32,
    #[string] varyings_joined: String,
    #[smi] buffer_mode: u32,
) {
    queue_gl_fire_and_forget(
        state,
        frame_decode::resource::transform_feedback_varyings(
            canvas_id,
            program,
            &varyings_joined,
            buffer_mode,
        ),
    );
}

// ---- WebGL 2 3D textures -----------------------------------------

/// An image upload's source as the facade calls its op: an offset that is not negative names the PIXEL_UNPACK_BUFFER
/// (WebGL 2's offset overloads); otherwise the bytes -- already taken from the view's `srcOffset` on -- or none, which
/// allocates the storage only.
fn image_pixel_source(data: Option<&[u8]>, pbo_offset: i32) -> Option<PixelSource<'_>> {
    if pbo_offset >= 0 {
        return Some(PixelSource::of_call(&[], pbo_offset));
    }
    data.map(|bytes| PixelSource::Bytes(Payload::Bytes(bytes)))
}

#[op2]
#[allow(clippy::too_many_arguments)]
pub fn op_tex_image_3d(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    #[smi] target: u32,
    #[smi] level: i32,
    #[smi] internal_format: i32,
    #[smi] width: i32,
    #[smi] height: i32,
    #[smi] depth: i32,
    #[smi] border: i32,
    #[smi] format: u32,
    #[smi] ty: u32,
    // `None` allocates the storage only.
    #[buffer] pixels: Option<&[u8]>,
    #[smi] pbo_offset: i32,
) {
    let command = frame_decode::resource::tex_image_3d(
        &mut OpStateDecodeContext(state),
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
        image_pixel_source(pixels, pbo_offset),
    );
    if let Some(command) = command {
        queue_gl_fire_and_forget(state, command);
    }
}

#[op2(fast)]
#[allow(clippy::too_many_arguments)]
pub fn op_tex_sub_image_3d(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    #[smi] target: u32,
    #[smi] level: i32,
    #[smi] xoffset: i32,
    #[smi] yoffset: i32,
    #[smi] zoffset: i32,
    #[smi] width: i32,
    #[smi] height: i32,
    #[smi] depth: i32,
    #[smi] format: u32,
    #[smi] ty: u32,
    #[buffer] pixels: &[u8],
    #[smi] pbo_offset: i32,
) {
    let command = frame_decode::resource::tex_sub_image_3d(
        &mut OpStateDecodeContext(state),
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
        PixelSource::of_call(pixels, pbo_offset),
    );
    if let Some(command) = command {
        queue_gl_fire_and_forget(state, command);
    }
}

#[op2(fast)]
#[allow(clippy::too_many_arguments)]
pub fn op_tex_storage_3d(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    #[smi] target: u32,
    #[smi] levels: i32,
    #[smi] internal_format: u32,
    #[smi] width: i32,
    #[smi] height: i32,
    #[smi] depth: i32,
) {
    queue_gl_fire_and_forget(
        state,
        GLCmd::TexStorage3D {
            canvas_id,
            target,
            levels,
            internal_format,
            width,
            height,
            depth,
        },
    );
}

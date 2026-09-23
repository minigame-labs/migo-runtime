use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use crossbeam_channel::{RecvTimeoutError, bounded};
use deno_core::{OpState, op2};
use deno_error::JsErrorBox;
use shared::{
    error::{EngineError, ErrorCode},
    op_state::CanvasOpState,
    protocol::render_cmd::{
        Canvas2DCmd, CanvasCmd, CanvasId, RenderCmdResp, RenderCommand, checked_canvas_pixel_count,
    },
    render_command_sender::SendError,
};

const SYNC_TIMEOUT: Duration = Duration::from_millis(1000);

/// Process-global counter for JS-allocated offscreen canvas ids.
///
/// The base is `shared`'s, not this crate's: the external producer allocates
/// from the same range for the record that stands in for this op, and the
/// renderer refuses a registration below it. One definition, because the
/// property is an agreement between three places rather than a local choice.
static NEXT_JS_OFFSCREEN_CANVAS_ID: AtomicU32 =
    AtomicU32::new(shared::protocol::render_cmd::PRODUCER_CANVAS_ID_BASE);

#[inline]
fn js_err_from_engine(e: EngineError) -> JsErrorBox {
    match &e.detail {
        Some(d) => JsErrorBox::generic(format!("[{:?}] {} ({})", e.code, e.msg, d)),
        None => JsErrorBox::generic(format!("[{:?}] {}", e.code, e.msg)),
    }
}

#[inline]
fn from_crossbeam_recv_err(e: RecvTimeoutError, timeout_msg: &'static str) -> EngineError {
    match e {
        RecvTimeoutError::Timeout => EngineError::new(ErrorCode::Timeout)
            .with_msg(timeout_msg)
            .with_detail("crossbeam recv_timeout".to_string()),
        RecvTimeoutError::Disconnected => EngineError::new(ErrorCode::Disconnected)
            .with_msg("response channel disconnected")
            .with_detail("crossbeam disconnected".to_string()),
    }
}

#[inline]
fn from_send_err(e: SendError) -> EngineError {
    match e {
        SendError::Timeout => EngineError::new(ErrorCode::Timeout)
            .with_msg("send render command timed out")
            .with_detail(e.to_string()),
        SendError::Disconnected => EngineError::new(ErrorCode::Disconnected)
            .with_msg("send render command failed")
            .with_detail(e.to_string()),
        SendError::Overflow => EngineError::new(ErrorCode::Internal)
            .with_msg("send render command overflowed")
            .with_detail(e.to_string()),
    }
}

#[inline]
fn send_canvas_sync<T>(
    ctx: &CanvasOpState,
    build: impl FnOnce(RenderCmdResp<T>) -> RenderCommand,
    timeout_msg: &'static str,
) -> Result<T, JsErrorBox> {
    let (resp_tx, resp_rx) = bounded::<Result<T, EngineError>>(1);
    let resp = RenderCmdResp::from_sync(resp_tx);

    ctx.tx
        .send_blocking_bounded(build(resp))
        .map_err(|e| js_err_from_engine(from_send_err(e)))?;

    match resp_rx.recv_timeout(SYNC_TIMEOUT) {
        Ok(res) => res.map_err(js_err_from_engine),
        Err(e) => Err(js_err_from_engine(from_crossbeam_recv_err(e, timeout_msg))),
    }
}

#[op2(fast)]
pub fn op_create_offscreen_canvas(
    state: &mut OpState,
    #[smi] width: u32,
    #[smi] height: u32,
) -> Result<u32, JsErrorBox> {
    if checked_canvas_pixel_count(width, height).is_none() {
        return Err(js_err_from_engine(
            EngineError::new(ErrorCode::InvalidArgument)
                .with_msg("offscreen canvas dimensions exceed the surface pixel cap"),
        ));
    }
    crate::rendering::webgl::frame_collector::flush_unified_barrier(state)
        .map_err(|e| js_err_from_engine(from_send_err(e)))?;
    let ctx = state.borrow::<CanvasOpState>();

    // Fire-and-forget: JS allocates the id locally and posts a
    // RegisterOffscreen command without waiting for the render thread.
    // Subsequent ops on this canvas (`getContext`, draw calls) queue
    // on the same FIFO, so ordering is preserved.
    let raw_id = NEXT_JS_OFFSCREEN_CANVAS_ID.fetch_add(1, Ordering::Relaxed);
    let id = CanvasId::from(raw_id);

    let cmd = RenderCommand::Canvas(CanvasCmd::RegisterOffscreen { id, width, height });

    ctx.tx.send(cmd).map_err(|e| {
        js_err_from_engine(
            EngineError::new(ErrorCode::Disconnected)
                .with_msg("send register_offscreen failed")
                .with_detail(e.to_string()),
        )
    })?;

    Ok(raw_id)
}

#[op2]
#[serde]
pub fn op_get_canvas_info(state: &mut OpState, #[smi] id: u32) -> Result<(u32, u32), JsErrorBox> {
    crate::rendering::webgl::frame_collector::flush_unified_barrier(state)
        .map_err(|e| js_err_from_engine(from_send_err(e)))?;
    let ctx = state.borrow::<CanvasOpState>();

    send_canvas_sync(
        ctx,
        |resp| RenderCommand::Canvas(CanvasCmd::GetInfo { id, resp }),
        "get_canvas_info timed out",
    )
}

#[op2]
pub fn op_resize_canvas(
    state: &mut OpState,
    #[smi] id: u32,
    w: Option<u32>,
    h: Option<u32>,
) -> Result<(), JsErrorBox> {
    // JS changes width and height in separate operations. Preflight each
    // supplied dimension (the manager validates the final pair atomically).
    if w.is_some_and(|width| checked_canvas_pixel_count(width, 1).is_none())
        || h.is_some_and(|height| checked_canvas_pixel_count(1, height).is_none())
    {
        return Err(js_err_from_engine(
            EngineError::new(ErrorCode::InvalidArgument)
                .with_msg("canvas resize dimension exceeds the surface pixel cap"),
        ));
    }
    // Route resize through the UnifiedFrameCollector so it interleaves
    // with `FillText` / `TexImage2DFromCanvas2D` in JS-issue order on
    // the render thread.  Previously this went through `ctx.tx` as an
    // immediate `CanvasCmd::ResizeCanvas`, which raced with the
    // collector-buffered draw/upload ops: cocos's text-label pattern
    // (canvas.width=W; fillText; texImage2D) ended up with multiple
    // resizes arriving before the corresponding fillTexts, leaving the
    // pbuffer at the LATEST size while the upload of an EARLIER cycle
    // requested a different size — random-blank labels on arm64.
    //
    // Falls back to the legacy `CanvasCmd::ResizeCanvas` path only when
    // the frame collector is not attached (smoke tests, headless
    // contexts) — production runtimes always have it.
    if let Some(collector) =
        state.try_borrow_mut::<crate::rendering::webgl::frame_collector::UnifiedFrameCollector>()
    {
        collector.push(id, Canvas2DCmd::ResizeCanvas { w, h });
        return Ok(());
    }
    let ctx = state.borrow::<CanvasOpState>();
    let cmd = RenderCommand::Canvas(CanvasCmd::ResizeCanvas { id, w, h });
    ctx.tx.send(cmd).map_err(|e| {
        js_err_from_engine(
            EngineError::new(ErrorCode::Disconnected)
                .with_msg("send resize_canvas failed")
                .with_detail(e.to_string()),
        )
    })?;
    Ok(())
}

/// Inner implementation for `op_destroy_canvas`, callable from tests.
///
/// Design §8 ordering invariant: flush the unified barrier BEFORE borrowing
/// `CanvasOpState` and sending the synchronous DestroyCanvas, so any pending
/// collector commands (Canvas2D draw ops, GL batch) for this canvas arrive at
/// the render thread as a FramePacket barrier strictly BEFORE the DestroyCanvas
/// command. Without this, a pending FillRect or GlBatch for canvas N could
/// arrive AFTER the render thread has already destroyed canvas N's resources,
/// causing use-after-free on the render side.
///
/// On flush error (channel timeout / disconnect), the error is surfaced to JS
/// immediately — we do NOT destroy-then-lose pending commands.
pub(crate) fn destroy_canvas_impl(state: &mut OpState, rid: u32) -> Result<(), JsErrorBox> {
    // Onscreen canvas (id=1) is not destroyed by render thread; just drop JS-side ownership.
    if rid == 1 {
        return Ok(());
    }

    // Flush any pending collector commands as a barrier before sending DestroyCanvas.
    // This ensures program-order: pending Canvas2D/GL work for this canvas arrives at
    // the render thread before the synchronous destroy.
    crate::rendering::webgl::frame_collector::flush_unified_barrier(state).map_err(|e| {
        use shared::error::{EngineError, ErrorCode};
        use shared::render_command_sender::SendError;
        let err = match e {
            SendError::Timeout => EngineError::new(ErrorCode::Timeout)
                .with_msg("flush barrier before destroy_canvas timed out"),
            SendError::Disconnected => EngineError::new(ErrorCode::Disconnected)
                .with_msg("flush barrier before destroy_canvas: channel disconnected"),
            SendError::Overflow => EngineError::new(ErrorCode::Internal)
                .with_msg("flush barrier before destroy_canvas: channel overflow"),
        };
        js_err_from_engine(err)
    })?;

    let ctx = state.borrow::<CanvasOpState>();
    let _ = send_canvas_sync(
        ctx,
        |resp| RenderCommand::Canvas(CanvasCmd::DestroyCanvas { id: rid, resp }),
        "destroy_canvas timed out",
    )?;

    Ok(())
}

#[op2(fast)]
pub fn op_destroy_canvas(state: &mut OpState, #[smi] rid: u32) -> Result<(), JsErrorBox> {
    destroy_canvas_impl(state, rid)
}

#[cfg(test)]
mod tests {
    use super::send_canvas_sync;
    use shared::{
        FrameOp,
        op_state::CanvasOpState,
        protocol::render_cmd::{Canvas2DCmd, CanvasCmd, GLCmd, RenderCmdResp, RenderCommand},
        render_command_sender::CommandSender,
    };

    #[test]
    fn send_canvas_sync_times_out_instead_of_dropping_when_queue_is_full() {
        let (tx, _rx) = CommandSender::new();
        let ctx = CanvasOpState::for_host(tx, 1);

        for _ in 0..ctx.tx.capacity() {
            ctx.tx
                .send(RenderCommand::Canvas(CanvasCmd::ResizeCanvas {
                    id: 7,
                    w: None,
                    h: None,
                }))
                .unwrap();
        }

        let err = send_canvas_sync(
            &ctx,
            |resp: RenderCmdResp<(u32, u32)>| {
                RenderCommand::Canvas(CanvasCmd::GetInfo { id: 7, resp })
            },
            "get_canvas_info timed out",
        )
        .unwrap_err();

        assert!(
            err.to_string().contains("[Timeout]"),
            "expected timeout error when sync canvas send hits a full queue, got: {err}"
        );
    }

    // ── Task 6 RED: op_destroy_canvas barrier ──────────────────────────────────

    /// Build a minimal OpState with both a CanvasOpState (connected render channel)
    /// and a UnifiedFrameCollector.
    fn new_canvas_op_state_with_collector(
        tx: shared::render_command_sender::CommandSender,
    ) -> deno_core::OpState {
        use crate::rendering::webgl::frame_collector::UnifiedFrameCollector;
        let mut state = deno_core::OpState::new(None);
        state.put(CanvasOpState::for_host(tx, 2));
        state.put(UnifiedFrameCollector::new());
        state
    }

    #[test]
    fn task6_destroy_canvas_flushes_barrier_before_destroy_command() {
        use crate::rendering::webgl::frame_collector::UnifiedFrameCollector;

        let (tx, rx) = CommandSender::new();

        // Spawn a responder that handles both the barrier FramePacket and the
        // sync DestroyCanvas. It collects commands in arrival order.
        let responder = std::thread::spawn(move || {
            let timeout = std::time::Duration::from_secs(2);
            let mut commands: Vec<&'static str> = Vec::new();
            loop {
                match rx.recv_timeout(timeout) {
                    Ok(RenderCommand::FramePacket(packet)) => {
                        // Any FramePacket arriving before DestroyCanvas is the barrier.
                        let has_canvas_batch = packet
                            .ops()
                            .iter()
                            .any(|op| matches!(op, FrameOp::CanvasBatch(_)));
                        let has_gl_batch = packet
                            .ops()
                            .iter()
                            .any(|op| matches!(op, FrameOp::GlBatch(_)));
                        if has_canvas_batch || has_gl_batch {
                            commands.push("barrier_packet");
                        } else {
                            commands.push("empty_packet");
                        }
                    }
                    Ok(RenderCommand::Canvas(CanvasCmd::DestroyCanvas { resp, id: _ })) => {
                        resp.ok(());
                        commands.push("destroy_canvas");
                        break;
                    }
                    Ok(_) => {
                        commands.push("other");
                    }
                    Err(_) => break,
                }
            }
            commands
        });

        let mut state = new_canvas_op_state_with_collector(tx);

        // Push a 2D command into the collector to simulate pending work.
        {
            let collector = state.borrow_mut::<UnifiedFrameCollector>();
            collector.push_canvas2d(
                7,
                Canvas2DCmd::FillRect {
                    x: 0.0,
                    y: 0.0,
                    w: 10.0,
                    h: 10.0,
                },
            );
        }

        // Call op_destroy_canvas for canvas 7 (not the immortal rid=1).
        super::destroy_canvas_impl(&mut state, 7).expect("op_destroy_canvas must not fail");

        let commands = responder.join().expect("responder must not panic");
        assert_eq!(
            commands.len(),
            2,
            "must see exactly barrier_packet + destroy_canvas, got: {:?}",
            commands
        );
        assert_eq!(
            commands[0], "barrier_packet",
            "barrier FramePacket must arrive BEFORE DestroyCanvas"
        );
        assert_eq!(
            commands[1], "destroy_canvas",
            "DestroyCanvas must arrive AFTER the barrier"
        );
    }

    #[test]
    fn task6_destroy_canvas_rid1_never_destroys_and_does_not_flush() {
        use crate::rendering::webgl::frame_collector::UnifiedFrameCollector;

        let (tx, rx) = CommandSender::new();
        let mut state = new_canvas_op_state_with_collector(tx);

        // Push pending work into the collector.
        {
            let collector = state.borrow_mut::<UnifiedFrameCollector>();
            collector.push_canvas2d(
                1,
                Canvas2DCmd::FillRect {
                    x: 0.0,
                    y: 0.0,
                    w: 1.0,
                    h: 1.0,
                },
            );
        }

        // Destroying rid=1 must be a silent no-op: no flush, no DestroyCanvas sent.
        super::destroy_canvas_impl(&mut state, 1).expect("op_destroy_canvas(1) must succeed");

        // Give any async messages time to arrive.
        std::thread::sleep(std::time::Duration::from_millis(10));

        assert!(
            rx.try_recv().is_err(),
            "destroying the immortal onscreen canvas (rid=1) must send nothing to the render thread"
        );

        // Collector still has the pending work (we didn't flush it).
        let bytes = state
            .borrow::<UnifiedFrameCollector>()
            .approx_pending_bytes();
        assert!(
            bytes > 0,
            "pending 2D work must remain in the collector after no-op rid=1 destroy"
        );
    }

    #[test]
    fn task6_destroy_canvas_with_pending_gl_batch_flushes_gl_before_destroy() {
        use crate::rendering::webgl::frame_collector::UnifiedFrameCollector;

        let (tx, rx) = CommandSender::new();

        let responder = std::thread::spawn(move || {
            let timeout = std::time::Duration::from_secs(2);
            let mut order: Vec<&'static str> = Vec::new();
            loop {
                match rx.recv_timeout(timeout) {
                    Ok(RenderCommand::FramePacket(packet)) => {
                        let has_gl = packet
                            .ops()
                            .iter()
                            .any(|op| matches!(op, FrameOp::GlBatch(_)));
                        if has_gl {
                            order.push("gl_barrier");
                        } else {
                            order.push("empty_barrier");
                        }
                    }
                    Ok(RenderCommand::Canvas(CanvasCmd::DestroyCanvas { resp, id: _ })) => {
                        resp.ok(());
                        order.push("destroy");
                        break;
                    }
                    Ok(_) => order.push("other"),
                    Err(_) => break,
                }
            }
            order
        });

        let mut state = new_canvas_op_state_with_collector(tx);

        // Push a GL command into the collector.
        {
            let collector = state.borrow_mut::<UnifiedFrameCollector>();
            collector.push_gl(GLCmd::Clear {
                canvas_id: 7,
                bit_field: 0x4000,
            });
        }

        super::destroy_canvas_impl(&mut state, 7).expect("op_destroy_canvas must succeed");

        let order = responder.join().expect("responder panicked");
        assert!(
            order.contains(&"gl_barrier"),
            "pending GL work must be flushed before destroy; got: {order:?}"
        );
        let gl_pos = order.iter().position(|&s| s == "gl_barrier").unwrap();
        let destroy_pos = order.iter().position(|&s| s == "destroy").unwrap();
        assert!(
            gl_pos < destroy_pos,
            "GL barrier must come before DestroyCanvas; got order: {order:?}"
        );
    }

    #[test]
    fn canvas_entrypoints_and_2d_batching_keep_the_allocation_caps() {
        let canvas = include_str!("canvas.rs");
        let context2d = include_str!("../rendering/webgl/context2d.rs");
        let js = include_str!("../rendering/webgl/02_2d_context.js");
        let manager = include_str!("../../../graphics/src/canvas/manager/mod.rs");
        let surface = include_str!("../../../graphics/src/backend/gl/surface.rs");

        assert!(canvas.contains("checked_canvas_pixel_count(width, height)"));
        assert!(context2d.contains("checked_canvas_rgba_byte_len(width, height)"));
        assert!(manager.contains("checked_canvas_pixel_count(w, h)"));
        assert!(manager.contains("checked_canvas_pixel_count(new_w, new_h)"));
        assert!(surface.contains("checked_canvas_rgba_byte_len(width, height)"));
        assert!(context2d.contains("entry_count > MAX_DRAW_IMAGE_BATCH_ENTRIES"));
        assert!(js.contains("MAX_DRAW_IMAGE_BATCH_ENTRIES = 65_536"));
        assert!(js.contains("checkedImageDataDimensions"));
        assert!(js.contains("\"IndexSizeError\""));
        assert!(js.contains("Math.abs(width)"));
        assert!(js.contains("x += rawWidth"));
        assert!(js.contains("y += rawHeight"));
    }
}

//! The main canvas must describe the surface the content is drawing into.
//!
//! Android delivers `surfaceCreated` at one size and `surfaceChanged` at
//! another moments later (the system bars hiding is the common cause), and the
//! content's `migo.createCanvas()` lands in between. A canvas that reports the
//! first of those two sizes forever leaves every game that fills its canvas
//! with a dead band along the edge the surface grew into -- while every layer
//! reports success, because nothing in the rendering path is wrong.
//!
//! These boot a real runtime and answer `op_get_canvas_info` from a stand-in
//! render thread, so what is under test is the JS the content actually runs
//! rather than a description of it.

#[cfg(test)]
mod canvas_follows_surface_tests {
    use std::{
        path::PathBuf,
        sync::{
            Arc,
            atomic::{AtomicBool, AtomicU32, Ordering},
        },
    };

    use deno_core::{FastString, JsRuntime, RuntimeOptions};
    use shared::{
        channel::ThreadWakeup,
        device::gpu_caps::GpuCaps,
        network_policy::NetworkPolicy,
        op_state::{AudioSender, HostOpState},
        protocol::render_cmd::{CanvasCmd, RenderCommand},
        render_command_sender::CommandSender,
    };

    /// The size the stand-in render thread reports for the onscreen canvas.
    ///
    /// Shared so a test can move the surface between JS turns, which is the
    /// whole scenario: the render thread resizes the drawing buffer when the
    /// platform hands it a new surface, and the question is whether the JS half
    /// ever finds out.
    #[derive(Clone)]
    struct SurfaceSize(Arc<(AtomicU32, AtomicU32)>);

    impl SurfaceSize {
        fn new(width: u32, height: u32) -> Self {
            Self(Arc::new((AtomicU32::new(width), AtomicU32::new(height))))
        }

        fn set(&self, width: u32, height: u32) {
            self.0.0.store(width, Ordering::SeqCst);
            self.0.1.store(height, Ordering::SeqCst);
        }

        fn get(&self) -> (u32, u32) {
            (
                self.0.0.load(Ordering::SeqCst),
                self.0.1.load(Ordering::SeqCst),
            )
        }
    }

    /// Answer the synchronous canvas commands the JS half blocks on.
    ///
    /// Only `GetInfo` and `DestroyCanvas` carry responders; everything else is
    /// fire-and-forget and is drained so the bounded queue cannot fill.
    /// `ResizeCanvas` is recorded through `SurfaceSize` so an explicit
    /// `canvas.width = N` is reflected back the way a real render thread would.
    fn spawn_render_stub(
        rx: crossbeam_channel::Receiver<RenderCommand>,
        size: SurfaceSize,
    ) -> std::thread::JoinHandle<()> {
        std::thread::spawn(move || {
            let timeout = std::time::Duration::from_millis(500);
            while let Ok(cmd) = rx.recv_timeout(timeout) {
                match cmd {
                    RenderCommand::Canvas(CanvasCmd::GetInfo { resp, .. }) => {
                        resp.ok(size.get());
                    }
                    RenderCommand::Canvas(CanvasCmd::DestroyCanvas { resp, .. }) => {
                        resp.ok(());
                    }
                    RenderCommand::Canvas(CanvasCmd::ResizeCanvas { w, h, .. }) => {
                        let (cur_w, cur_h) = size.get();
                        size.set(w.unwrap_or(cur_w), h.unwrap_or(cur_h));
                    }
                    _ => {}
                }
            }
        })
    }

    fn test_host_state(render_tx: CommandSender) -> HostOpState {
        let (host_tx, _critical_host_tx, _host_rx) = shared::host_channel::channel(1);

        HostOpState {
            callback_ids: std::sync::Arc::new(shared::callback_id::CallbackIdAllocator::default()),
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
            audio_tx: AudioSender::new(shared::audio_channel::disconnected(), ThreadWakeup::new()),
            host_tx,
            // No window-info service, exactly like a C-ABI host that supplies
            // none: the canvas must still follow the surface.
            device_services: None,
            raf_rx: None,
            raf_demand: Arc::new(shared::raf_signal::RafDemand::new()),
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
        }
    }

    fn boot_runtime(size: SurfaceSize) -> (JsRuntime, std::thread::JoinHandle<()>) {
        let (render_tx, render_rx) = CommandSender::new();
        let stub = spawn_render_stub(render_rx, size);
        let mut rt = JsRuntime::new(RuntimeOptions {
            extensions: crate::main_extensions(test_host_state(render_tx)),
            ..Default::default()
        });
        crate::harden_global_scope(&mut rt);
        (rt, stub)
    }

    fn eval(rt: &mut JsRuntime, src: &str) {
        rt.execute_script("<test>", FastString::from(src.to_string()))
            .expect("script must not throw");
    }

    /// Assert in JS, so the failure message carries the numbers the content saw.
    fn assert_js(rt: &mut JsRuntime, src: &str) {
        let wrapped = format!(
            "(()=>{{ {src}; if (!__ok) throw new Error('assertion failed: ' + __msg); }})()"
        );
        rt.execute_script("<test:assert>", FastString::from(wrapped))
            .expect("assertion script");
    }

    /// Stand in for the host, which invokes this hook once the render thread has
    /// finished installing the new surface.
    const TRIGGER_RESIZE: &str =
        "globalThis[Symbol.for('Migo.hostBridge')]._internalTriggerWindowResize()";

    #[test]
    fn a_canvas_the_content_never_sized_follows_the_surface() {
        let size = SurfaceSize::new(360, 745);
        let (mut rt, _stub) = boot_runtime(size.clone());

        eval(&mut rt, "globalThis.__c = migo.createCanvas();");
        assert_js(
            &mut rt,
            "let __ok = __c.width === 360 && __c.height === 745; \
             let __msg = 'first size ' + __c.width + 'x' + __c.height",
        );

        // The system bars hide: same window, taller surface.
        size.set(360, 780);
        eval(&mut rt, TRIGGER_RESIZE);

        assert_js(
            &mut rt,
            "let __ok = __c.width === 360 && __c.height === 780; \
             let __msg = 'after the surface grew the canvas still reports ' \
                 + __c.width + 'x' + __c.height",
        );
    }

    /// The negative control for the test above: with no surface change, nothing
    /// moves. Without it, a fix that simply reported the render thread's size on
    /// every call would look identical.
    #[test]
    fn a_resize_that_did_not_change_the_surface_changes_nothing() {
        let size = SurfaceSize::new(360, 745);
        let (mut rt, _stub) = boot_runtime(size.clone());

        eval(&mut rt, "globalThis.__c = migo.createCanvas();");
        eval(&mut rt, TRIGGER_RESIZE);

        assert_js(
            &mut rt,
            "let __ok = __c.width === 360 && __c.height === 745; \
             let __msg = 'size drifted to ' + __c.width + 'x' + __c.height",
        );
    }

    /// Content that picked its own backing store owns it.
    ///
    /// A DPR-naive engine (Phaser's `Scale.NONE`, vanilla 2D at resolution 1)
    /// sets a fixed size on purpose and re-reads it forever; a canvas that
    /// silently grew under it is the same defect in the other direction. This is
    /// also what a browser does -- an explicitly sized canvas does not resize
    /// because the window did.
    #[test]
    fn a_canvas_the_content_sized_is_never_overwritten() {
        let size = SurfaceSize::new(360, 745);
        let (mut rt, _stub) = boot_runtime(size.clone());

        eval(
            &mut rt,
            "globalThis.__c = migo.createCanvas(); __c.width = 800; __c.height = 600;",
        );

        // The surface moves underneath it.
        size.set(360, 780);
        eval(&mut rt, TRIGGER_RESIZE);

        assert_js(
            &mut rt,
            "let __ok = __c.width === 800 && __c.height === 600; \
             let __msg = 'the engine overwrote a size the content chose: ' \
                 + __c.width + 'x' + __c.height",
        );
    }

    /// Setting one dimension claims the canvas, not just that dimension.
    ///
    /// `canvas.width = N` alone is a real pattern, and a half-owned canvas whose
    /// height still tracked the surface would be a shape nobody can reason about.
    #[test]
    fn setting_one_dimension_claims_the_whole_canvas() {
        let size = SurfaceSize::new(360, 745);
        let (mut rt, _stub) = boot_runtime(size.clone());

        eval(
            &mut rt,
            "globalThis.__c = migo.createCanvas(); __c.width = 800;",
        );

        size.set(360, 780);
        eval(&mut rt, TRIGGER_RESIZE);

        assert_js(
            &mut rt,
            "let __ok = __c.height === 745; \
             let __msg = 'height tracked the surface on a canvas the content had claimed: ' \
                 + __c.height",
        );
    }

    /// Offscreen canvases are the content's from the moment it asks for one, so
    /// a surface change must not touch them.
    #[test]
    fn offscreen_canvases_do_not_follow_the_surface() {
        let size = SurfaceSize::new(360, 745);
        let (mut rt, _stub) = boot_runtime(size.clone());

        eval(
            &mut rt,
            "globalThis.__main = migo.createCanvas(); globalThis.__off = migo.createCanvas();",
        );
        let before = size.get();

        size.set(360, 780);
        eval(&mut rt, TRIGGER_RESIZE);

        assert_js(
            &mut rt,
            &format!(
                "let __ok = __off.width === {} && __off.height === {}; \
                 let __msg = 'an offscreen canvas tracked the surface: ' \
                     + __off.width + 'x' + __off.height",
                before.0, before.1
            ),
        );
    }

    /// Content that never asked for a canvas must not be given one because the
    /// window moved: acquiring the onscreen canvas has render-thread side
    /// effects, and a resize is not a request for it.
    #[test]
    fn a_surface_change_does_not_create_a_canvas_nobody_asked_for() {
        let size = SurfaceSize::new(360, 745);
        let (mut rt, _stub) = boot_runtime(size.clone());

        size.set(360, 780);
        eval(&mut rt, TRIGGER_RESIZE);

        // If the resize had created it, this first `createCanvas()` would return
        // the canvas built during the resize rather than one measured now.
        eval(&mut rt, "globalThis.__c = migo.createCanvas();");
        assert_js(
            &mut rt,
            "let __ok = __c.width === 360 && __c.height === 780; \
             let __msg = 'canvas ' + __c.width + 'x' + __c.height",
        );
    }
}

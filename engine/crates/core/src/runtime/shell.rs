//! The part of a session that has nothing to do with JavaScript.
//!
//! A session owns a render thread, a surface, a frame clock, audio, a platform,
//! a network policy, GPU capabilities, background and context-loss state, and a
//! generation. None of that changes with where the content's JavaScript runs --
//! and on iOS Performance+ it runs in another process entirely, inside WebKit's
//! WebContent, because that is the only place Apple grants a JIT.
//!
//! So the bring-up lives here, once, and each execution mode takes ownership of
//! the parts it needs. What is deliberately *not* here is the ownership: the
//! embedded `Host` keeps its own fields and reads them as `self.render`, not
//! `self.shell.render`. Moving construction is what avoids duplicating four
//! hundred lines of careful ordering; moving ownership would have renamed every
//! use site in a 1800-line file for no property either mode gains.
//!
//! The ordering here is load-bearing and was measured, not guessed. See
//! `docs/PROGRESS-apple-android.md` and the Android startup work: the render
//! thread is launched before the JavaScript runtime is built so GPU bring-up
//! and V8 construction overlap, and `gpu_init_started` is taken at the launch
//! rather than at the wait so the two-second budget is not restarted by
//! whatever happens in between.

use std::sync::{Arc, atomic::AtomicBool};
use std::time::Instant;

use tracing::info;

use shared::{
    config::InitOptions,
    error::EngineResult,
    host_channel::CriticalHostCommandSender,
    op_state::{HostTx, NetworkPolicy, RafRx},
    protocol::host_cmd::HostCommand,
    raf_signal::RafDemandRef,
    render_event::RenderEventReceiver,
    surface::SurfaceLease,
};

use crate::runtime::HostId;
use crate::services::{AudioService, PlatformServices, RenderService};

/// Cleans process-global registrations if `Host::new` exits before ownership
/// transfers to the fully assembled `Host` and its normal `Drop` path.
pub(crate) struct HostStartupGuard {
    id: HostId,
    armed: bool,
    console_registered: bool,
}

impl HostStartupGuard {
    fn new(id: HostId) -> Self {
        Self {
            id,
            armed: true,
            console_registered: false,
        }
    }

    fn mark_console_registered(&mut self) {
        self.console_registered = true;
    }

    pub(crate) fn disarm(&mut self) {
        self.armed = false;
        self.console_registered = false;
    }
}

impl Drop for HostStartupGuard {
    fn drop(&mut self) {
        if self.console_registered {
            shared::console_log::unregister_console_log(self.id);
        }
        // The frame clock's sender used to need a branch here, and needing one was
        // the problem: it lived in a registry of its own, so somebody had to
        // remember to retire it on each of spawn failure, startup failure and
        // ordinary exit. The external-frame product had no such owner and leaked an
        // entry per session. It now sits in the Host registry handle and is retired
        // by `unregister_sender`, which every exit path already goes through.
        // The text texture cache registers itself lazily, from whichever
        // of the render thread or the JS extension reaches this session
        // first, so there is no `mark_*_registered` edge to hang this on.
        // A `Host::new` that failed after either could have created it and
        // there is no assembled `Host` to run the normal teardown, so
        // clear it whenever the guard is still armed. Unregistering an id
        // that was never registered is a no-op.
        //
        // The image alias table is registered from one place only — the JS
        // extension's bring-up — but leaks the same way for the same reason, and
        // this guard drops after the runtime it would have been registered by,
        // so nothing can register it again behind us.
        if self.armed {
            shared::text_texture_cache::unregister_text_cache(self.id);
            // Registered only by the embedded runtime's bring-up, so only a
            // build that has one can have created it.
            #[cfg(feature = "embedded-v8")]
            runtime_v8::unregister_image_cache(self.id);
            shared::services::forget_downloaded_zips(self.id);
        }
    }
}

/// Everything a session owns that does not depend on where its JavaScript runs.
///
/// Fields rather than accessors: both execution modes destructure this once at
/// construction and then own the parts outright. An accessor-shaped shell would
/// put a borrow between the host loop and its own render service, which is the
/// kind of ceremony that gets refactored away later by someone who does not
/// know why it was there.
pub(crate) struct SessionShell {
    pub(crate) t_start: Instant,
    /// Armed until the caller has a fully assembled session. Handed back so the
    /// mode-specific half stays inside its protection: a failure between here
    /// and the assembled session still unregisters the process-global tables.
    pub(crate) startup_guard: HostStartupGuard,

    pub(crate) render: RenderService,
    pub(crate) render_events: RenderEventReceiver,
    pub(crate) render_notify: Arc<tokio::sync::Notify>,
    /// When the render thread was launched, i.e. when the GPU readiness budget
    /// started -- not when someone got round to waiting for it.
    pub(crate) gpu_init_started: Instant,

    pub(crate) audio: AudioService,
    pub(crate) network_policy: NetworkPolicy,
    pub(crate) gpu_caps: Arc<shared::device::gpu_caps::GpuCaps>,
    pub(crate) context_lost: Arc<shared::op_state::ContextLostState>,
    /// Why the render worker stopped, if it stopped on its own. Whichever mode
    /// observes the frame clock closing reads this, because nothing the worker
    /// could send would be drained after that.
    pub(crate) render_exit: Arc<shared::render_exit::RenderExit>,

    /// The frame clock. Both modes need it; they differ only in where the tick
    /// goes -- an async op resolving in this process, or a control message to a
    /// producer in another one.
    pub(crate) raf_rx: RafRx,
    pub(crate) raf_demand: RafDemandRef,
    pub(crate) request_vsync: Option<Arc<dyn Fn() + Send + Sync>>,

    pub(crate) backgrounded: Arc<AtomicBool>,
    pub(crate) timer_backgrounded: Arc<AtomicBool>,
}

impl SessionShell {
    /// Bring up the render thread, the frame clock and the session's services.
    ///
    /// Everything here runs before either execution mode exists, and the order
    /// is the one the embedded path already used -- this function is that code,
    /// moved rather than rewritten, so the startup profile is unchanged.
    pub(crate) fn build(
        id: HostId,
        host_tx: &HostTx,
        critical_host_tx: CriticalHostCommandSender,
        surface: Option<SurfaceLease>,
        graphics_platform: graphics::egl_platform::GraphicsPlatform,
        platform: &Arc<dyn PlatformServices>,
        init_options: &InitOptions,
        surface_control: Arc<shared::surface::SurfaceControl>,
        vsync_rx: Option<crossbeam_channel::Receiver<f64>>,
    ) -> EngineResult<Self> {
        // ---- Startup timing instrumentation ----
        let t_start = Instant::now();

        // ---- RAF signal (render thread → JS async op) ----
        // On Android: eventfd (low-latency epoll wake).
        // Other platforms: tokio mpsc channel (unchanged behavior).
        let (raf_tx, raf_rx) = shared::raf_signal::create_raf_pair();

        // ---- R1 RAF demand latch (host op <-> render thread) ----
        let raf_demand = Arc::new(shared::raf_signal::RafDemand::new());
        // Allocate the first session ticket up front so pre-signals (free-run
        // RAF) have a stable ticket to match from the very first frame.
        raf_demand.begin_session();

        // ---- VSync channel (Choreographer JNI → render thread) ----
        // Created with its sender before this thread existed, and handed in: the
        // sender belongs to the direct ingress the caller receives from the Host
        // registration, and the caller receives that before the thread runs. Whether
        // there is one at all is still the same question, asked in the one place
        // that can answer it early enough. Passing a never-fed `Some(receiver)`
        // where the engine paces itself would make RenderThread wait for timestamps
        // nobody sends, freezing the loop.
        let uses_external_vsync = platform.uses_external_vsync();
        debug_assert_eq!(
            vsync_rx.is_some(),
            uses_external_vsync,
            "the frame clock's two ends must agree about whether it exists"
        );
        let mut startup_guard = HostStartupGuard::new(id);

        // Immutable per-host policy. Audio captures this in a lazy client
        // factory; JS network ops keep the same snapshot across restarts.
        let network_policy = {
            use shared::op_state::NetworkPolicy;
            let mut policy = NetworkPolicy::default();
            if let Some(wl) = init_options.extras().get("domain_whitelist") {
                if let Some(arr) = wl.as_array() {
                    policy.domain_whitelist = arr
                        .iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect();
                }
            }
            if let Some(v) = init_options.extras().get("enforce_https") {
                policy.enforce_https = v.as_bool().unwrap_or(false);
            }
            policy
        };

        // ---- Services ----
        // AudioService is lazy — no thread spawned until the first
        // real audio command.  Saves ~80 ms on cold start.
        let audio = AudioService::new(host_tx.clone(), network_policy.clone());
        let gpu_caps = shared::device::gpu_caps::GpuCaps::new();

        // Authoritative GL-context-loss state. Created here (before the render
        // thread) and written exclusively by the render thread (edge-triggered
        // level + epoch); the host and JS `op_gl_is_context_lost` only read it.
        // Survives JS runtime restarts (same GL context / render thread) via the
        // shared `Arc`.
        let context_lost = Arc::new(shared::op_state::ContextLostState::default());

        // Created here, beside the other levels the render thread writes and the
        // session reads. The worker publishes into it on the way out.
        let render_exit = Arc::new(shared::render_exit::RenderExit::default());

        // Coalescing render-feedback wake: the render thread's event channel
        // fires this after every successfully enqueued event, so the host loop
        // drains + reconciles promptly (Canvas/GL/swap/RAF-backpressure/context)
        // without a polling timer. Tokio `Notify` coalesces a burst to one permit
        // and latches a permit if no waiter is registered, so an event emitted
        // inside a JS poll is delivered on the next select iteration rather than
        // lost.
        let render_notify = Arc::new(tokio::sync::Notify::new());
        let render_wake: Option<Arc<dyn Fn() + Send + Sync>> = {
            let notify = render_notify.clone();
            Some(Arc::new(move || {
                notify.notify_one();
            }))
        };

        // R1: one-shot frame arm, one route per platform.
        //
        // With an external vsync source this routes to `platform.request_vsync(id)`
        // (Android posts a single Choreographer frame callback via JNI). Without
        // one the engine paces frames itself and its clock stops whenever nothing
        // is animating, so the arm has to wake the render thread instead: a
        // demand nudge it selects on. The payload is the wakeup — demand itself is
        // read from the latch — so a nudge already pending is not worth queueing
        // twice, which is what the single slot and `try_send` express.
        //
        // Either way the closure keeps `graphics` decoupled from `platform`: the
        // render thread and `op_await_next_frame` only invoke `Arc<dyn Fn()>`.
        let (frame_demand_rx, request_vsync): (
            Option<crossbeam_channel::Receiver<()>>,
            Option<Arc<dyn Fn() + Send + Sync>>,
        ) = if uses_external_vsync {
            let platform = platform.clone();
            (None, Some(Arc::new(move || platform.request_vsync(id))))
        } else {
            let (nudge_tx, nudge_rx) = crossbeam_channel::bounded::<()>(1);
            (
                Some(nudge_rx),
                Some(Arc::new(move || {
                    let _ = nudge_tx.try_send(());
                })),
            )
        };
        let install_reporter_tx = critical_host_tx.clone();
        let report_surface_installed: Arc<
            dyn Fn(shared::surface::SurfaceCandidateRevision) + Send + Sync,
        > = Arc::new(move |revision| {
            let _ = install_reporter_tx.send(HostCommand::SurfaceInstalled { revision });
        });
        let report_surface_loss: Arc<
            dyn Fn(shared::surface::PublicSurfaceGeneration, shared::surface::SurfaceLossReason)
                + Send
                + Sync,
        > = Arc::new(move |public_generation, reason| {
            let _ = critical_host_tx.send(HostCommand::SurfaceLost {
                public_generation,
                reason,
            });
        });

        let render = RenderService::new(
            raf_tx,
            vsync_rx,
            frame_demand_rx,
            id,
            surface,
            graphics_platform,
            init_options.pixel_ratio(),
            init_options.target_fps(),
            Some(init_options.cache_dir().to_path_buf()),
            gpu_caps.clone(),
            context_lost.clone(),
            render_exit.clone(),
            render_wake,
            raf_demand.clone(),
            // The render thread's own arm route is its clock when the engine paces
            // frames, so it is deliberately not handed the nudge: a thread nudging
            // itself is a wakeup that arms nothing.
            uses_external_vsync.then(|| request_vsync.clone()).flatten(),
            surface_control,
            report_surface_loss,
            report_surface_installed,
        )?;
        // Preserve the old two-second render-startup budget. V8 construction
        // below consumes this same deadline while the render thread initializes.
        let gpu_init_started = Instant::now();
        let render_events = render.events();

        let backgrounded = Arc::new(AtomicBool::new(false));
        let timer_backgrounded = Arc::new(AtomicBool::new(false));

        // ---- Console log buffer (debug only) ----
        if init_options.debug_enabled() {
            shared::console_log::register_console_log(id);
            startup_guard.mark_console_registered();
        }

        info!(
            "[Host {}] session shell: {:.1}ms (render launched, frame clock wired)",
            id,
            t_start.elapsed().as_secs_f64() * 1000.0
        );

        Ok(Self {
            t_start,
            startup_guard,
            render,
            render_events,
            render_notify,
            gpu_init_started,
            audio,
            network_policy,
            gpu_caps,
            context_lost,
            render_exit,
            raf_rx,
            raf_demand,
            request_vsync,
            backgrounded,
            timer_backgrounded,
        })
    }
}

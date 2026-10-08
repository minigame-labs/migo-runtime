use tracing::{info, warn};

use graphics::RenderThread;

use shared::{
    error::{EngineError, EngineResult, ErrorCode},
    protocol::render_cmd::{CanvasCmd, RenderCommand},
    render_command_sender::SendError,
    render_event::RenderEventReceiver,
    surface::{PixelRatio, SurfaceGeneration, SurfaceLease},
};

use super::{SurfaceAttachmentSlot, SurfaceTransitionError};

pub(crate) struct RenderService {
    host_id: i32,
    attachment: SurfaceAttachmentSlot,
    /// Where the Surface to install is published. Held rather than only handed to
    /// the render thread, because every update publishes through it.
    surface_control: std::sync::Arc<shared::surface::SurfaceControl>,
    /// The publication whose install has been asked for and not yet reported.
    ///
    /// Nothing waits for a recreate, so this is how a report is matched to the request
    /// that earned it: `SurfaceInstalled` arrives on the must-deliver channel and
    /// `confirm_install` commits only for the publication recorded here. A report for
    /// any other -- a superseded attempt's -- is ignored rather than allowed to conclude
    /// something about a Surface this service is no longer waiting on.
    ///
    /// A revision and not the lease, deliberately. Holding the lease here would pin the
    /// host's native Surface until the report arrived -- and if it never did, or the
    /// host detached first, past the point `RELEASED` is meant to be publishable. That
    /// is the defect this whole arrangement exists to remove, and keeping a second copy
    /// of the lease would have reintroduced it one layer up. The candidate level
    /// already holds it, and hands it back only while it is live.
    outstanding: Option<shared::surface::SurfaceCandidateRevision>,
    thread: RenderThread,
}

/// Only the embedded execution restores a surface today. Restoring one in an
/// external-frame session means the producer -- in another process -- has to
/// learn that its surface generation advanced before it builds another packet,
/// and that announcement travels on the control channel. Restoring without it
/// would leave a producer drawing against a generation the renderer has retired.
#[cfg(feature = "embedded-v8")]
fn surface_for_restore(lease: Option<SurfaceLease>) -> EngineResult<SurfaceLease> {
    lease.ok_or_else(|| {
        EngineError::new(ErrorCode::InvalidOperation)
            .with_msg("restore surface: no live surface available")
    })
}

/// Classify an arbitration rejection by what it means, not by where it happened.
///
/// A stale generation is the host having taken its Surface back, which is nobody's
/// fault and nothing to report: `Cancelled` is the code the rest of this file already
/// uses for "what this was for is gone". A conflicting live generation is an ordering
/// error -- two live generations at once -- and stays `InvalidOperation`.
///
/// Both used to be `InvalidOperation`, and the consumer's decision about whether to
/// report is made on the code, so an ordinary attach/detach race reached the host as
/// MIGO_ERROR_INTERNAL. Deciding it here is what makes that decision possible at all:
/// this is the only place that knows which of the two it was.
fn transition_error(context: &'static str, error: SurfaceTransitionError) -> EngineError {
    let code = match error {
        SurfaceTransitionError::StaleGeneration => ErrorCode::Cancelled,
        SurfaceTransitionError::ConflictingLiveGeneration => ErrorCode::InvalidOperation,
    };
    EngineError::new(code)
        .with_msg(context)
        .with_detail(error.to_string())
}

// Covers `surface_for_restore`, which exists only for the embedded execution.
#[cfg(all(test, feature = "embedded-v8"))]
mod tests {
    use super::surface_for_restore;

    #[test]
    fn restore_surface_requires_live_surface() {
        let err = surface_for_restore(None).unwrap_err();
        assert_eq!(err.code, shared::error::ErrorCode::InvalidOperation);
        assert_eq!(err.msg, "restore surface: no live surface available");
    }
}

impl RenderService {
    /// How many times one Surface update may be attempted. See `update_surface`.
    const INSTALL_ATTEMPTS: u32 = 3;

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        raf_tx: shared::raf_signal::RafSender,
        vsync_rx: Option<crossbeam_channel::Receiver<f64>>,
        frame_demand_rx: Option<crossbeam_channel::Receiver<()>>,
        host_id: i32,
        // `None` when the session is created before its window Surface
        // exists. Everything the render thread does that a Surface is not
        // needed for -- EGL display and config, the pbuffer resource context,
        // the GLES dispatch table, capability detection, Skia -- then runs
        // while the host application is still laying out its window, instead
        // of after. Measured on a Mate 30 Pro that is ~50 ms taken off the
        // path to first frame, in a window that was provably idle: an
        // Activity that rotates to a landscape game sits 150 ms between
        // `onCreate` and `surfaceCreated` doing nothing the engine could not
        // have been doing.
        initial_surface: Option<SurfaceLease>,
        graphics_platform: graphics::egl_platform::GraphicsPlatform,
        pixel_ratio: f32,
        target_fps: i32,
        app_cache_dir: Option<std::path::PathBuf>,
        gpu_caps: std::sync::Arc<shared::device::gpu_caps::GpuCaps>,
        context_lost: std::sync::Arc<shared::op_state::ContextLostState>,
        render_exit: std::sync::Arc<shared::render_exit::RenderExit>,
        wake: Option<std::sync::Arc<dyn Fn() + Send + Sync>>,
        raf_demand: shared::raf_signal::RafDemandRef,
        request_vsync: Option<std::sync::Arc<dyn Fn() + Send + Sync>>,
        surface_control: std::sync::Arc<shared::surface::SurfaceControl>,
        report_surface_loss: std::sync::Arc<
            dyn Fn(shared::surface::PublicSurfaceGeneration, shared::surface::SurfaceLossReason)
                + Send
                + Sync,
        >,
        report_surface_installed: std::sync::Arc<
            dyn Fn(shared::surface::SurfaceCandidateRevision) + Send + Sync,
        >,
    ) -> EngineResult<Self> {
        // Published for the render thread to read rather than handed to it, so a
        // host that detaches while the GPU is still coming up is answered at once
        // instead of waiting out EGL initialization. The logical owner of the
        // attachment stays here: this slot arbitrates generations, answers
        // `has_live_surface`, and is what a context restore reads.
        if let Some(lease) = initial_surface.as_ref() {
            surface_control.publish_candidate(lease.clone());
        }

        let thread = RenderThread::spawn(
            raf_tx,
            vsync_rx,
            frame_demand_rx,
            host_id,
            graphics_platform,
            pixel_ratio,
            app_cache_dir,
            gpu_caps,
            // Resolved here rather than inside the render thread: a GPU startup
            // timeout detaches that thread without joining it, and the host's
            // startup guard unregisters this cache, so a get-or-create reached
            // after that point would leak a registry entry per failed startup.
            shared::text_texture_cache::text_cache_for_host(host_id),
            context_lost,
            wake,
            raf_demand,
            request_vsync,
            std::sync::Arc::clone(&surface_control),
            report_surface_loss,
            report_surface_installed,
            render_exit,
        )?;
        // Apply the host's configured target FPS to the render thread immediately
        // so the first vsync tick already runs at the right cadence.
        let _ = thread
            .sender()
            .send(RenderCommand::FrameRate(shared::frame_rate::clamp_fps(
                target_fps.max(0) as u32,
            )));
        Ok(Self {
            host_id,
            attachment: match initial_surface {
                Some(lease) => SurfaceAttachmentSlot::from_initial(lease),
                // Not "detached after having been attached": never attached.
                // `update_surface` reaches `install_surface_lease` with
                // `binding.is_live()` false either way, so the first Surface a
                // warm-started session receives takes the same install path an
                // initial one would have.
                None => SurfaceAttachmentSlot::empty(),
            },
            surface_control,
            outstanding: None,
            thread,
        })
    }

    #[inline]
    pub(crate) fn sender(&self) -> shared::render_command_sender::CommandSender {
        self.thread.sender()
    }

    /// Whether the host currently holds a live onscreen surface. False after
    /// `on_surface_destroyed()` until the next successful `update_surface()`.
    #[inline]
    pub(crate) fn has_live_surface(&self) -> bool {
        self.attachment.has_live_surface()
    }

    #[inline]
    pub(crate) fn events(&self) -> RenderEventReceiver {
        self.thread.events()
    }

    /// F-2: pass-through accessor so `HostOpState` can adopt the
    /// render thread's `SharedTextMeasurer`.  The measurer is
    /// built at `RenderThread::spawn` time and lives for the
    /// lifetime of the render service.
    #[inline]
    /// Handed to the JavaScript runtime's op state so the text fast path can
    /// measure inline instead of crossing to the render thread. There is no
    /// such op state in an external-frame session: the producer measures text
    /// in WebContent, and what crosses is the result.
    #[cfg(feature = "embedded-v8")]
    pub(crate) fn text_measurer(&self) -> shared::text_measurer::SharedTextMeasurer {
        self.thread.text_measurer()
    }

    /// Ask the renderer to install a Surface, and return once it has been asked.
    ///
    /// Deliberately does **not** wait for the install. It used to: the recreate carried
    /// a reply channel and this blocked on it for up to 500 ms per attempt, on the
    /// session thread -- which in the embedded product is the thread that runs
    /// JavaScript. EGL is exactly what makes that wait long (33 ms on macOS, a measured
    /// 5.7-41 s on the iOS simulator while ANGLE compiles Metal shaders cold), so a
    /// resize arriving at the wrong moment stalled content for up to 1.5 s.
    ///
    /// Nothing was bought by waiting. Both outcomes already travel on the must-deliver
    /// Host control stream -- `SurfaceInstalled` for the publication that landed,
    /// `SurfaceLost` for one the platform refused -- and it is
    /// [`Self::confirm_install`] that commits the attachment either way.
    ///
    /// `Ok` therefore means *queued*, not *installed*. The retry that remains is for the
    /// enqueue alone: `dispatch` is bounded-blocking at 8 ms, and a recreate the queue
    /// refuses is one nothing else will ever issue, which would strand the app on a
    /// stale frame with no further Surface callback coming.
    pub(crate) fn update_surface(
        &mut self,
        lease: SurfaceLease,
        pixel_ratio: Option<PixelRatio>,
    ) -> EngineResult<()> {
        let mut attempts = 1u32;
        let mut result = self.install_surface(lease.clone(), pixel_ratio);
        // A retired Surface is not worth retrying for: the host has taken it back and
        // a later attempt would be arbitrating over something that no longer exists.
        while result.is_err() && lease.is_live() && attempts < Self::INSTALL_ATTEMPTS {
            attempts += 1;
            warn!(
                "[Host {}] update_surface attempt {} failed: {:?}",
                self.host_id,
                attempts,
                result.as_ref().err()
            );
            result = self.install_surface(lease.clone(), pixel_ratio);
        }
        result
    }

    /// Commit an install the renderer has reported.
    ///
    /// Returns whether it committed, which is what a caller uses to decide about
    /// resuming. The only commit path: nothing waits for a recreate any more, so a
    /// Surface becomes the Host's attachment here and nowhere else.
    ///
    /// Idempotent, and a report for anything but the outstanding publication is ignored
    /// rather than acted on -- a stale one must not be allowed to conclude that the
    /// Surface was destroyed.
    pub(crate) fn confirm_install(
        &mut self,
        revision: shared::surface::SurfaceCandidateRevision,
    ) -> bool {
        // Compared before it is taken, and that ordering is the point. Retries publish
        // as they go, so a report for revision 1 can arrive while revision 3 is what
        // this service is waiting for -- and taking first would discard 3 on the
        // mismatch, leaving 3's own report with nothing to commit.
        if self.outstanding != Some(revision) {
            return false;
        }
        self.outstanding = None;
        // Read back from the level rather than from a copy kept here. It answers with
        // the lease only while that publication is still the live one, so a generation
        // retired between the install and this report -- the host taking its Surface
        // back -- arrives as `None` instead of as an attachment to publish.
        let Some(lease) = self.surface_control.live_candidate_for(revision) else {
            return false;
        };
        let size = lease.size();
        if self.attachment.commit(lease).is_err() {
            return false;
        }
        info!(
            "[Host {}] Surface install confirmed: publication={revision}, {}x{}",
            self.host_id, size.0, size.1
        );
        true
    }

    fn install_surface(
        &mut self,
        lease: SurfaceLease,
        pixel_ratio: Option<PixelRatio>,
    ) -> Result<(), EngineError> {
        self.attachment
            .prepare(&lease)
            .map_err(|error| transition_error("recreate onscreen: rejected Surface", error))?;
        let surface_size = lease.size();

        // Published before the wake, never carried by it. A lease riding the
        // command would pin the host's native Surface for as long as the command
        // sat in the queue -- which, before the first frame, is however long EGL
        // initialization takes, and `RELEASED` cannot be published while any lease
        // is alive. A retirement revokes the level instead.
        let revision = self.surface_control.publish_candidate(lease);

        // Bounded-blocking through the policy-aware `dispatch` (Sync class, 8 ms)
        // rather than the legacy drop-on-full `send`: a transiently full render queue
        // must not silently drop a recreate, because nothing else would ever install
        // this Surface and the app would sit on a stale frame.
        self.sender()
            .dispatch(RenderCommand::Canvas(CanvasCmd::RecreateOnscreen {
                revision,
                pixel_ratio,
            }))
            .map_err(|error| {
                // Two different facts, and the consumer decides whether to report on the
                // code, so they must not share one. Both used to be `Cancelled`, which
                // the session excludes -- so a queue that stayed full reached nobody and
                // the app sat on a stale-sized frame with no further Surface callback
                // coming. This is the same shape as flattening every arbitration variant
                // onto `InvalidOperation`: the distinction cannot be recovered later,
                // because choosing the code is where it was destroyed.
                let code = match error {
                    // The worker is gone. `RenderExit` reports the reason this does not
                    // have, and the session is right to stay quiet about it.
                    SendError::Disconnected => ErrorCode::Cancelled,
                    // The worker is alive and did not take the request within its 8 ms
                    // bound. Nothing else will ever install this Surface, so the host
                    // has to hear about it to attach another.
                    SendError::Timeout | SendError::Overflow => ErrorCode::Timeout,
                };
                EngineError::new(code)
                    .with_msg("recreate onscreen: send failed")
                    .with_detail(error.to_string())
            })?;

        // Recorded only once the queue has accepted it, because that is exactly when
        // an answer becomes owed. `confirm_install` is the sole commit path now, so
        // this is what tells a late report from a stale one.
        self.outstanding = Some(revision);
        info!(
            "RenderService::update_surface queued: requested={}x{}, publication={revision}",
            surface_size.0, surface_size.1
        );
        Ok(())
    }

    /// Pause rendering (stop RAF ticker and frame presentation).
    pub(crate) fn pause(&mut self) {
        // Bounded-blocking: dropping Pause/Resume on a full render queue
        // desynchronizes lifecycle state and can leave the app frozen.
        let _ = self.sender().send_blocking_bounded(RenderCommand::Pause);
    }

    /// Record surface loss and clear any stale surface handle.
    pub(crate) fn on_surface_destroyed(&mut self, generation: SurfaceGeneration) {
        // Only the exact current generation may cross this bridge. A delayed
        // destroy for an older attachment cannot invalidate a newer Surface.
        if !self.attachment.detach(generation) {
            return;
        }
        // Deliberately override SurfaceDestroyed's drop-on-full lifecycle
        // policy so render-side state converges promptly. Presentation safety
        // does not depend on delivery: the retired generation token is the
        // queue-independent barrier checked at every present boundary.
        let _ = self
            .sender()
            .send_blocking_bounded(RenderCommand::SurfaceDestroyed { generation });
    }

    /// Resume rendering (restart RAF ticker and frame presentation).
    pub(crate) fn resume(&mut self) {
        let _ = self.sender().send_blocking_bounded(RenderCommand::Resume);
    }

    /// Re-signal the current live surface to the render thread.
    ///
    /// This is only valid if the session still retains a live `SurfaceRef`.
    /// After `on_surface_destroyed()`, the handle is cleared and callers must
    /// wait for a fresh `update_surface()` instead of reusing a stale surface.
    /// See [`surface_for_restore`].
    #[cfg(feature = "embedded-v8")]
    pub(crate) fn restore_surface(&mut self) -> EngineResult<()> {
        self.update_surface(surface_for_restore(self.attachment.live_lease())?, None)
    }

    pub(crate) fn shutdown(&mut self) {
        self.thread.shutdown();
    }

    pub(crate) fn shutdown_detached(&mut self) {
        self.thread.shutdown_detached();
    }
}

impl Drop for RenderService {
    fn drop(&mut self) {
        // `Host::drop` performs the normal joined shutdown first. This fallback
        // only has work on partial construction, `?` returns, or unwinding.
        self.shutdown_detached();
    }
}

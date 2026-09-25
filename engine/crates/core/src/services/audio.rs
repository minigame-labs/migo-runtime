#[cfg(feature = "host-audio")]
use audio::AudioThread;

#[cfg(feature = "host-audio")]
use shared::audio_channel::{AudioCommandReceiver, AudioCommandSender};
// The restart barrier's: only an execution whose runtime restarts in place has
// one. The external execution's generation is fixed for the session's life --
// a restart there is a new session -- so it neither sends nor awaits it.
#[cfg(all(feature = "host-audio", feature = "embedded-v8"))]
use shared::audio_channel::{AudioCleanupTicket, AudioCleanupWaitError, AudioCommandSendError};
#[cfg(feature = "host-audio")]
use shared::audio_resources::AudioResourceRegistry;
#[cfg(feature = "host-audio")]
use shared::channel::ThreadWakeup;
#[cfg(feature = "host-audio")]
use shared::error::{EngineError, EngineResult, ErrorCode};
#[cfg(feature = "host-audio")]
use shared::op_state::{AudioHostStartSignal, AudioSender, HostTx};
#[cfg(feature = "host-audio")]
use shared::protocol::audio_cmd::AudioCmd;
#[cfg(feature = "host-audio")]
use std::sync::Arc;
#[cfg(all(feature = "host-audio", feature = "embedded-v8"))]
use std::time::Duration;
#[cfg(feature = "host-audio")]
use tracing::info;

/// Lazy audio service — the actual `AudioThread` is not spawned until the
/// first real audio command arrives.
///
/// Creating the `AudioThread` during `Host::new()` was one of the most
/// expensive steps (~50–100 ms on mid-range devices) because it:
///   - Initialises the `cpal` audio output (enumerates devices, opens stream)
///   - Allocates ring buffers, resampler state, etc.
///
/// Most mini-games do not use audio until well after first frame (e.g.,
/// background music starts after splash screen).  By deferring thread
/// creation until the first `AudioCmd`, we remove ~80 ms from cold-start.
///
/// ## Implementation
///
/// 1. `AudioService::new()` creates the channel, wakeup, and a policy-capturing
///    HTTP factory closure. It does not build a reqwest client/pool or thread.
/// 2. `sender()` returns an [`AudioSender`] backed by the channel.
/// 3. A successful pre-start command notifies the Host event loop, which calls
///    `check_and_start()`. It drains queued commands and, upon the first *real*
///    audio command (i.e. not PauseAll/ResumeAll/Shutdown), spawns the
///    real `AudioThread` via [`AudioThread::spawn_with_channel`] which
///    re-uses the **same channel** — no forwarding task needed.
#[cfg(feature = "host-audio")]
pub(crate) struct AudioService {
    /// Sender end of the channel.  Ops write here immediately.
    tx: AudioCommandSender,
    /// Wakeup handle shared with [`AudioSender`] instances.
    wakeup: ThreadWakeup,
    /// Receiver end — held until the thread is started, then handed off.
    rx: Option<AudioCommandReceiver>,
    /// Commands dequeued before the thread starts (replayed on start).
    pending: Vec<AudioCmd>,
    /// Handle to the spawned `AudioThread` (Some once started).
    thread: Option<AudioThread>,
    /// Host command sender — needed to construct `AudioThread`.
    host_tx: HostTx,
    /// Tracks whether the app is currently paused (OnHide received).
    /// If the thread is lazily started while paused, we immediately
    /// send `PauseAll` to avoid audio playing in the background.
    is_paused: bool,
    /// Coalescing lazy-audio host-start signal. Handed to every host
    /// `AudioSender` so a pre-start command nudges the host loop to call
    /// `check_and_start()`, and disabled (`mark_started`) once the thread runs.
    /// Replaces the lazy-audio poll that the deleted 3-second heartbeat did.
    host_start: Arc<AudioHostStartSignal>,
    /// Host-lifetime owner of JS `AudioBuffer` backing accounting. Every
    /// `AudioSender` cloned into an isolate points at this same registry.
    resources: AudioResourceRegistry,
    /// Per-host factory. The audio thread invokes it only on the first remote
    /// cache miss, then keeps the resulting reqwest pool for its lifetime.
    http_client_factory: audio::streaming::StreamingHttpClientFactory,
}

/// What the audio thread starts with: everything buffered before it existed, in
/// arrival order, with `PauseAll` last when the app is backgrounded.
///
/// **The buffered commands go to the thread rather than back into the channel.**
/// Re-injecting them would deadlock now that the transport is bounded: at the
/// moment of handover nothing is draining the queue — the receiver is still held
/// by the service — so a full queue would park the caller forever. It also
/// removes an ordering argument that was never sound, since the game thread can
/// enqueue while the handover runs and a re-injected command would then land
/// behind a newer one.
///
/// Extracted because the handover itself needs an audio device and a host test
/// cannot provide one, while this can be observed exactly.
/// The client streamed audio (`InnerAudioContext.src = "https://..."`) is
/// fetched with: the policy-checked one every outbound request in this engine
/// uses -- allow list, HTTPS enforcement, an SSRF-checking resolver and a
/// redirect gate (`migo_services::network::client`). Built lazily and once,
/// because a session that never streams audio should not pay for a TLS client.
#[cfg(feature = "host-audio")]
pub(crate) fn streaming_http_client_factory(
    network_policy: shared::op_state::NetworkPolicy,
) -> audio::streaming::StreamingHttpClientFactory {
    Arc::new(move || {
        migo_services::network::client::create_policy_http_client(
            "migo",
            true,
            &network_policy,
            migo_services::network::gate::GateKind::AudioStreamRedirect,
            "audio",
        )
        .map_err(|error| {
            EngineError::from_detail(
                ErrorCode::IoError,
                format!("failed to build audio HTTP client: {error}"),
            )
        })
    })
}

#[cfg(feature = "host-audio")]
fn take_startup_backlog(pending: &mut Vec<AudioCmd>, is_paused: bool) -> Vec<AudioCmd> {
    let mut backlog: Vec<AudioCmd> = pending.drain(..).collect();
    if is_paused {
        // Last, so it wins over anything that asked for playback.
        backlog.push(AudioCmd::PauseAll);
    }
    backlog
}

#[cfg(feature = "host-audio")]
fn complete_prestart_release_all_contexts(rx: &AudioCommandReceiver, pending: &mut Vec<AudioCmd>) {
    pending.clear();
    rx.discard_prestart_commands();
    rx.complete_release_all_contexts();
}

#[cfg(all(feature = "host-audio", feature = "embedded-v8"))]
fn cleanup_send_error(error: AudioCommandSendError) -> EngineError {
    match error {
        AudioCommandSendError::Full(_) | AudioCommandSendError::ByteLimit(_) => {
            EngineError::from_detail(
                ErrorCode::InputSaturated,
                "audio restart cleanup lane is saturated",
            )
        }
        AudioCommandSendError::Disconnected(_) => EngineError::from_detail(
            ErrorCode::Disconnected,
            "audio restart cleanup lane is disconnected",
        ),
    }
}

#[cfg(all(feature = "host-audio", feature = "embedded-v8"))]
async fn await_cleanup_ticket(ticket: AudioCleanupTicket, timeout: Duration) -> EngineResult<()> {
    match tokio::time::timeout(timeout, ticket.wait()).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(AudioCleanupWaitError::Disconnected)) => Err(EngineError::from_detail(
            ErrorCode::Disconnected,
            "audio thread disconnected before restart cleanup completed",
        )),
        Err(_) => Err(EngineError::from_detail(
            ErrorCode::Timeout,
            "audio restart cleanup timed out",
        )),
    }
}

/// The platform's audio service, where the engine is the platform.
///
/// On iOS the process's `AVAudioSession` is the engine's -- nothing else on
/// that lane configures it (`audio::apple_session`) -- so `setInnerAudioOption`
/// acts on it directly. Elsewhere the platform's own services answer, or
/// nothing does.
#[cfg(feature = "host-audio")]
pub(crate) fn platform_audio_service() -> Option<Arc<dyn shared::services::AudioPlatformService>> {
    #[cfg(target_os = "ios")]
    {
        Some(audio::apple_session::platform_service())
    }
    #[cfg(not(target_os = "ios"))]
    {
        None
    }
}

#[cfg(feature = "host-audio")]
impl AudioService {
    /// Create a lazy audio service. **No thread or HTTP client is created.**
    /// `http_client_factory` builds the client streamed audio fetches with,
    /// lazily and once: the caller's, because the policy-checked client is the
    /// network layer's and each execution has its own (see
    /// [`streaming_http_client_factory`]).
    pub(crate) fn new(
        host_tx: HostTx,
        http_client_factory: audio::streaming::StreamingHttpClientFactory,
    ) -> Self {
        let (tx, rx) = shared::audio_channel::channel();
        let wakeup = ThreadWakeup::new();
        Self {
            tx,
            wakeup,
            rx: Some(rx),
            pending: Vec::new(),
            thread: None,
            host_tx,
            is_paused: false,
            host_start: AudioHostStartSignal::new(),
            resources: AudioResourceRegistry::new(),
            http_client_factory,
        }
    }

    /// Return an [`AudioSender`] that auto-wakes the audio thread on each
    /// send.  Safe to call before the thread is started — commands queue up
    /// and are replayed once the thread spawns.  Carries the lazy-audio
    /// host-start signal so a pre-start send nudges the host loop.
    #[inline]
    pub(crate) fn sender(&self) -> AudioSender {
        AudioSender::hosted(
            self.tx.clone(),
            self.wakeup.clone(),
            self.host_start.clone(),
            self.resources.clone(),
        )
    }

    /// The lazy-audio host-start signal, for the host event loop to select on.
    #[inline]
    pub(crate) fn start_signal(&self) -> Arc<AudioHostStartSignal> {
        self.host_start.clone()
    }

    /// Called from the Host event loop when the coalescing start signal fires.
    ///
    /// Drains queued commands from the channel and starts the audio thread
    /// on the first "real" command (anything except PauseAll/ResumeAll/
    /// Shutdown).  Cost when thread is already running: one branch.
    pub(crate) fn check_and_start(&mut self) -> EngineResult<()> {
        if self.thread.is_some() {
            return Ok(());
        }

        if let Some(ref mut rx) = self.rx {
            while let Ok(cmd) = rx.try_recv() {
                if matches!(cmd, AudioCmd::ReleaseAllContexts) {
                    complete_prestart_release_all_contexts(rx, &mut self.pending);
                    continue;
                }
                let is_lifecycle = matches!(
                    cmd,
                    AudioCmd::PauseAll | AudioCmd::ResumeAll | AudioCmd::Shutdown
                );
                self.pending.push(cmd);
                if !is_lifecycle {
                    // A real audio command arrived — start the thread now.
                    return self.start_thread();
                }
            }
        }
        Ok(())
    }

    /// Force-start the thread even if no real command has arrived.
    ///
    /// Used by `on_restart()` which needs a live thread for ResumeAll.
    #[allow(dead_code)]
    pub(crate) fn ensure_started(&mut self) -> EngineResult<()> {
        if self.thread.is_some() {
            return Ok(());
        }
        self.start_thread()
    }

    fn start_thread(&mut self) -> EngineResult<()> {
        info!(
            "AudioService: lazily starting audio thread ({} buffered cmds)",
            self.pending.len()
        );

        let startup_backlog = take_startup_backlog(&mut self.pending, self.is_paused);

        // Hand the receiver + wakeup directly to the thread — the thread
        // reads from the same channel that ops write to.  No forwarding
        // task needed.
        let rx = self
            .rx
            .take()
            .expect("[BUG] AudioService::start_thread called twice");
        let thread = AudioThread::spawn_with_channel(
            self.tx.clone(),
            rx,
            startup_backlog,
            self.wakeup.clone(),
            self.host_tx.clone(),
            self.http_client_factory.clone(),
        )?;

        self.thread = Some(thread);
        // The real audio thread is installed: disable the host-start signal so
        // later sends stop nudging the host loop (release ordering, after the
        // thread is stored).
        self.host_start.mark_started();
        Ok(())
    }

    /// Pause all audio processing (app going to background).
    pub(crate) fn pause(&mut self) {
        self.is_paused = true;
        if self.thread.is_some() {
            let _ = self.tx.try_send(AudioCmd::PauseAll);
            self.wakeup.notify();
        }
        // If thread not started, no-op — nothing to pause.
        // `is_paused` is tracked so that if the thread starts later,
        // it immediately receives PauseAll.
    }

    /// Resume all audio processing (app returning to foreground).
    pub(crate) fn resume(&mut self) {
        self.is_paused = false;
        if self.thread.is_some() {
            let _ = self.tx.try_send(AudioCmd::ResumeAll);
            self.wakeup.notify();
        }
    }

    /// Must-deliver restart barrier. A running audio thread acknowledges only
    /// after discarding older WebAudio commands and releasing every WebAudio
    /// context. Before lazy start there can be no native contexts, so the
    /// service drops the old backlog/channel itself and completes immediately.
    #[cfg(feature = "embedded-v8")]
    pub(crate) async fn release_all_contexts(&mut self) -> EngineResult<()> {
        const CLEANUP_TIMEOUT: Duration = Duration::from_secs(3);

        let ticket = self
            .sender()
            .release_all_contexts()
            .map_err(cleanup_send_error)?;
        if self.thread.is_none() {
            let rx = self.rx.as_ref().ok_or_else(|| {
                EngineError::from_detail(
                    ErrorCode::Disconnected,
                    "audio receiver is unavailable before startup cleanup",
                )
            })?;
            complete_prestart_release_all_contexts(rx, &mut self.pending);
        }

        self.wakeup.notify();
        await_cleanup_ticket(ticket, CLEANUP_TIMEOUT).await
    }

    /// End the restart fence only after the old isolate and all of its senders
    /// have been retired. A failed/timed-out barrier deliberately cannot reopen.
    #[cfg(feature = "embedded-v8")]
    pub(crate) fn finish_release_all_contexts(&mut self) {
        self.tx.finish_release_all_contexts();
    }

    /// Fence JS backing admission before the native cleanup barrier begins.
    #[cfg(feature = "embedded-v8")]
    pub(crate) fn begin_retire(&self, runtime_generation: i64) {
        self.resources.begin_retire(runtime_generation);
    }

    /// Return JS backing permits only after the owning isolate is destroyed.
    #[cfg(feature = "embedded-v8")]
    pub(crate) fn finish_runtime_drop(&self, runtime_generation: i64) {
        self.resources.finish_runtime_drop(runtime_generation);
    }

    pub(crate) fn shutdown(&mut self) {
        if let Some(ref mut thread) = self.thread {
            thread.shutdown();
        }
    }
}

#[cfg(not(feature = "host-audio"))]
pub(crate) struct AudioService {
    /// Permanently disconnected: this profile has no audio thread to send to.
    ///
    /// It used to hold a live receiver it never read, so a send queued for the
    /// life of the session — harmless only because the audio ops are compiled out
    /// of this profile and nothing could reach it. Behind a bounded transport that
    /// same shape parks the first producer to fill it, so there is no receiver at
    /// all now and a send fails at once. See `shared::audio_channel::disconnected`.
    // Read only through the accessors the embedded execution uses. An
    // external-frame session has no op state to hand them to and no host loop
    // waiting on the start signal, so in that build these are the shape of a
    // service that is present and idle -- which is what it is, until the
    // control channel gives the producer a way to ask for a sound.
    #[cfg(feature = "embedded-v8")]
    tx: shared::audio_channel::AudioCommandSender,
    #[cfg(feature = "embedded-v8")]
    wakeup: shared::channel::ThreadWakeup,
    #[cfg(feature = "embedded-v8")]
    start_signal: std::sync::Arc<shared::op_state::AudioHostStartSignal>,
}

#[cfg(not(feature = "host-audio"))]
impl AudioService {
    pub(crate) fn new(
        _host_tx: shared::op_state::HostTx,
        _network_policy: shared::op_state::NetworkPolicy,
    ) -> Self {
        #[cfg(feature = "embedded-v8")]
        let tx = shared::audio_channel::disconnected();
        #[cfg(feature = "embedded-v8")]
        let start_signal = shared::op_state::AudioHostStartSignal::new();
        #[cfg(feature = "embedded-v8")]
        start_signal.mark_started();
        Self {
            #[cfg(feature = "embedded-v8")]
            tx,
            #[cfg(feature = "embedded-v8")]
            wakeup: shared::channel::ThreadWakeup::new(),
            #[cfg(feature = "embedded-v8")]
            start_signal,
        }
    }

    #[inline]
    /// Handed to the JavaScript runtime's op state; the audio ops write to it. An external-frame session has no op state and no host
    /// loop of that shape, so nothing here reaches it.
    #[cfg(feature = "embedded-v8")]
    pub(crate) fn sender(&self) -> shared::op_state::AudioSender {
        shared::op_state::AudioSender::new(self.tx.clone(), self.wakeup.clone())
    }

    #[inline]
    /// Selected on by the embedded host loop, which starts the audio thread lazily on the first audio op. An external-frame session has no op state and no host
    /// loop of that shape, so nothing here reaches it.
    #[cfg(feature = "embedded-v8")]
    pub(crate) fn start_signal(&self) -> std::sync::Arc<shared::op_state::AudioHostStartSignal> {
        self.start_signal.clone()
    }

    #[inline]
    /// Called by the embedded host loop when `start_signal` fires. An external-frame session has no op state and no host
    /// loop of that shape, so nothing here reaches it.
    #[cfg(feature = "embedded-v8")]
    pub(crate) fn check_and_start(&mut self) -> shared::error::EngineResult<()> {
        Ok(())
    }

    #[inline]
    pub(crate) fn pause(&mut self) {}

    #[inline]
    pub(crate) fn resume(&mut self) {}

    #[inline]
    /// Part of the embedded execution's runtime-restart and teardown
    /// sequence: an external-frame session has no JavaScript runtime to
    /// retire, and its audio contexts are not owned by one.
    #[cfg(feature = "embedded-v8")]
    pub(crate) async fn release_all_contexts(&mut self) -> shared::error::EngineResult<()> {
        Ok(())
    }

    #[inline]
    /// Part of the embedded execution's runtime-restart and teardown
    /// sequence: an external-frame session has no JavaScript runtime to
    /// retire, and its audio contexts are not owned by one.
    #[cfg(feature = "embedded-v8")]
    pub(crate) fn finish_release_all_contexts(&mut self) {}

    #[inline]
    /// Part of the embedded execution's runtime-restart and teardown
    /// sequence: an external-frame session has no JavaScript runtime to
    /// retire, and its audio contexts are not owned by one.
    #[cfg(feature = "embedded-v8")]
    pub(crate) fn begin_retire(&self, _runtime_generation: i64) {}

    #[inline]
    /// Part of the embedded execution's runtime-restart and teardown
    /// sequence: an external-frame session has no JavaScript runtime to
    /// retire, and its audio contexts are not owned by one.
    #[cfg(feature = "embedded-v8")]
    pub(crate) fn finish_runtime_drop(&self, _runtime_generation: i64) {}

    #[inline]
    pub(crate) fn shutdown(&mut self) {}
}

#[cfg(all(test, feature = "host-audio"))]
mod tests {
    use super::*;

    fn create_context(ctx_id: u32) -> AudioCmd {
        AudioCmd::CreateContext {
            ctx_id,
            sample_rate: None,
        }
    }

    fn label(cmd: &AudioCmd) -> String {
        match cmd {
            AudioCmd::CreateContext { ctx_id, .. } => format!("create({ctx_id})"),
            AudioCmd::PauseAll => "pause".to_string(),
            _ => "other".to_string(),
        }
    }

    /// Commands accepted before the thread existed must reach it, in order. A
    /// handover that dropped them would lose a `CreateContext` and every later
    /// command addressing that id would fail — the failure this protocol has no
    /// error path for.
    #[test]
    fn the_backlog_carries_every_buffered_command_in_arrival_order() {
        let mut pending = vec![create_context(1), create_context(2)];

        let backlog = take_startup_backlog(&mut pending, false);

        assert_eq!(
            backlog.iter().map(label).collect::<Vec<_>>(),
            vec!["create(1)", "create(2)"]
        );
        assert!(
            pending.is_empty(),
            "the buffer kept a copy, so the thread will see it twice"
        );
    }

    /// A backgrounded app must not start playing what it buffered, so the pause
    /// goes last: ahead of the buffered commands it would be undone by them.
    #[test]
    fn a_backgrounded_app_hands_over_its_pause_last() {
        let mut pending = vec![create_context(1)];

        let backlog = take_startup_backlog(&mut pending, true);

        assert_eq!(
            backlog.iter().map(label).collect::<Vec<_>>(),
            vec!["create(1)", "pause"]
        );
    }

    #[test]
    fn a_foreground_app_hands_over_no_pause() {
        let mut pending = vec![create_context(1)];

        let backlog = take_startup_backlog(&mut pending, false);

        assert_eq!(
            backlog.iter().map(label).collect::<Vec<_>>(),
            vec!["create(1)"]
        );
    }

    #[test]
    fn prestart_release_barrier_discards_pending_and_full_channel_before_ack() {
        let (tx, rx) = shared::audio_channel::channel();
        for ctx_id in 100..100 + shared::audio_channel::AUDIO_COMMAND_CAPACITY as u32 {
            tx.try_send(create_context(ctx_id))
                .expect("fixture fills the ordinary data queue");
        }
        let ticket = tx
            .request_release_all_contexts()
            .expect("cleanup bypasses full data");
        let (resp, mut response) = tokio::sync::oneshot::channel();
        let mut pending = vec![AudioCmd::CloseContext { ctx_id: 9, resp }];

        complete_prestart_release_all_contexts(&rx, &mut pending);

        assert!(pending.is_empty());
        assert!(rx.is_empty());
        assert!(matches!(
            response.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Closed)
        ));
        assert!(ticket.is_complete(), "pre-start cleanup completes directly");
    }

    #[cfg(feature = "embedded-v8")]
    #[tokio::test]
    async fn cleanup_timeout_is_fail_closed_and_keeps_late_data_fenced() {
        let (tx, _rx) = shared::audio_channel::channel();
        let ticket = tx.request_release_all_contexts().unwrap();

        let error = await_cleanup_ticket(ticket, Duration::ZERO)
            .await
            .expect_err("an unacknowledged barrier must time out");

        assert_eq!(error.code, ErrorCode::Timeout);
        assert!(matches!(
            tx.try_send(create_context(77)),
            Err(AudioCommandSendError::Full(AudioCmd::CreateContext {
                ctx_id: 77,
                ..
            }))
        ));
    }

    fn service() -> AudioService {
        let (host_tx, _critical_host_tx, _host_rx) = shared::host_channel::channel(1);
        AudioService::new(
            host_tx,
            streaming_http_client_factory(shared::op_state::NetworkPolicy::default()),
        )
    }

    fn one_frame() -> shared::audio_resources::AudioBufferFormat {
        shared::audio_resources::AudioBufferFormat {
            channels: 1,
            frames: 1,
            sample_rate: 48_000,
        }
    }

    #[test]
    fn every_service_sender_shares_the_host_resource_registry() {
        let service = service();
        let first = service.sender();
        let second = service.sender();
        let lease = first
            .resources()
            .expect("host sender carries resources")
            .reserve_backing(701, one_frame())
            .unwrap();

        assert!(second.resources().unwrap().release_buffer(lease.key()));
    }

    #[cfg(feature = "embedded-v8")]
    #[test]
    fn service_retire_and_drop_forward_to_the_shared_registry() {
        let service = service();
        let sender = service.sender();
        let lease = sender
            .resources()
            .unwrap()
            .reserve_backing(702, one_frame())
            .unwrap();

        service.begin_retire(702);
        assert_eq!(
            sender
                .resources()
                .unwrap()
                .reserve_backing(702, one_frame())
                .unwrap_err()
                .code,
            ErrorCode::InvalidOperation
        );
        service.finish_runtime_drop(702);
        assert!(!sender.resources().unwrap().release_buffer(lease.key()));
    }
}

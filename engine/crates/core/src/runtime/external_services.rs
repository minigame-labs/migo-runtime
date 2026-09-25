//! The service stream's host half.
//!
//! Content in another process asks this host to read a file, write a save, load
//! an image, start a sound. Those requests arrive as `MUS1` messages (see
//! `frame_wire::service` and *The service stream* in
//! `contracts/frame-wire/wire-v1.md`), on whichever thread the transport runs
//! on, and are answered by the same service code the embedded runtime's ops call
//! (`migo_services`).
//!
//! # Three places, one order
//!
//! - **Admission** happens on the transport's thread: the message is validated
//!   in full, its sequence is checked against the last one admitted, and its
//!   records are queued for the session thread -- all under one lock, so the
//!   queue order is the sequence order.
//! - **Dispatch** happens on the session thread, in queue order: a request is
//!   started, a command is applied, a synchronous call is given its turn (and
//!   runs on the thread that made it; see [`ServiceHost::call_sync`]). Started in order
//!   is the property the embedded runtime has -- its ops run on the JavaScript
//!   thread in call order and send to their subsystems' channels as they go --
//!   and it is the property content depends on: "create the node, then stop it".
//! - **Answers** are written to an outbox the transport drains, and the
//!   transport is woken to drain it.
//!
//! A synchronous call waits for the service message the producer sent before it
//! to be admitted, then joins the same queue, so it is dispatched after every
//! command content made before it -- a `readFileSync` never overtakes the
//! `writeFile` in front of it.
//!
//! # Reordering happens here, not in the producer
//!
//! A message larger than the socket's ceiling travels as a scheme request, and
//! the two paths can reorder. The producer does not wait for a scheme message
//! to land before sending the next one: it cannot, because a synchronous call
//! made meanwhile blocks the Worker, and a producer holding messages behind a
//! request only the Worker's event loop can settle would hold them past the
//! call that needs them -- a deadlock bounded only by the call's timeout. So
//! messages leave in sequence order, and the host holds one that arrives ahead
//! of its predecessor, within [`MAX_HELD_MESSAGES`] and [`MAX_HELD_BYTES`].

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use futures::future::BoxFuture;
use parking_lot::{Condvar, Mutex, RwLock};
use tracing::{info, warn};

use frame_wire::service::{
    MAX_INLINE_REPLY_BYTES, OUTCOME_ERROR, OUTCOME_OK, OwnedServiceRecord, ReplyError,
    SERVICE_DOWN_HEADER_BYTES, Sequencing, ServiceDownRecord, ServiceError as WireError,
    ServiceRecord, ServiceSequencer, down_envelope, encode_down_record, read_service_envelope,
    read_service_message,
};
use frame_wire::sync::SyncError;
use frame_wire::value::{OwnedValue, ValueWriter, read_values};
use migo_io::scheduler::IoScheduler;
use migo_services::ServiceError;
use migo_services::content::MountedContent;
use migo_services::image::cache::{ImageCache, SharedImageCache};
use shared::error::{EngineError, EngineResult, ErrorCode};

use super::service_args::{
    boolean, bytes, exactly, f64_of, i32_of, not_a, string, strings, u32_of,
};
use super::service_audio::{self, AudioBinding, LocalSources};
use super::service_network::{self, NetworkBinding, UploadSources};
use super::service_ops::id;

/// The most messages held ahead of a predecessor that has not arrived.
///
/// Everything the producer sends while one scheme request is in transit: a
/// scheme request takes about a millisecond, and a producer sending hundreds of
/// messages in that time is not one this host keeps up with anyway.
pub(crate) const MAX_HELD_MESSAGES: usize = 256;

/// The most bytes held ahead of a predecessor that has not arrived.
pub(crate) const MAX_HELD_BYTES: usize = frame_wire::service::MAX_SERVICE_MESSAGE_BYTES;

/// Messages admitted and not yet dispatched.
///
/// A bound, so a producer cannot make the host hold an unbounded backlog. A
/// full queue blocks the transport that is admitting, which pushes back on the
/// socket -- content's sends buffer in WebContent rather than here.
const WORK_CAPACITY: usize = 1024;

/// Answers waiting for the producer to take them, inline and parked together.
///
/// Every answer is owed exactly once, so they cannot be dropped the way a stale
/// tick can. What can be bounded is a producer that stopped reading: past this,
/// a new answer is replaced by an error saying so, which is small. A game
/// reading one large file at a time never approaches it.
pub(crate) const MAX_OUTBOX_BYTES: usize = 256 * 1024 * 1024;

/// Work for the session thread, in admission order.
pub(crate) enum ServiceWork {
    /// The records of one admitted message.
    Records(Vec<OwnedServiceRecord>),
    /// A synchronous call's turn: everything admitted before it has been
    /// started. The caller runs the op itself when this is sent -- see
    /// [`ServiceHost::call_sync`].
    Sync {
        turn: std::sync::mpsc::SyncSender<()>,
    },
}

/// Where the transport is told there is something to send.
///
/// Shared by the frame clock and the service outbox: both queue records the
/// transport did not cause, and both wake the same drain.
#[derive(Default)]
pub(crate) struct WakerSlot(Mutex<Option<super::external::DownlinkWaker>>);

impl WakerSlot {
    /// Install or clear. Clearing returns only once no call is in progress,
    /// which is what lets a host free whatever the waker points at.
    pub(crate) fn set(&self, waker: Option<super::external::DownlinkWaker>) {
        *self.0.lock() = waker;
    }

    pub(crate) fn wake(&self) {
        if let Some(wake) = self.0.lock().as_ref() {
            wake();
        }
    }
}

// ---------------------------------------------------------------------------
// The outbox
// ---------------------------------------------------------------------------

#[derive(Default)]
struct OutboxInner {
    /// Encoded records, each at most [`MAX_INLINE_REPLY_BYTES`].
    records: VecDeque<Vec<u8>>,
    /// Encoded reply records too large to send inline, by request id.
    parked: HashMap<u32, Vec<u8>>,
    bytes: usize,
}

/// What the host owes the producer: answers and events, in the order they were
/// produced.
pub(crate) struct ServiceOutbox {
    generation: u32,
    inner: Mutex<OutboxInner>,
    waker: Arc<WakerSlot>,
}

impl ServiceOutbox {
    fn new(generation: u32, waker: Arc<WakerSlot>) -> Self {
        Self {
            generation,
            inner: Mutex::new(OutboxInner::default()),
            waker,
        }
    }

    fn push(&self, record: &ServiceDownRecord, request_id: Option<u32>) {
        let mut encoded = encode_down_record(record);
        {
            let mut inner = self.inner.lock();
            if inner.bytes.saturating_add(encoded.len()) > MAX_OUTBOX_BYTES {
                let Some(request_id) = request_id else {
                    warn!(
                        "service outbox is full ({} bytes unread); an event was dropped",
                        inner.bytes
                    );
                    return;
                };
                // Still answered -- the promise must settle -- but with a
                // reason instead of a value the producer has no room for.
                encoded = encode_down_record(&ServiceDownRecord::Reply {
                    request_id,
                    outcome: Err(ReplyError {
                        class: "Error".into(),
                        message: format!(
                            "the answer was dropped: {} bytes of earlier answers are unread",
                            inner.bytes
                        ),
                    }),
                });
            }
            inner.bytes += encoded.len();
            if encoded.len() > MAX_INLINE_REPLY_BYTES
                && let Some(request_id) = request_id
            {
                let byte_length =
                    u32::try_from(encoded.len()).expect("an answer is bounded below 4 GiB");
                let notice = encode_down_record(&ServiceDownRecord::ReplyParked {
                    request_id,
                    byte_length,
                });
                inner.bytes += notice.len();
                inner.parked.insert(request_id, encoded);
                inner.records.push_back(notice);
            } else {
                inner.records.push_back(encoded);
            }
        }
        self.waker.wake();
    }

    /// Answer a request.
    pub(crate) fn reply(&self, request_id: u32, outcome: Result<OwnedValue, ServiceError>) {
        let outcome = outcome.map_err(|error| ReplyError {
            class: error.class.to_owned(),
            message: error.message,
        });
        self.push(
            &ServiceDownRecord::Reply {
                request_id,
                outcome,
            },
            Some(request_id),
        );
    }

    /// Tell content something no request asked for: input, for now (see
    /// `host_events`).
    pub(crate) fn event(&self, event: u32, values: Vec<OwnedValue>) {
        self.push(&ServiceDownRecord::Event { event, values }, None);
    }

    fn refused(&self, code: u32, sequence: u64) {
        self.push(&ServiceDownRecord::Refused { code, sequence }, None);
    }

    /// One `MDS1` message holding as many queued records as fit, or `None` when
    /// nothing is queued.
    pub(crate) fn take_message(&self) -> Option<Vec<u8>> {
        let mut inner = self.inner.lock();
        let first = inner.records.front()?.len();
        let budget = MAX_INLINE_REPLY_BYTES.max(first);
        let mut message = Vec::with_capacity(SERVICE_DOWN_HEADER_BYTES + first);
        message.extend_from_slice(&down_envelope(self.generation));
        let mut used = 0usize;
        while let Some(next) = inner.records.front() {
            if used > 0 && used + next.len() > budget {
                break;
            }
            let record = inner.records.pop_front().expect("peeked above");
            used += record.len();
            inner.bytes -= record.len();
            message.extend_from_slice(&record);
        }
        Some(message)
    }

    /// Take a parked answer. Once: the bytes are released as they are handed
    /// over.
    pub(crate) fn take_parked(&self, generation: u32, request_id: u32) -> Option<Vec<u8>> {
        if generation != self.generation {
            return None;
        }
        let mut inner = self.inner.lock();
        let record = inner.parked.remove(&request_id)?;
        inner.bytes -= record.len();
        Some(record)
    }

    #[cfg(test)]
    fn queued_bytes(&self) -> usize {
        self.inner.lock().bytes
    }
}

// ---------------------------------------------------------------------------
// The services themselves
// ---------------------------------------------------------------------------

/// What the services read: the session's io scheduler and the content the host
/// named.
pub(crate) struct ServiceContext {
    files_dir: PathBuf,
    cache_dir: PathBuf,
    /// Set once the session's id is known, before the session is handed to the
    /// host; see `spawn_external_frame_session`.
    scheduler: OnceLock<Arc<IoScheduler>>,
    session_id: OnceLock<i32>,
    content: RwLock<Option<MountedContent>>,
    /// This session's image alias table: which texture each of content's image
    /// ids names. Loads fill it on the session thread; the frame decoder reads
    /// it on the transport's thread for `texImage2D(…, image)`.
    aliases: SharedImageCache,
    /// The session's audio, once the session thread has built it. Read on the
    /// session thread and on the synchronous endpoint's: the sender is the one
    /// every audio command goes through, in the order it is dispatched.
    audio: OnceLock<AudioBinding>,
    /// The session's network: the policy content's requests are held to, and
    /// the handles a `fetch` hands out. Read on both threads, as audio is.
    network: OnceLock<NetworkBinding>,
    /// The session's own command channel, for the two calls content makes about
    /// the session itself: `exitMiniProgram` and `restartMiniProgram`. The
    /// embedded ops send the same commands through the same channel.
    lifecycle: OnceLock<shared::host_channel::HostCommandSender>,
    /// The platform's device services -- the keyboard, vibration, the display
    /// wake lock, the game log, the battery and the network: the same object
    /// the embedded ops call, so both executions reach the same host.
    device: OnceLock<Option<Arc<dyn shared::services::DeviceServices>>>,
    /// The renderer's capabilities and when its launch began: what content's
    /// compressed-texture query waits on, within the same budget the embedded
    /// execution gives it.
    gpu: OnceLock<(Arc<shared::device::gpu_caps::GpuCaps>, Instant)>,
    /// Whether the content this session mounts must be signed, resolved from
    /// the session's options when it starts. Unbound is unverified -- which is
    /// what a session started with signing off binds anyway.
    signing: OnceLock<migo_services::content::ContentSigning>,
}

/// A device service's refusal in the words the embedded op shows: its
/// message, which is what `JsErrorBox::generic` renders there.
fn device_error(error: shared::services::ServiceError) -> ServiceError {
    ServiceError::generic(error.message)
}

impl ServiceContext {
    fn new(files_dir: PathBuf, cache_dir: PathBuf) -> Self {
        Self {
            files_dir,
            cache_dir,
            scheduler: OnceLock::new(),
            session_id: OnceLock::new(),
            content: RwLock::new(None),
            aliases: Arc::new(parking_lot::Mutex::new(ImageCache::new())),
            audio: OnceLock::new(),
            network: OnceLock::new(),
            lifecycle: OnceLock::new(),
            device: OnceLock::new(),
            gpu: OnceLock::new(),
            signing: OnceLock::new(),
        }
    }

    fn session(&self) -> i32 {
        // Set before the session is handed out; zero only in a test that never
        // started one, where no image is loaded either.
        self.session_id.get().copied().unwrap_or(0)
    }

    /// The command that uploads a loaded image into a texture, for the frame
    /// decoder: the function the embedded runtime's ops call, over this
    /// session's table.
    pub(crate) fn image_upload(
        &self,
        upload: frame_decode::ImageUpload,
    ) -> Option<shared::protocol::render_cmd::GLCmd> {
        use migo_services::image::gl;
        let session = self.session();
        match upload {
            frame_decode::ImageUpload::Full {
                canvas_id,
                target,
                level,
                internalformat,
                format,
                type_,
                image_id,
            } => gl::tex_image_2d_from_image(
                &self.aliases,
                session,
                canvas_id,
                target,
                level,
                internalformat,
                format,
                type_,
                image_id,
            ),
            frame_decode::ImageUpload::Sub {
                canvas_id,
                target,
                level,
                xoffset,
                yoffset,
                format,
                type_,
                image_id,
            } => gl::tex_sub_image_2d_from_image(
                &self.aliases,
                session,
                canvas_id,
                target,
                level,
                xoffset,
                yoffset,
                format,
                type_,
                image_id,
            ),
        }
    }

    /// Release this session's claims on the decoded-bytes cache.
    ///
    /// The cache is process-wide and outlives the session, so a pin left here
    /// would keep those bytes un-evictable for the life of the process. The
    /// textures the table names need no destroy: they die with the renderer's
    /// context.
    fn release_images(&self) {
        let _textures_of_a_dead_context = self.aliases.lock().drain();
    }

    /// What an image load reads, or why there is nothing to load from.
    fn image_env(
        &self,
        render: &RenderHandles,
    ) -> Result<migo_services::image::ImageEnv, ServiceError> {
        let content = self.content.read().clone();
        Ok(migo_services::image::ImageEnv {
            scheduler: self.scheduler()?,
            vfs: content.as_ref().map(|content| Arc::clone(&content.vfs)),
            mount_table: content
                .as_ref()
                .map(|content| Arc::clone(&content.mount_table)),
            game_cache_dir: content.as_ref().map(|content| {
                content
                    .game_paths
                    .cache_dir()
                    .to_string_lossy()
                    .into_owned()
            }),
            gpu_caps: Arc::clone(&render.gpu_caps),
            // No hardware-buffer decode exists on this lane (it is Android's),
            // so CPU-backed pixels are always what a decode produces.
            cpu_backing_required: Arc::new(AtomicBool::new(true)),
            canvas: render.canvas.clone(),
            aliases: Arc::clone(&self.aliases),
            session: self.session(),
        })
    }

    pub(crate) fn bind_session(&self, session_id: i32) {
        let _ = self.session_id.set(session_id);
        let _ = self.scheduler.set(Arc::new(IoScheduler::new(session_id)));
    }

    /// Give the services the session's audio: the sender its audio service
    /// hands out, and the runtime generation that scopes `AudioBuffer` ids.
    /// Once, on the session thread, before the first service work is
    /// dispatched.
    pub(crate) fn bind_audio(
        &self,
        sender: shared::op_state::AudioSender,
        runtime_generation: i64,
        network_policy: shared::op_state::NetworkPolicy,
    ) {
        let _ = self.audio.set(AudioBinding {
            sender,
            runtime_generation,
            platform: crate::services::audio::platform_audio_service(),
            network_policy,
        });
    }

    /// Give the services this session's network policy: what content's
    /// requests are held to, and what the clients it builds are configured
    /// from. Once, before the first service work is dispatched.
    pub(crate) fn bind_network(
        &self,
        policy: shared::op_state::NetworkPolicy,
        backgrounded: Arc<AtomicBool>,
        runtime: tokio::runtime::Handle,
    ) {
        let _ = self
            .network
            .set(NetworkBinding::new(policy, backgrounded, runtime));
    }

    /// What an `http(s)://` image source is fetched with, or why there is
    /// nothing to fetch it with: the same policy and client `fetch()` uses.
    fn image_fetch(
        &self,
    ) -> Result<
        (
            shared::op_state::NetworkPolicy,
            migo_services::network::client::PolicyHttpClient,
        ),
        EngineError,
    > {
        self.network()
            .and_then(|network| network.image_client())
            .map_err(|error| {
                EngineError::new(ErrorCode::Unsupported)
                    .with_msg("image fetch unavailable")
                    .with_detail(error.message)
            })
    }

    /// Where an upload reads from: the mounted package's sandbox.
    fn upload_sources(&self) -> UploadSources {
        let content = self.content.read();
        UploadSources {
            vfs: content.as_ref().map(|content| Arc::clone(&content.vfs)),
            mount_table: content
                .as_ref()
                .map(|content| Arc::clone(&content.mount_table)),
        }
    }

    /// Give the services the session's own command channel. Once, on the
    /// session thread, before the first service work is dispatched.
    pub(crate) fn bind_lifecycle(&self, host_tx: shared::host_channel::HostCommandSender) {
        let _ = self.lifecycle.set(host_tx);
    }

    /// How this session's content is verified. Once, before content is loaded.
    pub(crate) fn bind_signing(&self, signing: migo_services::content::ContentSigning) {
        let _ = self.signing.set(signing);
    }

    /// Give the services the platform's device services, or say there are
    /// none. Once, on the session thread, before the first service work is
    /// dispatched.
    pub(crate) fn bind_device(&self, device: Option<Arc<dyn shared::services::DeviceServices>>) {
        let _ = self.device.set(device);
    }

    /// Give the services the renderer's capabilities. Once, on the session
    /// thread, before the session is ready.
    pub(crate) fn bind_gpu(&self, caps: Arc<shared::device::gpu_caps::GpuCaps>, launched: Instant) {
        let _ = self.gpu.set((caps, launched));
    }

    /// `op_webgl_query_compressed_caps`: bit 0 ETC2/EAC, bit 1 ASTC LDR -- the
    /// embedded op's bits (`runtime-v8`'s `error_state.rs`). Waits for the
    /// renderer to publish them, as the embedded execution does before it runs
    /// content; a renderer that failed or never answered has none, which is
    /// the embedded op's answer before caps are set.
    fn compressed_texture_caps(&self) -> u32 {
        let Some((caps, launched)) = self.gpu.get() else {
            return 0;
        };
        if !matches!(
            caps.wait_ready_until(*launched, super::shell::GPU_INIT_TIMEOUT),
            shared::device::gpu_caps::GpuCapsReadyState::Ready
        ) {
            return 0;
        }
        let snapshot = caps.snapshot();
        u32::from(snapshot.etc2) | (u32::from(snapshot.astc) << 1)
    }

    /// One device service, or the embedded op's own refusal when the platform
    /// has none -- the words it raises when `device_services` or the service
    /// is absent.
    fn device<T: ?Sized>(
        &self,
        what: &str,
        pick: impl FnOnce(&dyn shared::services::DeviceServices) -> Option<Arc<T>>,
    ) -> Result<Arc<T>, ServiceError> {
        self.device
            .get()
            .and_then(Option::as_deref)
            .and_then(pick)
            .ok_or_else(|| ServiceError::generic(format!("{what}:fail not supported")))
    }

    fn keyboard(
        &self,
        what: &str,
    ) -> Result<Arc<dyn shared::services::KeyboardService>, ServiceError> {
        self.device(what, |device| device.keyboard())
    }

    fn lifecycle(&self) -> Result<&shared::host_channel::HostCommandSender, ServiceError> {
        self.lifecycle
            .get()
            .ok_or_else(|| ServiceError::generic("the session's command channel is not bound"))
    }

    fn network(&self) -> Result<&NetworkBinding, ServiceError> {
        self.network
            .get()
            .ok_or_else(|| ServiceError::generic("the session's network is not started"))
    }

    fn audio(&self) -> Result<&AudioBinding, ServiceError> {
        self.audio
            .get()
            .ok_or_else(|| migo_services::audio::audio_error("the session's audio is not started"))
    }

    /// Where a call that reads a file the game shipped looks: the mounted
    /// package's sandbox, or -- before content is mounted -- nowhere.
    ///
    /// A packaged sound and a custom font are the same question, so they ask it
    /// once.
    pub(crate) fn local_sources(&self) -> LocalSources {
        let content = self.content.read();
        LocalSources {
            code_dir: content
                .as_ref()
                .map(|content| content.game_paths.code_dir().to_string_lossy().into_owned()),
            vfs: content.as_ref().map(|content| Arc::clone(&content.vfs)),
        }
    }

    /// Where an InnerAudioContext reads a packaged sound.
    fn audio_sources(&self) -> LocalSources {
        self.local_sources()
    }

    fn scheduler(&self) -> Result<Arc<IoScheduler>, ServiceError> {
        self.scheduler
            .get()
            .cloned()
            .ok_or_else(|| ServiceError::generic("the session's services are not started"))
    }

    /// What a file-system call reads: the scheduler, and the sandbox once
    /// content is mounted.
    fn fs_env(&self) -> Result<migo_services::fs::FsEnv, ServiceError> {
        Ok(super::service_fs::env(
            self.scheduler()?,
            self.content.read().as_ref(),
        ))
    }

    /// `require()`'s read, against the package root -- or, before content is
    /// mounted, against nothing, as the embedded op before a game loads.
    fn require(&self, specifier: &str, referrer_dir: &str) -> Result<OwnedValue, ServiceError> {
        let content = self.content.read().clone();
        let code_dir = content
            .as_ref()
            .map(|content| content.game_paths.code_dir().to_string_lossy().into_owned())
            .unwrap_or_default();
        migo_services::require::resolve_and_read(
            content.as_ref().map(|content| content.mount_table.as_ref()),
            &code_dir,
            specifier,
            referrer_dir,
        )
        .map(|module| {
            OwnedValue::Array(vec![
                OwnedValue::Str(module.code),
                OwnedValue::Str(module.abs_path),
                OwnedValue::Str(module.dir),
            ])
        })
    }

    /// The session's mount table, once content is mounted.
    fn mount_table(&self) -> Option<Arc<shared::vfs::MountTable>> {
        self.content
            .read()
            .as_ref()
            .map(|content| Arc::clone(&content.mount_table))
    }

    fn game_paths(&self) -> Option<Arc<shared::vfs::GamePaths>> {
        self.content
            .read()
            .as_ref()
            .map(|content| Arc::clone(&content.game_paths))
    }

    /// Mount the content the host named: `/code` from the installed package,
    /// and the game's `/user`, `/cache` and `/tmp`. Once per session.
    ///
    /// Under code signing the package is verified against `entry` first, as
    /// the embedded execution verifies before it evaluates: the producer is
    /// served from this mount, so nothing reaches it before this returns.
    pub(crate) fn load_content(&self, game_id: &str, entry: &str) -> EngineResult<PathBuf> {
        let session_id = *self.session_id.get().ok_or_else(|| {
            EngineError::new(ErrorCode::InvalidOperation).with_msg("the session is not started")
        })?;
        let scheduler = self.scheduler().map_err(|error| {
            EngineError::new(ErrorCode::InvalidOperation).with_msg(error.message)
        })?;
        let mut content = self.content.write();
        if content.is_some() {
            return Err(EngineError::new(ErrorCode::InvalidOperation)
                .with_msg("content is already loaded in this session"));
        }
        let mounted = MountedContent::mount_signed(
            &self.files_dir,
            &self.cache_dir,
            game_id,
            entry,
            session_id,
            &scheduler,
            self.signing
                .get()
                .unwrap_or(&migo_services::content::ContentSigning::Disabled),
        )?;
        let root = mounted.game_paths.code_dir().to_path_buf();
        info!(
            "[Host {session_id}] content '{game_id}' mounted at {}",
            root.display()
        );
        *content = Some(mounted);
        Ok(root)
    }

    pub(crate) fn content_module(
        &self,
        request_path: &str,
    ) -> Option<Result<Vec<u8>, migo_services::content::ModuleError>> {
        // Cloned out of the lock: a module read is file IO, and a load or a
        // service call must not wait behind it.
        let content = self.content.read().clone()?;
        Some(content.module_source(request_path))
    }

    pub(crate) fn content_root(&self) -> Option<PathBuf> {
        self.content
            .read()
            .as_ref()
            .map(|content| content.game_paths.code_dir().to_path_buf())
    }

    /// Run a synchronous op. Called on the host's synchronous endpoint once
    /// the call's turn comes (see [`ServiceHost::call_sync`]), never on the
    /// session thread: these block on file and database work.
    fn call_sync(&self, op: u32, args: Vec<OwnedValue>) -> Result<OwnedValue, ServiceError> {
        let paths = self.game_paths();
        let paths = paths.as_deref();
        match op {
            id::op_storage_get => {
                let [key] = exactly(op, args)?;
                migo_services::storage::get_sync(self.scheduler()?, paths, &string(op, 0, key)?)
                    .map(OwnedValue::Str)
            }
            id::op_storage_set => {
                let [key, value] = exactly(op, args)?;
                migo_services::storage::set_sync(
                    self.scheduler()?,
                    paths,
                    &string(op, 0, key)?,
                    &string(op, 1, value)?,
                )
                .map(|()| OwnedValue::Null)
            }
            id::op_storage_remove => {
                let [key] = exactly(op, args)?;
                migo_services::storage::remove_sync(self.scheduler()?, paths, &string(op, 0, key)?)
                    .map(|()| OwnedValue::Null)
            }
            id::op_storage_clear => {
                let [] = exactly(op, args)?;
                migo_services::storage::clear_sync(self.scheduler()?, paths)
                    .map(|()| OwnedValue::Null)
            }
            id::op_storage_info => {
                let [] = exactly(op, args)?;
                migo_services::storage::info_sync(self.scheduler()?, paths).map(OwnedValue::Str)
            }
            id::op_create_buffer_url => {
                let [buffer] = exactly(op, args)?;
                migo_services::storage::create_buffer_url(paths, &bytes(op, 0, buffer)?)
                    .map(OwnedValue::Str)
            }
            id::op_get_image_cache_stats => {
                let [] = exactly(op, args)?;
                let stats = migo_io::image_ops::get_image_cache_stats(self.session());
                serde_json::to_string(&stats)
                    .map(OwnedValue::Json)
                    .map_err(|error| ServiceError::generic(error.to_string()))
            }
            id::op_revoke_buffer_url => {
                let [url] = exactly(op, args)?;
                migo_services::storage::revoke_buffer_url(paths, &string(op, 0, url)?)
                    .map(|()| OwnedValue::Null)
            }
            // What content reads about the device: the host's last report,
            // through the same services the embedded ops read.
            id::op_webgl_query_compressed_caps => {
                let [] = exactly(op, args)?;
                Ok(OwnedValue::U32(self.compressed_texture_caps()))
            }
            id::op_get_battery_info => {
                let [] = exactly(op, args)?;
                self.device("getBatteryInfo", |device| device.battery())?
                    .get_info_json()
                    .map(OwnedValue::Str)
                    .map_err(device_error)
            }
            id::op_get_network_type => {
                let [] = exactly(op, args)?;
                self.device("getNetworkType", |device| device.network())?
                    .get_network_type_json()
                    .map(OwnedValue::Str)
                    .map_err(device_error)
            }
            id::op_require_resolve_and_read => {
                let [specifier, referrer_dir] = exactly(op, args)?;
                self.require(&string(op, 0, specifier)?, &string(op, 1, referrer_dir)?)
            }
            // What a game asks about its subpackages: the session's mount
            // table and the game's package store, read by the same service code
            // the embedded ops call.
            id::op_get_sub_packages => {
                let [] = exactly(op, args)?;
                // A session on this lane is started with the package it mounts
                // and no separate subpackage list; what is installed is what the
                // mount table shows.
                Ok(OwnedValue::Str(
                    migo_services::subpackage::sub_packages_json(&[]),
                ))
            }
            id::op_get_mount_generation => {
                let [] = exactly(op, args)?;
                Ok(OwnedValue::U64(
                    migo_services::subpackage::mount_generation(self.mount_table().as_deref()),
                ))
            }
            id::op_get_subpackage_identity => {
                let [root] = exactly(op, args)?;
                Ok(OwnedValue::Str(migo_services::subpackage::identity(
                    self.mount_table().as_deref(),
                    &string(op, 0, root)?,
                )))
            }
            id::op_is_subpackage_installed => {
                let [root] = exactly(op, args)?;
                Ok(OwnedValue::Bool(migo_services::subpackage::is_installed(
                    self.mount_table().as_deref(),
                    &string(op, 0, root)?,
                )))
            }
            id::op_is_subpackage_persisted => {
                let [name, root] = exactly(op, args)?;
                Ok(OwnedValue::Bool(migo_services::subpackage::is_persisted(
                    self.game_paths().as_deref(),
                    &string(op, 0, name)?,
                    &string(op, 1, root)?,
                )))
            }
            id::op_get_workers_path => {
                let [] = exactly(op, args)?;
                // A Worker's engine is staged beside the producer's, which is
                // the producer's own path to resolve; the host has none to give.
                Ok(OwnedValue::Str(String::new()))
            }
            file if super::service_fs::is_sync(file) => {
                super::service_fs::call_sync(&self.fs_env()?, file, args)
            }
            audio if service_audio::is_sync(audio) => {
                service_audio::call_sync(self.audio()?, audio, args)
            }
            network if service_network::is_sync(network) => service_network::call_sync(
                self.network()?,
                self.scheduler()?.as_ref(),
                network,
                args,
            ),
            other => Err(not_a(other, "synchronous")),
        }
    }

    /// Start an awaited op. The future owns what it needs and runs on the
    /// session thread's runtime.
    fn call_async(
        &self,
        op: u32,
        args: Vec<OwnedValue>,
        render: &RenderHandles,
    ) -> Result<BoxFuture<'static, Result<OwnedValue, ServiceError>>, ServiceError> {
        let paths = self.game_paths();
        Ok(match op {
            id::op_load_image => {
                let [image_id, src, tw, th] = exactly(op, args)?;
                let (image_id, src) = (u32_of(op, 0, image_id)?, string(op, 1, src)?);
                let (tw, th) = (u32_of(op, 2, tw)?, u32_of(op, 3, th)?);
                let env = self.image_env(render)?;
                // An `http(s)://` source is fetched by this session's network
                // service, under the policy every other request is held to.
                let fetch = self.image_fetch();
                Box::pin(async move {
                    migo_services::image::load_image(
                        &env,
                        image_id,
                        src,
                        (tw > 0).then_some(tw),
                        (th > 0).then_some(th),
                        |url| async move {
                            let (policy, client) = fetch?;
                            migo_services::network::image_source::fetch_http_image(
                                &policy, &client, &url,
                            )
                            .await
                        },
                    )
                    .await
                    .map(loaded_image)
                    .map_err(image_error)
                })
            }
            id::op_load_image_subrect => {
                let [image_id, src, sx, sy, sw, sh, rw, rh] = exactly(op, args)?;
                let image_id = u32_of(op, 0, image_id)?;
                let src = string(op, 1, src)?;
                let (sx, sy) = (i32_of(op, 2, sx)?, i32_of(op, 3, sy)?);
                let (sw, sh) = (u32_of(op, 4, sw)?, u32_of(op, 5, sh)?);
                let (rw, rh) = (u32_of(op, 6, rw)?, u32_of(op, 7, rh)?);
                let env = self.image_env(render)?;
                Box::pin(async move {
                    migo_services::image::load_image_subrect(
                        &env, image_id, src, sx, sy, sw, sh, rw, rh,
                    )
                    .await
                    .map(loaded_image)
                    .map_err(image_error)
                })
            }
            id::op_preload_images => {
                let [paths_arg] = exactly(op, args)?;
                let paths = strings(op, 0, paths_arg)?;
                let env = self.image_env(render)?;
                Box::pin(async move {
                    let entries = migo_services::image::preload_images(&env, paths).await;
                    Ok(OwnedValue::Array(
                        entries
                            .into_iter()
                            .map(|(path, ok, width, height, message)| {
                                OwnedValue::Array(vec![
                                    OwnedValue::Str(path),
                                    OwnedValue::Bool(ok),
                                    OwnedValue::U32(width),
                                    OwnedValue::U32(height),
                                    OwnedValue::Str(message),
                                ])
                            })
                            .collect(),
                    ))
                })
            }
            id::op_storage_get_async => {
                let [key] = exactly(op, args)?;
                let (scheduler, key) = (self.scheduler()?, string(op, 0, key)?);
                Box::pin(async move {
                    migo_services::storage::get(scheduler, paths.as_deref(), key)
                        .await
                        .map(OwnedValue::Str)
                })
            }
            id::op_storage_set_async => {
                let [key, value] = exactly(op, args)?;
                let (scheduler, key, value) = (
                    self.scheduler()?,
                    string(op, 0, key)?,
                    string(op, 1, value)?,
                );
                Box::pin(async move {
                    migo_services::storage::set(scheduler, paths.as_deref(), key, value)
                        .await
                        .map(|()| OwnedValue::Null)
                })
            }
            id::op_storage_remove_async => {
                let [key] = exactly(op, args)?;
                let (scheduler, key) = (self.scheduler()?, string(op, 0, key)?);
                Box::pin(async move {
                    migo_services::storage::remove(scheduler, paths.as_deref(), key)
                        .await
                        .map(|()| OwnedValue::Null)
                })
            }
            id::op_storage_clear_async => {
                let [] = exactly(op, args)?;
                let scheduler = self.scheduler()?;
                Box::pin(async move {
                    migo_services::storage::clear(scheduler, paths.as_deref())
                        .await
                        .map(|()| OwnedValue::Null)
                })
            }
            id::op_storage_info_async => {
                let [] = exactly(op, args)?;
                let scheduler = self.scheduler()?;
                Box::pin(async move {
                    migo_services::storage::info(scheduler, paths.as_deref())
                        .await
                        .map(OwnedValue::Str)
                })
            }
            file if super::service_fs::is_async(file) => {
                return super::service_fs::call_async(self.fs_env()?, file, args);
            }
            audio if service_audio::is_async(audio) => {
                return service_audio::call_async(self.audio()?, self.audio_sources(), audio, args);
            }
            network if service_network::is_async(network) => {
                return service_network::call_async(
                    self.network()?,
                    self.scheduler()?.as_ref(),
                    self.upload_sources(),
                    network,
                    args,
                );
            }
            other => return Err(not_a(other, "awaited")),
        })
    }

    /// Apply a command. Nothing answers it, so a failure is logged -- which is
    /// what the embedded runtime does with a failed fire-and-forget op too.
    fn command(
        &self,
        op: u32,
        args: Vec<OwnedValue>,
        render: &RenderHandles,
    ) -> Result<(), ServiceError> {
        match op {
            id::op_destroy_image => {
                let [image_id] = exactly(op, args)?;
                migo_services::image::destroy_image(
                    &self.aliases,
                    &render.canvas.tx,
                    u32_of(op, 0, image_id)?,
                );
                Ok(())
            }
            id::op_clear_image_cache => {
                let [] = exactly(op, args)?;
                let cache_dir = self
                    .game_paths()
                    .map(|paths| paths.cache_dir().to_string_lossy().into_owned());
                migo_services::image::clear_image_cache(
                    &self.aliases,
                    &render.canvas.tx,
                    cache_dir.as_deref(),
                    self.session(),
                );
                Ok(())
            }
            audio if service_audio::is_command(audio) => {
                service_audio::command(self.audio()?, audio, args)
            }
            network if service_network::is_command(network) => {
                service_network::command(self.network()?, network, args)
            }
            // The session, as content asks about itself. The same commands the
            // embedded ops send, on the same channel: what ends or restarts a
            // session is the host's, not the renderer's.
            id::op_exit_mini_program | id::op_restart_mini_program => {
                let [] = exactly(op, args)?;
                let command = if op == id::op_exit_mini_program {
                    shared::protocol::host_cmd::HostCommand::Shutdown
                } else {
                    shared::protocol::host_cmd::HostCommand::Restart
                };
                let what = if op == id::op_exit_mini_program {
                    "exitMiniProgram"
                } else {
                    "restartMiniProgram"
                };
                self.lifecycle()?
                    .try_send(command)
                    .map_err(|error| ServiceError::generic(format!("{what}:fail {error}")))
            }
            // The soft keyboard is host UI: opened, closed and corrected by
            // command, with what the player types coming back as input events.
            id::op_show_keyboard => {
                let [options] = exactly(op, args)?;
                self.keyboard("showKeyboard")?
                    .show(&string(op, 0, options)?)
                    .map_err(device_error)
            }
            id::op_hide_keyboard => {
                let [] = exactly(op, args)?;
                self.keyboard("hideKeyboard")?.hide().map_err(device_error)
            }
            id::op_update_keyboard => {
                let [value] = exactly(op, args)?;
                self.keyboard("updateKeyboard")?
                    .update(&string(op, 0, value)?)
                    .map_err(device_error)
            }
            // The device, as content asks the host to act on it.
            id::op_vibrate_short => {
                let [vibrate_type] = exactly(op, args)?;
                self.device("vibrateShort", |device| device.vibration())?
                    .vibrate_short(&string(op, 0, vibrate_type)?)
                    .map_err(device_error)
            }
            id::op_vibrate_long => {
                let [] = exactly(op, args)?;
                self.device("vibrateLong", |device| device.vibration())?
                    .vibrate_long()
                    .map_err(device_error)
            }
            id::op_set_keep_screen_on => {
                let [keep_on] = exactly(op, args)?;
                self.device("setKeepScreenOn", |device| device.screen())?
                    .set_keep_screen_on(boolean(op, 0, keep_on)?)
                    .map_err(device_error)
            }
            id::op_start_network_monitoring => {
                let [] = exactly(op, args)?;
                self.device("onNetworkStatusChange", |device| device.network())?
                    .start_monitoring()
                    .map_err(device_error)
            }
            id::op_stop_network_monitoring => {
                let [] = exactly(op, args)?;
                self.device("offNetworkStatusChange", |device| device.network())?
                    .stop_monitoring()
                    .map_err(device_error)
            }
            id::op_game_log_report => {
                let [entry] = exactly(op, args)?;
                self.device("gameLog.log", |device| device.game_log())?
                    .report_log(&string(op, 0, entry)?)
                    .map_err(device_error)
            }
            id::op_set_preferred_fps => {
                let [fps] = exactly(op, args)?;
                // The same filter the embedded op applies: a rate is rounded
                // and clamped into the range the engine offers, and one that is
                // not a number at all is ignored rather than clamped to the
                // bottom of it.
                if let Some(fps) = shared::frame_rate::requested_fps(f64_of(op, 0, fps)?) {
                    let _ = render
                        .canvas
                        .tx
                        .send(shared::protocol::render_cmd::RenderCommand::FrameRate(fps));
                }
                Ok(())
            }
            other => Err(not_a(other, "command")),
        }
    }
}

/// What the renderer is, for the services that hand it work: images upload
/// through it. Built on the session thread once the renderer is up, and owned
/// there -- a strong sender held anywhere else would keep the render queue
/// alive past the session.
pub(crate) struct RenderHandles {
    pub(crate) canvas: shared::op_state::CanvasOpState,
    pub(crate) gpu_caps: Arc<shared::device::gpu_caps::GpuCaps>,
}

/// An image load's answer, as the embedded op returns it: `[shared id,
/// [width, height]]`.
fn loaded_image((shared_id, (width, height)): (u32, (usize, usize))) -> OwnedValue {
    OwnedValue::Array(vec![
        OwnedValue::U32(shared_id),
        OwnedValue::Array(vec![
            OwnedValue::U32(width as u32),
            OwnedValue::U32(height as u32),
        ]),
    ])
}

/// An image load's failure, in the text the embedded op throws.
fn image_error(error: EngineError) -> ServiceError {
    ServiceError::generic(match &error.detail {
        Some(detail) => format!("[{:?}] {} ({})", error.code, error.msg, detail),
        None => format!("[{:?}] {}", error.code, error.msg),
    })
}

// ---------------------------------------------------------------------------
// Admission
// ---------------------------------------------------------------------------

/// What happened to a service message that was not refused.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ServiceAdmission {
    /// Admitted and queued for dispatch, with any held messages it released.
    Admitted,
    /// Ahead of a predecessor that has not arrived, and held until it does.
    Held,
    /// From another generation, and ignored: its producer is gone.
    OtherGeneration,
}

/// The admitted sequence and the messages held ahead of it.
struct Sequenced {
    sequencer: ServiceSequencer,
    /// By sequence: messages that arrived before their predecessor, with their
    /// size as received.
    held: std::collections::BTreeMap<u64, (Vec<OwnedServiceRecord>, usize)>,
    held_bytes: usize,
}

/// Why a service message was not admitted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ServiceSubmitError {
    /// The message broke a rule of the stream; the producer is told on the
    /// downlink, and the stream is broken from here.
    Refused(WireError),
    /// The session has ended: nothing more is admitted or answered.
    SessionEnded,
}

/// The service stream's host half, shared by the transports and the session.
pub(crate) struct ServiceHost {
    sequencer: Mutex<Sequenced>,
    admitted: Condvar,
    ended: AtomicBool,
    work: tokio::sync::mpsc::Sender<ServiceWork>,
    pub(crate) outbox: Arc<ServiceOutbox>,
    pub(crate) context: Arc<ServiceContext>,
}

impl ServiceHost {
    /// The host and the receiving end of its work queue, which the session
    /// thread drains.
    pub(crate) fn new(
        runtime_generation: u64,
        files_dir: PathBuf,
        cache_dir: PathBuf,
        waker: Arc<WakerSlot>,
    ) -> (Arc<Self>, tokio::sync::mpsc::Receiver<ServiceWork>) {
        // The wire carries the low 32 bits, as every other stream does.
        let generation = runtime_generation as u32;
        let (work, receiver) = tokio::sync::mpsc::channel(WORK_CAPACITY);
        (
            Arc::new(Self {
                sequencer: Mutex::new(Sequenced {
                    sequencer: ServiceSequencer::new(generation),
                    held: std::collections::BTreeMap::new(),
                    held_bytes: 0,
                }),
                admitted: Condvar::new(),
                ended: AtomicBool::new(false),
                work,
                outbox: Arc::new(ServiceOutbox::new(generation, waker)),
                context: Arc::new(ServiceContext::new(files_dir, cache_dir)),
            }),
            receiver,
        )
    }

    /// Admit one message, or hold it until its predecessor arrives.
    ///
    /// A message that is next is admitted with every held message that follows
    /// it. One that is ahead is held, within [`MAX_HELD_MESSAGES`] and
    /// [`MAX_HELD_BYTES`]; see the module docs for why reordering is the host's.
    ///
    /// A refusal is also reported to the producer on the downlink: the stream
    /// is broken from there, and a producer told nothing would wait forever on
    /// the requests in the refused message.
    ///
    /// Blocks while the work queue is full; see [`WORK_CAPACITY`]. Must not be
    /// called from inside a Tokio runtime.
    pub(crate) fn submit(&self, bytes: &[u8]) -> Result<ServiceAdmission, ServiceSubmitError> {
        let result = self.admit(bytes);
        if let Err(ServiceSubmitError::Refused(error)) = result {
            let sequence = read_service_envelope(bytes).map_or(0, |(_, sequence)| sequence);
            warn!("service message {sequence} refused: {error}");
            self.outbox.refused(error.code(), sequence);
        }
        result
    }

    fn admit(&self, bytes: &[u8]) -> Result<ServiceAdmission, ServiceSubmitError> {
        use ServiceSubmitError::{Refused, SessionEnded};
        // Validated in full before its sequence is looked at: a message that
        // would be refused must not first be held.
        let message = read_service_message(bytes).map_err(Refused)?;
        let records: Vec<OwnedServiceRecord> = message
            .records
            .iter()
            .map(ServiceRecord::to_owned_record)
            .collect();
        let mut state = self.sequencer.lock();
        if self.ended.load(Ordering::Acquire) {
            return Err(SessionEnded);
        }
        match state
            .sequencer
            .classify(message.generation, message.sequence)
        {
            Sequencing::OtherGeneration => return Ok(ServiceAdmission::OtherGeneration),
            Sequencing::Behind { expected } => {
                return Err(Refused(WireError::OutOfSequence {
                    expected,
                    received: message.sequence,
                }));
            }
            Sequencing::Ahead { expected } => {
                if state.held.contains_key(&message.sequence) {
                    return Err(Refused(WireError::OutOfSequence {
                        expected,
                        received: message.sequence,
                    }));
                }
                if state.held.len() >= MAX_HELD_MESSAGES
                    || state.held_bytes.saturating_add(bytes.len()) > MAX_HELD_BYTES
                {
                    return Err(Refused(WireError::TooFarAhead {
                        expected,
                        received: message.sequence,
                    }));
                }
                state.held_bytes += bytes.len();
                state.held.insert(message.sequence, (records, bytes.len()));
                return Ok(ServiceAdmission::Held);
            }
            Sequencing::Next => {}
        }
        // Queued under the sequencer lock, so queue order is sequence order --
        // this message, then every held one that now follows it.
        self.dispatch_admitted(&mut state, message.sequence, records)?;
        loop {
            let next = state.sequencer.admitted() + 1;
            let Some((records, size)) = state.held.remove(&next) else {
                break;
            };
            state.held_bytes -= size;
            self.dispatch_admitted(&mut state, next, records)?;
        }
        drop(state);
        self.admitted.notify_all();
        Ok(ServiceAdmission::Admitted)
    }

    fn dispatch_admitted(
        &self,
        state: &mut Sequenced,
        sequence: u64,
        records: Vec<OwnedServiceRecord>,
    ) -> Result<(), ServiceSubmitError> {
        if self
            .work
            .blocking_send(ServiceWork::Records(records))
            .is_err()
        {
            // The session thread is gone; nothing more will be answered.
            return Err(ServiceSubmitError::SessionEnded);
        }
        state.sequencer.admit(sequence);
        Ok(())
    }

    /// Wait until `sequence` has been admitted. Zero admits at once.
    pub(crate) fn wait_admitted(&self, sequence: u64, until: Instant) -> Result<(), SyncError> {
        let mut state = self.sequencer.lock();
        while state.sequencer.admitted() < sequence {
            if self.ended.load(Ordering::Acquire) {
                return Err(SyncError::SessionEnded);
            }
            if self.admitted.wait_until(&mut state, until).timed_out()
                && state.sequencer.admitted() < sequence
            {
                return Err(SyncError::TimedOut);
            }
        }
        Ok(())
    }

    /// Run one synchronous service call, in order behind every admitted
    /// message, and answer with its encoded outcome.
    ///
    /// The call joins the session's queue and waits for its turn -- the
    /// session thread reaching it means every record admitted before it has
    /// been started, which is the ordering content relies on -- and then runs
    /// **on the calling thread**. That thread is the host's synchronous
    /// endpoint, already blocked for exactly this answer; running the op there
    /// saves a hop and, measured on the iOS simulator (2026-09-19), removes a
    /// priority inversion: the endpoint's user-interactive queue used to wait
    /// on a blocking-pool thread at default QoS. `until` bounds the wait for the
    /// turn. A call that gives up waiting never runs; one that has started
    /// answers with what it did, because a write that happened is not a timeout.
    ///
    /// The op's own failure is an answer (`OUTCOME_ERROR`); the barrier failing
    /// -- a malformed call, a timeout, a session that ended -- is the error.
    pub(crate) fn call_sync(&self, params: &[u8], until: Instant) -> Result<Vec<u8>, SyncError> {
        if params.len() < 4 {
            return Err(SyncError::UnsupportedOperation);
        }
        let op = u32::from_le_bytes([params[0], params[1], params[2], params[3]]);
        let args = read_values(&params[4..])
            .map_err(|_| SyncError::UnsupportedOperation)?
            .iter()
            .map(|value| value.to_owned_value())
            .collect();
        let (turn, my_turn) = std::sync::mpsc::sync_channel(1);
        self.work
            .blocking_send(ServiceWork::Sync { turn })
            .map_err(|_| SyncError::SessionEnded)?;
        my_turn
            .recv_timeout(until.saturating_duration_since(Instant::now()))
            .map_err(|error| match error {
                std::sync::mpsc::RecvTimeoutError::Timeout => SyncError::TimedOut,
                std::sync::mpsc::RecvTimeoutError::Disconnected => SyncError::SessionEnded,
            })?;
        Ok(encode_outcome(self.context.call_sync(op, args)))
    }

    /// Refuse everything from here on and release every waiter.
    pub(crate) fn end(&self) {
        let mut state = self.sequencer.lock();
        self.ended.store(true, Ordering::Release);
        state.held.clear();
        state.held_bytes = 0;
        drop(state);
        self.admitted.notify_all();
    }
}

/// The service stream, usable without holding the session's own lock.
///
/// Admitting a POSTed message can wait for the socket message in front of it,
/// and that message arrives through the same boundary on another thread: a
/// caller that waited while holding the session lock would hold up the very
/// message it is waiting for. So the boundary takes this handle under the lock
/// and uses it outside, as it does the synchronous barrier's.
#[derive(Clone)]
pub struct ServiceHandle(pub(crate) Arc<ServiceHost>);

impl ServiceHandle {
    /// See [`super::external::ExternalFrameSession::submit_service`].
    pub fn submit(&self, bytes: &[u8]) -> Result<ServiceAdmission, ServiceSubmitError> {
        self.0.submit(bytes)
    }

    /// One `MDS1` message of queued answers and events, or `None`.
    pub fn take_message(&self) -> Option<Vec<u8>> {
        self.0.outbox.take_message()
    }

    /// The text the content module at `request_path` is evaluated as, for the
    /// origin that serves content to WebKit: see
    /// [`migo_services::content::MountedContent::module_source`]. `None` before
    /// content is loaded, when there is no package to serve from. Reads the
    /// file on the calling thread.
    pub fn content_module(
        &self,
        request_path: &str,
    ) -> Option<Result<Vec<u8>, migo_services::content::ModuleError>> {
        self.0.context.content_module(request_path)
    }

    /// A parked answer, taken once.
    pub fn take_parked(&self, generation: u32, request_id: u32) -> Option<Vec<u8>> {
        self.0.outbox.take_parked(generation, request_id)
    }
}

/// A synchronous call's reply: `outcome u32`, then the value or the error.
fn encode_outcome(outcome: Result<OwnedValue, ServiceError>) -> Vec<u8> {
    match outcome {
        Ok(value) => {
            let mut writer = ValueWriter::over(OUTCOME_OK.to_le_bytes().to_vec());
            value.write_to(&mut writer);
            writer.into_bytes()
        }
        Err(error) => {
            let mut writer = ValueWriter::over(OUTCOME_ERROR.to_le_bytes().to_vec());
            writer.str(error.class);
            writer.str(&error.message);
            writer.into_bytes()
        }
    }
}

// ---------------------------------------------------------------------------
// Dispatch, on the session thread
// ---------------------------------------------------------------------------

type InFlight = Arc<Mutex<HashMap<u32, tokio::task::AbortHandle>>>;

/// Starts admitted work on the session thread, in the order it was admitted.
pub(crate) struct ServiceDispatcher {
    context: Arc<ServiceContext>,
    outbox: Arc<ServiceOutbox>,
    in_flight: InFlight,
    render: RenderHandles,
}

impl Drop for ServiceDispatcher {
    /// The dispatcher goes when the session thread does, after its runtime --
    /// and every load that runtime was running -- has gone. So this is the one
    /// point after which nothing can pin an image again.
    fn drop(&mut self) {
        self.context.release_images();
    }
}

impl ServiceDispatcher {
    pub(crate) fn new(host: &ServiceHost, render: RenderHandles) -> Self {
        Self {
            context: Arc::clone(&host.context),
            outbox: Arc::clone(&host.outbox),
            in_flight: Arc::new(Mutex::new(HashMap::new())),
            render,
        }
    }

    /// Where host events for the producer are queued.
    pub(crate) fn outbox(&self) -> &ServiceOutbox {
        &self.outbox
    }

    /// Start one piece of work. Must run inside the session's Tokio runtime.
    pub(crate) fn dispatch(&self, work: ServiceWork) {
        match work {
            ServiceWork::Records(records) => {
                for record in records {
                    self.record(record);
                }
            }
            ServiceWork::Sync { turn } => {
                // The call's turn, not its work: the caller runs the op on its
                // own thread (see `ServiceHost::call_sync`), so the session
                // thread -- the one that forwards frame clock ticks -- never
                // blocks for a disk read. A caller that timed out has dropped
                // the receiver and will not run it.
                let _ = turn.send(());
            }
        }
    }

    fn record(&self, record: OwnedServiceRecord) {
        match record {
            OwnedServiceRecord::Request {
                request_id,
                op,
                args,
            } => match self.context.call_async(op, args, &self.render) {
                Ok(future) => {
                    let outbox = Arc::clone(&self.outbox);
                    let in_flight = Arc::clone(&self.in_flight);
                    // The entry is inserted under the lock the task removes it
                    // under, and before the task can run, so a task that
                    // finishes at once still finds its own entry.
                    let mut map = self.in_flight.lock();
                    let task = tokio::spawn(async move {
                        let outcome = future.await;
                        // Whoever removes the entry answers: this task, or a
                        // cancel that got there first. Exactly one answer.
                        if in_flight.lock().remove(&request_id).is_some() {
                            outbox.reply(request_id, outcome);
                        }
                    });
                    map.insert(request_id, task.abort_handle());
                }
                Err(error) => self.outbox.reply(request_id, Err(error)),
            },
            OwnedServiceRecord::Command { op, args } => {
                if let Err(error) = self.context.command(op, args, &self.render) {
                    warn!("service command refused: {error}");
                }
            }
            OwnedServiceRecord::Cancel { request_id } => {
                if let Some(task) = self.in_flight.lock().remove(&request_id) {
                    task.abort();
                    self.outbox.reply(
                        request_id,
                        Err(ServiceError::classed(
                            "AbortError",
                            "the request was cancelled",
                        )),
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::service_args::CLASS_TYPE_ERROR;
    use frame_wire::service::{encode_service_message, read_down_message, read_down_record_bytes};
    use frame_wire::value::read_value;

    fn host() -> (Arc<ServiceHost>, tokio::sync::mpsc::Receiver<ServiceWork>) {
        ServiceHost::new(
            1,
            PathBuf::new(),
            PathBuf::new(),
            Arc::new(WakerSlot::default()),
        )
    }

    fn message(sequence: u64, records: Vec<OwnedServiceRecord>) -> Vec<u8> {
        encode_service_message(1, sequence, &records)
    }

    fn request(request_id: u32, op: u32) -> OwnedServiceRecord {
        OwnedServiceRecord::Request {
            request_id,
            op,
            args: vec![],
        }
    }

    fn refused_codes(host: &ServiceHost) -> Vec<(u32, u64)> {
        let mut codes = Vec::new();
        while let Some(bytes) = host.outbox.take_message() {
            let (_, records) = read_down_message(&bytes).expect("a message the outbox wrote");
            for record in records {
                if let ServiceDownRecord::Refused { code, sequence } = record {
                    codes.push((code, sequence));
                }
            }
        }
        codes
    }

    fn queued_ids(work: &mut tokio::sync::mpsc::Receiver<ServiceWork>) -> Vec<u32> {
        std::iter::from_fn(|| work.try_recv().ok())
            .flat_map(|work| match work {
                ServiceWork::Records(records) => records
                    .into_iter()
                    .map(|record| match record {
                        OwnedServiceRecord::Request { request_id, .. } => request_id,
                        other => panic!("{other:?}"),
                    })
                    .collect::<Vec<_>>(),
                ServiceWork::Sync { .. } => panic!("no synchronous call was made"),
            })
            .collect()
    }

    #[test]
    fn messages_are_admitted_in_sequence_and_queued_in_that_order() {
        let (host, mut work) = host();
        for sequence in 1..=3u64 {
            assert_eq!(
                host.submit(&message(sequence, vec![request(sequence as u32, 99)])),
                Ok(ServiceAdmission::Admitted)
            );
        }
        assert_eq!(queued_ids(&mut work), vec![1, 2, 3]);
    }

    /// The scheme request that overtook the socket message sent before it: held,
    /// then released in order when the gap closes -- and the messages behind it
    /// with it.
    #[test]
    fn a_message_ahead_of_its_predecessor_is_held_and_released_in_order() {
        let (host, mut work) = host();
        assert_eq!(
            host.submit(&message(3, vec![request(3, 1)])),
            Ok(ServiceAdmission::Held)
        );
        assert_eq!(
            host.submit(&message(2, vec![request(2, 1)])),
            Ok(ServiceAdmission::Held)
        );
        assert!(
            queued_ids(&mut work).is_empty(),
            "nothing runs ahead of message 1"
        );
        assert_eq!(
            host.submit(&message(1, vec![request(1, 1)])),
            Ok(ServiceAdmission::Admitted)
        );
        assert_eq!(
            queued_ids(&mut work),
            vec![1, 2, 3],
            "queued in sequence order"
        );
        assert_eq!(
            host.submit(&message(4, vec![request(4, 1)])),
            Ok(ServiceAdmission::Admitted)
        );
    }

    #[test]
    fn the_hold_is_bounded_and_overflowing_it_is_refused() {
        let (host, _work) = host();
        for sequence in 2..(2 + MAX_HELD_MESSAGES as u64) {
            assert_eq!(
                host.submit(&message(sequence, vec![request(1, 1)])),
                Ok(ServiceAdmission::Held)
            );
        }
        let beyond = 2 + MAX_HELD_MESSAGES as u64;
        let refused = WireError::TooFarAhead {
            expected: 1,
            received: beyond,
        };
        assert_eq!(
            host.submit(&message(beyond, vec![request(1, 1)])),
            Err(ServiceSubmitError::Refused(refused))
        );
        assert_eq!(refused_codes(&host), vec![(refused.code(), beyond)]);
    }

    #[test]
    fn a_replayed_or_duplicated_message_is_refused() {
        let (host, _work) = host();
        host.submit(&message(1, vec![request(1, 1)])).unwrap();
        assert_eq!(
            host.submit(&message(1, vec![request(1, 1)])),
            Err(ServiceSubmitError::Refused(WireError::OutOfSequence {
                expected: 2,
                received: 1
            }))
        );
        let (host, _work) = super::tests::host();
        host.submit(&message(3, vec![request(1, 1)])).unwrap();
        assert!(matches!(
            host.submit(&message(3, vec![request(1, 1)])),
            Err(ServiceSubmitError::Refused(WireError::OutOfSequence { .. }))
        ));
    }

    #[test]
    fn a_message_from_another_generation_is_ignored_not_refused() {
        let (host, mut work) = host();
        let bytes = encode_service_message(7, 1, &[request(1, 1)]);
        assert_eq!(host.submit(&bytes), Ok(ServiceAdmission::OtherGeneration));
        assert!(work.try_recv().is_err(), "nothing was queued");
        assert!(refused_codes(&host).is_empty(), "and nothing was refused");
    }

    #[test]
    fn a_synchronous_call_waits_for_the_message_it_follows() {
        let (host, _work) = host();
        let waiter = {
            let host = Arc::clone(&host);
            std::thread::spawn(move || {
                host.wait_admitted(2, Instant::now() + std::time::Duration::from_secs(10))
            })
        };
        std::thread::sleep(std::time::Duration::from_millis(30));
        host.submit(&message(2, vec![request(2, 1)])).unwrap();
        assert!(!waiter.is_finished(), "held is not admitted");
        host.submit(&message(1, vec![request(1, 1)])).unwrap();
        assert_eq!(waiter.join().unwrap(), Ok(()));
    }

    #[test]
    fn ending_refuses_what_follows_and_releases_a_waiting_call() {
        let (host, _work) = host();
        let waiter = {
            let host = Arc::clone(&host);
            std::thread::spawn(move || {
                host.wait_admitted(5, Instant::now() + std::time::Duration::from_secs(10))
            })
        };
        std::thread::sleep(std::time::Duration::from_millis(30));
        let started = Instant::now();
        host.end();
        assert_eq!(waiter.join().unwrap(), Err(SyncError::SessionEnded));
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        assert_eq!(
            host.submit(&message(1, vec![request(1, 1)])),
            Err(ServiceSubmitError::SessionEnded)
        );
    }

    #[test]
    fn a_large_answer_is_parked_and_taken_once() {
        let (host, _work) = host();
        let big = OwnedValue::Bytes(vec![7; MAX_INLINE_REPLY_BYTES]);
        host.outbox.reply(5, Ok(big.clone()));
        let (_, records) = read_down_message(&host.outbox.take_message().unwrap()).unwrap();
        let [
            ServiceDownRecord::ReplyParked {
                request_id: 5,
                byte_length,
            },
        ] = records.as_slice()
        else {
            panic!("expected a parked notice, got {records:?}");
        };
        let parked = host.outbox.take_parked(1, 5).expect("parked");
        assert_eq!(parked.len(), *byte_length as usize);
        assert_eq!(
            read_down_record_bytes(&parked),
            Ok(ServiceDownRecord::Reply {
                request_id: 5,
                outcome: Ok(big)
            })
        );
        assert!(host.outbox.take_parked(1, 5).is_none(), "taken once");
        assert_eq!(host.outbox.queued_bytes(), 0, "and released");
    }

    #[test]
    fn a_message_batches_records_up_to_the_inline_bound() {
        let (host, _work) = host();
        // Just under half the bound once framed, so exactly two fit a message.
        let under_half = OwnedValue::Bytes(vec![0; MAX_INLINE_REPLY_BYTES / 2 - 64]);
        for request_id in 1..=3 {
            host.outbox.reply(request_id, Ok(under_half.clone()));
        }
        let first = host.outbox.take_message().unwrap();
        assert!(first.len() <= SERVICE_DOWN_HEADER_BYTES + MAX_INLINE_REPLY_BYTES);
        let first = read_down_message(&first).unwrap().1;
        let second = read_down_message(&host.outbox.take_message().unwrap())
            .unwrap()
            .1;
        assert_eq!(
            (first.len(), second.len()),
            (2, 1),
            "two fit, the third waits"
        );
        assert!(
            host.outbox.take_message().is_none(),
            "every answer delivered once"
        );
    }

    #[test]
    fn an_answer_is_encoded_as_its_outcome() {
        let ok = encode_outcome(Ok(OwnedValue::Str("v".into())));
        assert_eq!(&ok[..4], &OUTCOME_OK.to_le_bytes());
        assert_eq!(
            read_value(&ok[4..]).unwrap().to_owned_value(),
            OwnedValue::Str("v".into())
        );
        let failed = encode_outcome(Err(ServiceError::classed("StorageError", "full")));
        assert_eq!(&failed[..4], &OUTCOME_ERROR.to_le_bytes());
        assert_eq!(
            read_values(&failed[4..])
                .unwrap()
                .iter()
                .map(|value| value.to_owned_value())
                .collect::<Vec<_>>(),
            vec![
                OwnedValue::Str("StorageError".into()),
                OwnedValue::Str("full".into())
            ]
        );
    }

    #[test]
    fn a_call_with_the_wrong_arguments_is_a_type_error_naming_the_op() {
        let context = ServiceContext::new(PathBuf::new(), PathBuf::new());
        context.bind_session(1);
        let error = context
            .call_sync(id::op_storage_get, vec![OwnedValue::U32(1)])
            .expect_err("a number is not a key");
        assert_eq!(error.class, CLASS_TYPE_ERROR);
        assert_eq!(
            error.message,
            "argument 0 of op_storage_get is a string, not a u32"
        );
        let error = context
            .call_sync(id::op_storage_get, vec![])
            .expect_err("no key");
        assert_eq!(error.message, "op_storage_get takes 1 argument(s), not 0");
    }

    /// A storage round trip through the real services, the way the producer
    /// makes one: content mounted, a synchronous set, an awaited get.
    #[test]
    fn storage_answers_through_the_mounted_content() {
        let root =
            std::env::temp_dir().join(format!("migo-external-services-{}", std::process::id()));
        let files = root.join("files");
        let cache = root.join("cache");
        let installed = shared::vfs::GamePaths::new(&files, &cache, "g", 1).unwrap();
        std::fs::create_dir_all(installed.code_dir()).unwrap();

        let context = Arc::new(ServiceContext::new(files, cache));
        context.bind_session(1);
        assert!(
            context
                .call_sync(id::op_storage_get, vec![OwnedValue::Str("k".into())])
                .is_err(),
            "nothing to read before content is loaded"
        );
        let code = context
            .load_content("g", "game.js")
            .expect("installed content mounts");
        assert_eq!(code, installed.code_dir());
        assert!(
            context.load_content("g", "game.js").is_err(),
            "once per session"
        );

        context
            .call_sync(
                id::op_storage_set,
                vec![OwnedValue::Str("k".into()), OwnedValue::Str("v".into())],
            )
            .expect("set");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let (sender, _commands) = shared::render_command_sender::CommandSender::new();
        let render = RenderHandles {
            canvas: shared::op_state::CanvasOpState::for_host(sender, 1),
            gpu_caps: shared::device::gpu_caps::GpuCaps::new(),
        };
        let read = runtime
            .block_on(
                context
                    .call_async(
                        id::op_storage_get_async,
                        vec![OwnedValue::Str("k".into())],
                        &render,
                    )
                    .expect("a known async op"),
            )
            .expect("get");
        assert_eq!(read, OwnedValue::Str("v".into()));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The file system and `require` answer through the same sandbox the
    /// embedded ops read: a save written to `/user` reads back, `/code` lists
    /// the package, a descriptor reads what is there, a relative require
    /// resolves against the package root, and a write into `/code` is the
    /// embedded op's refusal.
    #[test]
    fn files_and_require_answer_through_the_mounted_content() {
        let root =
            std::env::temp_dir().join(format!("migo-external-services-fs-{}", std::process::id()));
        let files = root.join("files");
        let cache = root.join("cache");
        let installed = shared::vfs::GamePaths::new(&files, &cache, "g", 1).unwrap();
        std::fs::create_dir_all(installed.code_dir().join("js")).unwrap();
        std::fs::write(
            installed.code_dir().join("game.js"),
            "require('./js/main');",
        )
        .unwrap();
        std::fs::write(
            installed.code_dir().join("js/main.js"),
            "module.exports = 7;",
        )
        .unwrap();

        let context = Arc::new(ServiceContext::new(files, cache));
        context.bind_session(1);
        let before = context
            .call_sync(id::op_access_sync, vec![OwnedValue::Str("/user/a".into())])
            .expect_err("no sandbox before content is loaded");
        assert_eq!(before.class, "IOError");
        assert_eq!(before.message, "File system not initialized");
        assert!(
            context.content_module("/game.js").is_none(),
            "no module is served before content is loaded"
        );
        context
            .load_content("g", "game.js")
            .expect("installed content mounts");
        assert_eq!(
            context
                .content_module("/game.js")
                .expect("content is loaded")
                .expect("the entry is served"),
            shared::cjs_compat::wrap_cjs("require('./js/main');").into_bytes(),
            "the entry is served as the embedded loader evaluates it"
        );

        let str = |text: &str| OwnedValue::Str(text.into());
        let written = context
            .call_sync(
                id::op_write_or_append_file_sync,
                vec![
                    str("/user/save.json"),
                    OwnedValue::Null,
                    str("{\"level\":3}"),
                    str("utf8"),
                    OwnedValue::Bool(false),
                    OwnedValue::Bool(true),
                ],
            )
            .expect("write a save");
        assert_eq!(written, OwnedValue::Bool(true));
        let read = context
            .call_sync(
                id::op_read_file_sync,
                vec![str("/user/save.json"), OwnedValue::Null, OwnedValue::Null],
            )
            .expect("read it back");
        assert_eq!(read, OwnedValue::Bytes(b"{\"level\":3}".to_vec()));

        let stat = context
            .call_sync(
                id::op_stat_sync,
                vec![str("/user/save.json"), OwnedValue::Bool(false)],
            )
            .expect("stat");
        let OwnedValue::Array(stat) = stat else {
            panic!("a stat is an array")
        };
        assert_eq!(stat[0], OwnedValue::U32(0), "a single stat");
        let OwnedValue::Array(fields) = &stat[1] else {
            panic!("its fields")
        };
        assert_eq!(fields[1], OwnedValue::F64(11.0), "size, a Number");
        assert_eq!(fields[4], OwnedValue::Bool(true), "is_file");

        let listed = context
            .call_sync(id::op_readdir_sync, vec![str("/code")])
            .expect("readdir");
        let OwnedValue::Array(mut names) = listed else {
            panic!("names")
        };
        names.sort_by_key(|name| format!("{name:?}"));
        assert_eq!(names, vec![str("game.js"), str("js")]);

        let fd = context
            .call_sync(id::op_open_file_sync, vec![str("js/main.js"), str("r")])
            .expect("open a package file by its relative path");
        let OwnedValue::U32(fd) = fd else {
            panic!("a descriptor")
        };
        let window = context
            .call_sync(
                id::op_read_fd_into_sync,
                vec![OwnedValue::U32(fd), OwnedValue::U64(6), OwnedValue::U64(17)],
            )
            .expect("read into a window");
        assert_eq!(
            window,
            OwnedValue::Bytes(b"7;".to_vec()),
            "the filled prefix only"
        );
        context
            .call_sync(id::op_close_file_sync, vec![OwnedValue::U32(fd)])
            .expect("close");

        let refused = context
            .call_sync(
                id::op_write_or_append_file_sync,
                vec![
                    str("/code/game.js"),
                    OwnedValue::Bytes(vec![1]),
                    OwnedValue::Null,
                    OwnedValue::Null,
                    OwnedValue::Bool(false),
                    OwnedValue::Bool(true),
                ],
            )
            .expect_err("the package is read-only");
        assert_eq!(refused.message, "Permission denied: /code/game.js");

        let module = context
            .call_sync(
                id::op_require_resolve_and_read,
                vec![str("./js/main"), str("")],
            )
            .expect("require resolves against the package root");
        let OwnedValue::Array(module) = module else {
            panic!("code, path, dir")
        };
        assert_eq!(module[0], str("module.exports = 7;"));

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (sender, _commands) = shared::render_command_sender::CommandSender::new();
        let render = RenderHandles {
            canvas: shared::op_state::CanvasOpState::for_host(sender, 1),
            gpu_caps: shared::device::gpu_caps::GpuCaps::new(),
        };
        let awaited = runtime
            .block_on(
                context
                    .call_async(
                        id::op_read_file,
                        vec![str("game.js"), OwnedValue::U64(0), OwnedValue::U64(7)],
                        &render,
                    )
                    .expect("a known async op"),
            )
            .expect("an awaited ranged read");
        assert_eq!(awaited, OwnedValue::Bytes(b"require".to_vec()));
        let _ = std::fs::remove_dir_all(&root);
    }

    fn hex_bytes(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|at| u8::from_str_radix(&text[at..at + 2], 16).expect("hex"))
            .collect()
    }

    fn hex_text(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    /// The producer's file calls, as its lanes encode them, run through this
    /// host's dispatch on a real game sandbox, in the order content made them.
    ///
    /// `test/emit-file-calls.mjs write` records every service call a script of
    /// file operations makes -- all 41 file ops and `require`, sync and
    /// awaited -- and `read` checks what the producer makes of the answers this
    /// writes. A TypeError here is the producer and the host disagreeing about
    /// an op's arguments; the rest of the verdict is the producer's.
    #[test]
    #[ignore = "needs the producer's calls from node; run through scripts/test-performance-plus-engine-contract.sh"]
    fn the_producer_s_file_calls_run_on_the_host() {
        let dir = PathBuf::from(
            std::env::var("MIGO_FILE_CALLS_DIR")
                .expect("MIGO_FILE_CALLS_DIR names emit-file-calls.mjs's output"),
        );
        let calls: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join("calls.json")).expect("calls.json"),
        )
        .expect("the calls are JSON");

        let root =
            std::env::temp_dir().join(format!("migo-external-file-calls-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let files = root.join("files");
        let cache = root.join("cache");
        let installed = shared::vfs::GamePaths::new(&files, &cache, "g", 1).unwrap();
        std::fs::create_dir_all(installed.code_dir().join("js")).unwrap();
        std::fs::write(
            installed.code_dir().join("game.js"),
            "require('./js/main');",
        )
        .unwrap();
        std::fs::write(
            installed.code_dir().join("js/main.js"),
            "module.exports = 7;",
        )
        .unwrap();
        let context = Arc::new(ServiceContext::new(files, cache));
        context.bind_session(1);
        context
            .load_content("g", "game.js")
            .expect("installed content mounts");

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (sender, _commands) = shared::render_command_sender::CommandSender::new();
        let render = RenderHandles {
            canvas: shared::op_state::CanvasOpState::for_host(sender, 1),
            gpu_caps: shared::device::gpu_caps::GpuCaps::new(),
        };

        // The producer was answered descriptor 7 for every open; the host's
        // own descriptor replaces it in the calls that name one.
        const RECORDED_FD: u32 = 7;
        let takes_fd = |name: &str| {
            let base = name.strip_suffix("_sync").unwrap_or(name);
            matches!(
                base,
                "op_close_file"
                    | "op_fstat"
                    | "op_ftruncate"
                    | "op_write_file"
                    | "op_read_fd"
                    | "op_read_fd_into"
            )
        };
        let mut host_fd = None;
        let mut answers = Vec::new();
        let mut type_errors = Vec::new();
        for call in calls.as_array().expect("a list") {
            let name = call["op"].as_str().expect("an op name");
            let op = crate::runtime::service_ops::ALL
                .iter()
                .find(|(known, _)| *known == name)
                .map(|(_, id)| *id)
                .unwrap_or_else(|| panic!("{name} has no number"));
            let mut args = read_values(&hex_bytes(call["args"].as_str().expect("args")))
                .unwrap_or_else(|error| panic!("{name}: the arguments do not read: {error:?}"))
                .iter()
                .map(|value| value.to_owned_value())
                .collect::<Vec<_>>();
            if takes_fd(name) {
                assert_eq!(
                    args.first(),
                    Some(&OwnedValue::U32(RECORDED_FD)),
                    "{name} passes the descriptor it was given"
                );
                args[0] = OwnedValue::U32(host_fd.expect("a descriptor is open"));
            }
            let outcome = match call["shape"].as_str() {
                Some("sync") => context.call_sync(op, args),
                Some("async") => match context.call_async(op, args, &render) {
                    Ok(future) => runtime.block_on(future),
                    Err(error) => Err(error),
                },
                other => panic!("{name}: shape {other:?}"),
            };
            if name.starts_with("op_open_file")
                && let Ok(OwnedValue::U32(fd)) = &outcome
            {
                host_fd = Some(*fd);
            }
            if let Err(error) = &outcome
                && error.class == CLASS_TYPE_ERROR
            {
                type_errors.push(format!("{name}: {}", error.message));
            }
            answers.push(serde_json::json!({
                "op": name,
                "outcome": hex_text(&encode_outcome(outcome)),
            }));
        }
        std::fs::write(
            dir.join("answers.json"),
            serde_json::to_string(&answers).expect("JSON"),
        )
        .expect("write the answers");
        let _ = std::fs::remove_dir_all(&root);
        assert!(
            type_errors.is_empty(),
            "the host refused the producer's arguments:\n  {}",
            type_errors.join("\n  ")
        );
        println!("ran {} producer file calls on the host", answers.len());
    }

    /// The two calls content makes about the session itself reach the host's
    /// own command channel -- the one the embedded ops send on -- and a frame
    /// rate is filtered as the embedded op filters it: rounded into range, and
    /// dropped when it is not a number.
    #[test]
    fn the_session_s_own_commands_reach_the_host_s_channel() {
        let root = std::env::temp_dir().join(format!("migo-lifecycle-{}", std::process::id()));
        let context = Arc::new(ServiceContext::new(root.join("files"), root.join("cache")));
        context.bind_session(1);
        let (host_tx, _critical, mut host_rx) = shared::host_channel::channel(4);
        context.bind_lifecycle(host_tx);
        let (sender, commands) = shared::render_command_sender::CommandSender::new();
        let render = RenderHandles {
            canvas: shared::op_state::CanvasOpState::for_host(sender, 1),
            gpu_caps: shared::device::gpu_caps::GpuCaps::new(),
        };

        context
            .command(id::op_restart_mini_program, Vec::new(), &render)
            .expect("the channel takes it");
        context
            .command(id::op_exit_mini_program, Vec::new(), &render)
            .expect("the channel takes it");
        assert!(matches!(
            host_rx.try_recv(),
            Ok(shared::protocol::host_cmd::HostCommand::Restart)
        ));
        assert!(matches!(
            host_rx.try_recv(),
            Ok(shared::protocol::host_cmd::HostCommand::Shutdown)
        ));

        context
            .command(
                id::op_set_preferred_fps,
                vec![OwnedValue::F64(30.0)],
                &render,
            )
            .expect("a rate the engine offers");
        context
            .command(
                id::op_set_preferred_fps,
                vec![OwnedValue::F64(1000.0)],
                &render,
            )
            .expect("a rate past the range is clamped into it");
        context
            .command(
                id::op_set_preferred_fps,
                vec![OwnedValue::F64(f64::NAN)],
                &render,
            )
            .expect("a rate that is not a number is dropped");
        let rates: Vec<u32> = std::iter::from_fn(|| commands.try_recv().ok())
            .filter_map(|command| match command {
                shared::protocol::render_cmd::RenderCommand::FrameRate(fps) => Some(fps),
                _ => None,
            })
            .collect();
        assert_eq!(
            rates,
            vec![30, shared::frame_rate::MAX_FPS],
            "the rate is clamped, and a rate that is not a number sends nothing"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The soft keyboard reaches the host's keyboard service with content's own
    /// arguments, and a host that supplies none is refused in the embedded op's
    /// words -- which is what `migo.showKeyboard`'s `fail` sees on both lanes.
    #[test]
    fn keyboard_commands_reach_the_host_s_keyboard_or_are_refused_as_in_process() {
        #[derive(Default)]
        struct Recorded(parking_lot::Mutex<Vec<String>>);
        impl shared::services::KeyboardService for Recorded {
            fn show(&self, options: &str) -> Result<(), shared::services::ServiceError> {
                self.0.lock().push(format!("show {options}"));
                Ok(())
            }
            fn hide(&self) -> Result<(), shared::services::ServiceError> {
                self.0.lock().push("hide".into());
                Ok(())
            }
            fn update(&self, value: &str) -> Result<(), shared::services::ServiceError> {
                self.0.lock().push(format!("update {value}"));
                Err(shared::services::ServiceError::not_supported(
                    "updateKeyboard:fail busy",
                ))
            }
        }
        let root = std::env::temp_dir().join(format!("migo-keyboard-{}", std::process::id()));
        let (sender, _commands) = shared::render_command_sender::CommandSender::new();
        let render = RenderHandles {
            canvas: shared::op_state::CanvasOpState::for_host(sender, 1),
            gpu_caps: shared::device::gpu_caps::GpuCaps::new(),
        };

        let context = ServiceContext::new(root.join("files"), root.join("cache"));
        let keyboard = Arc::new(Recorded::default());
        // A platform whose only device service is this keyboard.
        struct Platform(Arc<Recorded>);
        impl shared::services::SensorServices for Platform {}
        impl shared::services::MediaServices for Platform {}
        impl shared::services::ConnectivityServices for Platform {}
        impl shared::services::CommerceServices for Platform {}
        impl shared::services::SystemUtilServices for Platform {
            fn keyboard(&self) -> Option<Arc<dyn shared::services::KeyboardService>> {
                Some(Arc::clone(&self.0) as Arc<dyn shared::services::KeyboardService>)
            }
        }
        context.bind_device(Some(Arc::new(Platform(Arc::clone(&keyboard)))));
        let options = r#"{"defaultValue":"hi","maxLength":8}"#;
        context
            .command(
                id::op_show_keyboard,
                vec![OwnedValue::Str(options.into())],
                &render,
            )
            .expect("shown");
        context
            .command(id::op_hide_keyboard, Vec::new(), &render)
            .expect("hidden");
        let refused = context
            .command(
                id::op_update_keyboard,
                vec![OwnedValue::Str("hello".into())],
                &render,
            )
            .expect_err("the service's own refusal comes back");
        assert_eq!(refused.message, "updateKeyboard:fail busy");
        assert_eq!(
            *keyboard.0.lock(),
            vec![
                format!("show {options}"),
                "hide".to_string(),
                "update hello".to_string()
            ]
        );

        let none = ServiceContext::new(root.join("files"), root.join("cache"));
        none.bind_device(None);
        for (op, args, what) in [
            (
                id::op_show_keyboard,
                vec![OwnedValue::Str("{}".into())],
                "showKeyboard",
            ),
            (id::op_hide_keyboard, Vec::new(), "hideKeyboard"),
            (
                id::op_update_keyboard,
                vec![OwnedValue::Str(String::new())],
                "updateKeyboard",
            ),
        ] {
            let error = none.command(op, args, &render).expect_err("no keyboard");
            assert_eq!(error.message, format!("{what}:fail not supported"));
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The compressed-texture query answers the renderer's formats in the
    /// embedded op's bits once the renderer publishes them, and none when the
    /// renderer failed -- the embedded op's answer before caps are set.
    #[test]
    fn compressed_texture_caps_are_the_renderer_s_in_the_embedded_bits() {
        let root = std::env::temp_dir().join(format!("migo-gpu-caps-{}", std::process::id()));
        let answer = |context: &ServiceContext| match context
            .call_sync(id::op_webgl_query_compressed_caps, Vec::new())
        {
            Ok(OwnedValue::U32(bits)) => bits,
            other => panic!("a u32, not {other:?}"),
        };
        for (etc2, astc, bits) in [(true, true, 0b11), (true, false, 0b01), (false, true, 0b10)] {
            let context = ServiceContext::new(root.join("files"), root.join("cache"));
            let caps = shared::device::gpu_caps::GpuCaps::new();
            context.bind_gpu(Arc::clone(&caps), Instant::now());
            // Published after the call begins waiting: the answer waits for it.
            let publisher = std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(20));
                caps.set(etc2, astc, false);
            });
            assert_eq!(answer(&context), bits, "etc2 {etc2} astc {astc}");
            publisher.join().unwrap();
        }
        let failed = ServiceContext::new(root.join("files"), root.join("cache"));
        let caps = shared::device::gpu_caps::GpuCaps::new();
        caps.set_failed("no GL");
        failed.bind_gpu(caps, Instant::now());
        assert_eq!(answer(&failed), 0);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The device commands and reads reach the platform's device services with
    /// content's own arguments, and a platform without one is refused in the
    /// embedded op's words -- so `vibrateShort`'s `fail` and `getBatteryInfo`'s
    /// answer are the same on both lanes.
    #[test]
    fn device_calls_reach_the_platform_s_services_or_are_refused_as_in_process() {
        use shared::services::{
            BatteryService, GameLogService, NetworkService, ScreenService, ServiceError,
            VibrationService,
        };
        #[derive(Default)]
        struct Recorded(parking_lot::Mutex<Vec<String>>);
        impl Recorded {
            fn push(&self, what: String) -> Result<(), ServiceError> {
                self.0.lock().push(what);
                Ok(())
            }
        }
        impl VibrationService for Recorded {
            fn vibrate_short(&self, type_: &str) -> Result<(), ServiceError> {
                self.push(format!("short {type_}"))
            }
            fn vibrate_long(&self) -> Result<(), ServiceError> {
                self.push("long".into())
            }
        }
        impl ScreenService for Recorded {
            fn set_keep_screen_on(&self, keep_on: bool) -> Result<(), ServiceError> {
                self.push(format!("keep {keep_on}"))
            }
        }
        impl GameLogService for Recorded {
            fn report_log(&self, log_json: &str) -> Result<(), ServiceError> {
                self.push(format!("log {log_json}"))
            }
        }
        impl NetworkService for Recorded {
            fn start_monitoring(&self) -> Result<(), ServiceError> {
                self.push("monitor".into())
            }
            fn stop_monitoring(&self) -> Result<(), ServiceError> {
                self.push("unmonitor".into())
            }
            fn get_network_type_json(&self) -> Result<String, ServiceError> {
                Ok(r#"{"networkType":"wifi","isConnected":true}"#.into())
            }
        }
        impl BatteryService for Recorded {
            fn get_info_json(&self) -> Result<String, ServiceError> {
                Ok(r#"{"level":"80"}"#.into())
            }
        }

        let root = std::env::temp_dir().join(format!("migo-device-{}", std::process::id()));
        let (sender, _commands) = shared::render_command_sender::CommandSender::new();
        let render = RenderHandles {
            canvas: shared::op_state::CanvasOpState::for_host(sender, 1),
            gpu_caps: shared::device::gpu_caps::GpuCaps::new(),
        };
        let device = Arc::new(Recorded::default());
        let context = ServiceContext::new(root.join("files"), root.join("cache"));
        // A platform whose device services are all this one recorder.
        struct Platform(Arc<Recorded>);
        impl shared::services::SensorServices for Platform {
            fn battery(&self) -> Option<Arc<dyn BatteryService>> {
                Some(Arc::clone(&self.0) as Arc<dyn BatteryService>)
            }
            fn vibration(&self) -> Option<Arc<dyn VibrationService>> {
                Some(Arc::clone(&self.0) as Arc<dyn VibrationService>)
            }
            fn screen(&self) -> Option<Arc<dyn ScreenService>> {
                Some(Arc::clone(&self.0) as Arc<dyn ScreenService>)
            }
        }
        impl shared::services::ConnectivityServices for Platform {
            fn network(&self) -> Option<Arc<dyn NetworkService>> {
                Some(Arc::clone(&self.0) as Arc<dyn NetworkService>)
            }
        }
        impl shared::services::CommerceServices for Platform {
            fn game_log(&self) -> Option<Arc<dyn GameLogService>> {
                Some(Arc::clone(&self.0) as Arc<dyn GameLogService>)
            }
        }
        impl shared::services::MediaServices for Platform {}
        impl shared::services::SystemUtilServices for Platform {}
        context.bind_device(Some(Arc::new(Platform(Arc::clone(&device)))));
        let commands = [
            (id::op_vibrate_short, vec![OwnedValue::Str("heavy".into())]),
            (id::op_vibrate_long, Vec::new()),
            (id::op_set_keep_screen_on, vec![OwnedValue::Bool(true)]),
            (id::op_start_network_monitoring, Vec::new()),
            (id::op_stop_network_monitoring, Vec::new()),
            (id::op_game_log_report, vec![OwnedValue::Str("{}".into())]),
        ];
        for (op, args) in commands.clone() {
            context.command(op, args, &render).expect("applied");
        }
        assert_eq!(
            *device.0.lock(),
            [
                "short heavy",
                "long",
                "keep true",
                "monitor",
                "unmonitor",
                "log {}"
            ]
        );
        let answer = |context: &ServiceContext, op| match context.call_sync(op, Vec::new()) {
            Ok(OwnedValue::Str(json)) => Ok(json),
            Ok(other) => panic!("a JSON string, not {other:?}"),
            Err(error) => Err(error.message),
        };
        assert_eq!(
            answer(&context, id::op_get_battery_info).as_deref(),
            Ok(r#"{"level":"80"}"#)
        );
        assert_eq!(
            answer(&context, id::op_get_network_type).as_deref(),
            Ok(r#"{"networkType":"wifi","isConnected":true}"#)
        );

        let none = ServiceContext::new(root.join("files"), root.join("cache"));
        none.bind_device(None);
        let words = [
            "vibrateShort",
            "vibrateLong",
            "setKeepScreenOn",
            "onNetworkStatusChange",
            "offNetworkStatusChange",
            "gameLog.log",
        ];
        for ((op, args), what) in commands.into_iter().zip(words) {
            let error = none
                .command(op, args, &render)
                .expect_err("no device services");
            assert_eq!(error.message, format!("{what}:fail not supported"));
        }
        assert_eq!(
            answer(&none, id::op_get_battery_info),
            Err("getBatteryInfo:fail not supported".to_string())
        );
        assert_eq!(
            answer(&none, id::op_get_network_type),
            Err("getNetworkType:fail not supported".to_string())
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The producer's network calls, as its lanes and `core-stream.mjs` encode
    /// them, run through this host's dispatch in the order content made them.
    ///
    /// `test/emit-network-calls.mjs write` records a script that builds a
    /// request and aborts it, then carries a `data:` URL the whole way --
    /// built, sent, read through `core.read` in the caller's chunks, closed --
    /// and `read` checks what the producer makes of the answers this writes.
    ///
    /// The handles are the host's. The producer was answered canned ones while
    /// recording, so each answer's canned form is read beside it and every rid
    /// the producer names is mapped onto the handle this host actually issued:
    /// a call that named a handle the host never gave out would name nothing.
    #[test]
    #[ignore = "needs the producer's calls from node; run through scripts/test-performance-plus-engine-contract.sh"]
    fn the_producer_s_network_calls_run_on_the_host() {
        let dir = PathBuf::from(
            std::env::var("MIGO_NETWORK_CALLS_DIR")
                .expect("MIGO_NETWORK_CALLS_DIR names emit-network-calls.mjs's output"),
        );
        let calls: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join("calls.json")).expect("calls.json"),
        )
        .expect("the calls are JSON");

        let root = std::env::temp_dir().join(format!(
            "migo-external-network-calls-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let context = Arc::new(ServiceContext::new(root.join("files"), root.join("cache")));
        context.bind_session(1);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        context.bind_network(
            shared::op_state::NetworkPolicy {
                domain_whitelist: vec!["allowed.example".to_string()],
                enforce_https: true,
            },
            Arc::new(AtomicBool::new(false)),
            runtime.handle().clone(),
        );
        let (sender, _commands) = shared::render_command_sender::CommandSender::new();
        let render = RenderHandles {
            canvas: shared::op_state::CanvasOpState::for_host(sender, 1),
            gpu_caps: shared::device::gpu_caps::GpuCaps::new(),
        };

        /// The handles an answer hands out, in the order the value carries
        /// them: `op_fetch` gives the request and what aborts it, and
        /// `op_fetch_send` gives the body's.
        fn handles_of(name: &str, value: &OwnedValue) -> Vec<u32> {
            let OwnedValue::Array(fields) = value else {
                return Vec::new();
            };
            let taken: &[usize] = match name {
                "op_fetch" => &[0, 1],
                "op_fetch_send" => &[4],
                _ => return Vec::new(),
            };
            taken
                .iter()
                .filter_map(|at| match fields.get(*at) {
                    Some(OwnedValue::U32(id)) => Some(*id),
                    _ => None,
                })
                .collect()
        }

        /// The value a recorded canned answer carried, for the handles it gave.
        fn canned_value(call: &serde_json::Value) -> Option<OwnedValue> {
            let text = call["canned"].as_str()?;
            let bytes = hex_bytes(text);
            let values = read_values(&bytes[4..]).ok()?;
            values.iter().next().map(|value| value.to_owned_value())
        }

        let names_a_handle = |name: &str| {
            matches!(
                name,
                "op_fetch_send" | "core_read" | "core_close" | "core_try_close"
            )
        };

        let mut host_of: HashMap<u32, u32> = HashMap::new();
        let mut answers = Vec::new();
        let mut refusals = Vec::new();
        for call in calls.as_array().expect("a list") {
            let name = call["op"].as_str().expect("an op name");
            let op = crate::runtime::service_ops::ALL
                .iter()
                .find(|(known, _)| *known == name)
                .map(|(_, id)| *id)
                .unwrap_or_else(|| panic!("{name} has no number"));
            let mut args = read_values(&hex_bytes(call["args"].as_str().expect("args")))
                .unwrap_or_else(|error| panic!("{name}: the arguments do not read: {error:?}"))
                .iter()
                .map(|value| value.to_owned_value())
                .collect::<Vec<_>>();
            if names_a_handle(name) {
                let OwnedValue::U32(recorded) = args[0] else {
                    panic!("{name} names a handle");
                };
                args[0] = OwnedValue::U32(*host_of.get(&recorded).unwrap_or_else(|| {
                    panic!("{name} names handle {recorded}, which no answer gave out")
                }));
            }
            let outcome = match call["shape"].as_str() {
                Some("sync") => context.call_sync(op, args),
                Some("async") => match context.call_async(op, args, &render) {
                    Ok(future) => runtime.block_on(future),
                    Err(error) => Err(error),
                },
                Some("command") => context
                    .command(op, args, &render)
                    .map(|()| OwnedValue::Null),
                other => panic!("{name}: shape {other:?}"),
            };
            if let Ok(value) = &outcome
                && let Some(recorded) = canned_value(call)
            {
                let given = handles_of(name, &recorded);
                let issued = handles_of(name, value);
                assert_eq!(
                    given.len(),
                    issued.len(),
                    "{name} answered {issued:?} where the producer was given {given:?}"
                );
                for (recorded, issued) in given.into_iter().zip(issued) {
                    host_of.insert(recorded, issued);
                }
            }
            if let Err(error) = &outcome
                && error.class == CLASS_TYPE_ERROR
            {
                refusals.push(format!("{name}: {}", error.message));
            }
            answers.push(serde_json::json!({
                "op": name,
                "outcome": hex_text(&encode_outcome(outcome)),
            }));
        }
        std::fs::write(
            dir.join("answers.json"),
            serde_json::to_string(&answers).expect("JSON"),
        )
        .expect("write the answers");
        let _ = std::fs::remove_dir_all(&root);
        assert!(
            refusals.is_empty(),
            "the host refused the producer's arguments:\n  {}",
            refusals.join("\n  ")
        );
        println!("ran {} producer network calls on the host", answers.len());
    }

    fn sync_params(op: u32, values: &[OwnedValue]) -> Vec<u8> {
        let mut writer = ValueWriter::over(op.to_le_bytes().to_vec());
        for value in values {
            value.write_to(&mut writer);
        }
        writer.into_bytes()
    }

    /// A synchronous call runs on the thread that made it, once the session
    /// thread gives it its turn -- not on a pool thread of lower priority.
    #[test]
    fn a_synchronous_call_runs_on_its_own_thread_when_its_turn_comes() {
        let (host, mut work) = host();
        host.context.bind_session(1);
        let turns = std::thread::spawn(move || {
            let Some(ServiceWork::Sync { turn }) = work.blocking_recv() else {
                panic!("the call's turn was queued");
            };
            turn.send(()).unwrap();
            std::thread::current().id()
        });
        let caller = std::thread::current().id();
        let answer = host
            .call_sync(
                &sync_params(id::op_access_sync, &[OwnedValue::Str("/user/a".into())]),
                Instant::now() + std::time::Duration::from_secs(10),
            )
            .expect("an answer");
        assert_ne!(turns.join().unwrap(), caller);
        // No content is loaded: the op's own IOError, run here.
        assert_eq!(
            u32::from_le_bytes(answer[..4].try_into().unwrap()),
            OUTCOME_ERROR
        );
    }

    /// A call that gave up waiting for its turn never runs.
    #[test]
    fn a_synchronous_call_that_times_out_waiting_is_not_run() {
        let (host, mut work) = host();
        host.context.bind_session(1);
        let outcome = host.call_sync(
            &sync_params(id::op_access_sync, &[OwnedValue::Str("/user/a".into())]),
            Instant::now() + std::time::Duration::from_millis(20),
        );
        assert_eq!(outcome, Err(SyncError::TimedOut));
        let Ok(ServiceWork::Sync { turn }) = work.try_recv() else {
            panic!("the call was queued");
        };
        assert!(turn.send(()).is_err(), "nobody is waiting to run it");
    }

    /// The host's input, routed and encoded as the external session does it,
    /// written for `test/host-events.test.mjs` -- which delivers it to the
    /// engine's own host bridge and checks what content's listeners see.
    ///
    /// The script ends in a focus loss with a finger, a mouse button, a key
    /// and a composition still held, so the retractions the shared routing
    /// synthesizes are part of what crosses.
    #[test]
    #[ignore = "writes the corpus for node; run through scripts/test-performance-plus-engine-contract.sh"]
    fn the_host_s_input_as_the_producer_receives_it() {
        use shared::payload_pool::PayloadPool;
        use shared::protocol::host_cmd::{
            GamepadButtonState, GamepadState, HostCommand, TouchData, TouchPoint, TouchType,
        };

        let dir = PathBuf::from(
            std::env::var("MIGO_HOST_EVENTS_DIR")
                .expect("MIGO_HOST_EVENTS_DIR names where to write"),
        );
        std::fs::create_dir_all(&dir).expect("the corpus directory");
        let outbox = ServiceOutbox::new(1, Arc::new(WakerSlot::default()));
        let mut sink = super::super::host_events::ServiceEventSink { outbox: &outbox };
        let mut state = super::super::input_state::InputState::default();

        let touches = PayloadPool::new(4);
        let touch = |touch_type, x: f32, y: f32, flags| {
            let mut points = [TouchPoint::default(); 10];
            points[0] = TouchPoint {
                id: 7,
                x,
                y,
                pressure: 0.5,
                flags,
            };
            HostCommand::OnTouch(
                touches
                    .try_insert(TouchData {
                        touch_type,
                        count: 1,
                        points,
                        timestamp_ms: 100,
                    })
                    .expect("a pooled touch"),
            )
        };
        let pads = PayloadPool::new(1);
        let mut pad = GamepadState {
            index: 0,
            axis_count: 2,
            button_count: 1,
            axes: [0.0; shared::protocol::host_cmd::GAMEPAD_MAX_AXES],
            buttons: [GamepadButtonState::default();
                shared::protocol::host_cmd::GAMEPAD_MAX_BUTTONS],
            timestamp_ms: 42.0,
        };
        pad.axes[0] = 0.5;
        pad.axes[1] = -0.25;
        pad.buttons[0] = GamepadButtonState {
            pressed: true,
            touched: true,
            value: 1.0,
        };
        let script = vec![
            touch(TouchType::Start, 10.0, 20.0, 1),
            touch(TouchType::Move, 11.0, 21.0, 1),
            HostCommand::OnKeyDown {
                key: "a".into(),
                code: "KeyA".into(),
                timestamp_ms: 5.0,
                modifiers: 2,
                repeat: false,
            },
            HostCommand::OnMouseDown {
                x: 1.5,
                y: 2.5,
                button: 0,
                timestamp_ms: 6.0,
            },
            HostCommand::OnWheel {
                delta_x: 1.0,
                delta_y: -2.0,
                delta_z: 0.0,
                delta_mode: 1,
                timestamp_ms: 7.0,
            },
            HostCommand::OnKeyboardInput {
                value: "h\u{e9}llo".into(),
                runtime_generation: None,
            },
            HostCommand::OnCompositionStart { data: "ni".into() },
            HostCommand::OnGamepadConnected {
                index: 0,
                id: "pad".into(),
                mapping: "standard".into(),
                axis_count: 2,
                button_count: 1,
            },
            HostCommand::OnGamepadState(pads.try_insert(pad).expect("a pooled sample")),
            HostCommand::OnFocusChanged { focused: false },
        ];
        for command in script {
            assert!(
                crate::runtime::input_route::route(&mut state, &mut sink, command).is_none(),
                "every scripted command is input"
            );
        }
        let mut written = 0;
        while let Some(message) = outbox.take_message() {
            std::fs::write(dir.join(format!("events-{written:03}.bin")), message)
                .expect("write a message");
            written += 1;
        }
        assert!(written > 0);
        println!("wrote {written} host-event messages");
    }
}

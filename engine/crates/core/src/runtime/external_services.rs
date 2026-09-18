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
//!   started, a command is applied, a synchronous call is run. Started in order
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
use shared::error::{EngineError, EngineResult, ErrorCode};

use super::service_ops::{self, id};

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

/// The class a malformed call is thrown as, matching what `deno_core` throws
/// when an op is handed an argument of the wrong type.
const CLASS_TYPE_ERROR: &str = "TypeError";

/// Work for the session thread, in admission order.
pub(crate) enum ServiceWork {
    /// The records of one admitted message.
    Records(Vec<OwnedServiceRecord>),
    /// A synchronous call, answered on `reply`.
    Sync {
        op: u32,
        args: Vec<OwnedValue>,
        reply: std::sync::mpsc::SyncSender<Result<OwnedValue, ServiceError>>,
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

    /// Tell content something no request asked for.
    #[allow(dead_code)] // The first events (input, sockets) land with their services.
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
}

impl ServiceContext {
    fn new(files_dir: PathBuf, cache_dir: PathBuf) -> Self {
        Self {
            files_dir,
            cache_dir,
            scheduler: OnceLock::new(),
            session_id: OnceLock::new(),
            content: RwLock::new(None),
        }
    }

    pub(crate) fn bind_session(&self, session_id: i32) {
        let _ = self.session_id.set(session_id);
        let _ = self.scheduler.set(Arc::new(IoScheduler::new(session_id)));
    }

    fn scheduler(&self) -> Result<Arc<IoScheduler>, ServiceError> {
        self.scheduler
            .get()
            .cloned()
            .ok_or_else(|| ServiceError::generic("the session's services are not started"))
    }

    fn game_paths(&self) -> Option<Arc<shared::vfs::GamePaths>> {
        self.content
            .read()
            .as_ref()
            .map(|content| Arc::clone(&content.game_paths))
    }

    /// Mount the content the host named: `/code` from the installed package,
    /// and the game's `/user`, `/cache` and `/tmp`. Once per session.
    pub(crate) fn load_content(&self, game_id: &str) -> EngineResult<PathBuf> {
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
        let mounted = MountedContent::mount(
            &self.files_dir,
            &self.cache_dir,
            game_id,
            session_id,
            &scheduler,
        )?;
        let root = mounted.game_paths.code_dir().to_path_buf();
        info!(
            "[Host {session_id}] content '{game_id}' mounted at {}",
            root.display()
        );
        *content = Some(mounted);
        Ok(root)
    }

    pub(crate) fn content_root(&self) -> Option<PathBuf> {
        self.content
            .read()
            .as_ref()
            .map(|content| content.game_paths.code_dir().to_path_buf())
    }

    /// Run a synchronous op. Called off the session thread (see
    /// [`ServiceDispatcher`]), because these block on file and database work.
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
            id::op_revoke_buffer_url => {
                let [url] = exactly(op, args)?;
                migo_services::storage::revoke_buffer_url(paths, &string(op, 0, url)?)
                    .map(|()| OwnedValue::Null)
            }
            other => Err(not_a(other, "synchronous")),
        }
    }

    /// Start an awaited op. The future owns what it needs and runs on the
    /// session thread's runtime.
    fn call_async(
        &self,
        op: u32,
        args: Vec<OwnedValue>,
    ) -> Result<BoxFuture<'static, Result<OwnedValue, ServiceError>>, ServiceError> {
        let paths = self.game_paths();
        Ok(match op {
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
            other => return Err(not_a(other, "awaited")),
        })
    }

    /// Apply a command. Nothing answers it, so a failure is logged -- which is
    /// what the embedded runtime does with a failed fire-and-forget op too.
    fn command(&self, op: u32, _args: Vec<OwnedValue>) -> Result<(), ServiceError> {
        Err(not_a(op, "command"))
    }
}

/// The error for an op this host does not run in that shape.
fn not_a(op: u32, shape: &str) -> ServiceError {
    ServiceError::classed(
        CLASS_TYPE_ERROR,
        match service_ops::name_of(op) {
            Some(name) => format!("{name} is not a {shape} service op"),
            None => format!("service op {op} is not one this host knows"),
        },
    )
}

fn exactly<const N: usize>(
    op: u32,
    args: Vec<OwnedValue>,
) -> Result<[OwnedValue; N], ServiceError> {
    let count = args.len();
    args.try_into().map_err(|_| {
        ServiceError::classed(
            CLASS_TYPE_ERROR,
            format!(
                "{} takes {N} argument(s), not {count}",
                service_ops::name_of(op).unwrap_or("this op")
            ),
        )
    })
}

fn wrong_type(op: u32, index: usize, wanted: &str, got: &OwnedValue) -> ServiceError {
    ServiceError::classed(
        CLASS_TYPE_ERROR,
        format!(
            "argument {index} of {} is a {wanted}, not a {}",
            service_ops::name_of(op).unwrap_or("this op"),
            got.kind_name()
        ),
    )
}

fn string(op: u32, index: usize, value: OwnedValue) -> Result<String, ServiceError> {
    match value {
        OwnedValue::Str(text) => Ok(text),
        other => Err(wrong_type(op, index, "string", &other)),
    }
}

fn bytes(op: u32, index: usize, value: OwnedValue) -> Result<Vec<u8>, ServiceError> {
    match value {
        OwnedValue::Bytes(bytes) => Ok(bytes),
        other => Err(wrong_type(op, index, "bytes", &other)),
    }
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
        let (reply, answer) = std::sync::mpsc::sync_channel(1);
        self.work
            .blocking_send(ServiceWork::Sync { op, args, reply })
            .map_err(|_| SyncError::SessionEnded)?;
        let outcome = answer
            .recv_timeout(until.saturating_duration_since(Instant::now()))
            .map_err(|error| match error {
                std::sync::mpsc::RecvTimeoutError::Timeout => SyncError::TimedOut,
                std::sync::mpsc::RecvTimeoutError::Disconnected => SyncError::SessionEnded,
            })?;
        Ok(encode_outcome(outcome))
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
}

impl ServiceDispatcher {
    pub(crate) fn new(host: &ServiceHost) -> Self {
        Self {
            context: Arc::clone(&host.context),
            outbox: Arc::clone(&host.outbox),
            in_flight: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Start one piece of work. Must run inside the session's Tokio runtime.
    pub(crate) fn dispatch(&self, work: ServiceWork) {
        match work {
            ServiceWork::Records(records) => {
                for record in records {
                    self.record(record);
                }
            }
            ServiceWork::Sync { op, args, reply } => {
                // Off the session thread: these block on file and database
                // work, and the session thread is the one that forwards frame
                // clock ticks. Dispatched in order is what the queue buys;
                // blocking the queue for the length of a disk read is not.
                let context = Arc::clone(&self.context);
                tokio::task::spawn_blocking(move || {
                    // A caller that timed out has dropped the receiver; the
                    // answer has nowhere to go and is dropped with it.
                    let _ = reply.send(context.call_sync(op, args));
                });
            }
        }
    }

    fn record(&self, record: OwnedServiceRecord) {
        match record {
            OwnedServiceRecord::Request {
                request_id,
                op,
                args,
            } => match self.context.call_async(op, args) {
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
                if let Err(error) = self.context.command(op, args) {
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
        let code = context.load_content("g").expect("installed content mounts");
        assert_eq!(code, installed.code_dir());
        assert!(context.load_content("g").is_err(), "once per session");

        context
            .call_sync(
                id::op_storage_set,
                vec![OwnedValue::Str("k".into()), OwnedValue::Str("v".into())],
            )
            .expect("set");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let read = runtime
            .block_on(
                context
                    .call_async(id::op_storage_get_async, vec![OwnedValue::Str("k".into())])
                    .expect("a known async op"),
            )
            .expect("get");
        assert_eq!(read, OwnedValue::Str("v".into()));
        let _ = std::fs::remove_dir_all(&root);
    }
}

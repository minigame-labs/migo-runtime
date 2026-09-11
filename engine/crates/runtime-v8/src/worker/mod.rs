use std::{cell::RefCell, future::Future, rc::Rc, sync::Arc, time::Duration};

use deno_core::{
    Extension, ExtensionArguments, FsModuleLoader, JsBuffer, JsRuntime, ModuleLoader, OpState,
    PollEventLoopOptions, RuntimeOptions, SharedArrayBufferStore, op2, resolve_path, v8,
};
use tokio::sync::mpsc;
use tracing::{error, info, warn};

use shared::op_state::HostOpState;

use crate::watchdog::{DeadlineWatchdog, DeadlineWatchdogConfig};

/// Maximum size for a single worker message payload (16 MB).
/// Prevents large messages from bypassing V8 heap limits.
const MAX_WORKER_MESSAGE_BYTES: usize = 16 * 1024 * 1024;

/// User messages allowed to wait in either Worker direction.
const WORKER_MESSAGE_QUEUE_CAPACITY: usize = 64;
/// Physical slots unavailable to user messages and reserved for termination.
const WORKER_CONTROL_RESERVE: usize = 1;
/// Aggregate queued payload budget per Worker message direction.
const MAX_WORKER_QUEUED_BYTES: usize = 64 * 1024 * 1024;
const WORKER_ERROR_QUEUE_CAPACITY: usize = 16;
const MAX_WORKER_ERROR_QUEUED_BYTES: usize = 1024 * 1024;

const WORKER_MESSAGE_QUEUE_FULL: &str = "Worker message queue full";
const WORKER_MESSAGE_BYTE_LIMIT: &str = "Worker message queue byte limit exceeded";

trait WorkerQueuePayload {
    fn queued_bytes(&self) -> usize;
}

struct WorkerQueueUsage {
    max_items: usize,
    max_bytes: usize,
    closed: std::sync::atomic::AtomicBool,
    items: std::sync::atomic::AtomicUsize,
    bytes: std::sync::atomic::AtomicUsize,
    rejected: std::sync::atomic::AtomicUsize,
}

#[derive(Debug)]
enum WorkerReserveError {
    Full,
    ByteLimit,
    Closed,
}

impl WorkerQueueUsage {
    fn try_reserve(
        self: &Arc<Self>,
        bytes: usize,
    ) -> Result<WorkerQueuePermit, WorkerReserveError> {
        self.items
            .fetch_update(
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
                |items| (items < self.max_items).then_some(items + 1),
            )
            .map_err(|_| WorkerReserveError::Full)?;

        let reserved_bytes = self.bytes.fetch_update(
            std::sync::atomic::Ordering::AcqRel,
            std::sync::atomic::Ordering::Acquire,
            |used| {
                used.checked_add(bytes)
                    .filter(|total| *total <= self.max_bytes)
            },
        );
        if reserved_bytes.is_err() {
            self.items.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
            return Err(WorkerReserveError::ByteLimit);
        }

        Ok(WorkerQueuePermit {
            usage: Arc::clone(self),
            bytes,
        })
    }
}

struct WorkerQueuePermit {
    usage: Arc<WorkerQueueUsage>,
    bytes: usize,
}

impl Drop for WorkerQueuePermit {
    fn drop(&mut self) {
        self.usage
            .bytes
            .fetch_sub(self.bytes, std::sync::atomic::Ordering::AcqRel);
        self.usage
            .items
            .fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
    }
}

struct WorkerQueueEntry<T> {
    value: T,
    _permit: Option<WorkerQueuePermit>,
}

impl<T> WorkerQueueEntry<T> {
    fn into_value(self) -> T {
        let Self { value, _permit } = self;
        drop(_permit);
        value
    }
}

struct WorkerQueueSender<T> {
    tx: mpsc::Sender<WorkerQueueEntry<T>>,
    usage: Arc<WorkerQueueUsage>,
}

impl<T> Clone for WorkerQueueSender<T> {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
            usage: Arc::clone(&self.usage),
        }
    }
}

struct WorkerQueueReceiver<T> {
    rx: mpsc::Receiver<WorkerQueueEntry<T>>,
    usage: Arc<WorkerQueueUsage>,
}

impl<T> Drop for WorkerQueueReceiver<T> {
    fn drop(&mut self) {
        self.usage
            .closed
            .store(true, std::sync::atomic::Ordering::Release);
    }
}

#[derive(Debug)]
enum WorkerQueueSendError<T> {
    Full(T),
    ByteLimit(T),
    Closed(T),
}

impl<T: WorkerQueuePayload> WorkerQueueSender<T> {
    /// Reserve a data slot and its queued byte budget before copying a V8
    /// backing store into a worker message.
    fn try_reserve(&self, bytes: usize) -> Result<WorkerQueuePermit, WorkerReserveError> {
        if self.usage.closed.load(std::sync::atomic::Ordering::Acquire) {
            self.usage
                .rejected
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return Err(WorkerReserveError::Closed);
        }
        match self.usage.try_reserve(bytes) {
            Ok(permit) => {
                if self.usage.closed.load(std::sync::atomic::Ordering::Acquire) {
                    drop(permit);
                    self.usage
                        .rejected
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    Err(WorkerReserveError::Closed)
                } else {
                    Ok(permit)
                }
            }
            Err(WorkerReserveError::Full) => {
                self.usage
                    .rejected
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Err(WorkerReserveError::Full)
            }
            Err(WorkerReserveError::ByteLimit) => {
                self.usage
                    .rejected
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Err(WorkerReserveError::ByteLimit)
            }
            Err(WorkerReserveError::Closed) => {
                self.usage
                    .rejected
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Err(WorkerReserveError::Closed)
            }
        }
    }

    fn try_send_entry(&self, entry: WorkerQueueEntry<T>) -> Result<(), WorkerQueueSendError<T>> {
        match self.tx.try_send(entry) {
            Ok(()) => Ok(()),
            Err(error) => {
                self.usage
                    .rejected
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Err(match error {
                    mpsc::error::TrySendError::Full(entry) => {
                        WorkerQueueSendError::Full(entry.into_value())
                    }
                    mpsc::error::TrySendError::Closed(entry) => {
                        WorkerQueueSendError::Closed(entry.into_value())
                    }
                })
            }
        }
    }

    fn send_reserved(
        &self,
        value: T,
        permit: WorkerQueuePermit,
    ) -> Result<(), WorkerQueueSendError<T>> {
        if value.queued_bytes() > permit.bytes {
            drop(permit);
            return Err(WorkerQueueSendError::ByteLimit(value));
        }
        let entry = WorkerQueueEntry {
            value,
            _permit: Some(permit),
        };
        self.try_send_entry(entry)
    }

    fn send(&self, value: T) -> Result<(), WorkerQueueSendError<T>> {
        let permit = match self.try_reserve(value.queued_bytes()) {
            Ok(permit) => permit,
            Err(WorkerReserveError::Full) => return Err(WorkerQueueSendError::Full(value)),
            Err(WorkerReserveError::ByteLimit) => {
                return Err(WorkerQueueSendError::ByteLimit(value));
            }
            Err(WorkerReserveError::Closed) => return Err(WorkerQueueSendError::Closed(value)),
        };
        self.send_reserved(value, permit)
    }
}

impl<T> WorkerQueueReceiver<T> {
    async fn recv(&mut self) -> Option<T> {
        self.rx.recv().await.map(WorkerQueueEntry::into_value)
    }

    #[cfg(test)]
    fn try_recv(&mut self) -> Result<T, mpsc::error::TryRecvError> {
        self.rx.try_recv().map(WorkerQueueEntry::into_value)
    }
}

fn worker_queue<T>(
    item_capacity: usize,
    byte_capacity: usize,
) -> (WorkerQueueSender<T>, WorkerQueueReceiver<T>) {
    let (tx, rx) = mpsc::channel(item_capacity);
    let usage = Arc::new(WorkerQueueUsage {
        max_items: item_capacity,
        max_bytes: byte_capacity,
        closed: std::sync::atomic::AtomicBool::new(false),
        items: std::sync::atomic::AtomicUsize::new(0),
        bytes: std::sync::atomic::AtomicUsize::new(0),
        rejected: std::sync::atomic::AtomicUsize::new(0),
    });
    (
        WorkerQueueSender {
            tx,
            usage: Arc::clone(&usage),
        },
        WorkerQueueReceiver { rx, usage },
    )
}

struct WorkerMessageSender {
    data: WorkerQueueSender<WorkerMessage>,
    control: mpsc::Sender<WorkerMessage>,
}

impl Clone for WorkerMessageSender {
    fn clone(&self) -> Self {
        Self {
            data: self.data.clone(),
            control: self.control.clone(),
        }
    }
}

impl WorkerMessageSender {
    #[cfg(test)]
    fn send(&self, value: WorkerMessage) -> Result<(), WorkerQueueSendError<WorkerMessage>> {
        self.data.send(value)
    }

    fn try_reserve(&self, bytes: usize) -> Result<WorkerQueuePermit, WorkerReserveError> {
        self.data.try_reserve(bytes)
    }

    fn send_reserved(
        &self,
        value: WorkerMessage,
        permit: WorkerQueuePermit,
    ) -> Result<(), WorkerQueueSendError<WorkerMessage>> {
        self.data.send_reserved(value, permit)
    }

    fn send_control(
        &self,
        value: WorkerMessage,
    ) -> Result<(), WorkerQueueSendError<WorkerMessage>> {
        debug_assert!(matches!(&value, WorkerMessage::Terminate));
        self.control.try_send(value).map_err(|error| match error {
            mpsc::error::TrySendError::Full(value) => WorkerQueueSendError::Full(value),
            mpsc::error::TrySendError::Closed(value) => WorkerQueueSendError::Closed(value),
        })
    }
}

struct WorkerMessageReceiver {
    data: WorkerQueueReceiver<WorkerMessage>,
    control: mpsc::Receiver<WorkerMessage>,
}

impl WorkerMessageReceiver {
    async fn recv(&mut self) -> Option<WorkerMessage> {
        if let Ok(control) = self.control.try_recv() {
            return Some(control);
        }
        tokio::select! {
            biased;
            control = self.control.recv() => match control {
                Some(control) => Some(control),
                None => self.data.recv().await,
            },
            data = self.data.recv() => data,
        }
    }

    #[cfg(test)]
    fn try_recv(&mut self) -> Result<WorkerMessage, mpsc::error::TryRecvError> {
        match self.control.try_recv() {
            Ok(control) => Ok(control),
            Err(mpsc::error::TryRecvError::Empty)
            | Err(mpsc::error::TryRecvError::Disconnected) => self.data.try_recv(),
        }
    }
}

fn worker_message_channel() -> (WorkerMessageSender, WorkerMessageReceiver) {
    let (data, data_rx) = worker_queue(WORKER_MESSAGE_QUEUE_CAPACITY, MAX_WORKER_QUEUED_BYTES);
    let (control, control_rx) = mpsc::channel(WORKER_CONTROL_RESERVE);
    (
        WorkerMessageSender { data, control },
        WorkerMessageReceiver {
            data: data_rx,
            control: control_rx,
        },
    )
}

fn worker_outbound_channel() -> (WorkerQueueSender<String>, WorkerQueueReceiver<String>) {
    worker_queue(WORKER_MESSAGE_QUEUE_CAPACITY, MAX_WORKER_QUEUED_BYTES)
}

fn worker_error_channel() -> (WorkerQueueSender<String>, WorkerQueueReceiver<String>) {
    worker_queue(WORKER_ERROR_QUEUE_CAPACITY, MAX_WORKER_ERROR_QUEUED_BYTES)
}

fn worker_lifecycle_channel() -> (WorkerLifecycleSender, WorkerLifecycleReceiver) {
    let (tx, rx) = tokio::sync::watch::channel(None);
    (
        WorkerLifecycleSender { tx },
        WorkerLifecycleReceiver {
            rx,
            delivered_background: Duration::ZERO,
            delivered_state: None,
            delivered_transition_at: None,
            pending: [None, None],
        },
    )
}

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Messages flowing from the main thread to the worker thread.
#[derive(Debug)]
pub(crate) enum WorkerMessage {
    /// A postMessage payload (JSON-serialized string).
    Message(String),
    /// A binary payload copied from the sender's ArrayBuffer backing store.
    Binary(Vec<u8>),
    /// Terminate the worker.
    Terminate,
}

impl WorkerQueuePayload for WorkerMessage {
    fn queued_bytes(&self) -> usize {
        match self {
            WorkerMessage::Message(message) => message.len(),
            WorkerMessage::Binary(data) => {
                data.len().saturating_add(base64_encoded_len(data.len()))
            }
            WorkerMessage::Terminate => 0,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct WorkerTimerLifecycleTransition {
    backgrounded: bool,
    occurred_at: tokio::time::Instant,
}

impl WorkerTimerLifecycleTransition {
    fn now(backgrounded: bool) -> Self {
        Self {
            backgrounded,
            occurred_at: tokio::time::Instant::now(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct WorkerTimerLifecycleSnapshot {
    current: WorkerTimerLifecycleTransition,
    completed_background: Duration,
    active_background_started_at: Option<tokio::time::Instant>,
}

struct WorkerLifecycleSender {
    tx: tokio::sync::watch::Sender<Option<WorkerTimerLifecycleSnapshot>>,
}

impl WorkerLifecycleSender {
    fn send(&self, transition: WorkerTimerLifecycleTransition) {
        let _ = self.tx.send_if_modified(|slot| match slot {
            Some(snapshot) if snapshot.current.backgrounded == transition.backgrounded => false,
            Some(snapshot) => {
                if snapshot.current.backgrounded {
                    let started_at = snapshot
                        .active_background_started_at
                        .take()
                        .unwrap_or(snapshot.current.occurred_at);
                    snapshot.completed_background = snapshot.completed_background.saturating_add(
                        transition.occurred_at.saturating_duration_since(started_at),
                    );
                }
                if transition.backgrounded {
                    snapshot.active_background_started_at = Some(transition.occurred_at);
                }
                snapshot.current = transition;
                true
            }
            None => {
                *slot = Some(WorkerTimerLifecycleSnapshot {
                    current: transition,
                    completed_background: Duration::ZERO,
                    active_background_started_at: transition
                        .backgrounded
                        .then_some(transition.occurred_at),
                });
                true
            }
        });
    }
}

struct WorkerLifecycleReceiver {
    rx: tokio::sync::watch::Receiver<Option<WorkerTimerLifecycleSnapshot>>,
    delivered_background: Duration,
    delivered_state: Option<bool>,
    delivered_transition_at: Option<tokio::time::Instant>,
    pending: [Option<WorkerTimerLifecycleTransition>; 2],
}

impl WorkerLifecycleReceiver {
    fn pop_pending(&mut self) -> Option<WorkerTimerLifecycleTransition> {
        let transition = self.pending[0].take();
        if transition.is_some() {
            self.pending[0] = self.pending[1].take();
        }
        transition
    }

    fn transition_from_snapshot(
        &mut self,
        snapshot: WorkerTimerLifecycleSnapshot,
    ) -> Option<WorkerTimerLifecycleTransition> {
        let completed_background = snapshot
            .completed_background
            .saturating_sub(self.delivered_background);
        let previous_state = self.delivered_state;
        let previous_transition_at = self.delivered_transition_at;
        self.delivered_background = snapshot.completed_background;
        self.delivered_state = Some(snapshot.current.backgrounded);
        self.delivered_transition_at = Some(snapshot.current.occurred_at);
        self.pending = [None, None];

        if previous_state == Some(true) {
            let wall_time = previous_transition_at
                .map(|occurred_at| {
                    snapshot
                        .current
                        .occurred_at
                        .saturating_duration_since(occurred_at)
                })
                .unwrap_or_default();
            let foreground_time = wall_time.saturating_sub(completed_background);
            if foreground_time.is_zero() && snapshot.current.backgrounded {
                return None;
            }

            // The JS timer pump is already frozen. Resume it far enough in the
            // past to account for the aggregate foreground time that occurred
            // between coalesced background intervals, then optionally re-freeze
            // at the latest hide edge.
            let resumed_at = snapshot
                .current
                .occurred_at
                .checked_sub(foreground_time)
                .unwrap_or(snapshot.current.occurred_at);
            if snapshot.current.backgrounded {
                self.pending[0] = Some(snapshot.current);
            }
            return Some(WorkerTimerLifecycleTransition {
                backgrounded: false,
                occurred_at: resumed_at,
            });
        }

        if !completed_background.is_zero() {
            // A watch channel intentionally coalesces bursts. Reconstruct the
            // completed background time immediately before the latest state
            // transition so the timer pump sees the exact aggregate frozen
            // duration while the transport remains O(1).
            let completed_at = snapshot.current.occurred_at;
            let completed_started_at = completed_at
                .checked_sub(completed_background)
                .unwrap_or(completed_at);
            self.pending[0] = Some(WorkerTimerLifecycleTransition {
                backgrounded: false,
                occurred_at: completed_at,
            });
            if snapshot.current.backgrounded {
                self.pending[1] = Some(snapshot.current);
            }
            return Some(WorkerTimerLifecycleTransition {
                backgrounded: true,
                occurred_at: completed_started_at,
            });
        }

        if previous_state == Some(snapshot.current.backgrounded) {
            None
        } else {
            Some(snapshot.current)
        }
    }

    fn try_recv(&mut self) -> Option<WorkerTimerLifecycleTransition> {
        loop {
            if let Some(transition) = self.pop_pending() {
                return Some(transition);
            }
            if !self.rx.has_changed().unwrap_or(false) {
                return None;
            }
            let snapshot = *self.rx.borrow_and_update();
            if let Some(transition) =
                snapshot.and_then(|snapshot| self.transition_from_snapshot(snapshot))
            {
                return Some(transition);
            }
        }
    }

    async fn recv(
        &mut self,
    ) -> Result<WorkerTimerLifecycleTransition, tokio::sync::watch::error::RecvError> {
        if let Some(transition) = self.pop_pending() {
            return Ok(transition);
        }
        loop {
            self.rx.changed().await?;
            let snapshot = *self.rx.borrow_and_update();
            if let Some(snapshot) = snapshot {
                if let Some(transition) = self.transition_from_snapshot(snapshot) {
                    return Ok(transition);
                }
            }
        }
    }
}

impl WorkerQueuePayload for String {
    fn queued_bytes(&self) -> usize {
        self.len()
    }
}

/// Typed events delivered only to the worker's internal message pump.
#[derive(Debug, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub(crate) enum WorkerInbound {
    Message {
        data: String,
    },
    Lifecycle {
        backgrounded: bool,
        #[serde(rename = "elapsedMicros")]
        elapsed_micros: u64,
    },
}

/// Stored in the **main** thread's `OpState` when a worker is active.
pub(crate) struct WorkerHandle {
    tx_to_worker: WorkerMessageSender,
    rx_from_worker: Arc<tokio::sync::Mutex<WorkerQueueReceiver<String>>>,
    rx_errors: Arc<tokio::sync::Mutex<WorkerQueueReceiver<String>>>,
    timer_backgrounded_tx: WorkerLifecycleSender,
    join_handle: Option<std::thread::JoinHandle<()>>,
    terminated: bool,
    /// Thread-safe handle to the worker's V8 isolate, published by the worker
    /// thread right after it builds its `JsRuntime`. Used to *forcibly* stop a
    /// runaway worker: the cooperative `Terminate` message is only observed
    /// when the worker is awaiting `op_worker_inner_recv_message`, so a
    /// compute-bound `while (true) {}` would otherwise never exit. Wrapped in a
    /// `Mutex<Option<..>>` because it is filled asynchronously (the worker may
    /// not have created the isolate yet when this handle is stored).
    isolate_handle: Arc<std::sync::Mutex<Option<v8::IsolateHandle>>>,
}

impl WorkerHandle {
    /// Force the worker to stop: interrupt any executing JS via the isolate
    /// handle (breaks a runaway loop that ignores the cooperative `Terminate`)
    /// and signal the message pump to exit. Safe to call more than once and
    /// after the worker isolate has already been disposed.
    fn force_terminate(&self) {
        if let Ok(guard) = self.isolate_handle.lock() {
            if let Some(h) = guard.as_ref() {
                h.terminate_execution();
            }
        }
        let _ = self.tx_to_worker.send_control(WorkerMessage::Terminate);
    }
}

impl Drop for WorkerHandle {
    fn drop(&mut self) {
        // Runtime drop is the ownership boundary for this nested V8 isolate.
        // Interrupt first, then join: otherwise the Host worker can return to
        // Engine teardown while this OS thread still retains V8, queues, and
        // host state from the retired runtime generation.
        self.force_terminate();
        if let Some(worker) = self.join_handle.take()
            && worker.join().is_err()
        {
            error!("[Worker] worker thread panicked during teardown");
        }
    }
}

/// Stored in the **worker** thread's `OpState`.
pub(crate) struct WorkerCtx {
    tx_to_main: WorkerQueueSender<String>,
    tx_errors: WorkerQueueSender<String>,
    rx_from_main: Arc<tokio::sync::Mutex<WorkerMessageReceiver>>,
    timer_backgrounded_rx: Arc<tokio::sync::Mutex<WorkerLifecycleReceiver>>,
}

// ---------------------------------------------------------------------------
// Error
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error, deno_error::JsError)]
pub enum WorkerError {
    #[class("WorkerError")]
    #[error("{0}")]
    Message(String),
}

fn worker_queue_error<T>(error: WorkerQueueSendError<T>, closed: &'static str) -> WorkerError {
    let message = match error {
        WorkerQueueSendError::Full(_) => {
            format!("{WORKER_MESSAGE_QUEUE_FULL} (max {WORKER_MESSAGE_QUEUE_CAPACITY} messages)")
        }
        WorkerQueueSendError::ByteLimit(_) => {
            format!("{WORKER_MESSAGE_BYTE_LIMIT} (max {MAX_WORKER_QUEUED_BYTES} bytes)")
        }
        WorkerQueueSendError::Closed(_) => closed.to_string(),
    };
    WorkerError::Message(message)
}

fn worker_reserve_error(error: WorkerReserveError) -> WorkerError {
    let message = match error {
        WorkerReserveError::Full => {
            format!("{WORKER_MESSAGE_QUEUE_FULL} (max {WORKER_MESSAGE_QUEUE_CAPACITY} messages)")
        }
        WorkerReserveError::ByteLimit => {
            format!("{WORKER_MESSAGE_BYTE_LIMIT} (max {MAX_WORKER_QUEUED_BYTES} bytes)")
        }
        WorkerReserveError::Closed => "Worker channel closed".to_string(),
    };
    WorkerError::Message(message)
}

// ---------------------------------------------------------------------------
// Module loader (reuse the same logic from core::runtime::loader)
// ---------------------------------------------------------------------------

/// Lightweight module loader for the worker thread.
/// Mirrors `MyModuleLoader` from `core::runtime::loader` — auto-adds `.js`,
/// patches AMD `define` modules, and enforces the `/code` sandbox via
/// the mount table (same security boundary as the main thread loader).
struct WorkerModuleLoader {
    inner: FsModuleLoader,
    mount_table: Option<Arc<shared::vfs::MountTable>>,
}

impl WorkerModuleLoader {
    /// Validate that a resolved module URL is within the /code sandbox.
    fn validate_sandbox(
        &self,
        url: &deno_core::ModuleSpecifier,
    ) -> Result<(), deno_core::error::ModuleLoaderError> {
        crate::loader::validate_content_module_url(
            url,
            self.mount_table.as_deref(),
            "Worker content",
        )
    }
}

impl deno_core::ModuleLoader for WorkerModuleLoader {
    fn resolve(
        &self,
        specifier: &str,
        referrer: &str,
        kind: deno_core::ResolutionKind,
    ) -> Result<deno_core::ModuleSpecifier, deno_core::error::ModuleLoaderError> {
        let spec = normalize_specifier(specifier, &kind);
        let url = self.inner.resolve(spec.as_ref(), referrer, kind)?;
        self.validate_sandbox(&url)?;
        Ok(url)
    }

    fn load(
        &self,
        module_specifier: &deno_core::ModuleSpecifier,
        maybe_referrer: Option<&deno_core::ModuleLoadReferrer>,
        options: deno_core::ModuleLoadOptions,
    ) -> deno_core::ModuleLoadResponse {
        // Defense in depth: validate on load too.
        if let Err(e) = self.validate_sandbox(module_specifier) {
            return deno_core::ModuleLoadResponse::Sync(Err(e));
        }
        let resp = self.inner.load(module_specifier, maybe_referrer, options);
        match resp {
            deno_core::ModuleLoadResponse::Sync(result) => {
                deno_core::ModuleLoadResponse::Sync(result.and_then(patch_amd))
            }
            deno_core::ModuleLoadResponse::Async(fut) => {
                let fut = async move {
                    let source = fut.await?;
                    patch_amd(source)
                };
                deno_core::ModuleLoadResponse::Async(Box::pin(fut))
            }
        }
    }

    fn prepare_load(
        &self,
        module_specifier: &deno_core::ModuleSpecifier,
        maybe_referrer: Option<String>,
        maybe_content: Option<String>,
        options: deno_core::ModuleLoadOptions,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<(), deno_core::error::ModuleLoaderError>>>,
    > {
        self.inner
            .prepare_load(module_specifier, maybe_referrer, maybe_content, options)
    }

    fn finish_load(&self) {}

    fn code_cache_ready(
        &self,
        module_specifier: deno_core::ModuleSpecifier,
        hash: u64,
        code_cache: &[u8],
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()>>> {
        self.inner
            .code_cache_ready(module_specifier, hash, code_cache)
    }
}

fn normalize_specifier<'a>(
    specifier: &'a str,
    kind: &deno_core::ResolutionKind,
) -> std::borrow::Cow<'a, str> {
    use std::borrow::Cow;

    let mut s: Cow<'a, str> = if *kind != deno_core::ResolutionKind::MainModule {
        if specifier.starts_with("./") || specifier.starts_with("../") || specifier.contains(':') {
            Cow::Borrowed(specifier)
        } else {
            Cow::Owned(format!("./{specifier}"))
        }
    } else {
        Cow::Borrowed(specifier)
    };

    let (path_part, suffix_part) = match s.find(['?', '#']) {
        Some(i) => (&s.as_ref()[..i], &s.as_ref()[i..]),
        None => (s.as_ref(), ""),
    };

    let has_js_like_ext =
        path_part.ends_with(".js") || path_part.ends_with(".mjs") || path_part.ends_with(".cjs");

    if !has_js_like_ext {
        let new_path = format!("{path_part}.js{suffix_part}");
        s = Cow::Owned(new_path);
    }

    s
}

fn patch_amd(
    mut source: deno_core::ModuleSource,
) -> Result<deno_core::ModuleSource, deno_core::error::ModuleLoaderError> {
    let code = String::from_utf8_lossy(source.code.as_bytes());
    if code.contains("define.amd") || code.contains("typeof define") {
        let mut patched = code.into_owned();
        patched.push_str("\nexport default globalThis._lastDefinedModule;\n");
        source.code = deno_core::ModuleSourceCode::String(patched.into());
    } else if shared::cjs_compat::is_cjs(&code) {
        let patched = shared::cjs_compat::wrap_cjs(&code);
        source.code = deno_core::ModuleSourceCode::String(patched.into());
    }
    Ok(source)
}

#[cfg(test)]
mod module_loader_security_tests {
    use super::*;
    use deno_core::ResolutionKind;

    #[test]
    fn worker_content_loader_rejects_internal_schemes_for_all_import_kinds() {
        let root = std::env::temp_dir().join("migo-worker-loader-sandbox");
        let loader = WorkerModuleLoader {
            inner: FsModuleLoader,
            mount_table: Some(Arc::new(shared::vfs::MountTable::new(root))),
        };
        let referrer = "file:///tmp/migo-worker-loader-sandbox/main.js";

        for kind in [ResolutionKind::Import, ResolutionKind::DynamicImport] {
            let error = loader
                .resolve("ext:core/mod.js", referrer, kind)
                .expect_err("worker content must not resolve runtime extension modules");
            assert!(
                error.to_string().contains("file"),
                "unexpected rejection: {error}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Main-thread ops (registered in `host_v8_worker`)
// ---------------------------------------------------------------------------

/// What a Worker inherits from its parent so their asynchronous results stay
/// distinguishable: one callback-id space, and the generation that created it.
///
/// A function rather than two lines inside the op, because those two lines are
/// the whole property and an op is not reachable from a unit test. Production
/// calls this — it is the only implementation, not a restatement beside one.
fn inherited_correlation(
    host: &HostOpState,
) -> (
    std::sync::Arc<shared::callback_id::CallbackIdAllocator>,
    i64,
) {
    (
        std::sync::Arc::clone(&host.callback_ids),
        host.runtime_generation,
    )
}

/// Create and spawn a worker thread. Only one worker can exist at a time.
#[op2(async(lazy), fast)]
async fn op_worker_create(
    state: Rc<RefCell<OpState>>,
    #[string] script_path: String,
) -> Result<(), WorkerError> {
    // Check existing worker. A previously terminated worker whose thread has
    // fully exited is reaped here so a new one can be created; but we refuse
    // while any worker is still alive OR still winding down after terminate()
    // (its thread not yet finished), so an old and new worker never coexist.
    {
        let needs_reap = {
            let st = state.borrow();
            match st.try_borrow::<WorkerHandle>() {
                None => false,
                Some(h) => {
                    let finished = h.join_handle.as_ref().map_or(true, |jh| jh.is_finished());
                    if h.terminated && finished {
                        true
                    } else {
                        return Err(WorkerError::Message(
                            "Only one worker can exist at a time. Call terminate() first.".into(),
                        ));
                    }
                }
            }
        };
        if needs_reap {
            // Drop the finished, terminated handle (its thread already exited,
            // so the detached JoinHandle leaks nothing).
            drop(state.borrow_mut().take::<WorkerHandle>());
        }
    }

    // Get the SharedArrayBufferStore from the main runtime (if set)
    let sab_store = {
        let st = state.borrow();
        st.try_borrow::<SharedArrayBufferStore>()
            .cloned()
            .unwrap_or_default()
    };

    // Read code_dir and clone a minimal HostOpState for the worker
    let (code_dir, worker_host_state) = {
        let st = state.borrow();
        let host = st.borrow::<HostOpState>();
        let code_dir = host.code_dir.clone().ok_or_else(|| {
            WorkerError::Message("No code directory set (game not loaded yet)".into())
        })?;

        // Fail-closed sandbox: refuse to spawn a worker before the /code mount
        // table exists, so the worker module loader always has a sandbox to
        // enforce (see WorkerModuleLoader::validate_sandbox).
        if host.mount_table.is_none() {
            return Err(WorkerError::Message(
                "No /code mount table set (game not fully loaded yet)".into(),
            ));
        }

        // Create dummy channels for services the worker does not use
        let (render_tx, _render_rx) = shared::render_command_sender::CommandSender::new();
        // Workers don't send audio commands in practice, but the type
        // system requires an AudioSender.  Use a no-op ThreadWakeup.
        let audio_tx = shared::op_state::AudioSender::new(
            shared::audio_channel::disconnected(),
            shared::channel::ThreadWakeup::new(),
        );

        let (callback_ids, runtime_generation) = inherited_correlation(host);
        let worker_state = HostOpState {
            callback_ids,
            runtime_generation,
            id: host.id,
            app_cache_dir: host.app_cache_dir.clone(),
            app_files_dir: host.app_files_dir.clone(),
            code_dir: host.code_dir.clone(),
            game_paths: host.game_paths.clone(),
            vfs: host.vfs.clone(),
            mount_table: host.mount_table.clone(),
            render_tx,
            // Workers don't drive measureText (no Canvas2D context in
            // Web Worker yet), so the fast-path measurer is left `None`.
            text_measurer: None,
            audio_tx,
            host_tx: host.host_tx.clone(),
            device_services: None,
            raf_rx: None,
            raf_demand: std::sync::Arc::new(shared::raf_signal::RafDemand::new()),
            request_vsync: None,
            sub_packages: host.sub_packages.clone(),
            workers_path: host.workers_path.clone(),
            network_policy: host.network_policy.clone(),
            backgrounded: host.backgrounded.clone(),
            timer_backgrounded: host.timer_backgrounded.clone(),
            webgl_context_created: host.webgl_context_created.clone(),
            context_lost: host.context_lost.clone(),
            code_signing_enabled: host.code_signing_enabled,
            gpu_caps: host.gpu_caps.clone(),
        };

        (code_dir, worker_state)
    };

    // Create bidirectional channels
    let (tx_main_to_worker, rx_main_to_worker) = worker_message_channel();
    let (tx_worker_to_main, rx_worker_to_main) = worker_outbound_channel();
    let (tx_worker_errors, rx_worker_errors) = worker_error_channel();
    let (timer_backgrounded_tx, timer_backgrounded_rx) = worker_lifecycle_channel();

    let worker_ctx = WorkerCtx {
        tx_to_main: tx_worker_to_main,
        tx_errors: tx_worker_errors,
        rx_from_main: Arc::new(tokio::sync::Mutex::new(rx_main_to_worker)),
        timer_backgrounded_rx: Arc::new(tokio::sync::Mutex::new(timer_backgrounded_rx)),
    };

    // Shared slot the worker thread fills with its isolate handle once its
    // JsRuntime is built, so the main thread can forcibly terminate a runaway
    // worker (see WorkerHandle::force_terminate).
    let isolate_handle: Arc<std::sync::Mutex<Option<v8::IsolateHandle>>> =
        Arc::new(std::sync::Mutex::new(None));

    // Spawn worker thread
    info!(
        "[Worker] spawning worker thread for script: {}",
        script_path
    );
    let join_handle = spawn_worker_thread(
        script_path,
        code_dir,
        worker_ctx,
        worker_host_state,
        sab_store,
        isolate_handle.clone(),
    )?;
    info!("[Worker] worker thread spawned, storing handle");

    // Store handle in main OpState
    let handle = WorkerHandle {
        tx_to_worker: tx_main_to_worker,
        rx_from_worker: Arc::new(tokio::sync::Mutex::new(rx_worker_to_main)),
        rx_errors: Arc::new(tokio::sync::Mutex::new(rx_worker_errors)),
        timer_backgrounded_tx,
        join_handle: Some(join_handle),
        terminated: false,
        isolate_handle,
    };

    state.borrow_mut().put(handle);
    Ok(())
}

/// Send a message from the main thread to the worker.
#[op2(fast)]
fn op_worker_post_message(
    state: &mut OpState,
    #[string] json_message: &str,
) -> Result<(), WorkerError> {
    if json_message.len() > MAX_WORKER_MESSAGE_BYTES {
        return Err(WorkerError::Message(format!(
            "Worker message too large: {} bytes (max {} bytes)",
            json_message.len(),
            MAX_WORKER_MESSAGE_BYTES
        )));
    }

    let handle = state
        .try_borrow::<WorkerHandle>()
        .ok_or_else(|| WorkerError::Message("No active worker".into()))?;

    if handle.terminated {
        return Err(WorkerError::Message("Worker has been terminated".into()));
    }

    info!(
        "[Worker] main->worker postMessage: {} bytes",
        json_message.len()
    );
    let permit = handle
        .tx_to_worker
        .try_reserve(json_message.len())
        .map_err(worker_reserve_error)?;
    let json_message = json_message.to_owned();
    handle
        .tx_to_worker
        .send_reserved(WorkerMessage::Message(json_message), permit)
        .map_err(|error| worker_queue_error(error, "Worker channel closed"))
}

/// Async op: wait for a message from the worker. Returns null when worker exits.
#[op2(async(lazy), fast)]
#[string]
async fn op_worker_recv_message(
    state: Rc<RefCell<OpState>>,
) -> Result<Option<String>, WorkerError> {
    let rx = {
        let st = state.borrow();
        let handle = st
            .try_borrow::<WorkerHandle>()
            .ok_or_else(|| WorkerError::Message("No active worker".into()))?;
        handle.rx_from_worker.clone()
    };

    info!("[Worker] main waiting for worker message...");
    let mut guard = rx.lock().await;
    let msg = guard.recv().await;
    info!(
        "[Worker] main received from worker: {:?}",
        msg.as_ref().map(|s| s.len())
    );
    Ok(msg)
}

/// Async op: wait for an error from the worker. Returns null when worker exits.
#[op2(async(lazy), fast)]
#[string]
async fn op_worker_recv_error(state: Rc<RefCell<OpState>>) -> Result<Option<String>, WorkerError> {
    let rx = {
        let st = state.borrow();
        let handle = st
            .try_borrow::<WorkerHandle>()
            .ok_or_else(|| WorkerError::Message("No active worker".into()))?;
        handle.rx_errors.clone()
    };

    let mut guard = rx.lock().await;
    Ok(guard.recv().await)
}

/// Terminate the active worker.
#[op2(fast)]
fn op_worker_terminate(state: &mut OpState) -> Result<(), WorkerError> {
    let handle = state
        .try_borrow_mut::<WorkerHandle>()
        .ok_or_else(|| WorkerError::Message("No active worker".into()))?;
    if !handle.terminated {
        handle.terminated = true;
        // Interrupt any executing JS (breaks a runaway `while (true) {}`) and
        // signal the message pump to exit.
        handle.force_terminate();
    }
    // The handle is intentionally KEPT in OpState (not taken) until the worker
    // thread has actually exited. `op_worker_create` reaps it once
    // `join_handle.is_finished()`, so a freshly created worker can never coexist
    // with an old one that is still winding down (e.g. briefly stuck in a native
    // op after the JS-loop interrupt).
    Ok(())
}

/// Send binary data from the main thread to the worker.
///
/// NOTE: This currently copies the buffer (deno_core limitation — true zero-copy
/// transfer requires V8 ArrayBuffer::Detach + BackingStore sharing which is not
/// yet wired). The JS-side ArrayBuffer is NOT detached/neutered.
/// Still faster than JSON-serializing large typed arrays.
#[op2]
fn op_worker_transfer_buffer(
    state: &mut OpState,
    #[buffer] data: JsBuffer,
) -> Result<(), WorkerError> {
    if data.len() > MAX_WORKER_MESSAGE_BYTES {
        return Err(WorkerError::Message(format!(
            "Transfer buffer too large: {} bytes (max {} bytes)",
            data.len(),
            MAX_WORKER_MESSAGE_BYTES
        )));
    }

    let handle = state
        .try_borrow::<WorkerHandle>()
        .ok_or_else(|| WorkerError::Message("No active worker".into()))?;

    if handle.terminated {
        return Err(WorkerError::Message("Worker has been terminated".into()));
    }

    // Admission is intentionally before `to_vec`: `[buffer]` borrows V8's
    // backing store for this synchronous op, so a saturated Worker queue does
    // not allocate a Rust copy or base64 staging input only to reject it.
    let queued_bytes = data.len().saturating_add(base64_encoded_len(data.len()));
    let permit = handle
        .tx_to_worker
        .try_reserve(queued_bytes)
        .map_err(worker_reserve_error)?;
    handle
        .tx_to_worker
        .send_reserved(WorkerMessage::Binary(data.to_vec()), permit)
        .map_err(|error| worker_queue_error(error, "Worker channel closed"))
}

// ---------------------------------------------------------------------------
// Worker-thread ops (registered in `host_v8_worker_inner`)
// ---------------------------------------------------------------------------

/// Send a message from the worker to the main thread.
#[op2(fast)]
fn op_worker_inner_post_message(
    state: &mut OpState,
    #[string] json_message: &str,
) -> Result<(), WorkerError> {
    if json_message.len() > MAX_WORKER_MESSAGE_BYTES {
        return Err(WorkerError::Message(format!(
            "Worker message too large: {} bytes (max {} bytes)",
            json_message.len(),
            MAX_WORKER_MESSAGE_BYTES
        )));
    }

    info!(
        "[Worker] worker->main postMessage: {} bytes",
        json_message.len()
    );
    let ctx = state.borrow::<WorkerCtx>();
    let permit = ctx
        .tx_to_main
        .try_reserve(json_message.len())
        .map_err(worker_reserve_error)?;
    let json_message = json_message.to_owned();
    ctx.tx_to_main
        .send_reserved(json_message, permit)
        .map_err(|error| worker_queue_error(error, "Main thread channel closed"))
}

fn worker_message_to_inbound(message: Option<WorkerMessage>) -> Option<WorkerInbound> {
    match message {
        Some(WorkerMessage::Message(json)) => {
            info!("[Worker] worker received from main: {} bytes", json.len());
            Some(WorkerInbound::Message { data: json })
        }
        Some(WorkerMessage::Binary(data)) => {
            // Encode binary as JSON with base64 payload so JS can reconstruct
            info!("[Worker] worker received binary: {} bytes", data.len());
            let encoded = deno_core::serde_json::json!({
                "__binary": true,
                "base64": base64_encode(&data),
                "byteLength": data.len()
            });
            Some(WorkerInbound::Message {
                data: encoded.to_string(),
            })
        }
        Some(WorkerMessage::Terminate) => {
            info!("[Worker] worker received Terminate signal");
            None
        }
        None => {
            info!("[Worker] worker channel closed (None)");
            None
        }
    }
}

async fn recv_worker_inbound(ctx: &WorkerCtx) -> Result<Option<WorkerInbound>, WorkerError> {
    let mut lifecycle = ctx.timer_backgrounded_rx.lock().await;
    let mut messages = ctx.rx_from_main.lock().await;

    if let Some(transition) = lifecycle.try_recv() {
        return Ok(Some(worker_lifecycle_to_inbound(transition)));
    }

    tokio::select! {
        biased;
        transition = lifecycle.recv() => {
            match transition {
                Ok(transition) => Ok(Some(worker_lifecycle_to_inbound(transition))),
                Err(_) => Ok(worker_message_to_inbound(messages.recv().await)),
            }
        }
        message = messages.recv() => Ok(worker_message_to_inbound(message)),
    }
}

fn worker_lifecycle_to_inbound(transition: WorkerTimerLifecycleTransition) -> WorkerInbound {
    let elapsed = tokio::time::Instant::now().saturating_duration_since(transition.occurred_at);
    WorkerInbound::Lifecycle {
        backgrounded: transition.backgrounded,
        elapsed_micros: elapsed.as_micros().min(u64::MAX as u128) as u64,
    }
}

/// Async op: wait for an internal lifecycle event or a user message.
/// Returns `None` when a Terminate signal is received.
#[op2(async(lazy), fast)]
#[serde]
async fn op_worker_inner_recv_message(
    state: Rc<RefCell<OpState>>,
) -> Result<Option<WorkerInbound>, WorkerError> {
    let ctx = {
        let st = state.borrow();
        let ctx = st.borrow::<WorkerCtx>();
        WorkerCtx {
            tx_to_main: ctx.tx_to_main.clone(),
            tx_errors: ctx.tx_errors.clone(),
            rx_from_main: ctx.rx_from_main.clone(),
            timer_backgrounded_rx: ctx.timer_backgrounded_rx.clone(),
        }
    };

    info!("[Worker] worker waiting for main message or lifecycle...");
    recv_worker_inbound(&ctx).await
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Simple base64 encoder (no external dependency).
fn base64_encode(data: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(CHARS[((triple >> 18) & 0x3F) as usize] as char);
        out.push(CHARS[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            out.push(CHARS[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(CHARS[(triple & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

fn base64_encoded_len(len: usize) -> usize {
    len.checked_add(2)
        .and_then(|value| value.checked_div(3))
        .and_then(|value| value.checked_mul(4))
        .unwrap_or(usize::MAX)
}

// ---------------------------------------------------------------------------
// Extension declarations
// ---------------------------------------------------------------------------

deno_core::extension!(
    host_v8_worker,
    deps = [host_v8_base],
    ops = [
        op_worker_create,
        op_worker_post_message,
        op_worker_transfer_buffer,
        op_worker_recv_message,
        op_worker_recv_error,
        op_worker_terminate,
    ],
    esm_entry_point = "ext:host_v8_worker/99_global_scope.js",
    esm = [
        dir "src/worker",
        "01_worker.js",
        "99_global_scope.js",
    ],
);

deno_core::extension!(
    host_v8_worker_inner,
    ops = [
        op_worker_inner_post_message,
        op_worker_inner_recv_message,
    ],
    esm = [
        dir "src/worker",
        "02_worker_inner.js",
    ],
    options = {
        ctx: WorkerCtx,
    },
    state = |state, options| {
        state.put::<WorkerCtx>(options.ctx);
    },
);

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

pub fn worker_extensions() -> Vec<Extension> {
    vec![host_v8_worker::init()]
}

pub fn worker_lazy_extensions() -> Vec<Extension> {
    vec![host_v8_worker::lazy_init()]
}

pub fn worker_inner_extensions(ctx: WorkerCtx) -> Vec<Extension> {
    vec![host_v8_worker_inner::init(ctx)]
}

pub(crate) fn set_timer_backgrounded(state: &mut OpState, backgrounded: bool) {
    let Some(handle) = state.try_borrow::<WorkerHandle>() else {
        return;
    };
    if handle.terminated {
        return;
    }
    handle
        .timer_backgrounded_tx
        .send(WorkerTimerLifecycleTransition::now(backgrounded));
}

/// Create the full extension set for a worker JsRuntime.
///
/// Includes all extensions needed by `98_global_scope_shared.js`:
/// base, console, event, utility, file, rendering (webgl/image), web, url, network.
/// This gives workers the same shared APIs as the main thread.
pub fn create_worker_runtime_extensions(ctx: WorkerCtx, host_state: HostOpState) -> Vec<Extension> {
    use crate::{
        base, console, env, event, file, io_state, lifecycle, network, rendering, url, utility,
        web, worker_runtime,
    };

    let mut exts: Vec<Extension> = Vec::new();

    exts.extend(base::base_extensions(host_state));
    exts.extend(io_state::io_state_extensions());
    exts.extend(console::console_extensions());
    exts.extend(event::event_extensions());
    exts.extend(utility::utility_extensions());
    exts.extend(file::file_extensions());
    exts.extend(rendering::rendering_extensions());
    exts.extend(web::web_extensions());
    exts.extend(url::url_extensions());
    exts.extend(network::network_extensions());

    #[cfg(feature = "api-media")]
    {
        // host_v8_audio depends on host_v8_lifecycle, so it must be registered
        // first (its esm imports onHide/onShow from the lifecycle module).
        exts.extend(lifecycle::lifecycle_extensions());
        exts.extend(crate::audio::audio_extensions());
    }

    exts.extend(env::env_extensions());
    exts.extend(worker_inner_extensions(ctx));
    exts.push(worker_runtime::init());

    exts
}

/// Create the exact Worker extension chain in lazy-init mode for snapshot
/// creation/restoration. Keep this order byte-identical to
/// [`create_worker_runtime_extensions`] and [`create_worker_runtime_extension_args`].
pub(crate) fn create_worker_runtime_lazy_extensions() -> Vec<Extension> {
    use crate::{
        base, console, env, event, file, io_state, lifecycle, network, rendering, url, utility,
        web, worker_runtime,
    };

    let mut exts = vec![
        base::host_v8_base::lazy_init(),
        io_state::host_v8_io_state::lazy_init(),
    ];
    exts.extend(console::console_lazy_extensions());
    exts.extend(event::event_lazy_extensions());
    exts.extend(utility::utility_lazy_extensions());
    exts.extend(file::file_lazy_extensions());
    exts.extend(rendering::rendering_lazy_extensions());
    exts.extend(web::web_lazy_extensions());
    exts.extend(url::url_lazy_extensions());
    exts.extend(network::network_lazy_extensions());

    #[cfg(feature = "api-media")]
    {
        exts.push(lifecycle::host_v8_lifecycle::lazy_init());
        exts.extend(crate::audio::audio_lazy_extensions());
    }

    exts.push(env::host_v8_env::lazy_init());
    exts.push(host_v8_worker_inner::lazy_init());
    exts.push(worker_runtime::lazy_init());
    exts
}

/// Runtime-only state callbacks for a restored Worker snapshot.
pub(crate) fn create_worker_runtime_extension_args(
    ctx: WorkerCtx,
    host_state: HostOpState,
) -> Vec<ExtensionArguments> {
    use crate::{
        base, console, env, event, file, io_state, lifecycle, network, rendering, url, utility,
        web, worker_runtime,
    };

    let mut args = vec![
        base::host_v8_base::args(host_state),
        io_state::host_v8_io_state::args(),
        console::host_v8_console::args(),
        event::host_v8_event::args(),
        utility::host_v8_utility::args(),
        file::host_v8_file::args(),
        rendering::image::host_v8_image::args(),
        rendering::webgl::host_v8_webgl::args(),
        web::host_v8_web::args(),
        url::host_v8_url::args(),
        network::network_extension_args(),
    ];

    #[cfg(feature = "api-media")]
    {
        args.push(lifecycle::host_v8_lifecycle::args());
        args.push(crate::audio::host_v8_audio::args());
    }

    args.push(env::host_v8_env::args());
    args.push(host_v8_worker_inner::args(ctx));
    args.push(worker_runtime::args());
    args
}

// ---------------------------------------------------------------------------
// Worker thread spawn
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Runaway protection via the process deadline watchdog
// ---------------------------------------------------------------------------

/// Max time a Worker may run untrusted JS without yielding before it is
/// force-terminated. Matches the host ANR budget: generous enough for module
/// compilation on low-end devices, tight enough to catch a `while (true)`.
const WORKER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// The single JSON error reported (exactly once) when the deadline watchdog
/// terminates a runaway Worker.
const WORKER_TIMEOUT_MSG: &str =
    r#"{"message":"Worker terminated: unresponsive (watchdog timeout)"}"#;

/// Report a Worker error unless the deadline watchdog already fired. On a
/// timeout the watchdog observer sends [`WORKER_TIMEOUT_MSG`] exactly once, so
/// the load/eval/event-loop error paths must not additionally report the
/// resulting "execution terminated" error.
fn report_worker_error(
    watchdog: Option<&DeadlineWatchdog>,
    tx_errors: &WorkerQueueSender<String>,
    message: String,
) {
    if watchdog.is_some_and(DeadlineWatchdog::timed_out) {
        return;
    }
    send_worker_diagnostic(tx_errors, message);
}

fn send_worker_diagnostic(tx_errors: &WorkerQueueSender<String>, message: String) {
    if let Err(error) = tx_errors.send(message) {
        let reason = match error {
            WorkerQueueSendError::Full(_) => "count limit",
            WorkerQueueSendError::ByteLimit(_) => "byte limit",
            WorkerQueueSendError::Closed(_) => "receiver closed",
        };
        warn!("[Worker] dropping diagnostic after {reason}");
    }
}

fn worker_initialization_error(stage: &str, error: &dyn std::fmt::Display) -> String {
    deno_core::serde_json::json!({
        "message": format!("Worker {stage} failed: {error}")
    })
    .to_string()
}

/// Guard every poll of module loading (V8 parse/compile of untrusted code counts
/// against the budget).
async fn worker_load_module(
    rt: &mut JsRuntime,
    watchdog: Option<&DeadlineWatchdog>,
    resolved: &deno_core::ModuleSpecifier,
) -> Result<deno_core::ModuleId, String> {
    let mut load = std::pin::pin!(rt.load_main_es_module(resolved));
    std::future::poll_fn(|cx| crate::watchdog::poll_guarded(watchdog, load.as_mut(), cx))
        .await
        .map_err(|e| e.to_string())
}

/// Guard the SYNCHRONOUS `mod_evaluate` constructor — deno_core 0.385 enters V8
/// to run the module top-level while building the returned future, so a
/// top-level `while (true) {}` must be covered here, not only in later polls —
/// plus every poll of the evaluation future and the event loop.
async fn worker_evaluate_module(
    rt: &mut JsRuntime,
    watchdog: Option<&DeadlineWatchdog>,
    module_id: deno_core::ModuleId,
) -> Result<(), String> {
    let evaluation = {
        let _scope = watchdog.map(DeadlineWatchdog::enter);
        rt.mod_evaluate(module_id)
    };
    let mut evaluation = std::pin::pin!(evaluation);
    std::future::poll_fn(move |cx| -> std::task::Poll<Result<(), String>> {
        let _scope = watchdog.map(DeadlineWatchdog::enter);
        if let std::task::Poll::Ready(res) = evaluation.as_mut().poll(cx) {
            return std::task::Poll::Ready(res.map_err(|e| e.to_string()));
        }
        match rt.poll_event_loop(cx, PollEventLoopOptions::default()) {
            std::task::Poll::Ready(Err(e)) => std::task::Poll::Ready(Err(e.to_string())),
            _ => std::task::Poll::Pending,
        }
    })
    .await
}

/// Guard every poll of the Worker event loop. The Worker message pump runs here,
/// so a runaway message handler is force-terminated on the next poll boundary.
async fn worker_run_event_loop(
    rt: &mut JsRuntime,
    watchdog: Option<&DeadlineWatchdog>,
) -> Result<(), String> {
    std::future::poll_fn(move |cx| {
        let _scope = watchdog.map(DeadlineWatchdog::enter);
        rt.poll_event_loop(cx, PollEventLoopOptions::default())
    })
    .await
    .map_err(|e| e.to_string())
}

/// Start the Worker receive loop and remove snapshot/bootstrap internals before
/// the isolate becomes reachable from another thread or game code is loaded.
fn initialize_worker_runtime(rt: &mut JsRuntime) -> Result<(), String> {
    rt.execute_script(
        "ext:worker_runtime/initialize_runtime.js",
        deno_core::FastString::from_static(
            r#"(() => {
                const start = globalThis.__migoStartWorkerMessagePump;
                if (typeof start !== "function") {
                    throw new Error("Worker message-pump bootstrap hook is missing");
                }
                start();
                if (!delete globalThis.__migoStartWorkerMessagePump ||
                    !delete globalThis.Deno ||
                    !delete globalThis.__bootstrap) {
                    throw new Error("Worker bootstrap globals could not be removed");
                }
                if ("__migoStartWorkerMessagePump" in globalThis ||
                    "Deno" in globalThis || "__bootstrap" in globalThis) {
                    throw new Error("Worker bootstrap globals remain visible");
                }
            })();"#,
        ),
    )
    .map(|_| ())
    .map_err(|error| error.to_string())
}

fn spawn_worker_thread(
    script_path: String,
    code_dir: String,
    ctx: WorkerCtx,
    host_state: HostOpState,
    sab_store: SharedArrayBufferStore,
    isolate_handle_slot: Arc<std::sync::Mutex<Option<v8::IsolateHandle>>>,
) -> Result<std::thread::JoinHandle<()>, WorkerError> {
    let tx_errors = ctx.tx_errors.clone();

    std::thread::Builder::new()
        .name("Migo-Worker".into())
        .spawn(move || {
            // Clone kept in the outer scope so we can clear the published isolate
            // handle once the thread exits (any path). `run` moves the original
            // clone into its async block. Clearing the slot makes a post-exit
            // `force_terminate` an observable no-op and aids state inspection.
            let slot_for_cleanup = isolate_handle_slot.clone();
            let run = || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_io()
                    .enable_time()
                    .event_interval(61)
                    .global_queue_interval(31)
                    .max_io_events_per_tick(1024)
                    .max_blocking_threads(2)
                    .build()
                    .expect("Failed to create worker tokio runtime");

                runtime.block_on(async move {
                    let worker_mount_table = host_state.mount_table.clone();
                    let snapshot_bytes = crate::snapshot::WORKER_SNAPSHOT_BYTES;
                    let use_snapshot = snapshot_bytes.is_some();
                    let (exts, extension_args) = if use_snapshot {
                        (
                            create_worker_runtime_lazy_extensions(),
                            Some(create_worker_runtime_extension_args(ctx, host_state)),
                        )
                    } else {
                        (
                            create_worker_runtime_extensions(ctx, host_state),
                            None,
                        )
                    };

                    let module_loader: Option<Rc<dyn ModuleLoader>> =
                        Some(Rc::new(WorkerModuleLoader {
                            inner: FsModuleLoader,
                            mount_table: worker_mount_table,
                        }));

                    // Apply the same V8 heap limits as the main thread to prevent
                    // worker code from OOM-ing the entire process.
                    let v8_limits = crate::V8LimitsConfig::default();
                    let create_params = Some(
                        v8::Isolate::create_params()
                            .heap_limits(v8_limits.initial_heap_size, v8_limits.max_heap_size),
                    );

                    info!(
                        "[Worker] creating JsRuntime with {} extensions (snapshot={})",
                        exts.len(),
                        use_snapshot
                    );
                    let mut rt = JsRuntime::new(RuntimeOptions {
                        module_loader,
                        extensions: exts,
                        create_params,
                        startup_snapshot: snapshot_bytes,
                        shared_array_buffer_store: Some(sab_store),
                        skip_op_registration: use_snapshot,
                        ..Default::default()
                    });
                    info!("[Worker] JsRuntime created successfully");

                    // Publish the isolate handle before anything runs JavaScript,
                    // so the main thread can forcibly terminate this worker
                    // (WorkerHandle::force_terminate) for the whole life of the
                    // isolate rather than from the point bootstrap finished.
                    //
                    // It used to be published after `initialize_worker_runtime`,
                    // which meant lazy extension init and the bootstrap script ran
                    // inside a window where the slot still held `None`: a hang in
                    // either left `force_terminate` a silent no-op, and
                    // `WorkerHandle::drop` joins, so the thread that asked for the
                    // teardown waited forever too. Terminating before any JS has
                    // run is well defined -- the flag simply makes the first
                    // execution unwind -- so there is no reason for the window to
                    // exist.
                    if let Ok(mut slot) = isolate_handle_slot.lock() {
                        *slot = Some(rt.v8_isolate().thread_safe_handle());
                    }

                    if let Some(extension_args) = extension_args {
                        if let Err(error) = rt.lazy_init_extensions(extension_args) {
                            error!("[Worker] snapshot state initialization failed: {error}");
                            send_worker_diagnostic(&tx_errors, worker_initialization_error(
                                "snapshot state initialization",
                                &error,
                            ));
                            return;
                        }
                    }

                    if let Err(error) = initialize_worker_runtime(&mut rt) {
                        error!("[Worker] runtime bootstrap failed: {error}");
                        send_worker_diagnostic(
                            &tx_errors,
                            worker_initialization_error("runtime bootstrap", &error),
                        );
                        return;
                    }

                    // Register near-heap-limit callback for OOM protection
                    {
                        let hard_cap = v8_limits.max_heap_size.saturating_add(8 * 1024 * 1024);
                        let oom_handle = rt.v8_isolate().thread_safe_handle();
                        let oom_fired = Arc::new(std::sync::atomic::AtomicBool::new(false));
                        let cb_fired = Arc::clone(&oom_fired);
                        let cb_tx = tx_errors.clone();

                        rt.add_near_heap_limit_callback(move |current_limit, _initial_limit| {
                            let first = cb_fired
                                .compare_exchange(
                                    false,
                                    true,
                                    std::sync::atomic::Ordering::SeqCst,
                                    std::sync::atomic::Ordering::SeqCst,
                                )
                                .is_ok();
                            if first {
                                warn!("[Worker] V8 heap limit reached, terminating");
                                oom_handle.terminate_execution();
                                send_worker_diagnostic(&cb_tx,
                                    r#"{"message":"Worker terminated: V8 heap limit exceeded"}"#
                                        .to_string(),
                                );
                            }
                            current_limit.saturating_add(1024 * 1024).min(hard_cap)
                        });
                    }

                    // Register with the ONE process deadline watchdog. This
                    // replaces the per-Worker one-second ticker + two-second
                    // monitor OS thread; the observer reports the timeout exactly
                    // once on `tx_errors`. Unconditional (not `v8-limits`-gated):
                    // Worker runaway protection exists even under
                    // `--no-default-features`. Declared after `rt` so it disarms +
                    // unregisters (RAII) before `rt`/the isolate drops on every
                    // exit path (success, error, panic).
                    let watchdog = {
                        let handle = rt.v8_isolate().thread_safe_handle();
                        let tx = tx_errors.clone();
                        let config = DeadlineWatchdogConfig::new(WORKER_TIMEOUT, "worker")
                            .with_observer(Arc::new(move |_| {
                                send_worker_diagnostic(&tx, WORKER_TIMEOUT_MSG.to_string());
                            }));
                        match DeadlineWatchdog::register_isolate(handle, config) {
                            Ok(w) => Some(w),
                            Err(e) => {
                                warn!(
                                    "[Worker] deadline watchdog unavailable, continuing without runaway protection: {e}"
                                );
                                None
                            }
                        }
                    };

                    // Resolve and load worker script
                    let code_path = std::path::PathBuf::from(&code_dir);
                    let resolved = match resolve_path(&script_path, &code_path) {
                        Ok(r) => r,
                        Err(e) => {
                            error!(
                                "[Worker] failed to resolve worker script '{}' in '{}': {}",
                                script_path, code_dir, e
                            );
                            report_worker_error(
                                watchdog.as_ref(),
                                &tx_errors,
                                format!(r#"{{"message":"Failed to resolve worker script: {}"}}"#, e),
                            );
                            return;
                        }
                    };

                    info!("[Worker] loading main module: {}", resolved);
                    let module_id =
                        match worker_load_module(&mut rt, watchdog.as_ref(), &resolved).await {
                            Ok(id) => id,
                            Err(e) => {
                                error!("[Worker] failed to load worker script: {}", e);
                                report_worker_error(
                                    watchdog.as_ref(),
                                    &tx_errors,
                                    format!(r#"{{"message":"Failed to load worker script: {}"}}"#, e),
                                );
                                return;
                            }
                        };

                    info!("[Worker] module loaded (id={}), evaluating...", module_id);
                    if let Err(e) = worker_evaluate_module(&mut rt, watchdog.as_ref(), module_id).await
                    {
                        error!("[Worker] worker script evaluation error: {}", e);
                        report_worker_error(
                            watchdog.as_ref(),
                            &tx_errors,
                            format!(r#"{{"message":"Worker script evaluation error: {}"}}"#, e),
                        );
                        return;
                    }

                    info!("[Worker] module evaluated, running event loop");
                    // Run event loop until it completes (message pump op keeps it alive)
                    if let Err(e) = worker_run_event_loop(&mut rt, watchdog.as_ref()).await {
                        error!("[Worker] event loop error: {}", e);
                        report_worker_error(
                            watchdog.as_ref(),
                            &tx_errors,
                            format!(r#"{{"message":"Worker event loop error: {}"}}"#, e),
                        );
                    }

                    info!("[Worker] thread exiting cleanly");
                });
            };

            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(run));
            if let Err(panic_info) = result {
                let panic_msg = panic_info
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| panic_info.downcast_ref::<&str>().map(|s| s.to_string()))
                    .unwrap_or_else(|| "Unknown panic".to_string());

                error!("[Worker] panicked: {}", panic_msg);
            }

            // Thread is exiting (clean, error, or panic): drop the published
            // isolate handle so the main thread stops holding a handle to a dead
            // isolate.
            if let Ok(mut slot) = slot_for_cleanup.lock() {
                *slot = None;
            }
        })
        .map_err(|e| WorkerError::Message(format!("Failed to spawn worker thread: {e}")))
}

#[cfg(test)]
mod worker_queue_boundary_tests {
    use super::*;

    const EXPECTED_MESSAGE_CAPACITY: usize = 64;

    fn main_worker_handle() -> (WorkerHandle, WorkerMessageReceiver) {
        let (tx_to_worker, rx_from_main) = worker_message_channel();
        let (_tx_from_worker, rx_from_worker) = worker_outbound_channel();
        let (_tx_errors, rx_errors) = worker_error_channel();
        let (timer_backgrounded_tx, _timer_backgrounded_rx) = worker_lifecycle_channel();
        (
            WorkerHandle {
                tx_to_worker,
                rx_from_worker: Arc::new(tokio::sync::Mutex::new(rx_from_worker)),
                rx_errors: Arc::new(tokio::sync::Mutex::new(rx_errors)),
                timer_backgrounded_tx,
                join_handle: None,
                terminated: false,
                isolate_handle: Arc::new(std::sync::Mutex::new(None)),
            },
            rx_from_main,
        )
    }

    #[test]
    fn worker_runtime_channels_are_explicitly_bounded() {
        let source = include_str!("mod.rs");
        let start = source
            .find("// Create bidirectional channels")
            .expect("worker channel construction");
        let end = source[start..]
            .find("// Shared slot")
            .map(|offset| start + offset)
            .expect("end of worker channel construction");
        let construction = &source[start..end];

        assert!(
            !construction.contains("mpsc::unbounded_channel"),
            "worker data, error, and lifecycle transports must have explicit bounds"
        );
    }

    #[test]
    fn main_to_worker_rejects_limit_plus_one_without_blocking() {
        let (handle, _rx) = main_worker_handle();
        for _ in 0..EXPECTED_MESSAGE_CAPACITY {
            handle
                .tx_to_worker
                .send(WorkerMessage::Message("x".into()))
                .expect("at count limit");
        }

        let error = handle
            .tx_to_worker
            .send(WorkerMessage::Message("x".into()))
            .map_err(|error| worker_queue_error(error, "Worker channel closed"))
            .expect_err("limit + 1 must be refused");
        assert!(error.to_string().contains("queue full"));
    }

    #[test]
    fn terminate_uses_reserved_capacity_after_the_data_limit() {
        let (handle, mut rx) = main_worker_handle();
        for _ in 0..EXPECTED_MESSAGE_CAPACITY {
            handle
                .tx_to_worker
                .send(WorkerMessage::Message("x".into()))
                .expect("at count limit");
        }

        handle.force_terminate();

        assert!(matches!(rx.try_recv(), Ok(WorkerMessage::Terminate)));
        for _ in 0..EXPECTED_MESSAGE_CAPACITY {
            assert!(matches!(rx.try_recv(), Ok(WorkerMessage::Message(_))));
        }
    }

    #[test]
    fn dropping_worker_handle_joins_the_terminated_thread() {
        let (mut handle, mut rx) = main_worker_handle();
        let completed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let completed_on_worker = Arc::clone(&completed);
        handle.join_handle = Some(std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("test runtime");
            assert!(matches!(
                runtime.block_on(rx.recv()),
                Some(WorkerMessage::Terminate)
            ));
            // Make a detached implementation fail deterministically: it can
            // enqueue Terminate quickly, but it cannot claim the worker has
            // completed before this thread actually reaches its exit path.
            std::thread::sleep(Duration::from_millis(50));
            completed_on_worker.store(true, std::sync::atomic::Ordering::Release);
        }));

        drop(handle);

        assert!(
            completed.load(std::sync::atomic::Ordering::Acquire),
            "dropping the main runtime must join its nested Worker"
        );
    }

    #[test]
    fn worker_byte_budget_accepts_limit_and_rejects_limit_plus_one() {
        let (tx, mut rx) = worker_queue::<String>(2, 4);

        tx.send("1234".into()).expect("the byte limit is inclusive");
        assert!(matches!(
            tx.send("x".into()),
            Err(WorkerQueueSendError::ByteLimit(value)) if value == "x"
        ));
        assert_eq!(tx.usage.bytes.load(std::sync::atomic::Ordering::Acquire), 4);

        assert_eq!(rx.try_recv().unwrap(), "1234");
        assert_eq!(tx.usage.bytes.load(std::sync::atomic::Ordering::Acquire), 0);
        assert!(matches!(
            tx.send("12345".into()),
            Err(WorkerQueueSendError::ByteLimit(value)) if value == "12345"
        ));
    }

    #[test]
    fn receive_and_receiver_drop_release_every_worker_permit() {
        let (tx, mut rx) = worker_queue::<String>(2, 8);
        tx.send("aa".into()).unwrap();
        tx.send("bbb".into()).unwrap();
        assert_eq!(tx.usage.items.load(std::sync::atomic::Ordering::Acquire), 2);
        assert_eq!(tx.usage.bytes.load(std::sync::atomic::Ordering::Acquire), 5);

        assert_eq!(rx.try_recv().unwrap(), "aa");
        assert_eq!(tx.usage.items.load(std::sync::atomic::Ordering::Acquire), 1);
        assert_eq!(tx.usage.bytes.load(std::sync::atomic::Ordering::Acquire), 3);

        drop(rx);
        assert_eq!(tx.usage.items.load(std::sync::atomic::Ordering::Acquire), 0);
        assert_eq!(tx.usage.bytes.load(std::sync::atomic::Ordering::Acquire), 0);
    }

    #[test]
    fn stopped_worker_receiver_returns_payload_and_releases_permit() {
        let (tx, rx) = worker_queue::<String>(1, 4);
        drop(rx);

        assert!(matches!(
            tx.send("data".into()),
            Err(WorkerQueueSendError::Closed(value)) if value == "data"
        ));
        assert_eq!(tx.usage.items.load(std::sync::atomic::Ordering::Acquire), 0);
        assert_eq!(tx.usage.bytes.load(std::sync::atomic::Ordering::Acquire), 0);
    }

    #[test]
    fn dropped_worker_receiver_rejects_reservation_before_copy() {
        let (tx, rx) = worker_queue::<WorkerMessage>(1, 8);
        drop(rx);

        assert!(matches!(tx.try_reserve(7), Err(WorkerReserveError::Closed)));
        assert_eq!(tx.usage.items.load(std::sync::atomic::Ordering::Acquire), 0);
        assert_eq!(tx.usage.bytes.load(std::sync::atomic::Ordering::Acquire), 0);
    }

    #[test]
    fn binary_admission_charges_raw_and_projected_base64_bytes() {
        let message = WorkerMessage::Binary(vec![1, 2, 3]);
        assert_eq!(message.queued_bytes(), 3 + 4);
    }

    #[test]
    fn binary_reservation_accepts_the_byte_limit_and_refuses_limit_plus_one() {
        let (tx, _rx) = worker_queue::<WorkerMessage>(2, 7);
        let payload = vec![1, 2, 3]; // 3 raw bytes + 4 projected base64 bytes.
        let permit = tx
            .try_reserve(7)
            .expect("the aggregate byte limit is inclusive");
        tx.send_reserved(WorkerMessage::Binary(payload), permit)
            .expect("the admitted payload must be queued");

        assert!(matches!(
            tx.try_reserve(1),
            Err(WorkerReserveError::ByteLimit)
        ));
        assert_eq!(tx.usage.items.load(std::sync::atomic::Ordering::Acquire), 1);
        assert_eq!(tx.usage.bytes.load(std::sync::atomic::Ordering::Acquire), 7);
    }

    #[test]
    fn binary_transfer_reserves_queue_capacity_before_copying_the_v8_buffer() {
        let source = include_str!("mod.rs");
        let start = source
            .find("fn op_worker_transfer_buffer")
            .expect("binary Worker op");
        let body = &source[start..source[start..]
            .find("// ---------------------------------------------------------------------------\n// Worker-thread ops")
            .map(|offset| start + offset)
            .expect("end of binary Worker op")];

        let reserve = body
            .find("try_reserve")
            .expect("binary payload must reserve queue capacity");
        let copy = body
            .find("data.to_vec()")
            .expect("binary payload must be copied only after admission");
        assert!(
            reserve < copy,
            "Worker admission must happen before copying the V8 buffer"
        );
        assert!(
            !body.contains("#[buffer(copy)]"),
            "#[buffer(copy)] copies the V8 buffer before the op can reserve queue capacity"
        );
    }

    #[test]
    fn worker_string_ops_borrow_then_reserve_before_copying() {
        let source = include_str!("mod.rs");
        for (name, end_marker) in [
            (
                "fn op_worker_post_message",
                "/// Async op: wait for a message",
            ),
            (
                "fn op_worker_inner_post_message",
                "fn worker_message_to_inbound",
            ),
        ] {
            let start = source.find(name).expect("Worker string op");
            let end = source[start..]
                .find(end_marker)
                .map(|offset| start + offset)
                .expect("end of Worker string op");
            let body = &source[start..end];

            assert!(
                body.contains("#[string] json_message: &str"),
                "{name} must borrow the V8 string"
            );
            let reserve = body.find("try_reserve").expect("queue admission");
            let copy = body.find("json_message.to_owned()").expect("owned payload");
            assert!(reserve < copy, "{name} must reserve before copying");
            assert!(body.contains("send_reserved"));
        }
    }

    #[test]
    fn pre_ready_js_messages_have_the_same_count_and_byte_bounds_as_rust() {
        let source = include_str!("01_worker.js");

        assert!(source.contains("const MAX_PENDING_MESSAGES = 64"));
        assert!(source.contains("const MAX_PENDING_MESSAGE_BYTES = 64 * 1024 * 1024"));
        assert!(source.contains("const MAX_WORKER_MESSAGE_BYTES = 16 * 1024 * 1024"));
        assert!(source.contains("#pendingMessageBytes = 0"));
        assert!(source.contains("utf8ByteLength(json)"));

        let queue_branch = source
            .split("if (!this.#ready)")
            .nth(1)
            .expect("pre-ready queue branch");
        let push = queue_branch
            .find("ArrayPrototypePush(this.#pendingMessages, json)")
            .expect("pending queue insertion");
        for guard in [
            "MAX_WORKER_MESSAGE_BYTES",
            "MAX_PENDING_MESSAGES",
            "MAX_PENDING_MESSAGE_BYTES",
        ] {
            assert!(
                queue_branch.find(guard).is_some_and(|offset| offset < push),
                "{guard} must be enforced before retaining a pending message"
            );
        }
    }

    #[test]
    fn pre_ready_js_byte_accounting_matches_utf8() {
        let source = include_str!("01_worker.js");
        let start = source
            .find("function utf8ByteLength")
            .expect("UTF-8 byte counter");
        let end = source[start..]
            .find("\n\nlet currentWorker")
            .map(|offset| start + offset)
            .expect("end of byte counter");
        let helper = &source[start..end];
        let script = format!(
            r#"const StringPrototypeCharCodeAt = Function.prototype.call.bind(String.prototype.charCodeAt);
{helper}
const cases = [["a", 1], ["é", 2], ["水", 3], ["😀", 4], ["aé水😀", 10]];
for (const [value, expected] of cases) {{
    const actual = utf8ByteLength(value);
    if (actual !== expected) throw new Error(`${{value}}: ${{actual}} !== ${{expected}}`);
}}"#
        );
        let mut runtime = JsRuntime::new(RuntimeOptions::default());
        runtime
            .execute_script(
                "<test:worker-pending-utf8>",
                deno_core::FastString::from(script),
            )
            .expect("UTF-8 byte accounting must execute and match Rust string bytes");
    }

    #[test]
    fn error_queue_rejections_are_counted() {
        let (tx, _rx) = worker_queue::<String>(1, 4);
        tx.send("err!".into()).unwrap();
        assert!(matches!(
            tx.send("next".into()),
            Err(WorkerQueueSendError::Full(_))
        ));
        assert_eq!(
            tx.usage.rejected.load(std::sync::atomic::Ordering::Acquire),
            1
        );
    }
}

#[cfg(test)]
mod timer_lifecycle_tests {
    use super::*;
    use std::{path::PathBuf, sync::atomic::AtomicBool, time::Duration};

    use deno_core::{FastString, PollEventLoopOptions, RuntimeOptions};
    use futures::future::poll_fn;
    use shared::{
        channel::ThreadWakeup,
        device::gpu_caps::GpuCaps,
        op_state::{AudioSender, NetworkPolicy},
        render_command_sender::CommandSender,
    };
    #[test]
    fn worker_and_parent_share_host_callback_id_space() {
        let parent = test_host_state(Arc::new(AtomicBool::new(false)));
        let (callback_ids, runtime_generation) = super::inherited_correlation(&parent);

        assert!(
            Arc::ptr_eq(&callback_ids, &parent.callback_ids),
            "a Worker must inherit the space, not open its own"
        );
        assert_eq!(runtime_generation, parent.runtime_generation);
        // Interleaved: a Worker-local allocator would restart at 1 and hand the
        // platform an id the parent has already used.
        assert_eq!(parent.callback_ids.allocate(), Ok(1));
        assert_eq!(callback_ids.allocate(), Ok(2));
        assert_eq!(parent.callback_ids.allocate(), Ok(3));
    }

    #[test]
    fn media_audio_player_uses_host_callback_ids_across_realms() {
        let source = include_str!("../audio/04_media_audio_player.js");
        assert!(
            source.contains("allocateHostCallbackId"),
            "MediaAudioPlayer ids must come from the host allocator"
        );
        assert!(
            !source.contains("nextMediaPlayerId"),
            "a per-realm counter can collide with a parent or Worker player"
        );
    }

    fn test_host_state(timer_backgrounded: Arc<AtomicBool>) -> HostOpState {
        let (render_tx, _render_rx) = CommandSender::new();
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
            device_services: None,
            raf_rx: None,
            raf_demand: std::sync::Arc::new(shared::raf_signal::RafDemand::new()),
            request_vsync: None,
            sub_packages: Vec::new(),
            workers_path: None,
            network_policy: NetworkPolicy::default(),
            backgrounded: Arc::new(AtomicBool::new(false)),
            timer_backgrounded,
            webgl_context_created: Arc::new(AtomicBool::new(false)),
            context_lost: Arc::new(shared::op_state::ContextLostState::default()),
            code_signing_enabled: false,
            gpu_caps: GpuCaps::new(),
        }
    }

    fn test_worker_ctx(
        rx_from_main: WorkerMessageReceiver,
        timer_backgrounded_rx: WorkerLifecycleReceiver,
    ) -> WorkerCtx {
        let (tx_to_main, _rx_to_main) = worker_outbound_channel();
        let (tx_errors, _rx_errors) = worker_error_channel();
        WorkerCtx {
            tx_to_main,
            tx_errors,
            rx_from_main: Arc::new(tokio::sync::Mutex::new(rx_from_main)),
            timer_backgrounded_rx: Arc::new(tokio::sync::Mutex::new(timer_backgrounded_rx)),
        }
    }

    fn exec(rt: &mut JsRuntime, source: impl Into<String>) {
        rt.execute_script("<test:worker-timer>", FastString::from(source.into()))
            .expect("worker timer script");
    }

    fn assert_js(rt: &mut JsRuntime, expression: &str) {
        exec(
            rt,
            format!(
                "if (!({expression})) throw new Error('worker timer assertion failed: ' + ({expression}));"
            ),
        );
    }

    async fn poll_once(rt: &mut JsRuntime) {
        poll_fn(|cx| {
            let _ = rt.poll_event_loop(cx, PollEventLoopOptions::default());
            std::task::Poll::Ready(())
        })
        .await;
    }

    async fn drain_ready(rt: &mut JsRuntime) {
        for _ in 0..6 {
            poll_once(rt).await;
            tokio::task::yield_now().await;
        }
    }

    async fn advance_and_drain(rt: &mut JsRuntime, duration: Duration) {
        poll_once(rt).await;
        tokio::time::advance(duration).await;
        tokio::time::advance(Duration::from_nanos(1)).await;
        tokio::task::yield_now().await;
        drain_ready(rt).await;
    }

    #[tokio::test(start_paused = true)]
    async fn lifecycle_change_preempts_a_queued_user_message() {
        let (message_tx, message_rx) = worker_message_channel();
        message_tx
            .send(WorkerMessage::Message("user".into()))
            .unwrap();
        let (lifecycle_tx, lifecycle_rx) = worker_lifecycle_channel();
        lifecycle_tx.send(WorkerTimerLifecycleTransition::now(true));
        let ctx = test_worker_ctx(message_rx, lifecycle_rx);

        assert_eq!(
            recv_worker_inbound(&ctx).await.unwrap(),
            Some(WorkerInbound::Lifecycle {
                backgrounded: true,
                elapsed_micros: 0,
            })
        );
        assert_eq!(
            recv_worker_inbound(&ctx).await.unwrap(),
            Some(WorkerInbound::Message {
                data: "user".into(),
            })
        );
    }

    #[tokio::test(start_paused = true)]
    async fn lifecycle_bursts_coalesce_to_the_latest_state() {
        let (_message_tx, message_rx) = worker_message_channel();
        let (lifecycle_tx, lifecycle_rx) = worker_lifecycle_channel();
        for index in 0..101 {
            lifecycle_tx.send(WorkerTimerLifecycleTransition::now(index % 2 == 0));
        }
        let ctx = test_worker_ctx(message_rx, lifecycle_rx);

        assert_eq!(
            recv_worker_inbound(&ctx).await.unwrap(),
            Some(WorkerInbound::Lifecycle {
                backgrounded: true,
                elapsed_micros: 0,
            })
        );
    }

    #[tokio::test(start_paused = true)]
    async fn coalesced_hide_then_show_preserves_the_background_interval() {
        let (_message_tx, message_rx) = worker_message_channel();
        let (lifecycle_tx, lifecycle_rx) = worker_lifecycle_channel();
        let ctx = test_worker_ctx(message_rx, lifecycle_rx);

        lifecycle_tx.send(WorkerTimerLifecycleTransition::now(true));
        tokio::time::advance(Duration::from_secs(10)).await;
        lifecycle_tx.send(WorkerTimerLifecycleTransition::now(false));

        assert_eq!(
            recv_worker_inbound(&ctx).await.unwrap(),
            Some(WorkerInbound::Lifecycle {
                backgrounded: true,
                elapsed_micros: 10_000_000,
            })
        );
        assert_eq!(
            recv_worker_inbound(&ctx).await.unwrap(),
            Some(WorkerInbound::Lifecycle {
                backgrounded: false,
                elapsed_micros: 0,
            })
        );
    }

    #[tokio::test(start_paused = true)]
    async fn coalesced_hide_show_hide_preserves_completed_and_current_background_time() {
        let (lifecycle_tx, mut lifecycle_rx) = worker_lifecycle_channel();

        lifecycle_tx.send(WorkerTimerLifecycleTransition::now(true));
        tokio::time::advance(Duration::from_secs(10)).await;
        lifecycle_tx.send(WorkerTimerLifecycleTransition::now(false));
        tokio::time::advance(Duration::from_secs(5)).await;
        lifecycle_tx.send(WorkerTimerLifecycleTransition::now(true));
        tokio::time::advance(Duration::from_secs(5)).await;

        let transitions = [
            lifecycle_rx.try_recv().expect("completed hide"),
            lifecycle_rx.try_recv().expect("synthetic show"),
            lifecycle_rx.try_recv().expect("current hide"),
        ];
        assert_eq!(
            transitions.map(|transition| transition.backgrounded),
            [true, false, true]
        );
        let now = tokio::time::Instant::now();
        assert_eq!(
            transitions.map(|transition| now.duration_since(transition.occurred_at)),
            [
                Duration::from_secs(15),
                Duration::from_secs(5),
                Duration::from_secs(5),
            ]
        );
        assert!(lifecycle_rx.try_recv().is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn coalescing_after_a_delivered_hide_preserves_the_foreground_interval() {
        let (lifecycle_tx, mut lifecycle_rx) = worker_lifecycle_channel();

        lifecycle_tx.send(WorkerTimerLifecycleTransition::now(true));
        assert!(lifecycle_rx.try_recv().unwrap().backgrounded);
        tokio::time::advance(Duration::from_secs(10)).await;
        lifecycle_tx.send(WorkerTimerLifecycleTransition::now(false));
        tokio::time::advance(Duration::from_secs(5)).await;
        lifecycle_tx.send(WorkerTimerLifecycleTransition::now(true));
        tokio::time::advance(Duration::from_secs(10)).await;
        lifecycle_tx.send(WorkerTimerLifecycleTransition::now(false));

        let resumed = lifecycle_rx.try_recv().expect("compressed foreground");
        assert!(!resumed.backgrounded);
        assert_eq!(
            tokio::time::Instant::now().duration_since(resumed.occurred_at),
            Duration::from_secs(5),
            "five seconds of foreground time must advance Worker timers"
        );
        assert!(lifecycle_rx.try_recv().is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn coalescing_after_a_delivered_hide_can_end_hidden_without_losing_foreground_time() {
        let (lifecycle_tx, mut lifecycle_rx) = worker_lifecycle_channel();

        lifecycle_tx.send(WorkerTimerLifecycleTransition::now(true));
        assert!(lifecycle_rx.try_recv().unwrap().backgrounded);
        tokio::time::advance(Duration::from_secs(10)).await;
        lifecycle_tx.send(WorkerTimerLifecycleTransition::now(false));
        tokio::time::advance(Duration::from_secs(5)).await;
        lifecycle_tx.send(WorkerTimerLifecycleTransition::now(true));
        tokio::time::advance(Duration::from_secs(10)).await;
        lifecycle_tx.send(WorkerTimerLifecycleTransition::now(false));
        tokio::time::advance(Duration::from_secs(5)).await;
        lifecycle_tx.send(WorkerTimerLifecycleTransition::now(true));
        tokio::time::advance(Duration::from_secs(5)).await;

        let transitions = [
            lifecycle_rx.try_recv().expect("compressed foreground"),
            lifecycle_rx.try_recv().expect("current hide"),
        ];
        assert_eq!(
            transitions.map(|transition| transition.backgrounded),
            [false, true]
        );
        let now = tokio::time::Instant::now();
        assert_eq!(
            transitions.map(|transition| now.duration_since(transition.occurred_at)),
            [Duration::from_secs(15), Duration::from_secs(5)]
        );
        assert!(lifecycle_rx.try_recv().is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn every_short_coalesced_lifecycle_sequence_preserves_total_foreground_time() {
        for initial_backgrounded in [false, true] {
            for transition_count in 1..=10 {
                let (lifecycle_tx, mut lifecycle_rx) = worker_lifecycle_channel();
                let started_at = tokio::time::Instant::now();
                lifecycle_tx.send(WorkerTimerLifecycleTransition {
                    backgrounded: initial_backgrounded,
                    occurred_at: started_at,
                });
                assert_eq!(
                    lifecycle_rx.try_recv().unwrap().backgrounded,
                    initial_backgrounded
                );

                let mut expected_foreground = Duration::ZERO;
                let mut state = initial_backgrounded;
                let mut cursor = started_at;
                for index in 0..transition_count {
                    let occurred_at = cursor + Duration::from_secs((index % 3 + 1) as u64);
                    if !state {
                        expected_foreground += occurred_at.duration_since(cursor);
                    }
                    state = !state;
                    lifecycle_tx.send(WorkerTimerLifecycleTransition {
                        backgrounded: state,
                        occurred_at,
                    });
                    cursor = occurred_at;
                }
                let ended_at = cursor;

                let mut encoded_state = initial_backgrounded;
                let mut encoded_cursor = started_at;
                let mut encoded_foreground = Duration::ZERO;
                while let Some(transition) = lifecycle_rx.try_recv() {
                    assert!(transition.occurred_at >= encoded_cursor);
                    assert!(transition.occurred_at <= ended_at);
                    if !encoded_state {
                        encoded_foreground += transition.occurred_at.duration_since(encoded_cursor);
                    }
                    encoded_state = transition.backgrounded;
                    encoded_cursor = transition.occurred_at;
                }
                if !encoded_state {
                    encoded_foreground += ended_at.duration_since(encoded_cursor);
                }

                assert_eq!(encoded_state, state);
                assert_eq!(
                    encoded_foreground, expected_foreground,
                    "initial={initial_backgrounded}, transitions={transition_count}"
                );
            }
        }
    }

    #[test]
    fn inbound_events_have_non_user_control_shapes() {
        let lifecycle = deno_core::serde_json::to_value(WorkerInbound::Lifecycle {
            backgrounded: true,
            elapsed_micros: 2500,
        })
        .unwrap();
        assert_eq!(lifecycle["type"], "lifecycle");
        assert_eq!(lifecycle["backgrounded"], true);
        assert_eq!(lifecycle["elapsedMicros"], 2500);
        assert!(lifecycle.get("data").is_none());

        let message = deno_core::serde_json::to_value(WorkerInbound::Message {
            data: "payload".into(),
        })
        .unwrap();
        assert_eq!(message["type"], "message");
        assert_eq!(message["data"], "payload");
        assert!(message.get("backgrounded").is_none());
    }

    #[test]
    fn worker_initialization_errors_are_valid_json() {
        let encoded = worker_initialization_error("runtime bootstrap", &"bad \"hook\"");
        let decoded: deno_core::serde_json::Value =
            deno_core::serde_json::from_str(&encoded).expect("valid Worker error JSON");
        assert_eq!(
            decoded["message"],
            "Worker runtime bootstrap failed: bad \"hook\""
        );
    }

    #[test]
    fn worker_snapshot_extensions_match_eager_and_argument_order() {
        let (_eager_message_tx, eager_message_rx) = worker_message_channel();
        let (_eager_lifecycle_tx, eager_lifecycle_rx) = worker_lifecycle_channel();
        let eager_names: Vec<_> = create_worker_runtime_extensions(
            test_worker_ctx(eager_message_rx, eager_lifecycle_rx),
            test_host_state(Arc::new(AtomicBool::new(false))),
        )
        .into_iter()
        .map(|extension| extension.name)
        .collect();

        let lazy_names: Vec<_> = create_worker_runtime_lazy_extensions()
            .into_iter()
            .map(|extension| extension.name)
            .collect();

        let (_args_message_tx, args_message_rx) = worker_message_channel();
        let (_args_lifecycle_tx, args_lifecycle_rx) = worker_lifecycle_channel();
        let argument_names: Vec<_> = create_worker_runtime_extension_args(
            test_worker_ctx(args_message_rx, args_lifecycle_rx),
            test_host_state(Arc::new(AtomicBool::new(false))),
        )
        .into_iter()
        .map(|arguments| arguments.name)
        .collect();

        assert_eq!(lazy_names, eager_names);
        assert_eq!(argument_names, eager_names);
    }

    #[tokio::test(start_paused = true)]
    async fn worker_pump_consumes_lifecycle_and_freezes_timer_remainder() {
        let (message_tx, message_rx) = worker_message_channel();
        let (lifecycle_tx, lifecycle_rx) = worker_lifecycle_channel();
        let timer_backgrounded = Arc::new(AtomicBool::new(false));
        let ctx = test_worker_ctx(message_rx, lifecycle_rx);
        let mut rt = JsRuntime::new(RuntimeOptions {
            extensions: create_worker_runtime_extensions(
                ctx,
                test_host_state(timer_backgrounded.clone()),
            ),
            ..Default::default()
        });
        initialize_worker_runtime(&mut rt).expect("worker runtime bootstrap");

        exec(
            &mut rt,
            "globalThis.__workerMessages = 0; \
             globalThis.__workerTimer = 0; \
             worker.onMessage(() => __workerMessages++); \
             setTimeout(() => __workerTimer++, 100)",
        );
        advance_and_drain(&mut rt, Duration::from_millis(30)).await;

        timer_backgrounded.store(true, std::sync::atomic::Ordering::Release);
        lifecycle_tx.send(WorkerTimerLifecycleTransition::now(true));
        drain_ready(&mut rt).await;
        advance_and_drain(&mut rt, Duration::from_secs(10)).await;
        assert_js(&mut rt, "__workerTimer === 0 && __workerMessages === 0");

        timer_backgrounded.store(false, std::sync::atomic::Ordering::Release);
        lifecycle_tx.send(WorkerTimerLifecycleTransition::now(false));
        drain_ready(&mut rt).await;
        advance_and_drain(&mut rt, Duration::from_millis(69)).await;
        assert_js(&mut rt, "__workerTimer === 0 && __workerMessages === 0");
        advance_and_drain(&mut rt, Duration::from_millis(1)).await;
        assert_js(&mut rt, "__workerTimer === 1 && __workerMessages === 0");

        drop(message_tx);
    }

    #[tokio::test(start_paused = true)]
    async fn worker_pump_uses_transition_time_when_lifecycle_delivery_is_delayed() {
        let (message_tx, message_rx) = worker_message_channel();
        let (lifecycle_tx, lifecycle_rx) = worker_lifecycle_channel();
        let timer_backgrounded = Arc::new(AtomicBool::new(false));
        let ctx = test_worker_ctx(message_rx, lifecycle_rx);
        let mut rt = JsRuntime::new(RuntimeOptions {
            extensions: create_worker_runtime_extensions(
                ctx,
                test_host_state(timer_backgrounded.clone()),
            ),
            ..Default::default()
        });
        initialize_worker_runtime(&mut rt).expect("worker runtime bootstrap");

        exec(
            &mut rt,
            "globalThis.__workerMessages = 0; \
             globalThis.__workerTimer = 0; \
             worker.onMessage(() => __workerMessages++); \
             setTimeout(() => __workerTimer++, 100)",
        );
        advance_and_drain(&mut rt, Duration::from_millis(30)).await;

        timer_backgrounded.store(true, std::sync::atomic::Ordering::Release);
        lifecycle_tx.send(WorkerTimerLifecycleTransition::now(true));
        tokio::time::advance(Duration::from_secs(10)).await;
        timer_backgrounded.store(false, std::sync::atomic::Ordering::Release);
        lifecycle_tx.send(WorkerTimerLifecycleTransition::now(false));
        drain_ready(&mut rt).await;

        assert_js(&mut rt, "__workerTimer === 0 && __workerMessages === 0");
        advance_and_drain(&mut rt, Duration::from_millis(69)).await;
        assert_js(&mut rt, "__workerTimer === 0 && __workerMessages === 0");
        advance_and_drain(&mut rt, Duration::from_millis(1)).await;
        assert_js(&mut rt, "__workerTimer === 1 && __workerMessages === 0");

        drop(message_tx);
    }

    #[tokio::test(start_paused = true)]
    async fn worker_created_hidden_keeps_its_first_timer_logical_until_show() {
        let (message_tx, message_rx) = worker_message_channel();
        let (lifecycle_tx, lifecycle_rx) = worker_lifecycle_channel();
        let timer_backgrounded = Arc::new(AtomicBool::new(true));
        let ctx = test_worker_ctx(message_rx, lifecycle_rx);
        let mut rt = JsRuntime::new(RuntimeOptions {
            extensions: create_worker_runtime_extensions(
                ctx,
                test_host_state(timer_backgrounded.clone()),
            ),
            ..Default::default()
        });
        initialize_worker_runtime(&mut rt).expect("worker runtime bootstrap");

        exec(
            &mut rt,
            "globalThis.__workerTimer = 0; setTimeout(() => __workerTimer++, 100)",
        );
        advance_and_drain(&mut rt, Duration::from_secs(10)).await;
        assert_js(&mut rt, "__workerTimer === 0");

        // A Worker created after the host entered the background has no prior
        // hide edge in its per-worker queue. The shared level initializes its
        // timer registry; the first queued edge is therefore show.
        timer_backgrounded.store(false, std::sync::atomic::Ordering::Release);
        lifecycle_tx.send(WorkerTimerLifecycleTransition::now(false));
        drain_ready(&mut rt).await;
        advance_and_drain(&mut rt, Duration::from_millis(99)).await;
        assert_js(&mut rt, "__workerTimer === 0");
        advance_and_drain(&mut rt, Duration::from_millis(1)).await;
        assert_js(&mut rt, "__workerTimer === 1");

        drop(message_tx);
    }
}

/// R4: Worker runaway protection via the one process deadline watchdog.
#[cfg(all(test, feature = "v8-limits"))]
mod watchdog_worker_tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicBool;
    use std::time::Duration;

    use deno_core::{FastString, JsRuntime, RuntimeOptions};
    use shared::{
        channel::ThreadWakeup,
        device::gpu_caps::GpuCaps,
        op_state::{AudioSender, HostOpState, NetworkPolicy},
        render_command_sender::CommandSender,
    };

    use crate::watchdog::{DeadlineWatchdog, DeadlineWatchdogConfig, Scheduler};

    fn wt_host_state() -> HostOpState {
        let (render_tx, _render_rx) = CommandSender::new();
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
        }
    }

    fn build_worker_rt(loader: Option<Rc<dyn ModuleLoader>>) -> JsRuntime {
        let (_tx, rx_from_main) = worker_message_channel();
        let (_ltx, lifecycle_rx) = worker_lifecycle_channel();
        let (tx_to_main, _rx_to_main) = worker_outbound_channel();
        let (tx_errors, _rx_errors) = worker_error_channel();
        let ctx = WorkerCtx {
            tx_to_main,
            tx_errors,
            rx_from_main: Arc::new(tokio::sync::Mutex::new(rx_from_main)),
            timer_backgrounded_rx: Arc::new(tokio::sync::Mutex::new(lifecycle_rx)),
        };
        let mut rt = JsRuntime::new(RuntimeOptions {
            module_loader: loader,
            extensions: create_worker_runtime_extensions(ctx, wt_host_state()),
            ..Default::default()
        });
        initialize_worker_runtime(&mut rt).expect("worker runtime bootstrap");
        rt
    }

    fn unique_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("migo-worker-wd-{tag}-{nanos}"))
    }

    #[tokio::test]
    async fn worker_top_level_infinite_loop_is_terminated_and_reports_once() {
        let dir = unique_dir("toplevel");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("main.js"), "while (true) {}").unwrap();

        let mut rt = build_worker_rt(Some(Rc::new(FsModuleLoader)));
        let sched = Scheduler::new_test();
        let (err_tx, mut err_rx) = worker_error_channel();
        let obs_tx = err_tx.clone();
        let config = DeadlineWatchdogConfig::new(Duration::from_millis(200), "worker-test")
            .with_observer(Arc::new(move |_| {
                let _ = obs_tx.send("watchdog-timeout".to_string());
            }));
        let wd = DeadlineWatchdog::register_isolate_on(
            sched,
            rt.v8_isolate().thread_safe_handle(),
            config,
        );

        let resolved = resolve_path("main.js", &dir).unwrap();
        let module_id = worker_load_module(&mut rt, Some(&wd), &resolved)
            .await
            .expect("module load (compile) succeeds; top-level runs during evaluate");
        let eval = worker_evaluate_module(&mut rt, Some(&wd), module_id).await;
        assert!(
            eval.is_err(),
            "a top-level infinite loop must be terminated"
        );
        assert!(wd.timed_out());

        // Mirror the run block: the eval error is suppressed because the observer
        // already reported the timeout exactly once.
        if let Err(e) = eval {
            report_worker_error(Some(&wd), &err_tx, format!("eval error: {e}"));
        }
        let first = tokio::time::timeout(Duration::from_secs(2), err_rx.recv())
            .await
            .expect("the observer must report the timeout")
            .expect("message present");
        assert_eq!(first, "watchdog-timeout");
        assert!(
            err_rx.try_recv().is_err(),
            "exactly one timeout error; the eval error must be suppressed"
        );
    }

    #[tokio::test]
    async fn worker_message_handler_infinite_loop_is_terminated() {
        let mut rt = build_worker_rt(None);
        let sched = Scheduler::new_test();
        let config = DeadlineWatchdogConfig::new(Duration::from_millis(200), "worker-test");
        let wd = DeadlineWatchdog::register_isolate_on(
            sched,
            rt.v8_isolate().thread_safe_handle(),
            config,
        );

        // A macrotask that never yields; it runs inside the guarded event loop,
        // not during the (unguarded) setup script.
        rt.execute_script(
            "<setup>",
            FastString::from_static("setTimeout(() => { while (true) {} }, 0);"),
        )
        .unwrap();

        let result = worker_run_event_loop(&mut rt, Some(&wd)).await;
        assert!(
            result.is_err(),
            "a runaway handler running in the event loop must be terminated"
        );
        assert!(wd.timed_out());
    }

    #[tokio::test]
    async fn worker_watchdog_unregisters_on_drop() {
        let mut rt = build_worker_rt(None);
        let sched = Scheduler::new_test();
        let wd = DeadlineWatchdog::register_isolate_on(
            sched,
            rt.v8_isolate().thread_safe_handle(),
            DeadlineWatchdogConfig::new(Duration::from_secs(10), "worker-test"),
        );
        assert_eq!(sched.registered_len(), 1);
        drop(wd);
        assert_eq!(
            sched.registered_len(),
            0,
            "dropping the watchdog must unregister the target (no leak on any exit path)"
        );
    }

    #[test]
    fn worker_uses_shared_scheduler_not_a_local_monitor() {
        let src = include_str!("mod.rs");
        // Forbidden symbols (split via concat! so this test's own text does not
        // match the needle).
        assert!(
            !src.contains(concat!("Migo-", "WorkerWatchdog")),
            "the per-Worker monitor thread must be gone"
        );
        assert!(
            !src.contains(concat!("WORKER_WATCHDOG_", "CHECK_INTERVAL")),
            "the periodic check interval constant must be gone"
        );
        assert!(
            !src.contains(concat!("WORKER_WATCHDOG_", "TIMEOUT")),
            "the old timeout constant must be gone"
        );
        assert!(
            !src.contains(concat!("spawn_worker_", "watchdog")),
            "the per-Worker watchdog spawner must be gone"
        );
        assert!(
            !src.contains(concat!("struct Worker", "Watchdog")),
            "the per-Worker heartbeat struct must be gone"
        );
        assert!(
            !src.contains(concat!("tokio::time::", "interval")),
            "the one-second ticker task must be gone"
        );
        // Required new wiring.
        assert!(
            src.contains(concat!("DeadlineWatchdog::register_", "isolate")),
            "the Worker must register with the shared process scheduler"
        );
        assert!(
            src.contains(concat!("poll_", "guarded")),
            "the Worker must guard its module-load/event-loop polls"
        );
        // Main-thread force-terminate stays independent.
        assert!(
            src.contains(concat!("fn force_", "terminate")),
            "WorkerHandle::force_terminate must remain"
        );
    }
}

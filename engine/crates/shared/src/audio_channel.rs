//! The audio command transport: a bounded, lossless queue from the thread
//! running the game to the audio thread.
//!
//! **Why it is bounded, and why bounding it is not the input queue's problem
//! again.** The steady-state bounded-queue requirement forbids unbounded queue growth under saturation. This
//! queue used to be `tokio::sync::mpsc::unbounded_channel`, drained at most
//! [`AUDIO_COMMANDS_PER_DRAIN`] commands per audio-thread iteration with the rest
//! deferred — an unbounded queue behind a capped drain, which is that growth
//! shape exactly, and the drain's own reason for existing names the producer that
//! reaches it: a game firing rapid bursts of automation or sound effects.
//!
//! The data queue is nonreplaceable: `AudioCmd` carries ids allocated on the
//! JavaScript side with fire-and-forget creates, so ordering *is* the protocol:
//! drop one data command and a later one addresses a node that was never
//! created. Lifecycle level changes (`PauseAll` / `ResumeAll`) are different:
//! they are coalesced to the latest level on their separate control lane.
//!
//! Saturation is reported to the producer without waiting. Request/response
//! commands are returned intact so the V8 op can distinguish `Full` from
//! `Disconnected` and finish its future immediately. Shutdown uses a separate
//! reserved control slot, so a full data queue cannot prevent teardown.

use crate::protocol::audio_cmd::AudioCmd;

/// How many commands the audio thread takes from the queue in one iteration.
///
/// The cap exists so a burst cannot starve mixing: whatever is left waits for the
/// next iteration, which a send's own wakeup brings forward immediately.
pub const AUDIO_COMMANDS_PER_DRAIN: usize = 256;

/// How many commands the queue holds before a send is refused.
///
/// Derived rather than chosen: a full queue must empty within a small fixed
/// number of consumer iterations. Four drains is the number.
pub const AUDIO_COMMAND_CAPACITY: usize = 4 * AUDIO_COMMANDS_PER_DRAIN;

/// Heap payload retained by ordinary commands waiting for the audio thread.
pub const MAX_AUDIO_COMMAND_QUEUED_BYTES: usize = 64 * 1024 * 1024;

// A capacity below one drain would leave a producer waiting a whole iteration per
// command, which is a bound in name only.
const _: () = assert!(AUDIO_COMMAND_CAPACITY >= AUDIO_COMMANDS_PER_DRAIN);

/// One notification is enough because pause/resume stores the latest level.
const AUDIO_CONTROL_NOTIFICATION_CAPACITY: usize = 1;
const AUDIO_CLEANUP_NOTIFICATION_CAPACITY: usize = 1;
const AUDIO_SHUTDOWN_CAPACITY: usize = 1;
const LIFECYCLE_NONE: u8 = 0;
const LIFECYCLE_PAUSED: u8 = 1;
const LIFECYCLE_RUNNING: u8 = 2;
const CLEANUP_PENDING: u8 = 0;
const CLEANUP_COMPLETE: u8 = 1;
const CLEANUP_DISCONNECTED: u8 = 2;
const LANE_CONTROL: u8 = 0;
const LANE_DATA: u8 = 1;
const LANE_CLEANUP: u8 = 2;
const NONTERMINAL_LANE_COUNT: u8 = 3;

struct AudioLifecycleState {
    latest: std::sync::atomic::AtomicU8,
}

struct AudioCleanupCycle {
    status: std::sync::atomic::AtomicU8,
    notify: tokio::sync::Notify,
}

struct AudioCleanupState {
    closed: std::sync::atomic::AtomicBool,
    data_fenced: std::sync::atomic::AtomicBool,
    publish_lock: std::sync::Mutex<()>,
    active: std::sync::Mutex<Option<std::sync::Arc<AudioCleanupCycle>>>,
}

/// Completion handle for one coalesced release-all barrier.
///
/// Cloning/requesting while a barrier is pending shares this fixed-size cycle;
/// the transport never allocates an unbounded list of acknowledgements.
#[derive(Clone)]
pub struct AudioCleanupTicket {
    cycle: std::sync::Arc<AudioCleanupCycle>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioCleanupWaitError {
    Disconnected,
}

impl AudioCleanupTicket {
    #[inline]
    pub fn is_complete(&self) -> bool {
        self.cycle.status.load(std::sync::atomic::Ordering::Acquire) == CLEANUP_COMPLETE
    }

    #[inline]
    pub fn is_disconnected(&self) -> bool {
        self.cycle.status.load(std::sync::atomic::Ordering::Acquire) == CLEANUP_DISCONNECTED
    }

    pub async fn wait(&self) -> Result<(), AudioCleanupWaitError> {
        loop {
            let notified = self.cycle.notify.notified();
            match self.cycle.status.load(std::sync::atomic::Ordering::Acquire) {
                CLEANUP_COMPLETE => return Ok(()),
                CLEANUP_DISCONNECTED => return Err(AudioCleanupWaitError::Disconnected),
                CLEANUP_PENDING => notified.await,
                _ => unreachable!("invalid audio cleanup status"),
            }
        }
    }
}

impl AudioCleanupState {
    fn new() -> Self {
        Self {
            closed: std::sync::atomic::AtomicBool::new(false),
            data_fenced: std::sync::atomic::AtomicBool::new(false),
            publish_lock: std::sync::Mutex::new(()),
            active: std::sync::Mutex::new(None),
        }
    }

    fn complete_active(&self) {
        let active = self
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(cycle) = active.as_ref() {
            cycle
                .status
                .store(CLEANUP_COMPLETE, std::sync::atomic::Ordering::Release);
            cycle.notify.notify_waiters();
        }
    }

    fn disconnect_active(&self) {
        let cycle = self
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(cycle) = cycle {
            cycle
                .status
                .store(CLEANUP_DISCONNECTED, std::sync::atomic::Ordering::Release);
            cycle.notify.notify_waiters();
        }
    }
}

struct AudioQueueUsage {
    max_items: usize,
    max_bytes: usize,
    closed: std::sync::atomic::AtomicBool,
    items: std::sync::atomic::AtomicUsize,
    bytes: std::sync::atomic::AtomicUsize,
}

/// Reservation acquired before a V8 payload is copied into an `AudioCmd`.
pub struct AudioCommandPermit {
    usage: std::sync::Arc<AudioQueueUsage>,
    bytes: usize,
}

impl AudioCommandPermit {
    /// Reduce this reservation to the bytes actually retained by its command.
    ///
    /// This never widens a reservation: a capacity observed after copying can
    /// safely be lower than the conservative pre-copy estimate, but a larger
    /// value remains charged at the original amount.
    #[inline]
    pub fn shrink_to(&mut self, actual_charge: usize) {
        if actual_charge >= self.bytes {
            return;
        }
        let released = self.bytes - actual_charge;
        self.usage
            .bytes
            .fetch_update(
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
                |used| Some(used.saturating_sub(released)),
            )
            .ok();
        self.bytes = actual_charge;
    }
}

impl Drop for AudioCommandPermit {
    fn drop(&mut self) {
        self.usage
            .bytes
            .fetch_update(
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
                |used| Some(used.saturating_sub(self.bytes)),
            )
            .ok();
        self.usage
            .items
            .fetch_update(
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
                |items| Some(items.saturating_sub(1)),
            )
            .ok();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioCommandReserveError {
    Full,
    ByteLimit,
    Disconnected,
}

pub enum AudioCommandSendError {
    Full(AudioCmd),
    ByteLimit(AudioCmd),
    Disconnected(AudioCmd),
}

impl std::fmt::Debug for AudioCommandSendError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Full(_) => "Full(..)",
            Self::ByteLimit(_) => "ByteLimit(..)",
            Self::Disconnected(_) => "Disconnected(..)",
        })
    }
}

impl AudioQueueUsage {
    fn try_reserve(
        self: &std::sync::Arc<Self>,
        bytes: usize,
    ) -> Result<AudioCommandPermit, AudioCommandReserveError> {
        if self.closed.load(std::sync::atomic::Ordering::Acquire) {
            return Err(AudioCommandReserveError::Disconnected);
        }
        self.items
            .fetch_update(
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
                |items| (items < self.max_items).then_some(items + 1),
            )
            .map_err(|_| AudioCommandReserveError::Full)?;

        if self
            .bytes
            .fetch_update(
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
                |used| {
                    used.checked_add(bytes)
                        .filter(|total| *total <= self.max_bytes)
                },
            )
            .is_err()
        {
            self.items
                .fetch_update(
                    std::sync::atomic::Ordering::AcqRel,
                    std::sync::atomic::Ordering::Acquire,
                    |items| Some(items.saturating_sub(1)),
                )
                .ok();
            return Err(AudioCommandReserveError::ByteLimit);
        }

        let permit = AudioCommandPermit {
            usage: std::sync::Arc::clone(self),
            bytes,
        };
        if self.closed.load(std::sync::atomic::Ordering::Acquire) {
            drop(permit);
            return Err(AudioCommandReserveError::Disconnected);
        }
        Ok(permit)
    }
}

struct AudioCommandEntry {
    command: AudioCmd,
    _permit: AudioCommandPermit,
}

impl AudioCommandEntry {
    fn into_command(self) -> AudioCmd {
        let Self { command, _permit } = self;
        drop(_permit);
        command
    }
}

pub struct AudioCommandSender {
    data: crossbeam_channel::Sender<AudioCommandEntry>,
    control: crossbeam_channel::Sender<()>,
    cleanup: crossbeam_channel::Sender<()>,
    shutdown: crossbeam_channel::Sender<AudioCmd>,
    usage: std::sync::Arc<AudioQueueUsage>,
    lifecycle: std::sync::Arc<AudioLifecycleState>,
    cleanup_state: std::sync::Arc<AudioCleanupState>,
}

impl Clone for AudioCommandSender {
    fn clone(&self) -> Self {
        Self {
            data: self.data.clone(),
            control: self.control.clone(),
            cleanup: self.cleanup.clone(),
            shutdown: self.shutdown.clone(),
            usage: std::sync::Arc::clone(&self.usage),
            lifecycle: std::sync::Arc::clone(&self.lifecycle),
            cleanup_state: std::sync::Arc::clone(&self.cleanup_state),
        }
    }
}

pub struct AudioCommandReceiver {
    data: crossbeam_channel::Receiver<AudioCommandEntry>,
    control: crossbeam_channel::Receiver<()>,
    cleanup: crossbeam_channel::Receiver<()>,
    shutdown: crossbeam_channel::Receiver<AudioCmd>,
    usage: std::sync::Arc<AudioQueueUsage>,
    lifecycle: std::sync::Arc<AudioLifecycleState>,
    cleanup_state: std::sync::Arc<AudioCleanupState>,
    next_lane: std::sync::atomic::AtomicU8,
}

impl Drop for AudioCommandReceiver {
    fn drop(&mut self) {
        // Linearize receiver teardown with an admitted sender's commit+send.
        // Once this lock is held, no callback can detach a V8 backing store
        // and then discover that the reserved data lane was disconnected.
        let _publish = self
            .cleanup_state
            .publish_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.cleanup_state
            .closed
            .store(true, std::sync::atomic::Ordering::Release);
        self.cleanup_state.disconnect_active();
        self.usage
            .closed
            .store(true, std::sync::atomic::Ordering::Release);
        self.usage
            .items
            .store(0, std::sync::atomic::Ordering::Release);
        self.usage
            .bytes
            .store(0, std::sync::atomic::Ordering::Release);
    }
}

impl AudioCommandSender {
    /// Reserve both one item and its retained payload before making a V8 copy.
    #[inline]
    pub fn try_reserve_data(
        &self,
        bytes: usize,
    ) -> Result<AudioCommandPermit, AudioCommandReserveError> {
        if self
            .cleanup_state
            .data_fenced
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(AudioCommandReserveError::Full);
        }
        self.usage.try_reserve(bytes)
    }

    /// Send a command using a reservation acquired before its payload copy.
    #[inline]
    pub fn try_send_reserved(
        &self,
        command: AudioCmd,
        permit: AudioCommandPermit,
    ) -> Result<(), AudioCommandSendError> {
        self.try_send_reserved_committing(command, permit, || {})
    }

    /// Publish an already-reserved data command while atomically committing an
    /// external ownership transfer (for example V8 backing-store detachment).
    ///
    /// The closure runs only after the cleanup fence and receiver liveness have
    /// been checked, under the same publication lock as receiver teardown; a
    /// successful reservation then makes `try_send` non-failing in normal
    /// operation. Callers must make every other fallible step before entering.
    #[inline]
    pub fn try_send_reserved_committing<F>(
        &self,
        command: AudioCmd,
        permit: AudioCommandPermit,
        commit: F,
    ) -> Result<(), AudioCommandSendError>
    where
        F: FnOnce(),
    {
        if matches!(
            command,
            AudioCmd::PauseAll
                | AudioCmd::ResumeAll
                | AudioCmd::ReleaseAllContexts
                | AudioCmd::Shutdown
        ) {
            drop(permit);
            return self.try_send(command);
        }

        let _publish = self
            .cleanup_state
            .publish_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self
            .cleanup_state
            .data_fenced
            .load(std::sync::atomic::Ordering::Acquire)
        {
            drop(permit);
            return Err(AudioCommandSendError::Full(command));
        }

        if command.queued_payload_bytes() > permit.bytes {
            drop(permit);
            return Err(AudioCommandSendError::ByteLimit(command));
        }

        if self
            .cleanup_state
            .closed
            .load(std::sync::atomic::Ordering::Acquire)
            || self.usage.closed.load(std::sync::atomic::Ordering::Acquire)
        {
            drop(permit);
            return Err(AudioCommandSendError::Disconnected(command));
        }

        commit();

        match self.data.try_send(AudioCommandEntry {
            command,
            _permit: permit,
        }) {
            Ok(()) => Ok(()),
            Err(crossbeam_channel::TrySendError::Full(entry)) => {
                Err(AudioCommandSendError::Full(entry.into_command()))
            }
            Err(crossbeam_channel::TrySendError::Disconnected(entry)) => {
                Err(AudioCommandSendError::Disconnected(entry.into_command()))
            }
        }
    }

    /// Never blocks. Lifecycle traffic and shutdown cannot consume data slots.
    #[inline]
    pub fn try_send(&self, command: AudioCmd) -> Result<(), AudioCommandSendError> {
        match command {
            AudioCmd::Shutdown => {
                self.shutdown
                    .try_send(AudioCmd::Shutdown)
                    .map_err(|error| match error {
                        crossbeam_channel::TrySendError::Full(command) => {
                            AudioCommandSendError::Full(command)
                        }
                        crossbeam_channel::TrySendError::Disconnected(command) => {
                            AudioCommandSendError::Disconnected(command)
                        }
                    })
            }
            AudioCmd::PauseAll => self.try_send_lifecycle(LIFECYCLE_PAUSED, command),
            AudioCmd::ResumeAll => self.try_send_lifecycle(LIFECYCLE_RUNNING, command),
            AudioCmd::ReleaseAllContexts => self.request_release_all_contexts().map(|_ticket| ()),
            command => {
                let permit = match self.try_reserve_data(command.queued_payload_bytes()) {
                    Ok(permit) => permit,
                    Err(AudioCommandReserveError::Full) => {
                        return Err(AudioCommandSendError::Full(command));
                    }
                    Err(AudioCommandReserveError::ByteLimit) => {
                        return Err(AudioCommandSendError::ByteLimit(command));
                    }
                    Err(AudioCommandReserveError::Disconnected) => {
                        return Err(AudioCommandSendError::Disconnected(command));
                    }
                };
                self.try_send_reserved(command, permit)
            }
        }
    }

    fn try_send_lifecycle(
        &self,
        latest: u8,
        command: AudioCmd,
    ) -> Result<(), AudioCommandSendError> {
        if self.usage.closed.load(std::sync::atomic::Ordering::Acquire) {
            return Err(AudioCommandSendError::Disconnected(command));
        }
        self.lifecycle
            .latest
            .store(latest, std::sync::atomic::Ordering::Release);
        match self.control.try_send(()) {
            Ok(()) | Err(crossbeam_channel::TrySendError::Full(())) => Ok(()),
            Err(crossbeam_channel::TrySendError::Disconnected(())) => {
                Err(AudioCommandSendError::Disconnected(command))
            }
        }
    }

    /// Request a must-deliver restart cleanup independently of ordinary data
    /// saturation. Repeated callers share one pending barrier and acknowledgement.
    pub fn request_release_all_contexts(
        &self,
    ) -> Result<AudioCleanupTicket, AudioCommandSendError> {
        let command = AudioCmd::ReleaseAllContexts;
        if self
            .cleanup_state
            .closed
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(AudioCommandSendError::Disconnected(command));
        }

        // Linearize the barrier against ordinary publication. Any data send
        // that completed before this lock is in the queue the handler drains;
        // any sender that publishes afterwards observes `data_fenced`.
        let _publish = self
            .cleanup_state
            .publish_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut active = self
            .cleanup_state
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self
            .cleanup_state
            .closed
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(AudioCommandSendError::Disconnected(command));
        }
        if let Some(cycle) = active.as_ref() {
            return Ok(AudioCleanupTicket {
                cycle: std::sync::Arc::clone(cycle),
            });
        }

        let cycle = std::sync::Arc::new(AudioCleanupCycle {
            status: std::sync::atomic::AtomicU8::new(CLEANUP_PENDING),
            notify: tokio::sync::Notify::new(),
        });
        self.cleanup_state
            .data_fenced
            .store(true, std::sync::atomic::Ordering::Release);
        *active = Some(std::sync::Arc::clone(&cycle));
        match self.cleanup.try_send(()) {
            Ok(()) | Err(crossbeam_channel::TrySendError::Full(())) => {
                Ok(AudioCleanupTicket { cycle })
            }
            Err(crossbeam_channel::TrySendError::Disconnected(())) => {
                *active = None;
                self.cleanup_state
                    .closed
                    .store(true, std::sync::atomic::Ordering::Release);
                self.cleanup_state
                    .data_fenced
                    .store(false, std::sync::atomic::Ordering::Release);
                cycle
                    .status
                    .store(CLEANUP_DISCONNECTED, std::sync::atomic::Ordering::Release);
                cycle.notify.notify_waiters();
                Err(AudioCommandSendError::Disconnected(command))
            }
        }
    }

    /// Reopen ordinary data publication after the Host has retired every old
    /// producer. A pending/unacknowledged cleanup stays fail-closed.
    pub fn finish_release_all_contexts(&self) {
        let _publish = self
            .cleanup_state
            .publish_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut active = self
            .cleanup_state
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let completed = active.as_ref().is_some_and(|cycle| {
            cycle.status.load(std::sync::atomic::Ordering::Acquire) == CLEANUP_COMPLETE
        });
        if completed {
            active.take();
            self.cleanup_state
                .data_fenced
                .store(false, std::sync::atomic::Ordering::Release);
        }
    }

    #[inline]
    pub fn capacity(&self) -> Option<usize> {
        self.data.capacity()
    }
}

impl AudioCommandReceiver {
    /// Terminal control takes priority over ordinary data backlog.
    #[inline]
    pub fn try_recv(&self) -> Result<AudioCmd, crossbeam_channel::TryRecvError> {
        loop {
            let shutdown_disconnected = match self.shutdown.try_recv() {
                Ok(command) => return Ok(command),
                Err(crossbeam_channel::TryRecvError::Empty) => false,
                Err(crossbeam_channel::TryRecvError::Disconnected) => true,
            };

            let start =
                self.next_lane.load(std::sync::atomic::Ordering::Acquire) % NONTERMINAL_LANE_COUNT;
            let mut all_disconnected = shutdown_disconnected;
            let mut retry_stale_lifecycle = false;
            for offset in 0..NONTERMINAL_LANE_COUNT {
                let lane = (start + offset) % NONTERMINAL_LANE_COUNT;
                let result = match lane {
                    LANE_CONTROL => match self.control.try_recv() {
                        Ok(()) => {
                            let command = match self
                                .lifecycle
                                .latest
                                .swap(LIFECYCLE_NONE, std::sync::atomic::Ordering::AcqRel)
                            {
                                LIFECYCLE_PAUSED => Some(AudioCmd::PauseAll),
                                LIFECYCLE_RUNNING => Some(AudioCmd::ResumeAll),
                                LIFECYCLE_NONE => {
                                    retry_stale_lifecycle = true;
                                    None
                                }
                                _ => unreachable!("invalid audio lifecycle level"),
                            };
                            command.map(Ok)
                        }
                        Err(error) => Some(Err(error)),
                    },
                    LANE_DATA => Some(self.data.try_recv().map(AudioCommandEntry::into_command)),
                    LANE_CLEANUP => Some(
                        self.cleanup
                            .try_recv()
                            .map(|()| AudioCmd::ReleaseAllContexts),
                    ),
                    _ => unreachable!("invalid audio command lane"),
                };

                match result {
                    Some(Ok(command)) => {
                        self.next_lane.store(
                            (lane + 1) % NONTERMINAL_LANE_COUNT,
                            std::sync::atomic::Ordering::Release,
                        );
                        return Ok(command);
                    }
                    Some(Err(crossbeam_channel::TryRecvError::Empty)) | None => {
                        all_disconnected = false;
                    }
                    Some(Err(crossbeam_channel::TryRecvError::Disconnected)) => {}
                }
            }
            if retry_stale_lifecycle {
                continue;
            }
            return if all_disconnected {
                Err(crossbeam_channel::TryRecvError::Disconnected)
            } else {
                Err(crossbeam_channel::TryRecvError::Empty)
            };
        }
    }

    /// While an older startup backlog exists, only shutdown and the restart
    /// barrier may jump ahead of it. Ordinary channel data still keeps FIFO
    /// ordering behind the backlog.
    pub fn try_recv_urgent(&self) -> Result<AudioCmd, crossbeam_channel::TryRecvError> {
        match self.shutdown.try_recv() {
            Ok(command) => return Ok(command),
            Err(crossbeam_channel::TryRecvError::Empty)
            | Err(crossbeam_channel::TryRecvError::Disconnected) => {}
        }
        self.cleanup
            .try_recv()
            .map(|()| AudioCmd::ReleaseAllContexts)
    }

    /// Drop every ordinary command currently waiting behind a restart barrier.
    /// Dropping entries releases response senders and queue byte/count permits.
    pub fn discard_data_queue(&self) {
        while let Ok(entry) = self.data.try_recv() {
            drop(entry);
        }
    }

    /// Pre-start restart cleanup: no native context exists, so discard all
    /// nonterminal queued work and consume the cleanup notification directly.
    pub fn discard_prestart_commands(&self) {
        self.discard_data_queue();
        while self.control.try_recv().is_ok() {}
        self.lifecycle
            .latest
            .store(LIFECYCLE_NONE, std::sync::atomic::Ordering::Release);
        while self.cleanup.try_recv().is_ok() {}
    }

    /// Publish the acknowledgement only after the caller has completed cleanup.
    pub fn complete_release_all_contexts(&self) {
        self.cleanup_state.complete_active();
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.data.len() + self.control.len() + self.cleanup.len() + self.shutdown.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// The transport the audio thread is built on.
pub fn channel() -> (AudioCommandSender, AudioCommandReceiver) {
    channel_with_limits(AUDIO_COMMAND_CAPACITY, MAX_AUDIO_COMMAND_QUEUED_BYTES)
}

fn channel_with_limits(
    item_capacity: usize,
    byte_capacity: usize,
) -> (AudioCommandSender, AudioCommandReceiver) {
    let (data_tx, data_rx) = crossbeam_channel::bounded(item_capacity);
    let (control_tx, control_rx) = crossbeam_channel::bounded(AUDIO_CONTROL_NOTIFICATION_CAPACITY);
    let (cleanup_tx, cleanup_rx) = crossbeam_channel::bounded(AUDIO_CLEANUP_NOTIFICATION_CAPACITY);
    let (shutdown_tx, shutdown_rx) = crossbeam_channel::bounded(AUDIO_SHUTDOWN_CAPACITY);
    let usage = std::sync::Arc::new(AudioQueueUsage {
        max_items: item_capacity,
        max_bytes: byte_capacity,
        closed: std::sync::atomic::AtomicBool::new(false),
        items: std::sync::atomic::AtomicUsize::new(0),
        bytes: std::sync::atomic::AtomicUsize::new(0),
    });
    let lifecycle = std::sync::Arc::new(AudioLifecycleState {
        latest: std::sync::atomic::AtomicU8::new(LIFECYCLE_NONE),
    });
    let cleanup_state = std::sync::Arc::new(AudioCleanupState::new());
    (
        AudioCommandSender {
            data: data_tx,
            control: control_tx,
            cleanup: cleanup_tx,
            shutdown: shutdown_tx,
            usage: std::sync::Arc::clone(&usage),
            lifecycle: std::sync::Arc::clone(&lifecycle),
            cleanup_state: std::sync::Arc::clone(&cleanup_state),
        },
        AudioCommandReceiver {
            data: data_rx,
            control: control_rx,
            cleanup: cleanup_rx,
            shutdown: shutdown_rx,
            usage,
            lifecycle,
            cleanup_state,
            next_lane: std::sync::atomic::AtomicU8::new(LANE_CONTROL),
        },
    )
}

/// A sender with no consumer and no queue, for a build that has no audio
/// subsystem to send to.
///
/// **A queue nobody drains is the thing this must not be.** The profile without
/// `api-media` used to hold a live receiver it never read, so a send would have
/// queued for the life of the session; that was harmless only because the audio
/// ops are compiled out of that profile and nothing could reach it. Behind a
/// bounded channel the same shape is worse than a leak — the first producer to
/// fill it waits forever. With no receiver at all, a send fails at once and
/// hands the command back, and the reason it is safe stops being a fact about
/// which ops happen to be registered.
pub fn disconnected() -> AudioCommandSender {
    // Zero capacity: there is no consumer, so there is no queue worth holding
    // slots for either.
    let (tx, rx) = channel();
    drop(rx);
    tx
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::ThreadWakeup;
    use crate::op_state::AudioSender;
    use migo_alloc_probe::{Burst, assert_no_steady_state_allocation};
    use std::sync::mpsc;
    use std::time::Duration;

    /// Generous: correct code returns in microseconds, and only code that parks
    /// forever reaches this.
    const LIVENESS_DEADLINE: Duration = Duration::from_secs(10);

    fn command(ctx_id: u32) -> AudioCmd {
        AudioCmd::CreateContext {
            ctx_id,
            sample_rate: None,
        }
    }

    /// The bounded-hot-paths requirement, stated about the transport
    /// itself. An unbounded channel reports no capacity at all, which is the
    /// difference this asserts.
    #[test]
    fn the_transport_is_bounded() {
        let (tx, rx) = channel();

        assert_eq!(
            tx.capacity(),
            Some(AUDIO_COMMAND_CAPACITY),
            "the audio command transport is unbounded, so a producer faster than \
             the drain grows it without limit"
        );
        assert!(rx.is_empty());
        tx.try_send(command(1)).unwrap();
        assert!(!rx.is_empty());
        assert_eq!(rx.len(), 1);
    }

    /// Boundedness as the producer meets it: past capacity the queue refuses and
    /// hands the command back rather than taking it or dropping it. The same
    /// policy the input queue and the deferred upload queue use.
    #[test]
    fn past_capacity_the_queue_hands_the_command_back() {
        let (tx, _rx) = channel();
        for ctx_id in 0..AUDIO_COMMAND_CAPACITY as u32 {
            tx.try_send(command(ctx_id))
                .expect("capacity must accept its own count");
        }

        let refused = tx.try_send(command(9999));

        assert!(
            matches!(
                refused,
                Err(AudioCommandSendError::Full(AudioCmd::CreateContext {
                    ctx_id: 9999,
                    ..
                }))
            ),
            "a full queue took the command anyway, or lost it: {refused:?}"
        );
    }

    #[test]
    fn shrinking_a_payload_permit_returns_exactly_the_unused_bytes() {
        let (tx, _rx) = channel_with_limits(2, 16);
        let mut first = tx
            .try_reserve_data(16)
            .expect("the full byte budget is initially available");

        first.shrink_to(3);

        let second = tx
            .try_reserve_data(13)
            .expect("shrinking must return the precise unused byte charge");
        assert_eq!(
            tx.usage.bytes.load(std::sync::atomic::Ordering::Acquire),
            16
        );
        drop(second);
        drop(first);
        assert_eq!(tx.usage.bytes.load(std::sync::atomic::Ordering::Acquire), 0);
    }

    #[test]
    fn shrinking_a_payload_permit_never_increases_its_charge() {
        let (tx, _rx) = channel_with_limits(2, 16);
        let mut permit = tx.try_reserve_data(8).unwrap();

        permit.shrink_to(9);

        assert_eq!(
            tx.usage.bytes.load(std::sync::atomic::Ordering::Acquire),
            8,
            "a shrink API must be unable to widen a reservation"
        );
    }

    #[test]
    fn shrinking_after_receiver_drop_cannot_underflow_reset_accounting() {
        let (tx, rx) = channel_with_limits(2, 16);
        let mut permit = tx.try_reserve_data(16).unwrap();
        drop(rx);

        permit.shrink_to(3);

        assert_eq!(
            tx.usage.bytes.load(std::sync::atomic::Ordering::Acquire),
            0,
            "receiver teardown resets accounting; a late shrink must stay at zero"
        );
        drop(permit);
        assert_eq!(tx.usage.bytes.load(std::sync::atomic::Ordering::Acquire), 0);
    }

    /// A saturated transport must return the original command without parking the
    /// V8 producer. Keeping the command is essential for request/response variants:
    /// their oneshot sender must be resolved or dropped by the caller immediately.
    #[test]
    fn a_send_into_a_full_queue_returns_full_without_blocking() {
        let (tx, _rx) = channel();
        for ctx_id in 0..AUDIO_COMMAND_CAPACITY as u32 {
            tx.try_send(command(ctx_id)).expect("fixture must fill it");
        }

        let (returned_tx, returned_rx) = mpsc::channel();
        let sender = AudioSender::new(tx, ThreadWakeup::new());
        std::thread::spawn(move || {
            let _ = returned_tx.send(sender.send(command(9999)));
        });

        let refused = returned_rx
            .recv_timeout(LIVENESS_DEADLINE)
            .expect("a full send blocked the V8 producer");
        assert!(refused.is_err(), "the saturated send was accepted");
    }

    #[test]
    fn lifecycle_bursts_coalesce_to_the_final_level_and_shutdown_stays_first() {
        let (tx, rx) = channel();
        for ctx_id in 0..AUDIO_COMMAND_CAPACITY as u32 {
            tx.try_send(command(ctx_id))
                .expect("fixture must fill data");
        }

        for index in 0..9 {
            tx.try_send(if index % 2 == 0 {
                AudioCmd::PauseAll
            } else {
                AudioCmd::ResumeAll
            })
            .expect("lifecycle updates must coalesce instead of being refused");
        }
        tx.try_send(AudioCmd::Shutdown)
            .expect("shutdown must not compete for the final data slot");

        assert!(matches!(rx.try_recv(), Ok(AudioCmd::Shutdown)));
        assert!(matches!(rx.try_recv(), Ok(AudioCmd::PauseAll)));
    }

    #[test]
    fn cleanup_barrier_bypasses_a_full_data_queue_and_coalesces_requests() {
        let (tx, rx) = channel_with_limits(2, 1024);
        tx.try_send(command(1)).unwrap();
        tx.try_send(command(2)).unwrap();

        let first = tx
            .request_release_all_contexts()
            .expect("cleanup has an independent reserved lane");
        let second = tx
            .request_release_all_contexts()
            .expect("a repeated cleanup request coalesces");
        assert_eq!(
            rx.len(),
            3,
            "two data entries plus one coalesced cleanup notification"
        );

        let mut saw_cleanup = false;
        for _ in 0..2 {
            if matches!(rx.try_recv(), Ok(AudioCmd::ReleaseAllContexts)) {
                saw_cleanup = true;
                break;
            }
        }
        assert!(saw_cleanup, "full data cannot block the cleanup lane");
        assert!(!first.is_complete(), "receiving is not the barrier ack");
        assert!(!second.is_complete(), "coalesced callers share the ack");

        rx.complete_release_all_contexts();
        assert!(first.is_complete());
        assert!(second.is_complete());
    }

    #[test]
    fn shutdown_is_highest_priority_and_cleanup_and_data_both_make_progress() {
        let (tx, rx) = channel_with_limits(2, 1024);
        tx.try_send(command(7)).unwrap();
        let ticket = tx.request_release_all_contexts().unwrap();
        tx.try_send(AudioCmd::Shutdown).unwrap();

        assert!(matches!(rx.try_recv(), Ok(AudioCmd::Shutdown)));

        let next = rx.try_recv().expect("data or cleanup gets a turn");
        let after = rx
            .try_recv()
            .expect("the other nonterminal lane gets a turn");
        assert!(matches!(
            next,
            AudioCmd::CreateContext { .. } | AudioCmd::ReleaseAllContexts
        ));
        assert!(matches!(
            after,
            AudioCmd::CreateContext { .. } | AudioCmd::ReleaseAllContexts
        ));
        assert_ne!(
            std::mem::discriminant(&next),
            std::mem::discriminant(&after),
            "neither a continuously ready cleanup lane nor data lane may starve the other"
        );
        rx.complete_release_all_contexts();
        assert!(ticket.is_complete());
    }

    #[test]
    fn receiver_drop_disconnects_pending_and_future_cleanup_requests() {
        let (tx, rx) = channel();
        let ticket = tx.request_release_all_contexts().unwrap();

        drop(rx);

        assert!(ticket.is_disconnected());
        assert!(matches!(
            tx.request_release_all_contexts(),
            Err(AudioCommandSendError::Disconnected(
                AudioCmd::ReleaseAllContexts
            ))
        ));
    }

    #[test]
    fn cleanup_fences_late_data_until_the_host_retires_old_producers() {
        let (tx, rx) = channel_with_limits(4, 1024);
        let permit = tx
            .try_reserve_data(0)
            .expect("an old producer reserves before the barrier");
        let ticket = tx.request_release_all_contexts().unwrap();

        assert!(matches!(
            tx.try_send_reserved(command(1), permit),
            Err(AudioCommandSendError::Full(AudioCmd::CreateContext {
                ctx_id: 1,
                ..
            }))
        ));
        assert!(matches!(
            tx.try_send(command(2)),
            Err(AudioCommandSendError::Full(AudioCmd::CreateContext {
                ctx_id: 2,
                ..
            }))
        ));

        assert!(matches!(rx.try_recv(), Ok(AudioCmd::ReleaseAllContexts)));
        rx.discard_data_queue();
        rx.complete_release_all_contexts();
        assert!(ticket.is_complete());
        assert!(matches!(
            tx.try_send(command(3)),
            Err(AudioCommandSendError::Full(AudioCmd::CreateContext {
                ctx_id: 3,
                ..
            }))
        ));

        tx.finish_release_all_contexts();
        tx.try_send(command(4))
            .expect("new producers may publish only after old ones are retired");
    }

    #[test]
    fn fenced_reservation_never_runs_the_commit_closure() {
        let (tx, _rx) = channel_with_limits(4, 1024);
        let permit = tx
            .try_reserve_data(0)
            .expect("reservation precedes the cleanup fence");
        let _ticket = tx.request_release_all_contexts().unwrap();
        let ran = std::sync::atomic::AtomicBool::new(false);

        let result = tx.try_send_reserved_committing(command(1), permit, || {
            ran.store(true, std::sync::atomic::Ordering::Release);
        });

        assert!(matches!(result, Err(AudioCommandSendError::Full(_))));
        assert!(
            !ran.load(std::sync::atomic::Ordering::Acquire),
            "a cleanup fence after reserve must prevent registry/detach commit"
        );
    }

    #[test]
    fn one_lifecycle_delivery_cannot_starve_waiting_data() {
        let (tx, rx) = channel();
        tx.try_send(command(7)).expect("data fixture");
        tx.try_send(AudioCmd::PauseAll).expect("initial lifecycle");

        assert!(matches!(rx.try_recv(), Ok(AudioCmd::PauseAll)));
        tx.try_send(AudioCmd::ResumeAll)
            .expect("a producer can refill the lifecycle lane immediately");

        assert!(
            matches!(rx.try_recv(), Ok(AudioCmd::CreateContext { ctx_id: 7, .. })),
            "after one lifecycle command, waiting data must receive the next turn"
        );
        assert!(matches!(rx.try_recv(), Ok(AudioCmd::ResumeAll)));
    }

    #[test]
    fn shutdown_has_dedicated_capacity_during_a_lifecycle_burst() {
        let (tx, _rx) = channel();
        for _ in 0..9 {
            tx.try_send(AudioCmd::PauseAll)
                .expect("lifecycle updates coalesce");
        }

        tx.try_send(AudioCmd::Shutdown)
            .expect("lifecycle traffic must not consume shutdown capacity");
    }

    #[test]
    fn audio_data_queue_rejects_limit_plus_one_payload_byte() {
        const EXPECTED_BYTE_LIMIT: usize = 64 * 1024 * 1024;
        const CHUNK_BYTES: usize = 16 * 1024 * 1024;

        let (tx, rx) = channel();
        let data = std::sync::Arc::new(vec![0; CHUNK_BYTES]);
        for ctx_id in 0..(EXPECTED_BYTE_LIMIT / CHUNK_BYTES) as u32 {
            let (resp, _response) = tokio::sync::oneshot::channel();
            tx.try_send(AudioCmd::DecodeAudioData {
                ctx_id,
                data: data.clone(),
                resp,
            })
            .expect("the aggregate byte limit is inclusive");
        }

        let (resp, _response) = tokio::sync::oneshot::channel();
        assert!(matches!(
            tx.try_send(AudioCmd::DecodeAudioData {
                ctx_id: 99,
                data: std::sync::Arc::new(vec![0]),
                resp,
            }),
            Err(AudioCommandSendError::ByteLimit(_))
        ));
        assert_eq!(
            tx.usage.bytes.load(std::sync::atomic::Ordering::Acquire),
            EXPECTED_BYTE_LIMIT
        );

        for _ in 0..EXPECTED_BYTE_LIMIT / CHUNK_BYTES {
            assert!(matches!(
                rx.try_recv(),
                Ok(AudioCmd::DecodeAudioData { .. })
            ));
        }
        assert_eq!(
            tx.usage.bytes.load(std::sync::atomic::Ordering::Acquire),
            0,
            "receiving commands must release their byte reservations"
        );

        let (resp, _response) = tokio::sync::oneshot::channel();
        tx.try_send(AudioCmd::DecodeAudioData {
            ctx_id: 100,
            data: std::sync::Arc::new(vec![0]),
            resp,
        })
        .expect("released bytes must be reusable");
        drop(rx);
        assert_eq!(
            tx.usage.bytes.load(std::sync::atomic::Ordering::Acquire),
            0,
            "receiver drop must release queued byte reservations"
        );
    }

    /// A closed transport must hand the command back at once. Waiting for a slot
    /// that no consumer will ever free is the one way a bounded queue can be
    /// worse than an unbounded one.
    #[test]
    fn a_disconnected_transport_returns_the_command_instead_of_waiting() {
        let (tx, rx) = channel();
        drop(rx);

        let outcome = AudioSender::new(tx, ThreadWakeup::new()).send(command(1));

        assert!(
            matches!(
                outcome,
                Err(AudioCommandSendError::Disconnected(
                    AudioCmd::CreateContext { ctx_id: 1, .. }
                ))
            ),
            "a send with no consumer did not return its command: {outcome:?}"
        );
    }

    /// The profile with no audio subsystem. Every send fails immediately, so
    /// nothing accumulates and nothing waits — the property the previous
    /// never-drained receiver had only by virtue of the ops being compiled out.
    #[test]
    fn a_disconnected_sender_holds_no_queue_and_refuses_every_send() {
        let sender = AudioSender::new(disconnected(), ThreadWakeup::new());

        for ctx_id in 0..AUDIO_COMMAND_CAPACITY as u32 + 1 {
            assert!(
                sender.send(command(ctx_id)).is_err(),
                "a build with no audio consumer queued command {ctx_id}"
            );
        }
    }

    /// The zero-allocation requirement, on a per-event path: one
    /// JavaScript audio call is one send. The unbounded channel this replaced
    /// bought a block from the heap every thirty-two messages, forever, on the
    /// thread running the game.
    ///
    /// One iteration is a send and the drain that matches it, so the queue neither
    /// fills nor makes the send wait — a burst that filled it would be measuring
    /// the wait instead.
    #[test]
    fn a_steady_state_audio_command_send_never_reaches_the_heap() {
        const WARMUP: usize = 4;
        const MEASURED: usize = 64;

        let (tx, rx) = channel();
        let sender = AudioSender::new(tx, ThreadWakeup::new());

        assert_no_steady_state_allocation(
            Burst {
                path: "audio transport: enqueue one command and take it",
                warmup: WARMUP,
                measured: MEASURED,
            },
            |iteration| {
                sender
                    .send(command(iteration as u32))
                    .expect("the consumer is this closure, so a slot is always free");
                std::hint::black_box(rx.try_recv().is_ok())
            },
        );
    }
}

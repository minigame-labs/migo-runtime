//! Ordered host-command transport with bounded input traffic and a reliable
//! control path.
//!
//! Normal and reliable input use fixed queue budgets. Trusted lifecycle and
//! asynchronous-result commands skip those quotas but use the same mutex
//! linearization point and FIFO, so input cannot displace them or reorder
//! across classes.

use std::{collections::VecDeque, sync::Arc};

use parking_lot::Mutex;
use tokio::sync::{
    Notify,
    mpsc::error::{SendError, TryRecvError, TrySendError},
};

use crate::protocol::host_cmd::HostCommand;

const DEFAULT_RELIABLE_INPUT_RESERVE: usize = 64;
const CONTROL_PLANE_HEADROOM: usize = 16;

/// Logical input stream used to constrain coalescing and terminal supersession.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputStream {
    Touch,
    Pointer,
    Composition,
    Gamepad(u32),
}

/// How an accepted input command entered the ordered queue.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputSendOutcome {
    Enqueued,
    Coalesced,
    Reserved,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum QueueLane {
    Normal,
    Reserved,
    Critical,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct InputMetadata {
    stream: InputStream,
    coalescible: bool,
}

struct QueuedHostCommand {
    command: HostCommand,
    lane: QueueLane,
    input: Option<InputMetadata>,
}

impl QueuedHostCommand {
    fn into_command(self) -> HostCommand {
        self.command
    }
}

struct QueueState {
    entries: VecDeque<QueuedHostCommand>,
    normal_len: usize,
    reserved_len: usize,
    receiver_open: bool,
    normal_senders: usize,
    critical_senders: usize,
}

struct SharedQueue {
    state: Mutex<QueueState>,
    not_empty: Notify,
    normal_capacity: usize,
    reliable_capacity: usize,
}

impl SharedQueue {
    fn input_epoch_start(state: &QueueState) -> usize {
        state
            .entries
            .iter()
            .rposition(|entry| entry.lane == QueueLane::Critical)
            .map_or(0, |index| index + 1)
    }

    fn push(
        &self,
        state: &mut QueueState,
        command: HostCommand,
        lane: QueueLane,
        input: Option<InputMetadata>,
    ) {
        match lane {
            QueueLane::Normal => state.normal_len += 1,
            QueueLane::Reserved => state.reserved_len += 1,
            // Deliberately uncounted, and so uncapped: this lane carries lifecycle
            // and result traffic, and refusing a surface destruction or a pending
            // result because a queue was full is a worse failure than any it could
            // prevent. The cost is that capacity is only reserved for the counted
            // lanes, so critical traffic outrunning the consumer can leave a later
            // `push_back` to reallocate. That is not on the input path's own
            // account -- input is refused at saturation rather than queued -- and
            // capping this lane to remove the allocation would trade a bounded
            // cost for an unbounded one.
            QueueLane::Critical => {}
        }
        state.entries.push_back(QueuedHostCommand {
            command,
            lane,
            input,
        });
    }

    fn pop(state: &mut QueueState) -> Option<HostCommand> {
        let queued = state.entries.pop_front()?;
        match queued.lane {
            QueueLane::Normal => state.normal_len -= 1,
            QueueLane::Reserved => state.reserved_len -= 1,
            QueueLane::Critical => {}
        }
        Some(queued.into_command())
    }

    fn senders_closed(state: &QueueState) -> bool {
        state.normal_senders == 0 && state.critical_senders == 0
    }
}

/// Cloneable normal-command sender shared by game-controlled producers.
///
/// Normal producers must not be able to bypass the pending-command budget:
///
/// ```compile_fail
/// use shared::{HostCommand, host_channel::channel};
///
/// let endpoints = channel(1);
/// endpoints
///     .0
///     .try_send_critical(HostCommand::OnHide)
///     .unwrap();
/// ```
///
/// The current control-plane method is unavailable as well:
///
/// ```compile_fail
/// use shared::{HostCommand, host_channel::channel};
///
/// let endpoints = channel(1);
/// endpoints.0.send(HostCommand::OnHide).unwrap();
/// ```
pub struct HostCommandSender {
    shared: Arc<SharedQueue>,
}

/// Trusted reliable-plane capability held only by the host registry.
pub struct CriticalHostCommandSender {
    shared: Arc<SharedQueue>,
}

/// Sole consumer for the ordered host-command stream.
pub struct HostCommandReceiver {
    shared: Arc<SharedQueue>,
}

/// Create an ordered host channel with a bounded normal-command budget.
pub fn channel(
    normal_capacity: usize,
) -> (
    HostCommandSender,
    CriticalHostCommandSender,
    HostCommandReceiver,
) {
    channel_with_reserve(normal_capacity, DEFAULT_RELIABLE_INPUT_RESERVE)
}

/// Create an ordered host channel with explicit normal and reliable budgets.
///
/// Production passes fixed capacities at Host construction. Keeping this
/// constructor public also lets low-capacity tests exercise every saturation
/// edge without sending hundreds of commands.
pub fn channel_with_reserve(
    normal_capacity: usize,
    reliable_capacity: usize,
) -> (
    HostCommandSender,
    CriticalHostCommandSender,
    HostCommandReceiver,
) {
    assert!(
        normal_capacity > 0,
        "host normal command capacity must be non-zero"
    );
    assert!(
        reliable_capacity > 0,
        "host reliable input capacity must be non-zero"
    );
    let shared = Arc::new(SharedQueue {
        state: Mutex::new(QueueState {
            entries: VecDeque::with_capacity(
                normal_capacity + reliable_capacity + CONTROL_PLANE_HEADROOM,
            ),
            normal_len: 0,
            reserved_len: 0,
            receiver_open: true,
            normal_senders: 1,
            critical_senders: 1,
        }),
        not_empty: Notify::new(),
        normal_capacity,
        reliable_capacity,
    });
    (
        HostCommandSender {
            shared: Arc::clone(&shared),
        },
        CriticalHostCommandSender {
            shared: Arc::clone(&shared),
        },
        HostCommandReceiver { shared },
    )
}

impl Clone for HostCommandSender {
    fn clone(&self) -> Self {
        self.shared.state.lock().normal_senders += 1;
        Self {
            shared: Arc::clone(&self.shared),
        }
    }
}

impl Drop for HostCommandSender {
    fn drop(&mut self) {
        let closed = {
            let mut state = self.shared.state.lock();
            state.normal_senders -= 1;
            SharedQueue::senders_closed(&state)
        };
        if closed {
            self.shared.not_empty.notify_waiters();
        }
    }
}

impl HostCommandSender {
    /// Enqueue a normal command without waiting, subject to the per-host quota.
    pub fn try_send(&self, command: HostCommand) -> Result<(), TrySendError<HostCommand>> {
        let mut state = self.shared.state.lock();
        if !state.receiver_open {
            return Err(TrySendError::Closed(command));
        }
        if state.normal_len == self.shared.normal_capacity {
            return Err(TrySendError::Full(command));
        }
        self.shared
            .push(&mut state, command, QueueLane::Normal, None);
        drop(state);
        self.shared.not_empty.notify_one();
        Ok(())
    }

    /// Enqueue replaceable state, or replace the newest pending state for the
    /// same stream when no transition in that stream follows it.
    pub fn try_send_coalescible(
        &self,
        stream: InputStream,
        command: HostCommand,
    ) -> Result<InputSendOutcome, TrySendError<HostCommand>> {
        let mut state = self.shared.state.lock();
        if !state.receiver_open {
            return Err(TrySendError::Closed(command));
        }

        let epoch_start = SharedQueue::input_epoch_start(&state);
        if let Some(newest) = state
            .entries
            .range_mut(epoch_start..)
            .rev()
            .find(|entry| entry.input.is_some_and(|input| input.stream == stream))
            && newest.input.is_some_and(|input| input.coalescible)
        {
            newest.command = command;
            return Ok(InputSendOutcome::Coalesced);
        }

        if state.normal_len == self.shared.normal_capacity {
            return Err(TrySendError::Full(command));
        }
        self.shared.push(
            &mut state,
            command,
            QueueLane::Normal,
            Some(InputMetadata {
                stream,
                coalescible: true,
            }),
        );
        drop(state);
        self.shared.not_empty.notify_one();
        Ok(InputSendOutcome::Enqueued)
    }

    /// Enqueue a non-coalescible transition with reserved capacity.
    pub fn try_send_reliable(
        &self,
        stream: Option<InputStream>,
        command: HostCommand,
    ) -> Result<InputSendOutcome, TrySendError<HostCommand>> {
        self.try_send_transition(stream, command, false)
    }

    /// Enqueue a terminal transition and supersede older coalescible state in
    /// the same stream.
    ///
    /// Capacity is proven before supersession, so a rejected terminal command
    /// cannot erase previously accepted input.
    pub fn try_send_terminal(
        &self,
        stream: Option<InputStream>,
        command: HostCommand,
    ) -> Result<InputSendOutcome, TrySendError<HostCommand>> {
        self.try_send_transition(stream, command, true)
    }

    fn try_send_transition(
        &self,
        stream: Option<InputStream>,
        command: HostCommand,
        supersede_coalescible: bool,
    ) -> Result<InputSendOutcome, TrySendError<HostCommand>> {
        let mut state = self.shared.state.lock();
        if !state.receiver_open {
            return Err(TrySendError::Closed(command));
        }

        let epoch_start = SharedQueue::input_epoch_start(&state);
        let removable_normal = if supersede_coalescible {
            stream.map_or(0, |stream| {
                state
                    .entries
                    .iter()
                    .skip(epoch_start)
                    .filter(|entry| {
                        entry.lane == QueueLane::Normal
                            && entry
                                .input
                                .is_some_and(|input| input.stream == stream && input.coalescible)
                    })
                    .count()
            })
        } else {
            0
        };
        let normal_after_supersession = state.normal_len - removable_normal;
        let (lane, outcome) = if normal_after_supersession < self.shared.normal_capacity {
            (QueueLane::Normal, InputSendOutcome::Enqueued)
        } else if state.reserved_len < self.shared.reliable_capacity {
            (QueueLane::Reserved, InputSendOutcome::Reserved)
        } else {
            return Err(TrySendError::Full(command));
        };

        if supersede_coalescible && let Some(stream) = stream {
            let mut index = 0;
            state.entries.retain(|entry| {
                let keep = index < epoch_start
                    || !entry
                        .input
                        .is_some_and(|input| input.stream == stream && input.coalescible);
                index += 1;
                keep
            });
        }
        state.normal_len = normal_after_supersession;
        self.shared.push(
            &mut state,
            command,
            lane,
            stream.map(|stream| InputMetadata {
                stream,
                coalescible: false,
            }),
        );
        drop(state);
        self.shared.not_empty.notify_one();
        Ok(outcome)
    }
}

impl Clone for CriticalHostCommandSender {
    fn clone(&self) -> Self {
        self.shared.state.lock().critical_senders += 1;
        Self {
            shared: Arc::clone(&self.shared),
        }
    }
}

impl Drop for CriticalHostCommandSender {
    fn drop(&mut self) {
        let closed = {
            let mut state = self.shared.state.lock();
            state.critical_senders -= 1;
            SharedQueue::senders_closed(&state)
        };
        if closed {
            self.shared.not_empty.notify_waiters();
        }
    }
}

impl CriticalHostCommandSender {
    /// Enqueue trusted lifecycle/result traffic without waiting or consuming
    /// normal command capacity.
    pub fn send(&self, command: HostCommand) -> Result<(), SendError<HostCommand>> {
        let mut state = self.shared.state.lock();
        if !state.receiver_open {
            return Err(SendError(command));
        }
        self.shared
            .push(&mut state, command, QueueLane::Critical, None);
        drop(state);
        self.shared.not_empty.notify_one();
        Ok(())
    }
}

impl HostCommandReceiver {
    /// Receive the next command in the single cross-class FIFO order.
    pub async fn recv(&mut self) -> Option<HostCommand> {
        loop {
            let notified = self.shared.not_empty.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();

            {
                let mut state = self.shared.state.lock();
                if let Some(command) = SharedQueue::pop(&mut state) {
                    return Some(command);
                }
                if SharedQueue::senders_closed(&state) {
                    return None;
                }
            }
            notified.await;
        }
    }

    /// Try to receive without waiting.
    pub fn try_recv(&mut self) -> Result<HostCommand, TryRecvError> {
        let mut state = self.shared.state.lock();
        if let Some(command) = SharedQueue::pop(&mut state) {
            return Ok(command);
        }
        if SharedQueue::senders_closed(&state) {
            Err(TryRecvError::Disconnected)
        } else {
            Err(TryRecvError::Empty)
        }
    }
}

impl Drop for HostCommandReceiver {
    fn drop(&mut self) {
        let mut state = self.shared.state.lock();
        state.receiver_open = false;
        state.entries.clear();
        state.normal_len = 0;
        state.reserved_len = 0;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{InputSendOutcome, InputStream, channel, channel_with_reserve};
    use crate::protocol::host_cmd::HostCommand;
    use migo_alloc_probe::{Burst, assert_no_steady_state_allocation};
    use tokio::sync::mpsc::error::{SendError, TrySendError};

    fn mouse_move(x: f32) -> HostCommand {
        HostCommand::OnMouseMove {
            x,
            y: 0.0,
            button: 0,
            timestamp_ms: f64::from(x),
        }
    }

    fn mouse_down() -> HostCommand {
        HostCommand::OnMouseDown {
            x: 2.0,
            y: 0.0,
            button: 0,
            timestamp_ms: 2.0,
        }
    }

    fn mouse_up() -> HostCommand {
        HostCommand::OnMouseUp {
            x: 3.0,
            y: 0.0,
            button: 0,
            timestamp_ms: 3.0,
        }
    }

    fn key_up(code: &str) -> HostCommand {
        HostCommand::OnKeyUp {
            key: code.to_owned(),
            code: code.to_owned(),
            timestamp_ms: 1.0,
            modifiers: 0,
            repeat: false,
        }
    }

    fn assert_move_x(command: HostCommand, expected: f32) {
        match command {
            HostCommand::OnMouseMove { x, .. } => assert_eq!(x, expected),
            other => panic!("expected mouse move, got {other:?}"),
        }
    }

    #[test]
    fn critical_bypasses_full_normal_budget_and_keeps_fifo() {
        let (tx, critical_tx, mut rx) = channel(1);
        tx.try_send(HostCommand::Restart).unwrap();

        assert!(matches!(
            tx.try_send(HostCommand::Shutdown),
            Err(TrySendError::Full(HostCommand::Shutdown))
        ));
        critical_tx.send(HostCommand::OnHide).unwrap();

        assert!(matches!(rx.try_recv(), Ok(HostCommand::Restart)));
        assert!(matches!(rx.try_recv(), Ok(HostCommand::OnHide)));
    }

    #[test]
    fn receiving_normal_command_releases_its_budget() {
        let (tx, _critical_tx, mut rx) = channel(1);
        tx.try_send(HostCommand::Restart).unwrap();
        assert!(matches!(rx.try_recv(), Ok(HostCommand::Restart)));
        tx.try_send(HostCommand::Shutdown).unwrap();
        assert!(matches!(rx.try_recv(), Ok(HostCommand::Shutdown)));
    }

    #[test]
    fn critical_then_normal_preserves_fifo() {
        let (tx, critical_tx, mut rx) = channel(1);
        critical_tx.send(HostCommand::OnHide).unwrap();
        tx.try_send(HostCommand::Restart).unwrap();
        assert!(matches!(rx.try_recv(), Ok(HostCommand::OnHide)));
        assert!(matches!(rx.try_recv(), Ok(HostCommand::Restart)));
    }

    #[test]
    fn closed_receiver_returns_original_command() {
        let (tx, critical_tx, rx) = channel(1);
        drop(rx);
        assert!(matches!(
            tx.try_send(HostCommand::Restart),
            Err(TrySendError::Closed(HostCommand::Restart))
        ));
        assert!(matches!(
            critical_tx.send(HostCommand::OnHide),
            Err(SendError(HostCommand::OnHide))
        ));
    }

    #[test]
    fn move_replaces_only_latest_state_before_same_stream_transition() {
        let (tx, _critical_tx, mut rx) = channel_with_reserve(3, 1);
        assert!(matches!(
            tx.try_send_coalescible(InputStream::Pointer, mouse_move(1.0)),
            Ok(InputSendOutcome::Enqueued)
        ));
        assert!(matches!(
            tx.try_send_coalescible(InputStream::Pointer, mouse_move(2.0)),
            Ok(InputSendOutcome::Coalesced)
        ));
        tx.try_send_reliable(Some(InputStream::Pointer), mouse_down())
            .unwrap();
        tx.try_send_coalescible(InputStream::Pointer, mouse_move(3.0))
            .unwrap();

        assert_move_x(rx.try_recv().unwrap(), 2.0);
        assert!(matches!(rx.try_recv(), Ok(HostCommand::OnMouseDown { .. })));
        assert_move_x(rx.try_recv().unwrap(), 3.0);
    }

    #[test]
    fn critical_focus_change_starts_a_new_coalescing_epoch() {
        let (tx, critical_tx, mut rx) = channel_with_reserve(2, 1);
        tx.try_send_coalescible(InputStream::Pointer, mouse_move(1.0))
            .unwrap();
        critical_tx
            .send(HostCommand::OnFocusChanged { focused: false })
            .unwrap();
        tx.try_send_coalescible(InputStream::Pointer, mouse_move(2.0))
            .unwrap();

        assert_move_x(rx.try_recv().unwrap(), 1.0);
        assert!(matches!(
            rx.try_recv(),
            Ok(HostCommand::OnFocusChanged { focused: false })
        ));
        assert_move_x(rx.try_recv().unwrap(), 2.0);
    }

    #[test]
    fn terminal_supersession_stops_at_critical_focus_change() {
        let (tx, critical_tx, mut rx) = channel_with_reserve(2, 1);
        tx.try_send_coalescible(InputStream::Pointer, mouse_move(1.0))
            .unwrap();
        critical_tx
            .send(HostCommand::OnFocusChanged { focused: false })
            .unwrap();
        tx.try_send_terminal(Some(InputStream::Pointer), mouse_up())
            .unwrap();

        assert_move_x(rx.try_recv().unwrap(), 1.0);
        assert!(matches!(
            rx.try_recv(),
            Ok(HostCommand::OnFocusChanged { focused: false })
        ));
        assert!(matches!(rx.try_recv(), Ok(HostCommand::OnMouseUp { .. })));
    }

    #[test]
    fn terminal_supersedes_older_motion_for_its_stream() {
        let (tx, _critical_tx, mut rx) = channel_with_reserve(2, 1);
        tx.try_send_coalescible(InputStream::Pointer, mouse_move(1.0))
            .unwrap();
        tx.try_send(HostCommand::Restart).unwrap();

        assert!(matches!(
            tx.try_send_terminal(Some(InputStream::Pointer), mouse_up()),
            Ok(InputSendOutcome::Enqueued)
        ));
        assert!(matches!(rx.try_recv(), Ok(HostCommand::Restart)));
        assert!(matches!(rx.try_recv(), Ok(HostCommand::OnMouseUp { .. })));
    }

    #[test]
    fn reliable_transition_uses_reserve_when_normal_lane_is_full() {
        let (tx, _critical_tx, mut rx) = channel_with_reserve(1, 1);
        tx.try_send(HostCommand::Restart).unwrap();

        assert!(matches!(
            tx.try_send_reliable(None, key_up("KeyA")),
            Ok(InputSendOutcome::Reserved)
        ));
        assert!(matches!(rx.try_recv(), Ok(HostCommand::Restart)));
        assert!(matches!(rx.try_recv(), Ok(HostCommand::OnKeyUp { .. })));
    }

    #[test]
    fn reliable_reserve_is_bounded_and_returns_original_command() {
        let (tx, _critical_tx, _rx) = channel_with_reserve(1, 1);
        tx.try_send(HostCommand::Restart).unwrap();
        tx.try_send_reliable(None, key_up("KeyA")).unwrap();

        assert!(matches!(
            tx.try_send_reliable(None, key_up("KeyB")),
            Err(TrySendError::Full(HostCommand::OnKeyUp { code, .. })) if code == "KeyB"
        ));
    }

    #[test]
    fn producer_barriers_keep_transition_between_motion_epochs() {
        let (tx, _critical_tx, mut rx) = channel_with_reserve(3, 1);
        let first_done = Arc::new(std::sync::Barrier::new(2));
        let transition_done = Arc::new(std::sync::Barrier::new(2));

        let motion_tx = tx.clone();
        let motion_first_done = Arc::clone(&first_done);
        let motion_transition_done = Arc::clone(&transition_done);
        let motion = std::thread::spawn(move || {
            motion_tx
                .try_send_coalescible(InputStream::Pointer, mouse_move(1.0))
                .unwrap();
            motion_first_done.wait();
            motion_transition_done.wait();
            motion_tx
                .try_send_coalescible(InputStream::Pointer, mouse_move(2.0))
                .unwrap();
        });

        first_done.wait();
        tx.try_send_reliable(Some(InputStream::Pointer), mouse_down())
            .unwrap();
        transition_done.wait();
        motion.join().unwrap();

        assert_move_x(rx.try_recv().unwrap(), 1.0);
        assert!(matches!(rx.try_recv(), Ok(HostCommand::OnMouseDown { .. })));
        assert_move_x(rx.try_recv().unwrap(), 2.0);
    }

    #[test]
    fn async_receiver_wakes_and_drains_before_sender_close() {
        let (tx, critical_tx, mut rx) = channel(1);
        let producer = std::thread::spawn(move || {
            tx.try_send(HostCommand::Restart).unwrap();
            drop(tx);
            drop(critical_tx);
        });
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();

        runtime.block_on(async {
            assert!(matches!(rx.recv().await, Some(HostCommand::Restart)));
            assert!(rx.recv().await.is_none());
        });
        producer.join().unwrap();
    }

    /// Steady-state allocation gate on the input transport.
    ///
    /// This counts allocations rather than reading the source for
    /// `VecDeque::with_capacity`, which is what
    /// `scripts/test-input-transport-contract.sh` does and why that script cannot
    /// fail when an allocation appears.
    ///
    /// The burst covers all three producer entry points plus the drain, and it
    /// deliberately carries no `String`-bearing command: constructing an
    /// `OnKeyUp { key, code }` allocates in the *caller*, which says nothing about
    /// the transport. What is measured is enqueue, coalesce, supersede and dequeue.
    #[test]
    fn steady_state_input_burst_never_reaches_the_heap() {
        let (tx, _critical_tx, mut rx) = channel_with_reserve(8, 4);

        assert_no_steady_state_allocation(
            Burst {
                path: "host_channel: coalescible motion, reliable and terminal transitions, drain",
                warmup: 4,
                measured: 64,
            },
            |iteration| {
                let x = iteration as f32;
                tx.try_send_coalescible(InputStream::Pointer, mouse_move(x))
                    .unwrap();
                tx.try_send_coalescible(InputStream::Pointer, mouse_move(x + 1.0))
                    .unwrap();
                tx.try_send_reliable(Some(InputStream::Pointer), mouse_down())
                    .unwrap();
                tx.try_send_coalescible(InputStream::Pointer, mouse_move(x + 2.0))
                    .unwrap();
                tx.try_send_terminal(Some(InputStream::Pointer), mouse_up())
                    .unwrap();

                let mut drained = 0usize;
                while rx.try_recv().is_ok() {
                    drained += 1;
                }
                // The terminal transition supersedes both surviving motion entries,
                // so a burst that drains anything else did not exercise the paths
                // this measurement claims to cover.
                assert_eq!(drained, 2);
                drained
            },
        );
    }
}

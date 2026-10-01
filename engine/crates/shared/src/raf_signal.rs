//! RAF (requestAnimationFrame) frame-timestamp signaling.
//!
//! The render thread signals "frame ready" with a timestamp; the host thread
//! (JS async op) waits for it.
//!
//! - **Android/Linux**: `eventfd` + `tokio::io::unix::AsyncFd` — true epoll wait,
//!   zero CPU when idle, ~1-3 µs wake latency.
//! - **Other platforms**: a one-frame slot and a `tokio::sync::Notify`, with the same
//!   meaning as the eventfd: signals that arrive before the consumer looks collapse into one
//!   wake carrying the NEWEST timestamp.

use std::sync::Arc;

/// Sender half — lives on the render thread (native OS thread).
pub struct RafSender(SenderInner);

/// Receiver half — lives on the host thread (tokio async).
/// Wrapped in `Arc` for restart survival.
pub struct RafReceiver(ReceiverInner);

impl RafSender {
    /// Signal the next frame timestamp (milliseconds).
    ///
    /// Returns `true` if the signal was delivered, `false` if it was dropped
    /// (channel full or write error).  The caller should count drops for
    /// debug stats.
    pub fn signal(&self, ts_ms: f64, ticket: u64) -> bool {
        // On every platform a signal that nobody has consumed yet is replaced by this one, never
        // queued behind it: the consumer must be woken for the newest frame, not for the oldest.
        match &self.0 {
            #[cfg(target_os = "android")]
            SenderInner::Eventfd { fd, frame } => {
                use std::os::fd::AsRawFd;
                *frame.lock() = RafFrame { ts_ms, ticket };
                let val: u64 = 1;
                loop {
                    let ret = unsafe {
                        libc::write(fd.as_raw_fd(), &val as *const u64 as *const libc::c_void, 8)
                    };
                    if ret == 8 {
                        return true;
                    }
                    // Retry on EINTR; give up on any other error.
                    let errno = std::io::Error::last_os_error();
                    if errno.raw_os_error() != Some(libc::EINTR) {
                        return false;
                    }
                }
            }
            SenderInner::Slot(slot) => {
                *slot.frame.lock() = RafFrame { ts_ms, ticket };
                // `notify_one` keeps one permit when nobody is waiting, so a signal that
                // arrives before `recv` is not lost, and several before it collapse into one.
                slot.wake.notify_one();
                true
            }
        }
    }
}

impl RafReceiver {
    /// Wait for the frame signal matching `expected_ticket`.
    ///
    /// Returns `None` only when the channel is truly closed (sender dropped).
    /// Spurious wakes (EAGAIN) and stale signals from a cancelled runtime are
    /// retried internally. Ticket matching prevents a soft restart from
    /// consuming a timestamp produced for the old isolate.
    pub async fn recv(&self, expected_ticket: u64) -> Option<f64> {
        loop {
            let frame = match &self.0 {
                #[cfg(target_os = "android")]
                ReceiverInner::Eventfd {
                    async_fd,
                    fd,
                    frame,
                } => {
                    use std::os::fd::AsRawFd;

                    let raw_fd = fd.as_raw_fd();
                    // Register the fd with epoll once and reuse it every frame.
                    // Recreating an `AsyncFd` per `recv()` did an epoll add+remove on
                    // every RAF wait. Lazily initialised because the first `recv()`
                    // is the earliest point a tokio reactor is guaranteed current;
                    // the host tokio runtime (and thus this registration) survives
                    // soft restarts, so the cached handle stays valid.
                    let async_fd = async_fd
                        .get_or_try_init(|| async {
                            tokio::io::unix::AsyncFd::with_interest(
                                raw_fd,
                                tokio::io::Interest::READABLE,
                            )
                        })
                        .await
                        .ok()?;

                    // Loop until we get a real read or a fatal error.
                    // EAGAIN (spurious wake) just re-enters the readable wait.
                    loop {
                        let mut guard = async_fd.readable().await.ok()?;

                        let mut buf = [0u8; 8];
                        let ret =
                            unsafe { libc::read(raw_fd, buf.as_mut_ptr() as *mut libc::c_void, 8) };
                        guard.clear_ready();

                        if ret == 8 {
                            break *frame.lock();
                        }

                        let errno = std::io::Error::last_os_error();
                        if errno.raw_os_error() == Some(libc::EAGAIN)
                            || errno.raw_os_error() == Some(libc::EINTR)
                        {
                            // Spurious wake or signal interrupt — retry.
                            continue;
                        }

                        // Real error (e.g. EBADF) — treat as closed.
                        return None;
                    }
                }
                ReceiverInner::Slot(slot) => {
                    // The newest signal, as the eventfd path gives. This was a bounded(2) channel,
                    // which dropped the NEWEST signal when full: a consumer that fell behind (a long
                    // frame, an idle screen) was handed the oldest two timestamps the render thread
                    // had written, and its next animation frame carried a time from before the
                    // stall began.
                    slot.wake.notified().await;
                    *slot.frame.lock()
                }
            };

            if frame_matches_ticket(frame.ticket, expected_ticket) {
                return Some(frame.ts_ms);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Internal enum dispatch
// ---------------------------------------------------------------------------

enum SenderInner {
    #[cfg(target_os = "android")]
    Eventfd {
        fd: std::os::fd::OwnedFd,
        frame: Arc<parking_lot::Mutex<RafFrame>>,
    },
    Slot(Arc<RafSlot>),
}

enum ReceiverInner {
    #[cfg(target_os = "android")]
    Eventfd {
        /// Cached epoll registration for `fd`, created on the first `recv()`
        /// (needs a live tokio reactor) and reused every frame to avoid a
        /// per-frame epoll add/remove. Declared before `fd` so it deregisters
        /// before the fd is closed on drop.
        async_fd: tokio::sync::OnceCell<tokio::io::unix::AsyncFd<std::os::fd::RawFd>>,
        fd: std::os::fd::OwnedFd,
        frame: Arc<parking_lot::Mutex<RafFrame>>,
    },
    Slot(Arc<RafSlot>),
}

/// The newest frame signal and the wake that says there is one.
struct RafSlot {
    frame: parking_lot::Mutex<RafFrame>,
    wake: tokio::sync::Notify,
}

#[derive(Clone, Copy, Debug)]
struct RafFrame {
    ts_ms: f64,
    ticket: u64,
}

#[inline]
fn frame_matches_ticket(delivered_ticket: u64, expected_ticket: u64) -> bool {
    // `>=` (not `==`) so the render thread can PRE-signal a frame with the
    // current session ticket before the JS consumer re-publishes demand, letting
    // `recv` return without blocking (free-run) instead of one eventfd wait per
    // frame — the wait that dropped the compute thread's utilisation and, via the
    // governor, its clock, halving stress fps. Restart-safe because session
    // tickets are monotonic: a fresh isolate's ticket is strictly greater, so it
    // ignores any stale lower-ticket signal left in the shared eventfd, and the
    // old isolate's in-flight `recv(old)` never matches the new session's frames.
    delivered_ticket >= expected_ticket
}

// ---------------------------------------------------------------------------
// Constructor
// ---------------------------------------------------------------------------

/// Create a matched (sender, receiver) pair.
///
/// On Android: uses eventfd for low-latency, low-power wake.
/// Falls back to the newest-frame slot on failure or other platforms.
pub fn create_raf_pair() -> (RafSender, Arc<RafReceiver>) {
    #[cfg(target_os = "android")]
    {
        match create_eventfd_pair() {
            Ok((tx, rx)) => {
                tracing::info!("RAF signal: using eventfd");
                return (tx, Arc::new(rx));
            }
            Err(e) => {
                tracing::warn!("RAF eventfd init failed ({e}), falling back to channel");
            }
        }
    }

    let (tx, rx) = create_channel_pair();
    tracing::info!("RAF signal: using the newest-frame slot");
    (tx, Arc::new(rx))
}

#[cfg(target_os = "android")]
fn create_eventfd_pair() -> Result<(RafSender, RafReceiver), String> {
    use std::os::fd::FromRawFd;

    let fd = unsafe { libc::eventfd(0, libc::EFD_NONBLOCK | libc::EFD_CLOEXEC) };
    if fd < 0 {
        return Err(format!("eventfd(): {}", std::io::Error::last_os_error()));
    }

    let fd2 = unsafe { libc::dup(fd) };
    if fd2 < 0 {
        unsafe { libc::close(fd) };
        return Err(format!("dup(eventfd): {}", std::io::Error::last_os_error()));
    }

    let frame = Arc::new(parking_lot::Mutex::new(RafFrame {
        ts_ms: 0.0,
        ticket: 0,
    }));

    Ok((
        RafSender(SenderInner::Eventfd {
            fd: unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) },
            frame: frame.clone(),
        }),
        RafReceiver(ReceiverInner::Eventfd {
            async_fd: tokio::sync::OnceCell::new(),
            fd: unsafe { std::os::fd::OwnedFd::from_raw_fd(fd2) },
            frame,
        }),
    ))
}

fn create_channel_pair() -> (RafSender, RafReceiver) {
    let slot = Arc::new(RafSlot {
        frame: parking_lot::Mutex::new(RafFrame {
            ts_ms: 0.0,
            ticket: 0,
        }),
        wake: tokio::sync::Notify::new(),
    });
    (
        RafSender(SenderInner::Slot(slot.clone())),
        RafReceiver(ReceiverInner::Slot(slot)),
    )
}

// ---------------------------------------------------------------------------
// RAF demand latch
// ---------------------------------------------------------------------------

use std::sync::atomic::{AtomicU64, Ordering};

/// Tracks whether a JS RAF consumer is actually awaiting a frame signal.
///
/// `op_await_next_frame` publishes demand (`mark_waiting`) before awaiting; the
/// render thread consumes it (`take_waiter`) and only then signals RAF, so a
/// dirty-only / upload-only frame never writes an unconsumed timestamp. On a
/// failed signal the render thread restores demand so RAF cannot freeze.
///
/// Shared via `Arc` between the host op (`HostOpState`) and the render thread;
/// survives JS soft restart the same way [`RafReceiver`] does.
#[derive(Debug, Default)]
pub struct RafDemand {
    next_ticket: AtomicU64,
    waiter_ticket: AtomicU64,
    /// The current isolate session's ticket. Constant for the life of a JS
    /// session and bumped by [`begin_session`] on each (soft-)restart, so it is
    /// monotonic across the shared eventfd. Every waiter in a session uses this
    /// same ticket (rather than a fresh per-frame ticket), which lets the render
    /// thread pre-signal a frame the consumer will accept — the basis of the
    /// free-run path that keeps the compute thread busy (see `frame_matches_ticket`).
    session_ticket: AtomicU64,
}

impl RafDemand {
    pub fn new() -> Self {
        Self::default()
    }

    /// Begin a new JS session: allocate a fresh, strictly-greater session ticket.
    /// Called once at host construction and again on every soft restart so a new
    /// isolate never accepts a signal produced for the old one.
    #[inline]
    pub fn begin_session(&self) -> u64 {
        let ticket = self.next_ticket.fetch_add(1, Ordering::Relaxed) + 1;
        self.session_ticket.store(ticket, Ordering::Release);
        ticket
    }

    /// The current session ticket, allocating one lazily if a session was never
    /// explicitly begun (defensive: keeps standalone/test `RafDemand`s working).
    #[inline]
    pub fn session_ticket(&self) -> u64 {
        let t = self.session_ticket.load(Ordering::Acquire);
        if t != 0 { t } else { self.begin_session() }
    }

    /// Publish demand before awaiting (op side). Uses the session ticket so the
    /// render thread can pre-signal a matching frame (`>=`) without blocking.
    #[inline]
    pub fn mark_waiting(&self) -> u64 {
        let ticket = self.session_ticket();
        self.waiter_ticket.store(ticket, Ordering::Release);
        ticket
    }

    /// Non-consuming read (render demand check / tests).
    #[inline]
    pub fn is_waiting(&self) -> bool {
        self.waiter_ticket.load(Ordering::Acquire) != 0
    }

    /// Consume the waiter (render side, before signalling). Returns its ticket;
    /// a second consecutive call returns `None` so a dirty-only frame
    /// following a signalled one never signals twice.
    #[inline]
    pub fn take_waiter(&self) -> Option<u64> {
        let ticket = self.waiter_ticket.swap(0, Ordering::AcqRel);
        (ticket != 0).then_some(ticket)
    }

    /// Re-arm demand after a failed signal so RAF is not frozen.
    #[inline]
    pub fn restore_waiter(&self, ticket: u64) {
        let _ =
            self.waiter_ticket
                .compare_exchange(0, ticket, Ordering::Release, Ordering::Relaxed);
    }
}

/// Shared handle to the RAF demand latch.
pub type RafDemandRef = Arc<RafDemand>;

#[cfg(test)]
mod demand_tests {
    use super::{RafDemand, create_channel_pair, frame_matches_ticket};

    #[test]
    fn new_is_not_waiting() {
        let d = RafDemand::new();
        assert!(!d.is_waiting());
        assert_eq!(d.take_waiter(), None, "no waiter to take");
    }

    #[test]
    fn mark_then_take_consumes_exactly_once() {
        let d = RafDemand::new();
        let ticket = d.mark_waiting();
        assert!(d.is_waiting());
        assert_eq!(
            d.take_waiter(),
            Some(ticket),
            "first take consumes the waiter"
        );
        assert!(!d.is_waiting(), "consumed");
        assert!(
            d.take_waiter().is_none(),
            "second take sees no waiter (no double signal)"
        );
    }

    #[test]
    fn restore_after_failed_signal_rearms() {
        let d = RafDemand::new();
        let ticket = d.mark_waiting();
        assert_eq!(d.take_waiter(), Some(ticket));
        d.restore_waiter(ticket); // signal failed
        assert!(d.is_waiting(), "restored demand so RAF is not frozen");
        assert_eq!(d.take_waiter(), Some(ticket), "retry consumes same ticket");
    }

    #[test]
    fn stale_restore_does_not_overwrite_newer_waiter() {
        let d = RafDemand::new();
        let old = d.mark_waiting();
        assert_eq!(d.take_waiter(), Some(old));
        let newer = d.mark_waiting();
        d.restore_waiter(old);
        assert_eq!(d.take_waiter(), Some(newer));
    }

    #[test]
    fn receiver_ticket_filter_rejects_soft_restart_stale_signal() {
        // A stale (lower) ticket is rejected; the current/newer one is accepted.
        assert!(!frame_matches_ticket(7, 8));
        assert!(frame_matches_ticket(8, 8));
        // Free-run: a signal carrying a >= ticket (same session) is accepted, so
        // the render thread can pre-signal before the consumer re-publishes demand.
        assert!(frame_matches_ticket(9, 8));
    }

    #[test]
    fn session_ticket_constant_within_session_and_bumps_across() {
        let d = RafDemand::new();
        let s1 = d.begin_session();
        assert_ne!(s1, 0, "session ticket is non-zero (0 == no waiter)");
        // Every frame in a session uses the SAME ticket (enables pre-signalling).
        assert_eq!(d.mark_waiting(), s1);
        assert_eq!(d.mark_waiting(), s1);
        // A new session's ticket is strictly greater (monotonic => restart-safe).
        let s2 = d.begin_session();
        assert!(s2 > s1);
        assert_eq!(d.mark_waiting(), s2);
    }

    #[test]
    fn free_run_presignal_matches_waiter_but_stale_session_is_ignored() {
        let d = RafDemand::new();
        let s1 = d.begin_session();
        // Render pre-signals with the session ticket; a later waiter accepts it.
        let expected = d.mark_waiting();
        assert!(
            frame_matches_ticket(s1, expected),
            "pre-signal accepted (>=)"
        );
        // After a soft restart (new session), the old session's signal is ignored.
        let s2 = d.begin_session();
        assert!(
            !frame_matches_ticket(s1, s2),
            "stale session signal rejected"
        );
    }

    #[test]
    fn session_ticket_lazily_allocated_if_never_begun() {
        let d = RafDemand::new();
        // Defensive: standalone RafDemand (no explicit begin_session) still yields
        // a usable non-zero ticket so mark/signal/recv line up.
        assert_ne!(d.session_ticket(), 0);
        assert_eq!(d.mark_waiting(), d.session_ticket());
    }

    /// A consumer that was not looking while several frames were signalled is handed the newest, not
    /// the oldest: the render thread signals every vsync while animating, and a long frame or an idle
    /// screen leaves several unconsumed.
    #[test]
    fn a_consumer_that_fell_behind_gets_the_newest_signal() {
        let (tx, rx) = create_channel_pair();
        for i in 0..10 {
            assert!(tx.signal(100.0 + i as f64 * 16.667, 5));
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("test runtime");
        let got = runtime.block_on(rx.recv(5)).unwrap();
        assert!(
            (got - (100.0 + 9.0 * 16.667)).abs() < 1e-9,
            "got {got}, not the newest"
        );
    }

    /// Signals collapse: ten signals are one wake, so the next `recv` waits for a signal made after
    /// the first was consumed rather than replaying an old one.
    #[test]
    fn a_consumed_signal_is_not_delivered_twice() {
        let (tx, rx) = create_channel_pair();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("test runtime");
        tx.signal(1.0, 3);
        assert_eq!(runtime.block_on(rx.recv(3)), Some(1.0));
        let second = runtime.block_on(async {
            tokio::time::timeout(std::time::Duration::from_millis(30), rx.recv(3)).await
        });
        assert!(
            second.is_err(),
            "a second recv replayed a consumed signal: {second:?}"
        );
        tx.signal(2.0, 3);
        assert_eq!(runtime.block_on(rx.recv(3)), Some(2.0));
    }

    #[test]
    fn channel_receiver_waits_past_stale_signal_for_current_ticket() {
        let (tx, rx) = create_channel_pair();
        assert!(tx.signal(1.0, 7));
        let producer = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(10));
            assert!(tx.signal(2.0, 8));
        });

        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("test runtime");
        assert_eq!(runtime.block_on(rx.recv(8)), Some(2.0));
        producer.join().expect("producer thread");
    }
}

//! Thread-safe wakeup primitives for cross-thread signaling.
//!
//! Provides [`ThreadWakeup`], with a general condvar mode and an opt-in
//! single-consumer audio mode. It allows
//! one thread to wake another from a timed or indefinite sleep. Used by the audio thread's
//! power management to enable instant wakeup from deep-sleep states when
//! a new command arrives.

use std::fmt;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

mod audio;
pub use audio::AudioNotifier;

#[derive(Clone)]
enum WakeKind {
    Condvar(Arc<(Mutex<bool>, Condvar)>),
    Audio(Arc<audio::AudioWake>),
}

/// A latched wakeup: general condvar mode or one fixed audio consumer.
///
/// When a thread is sleeping via [`wait`](ThreadWakeup::wait) or
/// [`wait_timeout`](ThreadWakeup::wait_timeout), another thread can call
/// [`notify`](ThreadWakeup::notify) to wake it immediately.
///
/// This is `Clone` and `Send + Sync` — multiple producers can hold clones
/// and wake the single consumer.
#[derive(Clone)]
pub struct ThreadWakeup {
    inner: WakeKind,
}

impl ThreadWakeup {
    /// Create a new wakeup handle (initially not signaled).
    pub fn new() -> Self {
        Self {
            inner: WakeKind::Condvar(Arc::new((Mutex::new(false), Condvar::new()))),
        }
    }

    /// The audio service has one fixed consumer for its lifetime. Producers can
    /// notify before lazy startup; only the actual consumer binds/warms its
    /// parker. Other users keep the existing condvar behaviour via `new`.
    pub fn new_audio() -> Self {
        Self {
            inner: WakeKind::Audio(Arc::new(audio::AudioWake::default())),
        }
    }

    /// Initialize/validate the single audio consumer before opening a device.
    pub fn bind_audio_thread(&self) {
        if let WakeKind::Audio(wake) = &self.inner {
            wake.bind();
        }
    }

    /// A callback-safe capability is only available in audio mode. Construction
    /// clones an Arc on the control thread; notifying does not clone or bind.
    pub fn audio_notifier(&self) -> Option<AudioNotifier> {
        match &self.inner {
            WakeKind::Audio(inner) => Some(AudioNotifier {
                inner: inner.clone(),
            }),
            WakeKind::Condvar(_) => None,
        }
    }

    /// Signal the sleeping thread to wake up.
    ///
    /// If the thread is currently waiting, it will return immediately. If it
    /// is not sleeping, the signal is latched and the next wait call returns
    /// instantly.
    #[inline]
    pub fn notify(&self) {
        let inner = match &self.inner {
            WakeKind::Condvar(inner) => inner,
            WakeKind::Audio(wake) => {
                wake.notify();
                return;
            }
        };
        let (lock, cvar) = &**inner;
        // Recover from poisoned mutex — if the peer thread panicked while
        // holding this lock we still want to deliver the wakeup signal
        // rather than cascading the panic to this thread.
        let mut signaled = lock.lock().unwrap_or_else(|e| e.into_inner());
        *signaled = true;
        cvar.notify_one();
    }

    /// Wait until explicitly signaled by [`notify`](ThreadWakeup::notify).
    ///
    /// Notifications are latched, so a notification sent before this call is
    /// consumed without blocking. The predicate loop handles spurious condvar
    /// wakeups without turning them into work-loop wakeups.
    pub fn wait(&self) {
        let inner = match &self.inner {
            WakeKind::Condvar(inner) => inner,
            WakeKind::Audio(wake) => {
                wake.wait();
                return;
            }
        };
        let (lock, cvar) = &**inner;
        let mut signaled = lock.lock().unwrap_or_else(|e| e.into_inner());
        while !*signaled {
            signaled = cvar.wait(signaled).unwrap_or_else(|e| e.into_inner());
        }
        *signaled = false;
    }

    /// Wait for a signal or timeout.
    ///
    /// Returns `true` if the wakeup was signaled (via [`notify`]), `false`
    /// if the timeout elapsed without a signal.  Callers can use this to
    /// distinguish an explicit wakeup from a periodic timer expiry.
    pub fn wait_timeout(&self, timeout: Duration) -> bool {
        let inner = match &self.inner {
            WakeKind::Condvar(inner) => inner,
            WakeKind::Audio(wake) => return wake.wait_timeout(timeout),
        };
        let (lock, cvar) = &**inner;
        // Recover from poisoned mutex rather than panicking — this prevents
        // a panic in the audio thread from cascading to the host thread.
        let mut signaled = lock.lock().unwrap_or_else(|e| e.into_inner());
        if *signaled {
            *signaled = false;
            return true;
        }
        let result = cvar
            .wait_timeout(signaled, timeout)
            .unwrap_or_else(|e| e.into_inner());
        signaled = result.0;
        let was_signaled = *signaled;
        *signaled = false;
        // Return true only when actually signaled; false on timeout.
        was_signaled
    }
}

impl Default for ThreadWakeup {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for ThreadWakeup {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ThreadWakeup").finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::thread;

    #[test]
    fn wait_consumes_notification_that_arrived_before_waiting() {
        let wakeup = ThreadWakeup::new();
        wakeup.notify();

        let (done_tx, done_rx) = mpsc::channel();
        thread::spawn(move || {
            wakeup.wait();
            done_tx.send(()).unwrap();
        });

        assert_eq!(done_rx.recv_timeout(Duration::from_secs(1)), Ok(()));
    }

    #[test]
    fn wait_blocks_until_notified() {
        let wakeup = ThreadWakeup::new();
        let waiter = wakeup.clone();
        let (started_tx, started_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();

        thread::spawn(move || {
            started_tx.send(()).unwrap();
            waiter.wait();
            done_tx.send(()).unwrap();
        });

        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(
            done_rx.recv_timeout(Duration::from_millis(25)),
            Err(mpsc::RecvTimeoutError::Timeout)
        );

        wakeup.notify();
        assert_eq!(done_rx.recv_timeout(Duration::from_secs(1)), Ok(()));
    }

    #[test]
    fn wait_timeout_consumes_latched_notification() {
        let wakeup = ThreadWakeup::new();
        wakeup.notify();

        assert!(wakeup.wait_timeout(Duration::from_secs(1)));
    }

    #[test]
    fn wait_timeout_returns_false_when_deadline_expires() {
        let wakeup = ThreadWakeup::new();

        assert!(!wakeup.wait_timeout(Duration::from_millis(10)));
    }

    #[test]
    fn wait_timeout_returns_true_when_notified_by_peer() {
        let wakeup = ThreadWakeup::new();
        let waiter = wakeup.clone();
        let (started_tx, started_rx) = mpsc::channel();
        let (result_tx, result_rx) = mpsc::channel();

        thread::spawn(move || {
            started_tx.send(()).unwrap();
            result_tx
                .send(waiter.wait_timeout(Duration::from_secs(1)))
                .unwrap();
        });

        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        wakeup.notify();
        assert_eq!(result_rx.recv_timeout(Duration::from_secs(1)), Ok(true));
    }
}

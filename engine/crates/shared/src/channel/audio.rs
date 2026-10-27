//! Single-consumer audio wakeup over the pinned Rust thread parker.
//!
//! The independent pending bit is essential: dependencies may consume the
//! thread's park token. Registration/warmup happens on the consumer before
//! opening a device; notification never initializes TLS, locks, logs or allocates
//! in Migo. Native scheduling/allocator properties still need platform evidence.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread::{self, Thread};
use std::time::{Duration, Instant};

#[derive(Default)]
pub(super) struct AudioWake {
    pending: AtomicBool,
    owner: OnceLock<Thread>,
}

/// A device callback's notification capability. It cannot reach a condvar or
/// bind a receiver; only the control thread can construct one.
#[derive(Clone)]
pub struct AudioNotifier {
    pub(super) inner: Arc<AudioWake>,
}

impl AudioNotifier {
    #[inline]
    pub fn notify(&self) {
        self.inner.notify();
    }
}

impl AudioWake {
    pub(super) fn bind(&self) {
        let current = thread::current();
        if let Some(owner) = self.owner.get() {
            assert_eq!(
                owner.id(),
                current.id(),
                "audio wakeup has one fixed consumer"
            );
            return;
        }
        // Initialize the pinned std parker on its own thread. In particular,
        // Darwin's dispatch semaphore may lazily create a kernel semaphore on
        // first wait. No notifier can unpark through us before publication.
        thread::park_timeout(Duration::from_nanos(1));
        if let Err(current) = self.owner.set(current) {
            assert_eq!(
                self.owner.get().unwrap().id(),
                current.id(),
                "audio wakeup has one fixed consumer"
            );
        }
    }

    #[inline]
    pub(super) fn notify(&self) {
        // Coalesce commands, decoder completions and device refill requests.
        // Every producer still publishes through this release RMW, including
        // when an earlier producer already supplied the wakeup.
        if !self.pending.swap(true, Ordering::Release)
            && let Some(owner) = self.owner.get()
        {
            owner.unpark();
        }
    }

    pub(super) fn wait(&self) {
        self.wait_before_park(|| {});
    }

    // A generic no-op disappears from production. Tests use only atomics in
    // this gap, forcing publication before park without consuming its token.
    fn wait_before_park(&self, mut before_park: impl FnMut()) {
        self.bind();
        while !self.pending.swap(false, Ordering::Acquire) {
            // A notification in the check-to-park gap leaves a std token;
            // an unrelated/spurious token cannot manufacture pending work.
            before_park();
            thread::park();
        }
    }

    pub(super) fn wait_timeout(&self, timeout: Duration) -> bool {
        self.bind();
        let started = Instant::now();
        loop {
            if self.pending.swap(false, Ordering::Acquire) {
                return true;
            }
            let remaining = timeout.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                return false;
            }
            thread::park_timeout(remaining);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::ThreadWakeup;
    use std::sync::mpsc;

    #[test]
    fn notifications_before_registration_are_latched_and_coalesced() {
        let wake = ThreadWakeup::new_audio();
        let notify = wake.audio_notifier().unwrap();
        for _ in 0..128 {
            notify.notify();
        }
        wake.bind_audio_thread();
        assert!(wake.wait_timeout(Duration::ZERO));
        assert!(!wake.wait_timeout(Duration::ZERO));
    }

    #[test]
    fn notification_in_the_last_check_to_park_gap_is_not_lost() {
        let wake = Arc::new(AudioWake::default());
        let waiter = wake.clone();
        let entered = Arc::new(AtomicBool::new(false));
        let proceed = Arc::new(AtomicBool::new(false));
        let (done_tx, done_rx) = mpsc::channel();
        let entered_consumer = entered.clone();
        let proceed_consumer = proceed.clone();
        let consumer = thread::spawn(move || {
            waiter.wait_before_park(|| {
                entered_consumer.store(true, Ordering::Release);
                while !proceed_consumer.load(Ordering::Acquire) {
                    std::hint::spin_loop();
                }
            });
            done_tx.send(()).unwrap();
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        while !entered.load(Ordering::Acquire) {
            assert!(
                Instant::now() < deadline,
                "consumer did not enter the park gap"
            );
            thread::yield_now();
        }
        wake.notify();
        proceed.store(true, Ordering::Release);
        done_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        consumer.join().unwrap();
    }

    #[test]
    fn registered_waiter_is_woken_by_the_callback_capability() {
        let wake = ThreadWakeup::new_audio();
        let notify = wake.audio_notifier().unwrap();
        let (ready_tx, ready_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let waiter = thread::spawn(move || {
            wake.bind_audio_thread();
            ready_tx.send(()).unwrap();
            wake.wait();
            done_tx.send(()).unwrap();
        });
        ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        notify.notify();
        done_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        waiter.join().unwrap();
    }

    #[test]
    fn pending_work_survives_an_unrelated_consumer_of_the_std_token() {
        let wake = ThreadWakeup::new_audio();
        wake.bind_audio_thread();
        wake.audio_notifier().unwrap().notify();
        thread::park(); // A dependency consumed our native token.
        assert!(wake.wait_timeout(Duration::ZERO));
        assert!(!wake.wait_timeout(Duration::ZERO));
    }

    #[test]
    fn foreign_tokens_do_not_report_work_or_shorten_the_deadline() {
        let wake = ThreadWakeup::new_audio();
        wake.bind_audio_thread();
        thread::current().unpark();
        let started = Instant::now();
        let timeout = Duration::from_millis(5);
        assert!(!wake.wait_timeout(timeout));
        assert!(started.elapsed() >= timeout);
    }

    #[test]
    fn another_thread_cannot_steal_the_registered_consumer() {
        let wake = ThreadWakeup::new_audio();
        wake.bind_audio_thread();
        let other = wake.clone();
        assert!(
            thread::spawn(move || other.bind_audio_thread())
                .join()
                .is_err()
        );
        wake.notify();
        assert!(wake.wait_timeout(Duration::ZERO));
    }

    #[test]
    fn a_late_callback_is_safe_after_the_consumer_exits() {
        let wake = ThreadWakeup::new_audio();
        let notify = wake.audio_notifier().unwrap();
        thread::spawn(move || wake.bind_audio_thread())
            .join()
            .unwrap();
        for _ in 0..128 {
            notify.notify();
        }
    }

    #[test]
    fn repeated_publish_and_sleep_races_do_not_lose_work() {
        use std::sync::atomic::AtomicUsize;
        let wake = ThreadWakeup::new_audio();
        let notify = wake.audio_notifier().unwrap();
        let published = Arc::new(AtomicUsize::new(0));
        let observed = published.clone();
        let (ack_tx, ack_rx) = mpsc::channel();
        let consumer = thread::spawn(move || {
            wake.bind_audio_thread();
            for expected in 1..=2048 {
                while observed.load(Ordering::Acquire) < expected {
                    assert!(wake.wait_timeout(Duration::from_secs(5)));
                }
                ack_tx.send(expected).unwrap();
            }
        });
        for sequence in 1..=2048 {
            published.store(sequence, Ordering::Release);
            notify.notify();
            assert_eq!(
                ack_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
                sequence
            );
        }
        consumer.join().unwrap();
    }

    #[test]
    fn the_first_notification_on_fresh_callback_capabilities_has_no_heap_activity() {
        use migo_alloc_probe::thread_counts;
        let fleet: Vec<_> = (0..65)
            .map(|_| {
                let wake = ThreadWakeup::new_audio();
                wake.bind_audio_thread();
                wake.audio_notifier().unwrap()
            })
            .collect();
        migo_alloc_probe::on_native_thread(move || {
            let before_probe = thread_counts();
            drop(std::hint::black_box(Box::new(73u8)));
            assert!(
                thread_counts().allocations > before_probe.allocations,
                "the counting allocator must be installed"
            );
            let before = thread_counts();
            for notifier in &fleet {
                notifier.notify();
            }
            let after = thread_counts();
            assert_eq!(
                after, before,
                "even the first callback notification must not allocate or free"
            );
        })
        .unwrap();
    }
}

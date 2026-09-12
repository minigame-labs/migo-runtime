//! The Hosts an Engine has retired and must join before it may die.
//!
//! A retired Host remains owned by this set until a completion monitor observes
//! that its registry entry is gone. The monitor owns no Host, so a callback on a
//! Host thread can call `migo_engine_destroy`: `take` still checks the original
//! Host handle and refuses a self-join. All joins and drops happen after the
//! mutex guard is released.

use std::sync::{
    Arc, Mutex, MutexGuard, PoisonError,
    atomic::{AtomicBool, Ordering},
};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::session_engine::SessionEngine;
pub(crate) struct RetiredHost {
    /// The Host remains owned here until its registry entry disappears.
    host: SessionEngine,
    completed: Arc<AtomicBool>,
    monitor: Option<JoinHandle<()>>,
}

/// Retired Hosts, owned until their completion monitor and Host are joined.
#[derive(Default)]
pub(crate) struct RetirementSet {
    hosts: Mutex<Vec<RetiredHost>>,
}

impl RetirementSet {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Ask `host` to stop and retain it while a monitor waits for unregister.
    ///
    /// External-frame transport storage is released before ownership enters the
    /// set; late packets then fail against a fresh ingress while the Host finishes
    /// its normal teardown. The Host itself is never moved to an untracked thread.
    pub(crate) fn retire(&self, mut host: SessionEngine) {
        #[cfg(feature = "external-frames")]
        host.release_submit_resources();

        if let Err(error) = host.request_shutdown() {
            tracing::error!("failed to request shutdown for Host {}: {error}", host.id());
        }
        let id = host.id();
        let completed = Arc::new(AtomicBool::new(false));
        let completed_for_monitor = Arc::clone(&completed);
        let monitor = thread::Builder::new()
            .name(format!("Migo-Retirement-{id}"))
            .spawn(move || {
                let started = std::time::Instant::now();
                loop {
                    if migo_core::host_ingress(id).is_err() {
                        completed_for_monitor.store(true, Ordering::Release);
                        return;
                    }
                    if started.elapsed() >= Duration::from_secs(5) {
                        tracing::warn!(
                            "retired Host {id} has not exited after {:.1}s",
                            started.elapsed().as_secs_f64()
                        );
                    }
                    thread::sleep(Duration::from_millis(5));
                }
            })
            .ok();
        self.locked().push(RetiredHost {
            host,
            completed,
            monitor,
        });
    }

    /// Remove and join Hosts whose registry entries have disappeared.
    ///
    /// Completion is only a readiness hint. The monitor and Host handles are
    /// extracted first; both are joined outside the RetirementSet mutex.
    pub(crate) fn reap_completed(&self) {
        let completed = {
            let mut hosts = self.locked();
            let mut completed = Vec::new();
            let mut pending = Vec::with_capacity(hosts.len());
            for host in hosts.drain(..) {
                if host.completed.load(Ordering::Acquire) {
                    completed.push(host);
                } else {
                    pending.push(host);
                }
            }
            *hosts = pending;
            completed
        };
        for host in completed {
            if host.join().is_err() {
                tracing::error!("retired Host join failed during reap");
            }
        }
    }

    /// Every retired Host, owned, leaving the set empty.
    ///
    /// `Err` when one entry is the calling Host thread. The caller must join the
    /// returned entries outside every Engine/Session lock.
    pub(crate) fn take(&self) -> Result<Vec<RetiredHost>, ()> {
        let mut hosts = self.locked();
        if hosts.iter().any(|host| host.host.is_current_thread()) {
            return Err(());
        }
        Ok(std::mem::take(&mut *hosts))
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.locked().len()
    }

    fn locked(&self) -> MutexGuard<'_, Vec<RetiredHost>> {
        self.hosts.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl RetiredHost {
    pub(crate) fn join(mut self) -> Result<(), ()> {
        if let Some(monitor) = self.monitor.take() {
            monitor.join().map_err(|_| ())?;
        }
        self.host.join().map_err(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::mpsc, thread, time::Duration};

    use super::RetirementSet;

    /// A retired Host that parks until released, so a test can verify the
    /// monitor retains ownership while the RetirementSet remains usable.
    fn parked_host(id: i32) -> (crate::session_engine::SessionEngine, mpsc::Sender<()>) {
        let (release_tx, release_rx) = mpsc::channel();
        let join = thread::Builder::new()
            .name(format!("Migo-Main-retirement-{id}"))
            .spawn(move || {
                let _ = release_rx.recv();
            })
            .expect("spawn retired Host");
        (crate::session_engine::engine_for_test(id, join), release_tx)
    }

    #[test]
    fn completed_hosts_are_reaped_without_holding_the_set_lock() {
        let set = RetirementSet::new();
        let (host, release) = parked_host(9_001);
        set.retire(host);
        assert_eq!(set.len(), 1);
        release.send(()).expect("release retired Host");

        for _ in 0..100 {
            set.reap_completed();
            if set.len() == 0 {
                return;
            }
            thread::sleep(Duration::from_millis(2));
        }
        panic!("completed retired Host was not reaped");
    }

    #[test]
    fn take_transfers_hosts_and_engine_destruction_can_join_outside_lock() {
        let set = RetirementSet::new();
        let (first, release_first) = parked_host(9_002);
        let (second, release_second) = parked_host(9_003);
        set.retire(first);
        set.retire(second);
        assert_eq!(set.len(), 2);

        let retired = set.take().expect("take retired Hosts");
        assert_eq!(set.len(), 0);
        release_first.send(()).expect("release first Host");
        release_second.send(()).expect("release second Host");
        for host in retired {
            host.join().expect("join retired Host");
        }
    }

    #[test]
    fn repeated_retirement_reaps_completed_hosts_without_growth() {
        let set = RetirementSet::new();
        for id in 10_000..10_100 {
            let (host, release) = parked_host(id);
            set.retire(host);
            release.send(()).expect("release retired Host");
            for _ in 0..100 {
                set.reap_completed();
                if set.len() == 0 {
                    break;
                }
                thread::sleep(Duration::from_millis(1));
            }
            assert_eq!(set.len(), 0, "retirement set grew at iteration {id}");
        }
    }
}

//! The Hosts an Engine has retired and must join before it may die.
//!
//! A retired Host remains owned by this set until its thread has returned, which
//! is asked of the thread itself (`is_finished`): a join then cannot block, so
//! reaping never waits on a Host. Asking a registry instead -- whether the Host's
//! entry is gone -- answered "done" for a Host that had never registered, and a
//! reap then joined a thread that was still running. `take` checks the Host
//! handle and refuses a self-join, so a callback on a Host thread can call
//! `migo_engine_destroy`. All joins and drops happen after the mutex guard is
//! released.

use std::sync::{Mutex, MutexGuard, PoisonError};

use crate::session_engine::SessionEngine;

pub(crate) struct RetiredHost {
    host: SessionEngine,
}

/// Retired Hosts, owned until they are joined.
#[derive(Default)]
pub(crate) struct RetirementSet {
    hosts: Mutex<Vec<RetiredHost>>,
}

impl RetirementSet {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Ask `host` to stop and retain it until its thread returns.
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
        self.locked().push(RetiredHost { host });
    }

    /// Remove and join the Hosts whose threads have returned.
    ///
    /// Only finished threads are taken, so no join here waits; they are joined
    /// outside the RetirementSet mutex.
    pub(crate) fn reap_completed(&self) {
        let completed = {
            let mut hosts = self.locked();
            let mut completed = Vec::new();
            let mut pending = Vec::with_capacity(hosts.len());
            for host in hosts.drain(..) {
                if host.host.is_finished() {
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
    fn a_host_still_running_is_never_reaped_even_one_that_never_registered() {
        // A registry would read this Host as gone -- it never registered -- and
        // a reap that believed it would join a thread parked on `release`.
        let set = RetirementSet::new();
        let (host, release) = parked_host(9_004);
        set.retire(host);
        for _ in 0..20 {
            set.reap_completed();
            thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(set.len(), 1, "a running Host was reaped");

        release.send(()).expect("release retired Host");
        for _ in 0..100 {
            set.reap_completed();
            if set.len() == 0 {
                return;
            }
            thread::sleep(Duration::from_millis(2));
        }
        panic!("the Host returned and was not reaped");
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

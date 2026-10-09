use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use parking_lot::{Condvar, Mutex};
use shared::services::Scope;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Denied;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lifecycle {
    Active,
    Closing,
}

#[derive(Debug)]
struct HostPermissions {
    lifecycle: Lifecycle,
    scopes: HashMap<Scope, bool>,
    active_operations: usize,
}

#[derive(Debug)]
struct HostControl {
    transition: Mutex<()>,
    permissions: Mutex<HostPermissions>,
    idle: Condvar,
}

type HostState = Arc<HostControl>;

#[derive(Debug, Default)]
struct Hosts {
    /// Ids whose session has been cleared. A tombstone must reject exactly the
    /// ids that were retired -- not every id at or below the highest ever
    /// opened. Ids are allocated on the caller thread but opened from each
    /// session's own thread, so they do not arrive here in allocation order,
    /// and a high-water mark would refuse a live lower-id session outright.
    /// Bounded by the number of sessions the process ever created.
    retired: HashSet<i32>,
    live: HashMap<i32, HostState>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum UpdateError<E> {
    Closed,
    Cleanup(E),
}

/// Serializes a session's protected Android calls with permission updates.
///
/// The main JS Host is serialized already, but workers and host permission
/// callbacks can arrive concurrently. Admission is recorded under the
/// per-host mutex, while the external operation runs under a counted lease so
/// revocation can wait without retaining that mutex across JNI. The
/// `parking_lot` primitives are deliberately non-poisoning: a caught panic must
/// not prevent later cleanup.
#[derive(Default)]
pub(crate) struct PermissionGate {
    hosts: Mutex<Hosts>,
}

struct OperationLease<'a> {
    host: &'a HostControl,
}

impl Drop for OperationLease<'_> {
    fn drop(&mut self) {
        let mut state = self.host.permissions.lock();
        state.active_operations -= 1;
        if state.active_operations == 0 {
            self.host.idle.notify_all();
        }
    }
}

#[cfg_attr(test, allow(dead_code))]
impl PermissionGate {
    fn host_state(&self, host_id: i32) -> Option<HostState> {
        self.hosts.lock().live.get(&host_id).cloned()
    }

    /// Admit a session id, or refuse one that is already live or was retired.
    ///
    /// Refusal is not advisory: no `HostControl` exists for a refused id, so
    /// every later permission check for it is denied. Callers must not discard
    /// the answer.
    #[must_use]
    pub(crate) fn open(&self, host_id: i32) -> bool {
        let mut hosts = self.hosts.lock();
        if hosts.retired.contains(&host_id) || hosts.live.contains_key(&host_id) {
            return false;
        }
        hosts.live.insert(
            host_id,
            Arc::new(HostControl {
                transition: Mutex::new(()),
                permissions: Mutex::new(HostPermissions {
                    lifecycle: Lifecycle::Active,
                    scopes: HashMap::new(),
                    active_operations: 0,
                }),
                idle: Condvar::new(),
            }),
        );
        true
    }

    /// Record a permission decision, and on a revocation tear the scope's
    /// resources down once nothing is still using them.
    ///
    /// **`cleanup` runs with this host's transition mutex held, and must not
    /// re-enter the gate for the same host.** It is the only place in this crate
    /// where a lock spans a call out to Java, and it is deliberate: the mutex is
    /// what keeps a later grant of the same scope from taking effect while the
    /// revocation's teardown is still in flight, so releasing it first would let
    /// a freshly granted operation's resources be destroyed by the revocation it
    /// raced. The `permissions` mutex -- the one every per-event protected call
    /// takes -- is released before `cleanup`, so this does not serialise the hot
    /// path.
    ///
    /// The shipped Android implementation satisfies the constraint by
    /// construction: `NativeExports.revokePermissionResources` reports failure by
    /// scheduling `GameSession::close` through `sMainHandler::post`, so its
    /// re-entry lands as a later looper message rather than as a nested native
    /// call. An embedder that instead called back synchronously would deadlock
    /// here, which is why the requirement is stated rather than left to be
    /// inferred from the lock's scope.
    pub(crate) fn update<E>(
        &self,
        host_id: i32,
        scope: Scope,
        granted: bool,
        cleanup: impl FnOnce() -> Result<(), E>,
    ) -> Result<(), UpdateError<E>> {
        let host = self.host_state(host_id).ok_or(UpdateError::Closed)?;
        let _transition = host.transition.lock();
        {
            let mut state = host.permissions.lock();
            if state.lifecycle == Lifecycle::Closing {
                return Err(UpdateError::Closed);
            }
            state.scopes.insert(scope, granted);
            if !granted {
                while state.active_operations != 0 {
                    host.idle.wait(&mut state);
                }
            }
        }
        if !granted {
            cleanup().map_err(UpdateError::Cleanup)?;
        }
        Ok(())
    }

    pub(crate) fn clear(&self, host_id: i32) {
        self.clear_with(host_id, || {});
    }

    fn clear_with(&self, host_id: i32, while_closing: impl FnOnce()) {
        let Some(host) = self.host_state(host_id) else {
            return;
        };
        let _transition = host.transition.lock();
        {
            let mut state = host.permissions.lock();
            state.lifecycle = Lifecycle::Closing;
            state.scopes.clear();
            while state.active_operations != 0 {
                host.idle.wait(&mut state);
            }
        }
        while_closing();
        let mut hosts = self.hosts.lock();
        if hosts
            .live
            .get(&host_id)
            .is_some_and(|current| Arc::ptr_eq(current, &host))
        {
            hosts.live.remove(&host_id);
            hosts.retired.insert(host_id);
        }
    }

    /// Whether `host_id` belonged to a session that has been cleared.
    ///
    /// Retirement is monotonic, so reading it after a refusal cannot turn a
    /// retired id back into a live one.
    fn is_retired(&self, host_id: i32) -> bool {
        self.hosts.lock().retired.contains(&host_id)
    }

    /// Admit a session id and hand back its handle, reporting the one refusal that is
    /// a bug.
    ///
    /// A refusal is legitimate when the id is still live: a session restart
    /// rebuilds device services for a host whose grants must survive, and the
    /// returned handle is that host's existing one. A *retired* id is not -- no
    /// `HostControl` will ever exist for it, so every permission check that session
    /// makes is denied for its whole life, which content cannot distinguish from the
    /// user refusing. That is silent, so it is logged loudly and trips a debug build.
    ///
    /// This is the **only** acquisition of the live-host map a session is allowed:
    /// everything per-event goes through the returned [`SessionGate`].
    pub(crate) fn open_session(&self, host_id: i32) -> SessionGate {
        if !self.open(host_id) && self.is_retired(host_id) {
            tracing::error!(
                host_id,
                "permission gate refused a retired host id; every permission \
                 check for this session will be denied"
            );
            debug_assert!(
                false,
                "permission gate refused retired host id {host_id}; \
                 permissions will be denied for this session"
            );
        }
        self.session(host_id)
    }

    /// The handle for an id that has already been admitted, without admitting it and
    /// without reporting a refusal.
    ///
    /// Separate from [`Self::open_session`] because the report is a `debug_assert!`:
    /// a caller that wants a handle for an id it knows to be retired -- which is what
    /// asserting the retirement holds requires -- must not trip it.
    pub(crate) fn session(&self, host_id: i32) -> SessionGate {
        SessionGate {
            host_id,
            host: self.host_state(host_id),
        }
    }

    #[cfg(test)]
    fn live_host_count_for_tests(&self) -> usize {
        self.hosts.lock().live.len()
    }
}

/// One session's handle on the permission gate, resolved once when its device services
/// are built.
///
/// **Why a handle and not an id.** The steady-state requirement forbids a per-event path acquiring a
/// lock shared beyond its own session, and the gate's live-host map is exactly that:
/// every gated Android device call used to look the session up in it first, including
/// the Bluetooth characteristic writes, which are a steady hot path. Two
/// sessions doing that traffic serialised on one mutex. The handle removes the lookup
/// rather than making it cheaper, which is the same move made for the text
/// texture cache and the input path made for the debug-stats registry.
///
/// **Holding the control block is also what makes refusal correct after teardown.**
/// `clear` marks the lifecycle `Closing` before it removes the map entry, so a handle
/// that outlives its session refuses through the flag rather than through the entry's
/// absence — the same answer, without the map.
///
/// `host` is `None` only for an id the gate refused as retired. That is not a
/// fallback: no `HostControl` will ever exist for it, so every check is denied, which
/// is what [`PermissionGate::open_session`] reports.
#[derive(Clone)]
pub(crate) struct SessionGate {
    host_id: i32,
    host: Option<HostState>,
}

impl SessionGate {
    /// The id this session was admitted under, for the JNI calls that still need it.
    pub(crate) fn host_id(&self) -> i32 {
        self.host_id
    }

    /// Run a protected operation under this session's admission, refusing if the
    /// required scope is not granted or the session is closing.
    ///
    /// The external operation runs under a counted lease rather than the host mutex,
    /// so revocation can wait for it without retaining that mutex across JNI.
    pub(crate) fn run<T>(
        &self,
        required_scope: Option<Scope>,
        operation: impl FnOnce() -> T,
    ) -> Result<T, Denied> {
        let host = self.host.as_ref().ok_or(Denied)?;
        {
            let mut state = host.permissions.lock();
            if state.lifecycle == Lifecycle::Closing {
                return Err(Denied);
            }
            if let Some(scope) = required_scope
                && state.scopes.get(&scope) != Some(&true)
            {
                return Err(Denied);
            }
            state.active_operations += 1;
        }
        let lease = OperationLease { host };
        let result = operation();
        drop(lease);
        Ok(result)
    }

    /// What the host has said about `scope`: `Some(granted)`, or `None` when it has
    /// not spoken or the session is closing.
    pub(crate) fn scope_state(&self, scope: Scope) -> Option<bool> {
        let host = self.host.as_ref()?;
        let state = host.permissions.lock();
        if state.lifecycle == Lifecycle::Closing {
            return None;
        }
        state.scopes.get(&scope).copied()
    }
}

#[cfg(test)]
mod tests {
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::sync::{Arc, Mutex, mpsc};
    use std::thread;

    use migo_contention_probe::{PATIENCE, PerEventPath, assert_completes_while_mutex_locked};

    use super::*;

    /// The steady-state requirement: no per-event path acquires a lock shared beyond its own session.
    ///
    /// This is the path that requirement was first written for, and had
    /// no named gate until this test. Every gated Android device call goes through
    /// `permission_jni_call` to `PermissionGate::run`, and that includes the Bluetooth
    /// characteristic writes, which are a steady hot path.
    ///
    /// The lock's process-wide scope comes from `PERMISSION_GATE` being a `OnceLock`
    /// singleton, so a gate built here has the same relationship to `hosts` that the
    /// shipped one does: one map, every session in it. Holding it and requiring a gated
    /// call to finish therefore asks exactly the production question.
    #[test]
    fn a_gated_device_call_does_not_reach_the_process_wide_live_host_map() {
        const HOST: i32 = 9701;
        let gate = Arc::new(PermissionGate::default());
        assert!(gate.open(HOST), "test setup failed to open the host");
        gate.update(HOST, Scope::Bluetooth, true, || Ok::<(), ()>(()))
            .expect("test setup failed to grant the scope");

        let session = gate.session(HOST);
        let wrote = assert_completes_while_mutex_locked(
            PerEventPath {
                path: "a gated Bluetooth characteristic write",
                shared_lock: "the permission gate's live-host map",
                patience: PATIENCE,
            },
            &gate.hosts,
            move || session.run(Some(Scope::Bluetooth), || 0xB1E),
        );
        // The burst has to have happened: a refused call completes instantly and would
        // satisfy the gate while proving nothing about the lock.
        assert_eq!(
            wrote,
            Ok(0xB1E),
            "the gated call must have been admitted, or its completion says nothing"
        );
    }

    #[test]
    fn revocation_waits_for_admitted_operation_then_tears_it_down() {
        let gate = Arc::new(PermissionGate::default());
        assert!(gate.open(41), "test setup failed to open the host");
        gate.update(41, Scope::Camera, true, || Ok::<(), ()>(()))
            .unwrap();

        let resource_live = Arc::new(Mutex::new(false));
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let operation_session = gate.session(41);
        let operation_resource = resource_live.clone();
        let operation = thread::spawn(move || {
            operation_session
                .run(Some(Scope::Camera), || {
                    entered_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    *operation_resource.lock().unwrap() = true;
                })
                .unwrap();
        });
        entered_rx.recv().unwrap();

        let (revoked_tx, revoked_rx) = mpsc::channel();
        let revocation_gate = gate.clone();
        let revocation_resource = resource_live.clone();
        let revocation = thread::spawn(move || {
            revocation_gate
                .update(41, Scope::Camera, false, || {
                    *revocation_resource.lock().unwrap() = false;
                    Ok::<(), ()>(())
                })
                .unwrap();
            revoked_tx.send(()).unwrap();
        });

        assert!(
            revoked_rx.try_recv().is_err(),
            "revocation bypassed the live operation"
        );
        release_tx.send(()).unwrap();
        operation.join().unwrap();
        revocation.join().unwrap();
        revoked_rx.recv().unwrap();

        assert!(
            !*resource_live.lock().unwrap(),
            "resource survived revocation"
        );
        assert_eq!(
            gate.session(41).run(Some(Scope::Camera), || ()),
            Err(Denied)
        );
    }

    /// A handle that outlives its session must be refused, and only a handle can ask.
    ///
    /// Resolving the session once at bring-up moved which guard does the refusing.
    /// Before, `clear` removing the live-host entry was enough on its own: every call
    /// looked the id up and got nothing. A handle keeps the `HostControl` alive, so the
    /// `Closing` lifecycle flag -- previously belt and braces behind the map -- is now
    /// the whole of it.
    ///
    /// Nothing else can see that. `clear_waits_for_inflight_update_then_leaves_a_tombstone`
    /// and `a_cleared_id_stays_retired_when_a_higher_id_opens_afterwards` both ask the
    /// gate for a handle *after* the clear, so they get an empty one and are satisfied by
    /// the map alone. The `Ok(())` before the clear here is the positive control: without
    /// it, a gate that refused everything would pass.
    ///
    /// **The call is deliberately unscoped, and the first version of this test was
    /// wrong for want of that.** `close_adapter` and `stop_devices_discovery` are real
    /// unscoped protected calls, and an unscoped call is the only one whose post-teardown
    /// refusal can come from nothing but the lifecycle flag: `clear` empties the scope map
    /// as well, so a scoped call is refused for want of a grant even by a `clear` that
    /// never marked the session closing. Asserted with `Some(Scope::Camera)`, the mutant
    /// that removes the flag walked past all 52 tests.
    #[test]
    fn a_handle_taken_before_teardown_is_refused_after_it() {
        let gate = PermissionGate::default();
        let session = gate.open_session(81);
        assert_eq!(
            session.run(None, || ()),
            Ok(()),
            "an unscoped protected call was refused before teardown, so the refusal \
             below proves nothing"
        );

        gate.clear(81);

        assert_eq!(
            session.run(None, || ()),
            Err(Denied),
            "a handle that outlived its session kept admitting protected calls"
        );
    }

    #[test]
    fn caught_operation_panic_does_not_block_later_cleanup() {
        let gate = PermissionGate::default();
        assert!(gate.open(51), "test setup failed to open the host");

        let panic = catch_unwind(AssertUnwindSafe(|| {
            let _ = gate.session(51).run(None, || panic!("operation failed"));
        }));
        assert!(panic.is_err());

        let mut cleaned = false;
        gate.update(51, Scope::Camera, false, || {
            cleaned = true;
            Ok::<(), ()>(())
        })
        .unwrap();
        assert!(cleaned, "a prior operation panic prevented cleanup");
    }

    #[test]
    fn protected_operation_does_not_hold_host_mutex_across_external_code() {
        let gate = PermissionGate::default();
        assert!(gate.open(56), "test setup failed to open the host");
        gate.update(56, Scope::Camera, true, || Ok::<(), ()>(()))
            .unwrap();
        let host = gate.host_state(56).expect("open host");

        gate.session(56)
            .run(Some(Scope::Camera), || {
                assert!(
                    host.permissions.try_lock().is_some(),
                    "protected operation retained the Rust host mutex across JNI"
                );
            })
            .unwrap();
    }

    #[test]
    fn clear_waits_for_inflight_update_then_leaves_a_tombstone() {
        let gate = Arc::new(PermissionGate::default());
        assert!(gate.open(61), "test setup failed to open the host");
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();

        let updating = gate.clone();
        let update = thread::spawn(move || {
            updating.update(61, Scope::Camera, false, || {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok::<(), ()>(())
            })
        });
        entered_rx.recv().unwrap();

        let clearing = gate.clone();
        let (cleared_tx, cleared_rx) = mpsc::channel();
        let clear = thread::spawn(move || {
            clearing.clear(61);
            cleared_tx.send(()).unwrap();
        });
        assert!(cleared_rx.try_recv().is_err());

        release_tx.send(()).unwrap();
        update.join().unwrap().unwrap();
        clear.join().unwrap();
        assert_eq!(gate.session(61).scope_state(Scope::Camera), None);
        assert!(
            gate.update(61, Scope::Camera, true, || Ok::<(), ()>(()))
                .is_err()
        );
    }

    #[test]
    fn update_waiting_behind_clear_cannot_recreate_the_host() {
        let gate = Arc::new(PermissionGate::default());
        assert!(gate.open(71), "test setup failed to open the host");
        let (closing_tx, closing_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();

        let clearing = gate.clone();
        let clear = thread::spawn(move || {
            clearing.clear_with(71, || {
                closing_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            });
        });
        closing_rx.recv().unwrap();

        let updating = gate.clone();
        let (updated_tx, updated_rx) = mpsc::channel();
        let update = thread::spawn(move || {
            let result = updating.update(71, Scope::Camera, true, || Ok::<(), ()>(()));
            updated_tx.send(result).unwrap();
        });
        assert!(updated_rx.try_recv().is_err());

        release_tx.send(()).unwrap();
        clear.join().unwrap();
        assert!(updated_rx.recv().unwrap().is_err());
        update.join().unwrap();
        assert_eq!(gate.session(71).scope_state(Scope::Camera), None);
    }

    #[test]
    fn a_lower_host_id_opened_after_a_higher_one_still_gets_its_permissions() {
        // Host ids are allocated on the caller thread but opened from each
        // session's own thread, so two sessions starting together can arrive
        // here in the opposite order. Neither is retired, so both must open.
        let gate = PermissionGate::default();
        assert!(gate.open(7), "the first session was refused");
        assert!(
            gate.open(5),
            "a live session was refused because a higher id opened first"
        );

        gate.update(5, Scope::Camera, true, || Ok::<(), ()>(()))
            .expect("granting a scope on an open host");
        assert_eq!(
            gate.session(5).run(Some(Scope::Camera), || ()),
            Ok(()),
            "a granted scope surfaced as a denial"
        );
        assert_eq!(gate.session(5).scope_state(Scope::Camera), Some(true));
    }

    #[test]
    fn a_cleared_id_stays_retired_when_a_higher_id_opens_afterwards() {
        let gate = PermissionGate::default();
        assert!(gate.open(11));
        gate.clear(11);

        // A later, unrelated session must not resurrect the retired id.
        assert!(gate.open(12));
        assert!(!gate.open(11), "a retired id was reopened");
        assert_eq!(gate.session(11).run(None, || ()), Err(Denied));
        assert_eq!(gate.session(11).scope_state(Scope::Camera), None);
    }

    #[test]
    fn clearing_one_host_leaves_another_live_host_untouched() {
        let gate = PermissionGate::default();
        assert!(gate.open(22));
        assert!(gate.open(21));
        gate.update(21, Scope::Camera, true, || Ok::<(), ()>(()))
            .expect("granting a scope on an open host");

        gate.clear(22);

        assert_eq!(
            gate.session(21).run(Some(Scope::Camera), || ()),
            Ok(()),
            "clearing a sibling host denied a live host"
        );
        // A fresh id below the cleared one is still admissible.
        assert!(gate.open(20));
        assert_eq!(gate.session(20).run(None, || ()), Ok(()));
        assert_eq!(gate.live_host_count_for_tests(), 2);
    }

    #[test]
    fn reopening_a_live_host_is_tolerated_because_a_restart_rebuilds_services() {
        // `on_restart` rebuilds device services for the same, still-live id and
        // does not clear permissions, so its grants must survive untouched.
        let gate = PermissionGate::default();
        let first = gate.open_session(31);
        gate.update(31, Scope::Camera, true, || Ok::<(), ()>(()))
            .expect("granting a scope on an open host");

        let rebuilt = gate.open_session(31);

        assert_eq!(
            rebuilt.scope_state(Scope::Camera),
            Some(true),
            "rebuilding services for a live host dropped its grants"
        );
        assert_eq!(rebuilt.run(Some(Scope::Camera), || ()), Ok(()));
        assert_eq!(
            first.scope_state(Scope::Camera),
            Some(true),
            "the handle the pre-restart services hold stopped seeing its own grants"
        );
        assert_eq!(gate.live_host_count_for_tests(), 1);
    }

    /// The report is a `debug_assert!`, so what "reported" means differs by
    /// profile and both are asserted here. A single unconditional
    /// `#[should_panic]` failed the release suite outright, which is worse than
    /// the silence it was written to catch: it makes `cargo test --release`
    /// unusable and so stops anyone running the rest of these tests in the
    /// profile that ships.
    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "refused retired host id")]
    fn reopening_a_retired_host_trips_a_debug_build() {
        let gate = PermissionGate::default();
        let _ = gate.open_session(32);
        gate.clear(32);
        let _ = gate.open_session(32);
    }

    /// In release the report is a log line, so the observable contract is that
    /// the call returns and the retired id is still refused. Without this the
    /// release profile would assert nothing at all about this case.
    #[test]
    #[cfg(not(debug_assertions))]
    fn reopening_a_retired_host_returns_without_admitting_it_in_release() {
        let gate = PermissionGate::default();
        let _ = gate.open_session(32);
        gate.clear(32);
        let _ = gate.open_session(32);
        assert_eq!(gate.live_host_count_for_tests(), 0);
        assert!(!gate.open(32));
    }

    #[test]
    fn successful_clear_reclaims_hosts_without_allowing_id_reuse() {
        let gate = PermissionGate::default();
        for host_id in 1000..2000 {
            assert!(gate.open(host_id));
            gate.clear(host_id);
        }

        assert_eq!(gate.live_host_count_for_tests(), 0);
        assert!(!gate.open(1500));
        assert!(gate.open(2000));
        assert!(!gate.open(2000));
        gate.clear(2000);
        assert_eq!(gate.live_host_count_for_tests(), 0);
        assert!(!gate.open(2000));
    }
}

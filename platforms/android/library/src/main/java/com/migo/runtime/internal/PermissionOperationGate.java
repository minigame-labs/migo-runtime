package com.migo.runtime.internal;

import java.util.ArrayList;
import java.util.HashMap;
import java.util.HashSet;
import java.util.Map;
import java.util.Objects;
import java.util.Set;
import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.ConcurrentMap;
import java.util.function.BooleanSupplier;

/** Linearizes permission updates, deferred framework entry, and terminal close per session. */
public final class PermissionOperationGate {
    public static final class Result {
        private final RuntimeException failure;

        private Result(RuntimeException failure) {
            this.failure = failure;
        }

        public RuntimeException failure() {
            return failure;
        }
    }

    public static final class Pending {
        private final Session session;
        private final Entry entry;
        private ResourceCleanup.Action cancellation;
        private boolean active = true;

        private Pending(
                Session session,
                Entry entry,
                ResourceCleanup.Action cancellation) {
            this.session = session;
            this.entry = entry;
            this.cancellation = cancellation;
        }

        public void setCancellation(ResourceCleanup.Action cancellation) {
            Objects.requireNonNull(cancellation, "cancellation");
            synchronized (session) {
                this.cancellation = cancellation;
            }
        }
    }

    private enum Lifecycle {
        ACTIVE,
        CLOSING
    }

    private static final class Session {
        final Object transition = new Object();
        Lifecycle lifecycle = Lifecycle.ACTIVE;
        final Map<String, Entry> scopes = new HashMap<>();
    }

    private static final class Entry {
        boolean granted;
        int activeRuns;
        final Set<Pending> pending = new HashSet<>();
    }

    /**
     * Concurrent so a per-event callback admission resolves its session without taking a
     * lock shared with every other session. Only admission and retirement need mutual
     * exclusion, and {@link #openGuard} provides it.
     */
    private final ConcurrentMap<Integer, Session> sessions = new ConcurrentHashMap<>();

    /**
     * Mutual exclusion for admission and retirement only, and package-private so the
     * contention gate in this package can hold it.
     *
     * Specification Section 7.3 requires a test that fails when a per-event operation
     * acquires a lock shared beyond its own session, and says these are enforced by tests
     * rather than by inspection. The only way to observe an acquisition rather than reason
     * about one is to hold the lock and require the per-event path to finish anyway, which
     * needs the monitor a test can synchronize on. Reflection would reach a private field
     * too, and would go on compiling after a rename.
     */
    final Object openGuard = new Object();

    /**
     * Ids whose session has been closed. A tombstone must refuse exactly the ids that were
     * retired -- not every id at or below the highest ever opened. Those coincide only when
     * ids are opened in increasing order, and they are not: ids are allocated on the caller
     * thread while the gate is opened from each session's own thread, so two sessions
     * starting together can arrive in the opposite order and a high-water mark would refuse
     * the live lower-id session outright.
     *
     * <p>A plain set guarded by {@link #openGuard} rather than a concurrent one, because
     * retiring an id and dropping its session must be one step: a concurrent set would let
     * an {@link #open} of the same id slip between the removal and the retirement and
     * resurrect it. Bounded by the number of sessions the process ever created.
     */
    private final Set<Integer> retiredSessionIds = new HashSet<>();

    /** What an admission attempt did, and when it refused, which refusal it was. */
    public enum Admission {
        ADMITTED,
        /** The id is still live, so this is a duplicate registration. */
        ALREADY_LIVE,
        /** The id belonged to a closed session; every permission check for it is denied. */
        RETIRED
    }

    /**
     * Admit a previously unseen session, or say why not. A closing tombstone is never
     * reopened.
     *
     * The two refusals mean different things to a caller and are answered in the *same*
     * acquisition of {@link #openGuard} rather than by a second query, so no caller can
     * observe a state between them and none can recompute the distinction differently.
     */
    public Admission admit(int sessionId) {
        synchronized (openGuard) {
            if (retiredSessionIds.contains(sessionId)) return Admission.RETIRED;
            if (sessions.containsKey(sessionId)) return Admission.ALREADY_LIVE;
            sessions.put(sessionId, new Session());
            return Admission.ADMITTED;
        }
    }

    /**
     * Whether this id belonged to a session that has since closed.
     *
     * <p>Separate from {@link #admit} because a caller may need the fact without claiming
     * the id — `NativeExports` retains a late surface-loss report only for a session that
     * has not registered *yet*, and "absent from the live map" alone cannot tell that from
     * "already gone". Answering it from the set that already exists is what keeps there
     * from being a second process-life record of the same fact.
     *
     * <p>Not a substitute for `admit`'s answer: this is a query, so a concurrent close can
     * make it stale the moment it returns. Only use it where a stale `false` is harmless.
     */
    public boolean isRetired(int sessionId) {
        synchronized (openGuard) {
            return retiredSessionIds.contains(sessionId);
        }
    }

    public Result update(
            int sessionId,
            String scope,
            boolean granted,
            BooleanSupplier updateNative) {
        Session session = session(sessionId);
        if (session == null) return rejected("permission session is not open");
        synchronized (session.transition) {
            Entry entry;
            synchronized (session) {
                if (session.lifecycle != Lifecycle.ACTIVE) {
                    return rejected("permission session is closing");
                }
                entry = entry(session, scope);
                entry.granted = false;
                if (!granted) awaitIdle(session, entry);
            }
            try {
                if (granted) {
                    requireNativeSuccess(updateNative);
                    synchronized (session) {
                        entry.granted = true;
                    }
                } else {
                    ResourceCleanup.runAll(
                            () -> runCancellations(session, entry),
                            () -> requireNativeSuccess(updateNative));
                }
                return new Result(null);
            } catch (RuntimeException error) {
                return new Result(error);
            }
        }
    }

    public Result revoke(int sessionId, String scope) {
        Session session = session(sessionId);
        if (session == null) return new Result(null);
        synchronized (session.transition) {
            Entry entry;
            synchronized (session) {
                entry = session.scopes.get(scope);
                if (entry == null) return new Result(null);
                entry.granted = false;
                awaitIdle(session, entry);
            }
            try {
                runCancellations(session, entry);
                return new Result(null);
            } catch (RuntimeException error) {
                return new Result(error);
            }
        }
    }

    /** Marks the session closing and retries every retained pending cancellation. */
    public Result close(int sessionId) {
        Session session = session(sessionId);
        if (session == null) return new Result(null);
        synchronized (session.transition) {
            Result result;
            ArrayList<Entry> entries;
            synchronized (session) {
                session.lifecycle = Lifecycle.CLOSING;
                for (Entry entry : session.scopes.values()) {
                    entry.granted = false;
                }
                entries = new ArrayList<>(session.scopes.values());
                for (Entry entry : entries) {
                    awaitIdle(session, entry);
                }
            }
            ArrayList<ResourceCleanup.Action> cancellations = new ArrayList<>();
            for (Entry entry : entries) {
                cancellations.add(() -> runCancellations(session, entry));
            }
            try {
                ResourceCleanup.runAll(
                        cancellations.toArray(new ResourceCleanup.Action[0]));
                result = new Result(null);
            } catch (RuntimeException error) {
                result = new Result(error);
            }
            if (result.failure() == null) {
                // Retirement and removal are one step under the admission guard, so no open
                // can observe the id as neither live nor retired and resurrect it. A failed
                // close returns above with the session retained for retry and must not
                // retire an id that is still live.
                synchronized (openGuard) {
                    if (sessions.remove(sessionId, session)) {
                        retiredSessionIds.add(sessionId);
                    }
                }
            }
            return result;
        }
    }

    public Pending register(
            int sessionId,
            String scope,
            ResourceCleanup.Action cancellation) {
        Session session = session(sessionId);
        if (session == null) return null;
        synchronized (session) {
            if (session.lifecycle != Lifecycle.ACTIVE) return null;
            Entry entry = session.scopes.get(scope);
            if (entry == null || !entry.granted) return null;
            Pending pending = new Pending(session, entry, cancellation);
            entry.pending.add(pending);
            return pending;
        }
    }

    public Pending register(int sessionId, String scope) {
        return register(sessionId, scope, () -> {});
    }

    public boolean enter(Pending pending, Runnable frameworkCall) {
        if (pending == null) return false;
        synchronized (pending.session) {
            if (pending.session.lifecycle != Lifecycle.ACTIVE
                    || !pending.active
                    || !pending.entry.granted) {
                return false;
            }
            frameworkCall.run();
            return true;
        }
    }

    /** Runs an admitted scope callback while denial retains a draining lease. */
    public boolean runIfGranted(
            int sessionId,
            String scope,
            BooleanSupplier callback) {
        Objects.requireNonNull(callback, "callback");
        Session session = session(sessionId);
        if (session == null) return false;
        Entry entry;
        synchronized (session) {
            if (session.lifecycle != Lifecycle.ACTIVE) return false;
            entry = session.scopes.get(scope);
            if (entry == null || !entry.granted) return false;
            entry.activeRuns++;
        }
        try {
            return callback.getAsBoolean();
        } finally {
            synchronized (session) {
                entry.activeRuns--;
                if (entry.activeRuns == 0) session.notifyAll();
            }
        }
    }

    public void finish(Pending pending) {
        if (pending == null) return;
        synchronized (pending.session) {
            pending.active = false;
            pending.entry.pending.remove(pending);
        }
    }

    int retainedSessionCountForTests() {
        return sessions.size();
    }

    private Session session(int sessionId) {
        return sessions.get(sessionId);
    }

    private static Entry entry(Session session, String scope) {
        Entry existing = session.scopes.get(scope);
        if (existing != null) return existing;
        Entry created = new Entry();
        session.scopes.put(scope, created);
        return created;
    }

    private static Result rejected(String message) {
        return new Result(new IllegalStateException(message));
    }

    /**
     * Runs each retained cancellation with the session monitor released, so a foreign
     * cleanup action cannot convoy unrelated scopes. The caller must already hold the
     * transition lock and have published a denied or closing state, which is what stops a
     * new lease or registration from appearing while these actions run.
     *
     * <p>Each action is captured while the pending set is snapshotted, under the same
     * monitor {@link Pending#setCancellation} publishes it with, so the action invoked is
     * unambiguously the one the snapshot admitted rather than whatever the non-final field
     * happens to hold once the monitor has been released. A pending is retired only after
     * its action returns normally, so a throwing cancellation stays for a later retry.
     */
    private static void runCancellations(Session session, Entry entry) {
        ArrayList<ResourceCleanup.Action> cancellations = new ArrayList<>();
        synchronized (session) {
            for (Pending pending : new ArrayList<>(entry.pending)) {
                ResourceCleanup.Action cancellation = pending.cancellation;
                cancellations.add(() -> {
                    cancellation.run();
                    synchronized (session) {
                        pending.active = false;
                        entry.pending.remove(pending);
                    }
                });
            }
        }
        ResourceCleanup.runAll(cancellations.toArray(new ResourceCleanup.Action[0]));
    }

    private static void awaitIdle(Session session, Entry entry) {
        boolean interrupted = false;
        while (entry.activeRuns != 0) {
            try {
                session.wait();
            } catch (InterruptedException error) {
                interrupted = true;
            }
        }
        if (interrupted) Thread.currentThread().interrupt();
    }

    private static void requireNativeSuccess(BooleanSupplier updateNative) {
        if (!updateNative.getAsBoolean()) {
            throw new IllegalStateException("native permission update failed");
        }
    }
}

package com.migo.runtime.internal;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertNotEquals;
import static org.junit.Assert.assertNotNull;
import static org.junit.Assert.assertNull;
import static org.junit.Assert.assertTrue;

import java.lang.reflect.Field;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.Collections;
import java.util.List;
import java.util.concurrent.ConcurrentMap;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.CyclicBarrier;
import java.util.concurrent.TimeUnit;
import com.migo.runtime.internal.PermissionOperationGate.Admission;
import org.junit.Test;

public final class PermissionOperationGateTest {
    /**
     * Budget for asserting that a transition stays blocked behind a lease. A correct drain
     * can never release it, so only a regression can make this observation fail.
     */
    private static final long BLOCKED_OBSERVATION_MILLIS = 200;

    /**
     * Budget for requiring a per-event path to finish while the admission guard is held.
     * Six orders of magnitude above what an unblocked lookup needs, because the cost of
     * being wrong the other way is a flaky gate. Shortening it can only make a correct
     * path look blocked, which fails closed.
     */
    private static final long CONTENTION_PATIENCE_MILLIS = 2_000;

    /**
     * Section 7.3: no per-event path acquires a lock shared beyond its own session.
     *
     * This is the gate that requirement was first written for on this side of the JNI
     * boundary, and the JVM half the Rust probe says nothing about. An earlier attempt was
     * withdrawn for being unable to fail -- it took the shared lock inside the very helper
     * it called -- and the design recorded to replace it was JVM thread contention
     * monitoring plus an assertion that admission-attributable blocked time is zero. That
     * measures the same structural fact less directly and has the shape of a metric that
     * cannot fail: a run where nothing was admitted also blocks for zero milliseconds.
     *
     * So this takes the Rust probe's shape instead. Manufacture the contention rather than
     * wait for load, by holding {@code openGuard} and requiring the per-event admission --
     * {@code runIfGranted}, which is what a BLE characteristic notification takes -- to
     * complete on another thread anyway. An uncontended acquisition, which a load test
     * cannot see, fails this too.
     *
     * Three details are load-bearing. The admission runs on another thread, because on the
     * holder's own thread Java's monitors are reentrant and it would pass with or without
     * the property. Saturation is asserted before the admission starts, since an admission
     * handed an unheld guard proves nothing. And the callback's own return value is
     * asserted, because a refused admission returns instantly and would satisfy the timing
     * assertion while never reaching the lookup at all.
     */
    /**
     * The two refusals are different facts and the gate is what knows which.
     *
     * `admit` replaced a `boolean open`, and the boolean is why the one caller that acts on
     * a refusal --- `NativeExports.registerSession`, which throws from the `GameSession`
     * constructor --- threw a message naming the closing case for both. A duplicate
     * registration and a reused id call for different things from a host: the first is two
     * sessions sharing an id, the second is an id whose permissions can never be granted
     * again.
     *
     * Answered in one acquisition of the admission guard rather than by a second query, so
     * no caller can observe a state between the two and none can recompute the distinction
     * differently. That is what makes the enum the right shape and a `boolean` plus an
     * `isRetired` accessor the wrong one.
     *
     * The admitted case is asserted here too: without it, a gate that refused everything
     * would satisfy both refusal assertions.
     */
    /**
     * The verdicts callers act on, asserted as verdicts.
     *
     * Mutation testing found `enter`, `runIfGranted` and `register` returning constants
     * and killing nothing: the suite drove their state machine thoroughly and then looked
     * only at side effects. That is a gate covering the half it was designed for -- the
     * caller does not see the side effect, it sees the boolean, and
     * `NativeExports.runIfPermissionGranted` hands that boolean straight to a JNI return.
     *
     * Each assertion below is paired with its opposite polarity, because a method pinned
     * only where it returns false is satisfied by one that always returns false.
     */
    @Test
    public void admittedAndRefusedOperationsReportOppositeVerdicts() {
        PermissionOperationGate gate = new PermissionOperationGate();
        assertEquals(Admission.ADMITTED, gate.admit(3401));
        assertNull(gate.update(3401, "scope.camera", true, () -> true).failure());

        List<String> ran = new ArrayList<>();

        PermissionOperationGate.Pending granted = gate.register(3401, "scope.camera");
        assertNotNull("a granted scope must yield a usable lease", granted);
        assertTrue(
                "an admitted framework call reports success",
                gate.enter(granted, () -> ran.add("admitted")));
        assertEquals(Collections.singletonList("admitted"), ran);

        // Same lease, after it is finished: the refusal is the same call reporting the
        // opposite verdict, so neither polarity can be a constant.
        gate.finish(granted);
        assertFalse(
                "a finished lease admits nothing and says so",
                gate.enter(granted, () -> ran.add("after-finish")));
        assertEquals(Collections.singletonList("admitted"), ran);

        assertFalse("a null lease is refused, not dereferenced", gate.enter(null, () -> {
            ran.add("null-lease");
        }));
        assertEquals(Collections.singletonList("admitted"), ran);
    }

    /**
     * `runIfGranted` returns the callback's own verdict, not a verdict of its own.
     *
     * The op behind it reports success or failure to content, so a gate that collapsed
     * this to a constant would either swallow every failure or discard every success.
     * A denied scope is asserted too, and it must not run the callback at all.
     */
    @Test
    public void runIfGrantedForwardsTheCallbacksOwnVerdict() {
        PermissionOperationGate gate = new PermissionOperationGate();
        assertEquals(Admission.ADMITTED, gate.admit(3402));
        assertNull(gate.update(3402, "scope.record", true, () -> true).failure());

        assertTrue(
                "a callback that succeeded is reported as success",
                gate.runIfGranted(3402, "scope.record", () -> true));
        assertFalse(
                "a callback that failed is reported as failure",
                gate.runIfGranted(3402, "scope.record", () -> false));

        List<String> ran = new ArrayList<>();
        assertFalse(
                "an ungranted scope is refused",
                gate.runIfGranted(3402, "scope.userLocation", () -> {
                    ran.add("ungranted");
                    return true;
                }));
        assertTrue("a refused admission must not run its callback", ran.isEmpty());

        assertFalse(
                "an unknown session is refused",
                gate.runIfGranted(9999, "scope.record", () -> true));
    }

    @Test
    public void admissionSaysWhetherARefusedIdIsLiveOrRetired() {
        PermissionOperationGate gate = new PermissionOperationGate();

        assertEquals(Admission.ADMITTED, gate.admit(3101));
        assertEquals(
                "a still-live id is a duplicate registration, not a closed session",
                Admission.ALREADY_LIVE,
                gate.admit(3101));

        assertNull(gate.close(3101).failure());
        assertEquals(
                "a closed id is retired, not merely live elsewhere",
                Admission.RETIRED,
                gate.admit(3101));

        // A neighbour is unaffected by either, which rules out an answer that depends on
        // anything but the id asked about.
        assertEquals(Admission.ADMITTED, gate.admit(3102));
    }

    @Test
    public void retirementIsQueryableWithoutClaimingTheId() {
        // The distinction `NativeExports` needs before retaining a late surface-loss
        // report: "has not registered yet" and "already closed" are both absent from the
        // live session map, and only the first is worth keeping an entry for.
        PermissionOperationGate gate = new PermissionOperationGate();

        assertFalse("an id nobody has opened is not retired", gate.isRetired(3201));
        assertEquals(Admission.ADMITTED, gate.admit(3201));
        assertFalse("a live id is not retired", gate.isRetired(3201));

        assertNull(gate.close(3201).failure());
        assertTrue("a closed id is retired", gate.isRetired(3201));

        // And the query must not have claimed anything on the way: an unopened neighbour
        // is still admissible, and asking about it repeatedly does not change that.
        assertFalse(gate.isRetired(3202));
        assertFalse(gate.isRetired(3202));
        assertEquals(Admission.ADMITTED, gate.admit(3202));
    }

    @Test
    public void perEventAdmissionDoesNotWaitForTheAdmissionGuard() throws Exception {
        PermissionOperationGate gate = new PermissionOperationGate();
        assertEquals(Admission.ADMITTED, gate.admit(31));
        assertNull(gate.update(31, "scope.bluetooth", true, () -> true).failure());

        CountDownLatch held = new CountDownLatch(1);
        CountDownLatch release = new CountDownLatch(1);
        Thread holder = new Thread(() -> {
            synchronized (gate.openGuard) {
                held.countDown();
                try {
                    release.await();
                } catch (InterruptedException e) {
                    Thread.currentThread().interrupt();
                }
            }
        }, "admission-guard-holder");
        holder.start();
        assertTrue("the guard was never taken, so nothing was contended",
                held.await(CONTENTION_PATIENCE_MILLIS, TimeUnit.MILLISECONDS));

        CountDownLatch admitted = new CountDownLatch(1);
        List<Boolean> outcome = Collections.synchronizedList(new ArrayList<>());
        Thread caller = new Thread(() -> {
            outcome.add(gate.runIfGranted(31, "scope.bluetooth", () -> true));
            admitted.countDown();
        }, "per-event-admission");
        caller.start();

        boolean finished = admitted.await(CONTENTION_PATIENCE_MILLIS, TimeUnit.MILLISECONDS);
        release.countDown();
        holder.join();
        caller.join();

        assertTrue(
                "a per-event admission did not complete in " + CONTENTION_PATIENCE_MILLIS
                        + "ms while the admission guard was held, so it acquires a lock"
                        + " shared beyond its own session",
                finished);
        assertEquals(
                "the admission was refused, so its speed says nothing about the lock",
                Arrays.asList(true),
                outcome);
    }

    /**
     * The instrument's own control, and it is not optional.
     *
     * The test above asserts an absence: that a per-event path did *not* wait. That is
     * satisfied by a guard nobody actually held, by a monitor this test failed to acquire,
     * and by a deadline long enough to hide anything. Opening a session is the operation
     * that genuinely takes {@code openGuard}, so requiring it to stay blocked for the same
     * held guard is what says the instrument can observe a wait at all.
     *
     * The bound here is deliberately the short one: a correct {@code open} can never
     * complete while the guard is held, so this observation cannot flake -- only a
     * regression that stopped taking the guard could make it fail.
     */
    @Test
    public void openingASessionDoesWaitForTheAdmissionGuard() throws Exception {
        PermissionOperationGate gate = new PermissionOperationGate();

        CountDownLatch held = new CountDownLatch(1);
        CountDownLatch release = new CountDownLatch(1);
        Thread holder = new Thread(() -> {
            synchronized (gate.openGuard) {
                held.countDown();
                try {
                    release.await();
                } catch (InterruptedException e) {
                    Thread.currentThread().interrupt();
                }
            }
        }, "admission-guard-holder");
        holder.start();
        assertTrue(held.await(CONTENTION_PATIENCE_MILLIS, TimeUnit.MILLISECONDS));

        CountDownLatch opened = new CountDownLatch(1);
        Thread opener = new Thread(() -> {
            gate.admit(41);
            opened.countDown();
        }, "session-open");
        opener.start();

        boolean finishedEarly = opened.await(BLOCKED_OBSERVATION_MILLIS, TimeUnit.MILLISECONDS);
        release.countDown();
        holder.join();
        opener.join();

        assertFalse(
                "opening a session completed while the admission guard was held, so the"
                        + " guard is not the mutual exclusion the per-event gate assumes"
                        + " it holds",
                finishedEarly);
        assertTrue("the opener never finished even after the guard was released",
                opened.await(CONTENTION_PATIENCE_MILLIS, TimeUnit.MILLISECONDS));
    }

    @Test
    public void missingSessionRejectsUpdatesAndRegistrationUntilExplicitlyOpened() {
        PermissionOperationGate gate = new PermissionOperationGate();

        assertNotNull(gate.update(6, "scope.camera", true, () -> true).failure());
        assertNull(gate.register(6, "scope.camera"));

        assertEquals(Admission.ADMITTED, gate.admit(6));
        assertNull(gate.update(6, "scope.camera", true, () -> true).failure());
        assertNotNull(gate.register(6, "scope.camera"));
    }

    @Test
    public void closeWaitsForDeferredFrameworkEntryBeforeCancelling() throws Exception {
        PermissionOperationGate gate = new PermissionOperationGate();
        assertEquals(Admission.ADMITTED, gate.admit(7));
        assertNull(gate.update(7, "scope.userLocation", true, () -> true).failure());
        List<String> events = Collections.synchronizedList(new ArrayList<>());
        PermissionOperationGate.Pending pending = gate.register(
                7, "scope.userLocation", () -> events.add("cancel"));
        assertNotNull(pending);

        CountDownLatch entered = new CountDownLatch(1);
        CountDownLatch release = new CountDownLatch(1);
        Thread framework = new Thread(() -> gate.enter(pending, () -> {
            events.add("enter");
            entered.countDown();
            await(release);
            events.add("return");
        }));
        framework.start();
        assertTrue(entered.await(1, TimeUnit.SECONDS));

        CountDownLatch closeAttempting = new CountDownLatch(1);
        PermissionOperationGate.Result[] closed = {null};
        Thread close = new Thread(() -> {
            closeAttempting.countDown();
            closed[0] = gate.close(7);
        });
        close.start();
        assertTrue(closeAttempting.await(1, TimeUnit.SECONDS));

        release.countDown();
        joinBounded(framework);
        joinBounded(close);

        assertNull(closed[0].failure());
        assertEquals(Arrays.asList("enter", "return", "cancel"), events);
        assertFalse(gate.enter(pending, () -> events.add("late")));
    }

    @Test
    public void updateWaitingBehindCloseCannotReopenTheSession() throws Exception {
        PermissionOperationGate gate = new PermissionOperationGate();
        assertEquals(Admission.ADMITTED, gate.admit(8));
        assertNull(gate.update(8, "scope.userLocation", true, () -> true).failure());
        CountDownLatch cancelling = new CountDownLatch(1);
        CountDownLatch releaseCancellation = new CountDownLatch(1);
        gate.register(8, "scope.userLocation", () -> {
            cancelling.countDown();
            await(releaseCancellation);
        });

        PermissionOperationGate.Result[] closed = {null};
        Thread close = new Thread(() -> closed[0] = gate.close(8));
        close.start();
        assertTrue(cancelling.await(1, TimeUnit.SECONDS));

        CountDownLatch updateAttempting = new CountDownLatch(1);
        PermissionOperationGate.Result[] updated = {null};
        int[] nativeUpdates = {0};
        Thread update = new Thread(() -> {
            updateAttempting.countDown();
            updated[0] = gate.update(8, "scope.camera", true, () -> {
                nativeUpdates[0]++;
                return true;
            });
        });
        update.start();
        assertTrue(updateAttempting.await(1, TimeUnit.SECONDS));

        releaseCancellation.countDown();
        joinBounded(close);
        joinBounded(update);

        assertNull(closed[0].failure());
        assertNotNull(updated[0].failure());
        assertEquals(0, nativeUpdates[0]);
        assertNotEquals(Admission.ADMITTED, gate.admit(8));
        assertNull(gate.register(8, "scope.camera"));
    }

    @Test
    public void failedCloseCancellationIsRetainedAndRetriedAcrossAllScopes() {
        PermissionOperationGate gate = new PermissionOperationGate();
        assertEquals(Admission.ADMITTED, gate.admit(9));
        assertNull(gate.update(9, "scope.userLocation", true, () -> true).failure());
        assertNull(gate.update(9, "scope.camera", true, () -> true).failure());
        int[] locationAttempts = {0};
        int[] cameraAttempts = {0};
        PermissionOperationGate.Pending location = gate.register(
                9,
                "scope.userLocation",
                () -> {
                    locationAttempts[0]++;
                    if (locationAttempts[0] == 1) {
                        throw new IllegalStateException("location cancel failed");
                    }
                });
        gate.register(9, "scope.camera", () -> cameraAttempts[0]++);

        PermissionOperationGate.Result first = gate.close(9);
        assertNotNull(first.failure());
        assertEquals(1, locationAttempts[0]);
        assertEquals(1, cameraAttempts[0]);
        assertFalse(gate.enter(location, () -> {}));

        PermissionOperationGate.Result retry = gate.close(9);
        assertNull(retry.failure());
        assertEquals(2, locationAttempts[0]);
        assertEquals(1, cameraAttempts[0]);
    }

    @Test
    public void nativeGrantFailureLeavesScopeDenied() {
        PermissionOperationGate gate = new PermissionOperationGate();
        assertEquals(Admission.ADMITTED, gate.admit(10));

        PermissionOperationGate.Result result =
                gate.update(10, "scope.camera", true, () -> false);

        assertNotNull(result.failure());
        assertNull(gate.register(10, "scope.camera"));
    }

    @Test
    public void closeLeavesTombstoneAndCannotRetainGrantOrBeReopened() {
        PermissionOperationGate gate = new PermissionOperationGate();
        assertEquals(Admission.ADMITTED, gate.admit(11));
        assertNull(gate.update(11, "scope.camera", true, () -> true).failure());

        assertNull(gate.close(11).failure());

        assertNotEquals(Admission.ADMITTED, gate.admit(11));
        assertNotNull(gate.update(11, "scope.camera", true, () -> true).failure());
        assertNull(gate.register(11, "scope.camera"));
    }

    @Test
    public void duplicateOpenCannotRepublishAnExistingSession() {
        PermissionOperationGate gate = new PermissionOperationGate();

        assertEquals(Admission.ADMITTED, gate.admit(12));
        assertNotEquals(Admission.ADMITTED, gate.admit(12));
    }

    @Test
    public void nativeUpdateDoesNotHoldSessionMonitorWhileWaitingForHostLock() throws Exception {
        PermissionOperationGate gate = new PermissionOperationGate();
        assertEquals(Admission.ADMITTED, gate.admit(13));
        Object hostLock = new Object();
        CountDownLatch nativeUpdateStarted = new CountDownLatch(1);
        CountDownLatch javaEntryStarted = new CountDownLatch(1);
        PermissionOperationGate.Result[] updated = {null};
        PermissionOperationGate.Pending[] registered = {null};

        Thread update = new Thread(() -> updated[0] = gate.update(
                13,
                "scope.camera",
                true,
                () -> {
                    nativeUpdateStarted.countDown();
                    await(javaEntryStarted);
                    synchronized (hostLock) {
                        return true;
                    }
                }));
        update.setDaemon(true);
        update.start();
        assertTrue(nativeUpdateStarted.await(1, TimeUnit.SECONDS));

        Thread protectedCall = new Thread(() -> {
            synchronized (hostLock) {
                javaEntryStarted.countDown();
                registered[0] = gate.register(13, "scope.camera");
            }
        });
        protectedCall.setDaemon(true);
        protectedCall.start();
        assertTrue(javaEntryStarted.await(1, TimeUnit.SECONDS));

        update.join(1000);
        protectedCall.join(1000);

        assertFalse("permission update retained the Java session monitor across native code",
                update.isAlive());
        assertFalse("protected JNI call retained the native host lock while entering Java",
                protectedCall.isAlive());
        assertNotNull(updated[0]);
        assertNull(updated[0].failure());
        assertNull(registered[0]);
        assertNotNull(gate.register(13, "scope.camera"));
    }

    @Test
    public void successfulCloseReclaimsSessionsWithoutAllowingIdReuse() throws Exception {
        PermissionOperationGate gate = new PermissionOperationGate();
        for (int sessionId = 1000; sessionId < 2000; sessionId++) {
            assertEquals(Admission.ADMITTED, gate.admit(sessionId));
            assertNull(gate.close(sessionId).failure());
        }
        assertEquals(0, gate.retainedSessionCountForTests());
        assertNotEquals(Admission.ADMITTED, gate.admit(1500));

        assertEquals(Admission.ADMITTED, gate.admit(2000));
        assertNull(gate.update(2000, "scope.camera", true, () -> true).failure());
        CountDownLatch cancellationStarted = new CountDownLatch(1);
        CountDownLatch releaseCancellation = new CountDownLatch(1);
        gate.register(2000, "scope.camera", () -> {
            cancellationStarted.countDown();
            await(releaseCancellation);
        });
        Thread close = new Thread(() -> gate.close(2000));
        close.start();
        assertTrue(cancellationStarted.await(1, TimeUnit.SECONDS));

        assertNotEquals(Admission.ADMITTED, gate.admit(2000));
        releaseCancellation.countDown();
        joinBounded(close);

        assertEquals(0, gate.retainedSessionCountForTests());
        assertNotEquals(Admission.ADMITTED, gate.admit(2000));
    }

    @Test
    public void aLowerSessionIdOpenedAfterAHigherOneStillGetsItsPermissions() {
        // Session ids are allocated on the caller thread but opened from each session's own
        // thread, so two sessions starting together can arrive here in the opposite order.
        // Neither id was retired, so both must be admitted and both must stay usable.
        PermissionOperationGate gate = new PermissionOperationGate();

        assertEquals("the first session was refused", Admission.ADMITTED, gate.admit(3007));
        assertEquals(
                "a live session was refused because a higher id opened first",
                Admission.ADMITTED, gate.admit(3005));

        assertNull(gate.update(3005, "scope.camera", true, () -> true).failure());
        assertNotNull(
                "a granted scope on a live session surfaced as a denial",
                gate.register(3005, "scope.camera"));
        assertNull(gate.update(3007, "scope.camera", true, () -> true).failure());
        assertNotNull(gate.register(3007, "scope.camera"));
    }

    @Test
    public void aClosedIdStaysRetiredWhenAHigherIdOpensAfterwards() {
        PermissionOperationGate gate = new PermissionOperationGate();
        assertEquals(Admission.ADMITTED, gate.admit(3011));
        assertNull(gate.close(3011).failure());

        // A later, unrelated session must not resurrect the retired id.
        assertEquals(Admission.ADMITTED, gate.admit(3012));

        assertNotEquals("a retired id was reopened", Admission.ADMITTED, gate.admit(3011));
        assertNotNull(gate.update(3011, "scope.camera", true, () -> true).failure());
        assertNull(gate.register(3011, "scope.camera"));
    }

    /**
     * A grant belongs to the Session that was granted it, and to no other. This gate is a
     * process-wide static keyed by session id, so two concurrent Sessions meet inside one
     * object -- and a grant that leaked between them would let one game use a capability
     * the user approved for another. That is the permission half of Section 6.4's
     * concurrent-session isolation, and it was the group task 0.21 recorded as untested.
     *
     * <p>The existing cross-session test grants a scope and then checks the granted
     * session still works. This checks the other direction, which nothing did: that the
     * session which was <em>not</em> granted is refused. Both directions are needed
     * because the first alone passes over a gate that grants everyone.
     *
     * <p>The positive assertion in the middle is that control: a gate that granted nobody
     * would satisfy both denials while breaking every permission in the product.
     */
    @Test
    public void aGrantOnOneSessionLeavesTheSameScopeDeniedOnAnother() {
        PermissionOperationGate gate = new PermissionOperationGate();
        assertEquals(Admission.ADMITTED, gate.admit(4001));
        assertEquals(Admission.ADMITTED, gate.admit(4002));

        assertNull(gate.update(4001, "scope.camera", true, () -> true).failure());

        assertTrue(
                "the session the grant was made for could not use it",
                gate.runIfGranted(4001, "scope.camera", () -> true));

        assertFalse(
                "one session's grant admitted another session's callback",
                gate.runIfGranted(4002, "scope.camera", () -> true));
        assertNull(
                "one session's grant let another session register a cancellation",
                gate.register(4002, "scope.camera"));
    }

    @Test
    public void closingOneSessionLeavesAnotherLiveSessionUntouched() {
        PermissionOperationGate gate = new PermissionOperationGate();
        assertEquals(Admission.ADMITTED, gate.admit(3022));
        assertEquals("a live session was refused because a higher id opened first",
                Admission.ADMITTED, gate.admit(3021));
        assertNull(gate.update(3021, "scope.camera", true, () -> true).failure());

        assertNull(gate.close(3022).failure());

        assertNotNull(
                "closing a sibling session denied a live session",
                gate.register(3021, "scope.camera"));
        assertTrue(gate.runIfGranted(3021, "scope.camera", () -> true));
        // A fresh id below the closed one is still admissible.
        assertEquals(Admission.ADMITTED, gate.admit(3020));
        assertNull(gate.update(3020, "scope.camera", true, () -> true).failure());
        assertEquals(2, gate.retainedSessionCountForTests());
    }

    @Test
    public void replacedCancellationRunsExactlyOnceAndAThrowingOneIsRetained() {
        PermissionOperationGate gate = new PermissionOperationGate();
        assertEquals(Admission.ADMITTED, gate.admit(3030));
        assertNull(gate.update(3030, "scope.userLocation", true, () -> true).failure());
        List<String> events = Collections.synchronizedList(new ArrayList<>());
        int[] replacementRuns = {0};
        PermissionOperationGate.Pending pending = gate.register(
                3030, "scope.userLocation", () -> events.add("original"));
        assertNotNull(pending);

        // The replacement is installed from inside the framework entry, exactly as
        // LocationProvider does once the framework hands back its cancellable request.
        assertTrue(gate.enter(pending, () -> pending.setCancellation(() -> {
            replacementRuns[0]++;
            events.add("replacement");
            if (replacementRuns[0] == 1) {
                throw new IllegalStateException("replacement cancel failed");
            }
        })));

        PermissionOperationGate.Result denied =
                gate.update(3030, "scope.userLocation", false, () -> true);
        assertNotNull("a throwing cancellation was reported as success", denied.failure());
        assertEquals(
                "denial ran a cancellation other than the installed replacement",
                Collections.singletonList("replacement"),
                events);
        assertEquals(1, replacementRuns[0]);
        assertFalse(gate.enter(pending, () -> events.add("late")));

        // The throwing entry is retained, so close retries it and retires it on success.
        assertNull(gate.close(3030).failure());
        assertEquals(
                Arrays.asList("replacement", "replacement"),
                events);
        assertEquals(2, replacementRuns[0]);

        assertNull(gate.close(3030).failure());
        assertEquals(
                "a retired cancellation ran again after succeeding",
                2,
                replacementRuns[0]);
    }

    /**
     * The executed cancellation must be the one published under the session monitor when the
     * pending set was snapshotted. Cancellations run with the monitor released, so a
     * concurrent {@code setCancellation} can otherwise swap the action of a pending that has
     * been snapshotted but not yet run, and the action actually invoked stops being the one
     * the snapshot admitted.
     */
    @Test
    public void aCancellationReplacedWhileAnotherRunsDoesNotSwapTheExecutedAction()
            throws Exception {
        PermissionOperationGate gate = new PermissionOperationGate();
        assertEquals(Admission.ADMITTED, gate.admit(3031));
        assertNull(gate.update(3031, "scope.userLocation", true, () -> true).failure());
        List<String> events = Collections.synchronizedList(new ArrayList<>());
        CountDownLatch firstEntered = new CountDownLatch(1);
        CountDownLatch releaseFirst = new CountDownLatch(1);
        // Snapshotted cancellations run sequentially on the denying thread, so the first one
        // to run can park while the replacement is installed on the one still queued.
        ResourceCleanup.Action original = () -> {
            events.add("original");
            if (firstEntered.getCount() > 0) {
                firstEntered.countDown();
                await(releaseFirst);
            }
        };
        PermissionOperationGate.Pending first =
                gate.register(3031, "scope.userLocation", original);
        PermissionOperationGate.Pending second =
                gate.register(3031, "scope.userLocation", original);
        assertNotNull(first);
        assertNotNull(second);

        PermissionOperationGate.Result[] denied = {null};
        Thread denial = new Thread(
                () -> denied[0] = gate.update(3031, "scope.userLocation", false, () -> true));
        denial.start();
        assertTrue(firstEntered.await(1, TimeUnit.SECONDS));

        first.setCancellation(() -> events.add("replacement"));
        second.setCancellation(() -> events.add("replacement"));
        releaseFirst.countDown();
        joinBounded(denial);

        assertNull(denied[0].failure());
        assertEquals(
                "a cancellation installed after the pending snapshot replaced the action the"
                        + " snapshot admitted",
                Arrays.asList("original", "original"),
                events);
    }

    @Test
    public void denialDrainsAdmittedScopeRunBeforeNativeUpdateAndRejectsLaterRun()
            throws Exception {
        PermissionOperationGate gate = new PermissionOperationGate();
        assertEquals(Admission.ADMITTED, gate.admit(2001));
        assertNull(gate.update(2001, "scope.bluetooth", true, () -> true).failure());
        assertNull(gate.update(2001, "scope.camera", true, () -> true).failure());
        List<String> events = Collections.synchronizedList(new ArrayList<>());
        CountDownLatch callbackEntered = new CountDownLatch(1);
        CountDownLatch releaseCallback = new CountDownLatch(1);
        boolean[] admitted = {false};

        Thread callback = new Thread(() -> admitted[0] = gate.runIfGranted(
                2001,
                "scope.bluetooth",
                () -> {
                    callbackEntered.countDown();
                    await(releaseCallback);
                    events.add("callback");
                    return true;
                }));
        callback.start();
        assertTrue(callbackEntered.await(1, TimeUnit.SECONDS));

        CountDownLatch unrelatedRegistrationFinished = new CountDownLatch(1);
        PermissionOperationGate.Pending[] unrelated = {null};
        Thread registration = new Thread(() -> {
            unrelated[0] = gate.register(2001, "scope.camera");
            unrelatedRegistrationFinished.countDown();
        });
        registration.start();
        assertTrue("external callback work retained the permission-session monitor",
                unrelatedRegistrationFinished.await(1, TimeUnit.SECONDS));
        assertNotNull(unrelated[0]);
        gate.finish(unrelated[0]);

        CountDownLatch nativeUpdateEntered = new CountDownLatch(1);
        CountDownLatch releaseNativeUpdate = new CountDownLatch(1);
        CountDownLatch denialStarted = new CountDownLatch(1);
        PermissionOperationGate.Result[] denied = {null};
        Thread denial = new Thread(() -> {
            denialStarted.countDown();
            denied[0] = gate.update(2001, "scope.bluetooth", false, () -> {
                events.add("native-update");
                nativeUpdateEntered.countDown();
                await(releaseNativeUpdate);
                return true;
            });
        });
        denial.start();
        assertTrue(denialStarted.await(1, TimeUnit.SECONDS));
        assertFalse(
                "denial reached the native update while an admitted scope run held its lease",
                nativeUpdateEntered.await(BLOCKED_OBSERVATION_MILLIS, TimeUnit.MILLISECONDS));

        releaseCallback.countDown();
        joinBounded(callback);
        try {
            assertTrue(nativeUpdateEntered.await(1, TimeUnit.SECONDS));
            assertEquals(Arrays.asList("callback", "native-update"), events);
            assertFalse(gate.runIfGranted(
                    2001, "scope.bluetooth", () -> {
                        events.add("late");
                        return true;
                    }));
        } finally {
            releaseNativeUpdate.countDown();
        }
        joinBounded(denial);
        joinBounded(registration);

        assertTrue(admitted[0]);
        assertNull(denied[0].failure());
        assertEquals(Arrays.asList("callback", "native-update"), events);
    }

    @Test
    public void closeDrainsAdmittedScopeRunBeforeCancellationAndRejectsLaterRun()
            throws Exception {
        PermissionOperationGate gate = new PermissionOperationGate();
        assertEquals(Admission.ADMITTED, gate.admit(2002));
        assertNull(gate.update(2002, "scope.bluetooth", true, () -> true).failure());
        List<String> events = Collections.synchronizedList(new ArrayList<>());
        CountDownLatch cancellationEntered = new CountDownLatch(1);
        assertNotNull(gate.register(2002, "scope.bluetooth", () -> {
            events.add("cancel");
            cancellationEntered.countDown();
        }));
        CountDownLatch callbackEntered = new CountDownLatch(1);
        CountDownLatch releaseCallback = new CountDownLatch(1);
        Thread callback = new Thread(() -> gate.runIfGranted(
                2002,
                "scope.bluetooth",
                () -> {
                    callbackEntered.countDown();
                    await(releaseCallback);
                    events.add("callback");
                    return true;
                }));
        callback.start();
        assertTrue(callbackEntered.await(1, TimeUnit.SECONDS));

        PermissionOperationGate.Result[] closed = {null};
        CountDownLatch closeStarted = new CountDownLatch(1);
        Thread close = new Thread(() -> {
            closeStarted.countDown();
            closed[0] = gate.close(2002);
        });
        close.start();
        assertTrue(closeStarted.await(1, TimeUnit.SECONDS));
        assertFalse(
                "close reached cancellation while an admitted scope run held its lease",
                cancellationEntered.await(BLOCKED_OBSERVATION_MILLIS, TimeUnit.MILLISECONDS));
        releaseCallback.countDown();
        joinBounded(callback);
        assertTrue(cancellationEntered.await(1, TimeUnit.SECONDS));
        joinBounded(close);

        assertNull(closed[0].failure());
        assertEquals(Arrays.asList("callback", "cancel"), events);
        assertFalse(gate.runIfGranted(2002, "scope.bluetooth", () -> true));
        assertEquals(0, gate.retainedSessionCountForTests());
        assertNotEquals(Admission.ADMITTED, gate.admit(2002));
    }

    @Test
    public void callbackFailureReleasesScopeRunLeaseForSubsequentDenial()
            throws Exception {
        PermissionOperationGate gate = new PermissionOperationGate();
        assertEquals(Admission.ADMITTED, gate.admit(2003));
        assertNull(gate.update(2003, "scope.bluetooth", true, () -> true).failure());
        IllegalStateException callbackFailure =
                new IllegalStateException("callback failed");

        try {
            gate.runIfGranted(2003, "scope.bluetooth", () -> {
                throw callbackFailure;
            });
            throw new AssertionError("callback failure was swallowed");
        } catch (IllegalStateException failure) {
            assertTrue(failure == callbackFailure);
        }

        CountDownLatch nativeUpdateEntered = new CountDownLatch(1);
        PermissionOperationGate.Result[] denied = {null};
        Thread denial = new Thread(() -> denied[0] = gate.update(
                2003,
                "scope.bluetooth",
                false,
                () -> {
                    nativeUpdateEntered.countDown();
                    return true;
                }));
        denial.setDaemon(true);
        denial.start();

        assertTrue("callback exception retained its permission lease",
                nativeUpdateEntered.await(1, TimeUnit.SECONDS));
        denial.join(1000);
        assertFalse("denial remained blocked after the callback exception", denial.isAlive());
        assertNotNull(denied[0]);
        assertNull(denied[0].failure());
        assertFalse(gate.runIfGranted(2003, "scope.bluetooth", () -> true));
    }

    @Test
    public void closeCancellationDoesNotRetainTheSessionMonitor() throws Exception {
        PermissionOperationGate gate = new PermissionOperationGate();
        assertEquals(Admission.ADMITTED, gate.admit(2004));
        assertNull(gate.update(2004, "scope.bluetooth", true, () -> true).failure());
        assertNull(gate.update(2004, "scope.camera", true, () -> true).failure());
        CountDownLatch cancellationEntered = new CountDownLatch(1);
        CountDownLatch releaseCancellation = new CountDownLatch(1);
        assertNotNull(gate.register(2004, "scope.bluetooth", () -> {
            cancellationEntered.countDown();
            await(releaseCancellation);
        }));

        Thread close = new Thread(() -> gate.close(2004));
        close.start();
        assertTrue(cancellationEntered.await(1, TimeUnit.SECONDS));

        CountDownLatch observed = new CountDownLatch(1);
        Thread observer = new Thread(() -> {
            gate.register(2004, "scope.camera");
            observed.countDown();
        });
        observer.setDaemon(true);
        observer.start();

        try {
            assertTrue(
                    "a blocked cancellation retained the permission-session monitor",
                    observed.await(1, TimeUnit.SECONDS));
        } finally {
            releaseCancellation.countDown();
        }
        joinBounded(close);
        joinBounded(observer);
    }

    /**
     * Contention on the per-event admission path is a structural property rather than a
     * behavioural one: every holder of the shared session map releases it in nanoseconds,
     * so no functional fixture can tell a shared monitor apart from a concurrent map. This
     * asserts the invariant directly; the sibling test guards the concurrency it enables.
     */
    @Test
    public void perEventSessionLookupTakesNoLockSharedAcrossSessions() throws Exception {
        Field sessions = PermissionOperationGate.class.getDeclaredField("sessions");

        assertTrue(
                "per-event admission must resolve a session without a lock shared across"
                        + " sessions; declared type was " + sessions.getType().getName(),
                ConcurrentMap.class.isAssignableFrom(sessions.getType()));
    }

    @Test
    public void twoSessionsAdmitCallbacksConcurrently() throws Exception {
        PermissionOperationGate gate = new PermissionOperationGate();
        assertEquals(Admission.ADMITTED, gate.admit(2005));
        assertNull(gate.update(2005, "scope.bluetooth", true, () -> true).failure());
        assertEquals(Admission.ADMITTED, gate.admit(2006));
        assertNull(gate.update(2006, "scope.bluetooth", true, () -> true).failure());
        CyclicBarrier bothInside = new CyclicBarrier(2);
        boolean[] admitted = {false, false};

        Thread first = new Thread(() -> admitted[0] = gate.runIfGranted(
                2005, "scope.bluetooth", () -> awaitBarrier(bothInside)));
        Thread second = new Thread(() -> admitted[1] = gate.runIfGranted(
                2006, "scope.bluetooth", () -> awaitBarrier(bothInside)));
        first.setDaemon(true);
        second.setDaemon(true);
        first.start();
        second.start();
        joinBounded(first);
        joinBounded(second);

        assertTrue(admitted[0]);
        assertTrue(admitted[1]);
    }

    private static boolean awaitBarrier(CyclicBarrier barrier) {
        try {
            barrier.await(5, TimeUnit.SECONDS);
            return true;
        } catch (Exception failure) {
            throw new AssertionError(
                    "two sessions could not hold an admitted callback at the same time",
                    failure);
        }
    }

    private static void joinBounded(Thread thread) throws InterruptedException {
        thread.join(TimeUnit.SECONDS.toMillis(10));
        assertFalse("thread did not finish: " + thread.getName(), thread.isAlive());
    }

    private static void await(CountDownLatch latch) {
        try {
            latch.await();
        } catch (InterruptedException interrupted) {
            Thread.currentThread().interrupt();
            throw new AssertionError(interrupted);
        }
    }
}

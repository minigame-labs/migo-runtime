package com.migo.runtime.internal;

import static org.junit.Assert.assertArrayEquals;
import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertNull;
import static org.junit.Assert.assertTrue;

import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.CyclicBarrier;
import java.util.concurrent.atomic.AtomicInteger;
import org.junit.Test;

/**
 * The exactly-once claim, driven rather than argued.
 *
 * <p>This arbitration has been wrong three times — dropped, leaked, then raced — so the
 * property is exercised at both orderings and then under a real race, and both a double
 * delivery and a lost report fail.
 */
public final class PendingSurfaceLossTest {

    @Test
    public void aReportRetainedBeforeRegistrationIsDeliveredByTheDrain() {
        PendingSurfaceLoss pending = new PendingSurfaceLoss();

        assertNull(
                "an unregistered session keeps its report rather than delivering it",
                pending.retainOrTakeBack(7, 3L, 2, id -> false));
        assertArrayEquals(
                "and registration is what takes it",
                new long[] {3L, 2},
                pending.drain(7));
        assertNull("only once", pending.drain(7));
        assertTrue(pending.isEmpty());
    }

    @Test
    public void aReportOvertakenByRegistrationIsHandedBackInstead() {
        PendingSurfaceLoss pending = new PendingSurfaceLoss();

        // Registration already happened, so the drain that would have replayed this found
        // nothing. The recheck is what notices, and it must hand the report back rather
        // than leave an entry nothing will ever look at again.
        assertArrayEquals(
                new long[] {9L, 3},
                pending.retainOrTakeBack(8, 9L, 3, id -> true));
        assertTrue("nothing may be left behind", pending.isEmpty());
        assertNull(pending.drain(8));
    }

    @Test
    public void theFirstReportInTheWindowIsTheOneKept() {
        PendingSurfaceLoss pending = new PendingSurfaceLoss();

        assertNull(pending.retainOrTakeBack(11, 1L, 3, id -> false));
        assertNull(pending.retainOrTakeBack(11, 2L, 2, id -> false));

        assertArrayEquals(
                "a second loss is for a Surface whose predecessor's loss is undelivered",
                new long[] {1L, 3},
                pending.drain(11));
    }

    @Test
    public void aDiscardedReportIsNotDeliveredLater() {
        PendingSurfaceLoss pending = new PendingSurfaceLoss();

        assertNull(pending.retainOrTakeBack(12, 5L, 1, id -> false));
        pending.discard(12);

        assertNull(pending.drain(12));
        assertTrue(pending.isEmpty());
    }

    /**
     * The interleaving the third defect was: registration running between the reporter's
     * lookup and its insert. Both sides are released together, and every iteration must
     * deliver exactly once and leave nothing retained.
     *
     * <p>It does not require a particular winner. Which side wins depends on the
     * scheduler, so an assertion about that would be flaky; both branches are covered
     * deterministically above by driving the {@code registered} predicate. What this adds
     * is that no real interleaving produces two deliveries or none.
     */
    @Test
    public void registrationRacingAReportDeliversItExactlyOnce() throws Exception {
        final int iterations = 2_000;
        AtomicInteger byReporter = new AtomicInteger();
        AtomicInteger byRegistration = new AtomicInteger();

        for (int i = 0; i < iterations; i++) {
            PendingSurfaceLoss pending = new PendingSurfaceLoss();
            // Stands in for `sSessions`: the reporter's `registered` predicate reads it,
            // and registration publishes into it before draining, exactly as
            // `registerSession` puts before it drains.
            ConcurrentHashMap<Integer, Boolean> sessions = new ConcurrentHashMap<>();
            CyclicBarrier start = new CyclicBarrier(2);
            AtomicInteger delivered = new AtomicInteger();
            final int id = i;

            Thread reporter = new Thread(() -> {
                await(start);
                if (sessions.containsKey(id)) {
                    // Registration was already visible; this is the ordinary direct path.
                    delivered.incrementAndGet();
                    byReporter.incrementAndGet();
                    return;
                }
                if (pending.retainOrTakeBack(id, 1L, 3, sessions::containsKey) != null) {
                    delivered.incrementAndGet();
                    byReporter.incrementAndGet();
                }
            });
            Thread registration = new Thread(() -> {
                await(start);
                sessions.put(id, Boolean.TRUE);
                if (pending.drain(id) != null) {
                    delivered.incrementAndGet();
                    byRegistration.incrementAndGet();
                }
            });

            reporter.start();
            registration.start();
            reporter.join();
            registration.join();

            assertEquals(
                    "iteration " + i + " must deliver exactly once (reporter="
                            + byReporter.get() + ", registration=" + byRegistration.get()
                            + ")",
                    1,
                    delivered.get());
            assertTrue("nothing may be left retained", pending.isEmpty());
        }
    }

    private static void await(CyclicBarrier barrier) {
        try {
            barrier.await();
        } catch (Exception e) {
            throw new AssertionError(e);
        }
    }
}

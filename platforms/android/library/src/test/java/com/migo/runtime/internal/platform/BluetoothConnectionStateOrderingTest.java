package com.migo.runtime.internal.platform;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertNotNull;
import static org.junit.Assert.assertTrue;

import java.util.Collections;
import java.util.List;
import java.util.concurrent.CopyOnWriteArrayList;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.TimeUnit;

import org.junit.Test;

/**
 * Task 0.24: a connection-state report must reflect the ownership decision that
 * was current when it is <em>delivered</em>, not when it was decided.
 *
 * <p>The defect these pin is not a lost report, it is a stale one that wins.
 * Deciding and delivering were separate steps, so a retired attempt could read
 * "no owner", be descheduled, and deliver its {@code false} after a replacement
 * had published and reported {@code true}. Content is then told the peripheral
 * is disconnected while it is connected, and stays wrong until some later event
 * corrects it -- there is no retry, because from the runtime's point of view
 * nothing failed.
 */
public final class BluetoothConnectionStateOrderingTest {
    private static final String DEVICE = "AA:BB:CC:DD:EE:FF";

    /**
     * How long to watch for a report that must not arrive yet.
     *
     * <p>Correct ordering can never deliver it inside this window, because the
     * thread that would is blocked on a monitor the test holds through another
     * thread. Only a regression makes the observation fail, so the budget buys
     * confidence rather than flakiness.
     */
    private static final long BLOCKED_OBSERVATION_MILLIS = 200;

    /**
     * The race itself: a retired attempt's disconnect, in flight, must not be
     * overtaken by its replacement's connect.
     *
     * <p>The retired attempt is parked <em>inside</em> its own report -- inside
     * the critical region, not before it -- because a signal raised before the
     * region is entered proves nothing about the region.
     */
    @Test
    public void aReplacementsConnectCannotOvertakeARetiredAttemptsDisconnect() throws Exception {
        CountDownLatch retiredReportEntered = new CountDownLatch(1);
        CountDownLatch releaseRetiredReport = new CountDownLatch(1);
        List<Boolean> delivered = new CopyOnWriteArrayList<>();
        BluetoothManager manager = new BluetoothManager(
                41,
                (operation, failure) -> {},
                (deviceId, connected) -> {
                    delivered.add(connected);
                    if (!connected) {
                        retiredReportEntered.countDown();
                        await(releaseRetiredReport);
                    }
                });

        BluetoothManager.GattAttempt retired = manager.beginGattAttempt(DEVICE);
        assertNotNull(retired);
        // Retire it without reporting: the map no longer holds it, which is the
        // state in which its decision reads "no owner".
        manager.abandonGattAttempt(DEVICE, retired);

        Thread late = new Thread(() -> manager.handleGattConnectionStateChange(
                DEVICE, retired, new FakeGattConnection(), true), "late-retired-callback");
        late.start();
        assertTrue(
                "the retired attempt never reached its report",
                retiredReportEntered.await(2, TimeUnit.SECONDS));

        BluetoothManager.GattAttempt replacement = manager.beginGattAttempt(DEVICE);
        assertNotNull(replacement);
        FakeGattConnection replacementConnection = new FakeGattConnection();
        Thread fresh = new Thread(() -> manager.handleGattConnectionStateChange(
                DEVICE, replacement, replacementConnection, true), "replacement-callback");
        fresh.start();

        awaitBlockedOn(fresh);
        assertEquals(
                "the replacement's connect was delivered while a disconnect was in flight",
                Collections.singletonList(Boolean.FALSE), delivered);

        releaseRetiredReport.countDown();
        joinBounded(late);
        joinBounded(fresh);

        assertEquals(
                "reports must arrive in the order they were decided",
                java.util.Arrays.asList(Boolean.FALSE, Boolean.TRUE), delivered);
        assertTrue(manager.hasGattConnection(DEVICE, replacementConnection));
    }

    /**
     * The other direction, which the ledger's description of this race did not
     * name: a superseded attempt's <em>connect</em> must not resurrect a device
     * whose teardown already completed.
     *
     * <p>Single-threaded and deterministic: the retirement happens inside
     * service discovery, which is exactly the window a slow {@code
     * discoverServices} leaves open on a device.
     */
    @Test
    public void aSupersededAttemptsConnectDoesNotResurrectATornDownDevice() {
        List<Boolean> delivered = new CopyOnWriteArrayList<>();
        BluetoothManager manager = new BluetoothManager(
                42,
                (operation, failure) -> {},
                (deviceId, connected) -> delivered.add(connected));

        BluetoothManager.GattAttempt attempt = manager.beginGattAttempt(DEVICE);
        assertNotNull(attempt);
        FakeGattConnection connection = new FakeGattConnection();
        // Discovery succeeds, but the device is torn down while it runs.
        connection.duringDiscovery = () -> manager.abandonGattAttempt(DEVICE, attempt);

        manager.handleGattConnectionStateChange(DEVICE, attempt, connection, true);

        assertEquals(
                "an attempt that no longer owns the device may not report it connected",
                Collections.emptyList(), delivered);
        assertFalse(manager.hasGattConnection(DEVICE, connection));
    }

    /**
     * The decision and the delivery must be one step, not two under one lock.
     *
     * <p>The first test in this file leaves a gap, and mutation found it: moving
     * only the <em>report</em> inside the monitor and leaving the ownership
     * re-check outside still orders the two reports, so that test passes -- while
     * the stale decision it is supposed to catch survives intact. What
     * discriminates is holding the monitor from the test itself, so the retired
     * attempt is stopped between reading the map and delivering: it wakes to a
     * world where a replacement exists, and must notice.
     */
    @Test
    public void aDecisionMadeBeforeAReplacementIsNotDeliveredAfterIt() throws Exception {
        List<Boolean> delivered = new CopyOnWriteArrayList<>();
        BluetoothManager manager = new BluetoothManager(
                44,
                (operation, failure) -> {},
                (deviceId, connected) -> delivered.add(connected));

        BluetoothManager.GattAttempt retired = manager.beginGattAttempt(DEVICE);
        assertNotNull(retired);
        manager.abandonGattAttempt(DEVICE, retired);

        Thread late;
        synchronized (manager.connectionStateOrderForTests()) {
            late = new Thread(() -> manager.handleGattConnectionStateChange(
                    DEVICE, retired, new FakeGattConnection(), true), "late-retired-callback");
            late.start();
            // Parked on the monitor. An implementation that decides before
            // taking it has already read "no owner" by now.
            awaitBlockedOn(late);
            // The replacement appears while the retired attempt is parked.
            assertNotNull(manager.beginGattAttempt(DEVICE));
        }
        joinBounded(late);

        assertEquals(
                "a disconnect decided before the replacement existed was delivered after it",
                Collections.emptyList(), delivered);
    }

    /**
     * The instrument control. Without it the first test could pass by never
     * delivering anything at all, and the second by a manager that reports
     * nothing ever.
     */
    @Test
    public void anUncontestedTransitionStillReportsBothStates() {
        List<Boolean> delivered = new CopyOnWriteArrayList<>();
        BluetoothManager manager = new BluetoothManager(
                43,
                (operation, failure) -> {},
                (deviceId, connected) -> delivered.add(connected));

        BluetoothManager.GattAttempt attempt = manager.beginGattAttempt(DEVICE);
        FakeGattConnection connection = new FakeGattConnection();
        manager.handleGattConnectionStateChange(DEVICE, attempt, connection, true);
        manager.handleGattConnectionStateChange(DEVICE, attempt, connection, false);

        assertEquals(java.util.Arrays.asList(Boolean.TRUE, Boolean.FALSE), delivered);
    }

    /**
     * Wait until {@code thread} is actually parked on a monitor.
     *
     * <p>Polling for the state rather than relying on a plausible fixed interval:
     * the short sleeps only avoid a busy loop, while the observed state determines
     * when the test proceeds.
     */
    private static void awaitBlockedOn(Thread thread) throws InterruptedException {
        long deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(2);
        while (System.nanoTime() < deadline) {
            if (thread.getState() == Thread.State.BLOCKED) {
                // Held long enough to be sure it is not passing through.
                Thread.sleep(BLOCKED_OBSERVATION_MILLIS);
                return;
            }
            Thread.sleep(1);
        }
        throw new AssertionError(
                "the replacement never blocked on the report in flight; it is in "
                        + thread.getState());
    }

    private static void joinBounded(Thread thread) throws InterruptedException {
        thread.join(TimeUnit.SECONDS.toMillis(5));
        assertFalse("thread " + thread.getName() + " did not finish", thread.isAlive());
    }

    private static void await(CountDownLatch latch) {
        try {
            if (!latch.await(5, TimeUnit.SECONDS)) throw new AssertionError("latch timed out");
        } catch (InterruptedException interrupted) {
            Thread.currentThread().interrupt();
            throw new AssertionError(interrupted);
        }
    }

    private static final class FakeGattConnection implements BluetoothManager.GattConnection {
        Runnable duringDiscovery;

        @Override public android.bluetooth.BluetoothGatt raw() {
            return null;
        }

        @Override public boolean discoverServices() {
            if (duringDiscovery != null) duringDiscovery.run();
            return true;
        }

        @Override public void disconnect() {}

        @Override public void close() {}
    }
}

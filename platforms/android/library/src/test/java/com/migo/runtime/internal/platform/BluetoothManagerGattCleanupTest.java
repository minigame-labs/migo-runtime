package com.migo.runtime.internal.platform;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertNotNull;
import static org.junit.Assert.assertTrue;

import com.migo.runtime.internal.PermissionOperationGate;

import java.util.ArrayList;
import java.util.Arrays;
import java.util.Collections;
import java.util.List;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.TimeUnit;
import com.migo.runtime.internal.PermissionOperationGate.Admission;
import org.junit.Test;

public final class BluetoothManagerGattCleanupTest {
    /**
     * Budget for asserting that teardown stays blocked. Correct linearization can never
     * release it, so only a regression can make this observation fail.
     */
    private static final long BLOCKED_OBSERVATION_MILLIS = 200;

    @Test
    public void discoverFailureRetainsGattWhenCloseFailsThenExplicitCloseRemovesIt() {
        List<RuntimeException> failures = new ArrayList<>();
        BluetoothManager manager = new BluetoothManager(
                7,
                (operation, failure) -> failures.add(failure),
                (deviceId, connected) -> {});
        FakeGattConnection connection = new FakeGattConnection();
        connection.discoverResult = false;
        connection.failCloseOnce = true;
        BluetoothManager.GattAttempt attempt = manager.beginGattAttempt("AA:BB");

        manager.handleGattConnectionStateChange("AA:BB", attempt, connection, true);

        assertTrue(manager.hasGattConnection("AA:BB", connection));
        assertEquals(1, connection.closeAttempts);
        assertEquals(1, failures.size());

        manager.closeGattConnection("AA:BB");

        assertFalse(manager.hasGattConnection("AA:BB", connection));
        assertEquals(2, connection.closeAttempts);
    }

    @Test
    public void disconnectedCallbackRetainsGattWhenCloseFailsThenExplicitCloseRetries() {
        List<String> states = new ArrayList<>();
        List<RuntimeException> failures = new ArrayList<>();
        BluetoothManager manager = new BluetoothManager(
                8,
                (operation, failure) -> failures.add(failure),
                (deviceId, connected) -> states.add(deviceId + ":" + connected));
        FakeGattConnection connection = new FakeGattConnection();
        connection.failCloseOnce = true;
        BluetoothManager.GattAttempt attempt = manager.beginGattAttempt("CC:DD");

        manager.handleGattConnectionStateChange("CC:DD", attempt, connection, false);

        assertTrue(manager.hasGattConnection("CC:DD", connection));
        assertEquals(1, connection.closeAttempts);
        assertEquals(1, failures.size());

        manager.closeGattConnection("CC:DD");

        assertFalse(manager.hasGattConnection("CC:DD", connection));
        assertEquals(2, connection.closeAttempts);
        assertEquals("CC:DD:false", states.get(states.size() - 1));
    }

    @Test
    public void lateConnectedCallbackAfterRevocationIsClosedAndNotRepublished() {
        List<String> states = new ArrayList<>();
        BluetoothManager manager = new BluetoothManager(
                9,
                (operation, failure) -> {},
                (deviceId, connected) -> states.add(deviceId + ":" + connected));
        FakeGattConnection connection = new FakeGattConnection();
        BluetoothManager.GattAttempt attempt = manager.beginGattAttempt("EE:FF");

        manager.handleGattConnectionStateChange("EE:FF", attempt, connection, true);
        manager.closeGattConnection("EE:FF");
        manager.handleGattConnectionStateChange("EE:FF", attempt, connection, true);

        assertFalse(manager.hasGattConnection("EE:FF", connection));
        assertEquals(2, connection.closeAttempts);
        assertEquals("EE:FF:false", states.get(states.size() - 1));
    }

    @Test
    public void inFlightConnectionResultAfterRevocationIsClosedInsteadOfPublished()
            throws Exception {
        BluetoothManager manager = new BluetoothManager(
                10,
                (operation, failure) -> {},
                (deviceId, connected) -> {});
        BluetoothManager.GattAttempt attempt = manager.beginGattAttempt("11:22");
        FakeGattConnection connection = new FakeGattConnection();
        CountDownLatch connectReturned = new CountDownLatch(1);
        CountDownLatch releaseResult = new CountDownLatch(1);
        boolean[] published = {true};

        Thread connect = new Thread(() -> {
            connectReturned.countDown();
            await(releaseResult);
            published[0] = manager.publishGattConnection("11:22", attempt, connection);
        });
        connect.start();
        assertTrue(connectReturned.await(1, TimeUnit.SECONDS));

        manager.closeGattConnection("11:22");
        releaseResult.countDown();
        joinBounded(connect);

        assertFalse(published[0]);
        assertFalse(manager.hasGattConnection("11:22", connection));
        assertEquals(1, connection.closeAttempts);
    }

    @Test
    public void failedCreationAbandonsAttemptAndRejectsItsLateCallback() {
        BluetoothManager manager = new BluetoothManager(
                11,
                (operation, failure) -> {},
                (deviceId, connected) -> {});
        BluetoothManager.GattAttempt failed = manager.beginGattAttempt("33:44");

        manager.abandonGattAttempt("33:44", failed);

        assertNotNull(manager.beginGattAttempt("33:44"));
        FakeGattConnection lateConnection = new FakeGattConnection();
        assertFalse(manager.publishGattConnection("33:44", failed, lateConnection));
        assertEquals(1, lateConnection.closeAttempts);
    }

    @Test
    public void distinctCandidateWithoutRawHandleIsRejectedAndClosed() {
        BluetoothManager manager = new BluetoothManager(
                12,
                (operation, failure) -> {},
                (deviceId, connected) -> {});
        BluetoothManager.GattAttempt attempt = manager.beginGattAttempt("55:66");
        FakeGattConnection first = new FakeGattConnection();
        FakeGattConnection second = new FakeGattConnection();

        assertTrue(manager.publishGattConnection("55:66", attempt, first));
        assertFalse(manager.publishGattConnection("55:66", attempt, second));

        assertTrue(manager.hasGattConnection("55:66", first));
        assertEquals(1, second.closeAttempts);
    }

    /**
     * A late candidate is never mapped -- the map holds the winning attempt -- so a
     * failed close has no map entry to keep it alive the way {@code closeAndRemoveGatt}
     * keeps a failed owned close. Without explicit retention the {@code BluetoothGatt}
     * is simply dropped: the OS handle stays open for process life and nothing will
     * ever try again.
     */
    @Test
    public void rejectedCandidateWhoseCloseFailsIsRetainedAndRetriedByTheNextClose() {
        List<String> failures = new ArrayList<>();
        BluetoothManager manager = new BluetoothManager(
                13,
                (operation, failure) -> failures.add(operation),
                (deviceId, connected) -> {});
        BluetoothManager.GattAttempt attempt = manager.beginGattAttempt("77:88");
        FakeGattConnection owner = new FakeGattConnection();
        FakeGattConnection candidate = new FakeGattConnection();
        candidate.failCloseOnce = true;

        assertTrue(manager.publishGattConnection("77:88", attempt, owner));
        assertFalse(manager.publishGattConnection("77:88", attempt, candidate));
        assertEquals(1, candidate.closeAttempts);
        assertEquals(1, failures.size());
        assertEquals(1, manager.unclosedCandidateCountForTests());

        manager.closeGattConnection("77:88");

        assertEquals("the retained candidate must be retried", 2, candidate.closeAttempts);
        assertEquals(0, manager.unclosedCandidateCountForTests());
        assertEquals(1, owner.closeAttempts);
        assertFalse(manager.hasGattConnection("77:88", owner));
    }

    /** A retry that fails again keeps the handle rather than dropping it. */
    @Test
    public void rejectedCandidateStaysRetainedWhileItsCloseKeepsFailing() {
        BluetoothManager manager = new BluetoothManager(
                14,
                (operation, failure) -> {},
                (deviceId, connected) -> {});
        BluetoothManager.GattAttempt attempt = manager.beginGattAttempt("99:aa");
        FakeGattConnection owner = new FakeGattConnection();
        FakeGattConnection candidate = new FakeGattConnection();
        candidate.failCloseAlways = true;

        assertTrue(manager.publishGattConnection("99:aa", attempt, owner));
        assertFalse(manager.publishGattConnection("99:aa", attempt, candidate));
        assertEquals(1, manager.unclosedCandidateCountForTests());

        manager.closeGattConnection("99:aa");
        assertEquals(2, candidate.closeAttempts);
        assertEquals(
                "a still-failing candidate must stay retained",
                1,
                manager.unclosedCandidateCountForTests());

        candidate.failCloseAlways = false;
        manager.closeGattConnection("99:aa");
        assertEquals(3, candidate.closeAttempts);
        assertEquals(0, manager.unclosedCandidateCountForTests());
    }

    @Test
    public void staleAttemptSensitiveCallbacksAreRejectedAndCurrentAttemptIsDelivered() {
        List<String> characteristicEvents = new ArrayList<>();
        List<String> mtuEvents = new ArrayList<>();
        boolean[] connectPermissionGranted = {true};
        BluetoothManager manager = new BluetoothManager(
                13,
                (operation, failure) -> {},
                (deviceId, connected) -> {},
                () -> connectPermissionGranted[0],
                () -> false,
                new BluetoothManager.GattEventReporter() {
                    @Override public void characteristic(
                            String deviceId, String serviceId, String characteristicId,
                            byte[] value) {
                        characteristicEvents.add(
                                deviceId + ":" + serviceId + ":" + characteristicId);
                    }

                    @Override public void mtu(String deviceId, int mtu) {
                        mtuEvents.add(deviceId + ":" + mtu);
                    }
                });
        BluetoothManager.GattAttempt oldAttempt = manager.beginGattAttempt("77:88");
        FakeGattConnection oldConnection = new FakeGattConnection();
        assertTrue(manager.publishGattConnection("77:88", oldAttempt, oldConnection));
        manager.closeGattConnection("77:88");
        BluetoothManager.GattAttempt currentAttempt = manager.beginGattAttempt("77:88");
        FakeGattConnection currentConnection = new FakeGattConnection();
        assertTrue(manager.publishGattConnection("77:88", currentAttempt, currentConnection));

        assertFalse(manager.handleGattCharacteristicRead(
                "77:88", oldAttempt, oldConnection, "old-service", "old-read", new byte[]{1}));
        assertFalse(manager.handleGattCharacteristicChanged(
                "77:88", oldAttempt, oldConnection, "old-service", "old-change", new byte[]{2}));
        assertFalse(manager.handleGattMtuChanged(
                "77:88", oldAttempt, oldConnection, 99));
        assertFalse(manager.handleGattRssiChanged(
                "77:88", oldAttempt, oldConnection, -99));
        assertEquals(0, characteristicEvents.size());
        assertEquals(0, mtuEvents.size());
        assertEquals(null, manager.cachedMtuForTests("77:88"));
        assertEquals(null, manager.cachedRssiForTests("77:88"));

        assertTrue(manager.handleGattCharacteristicRead(
                "77:88", currentAttempt, currentConnection,
                "new-service", "new-read", new byte[]{3}));
        assertTrue(manager.handleGattCharacteristicChanged(
                "77:88", currentAttempt, currentConnection,
                "new-service", "new-change", new byte[]{4}));
        assertTrue(manager.handleGattMtuChanged(
                "77:88", currentAttempt, currentConnection, 128));
        assertTrue(manager.handleGattRssiChanged(
                "77:88", currentAttempt, currentConnection, -42));
        assertEquals(2, characteristicEvents.size());
        assertEquals(1, mtuEvents.size());
        assertEquals(Integer.valueOf(128), manager.cachedMtuForTests("77:88"));
        assertEquals(Integer.valueOf(-42), manager.cachedRssiForTests("77:88"));

        connectPermissionGranted[0] = false;
        assertFalse(manager.handleGattMtuChanged(
                "77:88", currentAttempt, currentConnection, 256));
        assertEquals(Integer.valueOf(128), manager.cachedMtuForTests("77:88"));
        assertEquals(1, mtuEvents.size());
    }

    @Test
    public void standingScopeDenialDrainsAdmittedCallbackBeforeClosingGattAndRejectsLateData()
            throws Exception {
        PermissionOperationGate gate = new PermissionOperationGate();
        assertEquals(Admission.ADMITTED, gate.admit(14));
        assertEquals(null, gate.update(14, "scope.bluetooth", true, () -> true).failure());
        List<String> events = Collections.synchronizedList(new ArrayList<>());
        CountDownLatch callbackEntered = new CountDownLatch(1);
        CountDownLatch releaseCallback = new CountDownLatch(1);
        BluetoothManager manager = new BluetoothManager(
                14,
                (operation, failure) -> {},
                (deviceId, connected) -> {},
                callback -> gate.runIfGranted(14, "scope.bluetooth", callback),
                () -> true,
                () -> false,
                new BluetoothManager.GattEventReporter() {
                    @Override public void characteristic(
                            String deviceId, String serviceId, String characteristicId,
                            byte[] value) {
                        callbackEntered.countDown();
                        await(releaseCallback);
                        events.add("callback");
                    }

                    @Override public void mtu(String deviceId, int mtu) {
                        events.add("mtu");
                    }
                });
        BluetoothManager.GattAttempt attempt = manager.beginGattAttempt("99:AA");
        FakeGattConnection connection = new FakeGattConnection();
        assertTrue(manager.publishGattConnection("99:AA", attempt, connection));
        boolean[] delivered = {false};
        Thread callback = new Thread(() -> delivered[0] = manager.handleGattCharacteristicChanged(
                "99:AA", attempt, connection, "service", "characteristic", new byte[]{1}));
        callback.start();
        assertTrue(callbackEntered.await(1, TimeUnit.SECONDS));

        CountDownLatch falseStateEntered = new CountDownLatch(1);
        CountDownLatch allowTeardown = new CountDownLatch(1);
        CountDownLatch denialStarted = new CountDownLatch(1);
        PermissionOperationGate.Result[] denied = {null};
        Thread denial = new Thread(() -> {
            denialStarted.countDown();
            denied[0] = gate.update(14, "scope.bluetooth", false, () -> {
                events.add("false");
                falseStateEntered.countDown();
                await(allowTeardown);
                manager.closeGattConnection("99:AA");
                events.add("teardown");
                return true;
            });
        });
        denial.setDaemon(true);
        denial.start();
        assertTrue(denialStarted.await(1, TimeUnit.SECONDS));
        assertFalse(
                "standing-scope denial published false state while a callback lease was held",
                falseStateEntered.await(BLOCKED_OBSERVATION_MILLIS, TimeUnit.MILLISECONDS));

        releaseCallback.countDown();
        joinBounded(callback);
        try {
            assertTrue(falseStateEntered.await(1, TimeUnit.SECONDS));
            assertTrue(delivered[0]);
            assertEquals(Arrays.asList("callback", "false"), events);
            assertTrue(manager.hasGattConnection("99:AA", connection));
            assertEquals(0, connection.closeAttempts);

            assertFalse(manager.handleGattCharacteristicRead(
                    "99:AA", attempt, connection, "service", "late-read", new byte[]{2}));
            assertFalse(manager.handleGattCharacteristicChanged(
                    "99:AA", attempt, connection, "service", "late-change", new byte[]{3}));
            assertFalse(manager.handleGattMtuChanged(
                    "99:AA", attempt, connection, 256));
            assertFalse(manager.handleGattRssiChanged(
                    "99:AA", attempt, connection, -64));
            assertEquals(Arrays.asList("callback", "false"), events);
            assertEquals(null, manager.cachedMtuForTests("99:AA"));
            assertEquals(null, manager.cachedRssiForTests("99:AA"));
        } finally {
            allowTeardown.countDown();
            denial.join(1000);
        }

        assertFalse("standing permission denial did not finish", denial.isAlive());
        assertEquals(null, denied[0].failure());
        assertEquals(Arrays.asList("callback", "false", "teardown"), events);
        assertFalse(manager.hasGattConnection("99:AA", connection));
    }

    @Test
    public void sensitiveAdmissionDoesNotHoldGattAttemptWhileAcquiringPermissionLease()
            throws Exception {
        BluetoothManager[] manager = {null};
        CountDownLatch closeFinished = new CountDownLatch(1);
        manager[0] = new BluetoothManager(
                15,
                (operation, failure) -> {},
                (deviceId, connected) -> {},
                callback -> {
                    Thread close = new Thread(() -> {
                        manager[0].closeGattConnection("BB:CC");
                        closeFinished.countDown();
                    });
                    close.start();
                    try {
                        assertTrue("permission admission ran while retaining GattAttempt",
                                closeFinished.await(1, TimeUnit.SECONDS));
                        close.join();
                    } catch (InterruptedException interrupted) {
                        Thread.currentThread().interrupt();
                        throw new AssertionError(interrupted);
                    }
                    return callback.getAsBoolean();
                },
                () -> true,
                () -> false,
                new BluetoothManager.GattEventReporter() {
                    @Override public void characteristic(
                            String deviceId, String serviceId, String characteristicId,
                            byte[] value) {}

                    @Override public void mtu(String deviceId, int mtu) {}
                });
        BluetoothManager.GattAttempt attempt = manager[0].beginGattAttempt("BB:CC");
        FakeGattConnection connection = new FakeGattConnection();
        assertTrue(manager[0].publishGattConnection("BB:CC", attempt, connection));

        assertFalse(manager[0].handleGattMtuChanged(
                "BB:CC", attempt, connection, 256));

        assertEquals(1, connection.closeAttempts);
        assertEquals(null, manager[0].cachedMtuForTests("BB:CC"));
    }

    @Test
    public void androidConnectPermissionDenialRejectsEverySensitiveGattCallback() {
        List<String> events = new ArrayList<>();
        BluetoothManager manager = new BluetoothManager(
                16,
                (operation, failure) -> {},
                (deviceId, connected) -> {},
                () -> false,
                () -> false,
                recordingGattReporter(events));

        assertEverySensitiveGattCallbackRejected(manager, "CC:DD", events);
    }

    @Test
    public void terminalSessionRejectsEverySensitiveGattCallback() {
        List<String> events = new ArrayList<>();
        BluetoothManager manager = new BluetoothManager(
                17,
                (operation, failure) -> {},
                (deviceId, connected) -> {},
                () -> true,
                () -> true,
                recordingGattReporter(events));

        assertEverySensitiveGattCallbackRejected(manager, "DD:EE", events);
    }

    @Test
    public void connectedCallbackDoesNoFrameworkWorkOrTrueReportWhenCloseWinsAdmission()
            throws Exception {
        CountDownLatch admissionEntered = new CountDownLatch(1);
        CountDownLatch allowAdmission = new CountDownLatch(1);
        List<String> states = new ArrayList<>();
        BluetoothManager manager = new BluetoothManager(
                18,
                (operation, failure) -> {},
                (deviceId, connected) -> states.add(deviceId + ":" + connected),
                callback -> {
                    admissionEntered.countDown();
                    await(allowAdmission);
                    return callback.getAsBoolean();
                },
                () -> true,
                () -> false,
                recordingGattReporter(new ArrayList<>()));
        BluetoothManager.GattAttempt attempt = manager.beginGattAttempt("EE:FF");
        FakeGattConnection connection = new FakeGattConnection();
        assertTrue(manager.publishGattConnection("EE:FF", attempt, connection));
        Thread callback = new Thread(() -> manager.handleGattConnectionStateChange(
                "EE:FF", attempt, connection, true));
        callback.setDaemon(true);
        callback.start();

        boolean admissionObserved = admissionEntered.await(1, TimeUnit.SECONDS);
        if (admissionObserved) manager.closeGattConnection("EE:FF");
        allowAdmission.countDown();
        callback.join(1000);

        assertTrue("connected callback bypassed standing-scope admission", admissionObserved);
        assertFalse("connected callback remained blocked after admission resumed", callback.isAlive());
        assertEquals(0, connection.discoverAttempts);
        assertEquals(1, connection.closeAttempts);
        assertEquals(Arrays.asList("EE:FF:false"), states);
    }

    @Test
    public void connectedCallbackReportsBeforeConcurrentCloseWhenItOwnsAttemptAdmission()
            throws Exception {
        List<String> events = Collections.synchronizedList(new ArrayList<>());
        FakeGattConnection connection = new FakeGattConnection();
        connection.discoverEntered = new CountDownLatch(1);
        connection.releaseDiscovery = new CountDownLatch(1);
        connection.lifecycleEvents = events;
        BluetoothManager manager = new BluetoothManager(
                19,
                (operation, failure) -> {},
                (deviceId, connected) -> {
                    events.add("state:" + connected);
                    connection.dispatchInFlight = false;
                },
                () -> true,
                () -> false,
                recordingGattReporter(new ArrayList<>()));
        BluetoothManager.GattAttempt attempt = manager.beginGattAttempt("FF:00");
        assertTrue(manager.publishGattConnection("FF:00", attempt, connection));
        Thread callback = new Thread(() -> manager.handleGattConnectionStateChange(
                "FF:00", attempt, connection, true));
        callback.start();
        assertTrue(connection.discoverEntered.await(1, TimeUnit.SECONDS));

        CountDownLatch closeStarted = new CountDownLatch(1);
        CountDownLatch closeReturned = new CountDownLatch(1);
        Thread close = new Thread(() -> {
            closeStarted.countDown();
            manager.closeGattConnection("FF:00");
            closeReturned.countDown();
        });
        close.start();
        assertTrue(closeStarted.await(1, TimeUnit.SECONDS));
        assertFalse(
                "GATT teardown overtook a callback that owned attempt admission",
                closeReturned.await(BLOCKED_OBSERVATION_MILLIS, TimeUnit.MILLISECONDS));

        connection.releaseDiscovery.countDown();
        assertTrue(closeReturned.await(1, TimeUnit.SECONDS));
        callback.join(1000);
        close.join(1000);

        assertFalse("connected callback did not finish", callback.isAlive());
        assertFalse("concurrent GATT close did not finish", close.isAlive());
        assertFalse(
                "GATT was closed while the admitted connection dispatch was in flight",
                connection.closedDuringDispatch);
        // The close content asked for is reported too, after the connect it followed.
        assertEquals(Arrays.asList("discover", "state:true", "close", "state:false"), events);
        assertFalse(manager.hasGattConnection("FF:00", connection));
    }

    @Test
    public void standingScopeDenialRejectsConnectedDiscoveryWhileGattIsStillLive() {
        PermissionOperationGate gate = new PermissionOperationGate();
        assertEquals(Admission.ADMITTED, gate.admit(20));
        assertEquals(null, gate.update(20, "scope.bluetooth", true, () -> true).failure());
        assertEquals(null, gate.update(20, "scope.bluetooth", false, () -> true).failure());
        List<String> states = new ArrayList<>();
        BluetoothManager manager = new BluetoothManager(
                20,
                (operation, failure) -> {},
                (deviceId, connected) -> states.add(deviceId + ":" + connected),
                callback -> gate.runIfGranted(20, "scope.bluetooth", callback),
                () -> true,
                () -> false,
                recordingGattReporter(new ArrayList<>()));
        BluetoothManager.GattAttempt attempt = manager.beginGattAttempt("00:11");
        FakeGattConnection connection = new FakeGattConnection();
        assertTrue(manager.publishGattConnection("00:11", attempt, connection));

        manager.handleGattConnectionStateChange("00:11", attempt, connection, true);

        assertTrue(manager.hasGattConnection("00:11", connection));
        assertEquals(0, connection.discoverAttempts);
        assertEquals(Arrays.asList("00:11:false"), states);
    }

    @Test
    public void connectedCallbackAfterRetainedCloseFailureDoesNoFrameworkWorkAndKeepsRetryOwnership()
            throws Exception {
        CountDownLatch admissionEntered = new CountDownLatch(1);
        CountDownLatch allowAdmission = new CountDownLatch(1);
        List<String> states = Collections.synchronizedList(new ArrayList<>());
        BluetoothManager manager = new BluetoothManager(
                21,
                (operation, failure) -> {},
                (deviceId, connected) -> states.add(deviceId + ":" + connected),
                callback -> {
                    admissionEntered.countDown();
                    await(allowAdmission);
                    return callback.getAsBoolean();
                },
                () -> true,
                () -> false,
                recordingGattReporter(new ArrayList<>()));
        BluetoothManager.GattAttempt attempt = manager.beginGattAttempt("EE:11");
        FakeGattConnection connection = new FakeGattConnection();
        connection.failCloseOnce = true;
        assertTrue(manager.publishGattConnection("EE:11", attempt, connection));
        Thread callback = new Thread(() -> manager.handleGattConnectionStateChange(
                "EE:11", attempt, connection, true));
        callback.setDaemon(true);
        callback.start();
        assertTrue(admissionEntered.await(1, TimeUnit.SECONDS));

        try {
            manager.closeGattConnection("EE:11");
            throw new AssertionError("retained close failure was not reported");
        } catch (IllegalStateException expected) {
            assertEquals("gatt close failed", expected.getMessage());
        }
        allowAdmission.countDown();
        callback.join(1000);

        assertFalse("connected callback remained blocked after admission resumed",
                callback.isAlive());
        assertEquals(0, connection.discoverAttempts);
        assertEquals(1, connection.closeAttempts);
        assertEquals(Arrays.asList("EE:11:false"), states);
        assertTrue("failed close discarded the retryable GATT handle",
                manager.hasGattConnection("EE:11", connection));

        manager.closeGattConnection("EE:11");

        assertFalse(manager.hasGattConnection("EE:11", connection));
        assertEquals(2, connection.closeAttempts);
        assertEquals(0, connection.discoverAttempts);
    }

    @Test
    public void supersededAttemptDoesNotOverwriteReplacementConnectionState() {
        List<String> states = new ArrayList<>();
        BluetoothManager manager = new BluetoothManager(
                22,
                (operation, failure) -> {},
                (deviceId, connected) -> states.add(deviceId + ":" + connected),
                () -> true,
                () -> false,
                recordingGattReporter(new ArrayList<>()));
        BluetoothManager.GattAttempt superseded = manager.beginGattAttempt("AB:CD");
        FakeGattConnection supersededConnection = new FakeGattConnection();
        assertTrue(manager.publishGattConnection("AB:CD", superseded, supersededConnection));
        manager.closeGattConnection("AB:CD");
        BluetoothManager.GattAttempt current = manager.beginGattAttempt("AB:CD");
        FakeGattConnection currentConnection = new FakeGattConnection();
        assertTrue(manager.publishGattConnection("AB:CD", current, currentConnection));

        // The superseded attempt's own close, which content asked for.
        assertEquals(Arrays.asList("AB:CD:false"), states);

        manager.handleGattConnectionStateChange("AB:CD", current, currentConnection, true);
        assertEquals(Arrays.asList("AB:CD:false", "AB:CD:true"), states);

        manager.handleGattConnectionStateChange(
                "AB:CD", superseded, supersededConnection, true);

        assertEquals(Arrays.asList("AB:CD:false", "AB:CD:true"), states);
        assertTrue(manager.hasGattConnection("AB:CD", currentConnection));
        assertEquals(1, currentConnection.discoverAttempts);
        assertEquals(0, currentConnection.closeAttempts);
        assertEquals(2, supersededConnection.closeAttempts);
        assertEquals(0, supersededConnection.discoverAttempts);
    }

    @Test
    public void disconnectFailureWithSuccessfulCloseTransfersOwnershipWithoutDoubleClose() {
        List<String> failures = new ArrayList<>();
        List<String> operations = new ArrayList<>();
        BluetoothManager manager = new BluetoothManager(
                23,
                (operation, failure) -> {
                    operations.add(operation);
                    failures.add(failure.getMessage());
                },
                (deviceId, connected) -> {});
        BluetoothManager.GattAttempt attempt = manager.beginGattAttempt("CD:EF");
        FakeGattConnection connection = new FakeGattConnection();
        connection.failDisconnectOnce = true;
        assertTrue(manager.publishGattConnection("CD:EF", attempt, connection));

        manager.closeGattConnection("CD:EF");

        assertEquals(1, connection.disconnectAttempts);
        assertEquals(1, connection.closeAttempts);
        assertFalse("a closed GATT stayed mapped after a disconnect-only failure",
                manager.hasGattConnection("CD:EF", connection));
        assertEquals(Arrays.asList("BLE disconnect"), operations);
        assertEquals(Arrays.asList("gatt disconnect failed"), failures);

        manager.closeGattConnection("CD:EF");

        assertEquals(1, connection.closeAttempts);
    }

    private static BluetoothManager.GattEventReporter recordingGattReporter(
            List<String> events) {
        return new BluetoothManager.GattEventReporter() {
            @Override public void characteristic(
                    String deviceId, String serviceId, String characteristicId,
                    byte[] value) {
                events.add("characteristic");
            }

            @Override public void mtu(String deviceId, int mtu) {
                events.add("mtu");
            }
        };
    }

    private static void assertEverySensitiveGattCallbackRejected(
            BluetoothManager manager,
            String deviceId,
            List<String> events) {
        BluetoothManager.GattAttempt attempt = manager.beginGattAttempt(deviceId);
        FakeGattConnection connection = new FakeGattConnection();
        assertTrue(manager.publishGattConnection(deviceId, attempt, connection));

        assertFalse(manager.handleGattCharacteristicRead(
                deviceId, attempt, connection, "service", "read", new byte[]{1}));
        assertFalse(manager.handleGattCharacteristicChanged(
                deviceId, attempt, connection, "service", "change", new byte[]{2}));
        assertFalse(manager.handleGattMtuChanged(deviceId, attempt, connection, 256));
        assertFalse(manager.handleGattRssiChanged(deviceId, attempt, connection, -64));
        assertTrue(events.isEmpty());
        assertEquals(null, manager.cachedMtuForTests(deviceId));
        assertEquals(null, manager.cachedRssiForTests(deviceId));
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

    private static final class FakeGattConnection implements BluetoothManager.GattConnection {
        boolean discoverResult = true;
        boolean failCloseOnce;
        boolean failCloseAlways;
        boolean failDisconnectOnce;
        int discoverAttempts;
        int disconnectAttempts;
        int closeAttempts;
        CountDownLatch discoverEntered;
        CountDownLatch releaseDiscovery;
        List<String> lifecycleEvents;
        volatile boolean dispatchInFlight;
        volatile boolean closedDuringDispatch;

        @Override public android.bluetooth.BluetoothGatt raw() {
            return null;
        }

        @Override public boolean discoverServices() {
            discoverAttempts++;
            dispatchInFlight = true;
            if (discoverEntered != null) discoverEntered.countDown();
            if (releaseDiscovery != null) await(releaseDiscovery);
            if (lifecycleEvents != null) lifecycleEvents.add("discover");
            return discoverResult;
        }

        @Override public void disconnect() {
            disconnectAttempts++;
            if (failDisconnectOnce) {
                failDisconnectOnce = false;
                throw new IllegalStateException("gatt disconnect failed");
            }
        }

        @Override public void close() {
            closeAttempts++;
            if (dispatchInFlight) closedDuringDispatch = true;
            if (lifecycleEvents != null) lifecycleEvents.add("close");
            if (failCloseAlways) {
                throw new IllegalStateException("gatt close failed");
            }
            if (failCloseOnce) {
                failCloseOnce = false;
                throw new IllegalStateException("gatt close failed");
            }
        }
    }
}

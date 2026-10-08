package com.migo.runtime.internal.platform;

import static org.junit.Assert.assertArrayEquals;
import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertNotNull;
import static org.junit.Assert.assertSame;
import static org.junit.Assert.assertTrue;

import com.migo.runtime.internal.AllocationProbe;

import java.util.ArrayList;
import java.util.List;
import java.util.UUID;

import org.junit.Test;

/**
 * Per-event locking and allocation behavior for Bluetooth notifications.
 *
 * <p>Each callback must avoid a lock shared across sessions and avoid allocating
 * a connection wrapper or capturing lambdas.
 *
 * <p>What these gates cover, said plainly rather than implied: the dispatch from
 * {@code handleGattCharacteristicChanged} inward, which is where both lambdas
 * were, and {@code uuidText}, which is where the two formatted identifiers were.
 * The {@code BluetoothGattCallback} body that calls them takes a
 * {@code BluetoothGatt} and a {@code BluetoothGattCharacteristic}, framework
 * classes a plain JVM test cannot obtain, so the third allocation it removed --
 * one connection wrapper per callback -- is covered by
 * {@code connectionWrapperIsBuiltOncePerConnection} in shape and by nothing
 * on a device. That gap is real and named; it is not gated.
 */
public final class BluetoothNotificationAllocationTest {
    private static final String DEVICE = "1A:2B:3C:4D:5E:6F";
    private static final UUID SERVICE = UUID.fromString("0000180d-0000-1000-8000-00805f9b34fb");
    private static final UUID CHARACTERISTIC =
            UUID.fromString("00002a37-0000-1000-8000-00805f9b34fb");

    /**
     * The gate. A notification stream must reach the delivery boundary without
     * the JVM allocating anything.
     *
     * <p>The reporter counts rather than records, because a recorder would
     * allocate inside the measured window and the burst would be measuring the
     * test.
     */
    @Test
    public void aNotificationStreamNeverAllocates() {
        int[] delivered = {0};
        BluetoothManager manager = new BluetoothManager(
                31,
                (operation, failure) -> {},
                (deviceId, connected) -> {},
                () -> true,
                () -> false,
                new CountingReporter(delivered));
        FakeGattConnection connection = new FakeGattConnection();
        BluetoothManager.GattAttempt attempt = manager.beginGattAttempt(DEVICE);
        assertTrue(manager.publishGattConnection(DEVICE, attempt, connection));
        String serviceId = manager.uuidText(SERVICE);
        String characteristicId = manager.uuidText(CHARACTERISTIC);
        byte[] value = {0x5a, 0x00, 0x11};

        AllocationProbe.assertNoSteadyStateAllocation(
                "BluetoothManager.handleGattCharacteristicChanged", 8, 64,
                () -> manager.handleGattCharacteristicChanged(
                        DEVICE, attempt, connection, serviceId, characteristicId, value));

        assertEquals(72, delivered[0]);
    }

    /** The identifier formatting the notification path used to redo every time. */
    @Test
    public void aRepeatedUuidIsFormattedOnce() {
        BluetoothManager manager = new BluetoothManager(
                32,
                (operation, failure) -> {},
                (deviceId, connected) -> {});

        AllocationProbe.assertNoSteadyStateAllocation(
                "BluetoothManager.uuidText", 4, 64, () -> {
                    String text = manager.uuidText(CHARACTERISTIC);
                    if (text == null) throw new AssertionError("no text");
                });

        assertEquals(CHARACTERISTIC.toString(), manager.uuidText(CHARACTERISTIC));
        // Identity, not equality: a cache that re-formatted would still be equal.
        assertSame(manager.uuidText(SERVICE), manager.uuidText(SERVICE));
    }

    /**
     * The cache is fed by identifiers a remote peripheral chooses, so it is
     * bounded. Past the bound the text is still correct -- just not kept.
     */
    @Test
    public void anAbsurdNumberOfUuidsDoesNotGrowWithoutBound() {
        BluetoothManager manager = new BluetoothManager(
                33,
                (operation, failure) -> {},
                (deviceId, connected) -> {});
        List<UUID> flood = new ArrayList<>();
        for (int i = 0; i < 300; i++) {
            flood.add(new UUID(0x0000180dL, i));
        }

        for (UUID uuid : flood) {
            assertEquals(uuid.toString(), manager.uuidText(uuid));
        }

        UUID beyond = flood.get(flood.size() - 1);
        assertEquals(beyond.toString(), manager.uuidText(beyond));
        assertTrue(manager.uuidTextCacheSizeForTests() <= 256);
    }

    /**
     * Delivery must be unchanged by the carrier: the same three identifiers and
     * the same bytes reach the reporter, and a second notification through the
     * same carrier carries none of the first.
     */
    @Test
    public void reusingTheCarrierDeliversEachNotificationExactlyOnce() {
        List<String> seen = new ArrayList<>();
        List<byte[]> values = new ArrayList<>();
        BluetoothManager manager = new BluetoothManager(
                34,
                (operation, failure) -> {},
                (deviceId, connected) -> {},
                () -> true,
                () -> false,
                new BluetoothManager.GattEventReporter() {
                    @Override public void characteristic(
                            String deviceId,
                            String serviceId,
                            String characteristicId,
                            byte[] value) {
                        seen.add(deviceId + "|" + serviceId + "|" + characteristicId);
                        values.add(value);
                    }

                    @Override public void mtu(String deviceId, int mtu) {}
                });
        FakeGattConnection connection = new FakeGattConnection();
        BluetoothManager.GattAttempt attempt = manager.beginGattAttempt(DEVICE);
        assertTrue(manager.publishGattConnection(DEVICE, attempt, connection));

        assertTrue(manager.handleGattCharacteristicChanged(
                DEVICE, attempt, connection, "svc-a", "chr-a", new byte[] {1}));
        assertTrue(manager.handleGattCharacteristicChanged(
                DEVICE, attempt, connection, "svc-b", "chr-b", new byte[] {2, 3}));

        assertEquals(2, seen.size());
        assertEquals(DEVICE + "|svc-a|chr-a", seen.get(0));
        assertEquals(DEVICE + "|svc-b|chr-b", seen.get(1));
        assertArrayEquals(new byte[] {1}, values.get(0));
        assertArrayEquals(new byte[] {2, 3}, values.get(1));
    }

    /**
     * A delivered notification must not leave the connection reachable through
     * the carrier, which outlives it.
     */
    @Test
    public void aDeliveredNotificationLeavesNothingInTheCarrier() {
        BluetoothManager manager = new BluetoothManager(
                35,
                (operation, failure) -> {},
                (deviceId, connected) -> {});
        FakeGattConnection connection = new FakeGattConnection();
        BluetoothManager.GattAttempt attempt = manager.beginGattAttempt(DEVICE);
        assertTrue(manager.publishGattConnection(DEVICE, attempt, connection));

        assertTrue(manager.handleGattCharacteristicChanged(
                DEVICE, attempt, connection, "svc", "chr", new byte[] {9}));

        assertNotNull(attempt.connection());
        assertTrue(attempt.carrierIsEmptyForTests());
    }

    /**
     * A refused notification must clear the carrier too: the admission gate
     * returning false is the common case for a revoked session, and a carrier
     * left loaded would pin the connection just as firmly.
     */
    @Test
    public void aRefusedNotificationAlsoLeavesNothingInTheCarrier() {
        BluetoothManager manager = new BluetoothManager(
                36,
                (operation, failure) -> {},
                (deviceId, connected) -> {},
                () -> true,
                () -> true, // the session is terminated, so delivery is refused
                new BluetoothManager.GattEventReporter() {
                    @Override public void characteristic(
                            String deviceId,
                            String serviceId,
                            String characteristicId,
                            byte[] value) {
                        throw new AssertionError("a terminated session delivers nothing");
                    }

                    @Override public void mtu(String deviceId, int mtu) {}
                });
        FakeGattConnection connection = new FakeGattConnection();
        BluetoothManager.GattAttempt attempt = manager.beginGattAttempt(DEVICE);
        assertTrue(manager.publishGattConnection(DEVICE, attempt, connection));

        assertTrue(!manager.handleGattCharacteristicChanged(
                DEVICE, attempt, connection, "svc", "chr", new byte[] {9}));

        assertTrue(attempt.carrierIsEmptyForTests());
    }

    /** Counts deliveries without allocating, so the burst measures the path. */
    private static final class CountingReporter implements BluetoothManager.GattEventReporter {
        private final int[] delivered;

        CountingReporter(int[] delivered) {
            this.delivered = delivered;
        }

        @Override public void characteristic(
                String deviceId, String serviceId, String characteristicId, byte[] value) {
            delivered[0]++;
        }

        @Override public void mtu(String deviceId, int mtu) {}
    }

    private static final class FakeGattConnection implements BluetoothManager.GattConnection {
        @Override public android.bluetooth.BluetoothGatt raw() {
            return null;
        }

        @Override public boolean discoverServices() {
            return true;
        }

        @Override public void disconnect() {}

        @Override public void close() {}
    }
}

package com.migo.runtime.internal.platform;

import static org.junit.Assert.assertEquals;

import java.util.ArrayList;
import java.util.List;

import org.json.JSONObject;
import org.junit.Test;

/**
 * A scan session must not retain an unbounded number of remote beacon records.
 *
 * <p>The beacon callback receives advertisements from a remote environment. Before
 * the bound, every distinct beacon was retained forever until scanning stopped,
 * so a long-running session could grow its cache and rebuild an increasingly
 * large JSON array on every update.
 */
public final class BluetoothBeaconCacheBoundTest {
    @Test
    public void beaconCacheStopsRetainingNewRemoteEntriesAtItsBound() throws Exception {
        List<String> updates = new ArrayList<>();
        BluetoothManager manager = new BluetoothManager(
                61,
                (operation, failure) -> {},
                (deviceId, connected) -> {},
                callback -> callback.getAsBoolean(),
                () -> true,
                () -> false,
                new BluetoothManager.GattEventReporter() {
                    @Override public void characteristic(
                            String deviceId,
                            String serviceId,
                            String characteristicId,
                            byte[] value) {}

                    @Override public void mtu(String deviceId, int mtu) {}
                },
                updates::add);

        for (int i = 0; i < BluetoothManager.BEACON_CACHE_LIMIT + 1; i++) {
            manager.recordBeaconForTests("beacon-" + i, new JSONObject().put("id", i));
        }
        assertEquals(BluetoothManager.BEACON_CACHE_LIMIT,
                manager.discoveredBeaconCountForTests());
        assertEquals("an over-limit beacon is not republished",
                BluetoothManager.BEACON_CACHE_LIMIT, updates.size());
    }
}

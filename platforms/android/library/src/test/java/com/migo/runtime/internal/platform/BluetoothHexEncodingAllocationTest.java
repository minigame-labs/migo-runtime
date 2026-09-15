package com.migo.runtime.internal.platform;

import static org.junit.Assert.assertEquals;

import com.migo.runtime.internal.AllocationProbe;

import org.junit.Test;

public final class BluetoothHexEncodingAllocationTest {
    @Test
    public void preallocatedHexEncodingDoesNotAllocatePerAdvertisement() {
        byte[] source = new byte[31];
        char[] output = new char[source.length * 2];
        AllocationProbe.assertNoSteadyStateAllocation(
                "BluetoothManager.appendHex", 8, 64,
                () -> BluetoothManager.appendHex(source, 0, source.length, output, 0));
        assertEquals("00000000000000000000000000000000000000000000000000000000000000",
                new String(output));
    }

    @Test
    public void uuidEncodingReadsTheAdvertisementWithoutCopyingItsUuid() {
        byte[] source = new byte[32];
        char[] output = new char[36];
        BluetoothManager.appendUuid(source, 4, output);
        assertEquals("00000000-0000-0000-0000-000000000000", new String(output));
    }
}

package com.migo.runtime;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertNull;

import com.migo.runtime.internal.StatsProtocol;

import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import org.junit.Test;

/**
 * Host-JVM tests for the append-only native performance packet.
 *
 * <p>Written against {@link StatsProtocol} rather than against literals. The offsets used
 * to be spelled out here as well as in the reader, which made this test agree with a
 * misread field instead of catching it — the numbers matched because they were copied
 * from the same place, not because either matched the engine.
 * {@code scripts/test-stats-packet-offset-contract.sh} is what ties them to the writer.
 */
public final class PerformanceSnapshotTest {
    private static byte[] packet(int version, int length) {
        byte[] data = new byte[length];
        ByteBuffer buffer = ByteBuffer.wrap(data).order(ByteOrder.LITTLE_ENDIAN);
        buffer.putShort(0, (short) StatsProtocol.MAGIC);
        buffer.putShort(2, (short) version);
        return data;
    }

    @Test
    public void v6ParsesInputTransportTailWithoutMovingLegacyFields() {
        byte[] data = packet(StatsProtocol.VERSION, StatsProtocol.BYTE_LEN);
        ByteBuffer buffer = ByteBuffer.wrap(data).order(ByteOrder.LITTLE_ENDIAN);
        buffer.putInt(StatsProtocol.OFFSET_FPS_X10, 599);
        buffer.putInt(StatsProtocol.OFFSET_FRAME_TIME_US, 16_700);
        buffer.putInt(StatsProtocol.OFFSET_DROPPED_FRAMES, 2);
        buffer.putInt(StatsProtocol.OFFSET_FIRST_FRAME_MS, 345);
        buffer.putInt(StatsProtocol.OFFSET_COMMAND_DROPS, 4);
        buffer.putInt(StatsProtocol.OFFSET_INPUT_COALESCED, 101);
        buffer.putInt(StatsProtocol.OFFSET_INPUT_RELIABLE_RESERVE_USES, 7);
        buffer.putInt(StatsProtocol.OFFSET_INPUT_SATURATION_EVENTS, 3);

        PerformanceSnapshot snapshot = PerformanceSnapshot.fromStatsPacket(data);

        assertEquals(59.9f, snapshot.fps, 0.0f);
        assertEquals(16.7f, snapshot.frameTimeMs, 0.0f);
        assertEquals(2, snapshot.droppedFrames);
        assertEquals(345, snapshot.firstFrameMs);
        assertEquals(4, snapshot.commandDrops);
        assertEquals(101, snapshot.inputCoalesced);
        assertEquals(7, snapshot.inputReliableReserveUses);
        assertEquals(3, snapshot.inputSaturationEvents);
    }

    /**
     * A neighbouring field must not be read as this one. Each offset is written on its
     * own with every other byte left zero, so an off-by-one-field reader reports 0 for
     * what it was asked and the mistake is a failure rather than a plausible number.
     */
    @Test
    public void eachFieldIsReadFromItsOwnOffset() {
        int[] offsets = {
            StatsProtocol.OFFSET_DROPPED_FRAMES,
            StatsProtocol.OFFSET_FIRST_FRAME_MS,
            StatsProtocol.OFFSET_COMMAND_DROPS,
            StatsProtocol.OFFSET_INPUT_COALESCED,
            StatsProtocol.OFFSET_INPUT_RELIABLE_RESERVE_USES,
            StatsProtocol.OFFSET_INPUT_SATURATION_EVENTS,
        };
        int[] observed = new int[offsets.length];
        for (int i = 0; i < offsets.length; i++) {
            byte[] data = packet(StatsProtocol.VERSION, StatsProtocol.BYTE_LEN);
            ByteBuffer buffer = ByteBuffer.wrap(data).order(ByteOrder.LITTLE_ENDIAN);
            buffer.putInt(offsets[i], 0x5A5A);
            PerformanceSnapshot snapshot = PerformanceSnapshot.fromStatsPacket(data);
            observed[0] = snapshot.droppedFrames;
            observed[1] = snapshot.firstFrameMs;
            observed[2] = snapshot.commandDrops;
            observed[3] = snapshot.inputCoalesced;
            observed[4] = snapshot.inputReliableReserveUses;
            observed[5] = snapshot.inputSaturationEvents;
            for (int j = 0; j < observed.length; j++) {
                assertEquals(
                        "offset " + offsets[i] + " read into field " + j,
                        i == j ? 0x5A5A : 0,
                        observed[j]);
            }
        }
    }

    @Test
    public void v5DefaultsAbsentInputTransportTailToZero() {
        PerformanceSnapshot snapshot =
                PerformanceSnapshot.fromStatsPacket(
                        packet(5, StatsProtocol.OFFSET_INPUT_COALESCED));

        assertEquals(0, snapshot.inputCoalesced);
        assertEquals(0, snapshot.inputReliableReserveUses);
        assertEquals(0, snapshot.inputSaturationEvents);
    }

    @Test
    public void malformedPacketIsRejected() {
        assertNull(PerformanceSnapshot.fromStatsPacket(null));
        assertNull(
                PerformanceSnapshot.fromStatsPacket(
                        new byte[StatsProtocol.OFFSET_DROPPED_FRAMES + 3]));
        assertNull(
                PerformanceSnapshot.fromStatsPacket(
                        packet(StatsProtocol.VERSION, StatsProtocol.OFFSET_DROPPED_FRAMES + 4)));

        byte[] badMagic = packet(StatsProtocol.VERSION, StatsProtocol.BYTE_LEN);
        badMagic[0] = 0;
        assertNull(PerformanceSnapshot.fromStatsPacket(badMagic));
    }
}

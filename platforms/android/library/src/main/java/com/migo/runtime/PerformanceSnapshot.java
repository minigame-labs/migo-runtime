package com.migo.runtime;

import com.migo.runtime.internal.StatsProtocol;

import java.nio.ByteBuffer;
import java.nio.ByteOrder;

/**
 * Immutable snapshot of engine performance metrics.
 * Obtain via {@link GameSession#getPerformanceSnapshot()}.
 */
public final class PerformanceSnapshot {
    /** Current frames per second (0 if not rendering). */
    public final float fps;
    /** Last frame render time in milliseconds. */
    public final float frameTimeMs;
    /** Cumulative dropped frame count since session start. */
    public final int droppedFrames;
    /** Milliseconds from session start to first rendered frame (0 if not yet rendered). */
    public final int firstFrameMs;
    /** Cumulative host command drops due to queue overflow. */
    public final int commandDrops;
    /** Cumulative input updates safely coalesced in the host queue. */
    public final int inputCoalesced;
    /** Cumulative reliable transitions accepted through reserved capacity. */
    public final int inputReliableReserveUses;
    /** Cumulative input events refused because every eligible lane was full. */
    public final int inputSaturationEvents;

    public PerformanceSnapshot(float fps, float frameTimeMs, int droppedFrames,
                               int firstFrameMs, int commandDrops) {
        this(fps, frameTimeMs, droppedFrames, firstFrameMs, commandDrops, 0, 0, 0);
    }

    public PerformanceSnapshot(float fps, float frameTimeMs, int droppedFrames,
                               int firstFrameMs, int commandDrops, int inputCoalesced,
                               int inputReliableReserveUses, int inputSaturationEvents) {
        this.fps = fps;
        this.frameTimeMs = frameTimeMs;
        this.droppedFrames = droppedFrames;
        this.firstFrameMs = firstFrameMs;
        this.commandDrops = commandDrops;
        this.inputCoalesced = inputCoalesced;
        this.inputReliableReserveUses = inputReliableReserveUses;
        this.inputSaturationEvents = inputSaturationEvents;
    }

    static PerformanceSnapshot fromStatsPacket(byte[] data) {
        if (data == null || !StatsProtocol.has(data, StatsProtocol.OFFSET_DROPPED_FRAMES)) {
            return null;
        }
        ByteBuffer buffer = ByteBuffer.wrap(data).order(ByteOrder.LITTLE_ENDIAN);
        if ((buffer.getShort(0) & 0xFFFF) != StatsProtocol.MAGIC) return null;

        int version = buffer.getShort(2) & 0xFFFF;
        if (version >= StatsProtocol.VERSION && data.length < StatsProtocol.BYTE_LEN) return null;

        int fpsX10 = buffer.getInt(StatsProtocol.OFFSET_FPS_X10);
        int frameTimeUs = buffer.getInt(StatsProtocol.OFFSET_FRAME_TIME_US);
        int dropped = buffer.getInt(StatsProtocol.OFFSET_DROPPED_FRAMES);
        int firstFrameMs = read(buffer, data, StatsProtocol.OFFSET_FIRST_FRAME_MS);
        int commandDrops = read(buffer, data, StatsProtocol.OFFSET_COMMAND_DROPS);
        int inputCoalesced = read(buffer, data, StatsProtocol.OFFSET_INPUT_COALESCED);
        int inputReliableReserveUses =
                read(buffer, data, StatsProtocol.OFFSET_INPUT_RELIABLE_RESERVE_USES);
        int inputSaturationEvents =
                read(buffer, data, StatsProtocol.OFFSET_INPUT_SATURATION_EVENTS);

        return new PerformanceSnapshot(
                (fpsX10 & 0xFFFFFFFFL) / 10f,
                (frameTimeUs & 0xFFFFFFFFL) / 1000f,
                dropped,
                firstFrameMs,
                commandDrops,
                inputCoalesced,
                inputReliableReserveUses,
                inputSaturationEvents);
    }

    private static int read(ByteBuffer buffer, byte[] data, int offset) {
        return StatsProtocol.has(data, offset) ? buffer.getInt(offset) : 0;
    }

    @Override
    public String toString() {
        return String.format(
                "PerformanceSnapshot{fps=%.1f, frameTime=%.1fms, dropped=%d, "
                        + "firstFrame=%dms, inputSaturation=%d}",
                fps, frameTimeMs, droppedFrames, firstFrameMs, inputSaturationEvents);
    }
}

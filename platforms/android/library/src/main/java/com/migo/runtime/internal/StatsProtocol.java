package com.migo.runtime.internal;

/**
 * Absolute byte offsets of the native stats packet, and the header that identifies it.
 * <p>
 * The engine's {@code RenderMetricsSnapshot::as_le_bytes} in
 * {@code engine/crates/shared/src/stats.rs} is the authority; this is the reader's copy
 * of it, and {@code scripts/test-stats-packet-offset-contract.sh} fails the build if the
 * two disagree. Every constant here is named for the Rust field it addresses so that
 * check is mechanical rather than a judgement call.
 * <p>
 * A copy exists at all because the packet crosses a language boundary as raw
 * little-endian bytes: {@code NativeBridge.getDebugStats} hands back a {@code byte[]} and
 * nothing on this side can be generated from the Rust struct at build time. Before this
 * class, the same numbers were written out three times — in
 * {@link com.migo.runtime.PerformanceSnapshot}, in the debug overlay, and again in the
 * overlay's test — where a version that reordered a field would have been read as the
 * wrong counter with every one of them still green.
 * <p>
 * Offsets are absolute, not payload-relative. The readers used to add
 * {@code HEADER_LEN} to a payload offset at each call site, which meant two spellings of
 * every position and no way to compare either against the writer, whose
 * {@code bytes[132..136]} is absolute.
 */
public final class StatsProtocol {

    /** Magic bytes 'M' 'G', little-endian, at [0..2). */
    public static final int MAGIC = 0x4D47;

    /** Format version at [2..4). Appending fields bumps it. */
    public static final int VERSION = 6;

    /** 2 bytes of magic plus 2 of version. */
    public static final int HEADER_LEN = 4;

    /** Header plus the v6 payload. A shorter packet is from an older engine. */
    public static final int BYTE_LEN = 144;

    public static final int OFFSET_FPS_X10 = 4;
    public static final int OFFSET_FRAME_TIME_US = 8;
    public static final int OFFSET_DROPPED_FRAMES = 12;
    public static final int OFFSET_FATAL_ERROR_CODE = 16;
    public static final int OFFSET_FIRST_FRAME_MS = 20;
    public static final int OFFSET_COMMAND_DROPS = 24;
    public static final int OFFSET_RAF_LATENCY_US = 28;
    public static final int OFFSET_SWAP_BLOCK_US = 32;
    public static final int OFFSET_UPLOAD_QUEUE_DEPTH = 36;
    public static final int OFFSET_GLYPH_ATLAS_MISS = 40;
    public static final int OFFSET_PARTIAL_DAMAGE_FRAMES = 44;
    public static final int OFFSET_FULL_SURFACE_FRAMES = 48;
    public static final int OFFSET_DAMAGE_AREA_K_PIXELS = 52;
    public static final int OFFSET_UPLOAD_FRAME_REJECTIONS = 56;
    public static final int OFFSET_DROPPED_UPLOAD_RECOVERIES = 60;
    public static final int OFFSET_RENDER_QUEUE_LEN = 96;
    public static final int OFFSET_COLLECTOR_PENDING_BYTES = 100;
    public static final int OFFSET_WEBGL_ERROR_OVERFLOW = 104;
    public static final int OFFSET_SK_IMAGE_WRAPPERS = 108;
    public static final int OFFSET_DEFERRED_UPLOADS = 112;
    public static final int OFFSET_CANVAS2D_SNAPSHOTS_TAKEN = 116;
    public static final int OFFSET_CANVAS2D_SNAPSHOT_FALLBACKS = 120;
    public static final int OFFSET_CANVAS2D_SNAPSHOT_UPLOADS = 124;
    public static final int OFFSET_CANVAS2D_SNAPSHOT_FORCED_READBACKS = 128;
    public static final int OFFSET_INPUT_COALESCED = 132;
    public static final int OFFSET_INPUT_RELIABLE_RESERVE_USES = 136;
    public static final int OFFSET_INPUT_SATURATION_EVENTS = 140;

    private StatsProtocol() {}

    /**
     * Whether {@code data} is long enough to hold the four bytes at {@code offset}.
     * <p>
     * The packet is append-only, so a field added in a later version is simply absent
     * from an older engine's packet rather than a malformed one. Each read is guarded on
     * its own end rather than on the version, because the length is what is actually in
     * hand — a session can be talking to a `.so` whose version this build has never
     * heard of.
     */
    public static boolean has(byte[] data, int offset) {
        return data.length >= offset + 4;
    }
}

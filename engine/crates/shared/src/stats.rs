use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use parking_lot::RwLock;

pub struct RenderMetricsSnapshot {
    pub fps_x10: u32,
    pub frame_time_us: u32,
    pub dropped_frames: u32,
    pub fatal_error_code: u32,
    pub first_frame_ms: u32,
    pub command_drops: u32,
    pub raf_latency_us: u32,
    pub swap_block_us: u32,
    pub upload_queue_depth: u32,
    pub glyph_atlas_miss: u32,
    // ---- render optimization metrics (appended at tail) ----
    pub partial_damage_frames: u32,
    pub full_surface_frames: u32,
    pub damage_area_k_pixels: u32,
    pub upload_frame_rejections: u32,
    pub dropped_upload_recoveries: u32,
    // ---- IO subsystem metrics (v3, M5.1) ----
    pub decoder_fallback_count: u32,
    pub derived_cache_hits: u32,
    pub derived_cache_misses: u32,
    pub inline_image_policy_rejects: u32,
    pub ws_policy_rejects: u32,
    pub slow_io_count_100ms: u32,
    pub image_cache_admissions_rejected: u32,
    pub image_cache_trim_bytes: u32,
    // ---- render queue / collector / cache observability (v4) ----
    pub render_queue_len: u32,
    pub collector_pending_bytes: u32,
    /// Retired, always 0: WebGL errors are held one per code and cannot overflow.
    pub webgl_error_overflow: u32,
    pub sk_image_wrappers: u32,
    pub deferred_uploads: u32,
    // ---- Canvas2D zero-readback fast path (v5) ----
    pub canvas2d_snapshots_taken: u32,
    pub canvas2d_snapshot_fallbacks: u32,
    pub canvas2d_snapshot_uploads: u32,
    pub canvas2d_snapshot_forced_readbacks: u32,
    // ---- bounded input transport observability (v6) ----
    pub input_coalesced: u32,
    pub input_reliable_reserve_uses: u32,
    pub input_saturation_events: u32,
}

impl RenderMetricsSnapshot {
    /// Magic bytes: 'M' 'G' (0x4D47) - identifies this as a Migo stats packet.
    pub const MAGIC: u16 = 0x4D47;
    /// Protocol version. Increment when field layout changes.
    ///
    /// * v1 - initial render metrics
    /// * v2 - render optimisation counters appended (offsets 44-63)
    /// * v3 - IO subsystem counters appended (offsets 64-95).
    /// * v4 - render queue / collector / cache observability
    ///   counters appended (offsets 96-115).  Adds five fields:
    ///   render_queue_len, collector_pending_bytes,
    ///   webgl_error_overflow, sk_image_wrappers, deferred_uploads.
    /// * v5 - Canvas2D zero-readback snapshot counters appended
    ///   (offsets 116-131).  Adds four fields:
    ///   canvas2d_snapshots_taken, canvas2d_snapshot_fallbacks,
    ///   canvas2d_snapshot_uploads, canvas2d_snapshot_forced_readbacks.
    /// * v6 - bounded input transport counters appended (offsets 132-143).
    ///   Adds input_coalesced, input_reliable_reserve_uses, and
    ///   input_saturation_events.
    pub const VERSION: u16 = 6;
    /// 4-byte header (2 magic + 2 version) + 140 bytes payload = 144.
    pub const HEADER_LEN: usize = 4;
    pub const PAYLOAD_LEN: usize = 140;
    pub const BYTE_LEN: usize = Self::HEADER_LEN + Self::PAYLOAD_LEN; // 144

    pub fn as_le_bytes(&self) -> [u8; Self::BYTE_LEN] {
        let mut bytes = [0u8; Self::BYTE_LEN];
        // Header
        bytes[0..2].copy_from_slice(&Self::MAGIC.to_le_bytes());
        bytes[2..4].copy_from_slice(&Self::VERSION.to_le_bytes());
        // Payload (offsets shifted by HEADER_LEN = 4)
        bytes[4..8].copy_from_slice(&self.fps_x10.to_le_bytes());
        bytes[8..12].copy_from_slice(&self.frame_time_us.to_le_bytes());
        bytes[12..16].copy_from_slice(&self.dropped_frames.to_le_bytes());
        bytes[16..20].copy_from_slice(&self.fatal_error_code.to_le_bytes());
        bytes[20..24].copy_from_slice(&self.first_frame_ms.to_le_bytes());
        bytes[24..28].copy_from_slice(&self.command_drops.to_le_bytes());
        bytes[28..32].copy_from_slice(&self.raf_latency_us.to_le_bytes());
        bytes[32..36].copy_from_slice(&self.swap_block_us.to_le_bytes());
        bytes[36..40].copy_from_slice(&self.upload_queue_depth.to_le_bytes());
        bytes[40..44].copy_from_slice(&self.glyph_atlas_miss.to_le_bytes());
        bytes[44..48].copy_from_slice(&self.partial_damage_frames.to_le_bytes());
        bytes[48..52].copy_from_slice(&self.full_surface_frames.to_le_bytes());
        bytes[52..56].copy_from_slice(&self.damage_area_k_pixels.to_le_bytes());
        bytes[56..60].copy_from_slice(&self.upload_frame_rejections.to_le_bytes());
        bytes[60..64].copy_from_slice(&self.dropped_upload_recoveries.to_le_bytes());
        // ---- v3 appended: IO metrics ----
        bytes[64..68].copy_from_slice(&self.decoder_fallback_count.to_le_bytes());
        bytes[68..72].copy_from_slice(&self.derived_cache_hits.to_le_bytes());
        bytes[72..76].copy_from_slice(&self.derived_cache_misses.to_le_bytes());
        bytes[76..80].copy_from_slice(&self.inline_image_policy_rejects.to_le_bytes());
        bytes[80..84].copy_from_slice(&self.ws_policy_rejects.to_le_bytes());
        bytes[84..88].copy_from_slice(&self.slow_io_count_100ms.to_le_bytes());
        bytes[88..92].copy_from_slice(&self.image_cache_admissions_rejected.to_le_bytes());
        bytes[92..96].copy_from_slice(&self.image_cache_trim_bytes.to_le_bytes());
        // ---- v4 appended: queue / collector / cache observability ----
        bytes[96..100].copy_from_slice(&self.render_queue_len.to_le_bytes());
        bytes[100..104].copy_from_slice(&self.collector_pending_bytes.to_le_bytes());
        bytes[104..108].copy_from_slice(&self.webgl_error_overflow.to_le_bytes());
        bytes[108..112].copy_from_slice(&self.sk_image_wrappers.to_le_bytes());
        bytes[112..116].copy_from_slice(&self.deferred_uploads.to_le_bytes());
        // ---- v5 appended: Canvas2D snapshot fast-path counters ----
        bytes[116..120].copy_from_slice(&self.canvas2d_snapshots_taken.to_le_bytes());
        bytes[120..124].copy_from_slice(&self.canvas2d_snapshot_fallbacks.to_le_bytes());
        bytes[124..128].copy_from_slice(&self.canvas2d_snapshot_uploads.to_le_bytes());
        bytes[128..132].copy_from_slice(&self.canvas2d_snapshot_forced_readbacks.to_le_bytes());
        // ---- v6 appended: bounded input transport ----
        bytes[132..136].copy_from_slice(&self.input_coalesced.to_le_bytes());
        bytes[136..140].copy_from_slice(&self.input_reliable_reserve_uses.to_le_bytes());
        bytes[140..144].copy_from_slice(&self.input_saturation_events.to_le_bytes());
        bytes
    }
}

/// Runtime debug statistics collected by the render thread.
///
/// All fields are atomics for lock-free reads from the JNI polling thread.
#[derive(Default)]
pub struct DebugStats {
    /// Current FPS multiplied by 10 (e.g., 598 = 59.8 FPS).
    pub fps_x10: AtomicU32,
    /// Last frame time in microseconds.
    pub frame_time_us: AtomicU32,
    /// Total number of dropped RAF signals (cumulative).
    pub dropped_frames: AtomicU32,
    /// Fatal error code from the engine (0 = no error).
    /// Set when the host thread is terminated (e.g., OOM, Timeout).
    /// Java layer can poll this to detect engine errors.
    pub fatal_error_code: AtomicU32,
    /// Milliseconds from render thread start to first frame presentation.
    /// Set once on the first swap_buffers; remains 0 until then.
    pub first_frame_ms: AtomicU32,
    /// Normal HostCommand quota drops recorded by the host registry
    /// (cumulative, per session). Trusted lifecycle/surface commands bypass
    /// this quota and are not counted in this field.
    pub command_drops: AtomicU32,
    /// Cumulative replace-in-place operations in the bounded input queue.
    pub input_coalesced: AtomicU32,
    /// Cumulative accepted transitions that consumed reliable input reserve.
    pub input_reliable_reserve_uses: AtomicU32,
    /// Cumulative input commands refused after every eligible lane was full.
    pub input_saturation_events: AtomicU32,
    /// Last measured RAF scheduling latency in microseconds.
    pub raf_latency_us: AtomicU32,
    /// Last measured swap/present blocking time in microseconds.
    pub swap_block_us: AtomicU32,
    /// Current upload queue depth sampled by render diagnostics.
    pub upload_queue_depth: AtomicU32,
    /// Cumulative glyph atlas cache misses observed by text rendering.
    pub glyph_atlas_miss: AtomicU32,
    // ---- Render optimization metrics ----
    /// Cumulative frames where damage resolved to Partial.
    pub partial_damage_frames: AtomicU32,
    /// Cumulative frames where damage resolved to FullSurface.
    pub full_surface_frames: AtomicU32,
    /// Cumulative partial damage area in 1000-pixel units (kpx).
    pub damage_area_k_pixels: AtomicU32,
    /// Cumulative per-frame upload budget rejections.
    pub upload_frame_rejections: AtomicU32,
    /// Cumulative dropped upload recoveries.
    pub dropped_upload_recoveries: AtomicU32,

    // ---- P11 render diagnostics (not in fixed snapshot) ----
    // These counters are NOT serialised into `RenderMetricsSnapshot`
    // to keep the existing Java ByteBuffer layout stable.  They're
    // exposed through a separate debug API (see
    // `engine/crates/graphics/render_diagnostics.rs`).  Bumping the
    // snapshot version to add them can happen in a later commit
    // once the Android consumer is updated in lock-step.
    /// Cumulative WebGL / Canvas2D draw calls dispatched.
    /// Canvas2D draws are counted once per paint op; WebGL draws are
    /// counted at each `glDrawArrays{Instanced}` /
    /// `glDrawElements{Instanced}` dispatch.
    pub draw_calls: AtomicU32,
    /// Cumulative GL state change calls issued (post-dedup).  A
    /// high ratio of `state_changes / draw_calls` indicates the
    /// game is thrashing state between draws; a very low one means
    /// the dedup is catching most redundancy.
    pub state_changes: AtomicU32,
    /// Cumulative draws issued with **zero post-dedup state changes since the
    /// previous draw of the same frame** — the upper bound on what a draw-call
    /// batching pass could merge.
    ///
    /// `state_changes / draw_calls` measures how much state a frame churns but
    /// says nothing about *where* it churns: a frame that sets up heavily and
    /// then issues fifty untouched draws reads the same as one alternating a
    /// change per draw, and only the first has anything to merge. This
    /// separates them, which is the number the batching question was missing.
    ///
    /// `adjacent_draws / draw_calls` near zero closes that question for the
    /// workload measured — there is nothing to merge. A high ratio does not
    /// open it on its own: a merge also needs matching primitive modes and
    /// contiguous vertex or index ranges, which this does not check.
    pub adjacent_draws: AtomicU32,
    /// Cumulative draws that are adjacent **and could actually be merged**:
    /// same primitive mode, and a range continuing exactly where the previous
    /// draw's ended.
    ///
    /// The figure that can open the batching question, where `adjacent_draws`
    /// can only close it. A frame can be 98% adjacent and 0% mergeable —
    /// drawing the same six vertices sixty-four times has nothing between the
    /// draws, but one draw of 384 vertices would paint different pixels. Only a
    /// contiguous range makes the merge an identity, and an identity is the only
    /// merge a renderer may perform without the game's consent.
    pub mergeable_draws: AtomicU32,
    /// Cumulative bytes uploaded via PBO / direct `glTexSubImage2D`
    /// calls this frame.  Resets to 0 at each `Present`.
    pub texture_upload_bytes: AtomicU32,
    /// Cumulative measureText cache hits.  Paired with
    /// `measure_text_misses` gives the hit rate.
    pub measure_text_hits: AtomicU32,
    pub measure_text_misses: AtomicU32,
    /// Cumulative Skia shape-cache hits (SkTextBlob path cache).
    pub shape_cache_hits: AtomicU32,
    pub shape_cache_misses: AtomicU32,
    /// Cumulative Skia SkImage wrapper cache hits.
    pub sk_image_wrapper_hits: AtomicU32,
    pub sk_image_wrapper_misses: AtomicU32,
    /// Cumulative drop-shadow `ImageFilter` cache hits.  A high
    /// hit rate confirms games reuse the same shadow config
    /// across many draws; a low rate suggests per-draw param
    /// churn (e.g. animated shadow).
    pub shadow_filter_hits: AtomicU32,
    pub shadow_filter_misses: AtomicU32,
    /// Cumulative dash `PathEffect` cache hits.  Same interpretation
    /// as above but for stroked dashed lines.
    pub dash_effect_hits: AtomicU32,
    pub dash_effect_misses: AtomicU32,
    /// Cumulative gradient `Shader` cache hits.  UI-heavy scenes that
    /// repeat the same `ctx.createLinearGradient(...)` fill across
    /// many draws see very high hit rates here; a low rate suggests
    /// per-frame gradient rebuilds that should be hoisted JS-side.
    pub gradient_hits: AtomicU32,
    pub gradient_misses: AtomicU32,
    /// Cumulative hits on the process-global text texture cache
    /// (`shared::text_texture_cache`).  A hit means a fillText whose
    /// `(text, font, size, color, ...)` tuple was already rendered
    /// earlier in this process — the cached GL texture was reused
    /// via `TexImageSource` (text-cache source), skipping the entire offscreen
    /// Canvas2D + snapshot + blit pipeline.  Repeat-shop-open hit
    /// rates above ~80% are the design target.
    pub text_cache_hits: AtomicU32,
    pub text_cache_misses: AtomicU32,
    /// Current resident size in bytes (RGBA8 textures).  Gauge, not
    /// counter — written on insert/eviction/trim.
    pub text_cache_bytes: AtomicU32,
    /// Current live entry count.  Gauge.
    pub text_cache_entries: AtomicU32,
    /// Cumulative `GrDirectContext::reset()` calls (lazy reset
    /// path).  A fast-rising counter indicates frequent WebGL↔
    /// Canvas2D boundary crossings.
    pub skia_context_resets: AtomicU32,

    // ---- Canvas2D zero-readback fast path (cocos text rendering) ----
    //
    // `canvas2d_snapshots_taken` rises when `getImageData` successfully
    // captured a GPU snapshot; `canvas2d_snapshot_fallbacks` rises when
    // the snapshot path returned 0 and JS dropped to the legacy CPU
    // readback (GLES 2 device, FBO incomplete, oversized region, etc.).
    // `canvas2d_snapshot_uploads` rises every time a snapshot was
    // consumed by `texImage2D`.  In the steady-state cocos pattern we
    // expect taken ≈ uploads ≫ fallbacks; if uploads ≪ taken the
    // game is reading the lazy `.data` bytes instead of
    // routing into WebGL, and the snapshot work is wasted overhead.
    pub canvas2d_snapshots_taken: AtomicU32,
    pub canvas2d_snapshot_fallbacks: AtomicU32,
    pub canvas2d_snapshot_uploads: AtomicU32,
    /// Cumulative lazy snapshot `ImageData.data` materializations. A non-zero
    /// value indicates a game on the slow path even after the
    /// snapshot optimisation; investigate before assuming the perf
    /// win is universal.
    pub canvas2d_snapshot_forced_readbacks: AtomicU32,

    // ---- Render-thread error feedback (P1-1 / P2-10) ----
    //
    // These counters replace the previous `error!()`-and-lose
    // model: every render-thread-side failure that historically
    // was only visible in logcat now bumps a structured counter
    // the Java overlay / JS inspector can poll.  Writes are cheap
    // (`Relaxed` Add) and reads are lock-free.
    //
    // Not included in `RenderMetricsSnapshot` wire format yet to
    // keep the Java ByteBuffer layout stable; surfaced via
    // `render_diagnostics` for the debug overlay.
    /// Cumulative `Canvas2DBatch` / single `Canvas2D` command
    /// failures after routing through the dispatcher.  A
    /// fast-rising counter indicates Skia surface loss, AHB
    /// fallback chain exhaustion, or protocol-level regressions.
    pub canvas2d_cmd_errors: AtomicU32,
    /// Cumulative `GLBatch` / single `GL` command failures.
    pub gl_cmd_errors: AtomicU32,
    /// Cumulative `CanvasCmd` (create/destroy/recreate/resize)
    /// failures.
    pub canvas_cmd_errors: AtomicU32,
    /// Cumulative `swap_buffers_no_restore` failures (excludes
    /// the `EGL_CONTEXT_LOST` path, which is counted separately).
    pub swap_failures: AtomicU32,
    /// Cumulative `EGL_CONTEXT_LOST` detections.  The next frame
    /// runs `try_recover_context`, and each recovery attempt —
    /// successful or not — also bumps `context_recoveries`.
    pub context_lost_events: AtomicU32,
    pub context_recoveries: AtomicU32,
    /// Current consecutive-RAF-drop streak.  `dropped_frames` is
    /// monotonic; this is the count of drops *in a row*.  Reset
    /// to 0 when RAF dispatch succeeds.  Used by the RAF
    /// backpressure detector (P2-10): at ≥ 3 consecutive drops
    /// the render thread emits a `RenderEvent::RafBackpressure`.
    pub raf_drop_streak: AtomicU32,
    /// Cumulative times the RAF backpressure threshold was
    /// crossed; lets operators tell a one-shot network hiccup
    /// from sustained producer saturation.
    pub raf_backpressure_events: AtomicU32,

    /// Cumulative times the render thread hit the drain CPU
    /// budget (MAX_DRAIN_US) and broke out with commands still
    /// pending.  A rising counter means the render thread is
    /// consistently starved of time to clear its backlog —
    /// combine with `render_queue_len` to confirm backlog vs
    /// steady-state.  See `render_thread::drain_cmds`.
    pub drain_budget_exhausted: AtomicU32,

    // ---- IO subsystem counters (M5.1, surface at payload v3) ----
    /// Count of Android image decodes that fell back from the AHB
    /// zero-copy path to the RGBA `byte[]` path. Non-zero on API
    /// < 30 (no `Bitmap.getHardwareBuffer`) and on driver-specific
    /// edge cases where the hardware allocator silently fails.
    pub decoder_fallback_count: AtomicU32,

    /// Derived (on-disk RGBA sidecar) cache served the request
    /// without re-running the decoder.
    pub derived_cache_hits: AtomicU32,

    /// Derived cache miss — the decoder ran. Paired with hits
    /// gives the warm-cache ratio.
    pub derived_cache_misses: AtomicU32,

    /// `Image.src = "http(s)://..."` rejected by the shared
    /// network-policy gate (whitelist / HTTPS / SSRF).
    pub inline_image_policy_rejects: AtomicU32,

    /// `new WebSocket(url)` rejected by the same gate.
    pub ws_policy_rejects: AtomicU32,

    /// IO requests that took over 100 ms wall-clock time. A spike
    /// here almost always means disk pressure or an fsync storm
    /// rather than a Rust-side hang.
    pub slow_io_count_100ms: AtomicU32,

    /// Count of `image_cache::insert` calls rejected by the
    /// W-TinyLFU admission filter — the cache refused to evict a
    /// warmer entry for a colder newcomer.
    pub image_cache_admissions_rejected: AtomicU32,

    /// Bytes released from the image cache in response to
    /// `onTrimMemory`-driven trim calls. Cumulative.
    pub image_cache_trim_bytes: AtomicU32,

    // ---- Queue / collector / cache observability (v4) ----
    /// Current render command channel backlog (instantaneous).
    /// Sampled by the render thread once per frame; the Java debug
    /// overlay surfaces it as "render queue: N / cap" so a command
    /// storm is visible before it turns into a stall.
    pub render_queue_len: AtomicU32,

    /// Peak command bytes retained by the JS-side frame collector during the
    /// most recently completed logical frame. Published once at frame end;
    /// sync/auto-flush barriers do not reset the logical-frame peak.
    pub collector_pending_bytes: AtomicU32,

    /// Current count of `SkImage` wrapper entries held by
    /// `ImageStore::sk_image_cache`.  Dividing by the number of
    /// live `Canvas2DContext`s gives the wrapper multiplication
    /// factor incurred by per-context Skia caches; collapses to the
    /// wrapper count itself once `GrDirectContext` is shared.
    pub sk_image_wrappers: AtomicU32,

    /// Current count of uploads waiting for the next frame's
    /// budget to open up (see `CanvasManager::deferred_uploads`).
    /// A rising number means the per-frame upload budget is
    /// undersized for the current workload.
    pub deferred_uploads: AtomicU32,
}

impl DebugStats {
    #[inline]
    pub fn fps(&self) -> f32 {
        self.fps_x10.load(Ordering::Relaxed) as f32 / 10.0
    }

    #[inline]
    pub fn frame_time_ms(&self) -> f32 {
        self.frame_time_us.load(Ordering::Relaxed) as f32 / 1000.0
    }

    pub fn snapshot(&self) -> [u8; RenderMetricsSnapshot::BYTE_LEN] {
        // IO metrics live in a process-global aggregator because
        // most of their producers (image cache, network gate,
        // decode fallback path) don't have a host_id in scope.
        // Per-session attribution can be added later by making
        // the aggregator session-keyed; until then the counters
        // reflect "this process since start".
        let io = io_metrics_global();
        RenderMetricsSnapshot {
            fps_x10: self.fps_x10.load(Ordering::Relaxed),
            frame_time_us: self.frame_time_us.load(Ordering::Relaxed),
            dropped_frames: self.dropped_frames.load(Ordering::Relaxed),
            fatal_error_code: self.fatal_error_code.load(Ordering::Relaxed),
            first_frame_ms: self.first_frame_ms.load(Ordering::Relaxed),
            // `command_drops` aggregates per-session slot + the
            // process-global `CommandSender` overflow counter so
            // the Java overlay shows any dropped command, not just
            // those recorded via `send_command_to_host`.
            command_drops: self.command_drops.load(Ordering::Relaxed).saturating_add(
                crate::render_command_sender::send_overflow_total().min(u32::MAX as u64) as u32,
            ),
            raf_latency_us: self.raf_latency_us.load(Ordering::Relaxed),
            swap_block_us: self.swap_block_us.load(Ordering::Relaxed),
            upload_queue_depth: self.upload_queue_depth.load(Ordering::Relaxed),
            glyph_atlas_miss: self.glyph_atlas_miss.load(Ordering::Relaxed),
            partial_damage_frames: self.partial_damage_frames.load(Ordering::Relaxed),
            full_surface_frames: self.full_surface_frames.load(Ordering::Relaxed),
            damage_area_k_pixels: self.damage_area_k_pixels.load(Ordering::Relaxed),
            upload_frame_rejections: self.upload_frame_rejections.load(Ordering::Relaxed),
            dropped_upload_recoveries: self.dropped_upload_recoveries.load(Ordering::Relaxed),
            decoder_fallback_count: io.decoder_fallback_count.load(Ordering::Relaxed),
            derived_cache_hits: io.derived_cache_hits.load(Ordering::Relaxed),
            derived_cache_misses: io.derived_cache_misses.load(Ordering::Relaxed),
            inline_image_policy_rejects: io.inline_image_policy_rejects.load(Ordering::Relaxed),
            ws_policy_rejects: io.ws_policy_rejects.load(Ordering::Relaxed),
            slow_io_count_100ms: io.slow_io_count_100ms.load(Ordering::Relaxed),
            image_cache_admissions_rejected: io
                .image_cache_admissions_rejected
                .load(Ordering::Relaxed),
            image_cache_trim_bytes: io.image_cache_trim_bytes.load(Ordering::Relaxed),
            render_queue_len: self.render_queue_len.load(Ordering::Relaxed),
            collector_pending_bytes: self.collector_pending_bytes.load(Ordering::Relaxed),
            // Retired: WebGL errors are held as flags, one per code, so the
            // queue that used to overflow cannot. The slot stays so the
            // layout the platform overlays read is unchanged.
            webgl_error_overflow: 0,
            sk_image_wrappers: self.sk_image_wrappers.load(Ordering::Relaxed),
            deferred_uploads: self.deferred_uploads.load(Ordering::Relaxed),
            canvas2d_snapshots_taken: self.canvas2d_snapshots_taken.load(Ordering::Relaxed),
            canvas2d_snapshot_fallbacks: self.canvas2d_snapshot_fallbacks.load(Ordering::Relaxed),
            canvas2d_snapshot_uploads: self.canvas2d_snapshot_uploads.load(Ordering::Relaxed),
            canvas2d_snapshot_forced_readbacks: self
                .canvas2d_snapshot_forced_readbacks
                .load(Ordering::Relaxed),
            input_coalesced: self.input_coalesced.load(Ordering::Relaxed),
            input_reliable_reserve_uses: self.input_reliable_reserve_uses.load(Ordering::Relaxed),
            input_saturation_events: self.input_saturation_events.load(Ordering::Relaxed),
        }
        .as_le_bytes()
    }
}

// ---------------------------------------------------------------------------
// Process-global IO metrics aggregator
// ---------------------------------------------------------------------------
//
// Producers call `io_metrics_global().incr_*()` directly; the
// aggregator is a single `OnceLock` so the atomic counters land in
// the same cache line regardless of how many session `DebugStats`
// instances exist. Consolidating here avoids threading the
// session-scoped `DebugStats` through every `image_cache::insert`
// or `fetch_http_image` call site.

/// Why the AHB zero-copy decoder path rejected an image and fell
/// back to RGBA. Kept in a narrow enum so every failure is reported
/// as one of a few stable reasons; new reasons should be added as
/// explicit variants rather than folded into `Unknown`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AhbFallbackReason {
    /// AHB path doesn't handle this codec (e.g. unusual WebP, GIF).
    UnsupportedFormat,
    /// ImageDecoder constructed but refused to decode (corrupt data,
    /// unsupported color space, etc.).
    DecoderRejected,
    /// AHB allocation itself failed — pre-API-30 device, driver
    /// refusal, or low memory.
    HardwareBufferUnavailable,
    /// Image exceeds per-buffer pixel/byte limits.
    TooLarge,
    /// Anything we haven't classified yet.
    Unknown,
}

/// Atomic counters for the IO subsystem. Mirrored into every
/// session's [`DebugStats::snapshot`] at the time of the call.
#[derive(Default)]
pub struct IoMetrics {
    pub decoder_fallback_count: AtomicU32,
    pub derived_cache_hits: AtomicU32,
    pub derived_cache_misses: AtomicU32,
    pub inline_image_policy_rejects: AtomicU32,
    pub ws_policy_rejects: AtomicU32,
    pub slow_io_count_100ms: AtomicU32,
    pub image_cache_admissions_rejected: AtomicU32,
    pub image_cache_trim_bytes: AtomicU32,
    /// Per-reason AHB fallback counters. Sum equals
    /// `decoder_fallback_count`; splitting lets operators see *why*
    /// the fast path didn't engage without re-running traces.
    pub ahb_fallback_unsupported_format: AtomicU32,
    pub ahb_fallback_decoder_rejected: AtomicU32,
    pub ahb_fallback_hw_unavailable: AtomicU32,
    pub ahb_fallback_too_large: AtomicU32,
    pub ahb_fallback_unknown: AtomicU32,

    // ---- Per-operation latency counters (P4 observability) --------
    //
    // We expose `count`, `sum_ms`, and `max_ms` per class rather than
    // real histograms so the telemetry cost stays an atomic add per
    // op. Downstream dashboards can compute the mean (`sum / count`)
    // and the peak; true p50 / p99 require a histogram (tdigest,
    // HdrHistogram) that is a separate follow-up.
    pub fetch_count: AtomicU64,
    pub fetch_fail_count: AtomicU64,
    pub fetch_total_ms_sum: AtomicU64,
    pub fetch_total_ms_max: AtomicU64,
    pub fetch_first_byte_ms_sum: AtomicU64,
    pub fetch_first_byte_ms_max: AtomicU64,

    pub ws_connect_ms_sum: AtomicU64,
    pub ws_connect_ms_max: AtomicU64,
    pub ws_connect_count: AtomicU64,
    pub ws_msg_in_bytes_total: AtomicU64,

    pub storage_get_count: AtomicU64,
    pub storage_get_ms_sum: AtomicU64,
    pub storage_get_ms_max: AtomicU64,
    pub storage_set_count: AtomicU64,
    pub storage_set_ms_sum: AtomicU64,
    pub storage_set_ms_max: AtomicU64,
    pub storage_info_count: AtomicU64,
    pub storage_info_ms_sum: AtomicU64,
    pub storage_info_ms_max: AtomicU64,

    pub download_count: AtomicU64,
    pub download_bytes_total: AtomicU64,
    pub download_ms_sum: AtomicU64,
    pub download_ms_max: AtomicU64,
    pub upload_count: AtomicU64,
    pub upload_bytes_total: AtomicU64,
    pub upload_ms_sum: AtomicU64,
    pub upload_ms_max: AtomicU64,
}

/// Bucketed operation-class selector for [`IoMetrics::record_op`].
#[derive(Debug, Clone, Copy)]
pub enum OpClass {
    FetchTotal,
    FetchFirstByte,
    FetchFail,
    WsConnect,
    WsBytesIn(u64),
    StorageGet,
    StorageSet,
    StorageInfo,
    Download { bytes: u64 },
    Upload { bytes: u64 },
}

impl IoMetrics {
    /// Record a slow IO operation if `elapsed` is past the 100 ms
    /// threshold. Returns the post-increment count or 0 for the
    /// non-slow case, matching [`AtomicU32::fetch_add`] semantics.
    #[inline]
    pub fn record_if_slow(&self, elapsed_ms: u64) -> u32 {
        if elapsed_ms >= 100 {
            self.slow_io_count_100ms.fetch_add(1, Ordering::Relaxed) + 1
        } else {
            0
        }
    }

    /// Record an operation's elapsed time + payload info against the
    /// class-specific atomic counters. The `elapsed` argument is
    /// accepted as `Duration` so call sites don't have to convert to
    /// ms themselves; the stored field is ms (u64) because that's what
    /// the downstream metrics surface expects.
    pub fn record_op(&self, class: OpClass, elapsed: std::time::Duration) {
        let ms = elapsed.as_millis() as u64;
        #[inline]
        fn bump_max(slot: &AtomicU64, v: u64) {
            let mut cur = slot.load(Ordering::Relaxed);
            while v > cur {
                match slot.compare_exchange_weak(cur, v, Ordering::Relaxed, Ordering::Relaxed) {
                    Ok(_) => break,
                    Err(new) => cur = new,
                }
            }
        }
        match class {
            OpClass::FetchTotal => {
                self.fetch_count.fetch_add(1, Ordering::Relaxed);
                self.fetch_total_ms_sum.fetch_add(ms, Ordering::Relaxed);
                bump_max(&self.fetch_total_ms_max, ms);
            }
            OpClass::FetchFirstByte => {
                self.fetch_first_byte_ms_sum
                    .fetch_add(ms, Ordering::Relaxed);
                bump_max(&self.fetch_first_byte_ms_max, ms);
            }
            OpClass::FetchFail => {
                self.fetch_fail_count.fetch_add(1, Ordering::Relaxed);
            }
            OpClass::WsConnect => {
                self.ws_connect_count.fetch_add(1, Ordering::Relaxed);
                self.ws_connect_ms_sum.fetch_add(ms, Ordering::Relaxed);
                bump_max(&self.ws_connect_ms_max, ms);
            }
            OpClass::WsBytesIn(bytes) => {
                self.ws_msg_in_bytes_total
                    .fetch_add(bytes, Ordering::Relaxed);
            }
            OpClass::StorageGet => {
                self.storage_get_count.fetch_add(1, Ordering::Relaxed);
                self.storage_get_ms_sum.fetch_add(ms, Ordering::Relaxed);
                bump_max(&self.storage_get_ms_max, ms);
            }
            OpClass::StorageSet => {
                self.storage_set_count.fetch_add(1, Ordering::Relaxed);
                self.storage_set_ms_sum.fetch_add(ms, Ordering::Relaxed);
                bump_max(&self.storage_set_ms_max, ms);
            }
            OpClass::StorageInfo => {
                self.storage_info_count.fetch_add(1, Ordering::Relaxed);
                self.storage_info_ms_sum.fetch_add(ms, Ordering::Relaxed);
                bump_max(&self.storage_info_ms_max, ms);
            }
            OpClass::Download { bytes } => {
                self.download_count.fetch_add(1, Ordering::Relaxed);
                self.download_bytes_total
                    .fetch_add(bytes, Ordering::Relaxed);
                self.download_ms_sum.fetch_add(ms, Ordering::Relaxed);
                bump_max(&self.download_ms_max, ms);
            }
            OpClass::Upload { bytes } => {
                self.upload_count.fetch_add(1, Ordering::Relaxed);
                self.upload_bytes_total.fetch_add(bytes, Ordering::Relaxed);
                self.upload_ms_sum.fetch_add(ms, Ordering::Relaxed);
                bump_max(&self.upload_ms_max, ms);
            }
        }
    }

    /// Bump both the total AHB fallback counter and the per-reason
    /// bucket. Call sites pass a classified reason so the per-bucket
    /// counters stay in sync with `decoder_fallback_count`.
    #[inline]
    pub fn record_ahb_fallback(&self, reason: AhbFallbackReason) {
        self.decoder_fallback_count.fetch_add(1, Ordering::Relaxed);
        let bucket = match reason {
            AhbFallbackReason::UnsupportedFormat => &self.ahb_fallback_unsupported_format,
            AhbFallbackReason::DecoderRejected => &self.ahb_fallback_decoder_rejected,
            AhbFallbackReason::HardwareBufferUnavailable => &self.ahb_fallback_hw_unavailable,
            AhbFallbackReason::TooLarge => &self.ahb_fallback_too_large,
            AhbFallbackReason::Unknown => &self.ahb_fallback_unknown,
        };
        bucket.fetch_add(1, Ordering::Relaxed);
    }
}

static IO_METRICS: OnceLock<IoMetrics> = OnceLock::new();

/// Shared IO metrics accessible from anywhere in the workspace.
#[inline]
pub fn io_metrics_global() -> &'static IoMetrics {
    IO_METRICS.get_or_init(IoMetrics::default)
}

static STATS: OnceLock<RwLock<HashMap<i32, Arc<DebugStats>>>> = OnceLock::new();

fn stats_map() -> &'static RwLock<HashMap<i32, Arc<DebugStats>>> {
    STATS.get_or_init(|| RwLock::new(HashMap::new()))
}

/// The stats registry's lock, for Section 7.3's cross-session contention gate.
///
/// Every Session's `DebugStats` lives in one process-wide map so out-of-band
/// consumers can find a Session by id: the JNI debug poll, and the frame collector's
/// one-time resolve. That is sound only while no *per-event* path looks it up, which
/// Section 7.3 requires a test — not an argument — to establish. The per-event paths
/// live in other crates and cannot hold a private lock, so this hands it over, behind
/// a feature no shipped build enables.
#[cfg(any(test, feature = "contention-probe"))]
pub fn registry_lock_for_contention_probe() -> &'static RwLock<HashMap<i32, Arc<DebugStats>>> {
    stats_map()
}

/// The `DebugStats` for `id`, creating it on first ask.
///
/// Get-or-create rather than always-fresh because two bring-up paths reach for it —
/// host registration, so the input path can hold it, and the render thread — and
/// neither is ordered before the other. Always-fresh would leave whichever ran second
/// holding a handle the other cannot see, and its counters would simply vanish.
///
/// There is no stale entry for a later host to inherit: host ids only ever count up.
pub fn stats_for(id: i32) -> Arc<DebugStats> {
    Arc::clone(
        stats_map()
            .write()
            .entry(id)
            .or_insert_with(|| Arc::new(DebugStats::default())),
    )
}

/// Unregister stats for a host_id (cleanup on shutdown).
pub fn unregister_stats(id: i32) {
    stats_map().write().remove(&id);
}

/// Get the DebugStats for a host_id (used by JNI polling).
pub fn get_stats(id: i32) -> Option<Arc<DebugStats>> {
    stats_map().read().get(&id).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    #[test]
    fn render_metrics_snapshot_serializes_le_bytes() {
        let stats = DebugStats::default();
        stats.fps_x10.store(600, Ordering::Relaxed);
        stats.frame_time_us.store(16_600, Ordering::Relaxed);
        stats.first_frame_ms.store(320, Ordering::Relaxed);
        stats.command_drops.store(3, Ordering::Relaxed);
        stats.raf_latency_us.store(777, Ordering::Relaxed);
        stats.swap_block_us.store(888, Ordering::Relaxed);
        stats.upload_queue_depth.store(9, Ordering::Relaxed);
        stats.glyph_atlas_miss.store(10, Ordering::Relaxed);
        stats.input_coalesced.store(11, Ordering::Relaxed);
        stats
            .input_reliable_reserve_uses
            .store(12, Ordering::Relaxed);
        stats.input_saturation_events.store(13, Ordering::Relaxed);

        let bytes = stats.snapshot();

        // v6 payload = 140 bytes (v5's 128 + 12 input) + 4 header = 144.
        assert_eq!(bytes.len(), RenderMetricsSnapshot::BYTE_LEN);
        assert_eq!(RenderMetricsSnapshot::BYTE_LEN, 144);

        // Header: magic 'MG' (0x4D47) at [0..2], version 6 at [2..4].
        assert_eq!(
            u16::from_le_bytes(bytes[0..2].try_into().unwrap()),
            RenderMetricsSnapshot::MAGIC
        );
        assert_eq!(
            u16::from_le_bytes(bytes[2..4].try_into().unwrap()),
            RenderMetricsSnapshot::VERSION
        );
        assert_eq!(RenderMetricsSnapshot::VERSION, 6);

        // Payload fields — all offsets shifted by +4 (HEADER_LEN).
        assert_eq!(u32::from_le_bytes(bytes[4..8].try_into().unwrap()), 600);
        assert_eq!(u32::from_le_bytes(bytes[8..12].try_into().unwrap()), 16_600);
        assert_eq!(u32::from_le_bytes(bytes[20..24].try_into().unwrap()), 320);
        assert_eq!(u32::from_le_bytes(bytes[24..28].try_into().unwrap()), 3);
        assert_eq!(u32::from_le_bytes(bytes[28..32].try_into().unwrap()), 777);
        assert_eq!(u32::from_le_bytes(bytes[32..36].try_into().unwrap()), 888);
        assert_eq!(u32::from_le_bytes(bytes[36..40].try_into().unwrap()), 9);
        assert_eq!(u32::from_le_bytes(bytes[40..44].try_into().unwrap()), 10);
        // Render optimization fields default to 0 at offsets 44-63.
        assert_eq!(u32::from_le_bytes(bytes[44..48].try_into().unwrap()), 0);
        assert_eq!(u32::from_le_bytes(bytes[48..52].try_into().unwrap()), 0);
        assert_eq!(u32::from_le_bytes(bytes[132..136].try_into().unwrap()), 11);
        assert_eq!(u32::from_le_bytes(bytes[136..140].try_into().unwrap()), 12);
        assert_eq!(u32::from_le_bytes(bytes[140..144].try_into().unwrap()), 13);
    }

    #[test]
    fn new_render_optimization_fields_serialize_at_tail() {
        let stats = DebugStats::default();
        stats.partial_damage_frames.store(42, Ordering::Relaxed);
        stats.full_surface_frames.store(7, Ordering::Relaxed);
        stats.damage_area_k_pixels.store(1500, Ordering::Relaxed);
        stats.upload_frame_rejections.store(3, Ordering::Relaxed);
        stats.dropped_upload_recoveries.store(1, Ordering::Relaxed);

        let bytes = stats.snapshot();

        assert_eq!(bytes.len(), 144);
        // Payload offsets shifted by +4 (HEADER_LEN).
        assert_eq!(u32::from_le_bytes(bytes[44..48].try_into().unwrap()), 42);
        assert_eq!(u32::from_le_bytes(bytes[48..52].try_into().unwrap()), 7);
        assert_eq!(u32::from_le_bytes(bytes[52..56].try_into().unwrap()), 1500);
        assert_eq!(u32::from_le_bytes(bytes[56..60].try_into().unwrap()), 3);
        assert_eq!(u32::from_le_bytes(bytes[60..64].try_into().unwrap()), 1);
    }

    #[test]
    fn v4_queue_and_cache_fields_serialize_at_tail() {
        let stats = DebugStats::default();
        stats.render_queue_len.store(321, Ordering::Relaxed);
        stats
            .collector_pending_bytes
            .store(4_000_000, Ordering::Relaxed);
        stats.sk_image_wrappers.store(88, Ordering::Relaxed);
        stats.deferred_uploads.store(5, Ordering::Relaxed);

        let bytes = stats.snapshot();
        assert_eq!(bytes.len(), 144);
        assert_eq!(u32::from_le_bytes(bytes[96..100].try_into().unwrap()), 321);
        assert_eq!(
            u32::from_le_bytes(bytes[100..104].try_into().unwrap()),
            4_000_000
        );
        assert_eq!(
            u32::from_le_bytes(bytes[104..108].try_into().unwrap()),
            0,
            "the retired WebGL error-overflow slot"
        );
        assert_eq!(u32::from_le_bytes(bytes[108..112].try_into().unwrap()), 88);
        assert_eq!(u32::from_le_bytes(bytes[112..116].try_into().unwrap()), 5);

        let other = DebugStats::default().snapshot();
        assert_eq!(
            u32::from_le_bytes(other[100..104].try_into().unwrap()),
            0,
            "collector peak must be scoped to one DebugStats/session"
        );
    }

    #[test]
    fn io_metrics_global_is_a_process_singleton() {
        // Two calls must return the same object so increments from
        // different crates (io, runtime-v8, graphics) accumulate
        // into one view.
        let a = io_metrics_global() as *const IoMetrics;
        let b = io_metrics_global() as *const IoMetrics;
        assert_eq!(a, b);
    }

    #[test]
    fn io_metrics_increments_reflected_in_snapshot_tail() {
        // Drive a few counters from the global aggregator and check
        // the DebugStats snapshot picks them up at the expected
        // offsets. This is a contract test for the v3 layout.
        let io = io_metrics_global();
        let base_decoder = io.decoder_fallback_count.load(Ordering::Relaxed);
        let base_hits = io.derived_cache_hits.load(Ordering::Relaxed);
        io.decoder_fallback_count.fetch_add(2, Ordering::Relaxed);
        io.derived_cache_hits.fetch_add(5, Ordering::Relaxed);
        io.ws_policy_rejects.fetch_add(1, Ordering::Relaxed);

        let stats = DebugStats::default();
        let bytes = stats.snapshot();

        let decoder_fb = u32::from_le_bytes(bytes[64..68].try_into().unwrap());
        let dc_hits = u32::from_le_bytes(bytes[68..72].try_into().unwrap());
        let ws_rej = u32::from_le_bytes(bytes[80..84].try_into().unwrap());
        assert!(
            decoder_fb >= base_decoder + 2,
            "fallback counter not forwarded"
        );
        assert!(dc_hits >= base_hits + 5, "derived-cache hits not forwarded");
        assert!(ws_rej >= 1, "ws rejects not forwarded");
    }

    #[test]
    fn record_if_slow_only_bumps_above_threshold() {
        let io = io_metrics_global();
        let before = io.slow_io_count_100ms.load(Ordering::Relaxed);
        assert_eq!(io.record_if_slow(50), 0, "50ms must not count");
        assert_eq!(
            io.slow_io_count_100ms.load(Ordering::Relaxed),
            before,
            "counter must be untouched"
        );
        let post = io.record_if_slow(250);
        assert!(post > before, "250ms must bump counter");
    }
}

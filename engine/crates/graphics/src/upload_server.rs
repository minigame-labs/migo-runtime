use crate::device_caps::DeviceCapabilities;
use crate::device_profile::DeviceRenderProfile;
use crate::upload_thread::{DroppedUpload, UploadJob};
#[cfg(test)]
use std::sync::Arc;

#[derive(Debug, Clone)]
pub(crate) struct UploadBudget {
    max_jobs: usize,
    max_bytes: usize,
    jobs_left: usize,
    bytes_left: usize,
}

impl UploadBudget {
    pub(crate) fn new(max_jobs: usize, max_bytes: usize) -> Self {
        Self {
            max_jobs,
            max_bytes,
            jobs_left: max_jobs,
            bytes_left: max_bytes,
        }
    }

    pub(crate) fn from_profile(profile: DeviceRenderProfile) -> Self {
        Self::new(
            profile.max_upload_jobs_per_frame,
            profile.max_upload_bytes_per_frame,
        )
    }

    pub(crate) fn try_acquire(&mut self, bytes: usize) -> bool {
        if self.jobs_left == 0 || bytes > self.bytes_left {
            return false;
        }

        self.jobs_left -= 1;
        self.bytes_left -= bytes;
        true
    }

    pub(crate) fn release(&mut self, bytes: usize) {
        self.jobs_left = (self.jobs_left + 1).min(self.max_jobs);
        self.bytes_left = (self.bytes_left + bytes).min(self.max_bytes);
    }

    #[allow(dead_code)]
    pub(crate) fn jobs_left(&self) -> usize {
        self.jobs_left
    }

    #[allow(dead_code)]
    pub(crate) fn bytes_left(&self) -> usize {
        self.bytes_left
    }
}

/// Per-frame upload limit.  Decremented on each submit, reset at frame boundary.
/// Unlike UploadBudget (in-flight), this is a one-way countdown per frame —
/// completions do NOT restore it.
#[derive(Debug, Clone)]
pub(crate) struct FrameBudget {
    max_jobs: usize,
    max_bytes: usize,
    jobs_left: usize,
    bytes_left: usize,
}

impl FrameBudget {
    fn new(max_jobs: usize, max_bytes: usize) -> Self {
        Self {
            max_jobs,
            max_bytes,
            jobs_left: max_jobs,
            bytes_left: max_bytes,
        }
    }

    #[allow(dead_code)]
    fn try_acquire(&mut self, bytes: usize) -> bool {
        if self.jobs_left == 0 || bytes > self.bytes_left {
            return false;
        }
        self.jobs_left -= 1;
        self.bytes_left -= bytes;
        true
    }

    fn reset(&mut self) {
        self.jobs_left = self.max_jobs;
        self.bytes_left = self.max_bytes;
    }
}

#[derive(Debug, Clone)]
pub(crate) struct UploadServer {
    queue_depth: u32,
    /// In-flight concurrent limit: restored on completion.
    budget: UploadBudget,
    /// Per-frame submission limit: one-way countdown, reset at frame boundary.
    frame_budget: Option<FrameBudget>,
    /// Number of submissions rejected by frame budget since last reset.
    frame_rejections: u32,
}

impl UploadServer {
    #[allow(dead_code)]
    pub(crate) fn new(max_jobs: usize, max_bytes: usize) -> Self {
        Self {
            queue_depth: 0,
            budget: UploadBudget::new(max_jobs, max_bytes),
            frame_budget: None,
            frame_rejections: 0,
        }
    }

    pub(crate) fn for_device(caps: &DeviceCapabilities, api_level: u32) -> Self {
        let profile = caps.render_profile(api_level);
        let mut server = Self {
            queue_depth: 0,
            budget: UploadBudget::from_profile(profile),
            frame_budget: None,
            frame_rejections: 0,
        };
        server.set_frame_budget(
            profile.max_upload_jobs_per_frame,
            profile.max_upload_bytes_per_frame,
        );
        server
    }

    pub(crate) fn queue_depth(&self) -> u32 {
        self.queue_depth
    }

    #[allow(dead_code)]
    pub(crate) fn budget(&self) -> &UploadBudget {
        &self.budget
    }

    /// Configure per-frame submission limits.
    pub(crate) fn set_frame_budget(&mut self, max_jobs: usize, max_bytes: usize) {
        self.frame_budget = Some(FrameBudget::new(max_jobs, max_bytes));
    }

    /// Reset per-frame counters at frame boundary.  Called once per VSync/tick
    /// from the render thread's `present_frame_and_signal_raf`.
    pub(crate) fn reset_frame_budget(&mut self) {
        if let Some(ref mut fb) = self.frame_budget {
            fb.reset();
        }
        self.frame_rejections = 0;
    }

    /// Number of submissions rejected by per-frame budget since last reset.
    pub(crate) fn frame_rejections(&self) -> u32 {
        self.frame_rejections
    }

    pub(crate) fn try_acquire(&mut self, bytes: usize) -> bool {
        // Check per-frame budget (read-only probe, don't decrement yet).
        if let Some(ref fb) = self.frame_budget {
            if fb.jobs_left == 0 || bytes > fb.bytes_left {
                self.frame_rejections += 1;
                return false;
            }
        }
        // Check in-flight budget.
        if !self.budget.try_acquire(bytes) {
            return false;
        }
        // Both passed — now decrement frame budget.
        if let Some(ref mut fb) = self.frame_budget {
            // Safe: we checked above that there's room.
            fb.jobs_left -= 1;
            fb.bytes_left -= bytes;
        }
        self.queue_depth = self.queue_depth.saturating_add(1);
        true
    }

    pub(crate) fn try_acquire_job(&mut self, job: &UploadJob) -> bool {
        self.try_acquire(job.byte_len())
    }

    pub(crate) fn can_ever_fit_bytes(&self, bytes: usize) -> bool {
        if self.budget.max_jobs == 0 || bytes > self.budget.max_bytes {
            return false;
        }

        match &self.frame_budget {
            Some(frame) => frame.max_jobs > 0 && bytes <= frame.max_bytes,
            None => true,
        }
    }

    pub(crate) fn finish_job(&mut self, job: &UploadJob) {
        self.finish_job_bytes(job.byte_len());
    }

    /// Release budget by byte count directly (avoids reconstructing an UploadJob).
    pub(crate) fn finish_job_bytes(&mut self, byte_len: usize) {
        self.queue_depth = self.queue_depth.saturating_sub(1);
        self.budget.release(byte_len);
    }

    /// Recover budget for a single upload that completed but whose result
    /// could not be delivered to the render thread.
    pub(crate) fn recover_dropped(&mut self, dropped: &DroppedUpload) {
        self.queue_depth = self.queue_depth.saturating_sub(1);
        self.budget.release(dropped.byte_len);
    }
}

#[cfg(test)]
pub(crate) fn assert_defers_jobs_when_budget_is_exhausted() {
    let mut server = UploadServer::new(1, 1024);
    let first_job = UploadJob {
        image_id: 1,
        width: 16,
        height: 8,
        rgba: Arc::new(vec![0; 512]),
    };

    assert!(server.try_acquire_job(&first_job));
    assert_eq!(server.queue_depth(), 1);
    assert_eq!(server.budget().jobs_left(), 0);
    assert_eq!(server.budget().bytes_left(), 512);

    assert!(!server.try_acquire(768));
    assert_eq!(server.queue_depth(), 1);

    server.finish_job(&first_job);
    assert_eq!(server.queue_depth(), 0);
    assert_eq!(server.budget().jobs_left(), 1);
    assert_eq!(server.budget().bytes_left(), 1024);
    assert!(server.try_acquire(768));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device_caps::DeviceCapabilities;

    #[test]
    fn defers_jobs_when_budget_is_exhausted() {
        assert_defers_jobs_when_budget_is_exhausted();
    }

    #[test]
    fn device_defaults_follow_conservative_render_profile() {
        let caps = DeviceCapabilities {
            has_parallel_shader_compile: false,
            has_color_buffer_float: false,
            has_float_blend: false,
            has_anisotropic_filtering: false,
            has_color_buffer_half_float: false,
            has_float_filtering: false,
            has_standard_derivatives: false,
            has_shader_texture_lod: false,
            has_frag_depth: false,
            gles_version: (2, 0),
            has_pbo: false,
            has_fence_sync: false,
            has_compute: false,
            ahb_available: false,
            has_buffer_age: false,
            has_ext_buffer_age: false,
            has_partial_update: false,
            compressed_format_support: crate::compressed_upload::CompressedFormatSupport {
                etc2: false,
                astc: false,
            },
        };

        let server = UploadServer::for_device(&caps, 23);

        assert_eq!(server.budget().jobs_left(), 1);
        assert_eq!(server.budget().bytes_left(), 512 * 1024);
    }

    /// Simulates the live path pattern: CanvasManager calls try_acquire_job
    /// before submit, and finish_job when the upload fence signals.
    /// Conservative TierB profile (1 job, 512KB) must reject the second job.
    #[test]
    fn conservative_profile_rejects_second_concurrent_upload() {
        let caps = DeviceCapabilities {
            has_parallel_shader_compile: false,
            has_color_buffer_float: false,
            has_float_blend: false,
            has_anisotropic_filtering: false,
            has_color_buffer_half_float: false,
            has_float_filtering: false,
            has_standard_derivatives: false,
            has_shader_texture_lod: false,
            has_frag_depth: false,
            gles_version: (2, 0),
            has_pbo: false,
            has_fence_sync: false,
            has_compute: false,
            ahb_available: false,
            has_buffer_age: false,
            has_ext_buffer_age: false,
            has_partial_update: false,
            compressed_format_support: crate::compressed_upload::CompressedFormatSupport {
                etc2: false,
                astc: false,
            },
        };
        let mut server = UploadServer::for_device(&caps, 23);

        let job_a = UploadJob {
            image_id: 1,
            width: 64,
            height: 64,
            rgba: Arc::new(vec![0; 64 * 64 * 4]),
        };
        let job_b = UploadJob {
            image_id: 2,
            width: 32,
            height: 32,
            rgba: Arc::new(vec![0; 32 * 32 * 4]),
        };

        // First job fits within conservative budget (1 job, 512KB).
        assert!(server.try_acquire_job(&job_a));
        assert_eq!(server.queue_depth(), 1);

        // Second job must be rejected — budget allows only 1 concurrent.
        assert!(!server.try_acquire_job(&job_b));
        assert_eq!(server.queue_depth(), 1);

        // After first completes + frame boundary, second should succeed.
        server.finish_job(&job_a);
        server.reset_frame_budget(); // simulate next frame
        assert_eq!(server.queue_depth(), 0);
        assert!(server.try_acquire_job(&job_b));
    }

    /// Aggressive TierA profile (4 jobs, 4MB) must allow multiple concurrent uploads.
    #[test]
    fn aggressive_profile_allows_multiple_concurrent_uploads() {
        let caps = DeviceCapabilities {
            has_parallel_shader_compile: false,
            has_color_buffer_float: false,
            has_float_blend: false,
            has_anisotropic_filtering: false,
            has_color_buffer_half_float: false,
            has_float_filtering: false,
            has_standard_derivatives: false,
            has_shader_texture_lod: false,
            has_frag_depth: false,
            gles_version: (3, 0),
            has_pbo: true,
            has_fence_sync: true,
            has_compute: false,
            ahb_available: false,
            has_buffer_age: false,
            has_ext_buffer_age: false,
            has_partial_update: false,
            compressed_format_support: crate::compressed_upload::CompressedFormatSupport {
                etc2: false,
                astc: false,
            },
        };
        let mut server = UploadServer::for_device(&caps, 28);

        let make_job = |id: u64, size: usize| UploadJob {
            image_id: id,
            width: 64,
            height: 64,
            rgba: Arc::new(vec![0; size]),
        };

        // TierA budget: 4 jobs, 4MB.
        let jobs: Vec<_> = (0..4).map(|i| make_job(i, 256 * 1024)).collect();
        for job in &jobs {
            assert!(
                server.try_acquire_job(job),
                "job {} should be accepted",
                job.image_id
            );
        }
        assert_eq!(server.queue_depth(), 4);

        // 5th job must be rejected (job count exhausted).
        let fifth = make_job(4, 256 * 1024);
        assert!(!server.try_acquire_job(&fifth));

        // Completing two jobs + frame boundary restores capacity for more.
        server.finish_job(&jobs[0]);
        server.finish_job(&jobs[1]);
        server.reset_frame_budget(); // simulate next frame
        assert_eq!(server.queue_depth(), 2);
        assert!(server.try_acquire_job(&fifth));
    }

    /// Budget must also reject a single job that exceeds max_bytes.
    #[test]
    fn oversized_single_job_rejected_by_byte_budget() {
        let caps = DeviceCapabilities {
            has_parallel_shader_compile: false,
            has_color_buffer_float: false,
            has_float_blend: false,
            has_anisotropic_filtering: false,
            has_color_buffer_half_float: false,
            has_float_filtering: false,
            has_standard_derivatives: false,
            has_shader_texture_lod: false,
            has_frag_depth: false,
            gles_version: (2, 0),
            has_pbo: false,
            has_fence_sync: false,
            has_compute: false,
            ahb_available: false,
            has_buffer_age: false,
            has_ext_buffer_age: false,
            has_partial_update: false,
            compressed_format_support: crate::compressed_upload::CompressedFormatSupport {
                etc2: false,
                astc: false,
            },
        };
        // Conservative: 1 job, 512KB
        let mut server = UploadServer::for_device(&caps, 23);

        let huge_job = UploadJob {
            image_id: 1,
            width: 1024,
            height: 1024,
            rgba: Arc::new(vec![0; 1024 * 1024 * 4]), // 4MB > 512KB
        };
        assert!(!server.try_acquire_job(&huge_job));
        assert_eq!(server.queue_depth(), 0);
    }

    /// finish_job must not over-replenish budget beyond max values.
    #[test]
    fn finish_job_clamps_budget_to_max() {
        let mut server = UploadServer::new(2, 2048);
        let job = UploadJob {
            image_id: 1,
            width: 16,
            height: 16,
            rgba: Arc::new(vec![0; 512]),
        };

        assert!(server.try_acquire_job(&job));
        server.finish_job(&job);
        // Double finish must not exceed max.
        server.finish_job(&job);

        assert_eq!(server.budget().jobs_left(), 2);
        assert_eq!(server.budget().bytes_left(), 2048);
    }

    /// Simulates the budget leak scenario: upload completes on the upload
    /// thread but result_tx.try_send fails (channel full/disconnected).
    /// The upload thread sends a DroppedUpload per item through a dedicated
    /// channel. CanvasManager drains it and recovers budget per-item.
    #[test]
    fn dropped_result_budget_is_recoverable_via_drain() {
        use crate::upload_thread::DroppedUpload;

        let mut server = UploadServer::new(2, 8192);
        let job_a = UploadJob {
            image_id: 1,
            width: 32,
            height: 32,
            rgba: Arc::new(vec![0; 32 * 32 * 4]), // 4096 bytes
        };
        let job_b = UploadJob {
            image_id: 2,
            width: 16,
            height: 16,
            rgba: Arc::new(vec![0; 16 * 16 * 4]), // 1024 bytes
        };

        // Acquire budget for both jobs (total 5120 bytes from 8192 budget).
        assert!(server.try_acquire_job(&job_a));
        assert!(server.try_acquire_job(&job_b));
        assert_eq!(server.queue_depth(), 2);
        assert_eq!(server.budget().jobs_left(), 0);
        assert_eq!(server.budget().bytes_left(), 8192 - 4096 - 1024);

        // Simulate: job_a completed but result channel failed.
        // The upload thread sends a single DroppedUpload with image_id.
        let drop_a = DroppedUpload {
            image_id: job_a.image_id,
            byte_len: job_a.byte_len(),
        };

        // CanvasManager processes the dropped item: recovers budget AND
        // can use image_id to resolve the pending_load_response.
        server.recover_dropped(&drop_a);
        assert_eq!(server.queue_depth(), 1);
        assert_eq!(server.budget().bytes_left(), 7168);
        assert_eq!(server.budget().jobs_left(), 1);

        // job_b completes normally.
        server.finish_job(&job_b);
        assert_eq!(server.queue_depth(), 0);
        assert_eq!(server.budget().jobs_left(), 2);
        assert_eq!(server.budget().bytes_left(), 8192);
    }

    // ---- Per-frame budget tests ----

    /// Within a single frame, uploads beyond the per-frame budget are rejected
    /// even if in-flight slots are available.
    #[test]
    fn frame_budget_rejects_excess_uploads_within_same_frame() {
        // in-flight: 4 jobs, 4MB.  per-frame: 2 jobs, 1MB.
        let mut server = UploadServer::new(4, 4 * 1024 * 1024);
        server.set_frame_budget(2, 1024 * 1024);

        let small = |id| UploadJob {
            image_id: id,
            width: 16,
            height: 16,
            rgba: Arc::new(vec![0; 256 * 1024]), // 256KB
        };

        // First two pass both layers.
        assert!(server.try_acquire_job(&small(1)));
        assert!(server.try_acquire_job(&small(2)));
        // Third is blocked by per-frame (2 jobs used), even though in-flight has room.
        assert!(!server.try_acquire_job(&small(3)));
        assert_eq!(server.frame_rejections(), 1);
    }

    /// After reset_frame_budget(), new uploads in the next frame pass again.
    #[test]
    fn frame_budget_resets_at_frame_boundary() {
        let mut server = UploadServer::new(4, 4 * 1024 * 1024);
        server.set_frame_budget(2, 1024 * 1024);

        let small = |id| UploadJob {
            image_id: id,
            width: 16,
            height: 16,
            rgba: Arc::new(vec![0; 256 * 1024]),
        };

        assert!(server.try_acquire_job(&small(1)));
        assert!(server.try_acquire_job(&small(2)));
        assert!(!server.try_acquire_job(&small(3))); // frame budget exhausted

        // Simulate frame boundary — previous two are still in-flight.
        server.reset_frame_budget();
        // Now per-frame allows 2 more, and in-flight has 2 of 4 used.
        assert!(server.try_acquire_job(&small(3)));
        assert!(server.try_acquire_job(&small(4)));
        // In-flight full (4), so even though frame budget has room:
        assert!(!server.try_acquire_job(&small(5)));
    }

    /// Completion (finish_job) restores in-flight budget but does NOT
    /// restore per-frame budget — frame budget is a one-way countdown.
    #[test]
    fn completion_restores_inflight_but_not_frame_budget() {
        let mut server = UploadServer::new(4, 4 * 1024 * 1024);
        server.set_frame_budget(2, 1024 * 1024);

        let small = |id| UploadJob {
            image_id: id,
            width: 16,
            height: 16,
            rgba: Arc::new(vec![0; 256 * 1024]),
        };

        assert!(server.try_acquire_job(&small(1)));
        assert!(server.try_acquire_job(&small(2)));
        // Frame budget exhausted.
        assert!(!server.try_acquire_job(&small(3)));

        // Job 1 completes — restores in-flight slot.
        server.finish_job(&small(1));
        assert_eq!(server.queue_depth(), 1);
        // But frame budget is still exhausted — still rejected.
        assert!(!server.try_acquire_job(&small(3)));
        assert_eq!(server.frame_rejections(), 2);
    }

    /// Per-frame byte budget also gates large uploads within a frame.
    #[test]
    fn frame_byte_budget_rejects_oversized_upload() {
        let mut server = UploadServer::new(4, 4 * 1024 * 1024);
        server.set_frame_budget(4, 512 * 1024); // 4 jobs, but only 512KB/frame

        let big = UploadJob {
            image_id: 1,
            width: 64,
            height: 64,
            rgba: Arc::new(vec![0; 400 * 1024]), // 400KB
        };
        assert!(server.try_acquire_job(&big)); // 400KB < 512KB, passes

        let second = UploadJob {
            image_id: 2,
            width: 64,
            height: 64,
            rgba: Arc::new(vec![0; 200 * 1024]), // 200KB, total would be 600KB > 512KB
        };
        assert!(!server.try_acquire_job(&second));
    }

    #[test]
    fn oversized_job_can_never_fit_frame_budget() {
        let mut server = UploadServer::new(4, 4 * 1024 * 1024);
        server.set_frame_budget(2, 512 * 1024);

        assert!(!server.can_ever_fit_bytes(600 * 1024));
        assert!(server.can_ever_fit_bytes(400 * 1024));
    }

    #[test]
    fn temporary_frame_exhaustion_still_reports_job_as_fit_capable() {
        let mut server = UploadServer::new(4, 4 * 1024 * 1024);
        server.set_frame_budget(2, 1024 * 1024);

        let small = |id| UploadJob {
            image_id: id,
            width: 16,
            height: 16,
            rgba: Arc::new(vec![0; 256 * 1024]),
        };

        assert!(server.try_acquire_job(&small(1)));
        assert!(server.try_acquire_job(&small(2)));
        assert!(!server.try_acquire_job(&small(3)));

        assert!(server.can_ever_fit_bytes(256 * 1024));
    }

    #[test]
    fn oversized_and_temporary_rejections_split_differently() {
        let mut server = UploadServer::new(4, 4 * 1024 * 1024);
        server.set_frame_budget(2, 512 * 1024);

        assert!(!server.can_ever_fit_bytes(600 * 1024));
        assert!(server.can_ever_fit_bytes(256 * 1024));
    }

    /// Dropped upload recovery only restores in-flight, not frame budget.
    #[test]
    fn dropped_upload_does_not_restore_frame_budget() {
        use crate::upload_thread::DroppedUpload;

        let mut server = UploadServer::new(4, 4 * 1024 * 1024);
        server.set_frame_budget(2, 1024 * 1024);

        let small = |id| UploadJob {
            image_id: id,
            width: 16,
            height: 16,
            rgba: Arc::new(vec![0; 256 * 1024]),
        };

        assert!(server.try_acquire_job(&small(1)));
        assert!(server.try_acquire_job(&small(2)));
        assert!(!server.try_acquire_job(&small(3))); // frame exhausted

        // Job 1 dropped — in-flight recovered, frame budget NOT.
        server.recover_dropped(&DroppedUpload {
            image_id: 1,
            byte_len: 256 * 1024,
        });
        assert_eq!(server.queue_depth(), 1);
        assert!(!server.try_acquire_job(&small(3))); // still frame-blocked

        // Next frame resets.
        server.reset_frame_budget();
        assert!(server.try_acquire_job(&small(3)));
    }
}

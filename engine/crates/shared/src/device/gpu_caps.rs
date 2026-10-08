//! Per-session GPU image-path capabilities.
//!
//! `GpuCaps` is created by the host thread and shared with the render
//! thread via `Arc`.  The render thread calls `set()` after GL context
//! init; IO/JS threads read via `snapshot()`.
//!
//! `GpuCapsSnapshot` is a plain `Copy` point-in-time view. On-demand image
//! decodes retain the shared `GpuCaps` until their worker starts so the
//! one-way runtime AHB circuit breaker cannot be bypassed by queued work;
//! stable capability consumers such as preloading may still pass snapshots.
//!
//! `wait_ready()` blocks until the render thread has completed GL init
//! and called `set()`.  `Host::new()` calls this to ensure caps are
//! populated before any JS code can issue image loads.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};

fn remaining_wait(
    started: std::time::Instant,
    timeout: std::time::Duration,
) -> std::time::Duration {
    timeout.saturating_sub(started.elapsed())
}

#[derive(Debug, Clone)]
pub enum GpuCapsReadyState {
    Ready,
    Failed(String),
    Timeout,
}

/// Per-session GPU image-path capabilities.
///
/// Created once per host session, shared with the render thread.
/// The render thread writes after GL context init (via `set`);
/// IO/JS threads read (via `snapshot`).
#[derive(Debug)]
pub struct GpuCaps {
    etc2: AtomicBool,
    astc: AtomicBool,
    ahb: AtomicBool,
    color_buffer_float: AtomicBool,
    /// Set to `true` after `set()` is called.  `wait_ready()` blocks
    /// until this flag is true, ensuring no early snapshot reads
    /// uninitialized (all-false) caps.
    ready: AtomicBool,
    failed: AtomicBool,
    ready_lock: Mutex<bool>,
    ready_cv: Condvar,
    failure_detail: Mutex<Option<String>>,
}

impl Default for GpuCaps {
    fn default() -> Self {
        Self {
            etc2: AtomicBool::new(false),
            astc: AtomicBool::new(false),
            ahb: AtomicBool::new(false),
            color_buffer_float: AtomicBool::new(false),
            ready: AtomicBool::new(false),
            failed: AtomicBool::new(false),
            ready_lock: Mutex::new(false),
            ready_cv: Condvar::new(),
            failure_detail: Mutex::new(None),
        }
    }
}

impl GpuCaps {
    /// Create a new `Arc<GpuCaps>` with every capability defaulting to `false`.
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Called by the render thread after GL context init, with every capability at once.
    pub fn set(&self, caps: GpuCapsSnapshot) {
        self.etc2.store(caps.etc2, Ordering::Release);
        self.astc.store(caps.astc, Ordering::Release);
        self.ahb.store(caps.ahb, Ordering::Release);
        self.color_buffer_float
            .store(caps.color_buffer_float, Ordering::Release);
        self.ready.store(true, Ordering::Release);
        if let Ok(mut ready) = self.ready_lock.lock() {
            *ready = true;
            self.ready_cv.notify_all();
        }
    }

    /// Permanently disable AHB for this host session after a runtime import
    /// failure. Returns `true` only for the first enabled-to-disabled edge.
    /// Capability publication is complete before image work starts, so this
    /// flag is intentionally one-way and cannot race with a later `set()`.
    pub fn disable_ahb(&self) -> bool {
        self.ahb.swap(false, Ordering::AcqRel)
    }

    pub fn set_failed(&self, detail: impl Into<String>) {
        let detail = detail.into();
        if let Ok(mut failure) = self.failure_detail.lock() {
            *failure = Some(detail);
        }
        self.failed.store(true, Ordering::Release);
        self.ready.store(true, Ordering::Release);
        if let Ok(mut ready) = self.ready_lock.lock() {
            *ready = true;
            self.ready_cv.notify_all();
        }
    }

    /// Whether `set()` has been called.
    pub fn is_ready(&self) -> bool {
        self.ready.load(Ordering::Acquire)
    }

    /// Block until `set()` has been called by the render thread.
    ///
    pub fn wait_ready(&self, timeout: std::time::Duration) -> GpuCapsReadyState {
        if self.ready.load(Ordering::Acquire) {
            if self.failed.load(Ordering::Acquire) {
                let detail = self
                    .failure_detail
                    .lock()
                    .ok()
                    .and_then(|g| g.clone())
                    .unwrap_or_else(|| "GPU caps initialization failed".to_string());
                return GpuCapsReadyState::Failed(detail);
            }
            return GpuCapsReadyState::Ready;
        }
        let Ok(ready) = self.ready_lock.lock() else {
            return GpuCapsReadyState::Timeout;
        };
        if *ready {
            if self.failed.load(Ordering::Acquire) {
                let detail = self
                    .failure_detail
                    .lock()
                    .ok()
                    .and_then(|g| g.clone())
                    .unwrap_or_else(|| "GPU caps initialization failed".to_string());
                return GpuCapsReadyState::Failed(detail);
            }
            return GpuCapsReadyState::Ready;
        }
        match self.ready_cv.wait_timeout_while(ready, timeout, |r| !*r) {
            Ok((guard, _)) if *guard => {
                if self.failed.load(Ordering::Acquire) {
                    let detail = self
                        .failure_detail
                        .lock()
                        .ok()
                        .and_then(|g| g.clone())
                        .unwrap_or_else(|| "GPU caps initialization failed".to_string());
                    GpuCapsReadyState::Failed(detail)
                } else {
                    GpuCapsReadyState::Ready
                }
            }
            Ok(_) | Err(_) => GpuCapsReadyState::Timeout,
        }
    }

    /// Wait for initial capability publication without extending an existing
    /// startup deadline. `started` is captured by the host immediately after
    /// render service launch; work before this call consumes the same budget.
    pub fn wait_ready_until(
        &self,
        started: std::time::Instant,
        timeout: std::time::Duration,
    ) -> GpuCapsReadyState {
        self.wait_ready(remaining_wait(started, timeout))
    }

    /// Take an immutable point-in-time snapshot for passing to image decode functions.
    pub fn snapshot(&self) -> GpuCapsSnapshot {
        GpuCapsSnapshot {
            etc2: self.etc2.load(Ordering::Acquire),
            astc: self.astc.load(Ordering::Acquire),
            ahb: self.ahb.load(Ordering::Acquire),
            color_buffer_float: self.color_buffer_float.load(Ordering::Acquire),
        }
    }

    /// The bits `op_webgl_query_gpu_caps` answers content's WebGL with: 0 ETC2/EAC, 1 ASTC LDR, 2 float colour
    /// buffers (EXT_color_buffer_float). The embedded op and the external session's service both answer these.
    pub fn webgl_bits(&self) -> u32 {
        let caps = self.snapshot();
        u32::from(caps.etc2)
            | (u32::from(caps.astc) << 1)
            | (u32::from(caps.color_buffer_float) << 2)
    }
}

/// Immutable point-in-time copy of GPU caps.
///
/// Stable consumers may pass this directly. On-demand decode jobs should keep
/// the shared [`GpuCaps`] until worker execution and snapshot there so they
/// observe a runtime AHB disable edge.
#[derive(Debug, Clone, Copy, Default)]
pub struct GpuCapsSnapshot {
    pub etc2: bool,
    pub astc: bool,
    /// The renderer can import an API-26 AHardwareBuffer as a texture.
    pub ahb: bool,
    /// The float formats are colour-renderable (`GL_EXT_color_buffer_float`).
    pub color_buffer_float: bool,
}

#[cfg(test)]
mod tests {
    use super::{GpuCaps, GpuCapsReadyState, GpuCapsSnapshot, remaining_wait};
    use std::time::{Duration, Instant};

    #[test]
    fn wait_ready_observes_set() {
        let caps = GpuCaps::new();
        let worker = caps.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(10));
            worker.set(GpuCapsSnapshot {
                etc2: true,
                astc: false,
                ahb: true,
                color_buffer_float: false,
            });
        });
        assert!(matches!(
            caps.wait_ready(Duration::from_secs(1)),
            GpuCapsReadyState::Ready
        ));
        let snap = caps.snapshot();
        assert!(snap.etc2);
        assert!(!snap.astc);
        assert!(snap.ahb);
    }

    #[test]
    fn wait_ready_times_out_when_unset() {
        let caps = GpuCaps::new();
        assert!(matches!(
            caps.wait_ready(Duration::from_millis(1)),
            GpuCapsReadyState::Timeout
        ));
    }

    #[test]
    fn wait_ready_returns_failure() {
        let caps = GpuCaps::new();
        caps.set_failed("boom");
        match caps.wait_ready(Duration::from_millis(1)) {
            GpuCapsReadyState::Failed(msg) => assert!(msg.contains("boom")),
            other => panic!("expected failure, got {other:?}"),
        }
    }

    #[test]
    fn wait_ready_until_observes_already_published_caps_after_deadline() {
        let caps = GpuCaps::new();
        caps.set(GpuCapsSnapshot {
            etc2: true,
            astc: false,
            ahb: true,
            color_buffer_float: false,
        });
        let started = Instant::now() - Duration::from_secs(1);

        assert!(matches!(
            caps.wait_ready_until(started, Duration::from_millis(10)),
            GpuCapsReadyState::Ready
        ));
    }

    #[test]
    fn wait_ready_until_observes_already_published_failure_after_deadline() {
        let caps = GpuCaps::new();
        caps.set_failed("render init failed");
        let started = Instant::now() - Duration::from_secs(1);

        match caps.wait_ready_until(started, Duration::from_millis(10)) {
            GpuCapsReadyState::Failed(detail) => {
                assert_eq!(detail, "render init failed");
            }
            other => panic!("expected failure, got {other:?}"),
        }
    }

    #[test]
    fn wait_ready_until_expired_deadline_does_not_start_a_new_timeout() {
        let caps = GpuCaps::new();
        let started = Instant::now() - Duration::from_secs(1);

        assert!(matches!(
            caps.wait_ready_until(started, Duration::from_millis(10)),
            GpuCapsReadyState::Timeout
        ));
        assert_eq!(
            remaining_wait(started, Duration::from_millis(10)),
            Duration::ZERO
        );
    }

    #[test]
    fn wait_ready_until_uses_only_the_remaining_budget() {
        let caps = GpuCaps::new();
        let publisher = caps.clone();
        let started = Instant::now();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(10));
            publisher.set(GpuCapsSnapshot {
                etc2: false,
                astc: true,
                ahb: false,
                color_buffer_float: false,
            });
        });

        assert!(matches!(
            caps.wait_ready_until(started, Duration::from_secs(1)),
            GpuCapsReadyState::Ready
        ));
        assert!(caps.snapshot().astc);
    }

    #[test]
    fn ahb_can_be_disabled_after_a_runtime_import_failure() {
        let caps = GpuCaps::new();
        caps.set(GpuCapsSnapshot {
            etc2: false,
            astc: false,
            ahb: true,
            color_buffer_float: false,
        });
        assert!(caps.snapshot().ahb);
        assert!(caps.disable_ahb());
        assert!(!caps.snapshot().ahb);
        assert!(!caps.disable_ahb(), "second disable is idempotent");
    }
}

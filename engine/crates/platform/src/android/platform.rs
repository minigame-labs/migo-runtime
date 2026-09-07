use std::sync::Arc;

use migo_core::services::DeviceServices;
use migo_core::{DeviceServiceProvider, FrameClock, HostNotifier};
use tracing::error;

use crate::android::jni;
use crate::android::services::AndroidDeviceServices;

pub struct AndroidPlatform {
    host_keyboard: Option<Arc<dyn migo_core::services::KeyboardService>>,
}

impl AndroidPlatform {
    pub fn new() -> Self {
        Self::with_host_keyboard(None)
    }

    /// Build a platform that prefers a host-supplied keyboard over the JNI one.
    ///
    /// `AndroidKeyboard` reaches the Java SDK over JNI, and its accessor claims
    /// support unconditionally -- a claim that is false for a pure-native host,
    /// which has no JVM to reach. A host that supplied its own therefore wins,
    /// for the same reason `uses_external_vsync` reports what the host
    /// installed rather than what this platform would prefer.
    pub fn with_host_keyboard(
        host_keyboard: Option<Arc<dyn migo_core::services::KeyboardService>>,
    ) -> Self {
        Self { host_keyboard }
    }
}

impl DeviceServiceProvider for AndroidPlatform {
    fn create_device_services(&self, host_id: i32) -> Option<Arc<dyn DeviceServices>> {
        Some(Arc::new(AndroidDeviceServices::with_host_keyboard(
            host_id,
            self.host_keyboard.clone(),
        )))
    }
}

impl FrameClock for AndroidPlatform {
    fn uses_external_vsync(&self) -> bool {
        true
    }

    fn request_vsync(&self, host_id: i32) {
        // R1: route to `NativeExports.requestVsync`, which hops to the main
        // thread and arms one Choreographer callback. Failures here are benign
        // (idle races / session torn down), so log at debug and drop — never
        // escalate to notify_error and never flood on the hot path.
        if let Err(e) = jni::request_vsync(host_id) {
            tracing::debug!("[Host {}] request_vsync JNI call failed: {}", host_id, e);
        }
    }
}

impl HostNotifier for AndroidPlatform {
    fn notify_game_ready(&self, host_id: i32) {
        if let Err(e) = jni::notify_game_ready(host_id) {
            error!(
                "[Host {}] Failed to notify Java of game ready: {}",
                host_id, e
            );
        }
    }

    fn notify_exit(&self, host_id: i32) {
        if let Err(e) = jni::notify_exit(host_id) {
            error!("[Host {}] Failed to notify Java of exit: {}", host_id, e);
        }
    }

    fn notify_error(&self, host_id: i32, error_code: u16, message: &str, detail: &str) {
        if let Err(e) = jni::notify_error(host_id, error_code, message, detail) {
            error!(
                "[Host {}] Failed to notify Java of error (code={}): {}",
                host_id, error_code, e
            );
        }
    }

    fn notify_host_message(&self, host_id: i32, json: &str) {
        if let Err(e) = jni::notify_host_message(host_id, json) {
            error!(
                "[Host {}] Failed to deliver host message to Java: {}",
                host_id, e
            );
        }
    }

    fn notify_surface_lost(
        &self,
        host_id: i32,
        public_generation: shared::surface::PublicSurfaceGeneration,
        reason: shared::surface::SurfaceLossReason,
    ) {
        if let Err(e) = jni::notify_surface_lost(host_id, public_generation.get(), reason.as_u32())
        {
            error!(
                "[Host {host_id}] Failed to notify Java of Surface loss \
                 (generation={}, reason={reason:?}): {e}",
                public_generation.get()
            );
        }
    }
}

impl migo_core::RuntimeGenerationNotifier for AndroidPlatform {
    fn begin_runtime_restart(&self, host_id: i32, retired: i64, next: i64) {
        if let Err(e) = jni::begin_runtime_restart(host_id, retired, next) {
            // Logged rather than propagated: the restart is already under way and
            // there is nothing useful to do with a failure here. The Java side
            // stays at `retired` and keeps issuing tokens for it, which the Host
            // then drops at dispatch -- the fail-closed direction.
            error!("[Host {host_id}] begin_runtime_restart({retired} -> {next}) failed: {e}");
        }
    }

    fn complete_runtime_restart(&self, host_id: i32, next: i64) {
        if let Err(e) = jni::complete_runtime_restart(host_id, next) {
            error!("[Host {host_id}] complete_runtime_restart({next}) failed: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn android_declares_external_vsync_source() {
        assert!(AndroidPlatform::new().uses_external_vsync());
    }
}

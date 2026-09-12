//! Camera service trait for platform camera access.

use crate::protocol::error::ServiceError;

/// Camera service for photo capture, video recording, and frame streaming.
///
/// Platforms implement this trait to provide camera capabilities.
/// Commands that return data use JSON strings for flexibility.
/// Fire-and-forget commands return `Result<(), ServiceError>`.
///
/// Event delivery (frame data, stop, auth cancel) is handled by the platform
/// pushing events back to the JS runtime via `_internalOnCameraEvent` and
/// `_internalOnCameraFrameData`.
pub trait CameraService: Send + Sync {
    /// Create a camera instance with the given configuration (JSON).
    ///
    /// JSON fields:
    /// - `cameraId`: u32 - JS-assigned camera instance ID
    /// - `x`: number (default 0) - left x coordinate
    /// - `y`: number (default 0) - top y coordinate
    /// - `width`: number (default 300) - camera width
    /// - `height`: number (default 150) - camera height
    /// - `devicePosition`: "back" | "front" (default "back")
    /// - `flash`: "auto" | "on" | "off" (default "auto")
    /// - `size`: "small" | "medium" | "large" (default "small")
    ///
    /// Returns JSON: `{"cameraId": <id>}` on success.
    fn create(&self, _options_json: &str) -> Result<String, ServiceError> {
        Err(ServiceError::not_supported(
            "createCamera:fail not supported",
        ))
    }

    /// Destroy a camera instance and release all resources.
    fn destroy(&self, _camera_id: u32) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "camera.destroy:fail not supported",
        ))
    }

    /// Take a photo (fire-and-forget async variant).
    ///
    /// The platform submits the capture request and returns immediately.
    /// The result arrives asynchronously via `CameraEvent("takePhotoResult", ...)`,
    /// with the `request_id` echoed back inside the payload so JS can resolve the
    /// waiting promise.
    ///
    /// JSON fields (same as the retired sync variant, plus `requestId`):
    /// - `cameraId`: u32
    /// - `quality`: "high" | "normal" | "low" (default "normal")
    /// - `requestId`: u32 — echoed verbatim in the `takePhotoResult` payload
    ///
    /// On platforms that have not yet implemented this method the default
    /// returns "not supported" so existing callers keep failing gracefully
    /// rather than silently doing nothing.
    fn take_photo_async(&self, _request_id: u32, _options_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "camera.takePhoto:fail not supported",
        ))
    }

    /// Start video recording.
    ///
    /// JSON fields:
    /// - `cameraId`: u32
    ///
    /// Returns JSON: `{}` on success (recording started).
    fn start_record(&self, _options_json: &str) -> Result<String, ServiceError> {
        Err(ServiceError::not_supported(
            "camera.startRecord:fail not supported",
        ))
    }

    /// Stop video recording.
    ///
    /// JSON fields:
    /// - `cameraId`: u32
    /// - `compressed`: bool (default false)
    ///
    /// Returns JSON: `{"tempThumbPath": "<path>", "tempVideoPath": "<path>"}` on success.
    fn stop_record(&self, _options_json: &str) -> Result<String, ServiceError> {
        Err(ServiceError::not_supported(
            "camera.stopRecord:fail not supported",
        ))
    }

    /// Set camera zoom level.
    ///
    /// JSON fields:
    /// - `cameraId`: u32
    /// - `zoom`: number (zoom factor)
    ///
    /// Returns JSON: `{"zoom": <actual_zoom>}` on success.
    fn set_zoom(&self, _options_json: &str) -> Result<String, ServiceError> {
        Err(ServiceError::not_supported(
            "camera.setZoom:fail not supported",
        ))
    }

    /// Start listening for camera frame changes (high-frequency streaming).
    ///
    /// After calling this, the platform should continuously push frame data via
    /// `_internalOnCameraFrameData(cameraId, arrayBuffer, width, height)`.
    fn listen_frame_change(&self, _camera_id: u32) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "camera.listenFrameChange:fail not supported",
        ))
    }

    /// Stop listening for camera frame changes.
    fn close_frame_change(&self, _camera_id: u32) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "camera.closeFrameChange:fail not supported",
        ))
    }
}

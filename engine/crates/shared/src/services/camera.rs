//! The camera: `createCamera`'s preview, frames, photos, video and zoom.

use crate::protocol::error::ServiceError;
use crate::services::host_files::HostFiles;

/// The camera: its preview shown over the game, its frames, photos, video and
/// zoom.
///
/// Every method takes the request content made as JSON -- the camera's
/// `cameraId`, the method's options, and `requestId` for a call. A call is
/// answered through the hook [`CAMERA_RESULT_HOOKS`] names for its method
/// number, with the result or with `{"error"}`, once what it asked has
/// happened: `create` once the preview shows, `stop_record` once the video is
/// written. A command is carried out and answered by nothing.
///
/// What happens to a camera on its own -- it stops (the app went to the
/// background, another app took the device), the player refuses the system's
/// permission -- reaches content through [`CAMERA_EVENT_HOOKS`], each with
/// `{"cameraId"}`. Frames, while content listens for them, are a typed path of
/// their own: straight RGBA, as content reads them
/// (`protocol::camera_frame`).
pub trait CameraService: Send + Sync {
    /// Open camera `cameraId` -- `devicePosition` "back" or "front" -- and show
    /// its preview over the game at `x`, `y`, `width` x `height` in the game's
    /// logical pixels, covering it, with `flash` ("auto", "on", "off") and
    /// frames of `size` ("small", "medium", "large"). A call, answered `{}` once
    /// the preview shows; one that fails is followed by `destroy`.
    fn create(&self, _request_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "createCamera:fail not supported",
        ))
    }

    /// Close camera `cameraId` and take its preview away. A command.
    fn destroy(&self, _request_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "camera.destroy:fail not supported",
        ))
    }

    /// What content changed on camera `cameraId`: any of `x`, `y`, `width`,
    /// `height`, `devicePosition`, `flash` and `size`. A command.
    fn set(&self, _request_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported("camera:fail not supported"))
    }

    /// Take a photo at `quality` ("high", "normal", "low"). A call, answered
    /// `{"tempImagePath", "width", "height"}`, the file handed over.
    fn take_photo(&self, _request_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "camera.takePhoto:fail not supported",
        ))
    }

    /// Start recording video. A call, answered `{}` once it records.
    fn start_record(&self, _request_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "camera.startRecord:fail not supported",
        ))
    }

    /// Stop recording, `compressed` or not. A call, answered
    /// `{"tempThumbPath", "tempVideoPath"}` once the video is written, the
    /// files handed over.
    fn stop_record(&self, _request_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "camera.stopRecord:fail not supported",
        ))
    }

    /// Zoom to `zoom`, from 1. A call, answered `{"zoom"}` with the zoom the
    /// camera took.
    fn set_zoom(&self, _request_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "camera.setZoom:fail not supported",
        ))
    }

    /// Start handing content camera `cameraId`'s frames. A command.
    fn listen_frame_change(&self, _request_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "camera.listenFrameChange:fail not supported",
        ))
    }

    /// Stop handing content its frames. A command.
    fn close_frame_change(&self, _request_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "camera.closeFrameChange:fail not supported",
        ))
    }
}

/// The camera's methods by number (`MIGO_CAMERA_*` in
/// `include/migo/host_services.h`).
pub mod camera_method {
    pub const CREATE: u32 = 0;
    pub const DESTROY: u32 = 1;
    pub const SET: u32 = 2;
    pub const TAKE_PHOTO: u32 = 3;
    pub const START_RECORD: u32 = 4;
    pub const STOP_RECORD: u32 = 5;
    pub const SET_ZOOM: u32 = 6;
    pub const LISTEN_FRAME_CHANGE: u32 = 7;
    pub const CLOSE_FRAME_CHANGE: u32 = 8;
}

/// The hook each camera call is answered through, indexed by its method
/// number; a command has none. One table for every SDK: the C ABI's routing
/// and Android's result callback both read it.
pub const CAMERA_RESULT_HOOKS: [Option<&str>; 9] = [
    Some("_internalOnCreateCameraResult"),
    None,
    None,
    Some("_internalOnCameraTakePhotoResult"),
    Some("_internalOnCameraStartRecordResult"),
    Some("_internalOnCameraStopRecordResult"),
    Some("_internalOnCameraSetZoomResult"),
    None,
    None,
];

/// The files each camera call's answer hands over, by its method number: a
/// photo, a video and its cover.
pub const fn camera_result_files(method: u32) -> &'static HostFiles {
    match method {
        camera_method::TAKE_PHOTO => &HostFiles::CAMERA_TAKE_PHOTO,
        camera_method::STOP_RECORD => &HostFiles::CAMERA_STOP_RECORD,
        _ => &HostFiles::NONE,
    }
}

/// The hook each camera event reaches content through, indexed by its event
/// number (`MIGO_CAMERA_EVENT_*`): the camera stopped on its own, the player
/// refused the system's permission.
pub const CAMERA_EVENT_HOOKS: [&str; 2] = [
    "_internalOnCameraStopEvent",
    "_internalOnCameraAuthCancelEvent",
];

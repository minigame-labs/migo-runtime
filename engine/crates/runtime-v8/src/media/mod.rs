//! Media service ops (Camera, Image API, Video) and ESM modules.

use deno_core::{Extension, OpState, op2};
use deno_error::JsErrorBox;
use shared::op_state::HostOpState;
use shared::protocol::error::ServiceError;
use shared::services::{CameraService, ImageApiService, Scope, VideoService};

/// Look up the camera service and call `f` on it.
fn with_camera_service<F, T>(
    state: &mut OpState,
    err_msg: &'static str,
    f: F,
) -> Result<T, JsErrorBox>
where
    F: FnOnce(&dyn CameraService) -> Result<T, ServiceError>,
{
    let host = state.borrow::<HostOpState>();
    if let Some(ref services) = host.device_services {
        if let Some(svc) = services.camera() {
            return f(svc.as_ref()).map_err(JsErrorBox::generic);
        }
    }
    Err(JsErrorBox::generic(err_msg))
}

/// Look up the image API service and call `f` on it.
fn with_image_api<F, T>(state: &mut OpState, err_msg: &'static str, f: F) -> Result<T, JsErrorBox>
where
    F: FnOnce(&dyn ImageApiService) -> Result<T, ServiceError>,
{
    let host = state.borrow::<HostOpState>();
    if let Some(ref services) = host.device_services {
        if let Some(svc) = services.image_api() {
            return f(svc.as_ref()).map_err(JsErrorBox::generic);
        }
    }
    Err(JsErrorBox::generic(err_msg))
}

/// Look up the video service and call `f` on it.
fn with_video<F, T>(state: &mut OpState, err_msg: &'static str, f: F) -> Result<T, JsErrorBox>
where
    F: FnOnce(&dyn VideoService) -> Result<T, ServiceError>,
{
    let host = state.borrow::<HostOpState>();
    if let Some(ref services) = host.device_services {
        if let Some(svc) = services.video() {
            return f(svc.as_ref()).map_err(JsErrorBox::generic);
        }
    }
    Err(JsErrorBox::generic(err_msg))
}

// ==================== Camera Ops ====================

/// Create a camera instance. Returns JSON: `{"cameraId": <id>}`.
#[op2]
#[string]
pub fn op_camera_create(
    state: &mut OpState,
    #[string] options_json: &str,
) -> Result<String, JsErrorBox> {
    crate::permission::require_scope(state, Scope::Camera)?;
    with_camera_service(state, "createCamera:fail not supported", |c| {
        c.create(options_json)
    })
}

/// Destroy a camera instance and release all resources.
#[op2(fast)]
pub fn op_camera_destroy(state: &mut OpState, #[smi] camera_id: u32) -> Result<(), JsErrorBox> {
    with_camera_service(state, "camera.destroy:fail not supported", |c| {
        c.destroy(camera_id)
    })
}

/// Start a photo capture without blocking the V8 isolate.
///
/// The returned request id is echoed by the platform's `takePhotoResult`
/// camera event.  Keeping allocation here, before entering the platform
/// service, means every accepted request has a unique completion identity.
#[op2(fast)]
pub fn op_camera_take_photo(
    state: &mut OpState,
    #[string] options_json: &str,
) -> Result<u32, JsErrorBox> {
    crate::permission::require_scope(state, Scope::Camera)?;
    let request_id = state
        .borrow::<HostOpState>()
        .callback_ids
        .allocate()
        .map_err(|error| JsErrorBox::generic(error.to_string()))? as u32;
    with_camera_service(state, "camera.takePhoto:fail not supported", |c| {
        c.take_photo_async(request_id, options_json)?;
        Ok(request_id)
    })
}

/// Start video recording. Options as JSON.
#[op2]
#[string]
pub fn op_camera_start_record(
    state: &mut OpState,
    #[string] options_json: &str,
) -> Result<String, JsErrorBox> {
    crate::permission::require_scope(state, Scope::Camera)?;
    with_camera_service(state, "camera.startRecord:fail not supported", |c| {
        c.start_record(options_json)
    })
}

/// Stop video recording. Returns JSON with tempThumbPath, tempVideoPath.
#[op2]
#[string]
pub fn op_camera_stop_record(
    state: &mut OpState,
    #[string] options_json: &str,
) -> Result<String, JsErrorBox> {
    with_camera_service(state, "camera.stopRecord:fail not supported", |c| {
        c.stop_record(options_json)
    })
}

/// Set camera zoom level. Returns JSON with actual zoom applied.
#[op2]
#[string]
pub fn op_camera_set_zoom(
    state: &mut OpState,
    #[string] options_json: &str,
) -> Result<String, JsErrorBox> {
    crate::permission::require_scope(state, Scope::Camera)?;
    with_camera_service(state, "camera.setZoom:fail not supported", |c| {
        c.set_zoom(options_json)
    })
}

/// Start listening for camera frame changes (high-frequency streaming).
#[op2(fast)]
pub fn op_camera_listen_frame_change(
    state: &mut OpState,
    #[smi] camera_id: u32,
) -> Result<(), JsErrorBox> {
    crate::permission::require_scope(state, Scope::Camera)?;
    with_camera_service(state, "camera.listenFrameChange:fail not supported", |c| {
        c.listen_frame_change(camera_id)
    })
}

/// Stop listening for camera frame changes.
#[op2(fast)]
pub fn op_camera_close_frame_change(
    state: &mut OpState,
    #[smi] camera_id: u32,
) -> Result<(), JsErrorBox> {
    with_camera_service(state, "camera.closeFrameChange:fail not supported", |c| {
        c.close_frame_change(camera_id)
    })
}

// ==================== Image API Ops ====================

/// Save image to system photo album.
#[op2(fast)]
pub fn op_save_image_to_photos_album(
    state: &mut OpState,
    #[string] options_json: &str,
) -> Result<(), JsErrorBox> {
    crate::permission::require_scope(state, Scope::WritePhotosAlbum)?;
    with_image_api(state, "saveImageToPhotosAlbum:fail not supported", |svc| {
        svc.save_image_to_photos_album(options_json)
    })
}

/// Preview images and videos.
#[op2(fast)]
pub fn op_preview_media(
    state: &mut OpState,
    #[string] options_json: &str,
) -> Result<(), JsErrorBox> {
    with_image_api(state, "previewMedia:fail not supported", |svc| {
        svc.preview_media(options_json)
    })
}

/// Preview images fullscreen.
#[op2(fast)]
pub fn op_preview_image(
    state: &mut OpState,
    #[string] options_json: &str,
) -> Result<(), JsErrorBox> {
    with_image_api(state, "previewImage:fail not supported", |svc| {
        svc.preview_image(options_json)
    })
}

/// Compress image (async, result via callback).
#[op2(fast)]
pub fn op_compress_image(
    state: &mut OpState,
    #[string] options_json: &str,
) -> Result<(), JsErrorBox> {
    with_image_api(state, "compressImage:fail not supported", |svc| {
        svc.compress_image(options_json)
    })
}

/// Choose files from client session (async, result via callback).
#[op2(fast)]
pub fn op_choose_message_file(
    state: &mut OpState,
    #[string] options_json: &str,
) -> Result<(), JsErrorBox> {
    with_image_api(state, "chooseMessageFile:fail not supported", |svc| {
        svc.choose_message_file(options_json)
    })
}

/// Choose images from album or camera (async, result via callback).
#[op2(fast)]
pub fn op_choose_image(
    state: &mut OpState,
    #[string] options_json: &str,
) -> Result<(), JsErrorBox> {
    with_image_api(state, "chooseImage:fail not supported", |svc| {
        svc.choose_image(options_json)
    })
}

// ==================== Video Ops ====================

/// Create a video player instance. Returns JSON: `{"videoId": <id>}`.
#[op2]
#[string]
pub fn op_video_create(
    state: &mut OpState,
    #[string] options_json: &str,
) -> Result<String, JsErrorBox> {
    with_video(state, "createVideo:fail not supported", |v| {
        v.create(options_json)
    })
}

/// Start or resume video playback.
#[op2(fast)]
pub fn op_video_play(state: &mut OpState, #[smi] video_id: u32) -> Result<(), JsErrorBox> {
    with_video(state, "video.play:fail not supported", |v| v.play(video_id))
}

/// Pause video playback.
#[op2(fast)]
pub fn op_video_pause(state: &mut OpState, #[smi] video_id: u32) -> Result<(), JsErrorBox> {
    with_video(state, "video.pause:fail not supported", |v| {
        v.pause(video_id)
    })
}

/// Stop video playback and reset to beginning.
#[op2(fast)]
pub fn op_video_stop(state: &mut OpState, #[smi] video_id: u32) -> Result<(), JsErrorBox> {
    with_video(state, "video.stop:fail not supported", |v| v.stop(video_id))
}

/// Seek to a specific position in seconds.
#[op2(fast)]
pub fn op_video_seek(
    state: &mut OpState,
    #[smi] video_id: u32,
    position: f64,
) -> Result<(), JsErrorBox> {
    with_video(state, "video.seek:fail not supported", |v| {
        v.seek(video_id, position)
    })
}

/// Enter fullscreen mode.
#[op2(fast)]
pub fn op_video_request_fullscreen(
    state: &mut OpState,
    #[smi] video_id: u32,
    direction: i32,
) -> Result<(), JsErrorBox> {
    with_video(state, "video.requestFullScreen:fail not supported", |v| {
        v.request_fullscreen(video_id, direction)
    })
}

/// Exit fullscreen mode.
#[op2(fast)]
pub fn op_video_exit_fullscreen(
    state: &mut OpState,
    #[smi] video_id: u32,
) -> Result<(), JsErrorBox> {
    with_video(state, "video.exitFullScreen:fail not supported", |v| {
        v.exit_fullscreen(video_id)
    })
}

/// Set a video property (JSON-encoded key-value pair).
#[op2(fast)]
pub fn op_video_set_property(
    state: &mut OpState,
    #[smi] video_id: u32,
    #[string] options_json: &str,
) -> Result<(), JsErrorBox> {
    with_video(state, "video.setProperty:fail not supported", |v| {
        v.set_property(video_id, options_json)
    })
}

/// Destroy a video player instance and release all resources.
#[op2(fast)]
pub fn op_video_destroy(state: &mut OpState, #[smi] video_id: u32) -> Result<(), JsErrorBox> {
    with_video(state, "video.destroy:fail not supported", |v| {
        v.destroy(video_id)
    })
}

// ==================== Extension Definition ====================

deno_core::extension!(
    host_v8_media,
    deps = [host_v8_base],
    ops = [
        op_camera_create,
        op_camera_destroy,
        op_camera_take_photo,
        op_camera_start_record,
        op_camera_stop_record,
        op_camera_set_zoom,
        op_camera_listen_frame_change,
        op_camera_close_frame_change,
        op_save_image_to_photos_album,
        op_preview_media,
        op_preview_image,
        op_compress_image,
        op_choose_message_file,
        op_choose_image,
        op_video_create,
        op_video_play,
        op_video_pause,
        op_video_stop,
        op_video_seek,
        op_video_request_fullscreen,
        op_video_exit_fullscreen,
        op_video_set_property,
        op_video_destroy,
    ],
    esm_entry_point = "ext:host_v8_media/99_global_scope.js",
    esm = [
        dir "src/media",
        "01_camera.js",
        "02_image_api.js",
        "03_video_decoder.js",
        "04_video.js",
        "99_global_scope.js",
    ],
);

pub fn media_extensions() -> Vec<Extension> {
    vec![host_v8_media::init()]
}

pub fn media_lazy_extensions() -> Vec<Extension> {
    vec![host_v8_media::lazy_init()]
}

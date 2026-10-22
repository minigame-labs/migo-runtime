//! Media service ops (Camera, Image API, Video) and ESM modules.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use deno_core::{Extension, OpState, op2};
use deno_error::JsErrorBox;
use shared::op_state::HostOpState;
use shared::protocol::error::ServiceError;
use shared::services::{CameraService, ImageApiService, Scope, VideoService};

pub(crate) mod video_decoder;

use crate::file::host_paths::export_paths;
use migo_services::host_paths as files;

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
//
// Every one is a request the camera service answers -- a call through its
// result hook (`CAMERA_RESULT_HOOKS`), a command not at all -- carrying
// content's `cameraId` and options, and a call's `requestId`. Opening, taking
// and changing what the camera sees needs `scope.camera`; letting it go does
// not, so a camera the player stopped allowing can still be closed.

/// Open a camera and show its preview: a call, answered once it shows.
#[op2(fast)]
pub fn op_camera_create(
    state: &mut OpState,
    #[string] request_json: &str,
) -> Result<(), JsErrorBox> {
    crate::permission::require_scope(state, Scope::Camera)?;
    with_camera_service(state, "createCamera:fail not supported", |camera| {
        camera.create(request_json)
    })
}

/// Close a camera: a command.
#[op2(fast)]
pub fn op_camera_destroy(
    state: &mut OpState,
    #[string] request_json: &str,
) -> Result<(), JsErrorBox> {
    with_camera_service(state, "camera.destroy:fail not supported", |camera| {
        camera.destroy(request_json)
    })
}

/// What content changed on a camera -- where its preview is, which camera, the flash, the frame size: a command.
#[op2(fast)]
pub fn op_camera_set(state: &mut OpState, #[string] request_json: &str) -> Result<(), JsErrorBox> {
    crate::permission::require_scope(state, Scope::Camera)?;
    with_camera_service(state, "camera:fail not supported", |camera| {
        camera.set(request_json)
    })
}

/// Take a photo: a call, answered with the file.
#[op2(fast)]
pub fn op_camera_take_photo(
    state: &mut OpState,
    #[string] request_json: &str,
) -> Result<(), JsErrorBox> {
    crate::permission::require_scope(state, Scope::Camera)?;
    with_camera_service(state, "camera.takePhoto:fail not supported", |camera| {
        camera.take_photo(request_json)
    })
}

/// Start recording video: a call, answered once it records.
#[op2(fast)]
pub fn op_camera_start_record(
    state: &mut OpState,
    #[string] request_json: &str,
) -> Result<(), JsErrorBox> {
    crate::permission::require_scope(state, Scope::Camera)?;
    with_camera_service(state, "camera.startRecord:fail not supported", |camera| {
        camera.start_record(request_json)
    })
}

/// Stop recording: a call, answered with the video and its cover once written.
#[op2(fast)]
pub fn op_camera_stop_record(
    state: &mut OpState,
    #[string] request_json: &str,
) -> Result<(), JsErrorBox> {
    with_camera_service(state, "camera.stopRecord:fail not supported", |camera| {
        camera.stop_record(request_json)
    })
}

/// Zoom: a call, answered with the zoom taken.
#[op2(fast)]
pub fn op_camera_set_zoom(
    state: &mut OpState,
    #[string] request_json: &str,
) -> Result<(), JsErrorBox> {
    crate::permission::require_scope(state, Scope::Camera)?;
    with_camera_service(state, "camera.setZoom:fail not supported", |camera| {
        camera.set_zoom(request_json)
    })
}

/// Start handing content the frames: a command.
#[op2(fast)]
pub fn op_camera_listen_frame_change(
    state: &mut OpState,
    #[string] request_json: &str,
) -> Result<(), JsErrorBox> {
    crate::permission::require_scope(state, Scope::Camera)?;
    with_camera_service(
        state,
        "camera.listenFrameChange:fail not supported",
        |camera| camera.listen_frame_change(request_json),
    )
}

/// Stop handing content the frames: a command.
#[op2(fast)]
pub fn op_camera_close_frame_change(
    state: &mut OpState,
    #[string] request_json: &str,
) -> Result<(), JsErrorBox> {
    with_camera_service(
        state,
        "camera.closeFrameChange:fail not supported",
        |camera| camera.close_frame_change(request_json),
    )
}

// ==================== Image API Ops ====================
//
// Every one is a request the host answers through a hook. A request that names
// content's files has them resolved through the sandbox first (`export_paths`), so
// a host is only ever asked to read what the game itself can read: the path is
// content's to choose, and before this the host was handed it verbatim --
// `saveImageToPhotosAlbum({filePath: "<the host app's database>"})` put that file in
// the player's photo album. What a result names comes back through
// `shared::services::host_files`.
//
// The ones that name files are eager: a path that resolves to a file of its own
// needs no wait, so the request leaves in the call's own tick; only a package
// entry that has to be copied out first defers it.

fn image_api(
    state: &OpState,
    err_msg: &'static str,
) -> Result<Arc<dyn ImageApiService>, JsErrorBox> {
    state
        .borrow::<HostOpState>()
        .device_services
        .as_ref()
        .and_then(|services| services.image_api())
        .ok_or_else(|| JsErrorBox::generic(err_msg))
}

/// Save an image to the system photo album.
#[op2]
pub async fn op_save_image_to_photos_album(
    state: Rc<RefCell<OpState>>,
    #[string] request_json: String,
) -> Result<(), JsErrorBox> {
    let service = {
        let mut st = state.borrow_mut();
        crate::permission::require_scope(&mut st, Scope::WritePhotosAlbum)?;
        image_api(&st, "saveImageToPhotosAlbum:fail not supported")?
    };
    let request = export_paths(&state, &request_json, files::SAVE_IMAGE_TO_PHOTOS_ALBUM).await?;
    service
        .save_image_to_photos_album(&request)
        .map_err(JsErrorBox::generic)
}

/// Show images and videos in a full-screen viewer.
#[op2]
pub async fn op_preview_media(
    state: Rc<RefCell<OpState>>,
    #[string] request_json: String,
) -> Result<(), JsErrorBox> {
    let service = image_api(&state.borrow(), "previewMedia:fail not supported")?;
    let request = export_paths(&state, &request_json, files::PREVIEW_MEDIA).await?;
    service.preview_media(&request).map_err(JsErrorBox::generic)
}

/// Show images in a full-screen viewer.
#[op2]
pub async fn op_preview_image(
    state: Rc<RefCell<OpState>>,
    #[string] request_json: String,
) -> Result<(), JsErrorBox> {
    let service = image_api(&state.borrow(), "previewImage:fail not supported")?;
    let request = export_paths(&state, &request_json, files::PREVIEW_IMAGE).await?;
    service.preview_image(&request).map_err(JsErrorBox::generic)
}

/// Compress an image.
#[op2]
pub async fn op_compress_image(
    state: Rc<RefCell<OpState>>,
    #[string] request_json: String,
) -> Result<(), JsErrorBox> {
    let service = image_api(&state.borrow(), "compressImage:fail not supported")?;
    let request = export_paths(&state, &request_json, files::COMPRESS_IMAGE).await?;
    service
        .compress_image(&request)
        .map_err(JsErrorBox::generic)
}

/// Let the player choose files.
#[op2(fast)]
pub fn op_choose_message_file(
    state: &mut OpState,
    #[string] request_json: &str,
) -> Result<(), JsErrorBox> {
    image_api(state, "chooseMessageFile:fail not supported")?
        .choose_message_file(request_json)
        .map_err(JsErrorBox::generic)
}

/// Let the player choose images from the album or the camera.
#[op2(fast)]
pub fn op_choose_image(
    state: &mut OpState,
    #[string] request_json: &str,
) -> Result<(), JsErrorBox> {
    image_api(state, "chooseImage:fail not supported")?
        .choose_image(request_json)
        .map_err(JsErrorBox::generic)
}

/// Let the player choose or capture images and videos.
#[op2(fast)]
pub fn op_choose_media(
    state: &mut OpState,
    #[string] request_json: &str,
) -> Result<(), JsErrorBox> {
    image_api(state, "chooseMedia:fail not supported")?
        .choose_media(request_json)
        .map_err(JsErrorBox::generic)
}

// ==================== Video Ops ====================
//
// Every one is a request the video service carries out: a call answered
// through its result hook (`VIDEO_RESULT_HOOKS`) once it has happened -- the
// reference's promises settle then -- or a command answered by nothing, each
// with content's `videoId` and a call's `requestId`.

/// Make a player and show it. Its `src` and `poster` reach the host as the real
/// file or the checked URL they name, never content's string: a player that
/// opened it verbatim played any URL under a policy that granted nothing, and
/// any file the host could read.
#[op2(fast)]
pub fn op_video_create(
    state: &mut OpState,
    #[string] request_json: &str,
) -> Result<(), JsErrorBox> {
    let sandbox = crate::file::host_paths::sandbox_of(state);
    with_video(state, "createVideo:fail not supported", |v| {
        v.create(
            &files::export_json_now(&sandbox, request_json, files::VIDEO)
                .map_err(|e| ServiceError::invalid_param(e.message))?,
        )
    })
}

/// Take a player away.
#[op2(fast)]
pub fn op_video_destroy(
    state: &mut OpState,
    #[string] request_json: &str,
) -> Result<(), JsErrorBox> {
    with_video(state, "video.destroy:fail not supported", |v| {
        v.destroy(request_json)
    })
}

/// What content changed on a player; a new `src` or `poster` is exported as
/// `createVideo`'s are.
#[op2(fast)]
pub fn op_video_set(state: &mut OpState, #[string] request_json: &str) -> Result<(), JsErrorBox> {
    let sandbox = crate::file::host_paths::sandbox_of(state);
    with_video(state, "video:fail not supported", |v| {
        v.set(
            &files::export_json_now(&sandbox, request_json, files::VIDEO)
                .map_err(|e| ServiceError::invalid_param(e.message))?,
        )
    })
}

/// Play: a call, answered once it plays.
#[op2(fast)]
pub fn op_video_play(state: &mut OpState, #[string] request_json: &str) -> Result<(), JsErrorBox> {
    with_video(state, "video.play:fail not supported", |v| {
        v.play(request_json)
    })
}

/// Pause: a call, answered once paused.
#[op2(fast)]
pub fn op_video_pause(state: &mut OpState, #[string] request_json: &str) -> Result<(), JsErrorBox> {
    with_video(state, "video.pause:fail not supported", |v| {
        v.pause(request_json)
    })
}

/// Stop: a call, answered once stopped.
#[op2(fast)]
pub fn op_video_stop(state: &mut OpState, #[string] request_json: &str) -> Result<(), JsErrorBox> {
    with_video(state, "video.stop:fail not supported", |v| {
        v.stop(request_json)
    })
}

/// Seek: a call, answered once there.
#[op2(fast)]
pub fn op_video_seek(state: &mut OpState, #[string] request_json: &str) -> Result<(), JsErrorBox> {
    with_video(state, "video.seek:fail not supported", |v| {
        v.seek(request_json)
    })
}

/// Full screen: a call, answered once it fills the screen.
#[op2(fast)]
pub fn op_video_request_full_screen(
    state: &mut OpState,
    #[string] request_json: &str,
) -> Result<(), JsErrorBox> {
    with_video(state, "video.requestFullScreen:fail not supported", |v| {
        v.request_full_screen(request_json)
    })
}

/// Leave full screen: a call, answered once back where content put it.
#[op2(fast)]
pub fn op_video_exit_full_screen(
    state: &mut OpState,
    #[string] request_json: &str,
) -> Result<(), JsErrorBox> {
    with_video(state, "video.exitFullScreen:fail not supported", |v| {
        v.exit_full_screen(request_json)
    })
}

// ==================== Extension Definition ====================

deno_core::extension!(
    host_v8_media,
    deps = [host_v8_base],
    ops = [
        op_camera_create,
        op_camera_destroy,
        op_camera_set,
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
        op_choose_media,
        op_video_create,
        op_video_play,
        op_video_pause,
        op_video_stop,
        op_video_seek,
        op_video_request_full_screen,
        op_video_exit_full_screen,
        op_video_set,
        op_video_destroy,
        video_decoder::op_video_decoder_available,
        video_decoder::op_video_decoder_start,
        video_decoder::op_video_decoder_take_frame,
        video_decoder::op_video_decoder_seek,
        video_decoder::op_video_decoder_stop,
        video_decoder::op_video_decoder_remove,
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

// Global scope registration for host_v8_media APIs (api-media feature gate).

import * as cameraApi from 'ext:host_v8_media/01_camera.js';
import * as imageApi from 'ext:host_v8_media/02_image_api.js';
import * as videoDecoderApi from 'ext:host_v8_media/03_video_decoder.js';
import * as videoApi from 'ext:host_v8_media/04_video.js';
import * as gameRecorderApi from 'ext:host_v8_media/05_game_recorder.js';

import { primordials, core } from "ext:core/mod.js";
const { ObjectDefineProperties } = primordials;

ObjectDefineProperties(globalThis, {
    // Camera
    createCamera: core.propNonEnumerable(cameraApi.createCamera),
    _internalOnCreateCameraResult: core.propNonEnumerable(cameraApi._internalOnCreateCameraResult),
    _internalOnCameraTakePhotoResult: core.propNonEnumerable(cameraApi._internalOnCameraTakePhotoResult),
    _internalOnCameraStartRecordResult: core.propNonEnumerable(cameraApi._internalOnCameraStartRecordResult),
    _internalOnCameraStopRecordResult: core.propNonEnumerable(cameraApi._internalOnCameraStopRecordResult),
    _internalOnCameraSetZoomResult: core.propNonEnumerable(cameraApi._internalOnCameraSetZoomResult),
    _internalOnCameraStopEvent: core.propNonEnumerable(cameraApi._internalOnCameraStopEvent),
    _internalOnCameraAuthCancelEvent: core.propNonEnumerable(cameraApi._internalOnCameraAuthCancelEvent),
    _internalOnCameraFrameData: core.propNonEnumerable(cameraApi._internalOnCameraFrameData),

    // Image API
    saveImageToPhotosAlbum: core.propNonEnumerable(imageApi.saveImageToPhotosAlbum),
    _internalOnSaveImageToPhotosAlbumResult: core.propNonEnumerable(imageApi._internalOnSaveImageToPhotosAlbumResult),
    previewMedia: core.propNonEnumerable(imageApi.previewMedia),
    _internalOnPreviewMediaResult: core.propNonEnumerable(imageApi._internalOnPreviewMediaResult),
    previewImage: core.propNonEnumerable(imageApi.previewImage),
    _internalOnPreviewImageResult: core.propNonEnumerable(imageApi._internalOnPreviewImageResult),
    compressImage: core.propNonEnumerable(imageApi.compressImage),
    _internalOnCompressImageResult: core.propNonEnumerable(imageApi._internalOnCompressImageResult),
    chooseMessageFile: core.propNonEnumerable(imageApi.chooseMessageFile),
    _internalOnChooseMessageFileResult: core.propNonEnumerable(imageApi._internalOnChooseMessageFileResult),
    chooseImage: core.propNonEnumerable(imageApi.chooseImage),
    _internalOnChooseImageResult: core.propNonEnumerable(imageApi._internalOnChooseImageResult),
    chooseMedia: core.propNonEnumerable(imageApi.chooseMedia),
    _internalOnChooseMediaResult: core.propNonEnumerable(imageApi._internalOnChooseMediaResult),

    // VideoDecoder
    createVideoDecoder: core.propNonEnumerable(videoDecoderApi.createVideoDecoder),
    _internalOnVideoDecoderEvent: core.propNonEnumerable(videoDecoderApi._internalOnVideoDecoderEvent),

    // Video
    createVideo: core.propNonEnumerable(videoApi.createVideo),
    getGameRecorder: core.propNonEnumerable(gameRecorderApi.getGameRecorder),
    _internalOnVideoPlayResult: core.propNonEnumerable(videoApi._internalOnVideoPlayResult),
    _internalOnVideoPauseResult: core.propNonEnumerable(videoApi._internalOnVideoPauseResult),
    _internalOnVideoStopResult: core.propNonEnumerable(videoApi._internalOnVideoStopResult),
    _internalOnVideoSeekResult: core.propNonEnumerable(videoApi._internalOnVideoSeekResult),
    _internalOnVideoRequestFullScreenResult: core.propNonEnumerable(videoApi._internalOnVideoRequestFullScreenResult),
    _internalOnVideoExitFullScreenResult: core.propNonEnumerable(videoApi._internalOnVideoExitFullScreenResult),
    _internalOnVideoPlayEvent: core.propNonEnumerable(videoApi._internalOnVideoPlayEvent),
    _internalOnVideoPauseEvent: core.propNonEnumerable(videoApi._internalOnVideoPauseEvent),
    _internalOnVideoEndedEvent: core.propNonEnumerable(videoApi._internalOnVideoEndedEvent),
    _internalOnVideoTimeUpdateEvent: core.propNonEnumerable(videoApi._internalOnVideoTimeUpdateEvent),
    _internalOnVideoErrorEvent: core.propNonEnumerable(videoApi._internalOnVideoErrorEvent),
    _internalOnVideoWaitingEvent: core.propNonEnumerable(videoApi._internalOnVideoWaitingEvent),
    _internalOnVideoProgressEvent: core.propNonEnumerable(videoApi._internalOnVideoProgressEvent),
});

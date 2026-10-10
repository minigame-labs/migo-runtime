// Global scope registration for host_v8_media APIs (api-media feature gate).

import * as cameraApi from 'ext:host_v8_media/01_camera.js';
import * as imageApi from 'ext:host_v8_media/02_image_api.js';
import * as videoDecoderApi from 'ext:host_v8_media/03_video_decoder.js';
import * as videoApi from 'ext:host_v8_media/04_video.js';

import { primordials, core } from "ext:core/mod.js";
const { ObjectDefineProperties } = primordials;

ObjectDefineProperties(globalThis, {
    // Camera
    createCamera: core.propNonEnumerable(cameraApi.createCamera),
    _internalOnCameraEvent: core.propNonEnumerable(cameraApi._internalOnCameraEvent),
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

    // Video
    createVideo: core.propNonEnumerable(videoApi.createVideo),
    _internalTriggerVideoEvent: core.propNonEnumerable(videoApi._internalTriggerVideoEvent),
});

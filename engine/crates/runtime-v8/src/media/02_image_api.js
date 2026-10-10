import {
    op_save_image_to_photos_album,
    op_preview_media,
    op_preview_image,
    op_compress_image,
    op_choose_message_file,
    op_choose_image,
    op_choose_media,
} from "ext:core/ops";
import { createDeferredApi } from "ext:host_v8_base/02_async.js";

// Every call here is a request the host answers. Paths content names are its
// sandbox paths; the runtime resolves them before the request leaves, and a file
// a result names arrives as a `/tmp` path, already moved into the sandbox.
//
// A picker, a viewer and the photo album wait on a person -- a permission
// prompt, a choice -- so they have no timeout; compressImage keeps the default.

// ==================== saveImageToPhotosAlbum ====================

const _saveImageApi = createDeferredApi('saveImageToPhotosAlbum', 0);

function saveImageToPhotosAlbum(options) {
    return _saveImageApi.invoke(options, function (opts, requestId) {
        if (typeof opts.filePath !== 'string' || opts.filePath.length === 0) {
            throw new Error('filePath is required');
        }
        return op_save_image_to_photos_album(JSON.stringify({
            requestId: requestId,
            filePath: opts.filePath,
        }));
    });
}

function _internalOnSaveImageToPhotosAlbumResult(resultJson) {
    _saveImageApi.settle(resultJson);
}

// ==================== previewMedia ====================

const _previewMediaApi = createDeferredApi('previewMedia', 0);

function previewMedia(options) {
    return _previewMediaApi.invoke(options, function (opts, requestId) {
        const sources = opts.sources;
        if (!Array.isArray(sources) || sources.length === 0) {
            throw new Error('sources is required');
        }
        return op_preview_media(JSON.stringify({
            requestId: requestId,
            sources: sources.map(function (source) {
                const item = {
                    url: source && source.url,
                    type: source && source.type === 'video' ? 'video' : 'image',
                };
                if (source && typeof source.poster === 'string' && source.poster.length > 0) {
                    item.poster = source.poster;
                }
                return item;
            }),
            current: typeof opts.current === 'number' ? opts.current : 0,
            showmenu: opts.showmenu !== false,
            referrerPolicy: typeof opts.referrerPolicy === 'string' ? opts.referrerPolicy : 'no-referrer',
        }));
    });
}

function _internalOnPreviewMediaResult(resultJson) {
    _previewMediaApi.settle(resultJson);
}

// ==================== previewImage ====================

const _previewImageApi = createDeferredApi('previewImage', 0);

function previewImage(options) {
    return _previewImageApi.invoke(options, function (opts, requestId) {
        const urls = opts.urls;
        if (!Array.isArray(urls) || urls.length === 0) {
            throw new Error('urls is required');
        }
        return op_preview_image(JSON.stringify({
            requestId: requestId,
            urls: urls,
            current: typeof opts.current === 'string' && opts.current.length > 0 ? opts.current : urls[0],
            showmenu: opts.showmenu !== false,
            referrerPolicy: typeof opts.referrerPolicy === 'string' ? opts.referrerPolicy : 'no-referrer',
        }));
    });
}

function _internalOnPreviewImageResult(resultJson) {
    _previewImageApi.settle(resultJson);
}

// ==================== compressImage ====================

const _compressImageApi = createDeferredApi('compressImage');

function compressImage(options) {
    return _compressImageApi.invoke(options, function (opts, requestId) {
        if (typeof opts.src !== 'string' || opts.src.length === 0) {
            throw new Error('src is required');
        }
        return op_compress_image(JSON.stringify({
            requestId: requestId,
            src: opts.src,
            quality: opts.quality !== undefined ? opts.quality : 80,
            compressedWidth: opts.compressedWidth !== undefined ? opts.compressedWidth : 0,
            compressedHeight: opts.compressedHeight !== undefined ? opts.compressedHeight : 0,
        }));
    });
}

function _internalOnCompressImageResult(resultJson) {
    _compressImageApi.settle(resultJson);
}

// ==================== chooseMessageFile ====================

const _chooseMessageFileApi = createDeferredApi('chooseMessageFile', 0);

function chooseMessageFile(options) {
    return _chooseMessageFileApi.invoke(options, function (opts, requestId) {
        if (opts.count === undefined) {
            throw new Error('count is required');
        }
        op_choose_message_file(JSON.stringify({
            requestId: requestId,
            count: opts.count,
            type: opts.type !== undefined ? opts.type : 'all',
            extension: opts.extension !== undefined ? opts.extension : [],
        }));
    });
}

function _internalOnChooseMessageFileResult(resultJson) {
    _chooseMessageFileApi.settle(resultJson);
}

// ==================== chooseImage ====================

const _chooseImageApi = createDeferredApi('chooseImage', 0);

function chooseImage(options) {
    return _chooseImageApi.invoke(options, function (opts, requestId) {
        op_choose_image(JSON.stringify({
            requestId: requestId,
            count: opts.count !== undefined ? opts.count : 9,
            sizeType: opts.sizeType !== undefined ? opts.sizeType : ['original', 'compressed'],
            sourceType: opts.sourceType !== undefined ? opts.sourceType : ['album', 'camera'],
        }));
    });
}

function _internalOnChooseImageResult(resultJson) {
    _chooseImageApi.settle(resultJson);
}

// ==================== chooseMedia ====================

const _chooseMediaApi = createDeferredApi('chooseMedia', 0);

function chooseMedia(options) {
    return _chooseMediaApi.invoke(options, function (opts, requestId) {
        op_choose_media(JSON.stringify({
            requestId: requestId,
            count: opts.count !== undefined ? opts.count : 9,
            mediaType: opts.mediaType !== undefined ? opts.mediaType : ['image', 'video'],
            sourceType: opts.sourceType !== undefined ? opts.sourceType : ['album', 'camera'],
            maxDuration: opts.maxDuration !== undefined ? opts.maxDuration : 10,
            sizeType: opts.sizeType !== undefined ? opts.sizeType : ['original', 'compressed'],
            camera: opts.camera === 'front' ? 'front' : 'back',
        }));
    });
}

function _internalOnChooseMediaResult(resultJson) {
    _chooseMediaApi.settle(resultJson);
}

export {
    saveImageToPhotosAlbum,
    _internalOnSaveImageToPhotosAlbumResult,
    previewMedia,
    _internalOnPreviewMediaResult,
    previewImage,
    _internalOnPreviewImageResult,
    compressImage,
    _internalOnCompressImageResult,
    chooseMessageFile,
    _internalOnChooseMessageFileResult,
    chooseImage,
    _internalOnChooseImageResult,
    chooseMedia,
    _internalOnChooseMediaResult,
};

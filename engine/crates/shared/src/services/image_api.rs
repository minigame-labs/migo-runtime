//! Image and media picking, preview, saving and compression.

use crate::protocol::error::ServiceError;

/// Image and media requests the host answers.
///
/// Every method is a request carrying `requestId`, answered through its hook with
/// `{"requestId", ...result}` or `{"requestId", "error"}`. A path in a request is a
/// real path the runtime resolved from content's sandbox path, readable by the
/// host for as long as the request is open; a file a result names is handed over
/// to the runtime, which moves it into the session's `/tmp`
/// ([`crate::services::host_files`]).
pub trait ImageApiService: Send + Sync {
    /// Save an image to the system photo album.
    ///
    /// Request: `{"requestId", "filePath"}`. Answers `{}`, through
    /// `_internalOnSaveImageToPhotosAlbumResult`.
    fn save_image_to_photos_album(&self, _request_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "saveImageToPhotosAlbum:fail not supported",
        ))
    }

    /// Show images and videos in a full-screen viewer.
    ///
    /// Request: `{"requestId", "sources": [{"url", "type", "poster"}], "current",
    /// "showmenu", "referrerPolicy"}` -- `url` and `poster` are real paths or
    /// http(s) URLs. Answers `{}` once shown, through `_internalOnPreviewMediaResult`.
    fn preview_media(&self, _request_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "previewMedia:fail not supported",
        ))
    }

    /// Show images in a full-screen viewer.
    ///
    /// Request: `{"requestId", "urls", "current", "showmenu", "referrerPolicy"}` --
    /// each url a real path or an http(s) URL, `current` one of them. Answers `{}`
    /// once shown, through `_internalOnPreviewImageResult`.
    fn preview_image(&self, _request_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "previewImage:fail not supported",
        ))
    }

    /// Compress an image.
    ///
    /// Request: `{"requestId", "src", "quality", "compressedWidth",
    /// "compressedHeight"}`. Answers `{"tempFilePath"}`, a file handed over,
    /// through `_internalOnCompressImageResult`.
    fn compress_image(&self, _request_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "compressImage:fail not supported",
        ))
    }

    /// Let the player choose files.
    ///
    /// Request: `{"requestId", "count", "type", "extension"}`. Answers
    /// `{"tempFiles": [{"path", "size", "name", "type", "time"}]}`, each `path` a
    /// file handed over, through `_internalOnChooseMessageFileResult`.
    fn choose_message_file(&self, _request_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "chooseMessageFile:fail not supported",
        ))
    }

    /// Let the player choose images from the album or the camera.
    ///
    /// Request: `{"requestId", "count", "sizeType", "sourceType"}`. Answers
    /// `{"tempFilePaths", "tempFiles": [{"path", "size"}]}`, naming the same
    /// files handed over, through `_internalOnChooseImageResult`.
    fn choose_image(&self, _request_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "chooseImage:fail not supported",
        ))
    }

    /// Let the player choose or capture images and videos.
    ///
    /// Request: `{"requestId", "count", "mediaType", "sourceType", "maxDuration",
    /// "sizeType", "camera"}`. Answers `{"type", "tempFiles": [{"tempFilePath",
    /// "size", "duration", "height", "width", "thumbTempFilePath", "fileType"}]}`,
    /// each `tempFilePath` and `thumbTempFilePath` a file handed over, through
    /// `_internalOnChooseMediaResult`.
    fn choose_media(&self, _request_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "chooseMedia:fail not supported",
        ))
    }
}

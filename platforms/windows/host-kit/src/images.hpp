#ifndef MIGO_WINDOWS_HOST_KIT_IMAGES_HPP_
#define MIGO_WINDOWS_HOST_KIT_IMAGES_HPP_

// Internal to the Host Kit, not installed: the image service.

#include <migo/windows/host_services.hpp>

#include <cstdint>
#include <memory>
#include <string>
#include <utility>

struct HWND__;

namespace migo::windows_host {

/// The image service, Windows' way -- the Linux Host Kit's, for Win32: the
/// player's pictures and files through the system's file dialogs, images
/// through the Windows Imaging Component, videos described by Media
/// Foundation, previews in a viewer of its own.
///
/// - chooseImage, chooseMedia, chooseMessageFile: the system's open dialog,
///   modal to the App's window -- run on a thread of its own, a COM apartment
///   of its own, so the App's message loop goes on and nothing re-enters it --
///   from the Pictures folder for images and media. What the player picks is
///   copied, never lent: an image the engine draws (JPEG, PNG, GIF, BMP, WebP)
///   as it is, anything else WIC reads written upright (a PNG when it has
///   transparency, a JPEG otherwise); a video as it is, its length and size
///   read by Media Foundation and its cover the system's thumbnail, where they
///   can be. The camera is not offered: a call that allows only it is refused.
/// - compressImage: decoded upright at the size asked, a PNG kept a PNG and
///   anything else written as a JPEG at the quality asked.
/// - saveImageToPhotosAlbum: a copy in the player's Pictures folder, named as
///   a camera names a photo. saveFileToDisk: the system's save dialog, the
///   copy replacing what is there only once whole.
/// - previewImage, previewMedia: the viewer (`MediaViewer`), once every file is
///   open and every http(s) image downloaded -- the call answered as it shows.
///   A video in previewMedia is refused until the Host Kit plays videos.
///
/// One file dialog at a time. A dismissed dialog is the failure "cancel". The
/// work on files runs off the GUI thread; what it hands over is staged
/// (`Staging`). Destroyed, it closes the dialog and the viewer it showed and
/// stops the downloads in flight. GUI thread only.
class Images final {
public:
    /// Where staged files wait, and the player's photo album.
    struct Places {
        std::wstring staging;
        std::wstring album;
        /// The user's temporary directory and their Pictures folder.
        [[nodiscard]] static Places system();
    };

    /// Images larger than this many pixels are refused rather than decoded.
    static constexpr std::int64_t kMaxPixels = 50'000'000;

    /// `owner` is the App's top-level window, which the dialogs and the viewer
    /// are modal to.
    Images(HWND__ *owner, Places places = Places::system());
    ~Images();

    Images(const Images &) = delete;
    Images &operator=(const Images &) = delete;

    /// One of MIGO_IMAGE_*.
    void call(std::uint32_t method, const Json &request, HostServiceReply reply);

    /// The size `width` x `height` asks of a `source_width` x `source_height`
    /// image: both given, that; one, the other scaled to keep the shape; none,
    /// the image's own.
    [[nodiscard]] static std::pair<std::uint32_t, std::uint32_t> targetSize(std::uint32_t source_width,
                                                                            std::uint32_t source_height,
                                                                            std::uint32_t width,
                                                                            std::uint32_t height) noexcept;

private:
    class Impl;
    std::unique_ptr<Impl> impl_;
};

}  // namespace migo::windows_host

#endif  // MIGO_WINDOWS_HOST_KIT_IMAGES_HPP_

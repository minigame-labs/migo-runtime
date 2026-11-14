#ifndef MIGO_WINDOWS_HOST_KIT_MEDIA_VIEWER_HPP_
#define MIGO_WINDOWS_HOST_KIT_MEDIA_VIEWER_HPP_

// Internal to the Host Kit, not installed: previewImage's and previewMedia's viewer.

#include <functional>
#include <memory>
#include <string>
#include <vector>

struct HWND__;

namespace migo::windows_host {

class EmbeddedVideo;

/// previewImage's and previewMedia's viewer -- the Linux Host Kit's, for
/// Win32: the images and videos one at a time, fitted to it, from the one
/// content asked for. Left and Right, or the arrows either side, go between
/// them; Esc or the close box closes it. Modal to the App's window -- which it
/// disables while it shows -- and its client area's size, following it. Drawn
/// with Direct2D; an image that cannot be decoded is shown as a warning sign in
/// its place. A video is played by the Host Kit's video players, with its
/// poster and its controls, between the arrows.
///
/// It owns what it shows -- each file held open, so the engine may take back
/// the file it lent once the call is answered -- and that goes with it.
/// Nothing on it is in words, so there is nothing to translate. GUI thread
/// only.
class MediaViewer final {
public:
    /// One thing shown: an image held open, or a video -- held open, or
    /// streamed from its http(s) URL -- with its poster.
    struct Item {
        /// An open file HANDLE the viewer closes; null for a video streamed from `url`.
        void *file = nullptr;
        bool video = false;
        /// A video's: the URL it plays from, or that names its kind when it is `file`.
        std::string url;
        std::string poster;
    };

    /// What plays a video in the viewer's window: from `file`, a HANDLE it
    /// takes, or `url`. None where videos cannot be played.
    using VideoMaker = std::function<std::unique_ptr<EmbeddedVideo>(HWND__ *window, void *file, const std::string &url,
                                                                    const std::string &poster)>;

    /// `items` each hold what they show; `current` is shown first. `closed` is
    /// called, on the GUI thread and from a message the viewer handles, when
    /// the player closes it: the owner destroys it after, not from within.
    MediaViewer(HWND__ *owner, std::vector<Item> items, int current, std::function<void()> closed,
                VideoMaker videos = {});
    ~MediaViewer();

    MediaViewer(const MediaViewer &) = delete;
    MediaViewer &operator=(const MediaViewer &) = delete;

    [[nodiscard]] HWND__ *hwnd() const noexcept;
    /// The image shown, from 0, and how many there are.
    [[nodiscard]] int current() const noexcept;
    [[nodiscard]] int count() const noexcept;
    /// Whether the image shown could be decoded.
    [[nodiscard]] bool showsImage() const noexcept;

    void showPrevious();
    void showNext();

private:
    class Impl;
    std::unique_ptr<Impl> impl_;
};

}  // namespace migo::windows_host

#endif  // MIGO_WINDOWS_HOST_KIT_MEDIA_VIEWER_HPP_

#ifndef MIGO_WINDOWS_HOST_KIT_MEDIA_VIEWER_HPP_
#define MIGO_WINDOWS_HOST_KIT_MEDIA_VIEWER_HPP_

// Internal to the Host Kit, not installed: previewImage's and previewMedia's viewer.

#include <functional>
#include <memory>
#include <vector>

struct HWND__;

namespace migo::windows_host {

/// previewImage's and previewMedia's viewer -- the Linux Host Kit's, for
/// Win32: the images one at a time, fitted to it, from the one content asked
/// for. Left and Right, or the arrows either side, go between them; Esc or the
/// close box closes it. Modal to the App's window -- which it disables while
/// it shows -- and its client area's size, following it. Drawn with Direct2D;
/// an image that cannot be decoded is shown as a warning sign in its place.
///
/// It owns what it shows -- each file held open, so the engine may take back
/// the file it lent once the call is answered -- and that goes with it.
/// Nothing on it is in words, so there is nothing to translate. GUI thread
/// only.
class MediaViewer final {
public:
    /// `files` are open file handles (HANDLE), each an image, which the viewer
    /// closes; `current` is shown first. `closed` is called, on the GUI thread
    /// and from a message the viewer handles, when the player closes it: the
    /// owner destroys it after, not from within.
    MediaViewer(HWND__ *owner, std::vector<void *> files, int current, std::function<void()> closed);
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

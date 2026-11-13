#ifndef MIGO_WINDOWS_WIN32_SURFACE_VIEW_HPP_
#define MIGO_WINDOWS_WIN32_SURFACE_VIEW_HPP_

#include <migo/input.h>
#include <migo/windows/surface_host.hpp>

#include <cstdint>
#include <functional>
#include <memory>

// The window system's handles, by the names <windows.h> gives them, without
// making every App that includes this header include <windows.h> first.
struct HWND__;
struct HICON__;

namespace migo::windows_host {

/// The game's view: a child window, of a window the App owns, that the engine
/// draws the game in and that delivers the player's input to it.
///
/// The Windows Host Kit's counterpart of the Linux Host Kit's
/// `MigoQtX11SurfaceView`, with the same contract. The view creates its window
/// (`hwnd()`), hidden, as a child of `parent`; the App places and shows it
/// (`MoveWindow`, `ShowWindow`), and the view attaches the Surface once it is
/// shown at a size -- after the show is done, so the engine binds the window at
/// its size. A resize, or a move to a screen of another scale, updates the
/// Surface. Its pixels are physical: the process should be per-monitor DPI
/// aware (v2), and the scale content sees is the window's DPI over 96.
///
/// Input: a mouse produces a touch while a button is held -- what content on a
/// phone listens for -- and the mouse stream, hover included -- what content
/// on a PC mini-game platform listens for -- as `PointerDelivery` says; the
/// wheel, in lines, three a notch; a touch screen's and a pen's contacts, with
/// their pressure; keys, named as the DOM names them (`code` from the scan
/// code, `key` from the layout); the IME's composition, its preedit and what it
/// commits; and focus, which a click on the view takes. Coordinates are CSS
/// pixels: physical pixels over the scale.
///
/// `requestFrame()` answers the engine's request for a frame with one
/// `migo_session_notify_vsync` at the next frame of the desktop's compositor
/// (DwmFlush, on a thread of the view's own); requests before it coalesce.
///
/// Teardown is the C ABI's: `beginDetach()`, then the release reported
/// (`Listener::released`), polled on a timer of the view's -- or `close()`,
/// which does both and then destroys the window. The window must not be
/// destroyed while the Surface is attached or retiring, and destroying the view
/// then is a programming error that terminates, as `std::thread` does for a
/// joinable thread: dropping the only release handle leaves the window's
/// teardown unknowable.
///
/// GUI thread only: the thread that made it, which owns its window and is the
/// Session's owner thread.
class Win32SurfaceView final {
public:
    /// Which pointer streams a mouse produces. A host, not the engine, knows
    /// what its content listens for; both by default, because a desktop most
    /// often runs content written for a phone.
    enum class PointerDelivery : std::uint8_t { TouchAndMouse, TouchOnly, MouseOnly };

    /// What the view reports, on the GUI thread; any may be empty.
    struct Listener {
        std::function<void(std::uint64_t generation)> attached;
        std::function<void(std::uint64_t generation)> released;
        /// The release has taken this long, and is still waited for.
        std::function<void(std::uint64_t generation, std::int64_t elapsed_ms)> releaseStalled;
        std::function<void(MigoResult error)> surfaceError;
        /// One input event refused -- backpressure, not a Surface fault.
        std::function<void(MigoResult error)> inputRejected;
        std::function<void(bool locked)> pointerLockChanged;
    };

    /// The view's window, a hidden child of `parent`. `surface_host` drives
    /// the Session's Surface and must outlive the view.
    Win32SurfaceView(SurfaceHost &surface_host, HWND__ *parent, Listener listener = {});
    ~Win32SurfaceView();

    Win32SurfaceView(const Win32SurfaceView &) = delete;
    Win32SurfaceView &operator=(const Win32SurfaceView &) = delete;

    /// The view's window (an `HWND`); null once `close()` has destroyed it.
    [[nodiscard]] HWND__ *hwnd() const noexcept;

    [[nodiscard]] MigoResult attachSurface();
    [[nodiscard]] MigoResult beginDetach();
    [[nodiscard]] MigoResult pollDetach(bool *released);
    /// Detach, and destroy the window once the release is reported. Safe to
    /// call more than once.
    void close();

    void setPointerDelivery(PointerDelivery delivery) noexcept;
    [[nodiscard]] PointerDelivery pointerDelivery() const noexcept;

    /// Hold the pointer for the game (the window service's
    /// REQUEST_POINTER_LOCK): the cursor hidden and confined to the view, the
    /// mouse's raw motion moving content's pointer from where the cursor stood.
    /// Taken only while the view is attached, holds the keyboard and the cursor
    /// is over it. Released by Esc -- the player's way out; that Esc is not the
    /// game's -- by the view losing the keyboard, by a detach and by
    /// `unlockPointer()`. Returns whether a lock is held now.
    [[nodiscard]] bool lockPointer();
    void unlockPointer();
    [[nodiscard]] bool isPointerLocked() const noexcept;

    /// The cursor over the game (an `HCURSOR`), the window service's setCursor;
    /// null hides it. The arrow until set.
    void setCursor(HICON__ *cursor) noexcept;

    /// One frame, in answer to the engine asking for one (the App's
    /// `on_request_frame`).
    [[nodiscard]] MigoResult requestFrame();
    [[nodiscard]] bool isFramePending() const noexcept;
    [[nodiscard]] std::uint64_t framesDelivered() const noexcept;

    [[nodiscard]] SurfaceState surfaceState() const noexcept;
    [[nodiscard]] std::uint64_t generation() const noexcept;
    [[nodiscard]] MigoResult lastError() const noexcept;

    /// The scale the view reports: its window's DPI over 96.
    [[nodiscard]] float scale() const noexcept;

private:
    class Impl;
    std::unique_ptr<Impl> impl_;
};

}  // namespace migo::windows_host

#endif  // MIGO_WINDOWS_WIN32_SURFACE_VIEW_HPP_

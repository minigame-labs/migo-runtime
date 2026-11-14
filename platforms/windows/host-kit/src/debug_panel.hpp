#ifndef MIGO_WINDOWS_HOST_KIT_DEBUG_PANEL_HPP_
#define MIGO_WINDOWS_HOST_KIT_DEBUG_PANEL_HPP_

#include <migo/migo.h>
#include <migo/windows/win32_surface_view.hpp>

#include <chrono>
#include <cstdint>
#include <deque>
#include <optional>
#include <string>

struct HWND__;
struct HFONT__;

namespace migo::windows_host {

/// The debugging panel, shown while the debugging aids are on -- the Linux Host
/// Kit's, for Win32: the frames a second the view delivered and the latest
/// lines content wrote to its console, read from the Session twice a second
/// however fast content writes, over the top 40% of the game. A window of its
/// own, owned by the App's top-level window -- above it, minimized with it --
/// that follows the view as it moves and resizes, and takes no input (layered
/// and transparent to the mouse, never activated), so the game stays playable
/// under it. It follows the newest line.
///
/// GUI thread only. It reads the Session it was made with, so it goes before
/// the Session does.
class DebugPanel final {
public:
    /// The panel keeps the latest lines, as the Session's own buffer keeps its latest.
    static constexpr std::size_t kLineLimit = 200;

    DebugPanel(MigoSession *session, Win32SurfaceView &view);
    ~DebugPanel();

    DebugPanel(const DebugPanel &) = delete;
    DebugPanel &operator=(const DebugPanel &) = delete;

    [[nodiscard]] HWND__ *hwnd() const noexcept { return window_; }

private:
    struct Line {
        int level = 1;
        std::wstring text;
    };

    // Its window procedures, which need <windows.h>: in the source.
    friend struct DebugPanelProcedures;

    void refresh();
    void readConsole();
    void place();
    void paint();
    void makeFont(unsigned dpi);

    MigoSession *const session_;
    Win32SurfaceView &view_;
    HWND__ *root_ = nullptr;
    HWND__ *window_ = nullptr;
    HFONT__ *font_ = nullptr;
    unsigned font_dpi_ = 0;
    bool following_root_ = false;
    bool following_view_ = false;

    std::uint64_t cursor_ = 0;
    std::string buffer_;
    std::deque<Line> lines_;

    std::chrono::steady_clock::time_point sampled_at_{};
    std::uint64_t sampled_frames_ = 0;
    bool sampled_ = false;
    std::optional<double> frames_per_second_;
};

}  // namespace migo::windows_host

#endif  // MIGO_WINDOWS_HOST_KIT_DEBUG_PANEL_HPP_

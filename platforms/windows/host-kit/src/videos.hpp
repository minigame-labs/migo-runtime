#ifndef MIGO_WINDOWS_HOST_KIT_VIDEOS_HPP_
#define MIGO_WINDOWS_HOST_KIT_VIDEOS_HPP_

// Internal to the Host Kit, not installed: createVideo's players.

#include <migo/windows/host_services.hpp>
#include <migo/windows/win32_surface_view.hpp>

#include <cstdint>
#include <memory>
#include <string>

struct HWND__;

namespace migo::windows_host {

/// A video the Host Kit shows in a window of its own -- the preview viewer's --
/// with its poster and its controls; nobody hears what it does. It goes with
/// this object.
class EmbeddedVideo {
public:
    virtual ~EmbeddedVideo() = default;
    /// At `left`, `top`, `width` x `height` pixels of its window's client area;
    /// hidden when empty.
    virtual void place(int left, int top, int width, int height) = 0;
};

/// createVideo's players over one game's view -- the Linux Host Kit's
/// `VideoPlayers`, for Win32.
///
/// Each player is a window of its own over the game, owned by the App's
/// window -- the game is drawn into its view's swap chain, which no sibling can
/// reliably stand above -- placed where content put it (its coordinates the
/// view's logical pixels), cut to the view, following it as it moves, hidden
/// while it is. Played by Media Foundation's media engine in frame-server mode
/// on a Direct3D 11 device -- the GPU's decoders where there are, WARP and the
/// software decoders where there are not -- each frame drawn with Direct2D into
/// the player's own flip-model swap chain, paced by the desktop compositor. As
/// the other SDKs' players, each shows its poster until it plays, fits its
/// picture as objectFit says (contain, fill, cover) on its backgroundColor, and
/// plays, loops, mutes and runs at the rate content's options say. A player
/// under the game (underGameView) is not shown, as on the other SDKs; full
/// screen fills the game's view, a desktop's screen not being turned.
///
/// PLAY is answered once it plays, SEEK once it is there, PAUSE and STOP once
/// done; events report its play, pause, end, an error (once: its calls fail
/// after), a stall and how much it holds, with its position four times a
/// second while it plays. While the game is not running, its players hold
/// still and play on as it comes back. A click on a player is the game's.
/// GUI thread only.
class Videos final {
public:
    Videos(Win32SurfaceView &view, std::shared_ptr<HostServiceChannel> channel);
    /// Every player goes; its calls still unanswered fail ("video destroyed").
    ~Videos();

    Videos(const Videos &) = delete;
    Videos &operator=(const Videos &) = delete;

    /// One of MIGO_VIDEO_*.
    void call(std::uint32_t method, const Json &request, HostServiceReply reply);
    /// Whether the game runs: the players hold still while it does not.
    void setRunning(bool running);

    /// A video shown in `window`, a window of the Host Kit's own: played from
    /// `file` -- an open file HANDLE it takes, `url` naming only its kind -- or,
    /// with none, from `url`. None where videos cannot be played here.
    [[nodiscard]] std::unique_ptr<EmbeddedVideo> embed(HWND__ *window, void *file, const std::string &url,
                                                       const std::string &poster);

private:
    class Impl;
    std::unique_ptr<Impl> impl_;
};

}  // namespace migo::windows_host

#endif  // MIGO_WINDOWS_HOST_KIT_VIDEOS_HPP_

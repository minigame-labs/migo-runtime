#ifndef MIGO_WINDOWS_MANAGED_SESSION_HPP_
#define MIGO_WINDOWS_MANAGED_SESSION_HPP_

#include <migo/migo.h>
#include <migo/windows/builtin_host_services.hpp>
#include <migo/windows/host_services.hpp>
#include <migo/windows/win32_surface_view.hpp>

#include <cstdint>
#include <functional>
#include <memory>
#include <string>

struct HWND__;

namespace migo::windows_host {

/// One Migo Session, owned: its callback table, its view and its teardown --
/// the Managed shape, the Linux Host Kit's `MigoManagedSession` for Win32. The
/// Bound shape is an App driving `Win32SurfaceView` and a `SurfaceHost` with a
/// Session and a callback table of its own.
///
/// It does not own the `MigoEngine`, which one App may share among Sessions:
/// the engine, the parent window and the GUI thread outlive it.
///
/// Engine callbacks arrive on engine threads; each is run on the GUI thread,
/// posted to a message-only window of the session's own, before any of it
/// touches the App. The game runs while the App's top-level window is shown
/// and not minimized -- Windows tells a window of no other way it stops being
/// seen -- and is visible while it runs: both are reported to the Session as
/// they change. Content's frame requests are paced by the view; its
/// setKeepScreenOn holds the display awake (SetThreadExecutionState) while it
/// asks and the game runs. Host services the App names are the App's
/// (`Options::appServices`); the Host Kit answers the others it can
/// (`Win32BuiltInHostServices`), and the rest are refused.
///
/// Teardown is the C ABI's three steps and cannot be shortened: `close()`
/// begins the detach, the release is observed, then the Session is destroyed
/// and `Listener::closed` is called. Destroying this object while it is still
/// running is a programming error and terminates, as the view does.
///
/// GUI thread only: the thread that made it, the Session's owner thread.
class Win32ManagedSession final {
public:
    struct Options {
        /// The network content may reach, as `MigoSessionConfig` takes it;
        /// empty grants none.
        std::string networkPolicyJson;
        /// What the game is launched with; empty launches it with the engine's
        /// defaults.
        std::string launchOptionsJson;
        /// Who the App is, as content reads it (`getAppBaseInfo().host.appId`).
        /// Empty takes the process's AppUserModelID where it has one, and its
        /// executable's name otherwise.
        std::string appId;
        /// The host services the App answers itself, or null. The services it
        /// names are its; it outlives the Session and is called on the GUI
        /// thread.
        HostServiceProvider *appServices = nullptr;
        /// Where the Host Kit's image service stages and saves files.
        FileLocations files;
    };

    /// What the session reports, on the GUI thread; any may be empty.
    struct Listener {
        std::function<void()> contentReady;
        std::function<void(MigoResult code, const std::string &message)> contentFailed;
        std::function<void()> exitRequested;
        /// The Session has been destroyed; this object can be.
        std::function<void()> closed;
        /// A Surface or input call was refused.
        std::function<void(MigoResult code)> migoError;
        /// The debugging aids were turned on or off.
        std::function<void(bool on)> debugAidsChanged;
    };

    /// Creates the Session and its view -- a child of `parent` the App places
    /// and shows -- and installs the callback table, which the C ABI takes
    /// once, before the first attach.
    Win32ManagedSession(MigoEngine &engine, HWND__ *parent, Options options, Listener listener);
    /// The same, with no network, the default launch, no services of the App's
    /// own and nothing listened for.
    Win32ManagedSession(MigoEngine &engine, HWND__ *parent);
    ~Win32ManagedSession();

    Win32ManagedSession(const Win32ManagedSession &) = delete;
    Win32ManagedSession &operator=(const Win32ManagedSession &) = delete;

    /// Whether construction produced a usable Session; the failing step's
    /// result otherwise.
    [[nodiscard]] bool isValid() const noexcept;
    [[nodiscard]] MigoResult lastError() const noexcept;

    /// The game's view, for the App's layout; null when construction failed.
    [[nodiscard]] Win32SurfaceView *view() const noexcept;

    /// Start the content installed as `content_id`. Content needs the view's
    /// Surface: called before the view first attaches -- once the App has
    /// shown it -- the content is loaded then. Loads once: a second call
    /// returns MIGO_ERROR_INVALID_STATE.
    [[nodiscard]] MigoResult loadContent(const std::string &content_id, const std::string &entry = "game.js");

    /// Begin the asynchronous teardown; `Listener::closed` reports its end.
    /// Safe to call more than once.
    [[nodiscard]] MigoResult close();
    [[nodiscard]] bool isClosed() const noexcept;

    /// Whether the debugging aids are on: content's setEnableDebug turns them,
    /// and getAppBaseInfo().enableDebug says so. While they are on, a panel
    /// over the top of the game shows the frames a second the view delivers
    /// and what content writes to its console from then, and takes no input:
    /// the game stays playable under it.
    [[nodiscard]] bool debugAids() const noexcept;

    /// Turn the debugging aids on or off, and tell the Session, which tells
    /// content. An App that answers the screen service itself turns them here.
    /// Returns the Session's answer to the report; MIGO_ERROR_INVALID_STATE
    /// once the Session is closing.
    [[nodiscard]] MigoResult setDebugAids(bool on);

private:
    class Impl;
    std::unique_ptr<Impl> impl_;
};

}  // namespace migo::windows_host

#endif  // MIGO_WINDOWS_MANAGED_SESSION_HPP_

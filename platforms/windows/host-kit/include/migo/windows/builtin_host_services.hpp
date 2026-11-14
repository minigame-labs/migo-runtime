#ifndef MIGO_WINDOWS_BUILTIN_HOST_SERVICES_HPP_
#define MIGO_WINDOWS_BUILTIN_HOST_SERVICES_HPP_

#include <migo/windows/host_services.hpp>
#include <migo/windows/win32_surface_view.hpp>

#include <cstdint>
#include <filesystem>
#include <functional>
#include <memory>

namespace migo::windows_host {

/// The services Windows already offers an App, answered with its own -- the
/// Linux Host Kit's `MigoQtBuiltInHostServices` for Win32:
///
/// Toasts, loading indicators, modals and action sheets: a toast or a loading
/// indicator is a card over the middle of the game -- one at a time, taking no
/// input, with `mask` keeping the game's from it too -- drawn with Direct2D;
/// a modal or an action sheet is a dialog of the system's controls, modal to
/// the App's window without holding its message loop, the buttons in Windows'
/// order. Closing one any other way is the player declining it.
///
/// The clipboard: the system's, as text.
///
/// The window the game is shown in -- the App's top-level window: setWindowSize
/// is the game's size in pixels, the window growing or shrinking by what the
/// view must (refused while maximized or minimized); each change of its state
/// is reported (minimize, maximize, normalize); setCursor sets the cursor over
/// the game from a CSS keyword; the pointer lock is the view's
/// (`Win32SurfaceView::lockPointer`), reported as it is taken and released, and
/// refused -- and said to be -- while the player is not at the game.
///
/// The screen: a desktop's screen does not turn, its window does, so the
/// screen's orientation is the game's shape -- portrait when its view is taller
/// than wide, landscape otherwise -- reported each time the view crosses from
/// one to the other, and setDeviceOrientation turns the window, the view's
/// width and height exchanged (refused while maximized, or when the screen's
/// work area cannot hold it turned). setVisualEffectOnCapture keeps the window
/// out of screenshots and recordings ("hidden": WDA_EXCLUDEFROMCAPTURE, from
/// Windows 10 2004; refused where the system cannot). setEnableDebug is
/// handed to `debug_aids`, for whoever owns the Session to turn its aids.
/// The brightness is the screen the game is on's: an external monitor's
/// through DDC/CI, a laptop panel's through WMI -- a screen with neither fails
/// both calls. Windows tells an App of neither a screenshot nor a recording,
/// so those observers and getScreenRecordingState are refused, as on Linux.
///
/// Images and files: the player's pictures and files through the system's
/// open dialog -- modal to the App's window, run on a thread of its own so the
/// App's message loop goes on -- copied, never lent; compressImage through
/// the Windows Imaging Component; saveImageToPhotosAlbum into the player's
/// Pictures folder and saveFileToDisk through the system's save dialog; the
/// previews in a viewer of the Host Kit's own, http(s) images downloaded
/// first. The camera is not offered; a video in previewMedia is refused until
/// the Host Kit plays videos.
///
/// Location: Windows' location service (Windows.Devices.Geolocation), as its
/// privacy settings let desktop Apps use it; getFuzzyLocation at a city's
/// accuracy, getLocation at a street's or, isHighAccuracy, the finest.
///
/// Videos: createVideo's players, each a window of its own over the game,
/// played by Media Foundation's media engine and drawn with Direct2D.
///
/// Permission: the scopes the desktop decides. `scope.userLocation` (and its
/// background form) is Windows' location privacy setting's to decide.
/// `scope.writePhotosAlbum` is granted -- the player's Pictures folder is
/// theirs and the App's, which a desktop does not gate. Any other scope is the
/// App's to decide, and refused ("auth deny") without an answer of its own: an
/// App that decides scopes answers the permission service itself.
///
/// What an App answers itself it names in its own provider, which the Managed
/// session asks first. `scripts/gen-api-platform-matrix.py` reads the
/// `builtIn |= hostServiceBit(...)` lines of `services()` to count these APIs
/// as provided on Windows.
///
/// GUI thread only. What it shows goes with it.
/// Where the image service keeps what it hands over and what it saves.
struct FileLocations {
    /// The directory the files handed to the Session -- a picked image's copy,
    /// a compressed image -- wait in until it takes them; best on the volume of
    /// the engine's cache, so taking one is a rename. Empty: the user's
    /// temporary directory.
    std::filesystem::path staging;
    /// The player's photo album, which saveImageToPhotosAlbum saves to and the
    /// pickers open in. Empty: their Pictures folder.
    std::filesystem::path album;
};

class Win32BuiltInHostServices final : public HostServiceProvider {
public:
    /// `view` is the game's: its window's top-level is the window these
    /// services are of. `channel` is the Session's, which events are reported
    /// through. `debug_aids` is called with content's setEnableDebug.
    Win32BuiltInHostServices(Win32SurfaceView &view, std::shared_ptr<HostServiceChannel> channel,
                             std::function<void(bool on)> debug_aids = {}, FileLocations files = {});
    ~Win32BuiltInHostServices() override;

    Win32BuiltInHostServices(const Win32BuiltInHostServices &) = delete;
    Win32BuiltInHostServices &operator=(const Win32BuiltInHostServices &) = delete;

    [[nodiscard]] std::uint64_t services() const noexcept override;
    void call(MigoHostService service, std::uint32_t method, const Json &request, HostServiceReply reply) override;

    /// Whether the game runs: what plays over it -- its videos -- holds still
    /// while it does not, and plays on as it comes back. Whoever owns the
    /// Session's lifecycle passes it on.
    void setRunning(bool running);

    /// The view's pointer lock was taken or released: reported to content.
    /// Whoever listens to the view (`Win32SurfaceView::Listener::pointerLockChanged`)
    /// passes it on.
    void pointerLockChanged(bool locked);

private:
    class Impl;
    std::unique_ptr<Impl> impl_;
};

}  // namespace migo::windows_host

#endif  // MIGO_WINDOWS_BUILTIN_HOST_SERVICES_HPP_

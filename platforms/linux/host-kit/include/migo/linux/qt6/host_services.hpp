#ifndef MIGO_LINUX_QT6_HOST_SERVICES_HPP_
#define MIGO_LINUX_QT6_HOST_SERVICES_HPP_

#include <migo/linux/qt6/x11_surface_view.hpp>
#include <migo/migo.h>

#include <QJsonObject>
#include <QObject>
#include <QPointer>
#include <QString>

#include <atomic>
#include <cstdint>
#include <memory>
#include <mutex>
#include <optional>

class QWidget;

namespace migo::linux_host::qt6 {

/// The bit a service takes in `MigoHostCallbacks.host_services`.
[[nodiscard]] constexpr std::uint64_t hostServiceBit(MigoHostService service) noexcept {
    return std::uint64_t{1} << service;
}

class MigoQtHostServiceReply;

/// The way back to one Session for its host-service answers.
///
/// A dialog can be answered after the game that asked has gone, so an answer
/// does not hold the Session itself: it holds this channel, and the channel is
/// closed before the Session is destroyed. An answer through a closed channel
/// reaches nobody, which is what the C ABI does for a call whose content has
/// gone. `MigoManagedSession` opens and closes its own; an App that owns its
/// Session (the Bound shape) opens one and closes it before
/// `migo_session_destroy`.
class MigoQtHostServiceChannel final : public std::enable_shared_from_this<MigoQtHostServiceChannel> {
public:
    [[nodiscard]] static std::shared_ptr<MigoQtHostServiceChannel> open(MigoSession *session);

    MigoQtHostServiceChannel(const MigoQtHostServiceChannel &) = delete;
    MigoQtHostServiceChannel &operator=(const MigoQtHostServiceChannel &) = delete;

    /// Stops every answer and event made through this channel from reaching the
    /// Session. Waits for one in progress, so once it returns the Session can
    /// be destroyed.
    void close() noexcept;

    /// The answer to the call `call_id` names; a command (call id 0) has none.
    [[nodiscard]] MigoQtHostServiceReply reply(std::uint64_t call_id);

    /// Reports one of a declared service's events
    /// (`migo_session_post_host_service_event`). MIGO_OK once closed: there is
    /// no running content left to tell.
    MigoResult post(MigoHostService service, std::uint32_t event, const QJsonObject &payload);

private:
    explicit MigoQtHostServiceChannel(MigoSession *session) noexcept : session_(session) {}

    friend class MigoQtHostServiceReply;
    MigoResult complete(std::uint64_t call_id, const MigoHostServiceResult &result);

    std::mutex mutex_;
    MigoSession *session_;
};

/// The answer to one request content made of the host.
///
/// A call is answered exactly once, from any thread: the first `succeed` or
/// `fail` completes it, and any later one returns MIGO_ERROR_INVALID_STATE
/// without reaching the Session. Copies share that one answer, so a reply can
/// travel through Qt connections. A command has no answer: `expectsAnswer()`
/// is false and answering it does nothing.
class MigoQtHostServiceReply final {
public:
    [[nodiscard]] bool expectsAnswer() const noexcept { return call_id_ != 0; }

    /// Completes the call with `result`, a JSON object of the method's fields.
    MigoResult succeed(const QJsonObject &result = {});

    /// Fails the call. `reason` is required -- an empty one is refused with
    /// MIGO_ERROR_INVALID_ARGUMENT and leaves the call unanswered -- and does not
    /// name the API: content reads "<api>:fail <reason>". `error_code` is the
    /// API's numeric code, where it has one.
    MigoResult fail(const QString &reason, std::optional<std::int32_t> error_code = std::nullopt);

private:
    friend class MigoQtHostServiceChannel;
    MigoQtHostServiceReply(std::shared_ptr<MigoQtHostServiceChannel> channel, std::uint64_t call_id);

    MigoResult answer(const MigoHostServiceResult &result);

    std::shared_ptr<MigoQtHostServiceChannel> channel_;
    std::uint64_t call_id_;
    std::shared_ptr<std::atomic<bool>> answered_;
};

/// Something that answers host services: the App's own integrations, or the
/// Host Kit's built-in ones.
class MigoQtHostServiceProvider {
public:
    virtual ~MigoQtHostServiceProvider() = default;

    /// The services answered, one `hostServiceBit` per MIGO_HOST_SERVICE_*.
    /// Read once, when the Session's callback table is installed.
    [[nodiscard]] virtual std::uint64_t services() const noexcept = 0;

    /// One request for a service this provider answers, on the GUI thread.
    /// `request` is the method's options. A call is answered through `reply`,
    /// now or later; a command is carried out.
    virtual void call(MigoHostService service, std::uint32_t method, const QJsonObject &request,
                      MigoQtHostServiceReply reply) = 0;
};

/// The services Qt already offers an App, answered with Qt's own: the
/// clipboard; the game's toasts, loading indicator, modals and action sheets;
/// the window the game is shown in -- its size, its state, the cursor over the
/// game; the screen; and images -- compressing them, picking and saving files.
///
/// An App that names one of these services answers it instead; one that wants
/// a service refused names it and refuses. `scripts/gen-api-platform-matrix.py`
/// reads the `builtIn |= hostServiceBit(...)` lines of `services()` to count
/// these APIs as provided on Linux.
///
/// A toast and a loading indicator are one at a time: either replaces the
/// other, hideToast hides only a toast and hideLoading only a loading
/// indicator, and a toast hides itself after its duration. One with a mask
/// keeps the game's input from it while shown. Button colours are not applied:
/// a desktop's dialogs are the desktop's.
///
/// The window: `setWindowSize` is the game's size in pixels, the window growing
/// or shrinking by what the view must (refused while the window is full
/// screen); a change of the window's state is reported (minimize, maximize --
/// full screen included -- normalize); `setCursor` sets the cursor over the
/// game, a CSS keyword or an image and its hotspot; a pointer lock is the
/// view's (`MigoQtX11SurfaceView::lockPointer`), reported as it is taken and
/// released, and refused -- and said to be -- while the player is not at the game.
///
/// The screen: a desktop's screen does not turn, its window does, so the
/// screen's orientation is the game's shape -- portrait when its view is taller
/// than wide, landscape otherwise -- reported each time the view crosses from
/// one to the other, and `setDeviceOrientation` turns the window, the view's
/// width and height exchanged (refused while full screen, or when the screen
/// cannot hold the window turned). `setEnableDebug` is `debugAidsRequested`, for
/// whoever owns the Session to turn its debugging aids (`MigoManagedSession`
/// does). The brightness is the backlight's, read where the kernel shows it and
/// set through logind -- refused on a screen with none, an external monitor's or
/// a virtual machine's. The rest of the screen is refused: no Linux desktop
/// tells an app of a screenshot or a recording, nor keeps a window out of one.
///
/// Images: compressImage with Qt's image readers and writers (upright, scaled as
/// asked; a PNG stays a PNG, anything else becomes a JPEG at the quality asked);
/// chooseImage, chooseMedia and chooseMessageFile through the file dialog -- the
/// desktop's, through its portal, wherever Qt finds one -- answered with copies
/// of what the player picked; saveFileToDisk through the dialog to save; and
/// saveImageToPhotosAlbum into the player's Pictures folder. A file answered
/// with is staged in the App's cache (`QStandardPaths::CacheLocation`) until the
/// Session takes it; one it does not take is removed. previewImage and
/// previewMedia show the images in a viewer over the game, each held open --
/// an http(s) one downloaded -- so the engine may take back what it lent once
/// the call is answered. The camera and videos are refused.
///
/// GUI thread only. What is on screen goes with this object.
class Backlight;
class Images;

class MigoQtBuiltInHostServices final : public QObject, public MigoQtHostServiceProvider {
    Q_OBJECT

public:
    /// `view` is the game's: dialogs are its window's, indicators stand over it,
    /// a mask keeps its input from the game, and the window service is its
    /// window's. `channel` is the Session's, which the window's events are
    /// reported through.
    MigoQtBuiltInHostServices(MigoQtX11SurfaceView &view, std::shared_ptr<MigoQtHostServiceChannel> channel,
                              QObject *parent = nullptr);
    ~MigoQtBuiltInHostServices() override;

    [[nodiscard]] std::uint64_t services() const noexcept override;
    void call(MigoHostService service, std::uint32_t method, const QJsonObject &request,
              MigoQtHostServiceReply reply) override;

    /// The indicator shown, for an App or a test that reads what the player
    /// sees: its title, or empty when none is.
    [[nodiscard]] QString indicatorTitle() const;

Q_SIGNALS:
    /// Content's setEnableDebug: the debugging aids on or off. Content is told
    /// the new state by the Session once its owner reports it
    /// (`MIGO_HOST_APP_INFO_FLAG_DEBUG`).
    void debugAidsRequested(bool on);

private:
    void screen(std::uint32_t method, const QJsonObject &request, MigoQtHostServiceReply reply);

    class Interaction;
    class WindowService;
    QPointer<QWidget> surface_;
    std::unique_ptr<Interaction> interaction_;
    std::unique_ptr<WindowService> window_;
    std::unique_ptr<Backlight> backlight_;
    std::unique_ptr<Images> images_;
};

}  // namespace migo::linux_host::qt6

#endif  // MIGO_LINUX_QT6_HOST_SERVICES_HPP_

#ifndef MIGO_LINUX_QT6_HOST_SERVICES_HPP_
#define MIGO_LINUX_QT6_HOST_SERVICES_HPP_

#include <migo/linux/qt6/x11_surface_view.hpp>
#include <migo/migo.h>

#include <QImage>
#include <QJsonObject>
#include <QObject>
#include <QPointer>
#include <QSize>
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

    /// Records the standing decision for one of content's scopes
    /// (`migo_session_set_scope_state`), which every gated call is checked
    /// against. MIGO_OK once closed.
    MigoResult setScopeState(MigoScope scope, MigoScopeState state);

    /// Delivers one motion reading (`migo_session_post_sensor_sample`). Any
    /// thread; MIGO_OK once closed.
    MigoResult postSensorSample(const MigoSensorSample &sample);

    /// Delivers one characteristic's value, read or notified
    /// (`migo_session_post_ble_characteristic_value`). MIGO_OK once closed.
    MigoResult postBleCharacteristicValue(const QString &device, const QString &service, const QString &characteristic,
                                          const QByteArray &value);

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

/// Videos, for the image service. The Host Kit's core links no media framework:
/// the optional `migo::qt6-media` component is one (`MigoQtMedia`), and
/// without one chooseMedia and previewMedia refuse videos.
class MigoQtVideoSupport {
public:
    virtual ~MigoQtVideoSupport() = default;

    /// A video as chooseMedia describes it.
    struct Description {
        /// How long it plays, in seconds.
        double durationSeconds = 0;
        /// Its size as it is shown: upright, in square pixels.
        QSize size;
        /// Its first frame as it is shown; null when none could be decoded.
        QImage cover;
    };

    /// The video in `file`, or none when there is no video in it this plays.
    /// Any thread; blocks while it reads the file.
    [[nodiscard]] virtual std::optional<Description> describe(const QString &file) const = 0;

    /// A widget, a child of `parent`, playing `source` -- a file's path or an
    /// http(s) URL -- with its sound, and the controls to pause it and move
    /// through it; `poster` stands in its place until it first plays, its first
    /// frame when null. It stops when it goes. GUI thread.
    [[nodiscard]] virtual QWidget *player(const QString &source, const QImage &poster, QWidget &parent) const = 0;
};

/// The services Qt already offers an App, answered with Qt's own: the
/// clipboard; the game's toasts, loading indicator, modals and action sheets;
/// the window the game is shown in -- its size, its state, the cursor over the
/// game; the screen; images -- compressing them, picking and saving files; and
/// where the device is, with the scopes the desktop decides.
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
/// the call is answered. Videos are picked -- described by their length, size
/// and cover -- and previewed, played in the viewer, where the App gives the
/// support to (`MigoQtVideoSupport`), and refused where it does not. The camera
/// is refused.
///
/// Location: GeoClue's, on the system bus outside a sandbox and through the
/// desktop portal's Location inside one. getLocation asks it at street level,
/// or exactly with `isHighAccuracy`, and waits ten seconds for a position --
/// `highAccuracyExpireTime` instead, when given with it -- failing with
/// "unable to get location" when none comes; getFuzzyLocation asks at city
/// level, the engine rounding what it answers. GeoClue names the App to its
/// agent by `QGuiApplication::desktopFileName()`, or its application name.
///
/// Bluetooth: BlueZ's, as a BLE central -- the adapter, scanning, devices,
/// connections and GATT, answered as the Apple SDK answers them, with the same
/// codes; a device is named by its address. A connected device's RSSI (BlueZ
/// gives no app one), pairing and setting the MTU (the reference's for Android
/// only) and the peripheral mode are refused. iBeacons are heard in the BLE
/// advertisements BlueZ reports, on a scan of their own, and reported every
/// second as CoreLocation ranges them.
///
/// Motion: the accelerometer, gyroscope and magnetometer the kernel shows
/// through IIO, in the screen's frame (udev's mount matrix, else the kernel's),
/// read at the interval content asks for; the compass and the device's
/// orientation worked out from gravity and the field as Android's SensorManager
/// does. A sensor the device lacks is refused with what it lacks.
///
/// Permission: the scopes the desktop decides. `scope.userLocation` (and its
/// background form: a desktop has no other) is GeoClue's to decide -- its agent
/// or the portal asking the player the first time the App asks -- and is
/// recorded as decided; `scope.writePhotosAlbum` and `scope.bluetooth` are
/// granted, the player's Pictures folder and Bluetooth being theirs and the
/// App's, which a desktop does not gate.
/// Any other scope is the App's to decide, and refused ("auth deny") without
/// an answer of its own: an App that decides scopes answers the permission
/// service itself.
///
/// GUI thread only. What is on screen goes with this object.
class Backlight;
class Beacons;
class Bluetooth;
class Geolocation;
class Images;
class MotionSensors;

class MigoQtBuiltInHostServices final : public QObject, public MigoQtHostServiceProvider {
    Q_OBJECT

public:
    /// `view` is the game's: dialogs are its window's, indicators stand over it,
    /// a mask keeps its input from the game, and the window service is its
    /// window's. `channel` is the Session's, which the window's events are
    /// reported through.
    MigoQtBuiltInHostServices(MigoQtX11SurfaceView &view, std::shared_ptr<MigoQtHostServiceChannel> channel,
                              QObject *parent = nullptr);
    /// The same, with the videos `videos` describes and plays; null refuses
    /// videos. Shared, since describing one outlasts a call whose Session goes.
    MigoQtBuiltInHostServices(MigoQtX11SurfaceView &view, std::shared_ptr<MigoQtHostServiceChannel> channel,
                              std::shared_ptr<const MigoQtVideoSupport> videos, QObject *parent = nullptr);
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
    void location(std::uint32_t method, const QJsonObject &request, MigoQtHostServiceReply reply);
    void permission(std::uint32_t method, const QJsonObject &request, MigoQtHostServiceReply reply);
    void motion(std::uint32_t method, const QJsonObject &request, MigoQtHostServiceReply reply);

    class Interaction;
    class WindowService;
    QPointer<QWidget> surface_;
    std::shared_ptr<MigoQtHostServiceChannel> channel_;
    std::unique_ptr<Interaction> interaction_;
    std::unique_ptr<WindowService> window_;
    std::unique_ptr<Backlight> backlight_;
    std::unique_ptr<Images> images_;
    std::unique_ptr<Geolocation> geolocation_;
    /// Made the first time content starts a sensor.
    std::unique_ptr<MotionSensors> motion_;
    std::unique_ptr<Bluetooth> bluetooth_;
    /// Made the first time content looks for beacons, with a connection of its own.
    std::unique_ptr<Beacons> beacons_;
};

}  // namespace migo::linux_host::qt6

#endif  // MIGO_LINUX_QT6_HOST_SERVICES_HPP_

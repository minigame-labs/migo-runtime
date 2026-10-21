#include "backlight.hpp"
#include "bluetooth.hpp"
#include "fake_migo.hpp"
#include "geolocation.hpp"
#include "image_viewer.hpp"
#include "motion.hpp"
#include "x11_player.hpp"

#include <migo/linux/qt6/host_services.hpp>
#include <migo/linux/qt6/managed_session.hpp>

#include <QAction>
#include <QApplication>
#include <QClipboard>
#include <QCursor>
#include <QDBusConnection>
#include <QDBusContext>
#include <QDBusArgument>
#include <QDBusError>
#include <QDBusMetaType>
#include <QDBusMessage>
#include <QDBusObjectPath>
#include <QDeadlineTimer>
#include <QDir>
#include <QFile>
#include <QDialog>
#include <QFileDialog>
#include <QFileInfo>
#include <QHash>
#include <QHostAddress>
#include <QImageReader>
#include <QImageWriter>
#include <QJsonArray>
#include <QJsonDocument>
#include <QJsonObject>
#include <QLabel>
#include <QLineEdit>
#include <QMenu>
#include <QPixmap>
#include <QPlainTextEdit>
#include <QPushButton>
#include <QRegularExpression>
#include <QScreen>
#include <QStandardPaths>
#include <QTcpServer>
#include <QTcpSocket>
#include <QTemporaryDir>
#include <QVBoxLayout>
#include <QtTest>

#include <algorithm>
#include <cfloat>
#include <chrono>
#include <map>
#include <memory>
#include <mutex>
#include <optional>

using migo::linux_host::SurfaceHost;
using migo::linux_host::SurfaceState;
using migo::linux_host::qt6::Bluetooth;
using migo::linux_host::qt6::BluezInterfaces;
using migo::linux_host::qt6::BluezObjects;
using migo::linux_host::qt6::Geolocation;
using migo::linux_host::qt6::hostServiceBit;
using migo::linux_host::qt6::ImageViewer;
using migo::linux_host::qt6::MigoManagedSession;
using migo::linux_host::qt6::MigoQtBuiltInHostServices;
using migo::linux_host::qt6::MigoQtHostServiceChannel;
using migo::linux_host::qt6::MigoQtHostServiceProvider;
using migo::linux_host::qt6::MigoQtHostServiceReply;
using migo::linux_host::qt6::MigoQtX11SurfaceView;
using migo::linux_host::qt6::MotionSensors;

namespace {

bool require_xcb(const char *why, const char *file, int line) {
    if (QGuiApplication::platformName() == QStringLiteral("xcb")) return true;
    QTest::qSkip(why, file, line);
    return false;
}

std::string json(const QJsonObject &object) {
    return QJsonDocument(object).toJson(QJsonDocument::Compact).toStdString();
}

QJsonObject parsed(const std::string &payload) {
    return QJsonDocument::fromJson(QByteArray::fromStdString(payload)).object();
}

/// The answer the host gave the call `call_id`, if it has.
std::optional<fake_migo::HostServiceAnswer> answerTo(std::uint64_t call_id) {
    for (const auto &answer : fake_migo::host_service_answers()) {
        if (answer.call_id == call_id) return answer;
    }
    return std::nullopt;
}

/// The indicator standing over the game, or null.
QWidget *indicator() {
    for (QWidget *widget : QApplication::topLevelWidgets()) {
        if (widget->objectName() == QStringLiteral("migoIndicator") && widget->isVisible()) return widget;
    }
    return nullptr;
}

QString indicatorText() {
    QWidget *shown = indicator();
    if (shown == nullptr) return {};
    const auto labels = shown->findChildren<QLabel *>();
    for (QLabel *label : labels) {
        if (!label->text().isEmpty()) return label->text();
    }
    return {};
}

template <typename Window>
Window *shownWindow() {
    for (QWidget *widget : QApplication::topLevelWidgets()) {
        if (auto *window = qobject_cast<Window *>(widget); window != nullptr && window->isVisible()) return window;
    }
    return nullptr;
}

QPushButton *buttonNamed(QWidget &window, const QString &text) {
    const auto buttons = window.findChildren<QPushButton *>();
    for (QPushButton *button : buttons) {
        if (button->text() == text) return button;
    }
    return nullptr;
}

/// A Managed Session in a window of its own, closed on the way out so a failed
/// assertion reports itself rather than the wrapper's destroyed-while-open abort.
struct Managed {
    QWidget container;
    MigoManagedSession session;

    explicit Managed(const MigoManagedSession::Options &options = {})
        : session(*fake_migo::engine(), container, options) {}
    ~Managed() {
        if (!session.isClosed()) (void)session.close();
        // A view that attached (xcb) retires first: the library reports RELEASED,
        // and only then is the Session closed.
        if (!session.isClosed() && session.view() != nullptr) {
            fake_migo::set_release_status(session.view()->generation(), MIGO_SURFACE_RELEASE_RELEASED);
            QDeadlineTimer deadline(5000);
            while (!session.isClosed() && !deadline.hasExpired()) {
                QCoreApplication::processEvents(QEventLoop::AllEvents, 20);
            }
        }
    }
};

/// logind's part in a backlight, on the session bus: what it is asked to set,
/// written where the kernel shows the backlight, as logind has the kernel do.
class FakeLogind final : public QObject, protected QDBusContext {
    Q_OBJECT
    Q_CLASSINFO("D-Bus Interface", "org.freedesktop.login1.Session")

public:
    explicit FakeLogind(QString sysfs) : sysfs_(std::move(sysfs)) {}
    QStringList asked;
    bool refuse = false;

public Q_SLOTS:
    void SetBrightness(const QString &subsystem, const QString &name, uint brightness) {
        asked << QStringLiteral("%1 %2 %3").arg(subsystem, name).arg(brightness);
        if (refuse) {
            sendErrorReply(QDBusError::AccessDenied, QStringLiteral("not the active session"));
            return;
        }
        QFile file(sysfs_ + QLatin1Char('/') + name + QStringLiteral("/brightness"));
        if (file.open(QIODevice::WriteOnly)) file.write(QByteArray::number(brightness) + '\n');
    }

private:
    QString sysfs_;
};

/// Where GeoClue says the device is.
class FakeGeoClueLocation final : public QObject {
    Q_OBJECT
    Q_CLASSINFO("D-Bus Interface", "org.freedesktop.GeoClue2.Location")
    Q_PROPERTY(double Latitude MEMBER latitude)
    Q_PROPERTY(double Longitude MEMBER longitude)
    Q_PROPERTY(double Accuracy MEMBER accuracy)
    Q_PROPERTY(double Altitude MEMBER altitude)
    Q_PROPERTY(double Speed MEMBER speed)

public:
    double latitude = 31.2304;
    double longitude = 121.4737;
    double accuracy = 25;
    // GeoClue's "unknown" for each.
    double altitude = -DBL_MAX;
    double speed = -1;
};

class FakeGeoClue;

/// One of GeoClue's clients: started as its agent decides.
class FakeGeoClueClient final : public QObject, protected QDBusContext {
    Q_OBJECT
    Q_CLASSINFO("D-Bus Interface", "org.freedesktop.GeoClue2.Client")
    Q_PROPERTY(QString DesktopId MEMBER desktop_id)
    Q_PROPERTY(uint RequestedAccuracyLevel MEMBER accuracy_level)

public:
    FakeGeoClueClient(FakeGeoClue &geoclue, QString path) : geoclue_(geoclue), path_(std::move(path)) {}
    QString desktop_id;
    uint accuracy_level = 0;

public Q_SLOTS:
    void Start();
    void Stop() {}

private:
    FakeGeoClue &geoclue_;
    const QString path_;
};

/// GeoClue, on the session bus: its agent grants or refuses, and the position
/// is reported on starting, or not at all.
class FakeGeoClue final : public QObject {
    Q_OBJECT
    Q_CLASSINFO("D-Bus Interface", "org.freedesktop.GeoClue2.Manager")

public:
    explicit FakeGeoClue(QDBusConnection bus) : bus_(std::move(bus)) {
        registered_ = bus_.registerService(QStringLiteral("org.freedesktop.GeoClue2")) &&
                      bus_.registerObject(QStringLiteral("/org/freedesktop/GeoClue2/Manager"), this,
                                          QDBusConnection::ExportAllSlots) &&
                      bus_.registerObject(QStringLiteral("/org/freedesktop/GeoClue2/Location/1"), &location,
                                          QDBusConnection::ExportAllProperties);
    }
    ~FakeGeoClue() override {
        for (const auto &client : clients) bus_.unregisterObject(client.first);
        bus_.unregisterObject(QStringLiteral("/org/freedesktop/GeoClue2/Location/1"));
        bus_.unregisterObject(QStringLiteral("/org/freedesktop/GeoClue2/Manager"));
        bus_.unregisterService(QStringLiteral("org.freedesktop.GeoClue2"));
    }
    [[nodiscard]] bool registered() const { return registered_; }

    bool refuse = false;
    bool report = true;
    int created = 0;
    int deleted = 0;
    /// "<desktop id> <accuracy level>", each time a client starts.
    QStringList started;
    FakeGeoClueLocation location;
    std::map<QString, std::unique_ptr<FakeGeoClueClient>> clients;
    QDBusConnection bus_;

public Q_SLOTS:
    QDBusObjectPath CreateClient() {
        const QString path = QStringLiteral("/org/freedesktop/GeoClue2/Client/%1").arg(++created);
        auto client = std::make_unique<FakeGeoClueClient>(*this, path);
        bus_.registerObject(path, client.get(), QDBusConnection::ExportAllSlots | QDBusConnection::ExportAllProperties);
        clients.emplace(path, std::move(client));
        return QDBusObjectPath(path);
    }
    void DeleteClient(const QDBusObjectPath &client) {
        ++deleted;
        bus_.unregisterObject(client.path());
    }

private:
    bool registered_ = false;
};

void FakeGeoClueClient::Start() {
    geoclue_.started << QStringLiteral("%1 %2").arg(desktop_id).arg(accuracy_level);
    if (geoclue_.refuse) {
        sendErrorReply(QDBusError::AccessDenied, QStringLiteral("'%1' disallowed by the agent").arg(desktop_id));
        return;
    }
    if (!geoclue_.report) return;
    QDBusMessage updated =
        QDBusMessage::createSignal(path_, QStringLiteral("org.freedesktop.GeoClue2.Client"), QStringLiteral("LocationUpdated"));
    updated << QVariant::fromValue(QDBusObjectPath(QStringLiteral("/")))
            << QVariant::fromValue(QDBusObjectPath(QStringLiteral("/org/freedesktop/GeoClue2/Location/1")));
    geoclue_.bus_.send(updated);
}

/// A portal session, which the App closes.
class FakePortalSession final : public QObject {
    Q_OBJECT
    Q_CLASSINFO("D-Bus Interface", "org.freedesktop.portal.Session")

public:
    int closed = 0;

public Q_SLOTS:
    void Close() { ++closed; }
};

/// The desktop portal's Location, on the session bus: the player answers its
/// dialog with `response` (0 allowed, 1 refused), and it reports `location`.
class FakeLocationPortal final : public QObject, protected QDBusContext {
    Q_OBJECT
    Q_CLASSINFO("D-Bus Interface", "org.freedesktop.portal.Location")

public:
    explicit FakeLocationPortal(QDBusConnection bus) : bus_(std::move(bus)) {
        registered_ = bus_.registerService(QStringLiteral("org.freedesktop.portal.Desktop")) &&
                      bus_.registerObject(QStringLiteral("/org/freedesktop/portal/desktop"), this,
                                          QDBusConnection::ExportAllSlots);
    }
    ~FakeLocationPortal() override {
        if (!session_.isEmpty()) bus_.unregisterObject(session_);
        bus_.unregisterObject(QStringLiteral("/org/freedesktop/portal/desktop"));
        bus_.unregisterService(QStringLiteral("org.freedesktop.portal.Desktop"));
    }
    [[nodiscard]] bool registered() const { return registered_; }

    uint response = 0;
    QVariantMap location{{QStringLiteral("Latitude"), 48.8566}, {QStringLiteral("Longitude"), 2.3522},
                         {QStringLiteral("Accuracy"), 40.0}, {QStringLiteral("Altitude"), 35.0},
                         {QStringLiteral("Speed"), -1.0}};
    /// "<accuracy> <parent window>", each time a session starts.
    QStringList asked;
    FakePortalSession session;

public Q_SLOTS:
    QDBusObjectPath CreateSession(const QVariantMap &options) {
        accuracy_ = options.value(QStringLiteral("accuracy")).toUInt();
        session_ = QStringLiteral("/org/freedesktop/portal/desktop/session/%1/%2")
                       .arg(escaped(), options.value(QStringLiteral("session_handle_token")).toString());
        bus_.registerObject(session_, &session, QDBusConnection::ExportAllSlots);
        return QDBusObjectPath(session_);
    }
    QDBusObjectPath Start(const QDBusObjectPath &started, const QString &parent, const QVariantMap &options) {
        asked << QStringLiteral("%1 %2").arg(accuracy_).arg(parent);
        const QString request = QStringLiteral("/org/freedesktop/portal/desktop/request/%1/%2")
                                    .arg(escaped(), options.value(QStringLiteral("handle_token")).toString());
        // The dialog answers later, as a player does.
        QTimer::singleShot(0, this, [this, request, path = started.path()] {
            QDBusMessage answered = QDBusMessage::createSignal(request, QStringLiteral("org.freedesktop.portal.Request"),
                                                               QStringLiteral("Response"));
            answered << response << QVariantMap();
            bus_.send(answered);
            if (response != 0) return;
            QDBusMessage updated = QDBusMessage::createSignal(QStringLiteral("/org/freedesktop/portal/desktop"),
                                                              QStringLiteral("org.freedesktop.portal.Location"),
                                                              QStringLiteral("LocationUpdated"));
            updated << QVariant::fromValue(QDBusObjectPath(path)) << location;
            bus_.send(updated);
        });
        return QDBusObjectPath(request);
    }

private:
    /// The caller's unique name as a portal puts it in a path.
    QString escaped() const {
        QString sender = message().service();
        return sender.remove(0, 1).replace(QLatin1Char('.'), QLatin1Char('_'));
    }

    QDBusConnection bus_;
    bool registered_ = false;
    uint accuracy_ = 0;
    QString session_;
};

const QString kAdapterPath = QStringLiteral("/org/bluez/hci0");
const QString kDevicePath = QStringLiteral("/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF");
const QString kServicePath = QStringLiteral("/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF/service000a");
const QString kCharacteristicPath = QStringLiteral("/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF/service000a/char000b");

class FakeBluez;

/// BlueZ's adapter: scanning, with the filter it was given.
class FakeBluezAdapter final : public QObject {
    Q_OBJECT
    Q_CLASSINFO("D-Bus Interface", "org.bluez.Adapter1")

public:
    explicit FakeBluezAdapter(FakeBluez &bluez) : bluez_(bluez) {}
    QVariantMap filter;
    int started = 0;
    int stopped = 0;

public Q_SLOTS:
    void SetDiscoveryFilter(const QVariantMap &given) { filter = given; }
    void StartDiscovery();
    void StopDiscovery() { ++stopped; }

private:
    FakeBluez &bluez_;
};

/// A device: connected, its services resolved -- or refusing, or never resolving.
class FakeBluezDevice final : public QObject, protected QDBusContext {
    Q_OBJECT
    Q_CLASSINFO("D-Bus Interface", "org.bluez.Device1")

public:
    explicit FakeBluezDevice(FakeBluez &bluez) : bluez_(bluez) {}
    bool refuse = false;
    bool resolve = true;
    int disconnected = 0;

public Q_SLOTS:
    void Connect();
    void Disconnect() { ++disconnected; }

private:
    FakeBluez &bluez_;
};

/// A characteristic: read, written, notifying.
class FakeBluezCharacteristic final : public QObject {
    Q_OBJECT
    Q_CLASSINFO("D-Bus Interface", "org.bluez.GattCharacteristic1")

public:
    QByteArray value = QByteArray::fromHex("0a0b");
    QStringList written;
    bool notifying = false;

public Q_SLOTS:
    QByteArray ReadValue(const QVariantMap &) { return value; }
    void WriteValue(const QByteArray &bytes, const QVariantMap &options) {
        written << QString::fromLatin1(bytes.toHex()) + QLatin1Char(' ') + options.value(QStringLiteral("type")).toString();
    }
    void StartNotify() { notifying = true; }
    void StopNotify() { notifying = false; }
};

/// BlueZ on the session bus: one adapter, one device with a heart-rate
/// service, its ObjectManager answering what it holds and reporting changes.
class FakeBluez final : public QObject {
    Q_OBJECT
    Q_CLASSINFO("D-Bus Interface", "org.freedesktop.DBus.ObjectManager")

public:
    explicit FakeBluez(QDBusConnection bus, bool powered = true)
        : bus_(std::move(bus)), adapter(*this), device(*this) {
        qDBusRegisterMetaType<BluezInterfaces>();
        qDBusRegisterMetaType<BluezObjects>();
        QDBusArgument manufacturer;
        manufacturer.beginMap(QMetaType::fromType<quint16>(), QMetaType::fromType<QDBusVariant>());
        manufacturer.beginMapEntry();
        manufacturer << quint16{0x004c} << QDBusVariant(QByteArray::fromHex("0215"));
        manufacturer.endMapEntry();
        manufacturer.endMap();
        objects[QDBusObjectPath(kAdapterPath)][QStringLiteral("org.bluez.Adapter1")] = {
            {QStringLiteral("Powered"), powered}, {QStringLiteral("Discovering"), false}};
        objects[QDBusObjectPath(kDevicePath)][QStringLiteral("org.bluez.Device1")] = {
            {QStringLiteral("Address"), QStringLiteral("AA:BB:CC:DD:EE:FF")},
            {QStringLiteral("Name"), QStringLiteral("Heart")},
            {QStringLiteral("UUIDs"), QStringList{QStringLiteral("0000180d-0000-1000-8000-00805f9b34fb")}},
            {QStringLiteral("ManufacturerData"), QVariant::fromValue(manufacturer)},
            {QStringLiteral("Connected"), false},
            {QStringLiteral("ServicesResolved"), false}};
        objects[QDBusObjectPath(kServicePath)][QStringLiteral("org.bluez.GattService1")] = {
            {QStringLiteral("UUID"), QStringLiteral("0000180d-0000-1000-8000-00805f9b34fb")},
            {QStringLiteral("Primary"), true},
            {QStringLiteral("Device"), QVariant::fromValue(QDBusObjectPath(kDevicePath))}};
        objects[QDBusObjectPath(kCharacteristicPath)][QStringLiteral("org.bluez.GattCharacteristic1")] = {
            {QStringLiteral("UUID"), QStringLiteral("00002a37-0000-1000-8000-00805f9b34fb")},
            {QStringLiteral("Service"), QVariant::fromValue(QDBusObjectPath(kServicePath))},
            {QStringLiteral("Flags"), QStringList{QStringLiteral("read"), QStringLiteral("write"), QStringLiteral("notify")}},
            {QStringLiteral("MTU"), QVariant::fromValue(quint16{185})}};
        registered_ = bus_.registerService(QStringLiteral("org.bluez")) &&
                      bus_.registerObject(QStringLiteral("/"), this, QDBusConnection::ExportAllSlots) &&
                      bus_.registerObject(kAdapterPath, &adapter, QDBusConnection::ExportAllSlots) &&
                      bus_.registerObject(kDevicePath, &device, QDBusConnection::ExportAllSlots) &&
                      bus_.registerObject(kCharacteristicPath, &characteristic, QDBusConnection::ExportAllSlots);
    }
    ~FakeBluez() override {
        for (const QString &path : {kCharacteristicPath, kDevicePath, kAdapterPath, QStringLiteral("/")}) {
            bus_.unregisterObject(path);
        }
        bus_.unregisterService(QStringLiteral("org.bluez"));
    }
    [[nodiscard]] bool registered() const { return registered_; }

    /// Changes `properties` of `path`'s `interface`, and says so as BlueZ does.
    void change(const QString &path, const QString &interface, const QVariantMap &properties) {
        for (auto value = properties.cbegin(); value != properties.cend(); ++value) {
            objects[QDBusObjectPath(path)][interface].insert(value.key(), value.value());
        }
        QDBusMessage changed = QDBusMessage::createSignal(path, QStringLiteral("org.freedesktop.DBus.Properties"),
                                                          QStringLiteral("PropertiesChanged"));
        changed << interface << properties << QStringList();
        bus_.send(changed);
    }

    BluezObjects objects;
    FakeBluezAdapter adapter;
    FakeBluezDevice device;
    FakeBluezCharacteristic characteristic;

public Q_SLOTS:
    migo::linux_host::qt6::BluezObjects GetManagedObjects() { return objects; }

private:
    QDBusConnection bus_;
    bool registered_ = false;
};

void FakeBluezAdapter::StartDiscovery() {
    ++started;
    // The device advertising, heard.
    bluez_.change(kDevicePath, QStringLiteral("org.bluez.Device1"), {{QStringLiteral("RSSI"), QVariant::fromValue(qint16{-60})}});
}

void FakeBluezDevice::Connect() {
    if (refuse) {
        sendErrorReply(QStringLiteral("org.bluez.Error.Failed"), QStringLiteral("le-connection-abort-by-local"));
        return;
    }
    bluez_.change(kDevicePath, QStringLiteral("org.bluez.Device1"), {{QStringLiteral("Connected"), true}});
    if (resolve) bluez_.change(kDevicePath, QStringLiteral("org.bluez.Device1"), {{QStringLiteral("ServicesResolved"), true}});
}

/// A backlight where the kernel would show it.
void makeBacklight(const QString &sysfs, const QString &name, const char *type, int max, int brightness) {
    QDir().mkpath(sysfs + QLatin1Char('/') + name);
    const auto write = [&](const char *attribute, const QByteArray &value) {
        QFile file(sysfs + QLatin1Char('/') + name + QLatin1Char('/') + QLatin1String(attribute));
        QVERIFY(file.open(QIODevice::WriteOnly));
        file.write(value + '\n');
    };
    write("type", type);
    write("max_brightness", QByteArray::number(max));
    write("brightness", QByteArray::number(brightness));
}

/// An App's own services: the advert, and the clipboard it chose to answer itself.
class AppServices final : public MigoQtHostServiceProvider {
public:
    std::uint64_t services() const noexcept override {
        return hostServiceBit(MIGO_HOST_SERVICE_AD) | hostServiceBit(MIGO_HOST_SERVICE_CLIPBOARD);
    }
    void call(MigoHostService service, std::uint32_t method, const QJsonObject &request,
              MigoQtHostServiceReply reply) override {
        calls.push_back({service, method, request});
        (void)reply.succeed({{QStringLiteral("data"), QStringLiteral("the app's")}});
    }
    struct Call {
        MigoHostService service;
        std::uint32_t method;
        QJsonObject request;
    };
    std::vector<Call> calls;
};

/// A shown view, attached, with the built-in services over it; retired on the way out.
struct AttachedServices {
    SurfaceHost host{fake_migo::session()};
    QWidget container;
    MigoQtX11SurfaceView view{host, container};
    std::shared_ptr<MigoQtHostServiceChannel> channel = MigoQtHostServiceChannel::open(fake_migo::session());
    MigoQtBuiltInHostServices services{view, channel};

    AttachedServices() {
        view.resize(320, 180);
        container.show();
        view.show();
    }

    ~AttachedServices() {
        channel->close();
        if (view.surfaceState() == SurfaceState::Attached) (void)view.beginDetach();
        if (view.surfaceState() == SurfaceState::Retiring) {
            fake_migo::set_release_status(view.generation(), MIGO_SURFACE_RELEASE_RELEASED);
            bool released = false;
            (void)view.pollDetach(&released);
        }
    }

    void command(MigoHostService service, std::uint32_t method, const QJsonObject &request) {
        services.call(service, method, request, channel->reply(0));
    }
};

/// Whether the game was pressed at `at`.
bool pressAt(QPointF at) {
    for (const fake_migo::PointerRecord &pointer : fake_migo::pointers()) {
        if (pointer.event_type == MIGO_POINTER_EVENT_DOWN && pointer.x == static_cast<float>(at.x()) &&
            pointer.y == static_cast<float>(at.y())) {
            return true;
        }
    }
    return false;
}

void press(QWidget &view) {
    const QPointF at(10.0, 10.0);
    QMouseEvent event(QEvent::MouseButtonPress, at, at, view.mapToGlobal(at), Qt::LeftButton, Qt::LeftButton,
                      Qt::NoModifier);
    QCoreApplication::sendEvent(&view, &event);
}

/// The colour of each quadrant of a picture `writeQuadrants` writes.
QColor quadrant(bool left, bool top) {
    if (top) return left ? QColor(255, 0, 0) : QColor(0, 255, 0);
    return left ? QColor(0, 0, 255) : QColor(255, 255, 255);
}

/// A picture `size` large in four quadrants -- red, green; blue, white -- as `format`.
void writeQuadrants(const QString &path, const char *format, QSize size = {256, 192}) {
    QImage image(size, QImage::Format_RGB32);
    for (int y = 0; y < size.height(); ++y) {
        for (int x = 0; x < size.width(); ++x) {
            image.setPixel(x, y, quadrant(x < size.width() / 2, y < size.height() / 2).rgb());
        }
    }
    QImageWriter writer(path, format);
    writer.setQuality(95);
    QVERIFY2(writer.write(image), qPrintable(writer.errorString()));
}

/// The quadrants as a camera held on its side stores them: 256x192 as taken,
/// with an EXIF orientation (6) saying they are seen turned a quarter clockwise.
/// Qt's JPEG writer turns the pixels instead of saying so, so the orientation
/// is written here, in an APP1 segment after the start of the image.
void writeTurnedQuadrants(const QString &path) {
    writeQuadrants(path, "jpeg");
    QFile file(path);
    QVERIFY(file.open(QIODevice::ReadWrite));
    QByteArray bytes = file.readAll();
    QVERIFY(bytes.startsWith("\xFF\xD8"));
    static const char exif[] =
        "\xFF\xE1\x00\x22"                         // APP1, 34 bytes
        "Exif\x00\x00"                               //
        "MM\x00\x2A\x00\x00\x00\x08"                 // big-endian TIFF, IFD at 8
        "\x00\x01"                                   // one entry:
        "\x01\x12\x00\x03\x00\x00\x00\x01\x00\x06\x00\x00"  //   Orientation, SHORT, 1, 6
        "\x00\x00\x00\x00";                         // no next IFD
    bytes.insert(2, exif, sizeof(exif) - 1);
    QVERIFY(file.seek(0));
    QCOMPARE(file.write(bytes), bytes.size());
    file.close();
    QImageReader stored(path);
    QCOMPARE(stored.size(), QSize(256, 192));
    QCOMPARE(stored.transformation(), QImageIOHandler::TransformationRotate90);
}

void writeFile(const QString &path, const QByteArray &bytes) {
    QFile file(path);
    QVERIFY(file.open(QIODevice::WriteOnly));
    QCOMPARE(file.write(bytes), bytes.size());
}

QByteArray bytesOf(const QString &path) {
    QFile file(path);
    return file.open(QIODevice::ReadOnly) ? file.readAll() : QByteArray();
}

/// Whether the 16x16 pixels around `at` average within 48 of `want`, each channel.
bool looksLike(const QImage &image, QPoint at, const QColor &want) {
    qint64 red = 0, green = 0, blue = 0, count = 0;
    for (int y = at.y() - 8; y < at.y() + 8; ++y) {
        for (int x = at.x() - 8; x < at.x() + 8; ++x) {
            const QColor pixel = image.pixelColor(x, y);
            red += pixel.red();
            green += pixel.green();
            blue += pixel.blue();
            ++count;
        }
    }
    return qAbs(red / count - want.red()) <= 48 && qAbs(green / count - want.green()) <= 48 &&
           qAbs(blue / count - want.blue()) <= 48;
}

/// What the image at `path` is stored as, by its content.
QByteArray formatOf(const QString &path) {
    QImageReader reader(path);
    reader.setDecideFormatFromContent(true);
    return reader.format();
}

/// Takes the files an answer names into `into`, as the library takes them into
/// the Session's /tmp as the call completes, and says where each went.
class TakenFiles final {
public:
    explicit TakenFiles(QString into) : into_(std::move(into)) {
        QDir().mkpath(into_);
        fake_migo::set_host_service_completion([this](const fake_migo::HostServiceAnswer &answer) { take(answer); });
    }
    ~TakenFiles() { fake_migo::set_host_service_completion(nullptr); }

    /// Where the file the host named `named` went, or empty when it was not taken.
    QString operator[](const QString &named) const {
        const std::lock_guard<std::mutex> lock(mutex_);
        return taken_.value(named);
    }

private:
    void take(const fake_migo::HostServiceAnswer &answer) {
        if (answer.status != MIGO_HOST_SERVICE_STATUS_OK) return;
        const QJsonObject result = parsed(answer.payload);
        QStringList named{result.value(QStringLiteral("tempFilePath")).toString()};
        for (const QJsonValue &path : result.value(QStringLiteral("tempFilePaths")).toArray()) named << path.toString();
        for (const QJsonValue &file : result.value(QStringLiteral("tempFiles")).toArray()) {
            named << file.toObject().value(QStringLiteral("path")).toString()
                  << file.toObject().value(QStringLiteral("tempFilePath")).toString();
        }
        const std::lock_guard<std::mutex> lock(mutex_);
        for (const QString &file : std::as_const(named)) {
            if (file.isEmpty() || taken_.contains(file)) continue;
            const QString to = QStringLiteral("%1/host-%2.%3").arg(into_).arg(++taken_count_).arg(QFileInfo(file).suffix());
            if (QFile::rename(file, to)) taken_.insert(file, to);
        }
    }

    const QString into_;
    mutable std::mutex mutex_;
    QHash<QString, QString> taken_;
    int taken_count_ = 0;
};

/// The file dialog the player sees, or null.
QFileDialog *fileDialog() { return shownWindow<QFileDialog>(); }

/// The player picks `names` in `directory` -- or names the file to save -- and confirms.
void choose(QFileDialog &dialog, const QString &directory, const QStringList &names) {
    dialog.setDirectory(directory);
    auto *field = dialog.findChild<QLineEdit *>(QStringLiteral("fileNameEdit"));
    QVERIFY(field != nullptr);
    field->setText(names.size() == 1 ? names.first()
                                     : QLatin1Char('"') + names.join(QStringLiteral("\" \"")) + QLatin1Char('"'));
    // QFileDialog's own accept, which checks what was named, is reached through QDialog's.
    static_cast<QDialog &>(dialog).accept();
}

/// Asks the image service `method` as content does, and waits for its answer:
/// a failure "no answer" when none comes.
fake_migo::HostServiceAnswer askImages(std::uint32_t method, std::uint64_t call_id, const QJsonObject &request) {
    fake_migo::deliver_host_service_call_from_engine_thread(MIGO_HOST_SERVICE_IMAGE, method, call_id, json(request));
    (void)QTest::qWaitFor([call_id] { return answerTo(call_id).has_value(); }, 10000);
    if (std::optional<fake_migo::HostServiceAnswer> answer = answerTo(call_id)) return *answer;
    fake_migo::HostServiceAnswer none;
    none.status = MIGO_HOST_SERVICE_STATUS_FAIL;
    none.message = "no answer";
    return none;
}

/// Whether the directory `file` is staged in holds no file.
bool nothingLeftBeside(const QString &file) {
    return QDir(QFileInfo(file).path()).entryList(QDir::Files | QDir::Hidden).isEmpty();
}

/// An IIO device where the kernel would show it: `channel`'s raw values, and
/// what else it shows (a scale, an offset, a mount matrix), in a directory of
/// its own under `root`.
QString makeIio(const QString &root, const QString &name, const QString &channel, std::array<int, 3> raw,
                const QHash<QString, QByteArray> &attributes = {}) {
    const QString device = root + QLatin1Char('/') + name;
    QDir().mkpath(device);
    const char *axes[] = {"x", "y", "z"};
    for (int axis = 0; axis < 3; ++axis) {
        writeFile(QStringLiteral("%1/in_%2_%3_raw").arg(device, channel, QLatin1String(axes[axis])),
                  QByteArray::number(raw[axis]) + '\n');
    }
    for (auto attribute = attributes.cbegin(); attribute != attributes.cend(); ++attribute) {
        writeFile(device + QLatin1Char('/') + attribute.key(), attribute.value() + '\n');
    }
    return device;
}

/// Readings a sensor reported, as it reports them: on its own thread.
struct Readings {
    std::mutex mutex;
    std::vector<MigoSensorSample> samples;
    std::function<void(const MigoSensorSample &)> sink() {
        return [this](const MigoSensorSample &sample) {
            const std::lock_guard<std::mutex> lock(mutex);
            samples.push_back(sample);
        };
    }
    std::size_t size() {
        const std::lock_guard<std::mutex> lock(mutex);
        return samples.size();
    }
    MigoSensorSample last() {
        const std::lock_guard<std::mutex> lock(mutex);
        return samples.back();
    }
};

bool near(double actual, double expected) { return std::abs(actual - expected) < 1e-6; }

/// The preview shown, or null.
ImageViewer *imageViewer() {
    for (QWidget *widget : QApplication::topLevelWidgets()) {
        if (auto *viewer = dynamic_cast<ImageViewer *>(widget); viewer != nullptr && viewer->isVisible()) return viewer;
    }
    return nullptr;
}

/// Every file staged in the App's cache, where the image service keeps its own.
QStringList stagedFiles() {
    QStringList files;
    const QDir cache(QStandardPaths::writableLocation(QStandardPaths::CacheLocation));
    for (const QString &staging : cache.entryList({QStringLiteral("migo-host-files-*")}, QDir::Dirs)) {
        files << QDir(cache.filePath(staging)).entryList(QDir::Files | QDir::Hidden);
    }
    return files;
}

/// A web server on the loopback: a GET of one of `files`' paths answers its
/// bytes, of any other 404.
class LocalWeb final : public QObject {
public:
    explicit LocalWeb(QHash<QByteArray, QByteArray> files) : files_(std::move(files)) {
        connect(&server_, &QTcpServer::newConnection, this, [this] {
            while (QTcpSocket *socket = server_.nextPendingConnection()) {
                connect(socket, &QTcpSocket::readyRead, socket, [this, socket] { answer(*socket); });
                connect(socket, &QTcpSocket::disconnected, socket, &QObject::deleteLater);
            }
        });
        listening_ = server_.listen(QHostAddress::LocalHost, 0);
    }

    [[nodiscard]] bool listening() const { return listening_; }
    [[nodiscard]] QString url(const char *path) const {
        return QStringLiteral("http://127.0.0.1:%1%2").arg(server_.serverPort()).arg(QLatin1String(path));
    }

private:
    void answer(QTcpSocket &socket) {
        QByteArray &request = requests_[&socket];
        request += socket.readAll();
        if (!request.contains("\r\n\r\n")) return;
        const auto found = files_.constFind(request.split(' ').value(1));
        const QByteArray body = found == files_.cend() ? QByteArray() : *found;
        socket.write((found == files_.cend() ? "HTTP/1.1 404 Not Found\r\n" : "HTTP/1.1 200 OK\r\n") +
                     QByteArray("Content-Length: ") + QByteArray::number(body.size()) +
                     "\r\nConnection: close\r\n\r\n" + body);
        socket.disconnectFromHost();
        requests_.remove(&socket);
    }

    QTcpServer server_;
    bool listening_ = false;
    QHash<QByteArray, QByteArray> files_;
    QHash<QTcpSocket *, QByteArray> requests_;
};

}  // namespace

class QtHostServicesTest final : public QObject {
    Q_OBJECT

private slots:
    void initTestCase() {
        // The player's dialogs here are Qt's own, which a test can drive; an
        // App's are the desktop's.
        QCoreApplication::setAttribute(Qt::AA_DontUseNativeDialogs);
        // The App's cache, where images are staged, and the player's Pictures
        // folder, the photo album: the desktop's, as the XDG directories say.
        QVERIFY(home_.isValid());
        qputenv("XDG_CACHE_HOME", QFile::encodeName(home_.filePath(QStringLiteral("cache"))));
        qputenv("XDG_CONFIG_HOME", QFile::encodeName(home_.filePath(QStringLiteral("config"))));
        QVERIFY(QDir().mkpath(home_.filePath(QStringLiteral("config"))));
        writeFile(home_.filePath(QStringLiteral("config/user-dirs.dirs")),
                  "XDG_PICTURES_DIR=\"" + QFile::encodeName(pictures()) + "\"\n");
        QCOMPARE(QStandardPaths::writableLocation(QStandardPaths::PicturesLocation), pictures());
    }

    void init() { fake_migo::reset(); }

    /// A Managed Session declares what Qt answers itself, and creates the
    /// Session with no network and the default launch unless told otherwise.
    void a_managed_session_declares_the_built_in_services() {
        Managed fixture;
        MigoManagedSession &managed = fixture.session;
        QVERIFY(managed.isValid());

        const MigoHostCallbacks &installed = fake_migo::installed_callbacks();
        QVERIFY(installed.on_host_service_call != nullptr);
        QCOMPARE(installed.host_services, hostServiceBit(MIGO_HOST_SERVICE_INTERACTION) |
                                              hostServiceBit(MIGO_HOST_SERVICE_CLIPBOARD) |
                                              hostServiceBit(MIGO_HOST_SERVICE_WINDOW) |
                                              hostServiceBit(MIGO_HOST_SERVICE_SCREEN) |
                                              hostServiceBit(MIGO_HOST_SERVICE_IMAGE) |
                                              hostServiceBit(MIGO_HOST_SERVICE_LOCATION) |
                                              hostServiceBit(MIGO_HOST_SERVICE_PERMISSION) |
                                              hostServiceBit(MIGO_HOST_SERVICE_MOTION) |
                                              hostServiceBit(MIGO_HOST_SERVICE_BLUETOOTH));
        QCOMPARE(fake_migo::created_network_policy(), std::string());
        QCOMPARE(fake_migo::created_launch_options(), std::string());

        QCOMPARE(managed.close(), MIGO_OK);
    }

    /// The App's network policy and launch options reach the Session it is created with.
    void the_options_reach_the_session() {
        MigoManagedSession::Options options;
        options.networkPolicyJson = R"({"request":["example.com"]})";
        options.launchOptionsJson = R"({"scene":1007})";
        Managed fixture(options);
        MigoManagedSession &managed = fixture.session;
        QVERIFY(managed.isValid());

        QCOMPARE(fake_migo::created_network_policy(), std::string(R"({"request":["example.com"]})"));
        QCOMPARE(fake_migo::created_launch_options(), std::string(R"({"scene":1007})"));
        QCOMPARE(managed.close(), MIGO_OK);
    }

    /// The App says who it is: the identifier it gives, or else its desktop
    /// file name; with neither, the Session is told nothing.
    void the_app_says_who_it_is() {
        {
            Managed fixture;
            QCOMPARE(fake_migo::reported_host_app_id(), std::string());
        }
        fake_migo::reset();
        QGuiApplication::setDesktopFileName(QStringLiteral("com.example.desktop"));
        {
            Managed fixture;
            QCOMPARE(fake_migo::reported_host_app_id(), std::string("com.example.desktop"));
        }
        fake_migo::reset();
        {
            MigoManagedSession::Options options;
            options.appId = QStringLiteral("com.example.given");
            Managed fixture(options);
            QCOMPARE(fake_migo::reported_host_app_id(), std::string("com.example.given"));
        }
        QGuiApplication::setDesktopFileName(QString());
    }

    /// What the App names is the App's -- a service it adds, and one the Host
    /// Kit would otherwise answer -- and the rest is the Host Kit's.
    void the_app_answers_what_it_names() {
        AppServices app;
        MigoManagedSession::Options options;
        options.appServices = &app;
        Managed fixture(options);
        MigoManagedSession &managed = fixture.session;
        QVERIFY(managed.isValid());
        QCOMPARE(fake_migo::installed_callbacks().host_services,
                 hostServiceBit(MIGO_HOST_SERVICE_AD) | hostServiceBit(MIGO_HOST_SERVICE_CLIPBOARD) |
                     hostServiceBit(MIGO_HOST_SERVICE_INTERACTION) | hostServiceBit(MIGO_HOST_SERVICE_WINDOW) |
                     hostServiceBit(MIGO_HOST_SERVICE_SCREEN) | hostServiceBit(MIGO_HOST_SERVICE_IMAGE) |
                     hostServiceBit(MIGO_HOST_SERVICE_LOCATION) | hostServiceBit(MIGO_HOST_SERVICE_PERMISSION) |
                     hostServiceBit(MIGO_HOST_SERVICE_MOTION) | hostServiceBit(MIGO_HOST_SERVICE_BLUETOOTH));

        fake_migo::deliver_host_service_call_from_engine_thread(MIGO_HOST_SERVICE_CLIPBOARD,
                                                                MIGO_CLIPBOARD_GET_CLIPBOARD_DATA, 3, "{}");
        QTRY_VERIFY(answerTo(3).has_value());
        QCOMPARE(parsed(answerTo(3)->payload).value(QStringLiteral("data")).toString(), QStringLiteral("the app's"));
        QCOMPARE(app.calls.size(), std::size_t{1});

        fake_migo::deliver_host_service_call_from_engine_thread(
            MIGO_HOST_SERVICE_INTERACTION, MIGO_INTERACTION_SHOW_ACTION_SHEET, 4, json({{"itemList", QJsonArray()}}));
        QTRY_VERIFY(answerTo(4).has_value());
        QCOMPARE(answerTo(4)->message, std::string("itemList is empty"));
        QCOMPARE(app.calls.size(), std::size_t{1});

        QCOMPARE(managed.close(), MIGO_OK);
    }

    /// The clipboard is Qt's: what content writes, it reads back.
    void the_clipboard_round_trips() {
        Managed fixture;
        MigoManagedSession &managed = fixture.session;
        fake_migo::deliver_host_service_call_from_engine_thread(
            MIGO_HOST_SERVICE_CLIPBOARD, MIGO_CLIPBOARD_SET_CLIPBOARD_DATA, 1, json({{"data", "migo \u00e9"}}));
        QTRY_VERIFY(answerTo(1).has_value());
        QCOMPARE(answerTo(1)->status, MIGO_HOST_SERVICE_STATUS_OK);
        QCOMPARE(QGuiApplication::clipboard()->text(), QStringLiteral("migo \u00e9"));

        fake_migo::deliver_host_service_call_from_engine_thread(MIGO_HOST_SERVICE_CLIPBOARD,
                                                                MIGO_CLIPBOARD_GET_CLIPBOARD_DATA, 2, "{}");
        QTRY_VERIFY(answerTo(2).has_value());
        QCOMPARE(parsed(answerTo(2)->payload).value(QStringLiteral("data")).toString(), QStringLiteral("migo \u00e9"));
        QCOMPARE(managed.close(), MIGO_OK);
    }

    /// A modal answers which button the player chose, and an editable one what they typed.
    void a_modal_answers_the_players_choice() {
        Managed fixture;
        MigoManagedSession &managed = fixture.session;

        fake_migo::deliver_host_service_call_from_engine_thread(
            MIGO_HOST_SERVICE_INTERACTION, MIGO_INTERACTION_SHOW_MODAL, 10,
            json({{"title", "Quit?"}, {"content", "Progress is saved."}, {"confirmText", "Quit"}}));
        QTRY_VERIFY(shownWindow<QDialog>() != nullptr);
        QDialog *dialog = shownWindow<QDialog>();
        QVERIFY(buttonNamed(*dialog, QStringLiteral("\u53d6\u6d88")) != nullptr);
        buttonNamed(*dialog, QStringLiteral("Quit"))->click();
        QTRY_VERIFY(answerTo(10).has_value());
        QCOMPARE(parsed(answerTo(10)->payload),
                 QJsonObject({{QStringLiteral("confirm"), true}, {QStringLiteral("cancel"), false}}));

        fake_migo::deliver_host_service_call_from_engine_thread(
            MIGO_HOST_SERVICE_INTERACTION, MIGO_INTERACTION_SHOW_MODAL, 11,
            json({{"title", "Name"}, {"content", "Ada"}, {"editable", true}, {"placeholderText", "your name"}}));
        QTRY_VERIFY(shownWindow<QDialog>() != nullptr);
        dialog = shownWindow<QDialog>();
        QLineEdit *field = dialog->findChild<QLineEdit *>();
        QVERIFY(field != nullptr);
        QCOMPARE(field->text(), QStringLiteral("Ada"));
        QCOMPARE(field->placeholderText(), QStringLiteral("your name"));
        field->setText(QStringLiteral("Grace"));
        buttonNamed(*dialog, QStringLiteral("\u786e\u5b9a"))->click();
        QTRY_VERIFY(answerTo(11).has_value());
        QCOMPARE(parsed(answerTo(11)->payload).value(QStringLiteral("content")).toString(), QStringLiteral("Grace"));

        // Dismissed any other way, it is declined.
        fake_migo::deliver_host_service_call_from_engine_thread(
            MIGO_HOST_SERVICE_INTERACTION, MIGO_INTERACTION_SHOW_MODAL, 12, json({{"content", "Sure?"}}));
        QTRY_VERIFY(shownWindow<QDialog>() != nullptr);
        shownWindow<QDialog>()->reject();
        QTRY_VERIFY(answerTo(12).has_value());
        QCOMPARE(parsed(answerTo(12)->payload),
                 QJsonObject({{QStringLiteral("confirm"), false}, {QStringLiteral("cancel"), true}}));
        QCOMPARE(managed.close(), MIGO_OK);
    }

    /// An action sheet answers the item tapped, or fails as cancelled when dismissed.
    void an_action_sheet_answers_the_tap_or_the_dismissal() {
        Managed fixture;
        MigoManagedSession &managed = fixture.session;

        fake_migo::deliver_host_service_call_from_engine_thread(
            MIGO_HOST_SERVICE_INTERACTION, MIGO_INTERACTION_SHOW_ACTION_SHEET, 20,
            json({{"alertText", "Share"}, {"itemList", QJsonArray({"Friends", "Group"})}}));
        QTRY_VERIFY(shownWindow<QMenu>() != nullptr);
        QMenu *menu = shownWindow<QMenu>();
        QAction *group = nullptr;
        for (QAction *action : menu->actions()) {
            if (action->text() == QStringLiteral("Group")) group = action;
        }
        QVERIFY(group != nullptr);
        // As a click does it: the menu hides, then reports the item.
        menu->hide();
        group->trigger();
        QTRY_VERIFY(answerTo(20).has_value());
        QCOMPARE(parsed(answerTo(20)->payload).value(QStringLiteral("tapIndex")).toInt(-1), 1);

        fake_migo::deliver_host_service_call_from_engine_thread(
            MIGO_HOST_SERVICE_INTERACTION, MIGO_INTERACTION_SHOW_ACTION_SHEET, 21,
            json({{"itemList", QJsonArray({"Friends"})}}));
        QTRY_VERIFY(shownWindow<QMenu>() != nullptr);
        shownWindow<QMenu>()->close();
        QTRY_VERIFY(answerTo(21).has_value());
        QCOMPARE(answerTo(21)->status, MIGO_HOST_SERVICE_STATUS_FAIL);
        QCOMPARE(answerTo(21)->message, std::string("cancel"));
        QCOMPARE(managed.close(), MIGO_OK);
    }

    /// A toast hides itself; a loading indicator replaces it and stays; each
    /// hide hides only its own kind; a command is answered by nothing.
    void toasts_and_loading_are_one_at_a_time() {
        Managed fixture;
        MigoManagedSession &managed = fixture.session;

        fake_migo::deliver_host_service_call_from_engine_thread(
            MIGO_HOST_SERVICE_INTERACTION, MIGO_INTERACTION_SHOW_TOAST, 0,
            json({{"title", "Saved"}, {"duration", 400}}));
        QTRY_COMPARE(indicatorText(), QStringLiteral("Saved"));
        QTRY_VERIFY(indicator() == nullptr);

        fake_migo::deliver_host_service_call_from_engine_thread(
            MIGO_HOST_SERVICE_INTERACTION, MIGO_INTERACTION_SHOW_TOAST, 0,
            json({{"title", "Saved"}, {"duration", 60000}}));
        fake_migo::deliver_host_service_call_from_engine_thread(MIGO_HOST_SERVICE_INTERACTION,
                                                                MIGO_INTERACTION_HIDE_LOADING, 0, "{}");
        QTRY_COMPARE(indicatorText(), QStringLiteral("Saved"));
        fake_migo::deliver_host_service_call_from_engine_thread(
            MIGO_HOST_SERVICE_INTERACTION, MIGO_INTERACTION_SHOW_LOADING, 0, json({{"title", "Loading"}}));
        QTRY_COMPARE(indicatorText(), QStringLiteral("Loading"));
        fake_migo::deliver_host_service_call_from_engine_thread(MIGO_HOST_SERVICE_INTERACTION,
                                                                MIGO_INTERACTION_HIDE_TOAST, 0, "{}");
        QTRY_COMPARE(indicatorText(), QStringLiteral("Loading"));
        fake_migo::deliver_host_service_call_from_engine_thread(MIGO_HOST_SERVICE_INTERACTION,
                                                                MIGO_INTERACTION_HIDE_LOADING, 0, "{}");
        QTRY_VERIFY(indicator() == nullptr);

        QVERIFY(fake_migo::host_service_answers().empty());
        QCOMPARE(managed.close(), MIGO_OK);
    }

    /// While an indicator with a mask is up, the game hears none of the
    /// player's input; once it is gone, it does again.
    void a_mask_keeps_input_from_the_game() {
        if (!require_xcb("xcb-only test", __FILE__, __LINE__)) return;
        AttachedServices fixture;
        QTRY_COMPARE(fake_migo::calls().attach, 1);

        fixture.command(MIGO_HOST_SERVICE_INTERACTION, MIGO_INTERACTION_SHOW_LOADING,
                        {{QStringLiteral("title"), QStringLiteral("Wait")}, {QStringLiteral("mask"), true}});
        press(fixture.view);
        QCOMPARE(fake_migo::pointers().size(), std::size_t{0});

        fixture.command(MIGO_HOST_SERVICE_INTERACTION, MIGO_INTERACTION_HIDE_LOADING, {});
        press(fixture.view);
        QCOMPARE(fake_migo::pointers().size(), std::size_t{1});
    }

    /// An indicator stands over the game and takes none of the player's
    /// input: a click on it is the game's.
    void an_indicator_takes_none_of_the_players_input() {
        if (!require_xcb("xcb-only test", __FILE__, __LINE__)) return;
        AttachedServices fixture;
        QTRY_COMPARE(fake_migo::calls().attach, 1);
        fixture.command(MIGO_HOST_SERVICE_INTERACTION, MIGO_INTERACTION_SHOW_TOAST,
                        {{QStringLiteral("title"), QStringLiteral("Saved")}, {QStringLiteral("duration"), 10000}});
        QCOMPARE(fixture.services.indicatorTitle(), QStringLiteral("Saved"));
        x11_player::Hands hands;
        const QPoint centre = fixture.view.rect().center();
        QCursor::setPos(fixture.view.mapToGlobal(centre));
        hands.click(1);
        QTRY_VERIFY(pressAt(QPointF(centre)));
    }

    /// The cursor over the game is the one content asks for: a CSS keyword, the
    /// system's for "default", or an image with its hotspot clamped into it.
    void the_cursor_is_the_one_content_asks_for() {
        Managed fixture;
        MigoQtX11SurfaceView &view = *fixture.session.view();
        const auto cursor = [](const QJsonObject &request) {
            fake_migo::deliver_host_service_call_from_engine_thread(MIGO_HOST_SERVICE_WINDOW, MIGO_WINDOW_SET_CURSOR,
                                                                    0, json(request));
        };
        cursor({{"keyword", "pointer"}});
        QTRY_COMPARE(view.cursor().shape(), Qt::PointingHandCursor);
        cursor({{"keyword", "none"}});
        QTRY_COMPARE(view.cursor().shape(), Qt::BlankCursor);
        cursor({{"keyword", "default"}});
        QTRY_VERIFY(!view.testAttribute(Qt::WA_SetCursor));

        QTemporaryDir directory;
        QPixmap image(8, 8);
        image.fill(Qt::red);
        const QString path = directory.filePath(QStringLiteral("cursor.png"));
        QVERIFY(image.save(path));
        cursor({{"path", path}, {"x", 3}, {"y", 99}});
        QTRY_COMPARE(view.cursor().shape(), Qt::BitmapCursor);
        QCOMPARE(view.cursor().hotSpot(), QPoint(3, 7));
        // An image Qt cannot read leaves the cursor as it was.
        cursor({{"path", directory.filePath(QStringLiteral("none.png"))}});
        QCoreApplication::processEvents();
        QCOMPARE(view.cursor().shape(), Qt::BitmapCursor);
        QCOMPARE(fixture.session.close(), MIGO_OK);
    }

    /// The size asked for is the game's: the window grows or shrinks by what the
    /// view must, and a full-screen window is not sized.
    void the_window_is_sized_for_the_game() {
        Managed fixture;
        auto *layout = new QVBoxLayout(&fixture.container);
        layout->setContentsMargins(0, 0, 0, 0);
        layout->addWidget(fixture.session.view());
        fixture.container.resize(320, 200);
        fixture.container.show();
        QTRY_COMPARE(fixture.session.view()->size(), QSize(320, 200));

        const qreal ratio = fixture.session.view()->devicePixelRatioF();
        fake_migo::deliver_host_service_call_from_engine_thread(
            MIGO_HOST_SERVICE_WINDOW, MIGO_WINDOW_SET_WINDOW_SIZE, 50,
            json({{"width", qRound(400 * ratio)}, {"height", qRound(300 * ratio)}}));
        QTRY_VERIFY(answerTo(50).has_value());
        QCOMPARE(answerTo(50)->status, MIGO_HOST_SERVICE_STATUS_OK);
        QTRY_COMPARE(fixture.session.view()->size(), QSize(400, 300));

        fake_migo::deliver_host_service_call_from_engine_thread(MIGO_HOST_SERVICE_WINDOW,
                                                                MIGO_WINDOW_SET_WINDOW_SIZE, 51,
                                                                json({{"width", 0}, {"height", 300}}));
        QTRY_VERIFY(answerTo(51).has_value());
        QCOMPARE(answerTo(51)->message, std::string("width and height must be positive numbers"));

        fixture.container.showFullScreen();
        fake_migo::deliver_host_service_call_from_engine_thread(MIGO_HOST_SERVICE_WINDOW,
                                                                MIGO_WINDOW_SET_WINDOW_SIZE, 52,
                                                                json({{"width", 200}, {"height", 100}}));
        QTRY_VERIFY(answerTo(52).has_value());
        QCOMPARE(answerTo(52)->message, std::string("the window is full screen"));
        fixture.container.hide();
        QCOMPARE(fixture.session.close(), MIGO_OK);
    }

    /// Each change of the window's state is reported once, in the platform's
    /// words; a pointer lock asked for with the player elsewhere is refused,
    /// and said to be.
    void the_window_reports_its_state_and_a_refused_pointer_lock() {
        Managed fixture;
        const auto events = [] {
            std::vector<std::string> said;
            for (const auto &event : fake_migo::host_service_events()) {
                if (event.service == MIGO_HOST_SERVICE_WINDOW) said.push_back(event.payload);
            }
            return said;
        };
        // One change at a time, each settled: the platform confirms a state
        // change after the event loop runs, and a request made before that is
        // confirmed in its turn too.
        const auto settle = [](QWidget &window, Qt::WindowStates state) {
            window.setWindowState(state);
            QTest::qWait(100);
        };
        settle(fixture.container, Qt::WindowMinimized);
        settle(fixture.container, Qt::WindowMinimized);
        settle(fixture.container, Qt::WindowFullScreen);
        settle(fixture.container, Qt::WindowNoState);
        QCOMPARE(events(), (std::vector<std::string>{R"({"state":"minimize"})", R"({"state":"maximize"})",
                                                     R"({"state":"normalize"})"}));

        // The view does not hold the keyboard: Esc, the way out, would not reach it.
        fixture.session.view()->clearFocus();
        fake_migo::deliver_host_service_call_from_engine_thread(MIGO_HOST_SERVICE_WINDOW,
                                                                MIGO_WINDOW_REQUEST_POINTER_LOCK, 0, "{}");
        QTRY_COMPARE(events().size(), std::size_t{4});
        QCOMPARE(events().back(), std::string(R"({"locked":false})"));
        QCOMPARE(fixture.session.close(), MIGO_OK);
    }

    /// A lock content asks for with the player at the game is taken and said
    /// to be, as its release is.
    void a_pointer_lock_is_reported_as_it_is_taken_and_released() {
        if (!require_xcb("xcb-only test", __FILE__, __LINE__)) return;
        Managed fixture;
        MigoQtX11SurfaceView &view = *fixture.session.view();
        auto *layout = new QVBoxLayout(&fixture.container);
        layout->setContentsMargins(0, 0, 0, 0);
        layout->addWidget(&view);
        fixture.container.resize(320, 200);
        fixture.container.show();
        QTRY_COMPARE(fake_migo::calls().attach, 1);
        fixture.container.activateWindow();
        QVERIFY(QTest::qWaitForWindowActive(&fixture.container));
        view.setFocus();
        QCursor::setPos(view.mapToGlobal(QPoint(10, 10)));
        const auto locks = [] {
            std::vector<std::string> said;
            for (const auto &event : fake_migo::host_service_events()) {
                if (event.service == MIGO_HOST_SERVICE_WINDOW &&
                    event.event == MIGO_WINDOW_EVENT_POINTER_LOCK_CHANGE) {
                    said.push_back(event.payload);
                }
            }
            return said;
        };
        const auto command = [](std::uint32_t method) {
            fake_migo::deliver_host_service_call_from_engine_thread(MIGO_HOST_SERVICE_WINDOW, method, 0, "{}");
        };

        command(MIGO_WINDOW_REQUEST_POINTER_LOCK);
        QTRY_VERIFY(view.isPointerLocked());
        command(MIGO_WINDOW_REQUEST_POINTER_LOCK);
        command(MIGO_WINDOW_EXIT_POINTER_LOCK);
        QTRY_VERIFY(!view.isPointerLocked());
        QCOMPARE(locks(), (std::vector<std::string>{R"({"locked":true})", R"({"locked":false})"}));
        QCOMPARE(fixture.session.close(), MIGO_OK);
    }

    /// The screen's orientation is the game's shape: each crossing of the view
    /// from one shape to the other is reported once, and setDeviceOrientation
    /// turns the window -- refused for a value that is neither shape, while the
    /// window is full screen, and when the screen cannot hold it turned.
    void the_game_turns_with_its_window() {
        Managed fixture;
        MigoQtX11SurfaceView &view = *fixture.session.view();
        auto *layout = new QVBoxLayout(&fixture.container);
        layout->setContentsMargins(0, 0, 0, 0);
        layout->addWidget(&view);
        fixture.container.resize(320, 200);
        fixture.container.show();
        QTRY_COMPARE(view.size(), QSize(320, 200));
        const auto turns = [] {
            std::vector<std::string> said;
            for (const auto &event : fake_migo::host_service_events()) {
                if (event.service == MIGO_HOST_SERVICE_SCREEN &&
                    event.event == MIGO_SCREEN_EVENT_DEVICE_ORIENTATION_CHANGE) {
                    said.push_back(event.payload);
                }
            }
            return said;
        };
        const auto turn = [](std::uint64_t call_id, const char *value) {
            fake_migo::deliver_host_service_call_from_engine_thread(
                MIGO_HOST_SERVICE_SCREEN, MIGO_SCREEN_SET_DEVICE_ORIENTATION, call_id, json({{"value", value}}));
            QTRY_VERIFY(answerTo(call_id).has_value());
        };
        const std::size_t before = turns().size();

        turn(70, "portrait");
        QCOMPARE(answerTo(70)->status, MIGO_HOST_SERVICE_STATUS_OK);
        QTRY_COMPARE(view.size(), QSize(200, 320));
        QTRY_COMPARE(turns().size(), before + 1);
        QCOMPARE(turns().back(), std::string(R"({"value":"portrait"})"));
        turn(71, "portrait");
        QCOMPARE(answerTo(71)->status, MIGO_HOST_SERVICE_STATUS_OK);
        QCOMPARE(view.size(), QSize(200, 320));
        turn(72, "upside-down");
        QCOMPARE(answerTo(72)->message, std::string("value must be portrait or landscape"));

        // Wider than the screen is tall: turned, it would not fit.
        const QSize room = fixture.container.screen()->availableGeometry().size();
        fixture.container.resize(room.height() + 50, 100);
        QTRY_COMPARE(view.size(), QSize(room.height() + 50, 100));
        QTRY_COMPARE(turns().size(), before + 2);
        QCOMPARE(turns().back(), std::string(R"({"value":"landscape"})"));
        turn(73, "portrait");
        QCOMPARE(answerTo(73)->message, std::string("the screen cannot hold the window turned"));

        fixture.container.showFullScreen();
        turn(74, "portrait");
        QCOMPARE(answerTo(74)->message, std::string("the window is full screen"));
        fixture.container.hide();
        QCOMPARE(fixture.session.close(), MIGO_OK);
    }

    /// setEnableDebug turns the debugging aids: the Session is told -- with who
    /// the App is, as before -- and a panel over the top of the game shows what
    /// content writes to its console, taking none of the player's input.
    void the_debugging_aids_show_the_console_and_tell_the_session() {
        MigoManagedSession::Options options;
        options.appId = QStringLiteral("com.example.debugging");
        Managed fixture(options);
        MigoQtX11SurfaceView &view = *fixture.session.view();
        auto *layout = new QVBoxLayout(&fixture.container);
        layout->setContentsMargins(0, 0, 0, 0);
        layout->addWidget(&view);
        fixture.container.resize(320, 200);
        fixture.container.show();
        QTRY_COMPARE(view.size(), QSize(320, 200));
        QSignalSpy changes(&fixture.session, &MigoManagedSession::debugAidsChanged);
        const auto panel = [] {
            for (QWidget *window : QApplication::topLevelWidgets()) {
                if (window->objectName() == QLatin1String("migoDebugPanel")) return window;
            }
            return static_cast<QWidget *>(nullptr);
        };
        const auto debugging = [](bool on) {
            fake_migo::deliver_host_service_call_from_engine_thread(
                MIGO_HOST_SERVICE_SCREEN, MIGO_SCREEN_SET_ENABLE_DEBUG, 0, json({{"enableDebug", on}}));
        };
        QCOMPARE(panel(), nullptr);
        debugging(true);
        QTRY_VERIFY(fixture.session.debugAids());
        QCOMPARE(changes.size(), 1);
        QCOMPARE(fake_migo::reported_host_app_flags(), std::uint32_t{MIGO_HOST_APP_INFO_FLAG_DEBUG});
        QCOMPARE(fake_migo::reported_host_app_id(), std::string("com.example.debugging"));
        QVERIFY(panel() != nullptr);
        QVERIFY(panel()->isVisible());
        QCOMPARE(panel()->geometry().topLeft(), view.mapToGlobal(QPoint(0, 0)));
        QCOMPARE(panel()->width(), view.width());
        fake_migo::write_console(1, "hello");
        fake_migo::write_console(3, "it broke");
        auto *lines = panel()->findChild<QPlainTextEdit *>();
        QVERIFY(lines != nullptr);
        QTRY_VERIFY(lines->toPlainText().contains(QStringLiteral("E it broke")));
        QVERIFY(lines->toPlainText().contains(QStringLiteral("I hello")));

        if (QGuiApplication::platformName() == QStringLiteral("xcb")) {
            // A click on the panel is the game's.
            QTRY_COMPARE(fake_migo::calls().attach, 1);
            x11_player::Hands hands;
            QCursor::setPos(view.mapToGlobal(QPoint(10, 10)));
            hands.click(1);
            QTRY_VERIFY(pressAt(QPointF(10.0, 10.0)));
        }

        debugging(false);
        QTRY_VERIFY(!fixture.session.debugAids());
        QCOMPARE(changes.size(), 2);
        QCOMPARE(fake_migo::reported_host_app_flags(), std::uint32_t{0});
        QCOMPARE(fake_migo::reported_host_app_id(), std::string("com.example.debugging"));
        QCOMPARE(panel(), nullptr);
        QCOMPARE(fixture.session.close(), MIGO_OK);
    }

    /// The brightness is the backlight the desktop's own tools pick -- the
    /// firmware's over a raw one -- read where the kernel shows it and set
    /// through logind, never to unlit; a screen with none, or logind refusing,
    /// is said to be.
    void the_backlight_is_read_from_the_kernel_and_set_through_logind() {
        QDBusConnection bus = QDBusConnection::sessionBus();
        QVERIFY2(bus.isConnected(), "no session bus: run this test under dbus-run-session, as scripts/test-linux-qt-host-kit.sh does");
        QTemporaryDir sysfs;
        FakeLogind logind(sysfs.path());
        QVERIFY(bus.registerService(QStringLiteral("org.freedesktop.login1")));
        QVERIFY(bus.registerObject(QStringLiteral("/org/freedesktop/login1/session/auto"), &logind,
                                   QDBusConnection::ExportAllSlots));
        migo::linux_host::qt6::Backlight backlight(sysfs.path(), bus);
        std::optional<QString> said;
        const auto set = [&](double value) {
            said.reset();
            backlight.setBrightness(value, [&said](const QString &error) { said = error; });
            return QTest::qWaitFor([&said] { return said.has_value(); });
        };

        QVERIFY(!backlight.brightness().has_value());
        QVERIFY(set(0.5));
        QCOMPARE(*said, QStringLiteral("the screen has no backlight the system controls"));

        makeBacklight(sysfs.path(), QStringLiteral("intel_backlight"), "raw", 1000, 500);
        makeBacklight(sysfs.path(), QStringLiteral("acpi_video0"), "firmware", 100, 30);
        QCOMPARE(backlight.brightness(), std::optional<double>(0.3));
        QVERIFY(set(0.75));
        QCOMPARE(*said, QString());
        QCOMPARE(logind.asked.back(), QStringLiteral("backlight acpi_video0 75"));
        QCOMPARE(backlight.brightness(), std::optional<double>(0.75));
        QVERIFY(set(0.0));
        QCOMPARE(logind.asked.back(), QStringLiteral("backlight acpi_video0 1"));
        QVERIFY(set(4.0));
        QCOMPARE(logind.asked.back(), QStringLiteral("backlight acpi_video0 100"));

        logind.refuse = true;
        QVERIFY(set(0.5));
        QCOMPARE(*said, QStringLiteral("not the active session"));
        bus.unregisterObject(QStringLiteral("/org/freedesktop/login1/session/auto"));
        QVERIFY(bus.unregisterService(QStringLiteral("org.freedesktop.login1")));
    }

    /// A call is answered once; a failure needs a reason, and one without is
    /// refused without spending the answer.
    void a_call_is_answered_once_and_a_failure_names_why() {
        AppServices app;
        MigoManagedSession::Options options;
        options.appServices = &app;
        Managed fixture(options);
        MigoManagedSession &managed = fixture.session;
        std::shared_ptr<MigoQtHostServiceChannel> channel = MigoQtHostServiceChannel::open(fake_migo::session());

        fake_migo::deliver_host_service_call_from_engine_thread(MIGO_HOST_SERVICE_AD, 0, 30, "{}");
        QTRY_VERIFY(answerTo(30).has_value());
        // The App answered it; a second answer through a copy of the reply is refused here, not by the library.
        MigoQtHostServiceReply reply = channel->reply(31);
        QCOMPARE(reply.fail(QString()), MIGO_ERROR_INVALID_ARGUMENT);
        fake_migo::deliver_host_service_call_from_engine_thread(MIGO_HOST_SERVICE_AD, 0, 31, "{}");
        QTRY_VERIFY(answerTo(31).has_value());
        QCOMPARE(reply.fail(QStringLiteral("too late")), MIGO_ERROR_INVALID_ARGUMENT);
        MigoQtHostServiceReply copy = reply;
        QCOMPARE(copy.succeed(), MIGO_ERROR_INVALID_STATE);
        QCOMPARE(fake_migo::host_service_answers().size(), std::size_t{2});
        channel->close();
        QCOMPARE(managed.close(), MIGO_OK);
    }

    /// A dialog still up when the game goes is closed with it, and its answer
    /// reaches nobody: the Session it would have reached is destroyed.
    void what_is_shown_goes_with_the_game() {
        Managed fixture;
        MigoManagedSession &managed = fixture.session;
        fake_migo::deliver_host_service_call_from_engine_thread(
            MIGO_HOST_SERVICE_INTERACTION, MIGO_INTERACTION_SHOW_MODAL, 40, json({{"content", "Still there?"}}));
        QTRY_VERIFY(shownWindow<QDialog>() != nullptr);

        QCOMPARE(managed.close(), MIGO_OK);
        QVERIFY(managed.isClosed());
        QTRY_VERIFY(shownWindow<QDialog>() == nullptr);
        QVERIFY(!answerTo(40).has_value());
        QVERIFY(!fake_migo::session_is_alive());
    }

    /// compressImage: the picture kept, upright, at the size and quality asked;
    /// a PNG stays a PNG; what is not there, or would be too large, fails. The
    /// Session is handed the Host Kit's own file, and nothing is left behind.
    void an_image_is_compressed_as_asked() {
        QTemporaryDir scratch;
        TakenFiles taken(scratch.filePath(QStringLiteral("session-tmp")));
        Managed fixture;
        const QString jpeg = scratch.filePath(QStringLiteral("quadrants.jpg"));
        writeQuadrants(jpeg, "jpeg");

        const auto low = askImages(MIGO_IMAGE_COMPRESS_IMAGE, 1, {{"src", jpeg}, {"quality", 10}});
        QCOMPARE(low.status, MIGO_HOST_SERVICE_STATUS_OK);
        const QString staged = parsed(low.payload).value(QStringLiteral("tempFilePath")).toString();
        QVERIFY2(staged.endsWith(QStringLiteral(".jpg")), qPrintable(staged));
        QVERIFY(!staged.startsWith(scratch.path()));
        const QString lowFile = taken[staged];
        QVERIFY2(!lowFile.isEmpty(), qPrintable(staged));
        QVERIFY(nothingLeftBeside(staged));
        const QImage picture(lowFile);
        QCOMPARE(picture.size(), QSize(256, 192));
        QVERIFY(looksLike(picture, {64, 48}, quadrant(true, true)));
        QVERIFY(looksLike(picture, {192, 48}, quadrant(false, true)));
        QVERIFY(looksLike(picture, {64, 144}, quadrant(true, false)));
        QVERIFY(looksLike(picture, {192, 144}, quadrant(false, false)));
        QVERIFY(bytesOf(jpeg).size() > 0);

        // The quality asked for: a higher one keeps more, in more bytes.
        const auto high = askImages(MIGO_IMAGE_COMPRESS_IMAGE, 2, {{"src", jpeg}, {"quality", 90}});
        const QString highFile = taken[parsed(high.payload).value(QStringLiteral("tempFilePath")).toString()];
        QVERIFY(QFileInfo(lowFile).size() < QFileInfo(highFile).size());

        // A width alone scales the height with it; both are both.
        const auto narrow = askImages(MIGO_IMAGE_COMPRESS_IMAGE, 3, {{"src", jpeg}, {"compressedWidth", 64}});
        QCOMPARE(QImage(taken[parsed(narrow.payload).value(QStringLiteral("tempFilePath")).toString()]).size(),
                 QSize(64, 48));
        const auto exact =
            askImages(MIGO_IMAGE_COMPRESS_IMAGE, 4, {{"src", jpeg}, {"compressedWidth", 30}, {"compressedHeight", 40}});
        QCOMPARE(QImage(taken[parsed(exact.payload).value(QStringLiteral("tempFilePath")).toString()]).size(),
                 QSize(30, 40));

        // Stored turned, it comes out upright: what was top-left is now top-right.
        const QString turned = scratch.filePath(QStringLiteral("turned.jpg"));
        writeTurnedQuadrants(turned);
        const auto upright = askImages(MIGO_IMAGE_COMPRESS_IMAGE, 5, {{"src", turned}});
        QImageReader uprightReader(taken[parsed(upright.payload).value(QStringLiteral("tempFilePath")).toString()]);
        uprightReader.setAutoTransform(false);
        const QImage uprightPicture = uprightReader.read();
        QCOMPARE(uprightPicture.size(), QSize(192, 256));
        QVERIFY(looksLike(uprightPicture, {144, 64}, quadrant(true, true)));
        QVERIFY(looksLike(uprightPicture, {48, 64}, quadrant(true, false)));

        // A PNG stays a PNG.
        const QString png = scratch.filePath(QStringLiteral("quadrants.png"));
        writeQuadrants(png, "png");
        const auto stillPng = askImages(MIGO_IMAGE_COMPRESS_IMAGE, 6, {{"src", png}, {"quality", 10}});
        const QString pngStaged = parsed(stillPng.payload).value(QStringLiteral("tempFilePath")).toString();
        QVERIFY(pngStaged.endsWith(QStringLiteral(".png")));
        QCOMPARE(formatOf(taken[pngStaged]), QByteArray("png"));

        const auto missing = askImages(MIGO_IMAGE_COMPRESS_IMAGE, 7, {{"src", scratch.filePath("none.jpg")}});
        QCOMPARE(missing.status, MIGO_HOST_SERVICE_STATUS_FAIL);
        QCOMPARE(missing.message, std::string("file not found"));
        const auto notAnImage = scratch.filePath(QStringLiteral("notes.txt"));
        writeFile(notAnImage, "hello");
        QCOMPARE(askImages(MIGO_IMAGE_COMPRESS_IMAGE, 8, {{"src", notAnImage}}).message, std::string("invalid image"));
        QCOMPARE(askImages(MIGO_IMAGE_COMPRESS_IMAGE, 9,
                           {{"src", jpeg}, {"compressedWidth", 10000}, {"compressedHeight", 10000}}).message,
                 std::string("image too large"));
        QCOMPARE(askImages(MIGO_IMAGE_COMPRESS_IMAGE, 10, {}).message, std::string("src is required"));

        // What the Session does not take -- its game has gone -- is removed.
        fake_migo::set_host_service_completion(nullptr);
        const auto untaken = askImages(MIGO_IMAGE_COMPRESS_IMAGE, 11, {{"src", jpeg}});
        const QString left = parsed(untaken.payload).value(QStringLiteral("tempFilePath")).toString();
        QVERIFY(!left.isEmpty());
        QVERIFY(!QFileInfo::exists(left));
        QVERIFY(nothingLeftBeside(left));
        QCOMPARE(fixture.session.close(), MIGO_OK);
    }

    /// chooseImage: the player picks in the file dialog, and the game is given
    /// copies -- a JPEG, PNG, GIF, BMP or WebP as it is, anything else Qt reads
    /// as a JPEG -- never the player's own files, and no more than it asked for.
    void the_player_picks_images_and_the_game_gets_copies() {
        QTemporaryDir scratch;
        TakenFiles taken(scratch.filePath(QStringLiteral("session-tmp")));
        Managed fixture;
        const QString photos = scratch.filePath(QStringLiteral("photos"));
        QVERIFY(QDir().mkpath(photos));
        writeQuadrants(photos + QStringLiteral("/a.png"), "png");
        writeQuadrants(photos + QStringLiteral("/b.ppm"), "ppm");
        writeQuadrants(photos + QStringLiteral("/c.png"), "png");
        writeFile(photos + QStringLiteral("/notes.txt"), "hello");

        fake_migo::deliver_host_service_call_from_engine_thread(MIGO_HOST_SERVICE_IMAGE, MIGO_IMAGE_CHOOSE_IMAGE, 1,
                                                                json({{"count", 2}}));
        QTRY_VERIFY(fileDialog() != nullptr);
        QCOMPARE(fileDialog()->fileMode(), QFileDialog::ExistingFiles);
        choose(*fileDialog(), photos, {QStringLiteral("a.png"), QStringLiteral("b.ppm"), QStringLiteral("c.png")});
        QTRY_VERIFY_WITH_TIMEOUT(answerTo(1).has_value(), 10000);
        QCOMPARE(answerTo(1)->status, MIGO_HOST_SERVICE_STATUS_OK);
        const QJsonObject chosen = parsed(answerTo(1)->payload);
        const QJsonArray paths = chosen.value(QStringLiteral("tempFilePaths")).toArray();
        const QJsonArray files = chosen.value(QStringLiteral("tempFiles")).toArray();
        QCOMPARE(paths.size(), 2);
        QCOMPARE(files.size(), 2);
        for (int i = 0; i < 2; ++i) {
            QCOMPARE(files.at(i).toObject().value(QStringLiteral("path")).toString(), paths.at(i).toString());
            const QString file = taken[paths.at(i).toString()];
            QVERIFY2(!file.isEmpty(), qPrintable(paths.at(i).toString()));
            QCOMPARE(files.at(i).toObject().value(QStringLiteral("size")).toInteger(), QFileInfo(file).size());
        }
        // The PNG as it was; the PPM, which content cannot draw, as a JPEG of it.
        QCOMPARE(bytesOf(taken[paths.at(0).toString()]), bytesOf(photos + QStringLiteral("/a.png")));
        const QString converted = taken[paths.at(1).toString()];
        QCOMPARE(formatOf(converted), QByteArray("jpeg"));
        QVERIFY(looksLike(QImage(converted), {64, 48}, quadrant(true, true)));
        QVERIFY(QFileInfo::exists(photos + QStringLiteral("/a.png")));
        QVERIFY(QFileInfo::exists(photos + QStringLiteral("/b.ppm")));
        QVERIFY(nothingLeftBeside(paths.at(0).toString()));

        // Dismissed, it is the failure "cancel".
        fake_migo::deliver_host_service_call_from_engine_thread(MIGO_HOST_SERVICE_IMAGE, MIGO_IMAGE_CHOOSE_IMAGE, 2,
                                                                json({{"count", 1}}));
        QTRY_VERIFY(fileDialog() != nullptr);
        QCOMPARE(fileDialog()->fileMode(), QFileDialog::ExistingFile);
        fileDialog()->reject();
        QTRY_VERIFY(answerTo(2).has_value());
        QCOMPARE(answerTo(2)->message, std::string("cancel"));

        // What is no image fails, naming it, and leaves nothing staged.
        fake_migo::deliver_host_service_call_from_engine_thread(MIGO_HOST_SERVICE_IMAGE, MIGO_IMAGE_CHOOSE_IMAGE, 3,
                                                                json({}));
        QTRY_VERIFY(fileDialog() != nullptr);
        choose(*fileDialog(), photos, {QStringLiteral("a.png"), QStringLiteral("notes.txt")});
        QTRY_VERIFY_WITH_TIMEOUT(answerTo(3).has_value(), 10000);
        QCOMPARE(answerTo(3)->message, std::string("notes.txt: invalid image"));
        QVERIFY(nothingLeftBeside(paths.at(0).toString()));

        // The camera is not offered.
        QCOMPARE(askImages(MIGO_IMAGE_CHOOSE_IMAGE, 4, {{"sourceType", QJsonArray{"camera"}}}).message,
                 std::string("not supported"));
        QVERIFY(fileDialog() == nullptr);
        QCOMPARE(fixture.session.close(), MIGO_OK);
    }

    /// chooseMedia offers images, each with its size as seen -- upright; a call
    /// for only videos, or only the camera, is refused.
    void choose_media_offers_images() {
        QTemporaryDir scratch;
        TakenFiles taken(scratch.filePath(QStringLiteral("session-tmp")));
        Managed fixture;
        writeTurnedQuadrants(scratch.filePath(QStringLiteral("turned.jpg")));

        fake_migo::deliver_host_service_call_from_engine_thread(MIGO_HOST_SERVICE_IMAGE, MIGO_IMAGE_CHOOSE_MEDIA, 1,
                                                                json({{"mediaType", QJsonArray{"mix"}}, {"count", 1}}));
        QTRY_VERIFY(fileDialog() != nullptr);
        choose(*fileDialog(), scratch.path(), {QStringLiteral("turned.jpg")});
        QTRY_VERIFY_WITH_TIMEOUT(answerTo(1).has_value(), 10000);
        const QJsonObject chosen = parsed(answerTo(1)->payload);
        QCOMPARE(chosen.value(QStringLiteral("type")).toString(), QStringLiteral("image"));
        const QJsonArray items = chosen.value(QStringLiteral("tempFiles")).toArray();
        QCOMPARE(items.size(), 1);
        const QJsonObject item = items.at(0).toObject();
        QCOMPARE(item.value(QStringLiteral("fileType")).toString(), QStringLiteral("image"));
        QCOMPARE(item.value(QStringLiteral("width")).toInt(), 192);
        QCOMPARE(item.value(QStringLiteral("height")).toInt(), 256);
        const QString file = taken[item.value(QStringLiteral("tempFilePath")).toString()];
        QCOMPARE(bytesOf(file), bytesOf(scratch.filePath(QStringLiteral("turned.jpg"))));
        QCOMPARE(item.value(QStringLiteral("size")).toInteger(), QFileInfo(file).size());

        QCOMPARE(askImages(MIGO_IMAGE_CHOOSE_MEDIA, 2, {{"mediaType", QJsonArray{"video"}}}).message,
                 std::string("not supported"));
        QCOMPARE(askImages(MIGO_IMAGE_CHOOSE_MEDIA, 3, {{"sourceType", QJsonArray{"camera"}}}).message,
                 std::string("not supported"));
        QCOMPARE(fixture.session.close(), MIGO_OK);
    }

    /// chooseMessageFile: the files of the type asked for, each with the name
    /// the player saw, its kind and when it was last changed.
    void the_player_picks_message_files() {
        QTemporaryDir scratch;
        TakenFiles taken(scratch.filePath(QStringLiteral("session-tmp")));
        Managed fixture;
        const QString picked = scratch.filePath(QStringLiteral("picked"));
        QVERIFY(QDir().mkpath(picked));
        writeQuadrants(picked + QStringLiteral("/a.png"), "png");
        writeFile(picked + QStringLiteral("/clip.mp4"), "not really a video");
        writeFile(picked + QStringLiteral("/notes.txt"), "hello");
        const QStringList all{QStringLiteral("a.png"), QStringLiteral("clip.mp4"), QStringLiteral("notes.txt")};

        fake_migo::deliver_host_service_call_from_engine_thread(MIGO_HOST_SERVICE_IMAGE, MIGO_IMAGE_CHOOSE_MESSAGE_FILE,
                                                                1, json({{"count", 3}, {"type", "all"}}));
        QTRY_VERIFY(fileDialog() != nullptr);
        choose(*fileDialog(), picked, all);
        QTRY_VERIFY_WITH_TIMEOUT(answerTo(1).has_value(), 10000);
        const QJsonArray everything = parsed(answerTo(1)->payload).value(QStringLiteral("tempFiles")).toArray();
        QCOMPARE(everything.size(), 3);
        QStringList kinds;
        for (const QJsonValue &file : everything) {
            kinds << file.toObject().value(QStringLiteral("name")).toString() + QLatin1Char('=') +
                         file.toObject().value(QStringLiteral("type")).toString();
        }
        QCOMPARE(kinds, QStringList({"a.png=image", "clip.mp4=video", "notes.txt=file"}));

        // "file" is what is neither an image nor a video.
        fake_migo::deliver_host_service_call_from_engine_thread(MIGO_HOST_SERVICE_IMAGE, MIGO_IMAGE_CHOOSE_MESSAGE_FILE,
                                                                2, json({{"count", 3}, {"type", "file"}}));
        QTRY_VERIFY(fileDialog() != nullptr);
        choose(*fileDialog(), picked, all);
        QTRY_VERIFY_WITH_TIMEOUT(answerTo(2).has_value(), 10000);
        const QJsonArray files = parsed(answerTo(2)->payload).value(QStringLiteral("tempFiles")).toArray();
        QCOMPARE(files.size(), 1);
        const QJsonObject notes = files.at(0).toObject();
        QCOMPARE(notes.value(QStringLiteral("name")).toString(), QStringLiteral("notes.txt"));
        QCOMPARE(notes.value(QStringLiteral("type")).toString(), QStringLiteral("file"));
        QCOMPARE(notes.value(QStringLiteral("size")).toInteger(), qint64{5});
        QCOMPARE(notes.value(QStringLiteral("time")).toInteger(),
                 QFileInfo(picked + QStringLiteral("/notes.txt")).lastModified().toSecsSinceEpoch());
        QCOMPARE(bytesOf(taken[notes.value(QStringLiteral("path")).toString()]), QByteArray("hello"));

        // Extensions narrow files: a .txt is not a .md.
        fake_migo::deliver_host_service_call_from_engine_thread(
            MIGO_HOST_SERVICE_IMAGE, MIGO_IMAGE_CHOOSE_MESSAGE_FILE, 3,
            json({{"count", 1}, {"type", "file"}, {"extension", QJsonArray{".MD"}}}));
        QTRY_VERIFY(fileDialog() != nullptr);
        QCOMPARE(fileDialog()->nameFilters(), QStringList{QStringLiteral("Files (*.md)")});
        choose(*fileDialog(), picked, {QStringLiteral("notes.txt")});
        QTRY_VERIFY_WITH_TIMEOUT(answerTo(3).has_value(), 10000);
        QCOMPARE(parsed(answerTo(3)->payload).value(QStringLiteral("tempFiles")).toArray().size(), 0);
        QCOMPARE(fixture.session.close(), MIGO_OK);
    }

    /// saveFileToDisk: the dialog to save, offering the file's name, and the
    /// file copied where the player chose.
    void a_file_is_saved_where_the_player_chooses() {
        QTemporaryDir scratch;
        Managed fixture;
        const QString source = scratch.filePath(QStringLiteral("report.bin"));
        writeFile(source, QByteArray(300 * 1024, 'm'));
        const QString chosen = scratch.filePath(QStringLiteral("chosen"));
        QVERIFY(QDir().mkpath(chosen));

        fake_migo::deliver_host_service_call_from_engine_thread(MIGO_HOST_SERVICE_IMAGE, MIGO_IMAGE_SAVE_FILE_TO_DISK, 1,
                                                                json({{"filePath", source}}));
        QTRY_VERIFY(fileDialog() != nullptr);
        QCOMPARE(fileDialog()->acceptMode(), QFileDialog::AcceptSave);
        QCOMPARE(fileDialog()->findChild<QLineEdit *>(QStringLiteral("fileNameEdit"))->text(),
                 QStringLiteral("report.bin"));
        choose(*fileDialog(), chosen, {QStringLiteral("saved.bin")});
        QTRY_VERIFY_WITH_TIMEOUT(answerTo(1).has_value(), 10000);
        QCOMPARE(answerTo(1)->status, MIGO_HOST_SERVICE_STATUS_OK);
        QCOMPARE(bytesOf(chosen + QStringLiteral("/saved.bin")), bytesOf(source));

        fake_migo::deliver_host_service_call_from_engine_thread(MIGO_HOST_SERVICE_IMAGE, MIGO_IMAGE_SAVE_FILE_TO_DISK, 2,
                                                                json({{"filePath", source}}));
        QTRY_VERIFY(fileDialog() != nullptr);
        fileDialog()->reject();
        QTRY_VERIFY(answerTo(2).has_value());
        QCOMPARE(answerTo(2)->message, std::string("cancel"));

        QCOMPARE(askImages(MIGO_IMAGE_SAVE_FILE_TO_DISK, 3, {{"filePath", scratch.filePath("none")}}).message,
                 std::string("file not found"));
        QVERIFY(fileDialog() == nullptr);
        QCOMPARE(fixture.session.close(), MIGO_OK);
    }

    /// saveImageToPhotosAlbum: into the player's Pictures folder, named as a
    /// camera names a photo; what is no image is refused.
    void an_image_is_saved_to_the_pictures_folder() {
        QTemporaryDir scratch;
        Managed fixture;
        const QString png = scratch.filePath(QStringLiteral("shot.bin"));
        writeQuadrants(png, "png");

        QCOMPARE(askImages(MIGO_IMAGE_SAVE_IMAGE_TO_PHOTOS_ALBUM, 1, {{"filePath", png}}).status,
                 MIGO_HOST_SERVICE_STATUS_OK);
        QCOMPARE(askImages(MIGO_IMAGE_SAVE_IMAGE_TO_PHOTOS_ALBUM, 2, {{"filePath", png}}).status,
                 MIGO_HOST_SERVICE_STATUS_OK);
        const QStringList saved = QDir(pictures()).entryList(QDir::Files, QDir::Name);
        QCOMPARE(saved.size(), 2);
        for (const QString &name : saved) {
            QVERIFY2(QRegularExpression(QStringLiteral("^IMG_\\d+(_\\d+)?\\.png$")).match(name).hasMatch(), qPrintable(name));
            QCOMPARE(bytesOf(pictures() + QLatin1Char('/') + name), bytesOf(png));
        }

        const QString notes = scratch.filePath(QStringLiteral("notes.png"));
        writeFile(notes, "hello");
        QCOMPARE(askImages(MIGO_IMAGE_SAVE_IMAGE_TO_PHOTOS_ALBUM, 3, {{"filePath", notes}}).message,
                 std::string("not an image"));
        QCOMPARE(askImages(MIGO_IMAGE_SAVE_IMAGE_TO_PHOTOS_ALBUM, 4, {{"filePath", scratch.filePath("none.png")}}).message,
                 std::string("file not found"));
        QCOMPARE(QDir(pictures()).entryList(QDir::Files).size(), 2);
        QVERIFY(QDir(pictures()).removeRecursively());
        QCOMPARE(fixture.session.close(), MIGO_OK);
    }

    /// One file dialog at a time; one still up when the game goes is closed
    /// with it, and its answer reaches nobody.
    void a_file_dialog_is_one_at_a_time_and_goes_with_the_game() {
        Managed fixture;
        fake_migo::deliver_host_service_call_from_engine_thread(MIGO_HOST_SERVICE_IMAGE, MIGO_IMAGE_CHOOSE_IMAGE, 1,
                                                                json({}));
        QTRY_VERIFY(fileDialog() != nullptr);
        QCOMPARE(askImages(MIGO_IMAGE_CHOOSE_MESSAGE_FILE, 2, {{"count", 1}}).message,
                 std::string("another file dialog is showing"));

        QCOMPARE(fixture.session.close(), MIGO_OK);
        QVERIFY(fixture.session.isClosed());
        QTRY_VERIFY(fileDialog() == nullptr);
        QVERIFY(!answerTo(1).has_value());
    }

    /// previewImage: the viewer over the game, from the one asked for; the
    /// arrows go between them and no further; each image is held open, so what
    /// the engine lent may go once the call is answered; an http(s) one is
    /// downloaded; what is no image shows a warning in its place; Esc closes it,
    /// and nothing is left behind.
    void a_preview_shows_the_images_from_the_one_asked_for() {
        QTemporaryDir scratch;
        Managed fixture;
        const QString first = scratch.filePath(QStringLiteral("first.png"));
        const QString second = scratch.filePath(QStringLiteral("second.png"));
        const QString notes = scratch.filePath(QStringLiteral("notes.txt"));
        writeQuadrants(first, "png");
        writeQuadrants(second, "png", {64, 48});
        writeFile(notes, "hello");
        LocalWeb web({{"/third.png", bytesOf(first)}});
        QVERIFY(web.listening());

        const auto shown = askImages(MIGO_IMAGE_PREVIEW_IMAGE, 1,
                                     {{"urls", QJsonArray{first, second, web.url("/third.png"), notes}},
                                      {"current", second}});
        QCOMPARE(shown.status, MIGO_HOST_SERVICE_STATUS_OK);
        ImageViewer *viewer = imageViewer();
        QVERIFY(viewer != nullptr);
        QCOMPARE(viewer->count(), 4);
        QCOMPARE(viewer->current(), 1);
        QVERIFY(viewer->showsImage());

        // Taken back once the call is answered, they still show.
        QVERIFY(QFile::remove(first));
        QVERIFY(QFile::remove(second));
        QTest::keyClick(viewer, Qt::Key_Left);
        QCOMPARE(viewer->current(), 0);
        QVERIFY(viewer->showsImage());
        QTest::keyClick(viewer, Qt::Key_Left);
        QCOMPARE(viewer->current(), 0);
        QTest::keyClick(viewer, Qt::Key_Right);
        QVERIFY(viewer->showsImage());
        QTest::keyClick(viewer, Qt::Key_Right);
        QCOMPARE(viewer->current(), 2);
        QVERIFY(viewer->showsImage());
        QTest::keyClick(viewer, Qt::Key_Right);
        QCOMPARE(viewer->current(), 3);
        QVERIFY(!viewer->showsImage());
        QTest::keyClick(viewer, Qt::Key_Right);
        QCOMPARE(viewer->current(), 3);
        QVERIFY(stagedFiles().isEmpty());

        QTest::keyClick(viewer, Qt::Key_Escape);
        QTRY_VERIFY(imageViewer() == nullptr);
        QCOMPARE(fixture.session.close(), MIGO_OK);
    }

    /// A preview fails with the first image it cannot have; previewMedia shows
    /// images from the one asked for and refuses videos; another preview
    /// replaces the one shown, and the game going closes it.
    void a_preview_fails_replaces_and_goes_with_the_game() {
        QTemporaryDir scratch;
        Managed fixture;
        const QString image = scratch.filePath(QStringLiteral("image.png"));
        writeQuadrants(image, "png");
        LocalWeb web({});
        QVERIFY(web.listening());

        QCOMPARE(askImages(MIGO_IMAGE_PREVIEW_IMAGE, 1, {{"urls", QJsonArray{image, web.url("/missing.png")}}}).message,
                 std::string("download failed: HTTP 404"));
        QCOMPARE(askImages(MIGO_IMAGE_PREVIEW_IMAGE, 2, {{"urls", QJsonArray{scratch.filePath("none.png")}}}).message,
                 std::string("file not found"));
        QCOMPARE(askImages(MIGO_IMAGE_PREVIEW_IMAGE, 3, {{"urls", QJsonArray{}}}).message,
                 std::string("nothing to preview"));
        QCOMPARE(askImages(MIGO_IMAGE_PREVIEW_MEDIA, 4,
                           {{"sources", QJsonArray{QJsonObject{{"url", image}},
                                                   QJsonObject{{"url", image}, {"type", "video"}}}}})
                     .message,
                 std::string("not supported"));
        QVERIFY(imageViewer() == nullptr);
        QVERIFY(stagedFiles().isEmpty());

        QCOMPARE(askImages(MIGO_IMAGE_PREVIEW_MEDIA, 5,
                           {{"sources", QJsonArray{QJsonObject{{"url", image}}, QJsonObject{{"url", image}, {"type", "image"}}}},
                            {"current", 1}})
                     .status,
                 MIGO_HOST_SERVICE_STATUS_OK);
        QVERIFY(imageViewer() != nullptr);
        QCOMPARE(imageViewer()->count(), 2);
        QCOMPARE(imageViewer()->current(), 1);

        QCOMPARE(askImages(MIGO_IMAGE_PREVIEW_IMAGE, 6, {{"urls", QJsonArray{image}}}).status,
                 MIGO_HOST_SERVICE_STATUS_OK);
        QTRY_COMPARE(imageViewer()->count(), 1);
        int shown = 0;
        for (QWidget *widget : QApplication::topLevelWidgets()) {
            if (dynamic_cast<ImageViewer *>(widget) != nullptr && widget->isVisible()) ++shown;
        }
        QCOMPARE(shown, 1);

        QCOMPARE(fixture.session.close(), MIGO_OK);
        QVERIFY(fixture.session.isClosed());
        QTRY_VERIFY(imageViewer() == nullptr);
    }

    /// GeoClue: a client of the App's own, named by its desktop file and asked
    /// at the accuracy wanted, started -- its agent deciding -- and answered with
    /// the first position; requests while it runs share it; it stops and goes
    /// once answered. An unknown altitude or speed is no altitude or speed.
    void geoclue_says_where_the_device_is() {
        using namespace std::chrono_literals;
        QDBusConnection bus = QDBusConnection::sessionBus();
        QVERIFY2(bus.isConnected(), "no session bus: run this test under dbus-run-session");
        FakeGeoClue geoclue(bus);
        QVERIFY(geoclue.registered());
        Geolocation where(bus, Geolocation::Source::GeoClue, QStringLiteral("com.example.game"), [] { return QString(); });

        std::optional<Geolocation::Outcome> exact;
        std::optional<Geolocation::Outcome> city;
        where.locate(Geolocation::Accuracy::Exact, 5s, [&exact](const Geolocation::Outcome &outcome) { exact = outcome; });
        where.locate(Geolocation::Accuracy::City, 5s, [&city](const Geolocation::Outcome &outcome) { city = outcome; });
        QTRY_VERIFY(exact.has_value() && city.has_value());
        QVERIFY(exact->allowed);
        QVERIFY(exact->position.has_value());
        QCOMPARE(exact->position->latitude, 31.2304);
        QCOMPARE(exact->position->longitude, 121.4737);
        QCOMPARE(exact->position->accuracy, 25.0);
        QVERIFY(!exact->position->altitude.has_value());
        QVERIFY(!exact->position->speed.has_value());
        QCOMPARE(city->position->latitude, 31.2304);
        QCOMPARE(geoclue.created, 1);
        QCOMPARE(geoclue.started, QStringList{QStringLiteral("com.example.game 8")});
        QTRY_COMPARE(geoclue.deleted, 1);

        geoclue.location.altitude = 12.5;
        geoclue.location.speed = 3;
        std::optional<Geolocation::Outcome> moving;
        where.locate(Geolocation::Accuracy::Street, 5s, [&moving](const Geolocation::Outcome &outcome) { moving = outcome; });
        QTRY_VERIFY(moving.has_value());
        QCOMPARE(moving->position->altitude, std::optional<double>(12.5));
        QCOMPARE(moving->position->speed, std::optional<double>(3.0));
        QCOMPARE(geoclue.started.last(), QStringLiteral("com.example.game 6"));

        // Deciding is starting: answered as soon as it has, position or not.
        geoclue.report = false;
        std::optional<Geolocation::Outcome> decided;
        where.decide([&decided](const Geolocation::Outcome &outcome) { decided = outcome; });
        QTRY_VERIFY(decided.has_value());
        QVERIFY(decided->allowed);
        QVERIFY(!decided->position.has_value());
        QTRY_COMPARE(geoclue.deleted, 3);

        // Started, but no position in time.
        std::optional<Geolocation::Outcome> late;
        where.locate(Geolocation::Accuracy::Street, 200ms, [&late](const Geolocation::Outcome &outcome) { late = outcome; });
        QTRY_VERIFY(late.has_value());
        QVERIFY(late->allowed);
        QVERIFY(!late->position.has_value());
        QCOMPARE(late->error, QStringLiteral("unable to get location"));
        QTRY_COMPARE(geoclue.deleted, 4);

        // The agent refuses.
        geoclue.refuse = true;
        std::optional<Geolocation::Outcome> refused;
        where.decide([&refused](const Geolocation::Outcome &outcome) { refused = outcome; });
        QTRY_VERIFY(refused.has_value());
        QVERIFY(!refused->allowed);
        QVERIFY(refused->denied);
        QVERIFY2(refused->error.startsWith(QStringLiteral("location access denied")), qPrintable(refused->error));
    }

    /// Without GeoClue, nobody decides and there is no position.
    void without_geoclue_there_is_no_one_to_ask() {
        Geolocation where(QDBusConnection::sessionBus(), Geolocation::Source::GeoClue, QStringLiteral("com.example.game"),
                          [] { return QString(); });
        std::optional<Geolocation::Outcome> decided;
        where.decide([&decided](const Geolocation::Outcome &outcome) { decided = outcome; });
        QTRY_VERIFY(decided.has_value());
        QVERIFY(!decided->allowed);
        QVERIFY(!decided->denied);
        QVERIFY2(decided->error.startsWith(QStringLiteral("location services are not available")),
                 qPrintable(decided->error));
    }

    /// Inside a sandbox, the portal: its dialog over the game's window, at the
    /// accuracy wanted on its own scale; the player allows or refuses; the
    /// session is closed once answered.
    void the_portal_says_where_the_device_is() {
        using namespace std::chrono_literals;
        QDBusConnection bus = QDBusConnection::sessionBus();
        FakeLocationPortal portal(bus);
        QVERIFY(portal.registered());
        Geolocation where(bus, Geolocation::Source::Portal, QString(), [] { return QStringLiteral("x11:2a"); });

        std::optional<Geolocation::Outcome> located;
        where.locate(Geolocation::Accuracy::Exact, 5s, [&located](const Geolocation::Outcome &outcome) { located = outcome; });
        QTRY_VERIFY(located.has_value());
        QVERIFY(located->allowed);
        QVERIFY(located->position.has_value());
        QCOMPARE(located->position->latitude, 48.8566);
        QCOMPARE(located->position->altitude, std::optional<double>(35.0));
        QVERIFY(!located->position->speed.has_value());
        QCOMPARE(portal.asked, QStringList{QStringLiteral("5 x11:2a")});
        QTRY_COMPARE(portal.session.closed, 1);

        portal.response = 1;
        std::optional<Geolocation::Outcome> refused;
        where.decide([&refused](const Geolocation::Outcome &outcome) { refused = outcome; });
        QTRY_VERIFY(refused.has_value());
        QVERIFY(!refused->allowed);
        QVERIFY(refused->denied);
        QCOMPARE(portal.asked.last(), QStringLiteral("2 x11:2a"));
    }

    /// The permission service: the photo album granted and recorded, a scope
    /// the desktop does not decide refused and not recorded.
    void the_desktop_decides_the_scopes_it_gates() {
        Managed fixture;
        fake_migo::deliver_host_service_call_from_engine_thread(
            MIGO_HOST_SERVICE_PERMISSION, MIGO_PERMISSION_REQUEST_SCOPE, 1,
            json({{"scope", "scope.writePhotosAlbum"}, {"scopeId", static_cast<int>(MIGO_SCOPE_WRITE_PHOTOS_ALBUM)}}));
        QTRY_VERIFY(answerTo(1).has_value());
        QCOMPARE(answerTo(1)->status, MIGO_HOST_SERVICE_STATUS_OK);
        QCOMPARE(fake_migo::scope_state(MIGO_SCOPE_WRITE_PHOTOS_ALBUM), MIGO_SCOPE_STATE_GRANTED);

        fake_migo::deliver_host_service_call_from_engine_thread(
            MIGO_HOST_SERVICE_PERMISSION, MIGO_PERMISSION_REQUEST_SCOPE, 2,
            json({{"scope", "scope.userInfo"}, {"scopeId", static_cast<int>(MIGO_SCOPE_USER_INFO)}}));
        QTRY_VERIFY(answerTo(2).has_value());
        QCOMPARE(answerTo(2)->message, std::string("auth deny"));
        QCOMPARE(fake_migo::scope_state(MIGO_SCOPE_USER_INFO), MIGO_SCOPE_STATE_UNKNOWN);
        QCOMPARE(fixture.session.close(), MIGO_OK);
    }

    /// The accelerometer the kernel shows, in g and in the screen's frame:
    /// udev's mount matrix over the kernel's, the display's accelerometer over
    /// the base's; an offset and a scale applied; read until stopped. What the
    /// device lacks is named.
    void the_accelerometer_reads_in_g_in_the_screens_frame() {
        using namespace std::chrono_literals;
        QTemporaryDir sysfs;
        // 1000 counts of 0.00980665 m/s^2: one g.
        const QByteArray scale = "0.00980665";
        const QString flat = makeIio(sysfs.path(), QStringLiteral("iio:device0"), QStringLiteral("accel"), {0, 0, 1000},
                                     {{QStringLiteral("in_accel_scale"), scale}});
        {
            Readings readings;
            MotionSensors sensors({{flat, {}}}, readings.sink());
            QCOMPARE(sensors.missing(MIGO_SENSOR_ACCELEROMETER), QString());
            QCOMPARE(sensors.missing(MIGO_SENSOR_GYROSCOPE), QStringLiteral("the device has no gyroscope"));
            QCOMPARE(sensors.missing(MIGO_SENSOR_COMPASS), QStringLiteral("the device has no magnetometer"));
            QCOMPARE(sensors.missing(MIGO_SENSOR_DEVICE_MOTION), QStringLiteral("the device has no magnetometer"));
            sensors.start(MIGO_SENSOR_ACCELEROMETER, 20ms);
            QTRY_VERIFY(readings.size() >= 3);
            const MigoSensorSample face_up = readings.last();
            QCOMPARE(face_up.kind, MIGO_SENSOR_ACCELEROMETER);
            QCOMPARE(face_up.struct_size, static_cast<std::uint32_t>(sizeof(MigoSensorSample)));
            QVERIFY(near(face_up.values[0], 0) && near(face_up.values[1], 0) && near(face_up.values[2], 1));
            sensors.stop(MIGO_SENSOR_ACCELEROMETER);
            QTest::qWait(100);
            const std::size_t stopped = readings.size();
            QTest::qWait(100);
            QCOMPARE(readings.size(), stopped);
        }

        // udev's correction turns the sensor's x into the screen's -y; the
        // kernel's own matrix would have turned it otherwise.
        const QString turned = makeIio(sysfs.path(), QStringLiteral("iio:device1"), QStringLiteral("accel"), {1000, 0, 0},
                                       {{QStringLiteral("in_accel_scale"), scale},
                                        {QStringLiteral("mount_matrix"), "-1, 0, 0; 0, 1, 0; 0, 0, 1"}});
        {
            Readings readings;
            MotionSensors sensors({{turned, {{QStringLiteral("ACCEL_MOUNT_MATRIX"), QStringLiteral("0, 1, 0; -1, 0, 0; 0, 0, 1")}}}},
                                  readings.sink());
            sensors.start(MIGO_SENSOR_ACCELEROMETER, 20ms);
            QTRY_VERIFY(readings.size() >= 1);
            const MigoSensorSample sample = readings.last();
            QVERIFY2(near(sample.values[0], 0) && near(sample.values[1], -1) && near(sample.values[2], 0),
                     qPrintable(QStringLiteral("%1 %2 %3").arg(sample.values[0]).arg(sample.values[1]).arg(sample.values[2])));
        }
        {
            Readings readings;
            MotionSensors sensors({{turned, {}}}, readings.sink());
            sensors.start(MIGO_SENSOR_ACCELEROMETER, 20ms);
            QTRY_VERIFY(readings.size() >= 1);
            QVERIFY(near(readings.last().values[0], -1));
        }

        // A convertible: the base's accelerometer, listed first, is not the
        // device's; and an offset -- one for every axis -- is added before the scale.
        const QString base = makeIio(sysfs.path(), QStringLiteral("iio:device2"), QStringLiteral("accel"), {0, 1000, 0},
                                     {{QStringLiteral("in_accel_scale"), scale}});
        const QString display = makeIio(sysfs.path(), QStringLiteral("iio:device3"), QStringLiteral("accel"), {-10, -10, 990},
                                        {{QStringLiteral("in_accel_scale"), scale}, {QStringLiteral("in_accel_offset"), "10"}});
        {
            Readings readings;
            MotionSensors sensors({{base, {{QStringLiteral("ACCEL_LOCATION"), QStringLiteral("base")}}},
                                   {display, {{QStringLiteral("ACCEL_LOCATION"), QStringLiteral("display")}}}},
                                  readings.sink());
            sensors.start(MIGO_SENSOR_ACCELEROMETER, 20ms);
            QTRY_VERIFY(readings.size() >= 1);
            const MigoSensorSample sample = readings.last();
            QVERIFY2(near(sample.values[1], 0) && near(sample.values[2], 1),
                     qPrintable(QStringLiteral("%1 %2 %3").arg(sample.values[0]).arg(sample.values[1]).arg(sample.values[2])));
        }
    }

    /// The gyroscope in rad/s; the compass and the device's orientation from
    /// gravity and the field, as Android's SensorManager works them out.
    void the_compass_and_orientation_are_worked_out_as_android_does() {
        using namespace std::chrono_literals;
        // Face up, the top towards magnetic north: the field points north and down.
        const auto level = MotionSensors::rotation({0, 0, 9.81}, {0, 22, -42});
        QVERIFY(level.has_value());
        QVERIFY(near(MotionSensors::heading(*level), 0));
        const auto still = MotionSensors::orientation(*level);
        QVERIFY(near(still[0], 0) && near(still[1], 0) && near(still[2], 0));

        // Turned a quarter clockwise on the table: the top points east.
        const auto east = MotionSensors::rotation({0, 0, 9.81}, {-22, 0, -42});
        QVERIFY(near(MotionSensors::heading(*east), 90));
        QVERIFY(near(MotionSensors::orientation(*east)[0], 270));

        // Held up facing north, the screen towards the player.
        const auto upright = MotionSensors::rotation({0, 9.81, 0}, {0, -42, -22});
        const auto standing = MotionSensors::orientation(*upright);
        QVERIFY(near(standing[0], 0) && near(standing[1], 90) && near(standing[2], 0));

        // Tipped to the left, face up: gamma.
        const auto tipped = MotionSensors::orientation(*MotionSensors::rotation({4.905, 0, 8.496}, {0, 22, -42}));
        QVERIFY2(std::abs(tipped[2] - -30) < 0.01, qPrintable(QString::number(tipped[2])));

        // In free fall there is no down, and no east.
        QVERIFY(!MotionSensors::rotation({0, 0, 0}, {0, 22, -42}).has_value());

        // A device with all three: the readings in their units.
        QTemporaryDir sysfs;
        const QString imu = makeIio(sysfs.path(), QStringLiteral("iio:device0"), QStringLiteral("accel"), {0, 0, 1000},
                                    {{QStringLiteral("in_accel_scale"), "0.00981"},
                                     {QStringLiteral("in_anglvel_x_raw"), "0"},
                                     {QStringLiteral("in_anglvel_y_raw"), "0"},
                                     {QStringLiteral("in_anglvel_z_raw"), "500"},
                                     {QStringLiteral("in_anglvel_scale"), "0.001"},
                                     {QStringLiteral("in_magn_x_raw"), "0"},
                                     {QStringLiteral("in_magn_y_raw"), "220"},
                                     {QStringLiteral("in_magn_z_raw"), "-420"},
                                     {QStringLiteral("in_magn_scale"), "0.001"}});
        Readings readings;
        MotionSensors sensors({{imu, {}}}, readings.sink());
        QCOMPARE(sensors.missing(MIGO_SENSOR_GYROSCOPE), QString());
        QCOMPARE(sensors.missing(MIGO_SENSOR_DEVICE_MOTION), QString());
        sensors.start(MIGO_SENSOR_GYROSCOPE, 20ms);
        sensors.start(MIGO_SENSOR_COMPASS, 20ms);
        sensors.start(MIGO_SENSOR_DEVICE_MOTION, 20ms);
        QTRY_VERIFY(readings.size() >= 6);
        bool turning = false;
        bool compass = false;
        bool motion = false;
        {
            const std::lock_guard<std::mutex> lock(readings.mutex);
            for (const MigoSensorSample &sample : readings.samples) {
                if (sample.kind == MIGO_SENSOR_GYROSCOPE) turning = near(sample.values[2], 0.5);
                if (sample.kind == MIGO_SENSOR_COMPASS) {
                    compass = near(sample.values[0], 0) && sample.compass_accuracy == MIGO_COMPASS_ACCURACY_UNKNOWN;
                }
                if (sample.kind == MIGO_SENSOR_DEVICE_MOTION) {
                    motion = near(sample.values[0], 0) && near(sample.values[1], 0) && near(sample.values[2], 0);
                }
            }
        }
        QVERIFY(turning);
        QVERIFY(compass);
        QVERIFY(motion);
    }

    /// The motion service through the Session: a reading reaches it as the
    /// sensors' thread delivers it.
    void a_reading_reaches_the_session() {
        using namespace std::chrono_literals;
        Managed fixture;
        std::shared_ptr<MigoQtHostServiceChannel> channel = MigoQtHostServiceChannel::open(fake_migo::session());
        QTemporaryDir sysfs;
        const QString flat = makeIio(sysfs.path(), QStringLiteral("iio:device0"), QStringLiteral("accel"), {0, 0, 1000},
                                     {{QStringLiteral("in_accel_scale"), "0.00980665"}});
        MotionSensors sensors({{flat, {}}},
                              [channel](const MigoSensorSample &sample) { (void)channel->postSensorSample(sample); });
        sensors.start(MIGO_SENSOR_ACCELEROMETER, 20ms);
        QTRY_VERIFY(!fake_migo::sensor_samples().empty());
        QVERIFY(near(fake_migo::sensor_samples().front().values[2], 1));
        sensors.stop(MIGO_SENSOR_ACCELEROMETER);
        channel->close();
        QCOMPARE(fixture.session.close(), MIGO_OK);
    }

    /// Bluetooth through BlueZ, as the Apple SDK answers it: the adapter, a
    /// scan with its filter and the device it hears, a connection answered once
    /// its services are resolved, the GATT -- services, characteristics, a read,
    /// writes, notifications, the MTU -- the device going, and the codes.
    void bluetooth_is_bluez_as_a_central() {
        Managed fixture;
        std::shared_ptr<MigoQtHostServiceChannel> channel = MigoQtHostServiceChannel::open(fake_migo::session());
        QDBusConnection bus = QDBusConnection::sessionBus();
        std::vector<std::pair<std::uint32_t, QJsonObject>> events;
        std::vector<QStringList> values;
        Bluetooth bluetooth(
            bus, [&events](std::uint32_t event, const QJsonObject &payload) { events.emplace_back(event, payload); },
            [&values](const QString &device, const QString &service, const QString &characteristic,
                      const QByteArray &value) {
                values.push_back({device, service, characteristic, QString::fromLatin1(value.toHex())});
            });
        std::uint64_t next = 100;
        const auto ask = [&](std::uint32_t method, const QJsonObject &request) {
            const std::uint64_t id = ++next;
            fake_migo::expect_host_service_call(MIGO_HOST_SERVICE_BLUETOOTH, id);
            bluetooth.call(method, request, channel->reply(id));
            (void)QTest::qWaitFor([id] { return answerTo(id).has_value(); }, 5000);
            return answerTo(id).value_or(fake_migo::HostServiceAnswer{});
        };
        const auto code = [](const fake_migo::HostServiceAnswer &answer) {
            return answer.has_error_code ? answer.error_code : 0;
        };
        const QJsonObject device{{"deviceId", "aa:bb:cc:dd:ee:ff"}};
        const QJsonObject heart{{"deviceId", "AA:BB:CC:DD:EE:FF"}, {"serviceId", "180D"}, {"characteristicId", "2A37"}};

        // No BlueZ, no Bluetooth; and nothing open, nothing to ask.
        QCOMPARE(code(ask(MIGO_BLUETOOTH_OPEN_ADAPTER, {})), 10001);
        QCOMPARE(code(ask(MIGO_BLUETOOTH_CLOSE_ADAPTER, {})), 0);
        QCOMPARE(code(ask(MIGO_BLUETOOTH_GET_ADAPTER_STATE, {})), 10000);

        FakeBluez bluez(bus);
        QVERIFY(bluez.registered());
        QCOMPARE(ask(MIGO_BLUETOOTH_OPEN_ADAPTER, {}).status, MIGO_HOST_SERVICE_STATUS_OK);
        QCOMPARE(parsed(ask(MIGO_BLUETOOTH_GET_ADAPTER_STATE, {}).payload),
                 (QJsonObject{{"available", true}, {"discovering", false}}));
        QCOMPARE(code(ask(MIGO_BLUETOOTH_OPEN_ADAPTER, {{"mode", "peripheral"}})), 0);

        // A scan: BLE only, the services asked for; the device heard once.
        QCOMPARE(code(ask(MIGO_BLUETOOTH_START_DEVICES_DISCOVERY, {{"services", QJsonArray{"zz"}}})), 10013);
        QCOMPARE(ask(MIGO_BLUETOOTH_START_DEVICES_DISCOVERY, {{"services", QJsonArray{"180D"}}}).status,
                 MIGO_HOST_SERVICE_STATUS_OK);
        QCOMPARE(bluez.adapter.started, 1);
        QCOMPARE(bluez.adapter.filter.value(QStringLiteral("UUIDs")).toStringList(),
                 QStringList{QStringLiteral("0000180d-0000-1000-8000-00805f9b34fb")});
        QCOMPARE(bluez.adapter.filter.value(QStringLiteral("Transport")).toString(), QStringLiteral("le"));
        QTRY_VERIFY(std::any_of(events.begin(), events.end(), [](const auto &event) {
            return event.first == MIGO_BLUETOOTH_EVENT_DEVICE_FOUND;
        }));
        const auto found = std::find_if(events.begin(), events.end(), [](const auto &event) {
            return event.first == MIGO_BLUETOOTH_EVENT_DEVICE_FOUND;
        });
        const QJsonObject heard = found->second.value(QStringLiteral("devices")).toArray().at(0).toObject();
        QCOMPARE(heard.value(QStringLiteral("deviceId")).toString(), QStringLiteral("AA:BB:CC:DD:EE:FF"));
        QCOMPARE(heard.value(QStringLiteral("name")).toString(), QStringLiteral("Heart"));
        QCOMPARE(heard.value(QStringLiteral("RSSI")).toInt(), -60);
        QCOMPARE(heard.value(QStringLiteral("advertisData")).toString(), QStringLiteral("4c000215"));
        QCOMPARE(heard.value(QStringLiteral("advertisServiceUUIDs")).toArray().at(0).toString(),
                 QStringLiteral("0000180D-0000-1000-8000-00805F9B34FB"));
        QVERIFY(std::any_of(events.begin(), events.end(), [](const auto &event) {
            return event.first == MIGO_BLUETOOTH_EVENT_ADAPTER_STATE_CHANGE &&
                   event.second.value(QStringLiteral("discovering")).toBool();
        }));
        const auto foundCount = [&events] {
            return std::count_if(events.begin(), events.end(), [](const auto &event) {
                return event.first == MIGO_BLUETOOTH_EVENT_DEVICE_FOUND;
            });
        };
        bluez.change(kDevicePath, QStringLiteral("org.bluez.Device1"), {{QStringLiteral("RSSI"), QVariant::fromValue(qint16{-50})}});
        QTest::qWait(100);
        QCOMPARE(foundCount(), 1);
        QCOMPARE(parsed(ask(MIGO_BLUETOOTH_GET_DEVICES, {}).payload).value(QStringLiteral("devices")).toArray().size(), 1);
        QCOMPARE(ask(MIGO_BLUETOOTH_STOP_DEVICES_DISCOVERY, {}).status, MIGO_HOST_SERVICE_STATUS_OK);
        QTRY_COMPARE(bluez.adapter.stopped, 1);

        // A connection, answered once its services are resolved.
        QCOMPARE(code(ask(MIGO_BLUETOOTH_CREATE_BLE_CONNECTION, {{"deviceId", "11:22:33:44:55:66"}})), 10002);
        QCOMPARE(code(ask(MIGO_BLUETOOTH_GET_BLE_DEVICE_SERVICES, device)), 10006);
        QCOMPARE(ask(MIGO_BLUETOOTH_CREATE_BLE_CONNECTION, device).status, MIGO_HOST_SERVICE_STATUS_OK);
        QVERIFY(std::any_of(events.begin(), events.end(), [](const auto &event) {
            return event.first == MIGO_BLUETOOTH_EVENT_BLE_CONNECTION_STATE_CHANGE &&
                   event.second.value(QStringLiteral("connected")).toBool();
        }));
        QCOMPARE(code(ask(MIGO_BLUETOOTH_CREATE_BLE_CONNECTION, device)), -1);
        QCOMPARE(parsed(ask(MIGO_BLUETOOTH_GET_BLE_DEVICE_SERVICES, device).payload),
                 (QJsonObject{{"services", QJsonArray{QJsonObject{{"uuid", "0000180D-0000-1000-8000-00805F9B34FB"},
                                                                  {"isPrimary", true}}}}}));
        const QJsonObject characteristic =
            parsed(ask(MIGO_BLUETOOTH_GET_BLE_DEVICE_CHARACTERISTICS, {{"deviceId", "AA:BB:CC:DD:EE:FF"}, {"serviceId", "180D"}})
                       .payload)
                .value(QStringLiteral("characteristics")).toArray().at(0).toObject();
        QCOMPARE(characteristic.value(QStringLiteral("uuid")).toString(), QStringLiteral("00002A37-0000-1000-8000-00805F9B34FB"));
        QCOMPARE(characteristic.value(QStringLiteral("properties")).toObject(),
                 (QJsonObject{{"read", true}, {"write", true}, {"notify", true}, {"indicate", false},
                              {"writeNoResponse", false}, {"writeDefault", true}}));
        QCOMPARE(code(ask(MIGO_BLUETOOTH_GET_BLE_DEVICE_CHARACTERISTICS, {{"deviceId", "AA:BB:CC:DD:EE:FF"}, {"serviceId", "180F"}})),
                 10004);

        // A read is answered as issued; its value arrives on its own.
        QCOMPARE(ask(MIGO_BLUETOOTH_READ_BLE_CHARACTERISTIC_VALUE, heart).status, MIGO_HOST_SERVICE_STATUS_OK);
        QTRY_COMPARE(values.size(), std::size_t{1});
        QCOMPARE(values.at(0), (QStringList{"AA:BB:CC:DD:EE:FF", "0000180D-0000-1000-8000-00805F9B34FB",
                                            "00002A37-0000-1000-8000-00805F9B34FB", "0a0b"}));
        QJsonObject other = heart;
        other.insert(QStringLiteral("characteristicId"), QStringLiteral("2A38"));
        QCOMPARE(code(ask(MIGO_BLUETOOTH_READ_BLE_CHARACTERISTIC_VALUE, other)), 10005);

        // Writes: acknowledged, refused for bad data or a type it does not take.
        QJsonObject writing = heart;
        writing.insert(QStringLiteral("value"), QStringLiteral("0102"));
        QCOMPARE(ask(MIGO_BLUETOOTH_WRITE_BLE_CHARACTERISTIC_VALUE, writing).status, MIGO_HOST_SERVICE_STATUS_OK);
        QCOMPARE(bluez.characteristic.written, QStringList{QStringLiteral("0102 request")});
        writing.insert(QStringLiteral("value"), QStringLiteral("zz"));
        QCOMPARE(code(ask(MIGO_BLUETOOTH_WRITE_BLE_CHARACTERISTIC_VALUE, writing)), 10013);
        writing.insert(QStringLiteral("value"), QStringLiteral("01"));
        writing.insert(QStringLiteral("writeType"), QStringLiteral("writeNoResponse"));
        QCOMPARE(code(ask(MIGO_BLUETOOTH_WRITE_BLE_CHARACTERISTIC_VALUE, writing)), 10007);

        // Notifications: values while listening, none after.
        QJsonObject listening = heart;
        listening.insert(QStringLiteral("state"), true);
        QCOMPARE(ask(MIGO_BLUETOOTH_NOTIFY_BLE_CHARACTERISTIC_VALUE_CHANGE, listening).status, MIGO_HOST_SERVICE_STATUS_OK);
        QVERIFY(bluez.characteristic.notifying);
        bluez.change(kCharacteristicPath, QStringLiteral("org.bluez.GattCharacteristic1"),
                     {{QStringLiteral("Value"), QByteArray::fromHex("4c")}});
        QTRY_COMPARE(values.size(), std::size_t{2});
        QCOMPARE(values.at(1).last(), QStringLiteral("4c"));
        listening.insert(QStringLiteral("state"), false);
        QCOMPARE(ask(MIGO_BLUETOOTH_NOTIFY_BLE_CHARACTERISTIC_VALUE_CHANGE, listening).status, MIGO_HOST_SERVICE_STATUS_OK);
        bluez.change(kCharacteristicPath, QStringLiteral("org.bluez.GattCharacteristic1"),
                     {{QStringLiteral("Value"), QByteArray::fromHex("4d")}});
        QTest::qWait(100);
        QCOMPARE(values.size(), std::size_t{2});

        QCOMPARE(parsed(ask(MIGO_BLUETOOTH_GET_BLE_MTU, device).payload).value(QStringLiteral("mtu")).toInt(), 185);
        QCOMPARE(ask(MIGO_BLUETOOTH_GET_BLE_DEVICE_RSSI, device).message, std::string("not supported"));
        QCOMPARE(code(ask(MIGO_BLUETOOTH_START_BEACON_DISCOVERY, {})), 11000);

        // The device goes: content hears it, and its calls find no connection.
        bluez.change(kDevicePath, QStringLiteral("org.bluez.Device1"),
                     {{QStringLiteral("Connected"), false}, {QStringLiteral("ServicesResolved"), false}});
        QTRY_VERIFY(std::any_of(events.begin(), events.end(), [](const auto &event) {
            return event.first == MIGO_BLUETOOTH_EVENT_BLE_CONNECTION_STATE_CHANGE &&
                   !event.second.value(QStringLiteral("connected")).toBool();
        }));
        QCOMPARE(code(ask(MIGO_BLUETOOTH_GET_BLE_DEVICE_SERVICES, device)), 10006);

        // A connection refused, and one that never resolves in time.
        bluez.device.refuse = true;
        QCOMPARE(code(ask(MIGO_BLUETOOTH_CREATE_BLE_CONNECTION, device)), 10003);
        bluez.device.refuse = false;
        bluez.device.resolve = false;
        bluez.change(kDevicePath, QStringLiteral("org.bluez.Device1"), {{QStringLiteral("Connected"), false}});
        // Heard before the next connection is asked for, not during it.
        QTest::qWait(100);
        QJsonObject hurried = device;
        hurried.insert(QStringLiteral("timeout"), 150);
        QCOMPARE(code(ask(MIGO_BLUETOOTH_CREATE_BLE_CONNECTION, hurried)), 10012);

        // Closed, nothing is open; and a switch turned off is unavailable.
        QCOMPARE(ask(MIGO_BLUETOOTH_CLOSE_ADAPTER, {}).status, MIGO_HOST_SERVICE_STATUS_OK);
        QCOMPARE(code(ask(MIGO_BLUETOOTH_GET_ADAPTER_STATE, {})), 10000);
        bluez.change(kAdapterPath, QStringLiteral("org.bluez.Adapter1"), {{QStringLiteral("Powered"), false}});
        QCOMPARE(code(ask(MIGO_BLUETOOTH_OPEN_ADAPTER, {})), 10001);

        channel->close();
        QCOMPARE(fixture.session.close(), MIGO_OK);
    }

    /// UUIDs in any of their forms, in full and upper case.
    void a_uuid_is_named_in_full() {
        QCOMPARE(Bluetooth::fullUuid(QStringLiteral("180d")), QStringLiteral("0000180D-0000-1000-8000-00805F9B34FB"));
        QCOMPARE(Bluetooth::fullUuid(QStringLiteral("0000180D")), QStringLiteral("0000180D-0000-1000-8000-00805F9B34FB"));
        QCOMPARE(Bluetooth::fullUuid(QStringLiteral("6e400001-b5a3-f393-e0a9-e50e24dcca9e")),
                 QStringLiteral("6E400001-B5A3-F393-E0A9-E50E24DCCA9E"));
        QCOMPARE(Bluetooth::fullUuid(QStringLiteral("not-a-uuid")), QString());
        QCOMPARE(Bluetooth::fullUuid(QStringLiteral("12345")), QString());
    }

private:
    QString pictures() const { return home_.filePath(QStringLiteral("Pictures")); }

    QTemporaryDir home_;
};

QTEST_MAIN(QtHostServicesTest)
#include "qt_host_services_test.moc"

#ifndef MIGO_LINUX_QT6_BLUETOOTH_HPP_
#define MIGO_LINUX_QT6_BLUETOOTH_HPP_

// Internal to the Host Kit, not installed: Bluetooth, BlueZ's.

#include <migo/linux/qt6/host_services.hpp>

#include <QByteArray>
#include <QDBusConnection>
#include <QDBusContext>
#include <QDBusObjectPath>
#include <QHash>
#include <QJsonObject>
#include <QMap>
#include <QObject>
#include <QSet>
#include <QString>
#include <QStringList>
#include <QVariantMap>

#include <cstdint>
#include <functional>
#include <map>
#include <memory>

class QTimer;

namespace migo::linux_host::qt6 {

/// BlueZ's objects as its ObjectManager reports them: each object's
/// interfaces, each with its properties.
using BluezInterfaces = QMap<QString, QVariantMap>;
using BluezObjects = QMap<QDBusObjectPath, BluezInterfaces>;

/// Bluetooth for games as a BLE central, through BlueZ on the system bus: the
/// adapter, scanning, the devices found and connected, and GATT -- connections
/// with their services resolved, reads, writes, notifications, the MTU -- the
/// way the Apple SDK's MigoBluetooth answers them through CoreBluetooth, with
/// the same codes. A device is named by its address, as on Android.
///
/// What BlueZ offers no app -- a connected device's RSSI -- is refused, as are
/// what the reference has for Android only (pairing, asking whether a device is
/// paired, setting the MTU) and the peripheral mode; iBeacons are `Beacons`'s.
/// BlueZ is mirrored -- its objects and their properties -- from the
/// adapter opening to its closing; nothing is asked of the bus before.
///
/// GUI thread.
class Bluetooth final : public QObject, protected QDBusContext {
    Q_OBJECT

public:
    /// One of the service's events (MIGO_BLUETOOTH_EVENT_*).
    using Post = std::function<void(std::uint32_t event, const QJsonObject &payload)>;
    /// A characteristic's value, read or notified.
    using PostValue = std::function<void(const QString &device, const QString &service, const QString &characteristic,
                                         const QByteArray &value)>;

    /// `bus` is BlueZ's: the system's.
    Bluetooth(QDBusConnection bus, Post post, PostValue post_value, QObject *parent = nullptr);
    ~Bluetooth() override;

    void call(std::uint32_t method, const QJsonObject &request, MigoQtHostServiceReply reply);

    /// A UUID content named, in any of its forms (16-, 32- or 128-bit), in full
    /// and upper case, as the common platform shows them; empty when it is none.
    [[nodiscard]] static QString fullUuid(const QString &named);

private Q_SLOTS:
    void interfacesAdded(const QDBusObjectPath &path, const migo::linux_host::qt6::BluezInterfaces &interfaces);
    void interfacesRemoved(const QDBusObjectPath &path, const QStringList &interfaces);
    void propertiesChanged(const QString &interface, const QVariantMap &changed, const QStringList &invalidated);

private:
    struct Connection {
        QString device;
        /// The connect call, until the device's services are resolved.
        std::unique_ptr<MigoQtHostServiceReply> connecting;
        QTimer *timeout = nullptr;
        bool ready = false;
    };

    void open(const QJsonObject &request, MigoQtHostServiceReply reply);
    void answerOpen(MigoQtHostServiceReply &reply);
    void close();
    void startDiscovery(const QJsonObject &request, MigoQtHostServiceReply reply);
    void stopDiscovery();
    void connectedDevices(const QJsonObject &request, MigoQtHostServiceReply reply);
    void connect(const QJsonObject &request, MigoQtHostServiceReply reply);
    void disconnect(const QJsonObject &request, MigoQtHostServiceReply reply);
    void services(const QJsonObject &request, MigoQtHostServiceReply reply);
    void characteristics(const QJsonObject &request, MigoQtHostServiceReply reply);
    void read(const QJsonObject &request, MigoQtHostServiceReply reply);
    void write(const QJsonObject &request, MigoQtHostServiceReply reply);
    void notify(const QJsonObject &request, MigoQtHostServiceReply reply);
    void mtu(const QJsonObject &request, MigoQtHostServiceReply reply);

    /// The device a request names that is connected and ready, its service and
    /// characteristic: the path of each, or the call failed as why not.
    QString connection(const QJsonObject &request, MigoQtHostServiceReply &reply);
    QString service(const QString &device, const QJsonObject &request, MigoQtHostServiceReply &reply);
    QString characteristic(const QJsonObject &request, MigoQtHostServiceReply &reply);

    void ready(const QString &address);
    void connectionFailed(const QString &address, std::int32_t code);
    void deviceChanged(const QString &path, const QVariantMap &changed);
    void adapterChanged();
    void found(const QString &path);
    void sendBatch();
    void postAdapterState();

    [[nodiscard]] bool powered() const;
    [[nodiscard]] QVariantMap properties(const QString &path, const char *interface) const;
    [[nodiscard]] QString deviceAt(const QString &address) const;
    [[nodiscard]] QJsonObject describe(const QString &device) const;
    void connectSignals(bool on);

    QDBusConnection bus_;
    const Post post_;
    const PostValue post_value_;
    /// BlueZ's objects, kept as it reports them while the adapter is open.
    BluezObjects objects_;
    bool loaded_ = false;
    bool opened_ = false;
    bool discovering_ = false;
    bool allow_duplicates_ = false;
    /// The services the scan is for, in full; none is any. Checked here too:
    /// BlueZ's devices are every client's, so another's scan finds them as well.
    QSet<QString> services_;
    int interval_ms_ = 0;
    QString adapter_;
    /// Calls waiting for BlueZ's objects.
    std::vector<MigoQtHostServiceReply> opening_;
    /// Devices found or reached since the adapter opened, and those reported in
    /// this discovery.
    QSet<QString> seen_;
    QSet<QString> reported_;
    QHash<QString, QJsonObject> batch_;
    QTimer *flush_ = nullptr;
    std::map<QString, Connection> connections_;
    /// The characteristics content listens to.
    QSet<QString> notifying_;
};

}  // namespace migo::linux_host::qt6

#endif  // MIGO_LINUX_QT6_BLUETOOTH_HPP_

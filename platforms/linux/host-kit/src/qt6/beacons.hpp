#ifndef MIGO_LINUX_QT6_BEACONS_HPP_
#define MIGO_LINUX_QT6_BEACONS_HPP_

// Internal to the Host Kit, not installed: iBeacons, heard through BlueZ.

#include "bluetooth.hpp"

#include <QDBusConnection>
#include <QDBusContext>
#include <QHash>
#include <QJsonArray>
#include <QJsonObject>
#include <QObject>
#include <QSet>
#include <QString>

#include <chrono>
#include <cstdint>
#include <functional>
#include <optional>

class QTimer;

namespace migo::linux_host::qt6 {

/// iBeacons, heard in BLE advertisements through BlueZ: the Apple company
/// segment of type 0x02, length 0x15 -- the proximity UUID, major, minor and the
/// transmit power at a metre -- for the UUIDs content named.
///
/// It scans on a D-Bus connection of its own: BlueZ keeps one discovery session
/// and filter per client, so a beacon scan -- every advertisement, every
/// service -- and content's device scan neither narrow nor stop each other.
///
/// As CoreLocation ranges them on Apple, the beacons in range are reported
/// every second, those not heard for ten seconds dropped; the distance is
/// estimated from the RSSI and the transmit power as the Android SDK does. A
/// second start ranges the new UUIDs instead, and stopping or reading the
/// beacons always answers, as on both.
///
/// GUI thread.
class Beacons final : public QObject, protected QDBusContext {
    Q_OBJECT

public:
    /// `bus` is a connection to BlueZ's bus of this object's own.
    Beacons(QDBusConnection bus, Bluetooth::Post post, QObject *parent = nullptr);
    ~Beacons() override;

    void call(std::uint32_t method, const QJsonObject &request, MigoQtHostServiceReply reply);

    /// One beacon as an advertisement's Apple segment describes it -- `segment`
    /// being the data after the company identifier -- or none when it is not an
    /// iBeacon's.
    struct Sighting {
        QString uuid;
        int major = 0;
        int minor = 0;
        int power = 0;
    };
    [[nodiscard]] static std::optional<Sighting> parse(const QByteArray &segment);
    /// The distance in metres the RSSI suggests at `power`, the RSSI at a metre;
    /// -1 when it cannot be told.
    [[nodiscard]] static double accuracy(int rssi, int power);
    /// 0 unknown, 1 immediate, 2 near, 3 far.
    [[nodiscard]] static int proximity(double accuracy);

private Q_SLOTS:
    void interfacesAdded(const QDBusObjectPath &path, const migo::linux_host::qt6::BluezInterfaces &interfaces);
    void propertiesChanged(const QString &interface, const QVariantMap &changed, const QStringList &invalidated);

private:
    void start(QSet<QString> uuids, MigoQtHostServiceReply reply);
    void stop();
    void heard(const QString &device, const QVariantMap &properties);
    void report();
    [[nodiscard]] QJsonArray records() const;
    void connectSignals(bool on);

    QDBusConnection bus_;
    const Bluetooth::Post post_;
    QString adapter_;
    QSet<QString> uuids_;
    bool ranging_ = false;
    /// Each device's Apple segment, as last advertised: an RSSI may come alone.
    QHash<QString, QByteArray> segments_;
    struct Seen {
        Sighting sighting;
        int rssi = 0;
        std::chrono::steady_clock::time_point at;
    };
    /// The beacons heard, by uuid/major/minor.
    QHash<QString, Seen> seen_;
    QTimer *tick_ = nullptr;
    std::uint64_t run_ = 0;
};

}  // namespace migo::linux_host::qt6

#endif  // MIGO_LINUX_QT6_BEACONS_HPP_

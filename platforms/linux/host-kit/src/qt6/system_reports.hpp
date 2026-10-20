#ifndef MIGO_LINUX_QT6_SYSTEM_REPORTS_HPP_
#define MIGO_LINUX_QT6_SYSTEM_REPORTS_HPP_

// Internal to the Host Kit, not installed: what the system says about the
// device, which the Host Kit reports to the Session.

#include <migo/session.h>

#include <QDBusConnection>
#include <QDBusServiceWatcher>
#include <QObject>
#include <QString>
#include <QTimer>
#include <QVariantMap>

#include <cstdint>
#include <functional>
#include <optional>

namespace migo::linux_host::qt6 {

/// One D-Bus object's properties of one interface, read whole, and again each
/// time the object says they changed or its service comes back.
class WatchedProperties final : public QObject {
public:
    /// `changed` is called with the properties each time they are read; none
    /// when the object is not there to read.
    WatchedProperties(QDBusConnection bus, QString service, QString path, QString interface,
                      std::function<void(const std::optional<QVariantMap> &)> changed, QObject *parent = nullptr);

private:
    void read();

    QDBusConnection bus_;
    const QString service_;
    const QString path_;
    const QString interface_;
    std::function<void(const std::optional<QVariantMap> &)> changed_;
    /// Changes coalesce: however many the object signals at once, one read follows.
    QTimer reread_;
    QDBusServiceWatcher service_watcher_;
};

/// The battery as UPower shows it -- the display device, every battery the
/// machine has as one -- with the power profile's saver as the low-power mode.
/// A machine with no battery has none to report.
class BatteryReport final : public QObject {
public:
    struct State {
        std::uint32_t level_percent = 0;
        MigoBatteryFlags flags = MIGO_BATTERY_FLAG_NONE;
        bool operator==(const State &other) const noexcept {
            return level_percent == other.level_percent && flags == other.flags;
        }
    };

    /// `bus` is where UPower and the power profiles answer: the system's.
    /// `report` is called with the battery each time it changes.
    BatteryReport(QDBusConnection bus, std::function<void(const State &)> report, QObject *parent = nullptr);

private:
    void update();

    std::function<void(const State &)> report_;
    std::optional<QVariantMap> battery_;
    std::optional<QVariantMap> profiles_;
    std::optional<QVariantMap> legacy_profiles_;
    std::optional<State> reported_;
    WatchedProperties display_device_;
    WatchedProperties power_profiles_;
    /// power-profiles-daemon's name before it joined UPower's.
    WatchedProperties legacy_power_profiles_;
};

/// The network as NetworkManager shows it: the primary connection's kind, and
/// whether it reaches the internet. A machine NetworkManager does not run on
/// has none to report.
class NetworkReport final : public QObject {
public:
    struct State {
        MigoNetworkType type = MIGO_NETWORK_NONE;
        bool connected = false;
        bool operator==(const State &other) const noexcept {
            return type == other.type && connected == other.connected;
        }
    };

    /// `bus` is where NetworkManager answers: the system's. `report` is called
    /// with the network each time it changes.
    NetworkReport(QDBusConnection bus, std::function<void(const State &)> report, QObject *parent = nullptr);

private:
    void update(const std::optional<QVariantMap> &manager);

    std::function<void(const State &)> report_;
    std::optional<State> reported_;
    WatchedProperties manager_;
};

}  // namespace migo::linux_host::qt6

#endif  // MIGO_LINUX_QT6_SYSTEM_REPORTS_HPP_

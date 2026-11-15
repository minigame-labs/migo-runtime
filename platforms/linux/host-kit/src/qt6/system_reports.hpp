#ifndef MIGO_LINUX_QT6_SYSTEM_REPORTS_HPP_
#define MIGO_LINUX_QT6_SYSTEM_REPORTS_HPP_

// Internal to the Host Kit, not installed: what the system says about the
// device, which the Host Kit reports to the Session.

#include <migo/session.h>

#include <QDBusConnection>
#include <QDBusServiceWatcher>
#include <QHash>
#include <QJsonObject>
#include <QObject>
#include <QSocketNotifier>
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

/// The radios' switches as the kernel's rfkill device tells them: every radio
/// as it is added, and again each time a switch moves. Wi-Fi is on when a
/// wireless radio is blocked by neither its soft switch nor its hard one.
class RadioSwitches final : public QObject {
public:
    /// `device` is the rfkill device (`/dev/rfkill`); `changed` is called with
    /// whether Wi-Fi is on, once the radios the kernel has are known and each
    /// time that changes. A device that cannot be opened says nothing: then
    /// nothing is known of the switch, which is not the same as its being off.
    RadioSwitches(const QString &device, std::function<void(bool wifi_on)> changed, QObject *parent = nullptr);
    ~RadioSwitches() override;

private:
    void read();

    struct Radio {
        bool wireless = false;
        bool blocked = false;
    };
    int fd_ = -1;
    std::function<void(bool)> changed_;
    QHash<std::uint32_t, Radio> radios_;
    std::optional<bool> reported_;
    QSocketNotifier *notifier_ = nullptr;
};

/// The network as the system's network manager shows it: the primary
/// connection's kind, whether it reaches the internet, and whether Wi-Fi is
/// switched on. NetworkManager where it runs; otherwise systemd-networkd, the
/// manager of machines without a desktop's, whose Wi-Fi switch is the kernel's
/// rfkill. A machine neither runs on has no network to report.
class NetworkReport final : public QObject {
public:
    struct State {
        MigoNetworkType type = MIGO_NETWORK_NONE;
        bool connected = false;
        /// Wi-Fi is on, by its switch and its radio's (getSystemSetting's
        /// wifiEnabled); none when nothing says.
        std::optional<bool> wifi_enabled;
        bool operator==(const State &other) const noexcept {
            return type == other.type && connected == other.connected && wifi_enabled == other.wifi_enabled;
        }
    };

    /// `bus` is where NetworkManager and systemd-networkd answer: the system's.
    /// `radios` is the rfkill device. `report` is called with the network each
    /// time it changes.
    NetworkReport(QDBusConnection bus, std::function<void(const State &)> report, QObject *parent = nullptr,
                  const QString &radios = QStringLiteral("/dev/rfkill"));

private:
    void update();
    void describeNetworkd();

    QDBusConnection bus_;
    std::function<void(const State &)> report_;
    std::optional<State> reported_;
    std::optional<QVariantMap> networkmanager_;
    std::optional<QVariantMap> networkd_;
    /// networkd's own account of its links (Manager.Describe), read again
    /// whenever any of its objects says something changed.
    std::optional<QJsonObject> networkd_links_;
    std::optional<bool> wifi_switch_;
    QTimer redescribe_;
    WatchedProperties networkmanager_manager_;
    WatchedProperties networkd_manager_;
    RadioSwitches radios_;
};

}  // namespace migo::linux_host::qt6

#endif  // MIGO_LINUX_QT6_SYSTEM_REPORTS_HPP_

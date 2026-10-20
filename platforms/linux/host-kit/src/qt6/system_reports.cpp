#include "system_reports.hpp"

#include <QDBusMessage>
#include <QDBusPendingCallWatcher>
#include <QDBusPendingReply>

#include <cmath>
#include <utility>

namespace migo::linux_host::qt6 {

namespace {

const QString kProperties = QStringLiteral("org.freedesktop.DBus.Properties");

// UPower's device: its Type and State as UPower numbers them.
constexpr std::uint32_t kDeviceBattery = 2;
constexpr std::uint32_t kStateCharging = 1;

// NetworkManager's numbers.
constexpr std::uint32_t kConnectivityUnknown = 0;
constexpr std::uint32_t kConnectivityFull = 4;
constexpr std::uint32_t kStateConnectedLocal = 50;
constexpr std::uint32_t kStateConnectedGlobal = 70;

/// The kind of NetworkManager's primary connection in the API's words. Wired
/// is Wi-Fi, the nearest thing the API has; a modem's generation is not
/// NetworkManager's to say; a VPN or a tunnel says nothing of what it runs over.
MigoNetworkType networkType(const QString &connection) {
    static const QStringList wireless = {QStringLiteral("802-11-wireless"), QStringLiteral("802-11-olpc-mesh")};
    static const QStringList wired = {QStringLiteral("802-3-ethernet"), QStringLiteral("bridge"),
                                      QStringLiteral("bond"),           QStringLiteral("team"),
                                      QStringLiteral("vlan"),           QStringLiteral("infiniband"),
                                      QStringLiteral("veth"),           QStringLiteral("macvlan")};
    if (connection.isEmpty()) return MIGO_NETWORK_NONE;
    if (wireless.contains(connection) || wired.contains(connection)) return MIGO_NETWORK_WIFI;
    return MIGO_NETWORK_UNKNOWN;
}

}  // namespace

WatchedProperties::WatchedProperties(QDBusConnection bus, QString service, QString path, QString interface,
                                     std::function<void(const std::optional<QVariantMap> &)> changed,
                                     QObject *parent)
    : QObject(parent),
      bus_(std::move(bus)),
      service_(std::move(service)),
      path_(std::move(path)),
      interface_(std::move(interface)),
      changed_(std::move(changed)),
      service_watcher_(service_, bus_, QDBusServiceWatcher::WatchForOwnerChange) {
    reread_.setSingleShot(true);
    reread_.setInterval(0);
    connect(&reread_, &QTimer::timeout, this, &WatchedProperties::read);
    // The changed values are not read from the signal: the next read has them all.
    bus_.connect(service_, path_, kProperties, QStringLiteral("PropertiesChanged"), &reread_, SLOT(start()));
    connect(&service_watcher_, &QDBusServiceWatcher::serviceOwnerChanged, this,
            [this](const QString &, const QString &, const QString &owner) {
                if (owner.isEmpty()) {
                    changed_(std::nullopt);
                } else {
                    reread_.start();
                }
            });
    read();
}

void WatchedProperties::read() {
    QDBusMessage call = QDBusMessage::createMethodCall(service_, path_, kProperties, QStringLiteral("GetAll"));
    call << interface_;
    auto *watcher = new QDBusPendingCallWatcher(bus_.asyncCall(call), this);
    connect(watcher, &QDBusPendingCallWatcher::finished, this, [this, watcher] {
        const QDBusPendingReply<QVariantMap> reply = *watcher;
        watcher->deleteLater();
        changed_(reply.isError() ? std::nullopt : std::optional<QVariantMap>(reply.value()));
    });
}

BatteryReport::BatteryReport(QDBusConnection bus, std::function<void(const State &)> report, QObject *parent)
    : QObject(parent),
      report_(std::move(report)),
      display_device_(bus, QStringLiteral("org.freedesktop.UPower"),
                      QStringLiteral("/org/freedesktop/UPower/devices/DisplayDevice"),
                      QStringLiteral("org.freedesktop.UPower.Device"),
                      [this](const std::optional<QVariantMap> &device) {
                          battery_ = device;
                          update();
                      }),
      power_profiles_(bus, QStringLiteral("org.freedesktop.UPower.PowerProfiles"),
                      QStringLiteral("/org/freedesktop/UPower/PowerProfiles"),
                      QStringLiteral("org.freedesktop.UPower.PowerProfiles"),
                      [this](const std::optional<QVariantMap> &profiles) {
                          profiles_ = profiles;
                          update();
                      }),
      legacy_power_profiles_(bus, QStringLiteral("net.hadess.PowerProfiles"), QStringLiteral("/net/hadess/PowerProfiles"),
                             QStringLiteral("net.hadess.PowerProfiles"),
                             [this](const std::optional<QVariantMap> &profiles) {
                                 legacy_profiles_ = profiles;
                                 update();
                             }) {}

void BatteryReport::update() {
    // No battery is no report: content is told its call is not supported, the
    // right answer for a machine with none.
    if (!battery_.has_value() || !battery_->value(QStringLiteral("IsPresent")).toBool() ||
        battery_->value(QStringLiteral("Type")).toUInt() != kDeviceBattery) {
        return;
    }
    State state;
    state.level_percent = static_cast<std::uint32_t>(
        std::lround(qBound(0.0, battery_->value(QStringLiteral("Percentage")).toDouble(), 100.0)));
    if (battery_->value(QStringLiteral("State")).toUInt() == kStateCharging) state.flags |= MIGO_BATTERY_FLAG_CHARGING;
    const std::optional<QVariantMap> &profiles = profiles_.has_value() ? profiles_ : legacy_profiles_;
    if (profiles.has_value() && profiles->value(QStringLiteral("ActiveProfile")).toString() == QLatin1String("power-saver")) {
        state.flags |= MIGO_BATTERY_FLAG_LOW_POWER_MODE;
    }
    if (reported_ == state) return;
    reported_ = state;
    report_(state);
}

NetworkReport::NetworkReport(QDBusConnection bus, std::function<void(const State &)> report, QObject *parent)
    : QObject(parent),
      report_(std::move(report)),
      manager_(bus, QStringLiteral("org.freedesktop.NetworkManager"), QStringLiteral("/org/freedesktop/NetworkManager"),
               QStringLiteral("org.freedesktop.NetworkManager"),
               [this](const std::optional<QVariantMap> &manager) { update(manager); }) {}

void NetworkReport::update(const std::optional<QVariantMap> &manager) {
    // Without NetworkManager there is no report: content is told its call is not supported.
    if (!manager.has_value()) return;
    const std::uint32_t state = manager->value(QStringLiteral("State")).toUInt();
    const std::uint32_t connectivity = manager->value(QStringLiteral("Connectivity")).toUInt();
    State network;
    network.type = state >= kStateConnectedLocal
                       ? networkType(manager->value(QStringLiteral("PrimaryConnectionType")).toString())
                       : MIGO_NETWORK_NONE;
    // NetworkManager says the connection reaches the internet where it checks;
    // where checking is off it says it does not know, and its state says.
    network.connected = network.type != MIGO_NETWORK_NONE &&
                        (connectivity == kConnectivityFull ||
                         (connectivity == kConnectivityUnknown && state == kStateConnectedGlobal));
    if (reported_ == network) return;
    reported_ = network;
    report_(network);
}

}  // namespace migo::linux_host::qt6

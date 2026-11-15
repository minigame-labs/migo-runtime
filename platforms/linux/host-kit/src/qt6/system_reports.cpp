#include "system_reports.hpp"

#include <QDBusMessage>
#include <QDBusPendingCallWatcher>
#include <QDBusPendingReply>
#include <QJsonArray>
#include <QJsonDocument>

#include <fcntl.h>
#include <linux/rfkill.h>
#include <unistd.h>

#include <cerrno>
#include <cmath>
#include <limits>
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

// systemd-networkd.
const QString kNetworkd = QStringLiteral("org.freedesktop.network1");
const QString kNetworkdManagerPath = QStringLiteral("/org/freedesktop/network1");
const QString kNetworkdManager = QStringLiteral("org.freedesktop.network1.Manager");
constexpr int kMainRoutingTable = 254;

/// The kind of the link networkd routes the internet through, by networkd's
/// link Type, in the API's words as networkType says them: Wi-Fi, and wired as
/// Wi-Fi; a modem, a point-to-point link or a tunnel unknown.
MigoNetworkType networkdLinkType(const QString &type) {
    static const QStringList wired = {QStringLiteral("ether"), QStringLiteral("bridge"), QStringLiteral("bond"),
                                      QStringLiteral("vlan"),  QStringLiteral("team"),   QStringLiteral("infiniband")};
    if (type == QLatin1String("wlan") || wired.contains(type)) return MIGO_NETWORK_WIFI;
    return MIGO_NETWORK_UNKNOWN;
}

/// The Type of the link that carries networkd's preferred default route -- the
/// main table's route to everywhere with the lowest metric, the one the kernel
/// takes -- or none when no link has one.
std::optional<QString> networkdPrimaryLinkType(const QJsonObject &described) {
    std::optional<QString> type;
    double best = std::numeric_limits<double>::infinity();
    for (const QJsonValue &link : described.value(QStringLiteral("Interfaces")).toArray()) {
        const QJsonObject interface = link.toObject();
        for (const QJsonValue &value : interface.value(QStringLiteral("Routes")).toArray()) {
            const QJsonObject route = value.toObject();
            if (route.value(QStringLiteral("DestinationPrefixLength")).toInt(-1) != 0 ||
                route.value(QStringLiteral("Table")).toInt(kMainRoutingTable) != kMainRoutingTable) {
                continue;
            }
            const double metric = route.value(QStringLiteral("Priority")).toDouble(0);
            if (metric < best) {
                best = metric;
                type = interface.value(QStringLiteral("Type")).toString();
            }
        }
    }
    return type;
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

RadioSwitches::RadioSwitches(const QString &device, std::function<void(bool)> changed, QObject *parent)
    : QObject(parent), changed_(std::move(changed)) {
    fd_ = ::open(device.toLocal8Bit().constData(), O_RDONLY | O_NONBLOCK | O_CLOEXEC);
    if (fd_ < 0) return;
    notifier_ = new QSocketNotifier(fd_, QSocketNotifier::Read, this);
    connect(notifier_, &QSocketNotifier::activated, this, &RadioSwitches::read);
    // The kernel queues an ADD for every radio it has the moment the device
    // opens, so the first read knows them all.
    read();
}

RadioSwitches::~RadioSwitches() {
    if (fd_ >= 0) ::close(fd_);
}

void RadioSwitches::read() {
    // One event per read, in the original event's size: a kernel that knows a
    // longer one gives the shorter to a reader that asks for it.
    rfkill_event event{};
    ssize_t got = 0;
    while ((got = ::read(fd_, &event, sizeof(event))) == static_cast<ssize_t>(sizeof(event))) {
        if (event.op == RFKILL_OP_DEL) {
            radios_.remove(event.idx);
        } else if (event.op == RFKILL_OP_ADD || event.op == RFKILL_OP_CHANGE) {
            radios_.insert(event.idx, Radio{event.type == RFKILL_TYPE_WLAN, event.soft != 0 || event.hard != 0});
        }
    }
    if (got == 0 || (got < 0 && errno != EAGAIN && errno != EINTR)) {
        // The device went away: from here on nothing is known of the switch.
        notifier_->setEnabled(false);
        return;
    }
    bool wifi_on = false;
    for (const Radio &radio : std::as_const(radios_)) wifi_on = wifi_on || (radio.wireless && !radio.blocked);
    if (reported_ == wifi_on) return;
    reported_ = wifi_on;
    changed_(wifi_on);
}

NetworkReport::NetworkReport(QDBusConnection bus, std::function<void(const State &)> report, QObject *parent,
                             const QString &radios)
    : QObject(parent),
      bus_(bus),
      report_(std::move(report)),
      networkmanager_manager_(bus, QStringLiteral("org.freedesktop.NetworkManager"),
                              QStringLiteral("/org/freedesktop/NetworkManager"),
                              QStringLiteral("org.freedesktop.NetworkManager"),
                              [this](const std::optional<QVariantMap> &manager) {
                                  networkmanager_ = manager;
                                  update();
                              }),
      networkd_manager_(bus, kNetworkd, kNetworkdManagerPath, kNetworkdManager,
                        [this](const std::optional<QVariantMap> &manager) {
                            networkd_ = manager;
                            if (!manager.has_value()) networkd_links_.reset();
                            // Its links are read again with it: what routes where is theirs.
                            manager.has_value() ? redescribe_.start() : update();
                        }),
      radios_(radios, [this](bool wifi_on) {
          wifi_switch_ = wifi_on;
          update();
      }) {
    redescribe_.setSingleShot(true);
    redescribe_.setInterval(0);
    connect(&redescribe_, &QTimer::timeout, this, &NetworkReport::describeNetworkd);
    // A link's state and its routes change on the link's own object, which the
    // Manager's properties do not show: any of networkd's objects changing is a
    // reason to read the links again.
    bus_.connect(kNetworkd, QString(), QStringLiteral("org.freedesktop.DBus.Properties"),
                 QStringLiteral("PropertiesChanged"), &redescribe_, SLOT(start()));
}

void NetworkReport::describeNetworkd() {
    if (!networkd_.has_value()) return;
    const QDBusMessage call =
        QDBusMessage::createMethodCall(kNetworkd, kNetworkdManagerPath, kNetworkdManager, QStringLiteral("Describe"));
    auto *watcher = new QDBusPendingCallWatcher(bus_.asyncCall(call), this);
    connect(watcher, &QDBusPendingCallWatcher::finished, this, [this, watcher] {
        const QDBusPendingReply<QString> reply = *watcher;
        watcher->deleteLater();
        // Describe arrived in systemd 248; before it, what the links are is not
        // networkd's to say over the bus, and the state alone is reported.
        const QJsonDocument described =
            reply.isError() ? QJsonDocument() : QJsonDocument::fromJson(reply.value().toUtf8());
        networkd_links_ = described.isObject() ? std::optional<QJsonObject>(described.object()) : std::nullopt;
        update();
    });
}

void NetworkReport::update() {
    State network;
    if (networkmanager_.has_value()) {
        const QVariantMap &manager = *networkmanager_;
        const std::uint32_t state = manager.value(QStringLiteral("State")).toUInt();
        const std::uint32_t connectivity = manager.value(QStringLiteral("Connectivity")).toUInt();
        network.type = state >= kStateConnectedLocal
                           ? networkType(manager.value(QStringLiteral("PrimaryConnectionType")).toString())
                           : MIGO_NETWORK_NONE;
        // NetworkManager says the connection reaches the internet where it checks;
        // where checking is off it says it does not know, and its state says.
        network.connected = network.type != MIGO_NETWORK_NONE &&
                            (connectivity == kConnectivityFull ||
                             (connectivity == kConnectivityUnknown && state == kStateConnectedGlobal));
        network.wifi_enabled = manager.value(QStringLiteral("WirelessEnabled")).toBool() &&
                               manager.value(QStringLiteral("WirelessHardwareEnabled")).toBool();
    } else if (networkd_.has_value()) {
        // networkd routes the internet through the link with the preferred
        // default route, and says the machine is routable once a link has an
        // address the internet can answer. It checks no further: the state is
        // the answer, as NetworkManager's is where its check is off.
        const bool routable = networkd_->value(QStringLiteral("OperationalState")).toString() == QLatin1String("routable");
        if (networkd_links_.has_value()) {
            const std::optional<QString> primary = networkdPrimaryLinkType(*networkd_links_);
            network.type = primary.has_value() ? networkdLinkType(*primary) : MIGO_NETWORK_NONE;
        } else {
            network.type = routable ? MIGO_NETWORK_UNKNOWN : MIGO_NETWORK_NONE;
        }
        network.connected = network.type != MIGO_NETWORK_NONE && routable;
        network.wifi_enabled = wifi_switch_;
    } else {
        // Without either there is no report: content is told its call is not supported.
        return;
    }
    if (reported_ == network) return;
    reported_ = network;
    report_(network);
}

}  // namespace migo::linux_host::qt6

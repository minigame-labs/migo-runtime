#include "beacons.hpp"

#include <QDBusArgument>
#include <QDBusError>
#include <QDBusMessage>
#include <QDBusMetaType>
#include <QDBusPendingCallWatcher>
#include <QDBusPendingReply>
#include <QDBusVariant>
#include <QStringList>
#include <QTimer>

#include <algorithm>
#include <cmath>
#include <utility>

namespace migo::linux_host::qt6 {

namespace {

const QString kBluez = QStringLiteral("org.bluez");
const QString kAdapter = QStringLiteral("org.bluez.Adapter1");
const QString kDevice = QStringLiteral("org.bluez.Device1");
const QString kObjectManager = QStringLiteral("org.freedesktop.DBus.ObjectManager");
const QString kProperties = QStringLiteral("org.freedesktop.DBus.Properties");

/// Apple's company identifier, whose segment an iBeacon's is.
constexpr quint16 kApple = 0x004c;
/// How long a beacon not heard stays in range.
constexpr std::chrono::seconds kInRange{10};
/// How often the beacons in range are reported: CoreLocation's ranging.
constexpr int kReportMs = 1000;
/// The beacons kept at most -- what is around is anyone's to broadcast: the
/// Android SDK's bound.
constexpr int kLimit = 256;

/// The common platform's codes for a beacon call that failed.
void fail(MigoQtHostServiceReply &reply, std::int32_t code) {
    switch (code) {
        case 11001:
            (void)reply.fail(QStringLiteral("bluetooth service is unavailable"), code);
            return;
        case 11006:
            (void)reply.fail(QStringLiteral("invalid data"), code);
            return;
        default:
            (void)reply.fail(QStringLiteral("system error"), 11005);
            return;
    }
}

/// The Apple segment of a device's manufacturer data, after the company
/// identifier; empty when it advertises none.
QByteArray appleSegment(const QVariant &value) {
    QByteArray segment;
    if (value.userType() != qMetaTypeId<QDBusArgument>()) return segment;
    const QDBusArgument data = value.value<QDBusArgument>();
    data.beginMap();
    while (!data.atEnd()) {
        quint16 company = 0;
        QDBusVariant bytes;
        data.beginMapEntry();
        data >> company >> bytes;
        data.endMapEntry();
        if (company == kApple) segment = bytes.variant().toByteArray();
    }
    data.endMap();
    return segment;
}

QDBusMessage method(const QString &path, const QString &interface, const QString &name) {
    return QDBusMessage::createMethodCall(kBluez, path, interface, name);
}

}  // namespace

Beacons::Beacons(QDBusConnection bus, Bluetooth::Post post, QObject *parent)
    : QObject(parent), bus_(std::move(bus)), post_(std::move(post)) {
    qDBusRegisterMetaType<BluezInterfaces>();
    qDBusRegisterMetaType<BluezObjects>();
    tick_ = new QTimer(this);
    QObject::connect(tick_, &QTimer::timeout, this, [this] { report(); });
}

Beacons::~Beacons() {
    stop();
    QDBusConnection::disconnectFromBus(bus_.name());
}

std::optional<Beacons::Sighting> Beacons::parse(const QByteArray &segment) {
    if (segment.size() < 23 || static_cast<quint8>(segment.at(0)) != 0x02 || static_cast<quint8>(segment.at(1)) != 0x15) {
        return std::nullopt;
    }
    const auto byte = [&segment](int at) { return static_cast<quint8>(segment.at(at)); };
    Sighting sighting;
    sighting.uuid = Bluetooth::fullUuid(QString::fromLatin1(segment.mid(2, 16).toHex()));
    sighting.major = byte(18) << 8 | byte(19);
    sighting.minor = byte(20) << 8 | byte(21);
    sighting.power = static_cast<qint8>(segment.at(22));
    return sighting;
}

double Beacons::accuracy(int rssi, int power) {
    if (rssi == 0 || power == 0) return -1;
    const double ratio = static_cast<double>(rssi) / power;
    if (ratio < 1) return std::pow(ratio, 10);
    return 0.89976 * std::pow(ratio, 7.7095) + 0.111;
}

int Beacons::proximity(double accuracy) {
    if (accuracy < 0) return 0;
    if (accuracy < 0.5) return 1;
    return accuracy < 4 ? 2 : 3;
}

void Beacons::call(std::uint32_t method, const QJsonObject &request, MigoQtHostServiceReply reply) {
    switch (method) {
        case MIGO_BLUETOOTH_START_BEACON_DISCOVERY: {
            // Proximity UUIDs, in full: a beacon's is 128 bits.
            const QJsonArray named = request.value(QStringLiteral("uuids")).toArray();
            QSet<QString> uuids;
            for (const QJsonValue &value : named) {
                QString hex = value.toString();
                hex.remove(QLatin1Char('-'));
                const QString uuid = Bluetooth::fullUuid(value.toString());
                if (hex.size() != 32 || uuid.isEmpty()) return fail(reply, 11006);
                uuids.insert(uuid);
            }
            if (uuids.isEmpty()) return fail(reply, 11006);
            start(std::move(uuids), std::move(reply));
            return;
        }
        case MIGO_BLUETOOTH_STOP_BEACON_DISCOVERY:
            stop();
            (void)reply.succeed();
            return;
        case MIGO_BLUETOOTH_GET_BEACONS:
            (void)reply.succeed({{QStringLiteral("beacons"), records()}});
            return;
        default:
            (void)reply.fail(QStringLiteral("not supported"));
            return;
    }
}

void Beacons::start(QSet<QString> uuids, MigoQtHostServiceReply reply) {
    uuids_ = std::move(uuids);
    if (ranging_) {
        // Ranging the new UUIDs instead.
        seen_.clear();
        (void)reply.succeed();
        return;
    }
    const std::uint64_t run = ++run_;
    // Listened to before asking, so no advertisement falls between the two.
    connectSignals(true);
    auto *loading = new QDBusPendingCallWatcher(
        bus_.asyncCall(method(QStringLiteral("/"), kObjectManager, QStringLiteral("GetManagedObjects"))), this);
    QObject::connect(loading, &QDBusPendingCallWatcher::finished, this, [this, run, reply](QDBusPendingCallWatcher *call) mutable {
        call->deleteLater();
        // Stopped before it had started: started and stopped.
        if (run != run_) {
            (void)reply.succeed();
            return;
        }
        const QDBusPendingReply<BluezObjects> objects = *call;
        adapter_.clear();
        if (!objects.isError()) {
            const BluezObjects held = objects.value();
            for (auto object = held.cbegin(); object != held.cend(); ++object) {
                if (object.value().value(kAdapter).value(QStringLiteral("Powered")).toBool() && adapter_.isEmpty()) {
                    adapter_ = object.key().path();
                }
            }
            for (auto object = held.cbegin(); object != held.cend(); ++object) {
                const QByteArray segment = appleSegment(object.value().value(kDevice).value(QStringLiteral("ManufacturerData")));
                if (!segment.isEmpty()) segments_.insert(object.key().path(), segment);
            }
        }
        if (adapter_.isEmpty()) {
            connectSignals(false);
            segments_.clear();
            uuids_.clear();
            return fail(reply, 11001);
        }
        // Every advertisement, every device: the beacons are told apart here.
        QDBusMessage filter = method(adapter_, kAdapter, QStringLiteral("SetDiscoveryFilter"));
        filter << QVariantMap{{QStringLiteral("Transport"), QStringLiteral("le")}, {QStringLiteral("DuplicateData"), true}};
        bus_.asyncCall(filter);
        auto *starting = new QDBusPendingCallWatcher(bus_.asyncCall(method(adapter_, kAdapter, QStringLiteral("StartDiscovery"))), this);
        QObject::connect(starting, &QDBusPendingCallWatcher::finished, this, [this, run, reply](QDBusPendingCallWatcher *started) mutable {
            started->deleteLater();
            if (run != run_) {
                (void)reply.succeed();
                return;
            }
            const QDBusPendingReply<> scanning = *started;
            if (scanning.isError() && scanning.error().name() != QLatin1String("org.bluez.Error.InProgress")) {
                connectSignals(false);
                segments_.clear();
                seen_.clear();
                uuids_.clear();
                return fail(reply, 11005);
            }
            ranging_ = true;
            tick_->start(kReportMs);
            post_(MIGO_BLUETOOTH_EVENT_BEACON_SERVICE_CHANGE,
                  {{QStringLiteral("available"), true}, {QStringLiteral("discovering"), true}});
            (void)reply.succeed();
        });
    });
}

void Beacons::stop() {
    ++run_;
    uuids_.clear();
    if (!ranging_) {
        connectSignals(false);
        segments_.clear();
        seen_.clear();
        return;
    }
    ranging_ = false;
    tick_->stop();
    bus_.asyncCall(method(adapter_, kAdapter, QStringLiteral("StopDiscovery")));
    connectSignals(false);
    seen_.clear();
    segments_.clear();
    post_(MIGO_BLUETOOTH_EVENT_BEACON_SERVICE_CHANGE,
          {{QStringLiteral("available"), true}, {QStringLiteral("discovering"), false}});
}

void Beacons::connectSignals(bool on) {
    const auto signal = [this, on](const QString &path, const QString &interface, const QString &name, const char *slot) {
        if (on) {
            bus_.connect(kBluez, path, interface, name, this, slot);
        } else {
            bus_.disconnect(kBluez, path, interface, name, this, slot);
        }
    };
    signal(QStringLiteral("/"), kObjectManager, QStringLiteral("InterfacesAdded"),
           SLOT(interfacesAdded(QDBusObjectPath,migo::linux_host::qt6::BluezInterfaces)));
    signal(QString(), kProperties, QStringLiteral("PropertiesChanged"),
           SLOT(propertiesChanged(QString,QVariantMap,QStringList)));
}

void Beacons::interfacesAdded(const QDBusObjectPath &path, const BluezInterfaces &interfaces) {
    if (interfaces.contains(kDevice)) heard(path.path(), interfaces.value(kDevice));
}

void Beacons::propertiesChanged(const QString &interface, const QVariantMap &changed, const QStringList &) {
    if (interface == kDevice) heard(message().path(), changed);
}

void Beacons::heard(const QString &device, const QVariantMap &properties) {
    if (adapter_.isEmpty() || !device.startsWith(adapter_ + QLatin1Char('/'))) return;
    if (properties.contains(QStringLiteral("ManufacturerData"))) {
        const QByteArray segment = appleSegment(properties.value(QStringLiteral("ManufacturerData")));
        if (segment.isEmpty()) {
            segments_.remove(device);
        } else {
            segments_.insert(device, segment);
        }
    }
    // A sighting is an advertisement heard, which carries its RSSI -- from the
    // moment the scan is asked for, before BlueZ has answered that it runs.
    if (uuids_.isEmpty() || !properties.contains(QStringLiteral("RSSI"))) return;
    const std::optional<Sighting> sighting = parse(segments_.value(device));
    if (!sighting.has_value() || !uuids_.contains(sighting->uuid)) return;
    const QString key = QStringLiteral("%1/%2/%3").arg(sighting->uuid).arg(sighting->major).arg(sighting->minor);
    if (!seen_.contains(key) && seen_.size() >= kLimit) return;
    seen_.insert(key, Seen{*sighting, properties.value(QStringLiteral("RSSI")).toInt(), std::chrono::steady_clock::now()});
}

void Beacons::report() {
    const auto now = std::chrono::steady_clock::now();
    for (auto entry = seen_.begin(); entry != seen_.end();) {
        entry = now - entry->at > kInRange ? seen_.erase(entry) : std::next(entry);
    }
    post_(MIGO_BLUETOOTH_EVENT_BEACON_UPDATE, {{QStringLiteral("beacons"), records()}});
}

QJsonArray Beacons::records() const {
    QStringList keys = seen_.keys();
    keys.sort();
    QJsonArray beacons;
    for (const QString &key : std::as_const(keys)) {
        const Seen &seen = seen_.value(key);
        const double metres = accuracy(seen.rssi, seen.sighting.power);
        beacons.append(QJsonObject{{QStringLiteral("uuid"), seen.sighting.uuid},
                                   {QStringLiteral("major"), seen.sighting.major},
                                   {QStringLiteral("minor"), seen.sighting.minor},
                                   {QStringLiteral("proximity"), proximity(metres)},
                                   {QStringLiteral("accuracy"), metres},
                                   {QStringLiteral("rssi"), seen.rssi}});
    }
    return beacons;
}

}  // namespace migo::linux_host::qt6

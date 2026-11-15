#include "bluetooth.hpp"

#include <QDBusArgument>
#include <QDBusError>
#include <QDBusMessage>
#include <QDBusMetaType>
#include <QDBusPendingCallWatcher>
#include <QDBusPendingReply>
#include <QDBusVariant>
#include <QJsonArray>
#include <QRegularExpression>
#include <QTimer>

#include <utility>

namespace migo::linux_host::qt6 {

namespace {

const QString kBluez = QStringLiteral("org.bluez");
const QString kAdapter = QStringLiteral("org.bluez.Adapter1");
const QString kDevice = QStringLiteral("org.bluez.Device1");
const QString kService = QStringLiteral("org.bluez.GattService1");
const QString kCharacteristic = QStringLiteral("org.bluez.GattCharacteristic1");
const QString kObjectManager = QStringLiteral("org.freedesktop.DBus.ObjectManager");
const QString kProperties = QStringLiteral("org.freedesktop.DBus.Properties");

/// The common platform's codes for a Bluetooth call that failed, and their
/// words: the Apple SDK's MigoBluetoothError.
enum Code : std::int32_t {
    kNotOpened = 10000,
    kUnavailable = 10001,
    kNoDevice = 10002,
    kConnectionFailed = 10003,
    kNoService = 10004,
    kNoCharacteristic = 10005,
    kNotConnected = 10006,
    kNotSupported = 10007,
    kSystemError = 10008,
    kTimedOut = 10012,
    kInvalidData = 10013,
    kAlreadyConnected = -1,
};

QString wordsOf(std::int32_t code) {
    switch (code) {
        case kNotOpened: return QStringLiteral("not init");
        case kUnavailable: return QStringLiteral("not available");
        case kNoDevice: return QStringLiteral("no device");
        case kConnectionFailed: return QStringLiteral("connection fail");
        case kNoService: return QStringLiteral("no service");
        case kNoCharacteristic: return QStringLiteral("no characteristic");
        case kNotConnected: return QStringLiteral("no connection");
        case kNotSupported: return QStringLiteral("property not support");
        case kTimedOut: return QStringLiteral("operate time out");
        case kInvalidData: return QStringLiteral("invalid data");
        case kAlreadyConnected: return QStringLiteral("already connect");
        default: return QStringLiteral("system error");
    }
}

void fail(MigoQtHostServiceReply &reply, std::int32_t code) { (void)reply.fail(wordsOf(code), code); }

/// A BlueZ error as the common platform's code.
std::int32_t codeOf(const QDBusError &error) {
    const QString name = error.name();
    if (name == QLatin1String("org.bluez.Error.NotConnected")) return kNotConnected;
    if (name == QLatin1String("org.bluez.Error.NotPermitted") || name == QLatin1String("org.bluez.Error.NotSupported")) {
        return kNotSupported;
    }
    if (name == QLatin1String("org.bluez.Error.InvalidValueLength") ||
        name == QLatin1String("org.bluez.Error.InvalidArguments")) {
        return kInvalidData;
    }
    return kSystemError;
}

/// `value` as a `T`, whether QtDBus demarshalled it or left it marshalled.
template <typename T>
T demarshal(const QVariant &value) {
    if (value.userType() == qMetaTypeId<QDBusArgument>()) {
        T out;
        value.value<QDBusArgument>() >> out;
        return out;
    }
    return value.value<T>();
}

/// A device's manufacturer-specific segment as it was advertised: the company
/// identifier first, little-endian, then its data. BlueZ keys the data by the
/// identifier; a device advertises one.
QByteArray manufacturerData(const QVariant &value) {
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
        if (segment.isEmpty()) {
            segment.append(static_cast<char>(company & 0xff));
            segment.append(static_cast<char>(company >> 8));
            segment.append(bytes.variant().toByteArray());
        }
    }
    data.endMap();
    return segment;
}

/// The bytes a hex string spells, or none.
std::optional<QByteArray> bytesOf(const QString &hex) {
    static const QRegularExpression pairs(QStringLiteral("^(?:[0-9A-Fa-f]{2})*$"));
    if (!pairs.match(hex).hasMatch()) return std::nullopt;
    return QByteArray::fromHex(hex.toLatin1());
}

QString hexOf(const QByteArray &bytes) { return QString::fromLatin1(bytes.toHex()); }

QDBusMessage method(const QString &path, const QString &interface, const QString &name) {
    return QDBusMessage::createMethodCall(kBluez, path, interface, name);
}

}  // namespace

QString Bluetooth::fullUuid(const QString &named) {
    QString hex = named.trimmed();
    hex.remove(QLatin1Char('-'));
    static const QRegularExpression digits(QStringLiteral("^[0-9A-Fa-f]+$"));
    if (!digits.match(hex).hasMatch()) return {};
    hex = hex.toUpper();
    const QString base = QStringLiteral("-0000-1000-8000-00805F9B34FB");
    switch (hex.size()) {
        case 4:
            return QStringLiteral("0000") + hex + base;
        case 8:
            return hex + base;
        case 32:
            return QStringLiteral("%1-%2-%3-%4-%5")
                .arg(hex.mid(0, 8), hex.mid(8, 4), hex.mid(12, 4), hex.mid(16, 4), hex.mid(20));
        default:
            return {};
    }
}

Bluetooth::Bluetooth(QDBusConnection bus, Post post, PostValue post_value, QObject *parent)
    : QObject(parent), bus_(std::move(bus)), post_(std::move(post)), post_value_(std::move(post_value)) {
    qDBusRegisterMetaType<BluezInterfaces>();
    qDBusRegisterMetaType<BluezObjects>();
    flush_ = new QTimer(this);
    flush_->setSingleShot(true);
    QObject::connect(flush_, &QTimer::timeout, this, [this] { sendBatch(); });
}

Bluetooth::~Bluetooth() { close(); }

void Bluetooth::call(std::uint32_t method, const QJsonObject &request, MigoQtHostServiceReply reply) {
    switch (method) {
        case MIGO_BLUETOOTH_OPEN_ADAPTER:
            open(request, std::move(reply));
            return;
        case MIGO_BLUETOOTH_CLOSE_ADAPTER:
            close();
            (void)reply.succeed();
            return;
        case MIGO_BLUETOOTH_GET_ADAPTER_STATE:
            if (!opened_) return fail(reply, kNotOpened);
            (void)reply.succeed({{QStringLiteral("available"), powered()}, {QStringLiteral("discovering"), discovering_}});
            return;
        case MIGO_BLUETOOTH_START_DEVICES_DISCOVERY:
            startDiscovery(request, std::move(reply));
            return;
        case MIGO_BLUETOOTH_STOP_DEVICES_DISCOVERY:
            if (!opened_) return fail(reply, kNotOpened);
            stopDiscovery();
            (void)reply.succeed();
            return;
        case MIGO_BLUETOOTH_GET_DEVICES: {
            if (!opened_) return fail(reply, kNotOpened);
            QJsonArray devices;
            for (const QString &device : std::as_const(seen_)) {
                if (objects_.contains(QDBusObjectPath(device))) devices.append(describe(device));
            }
            (void)reply.succeed({{QStringLiteral("devices"), devices}});
            return;
        }
        case MIGO_BLUETOOTH_GET_CONNECTED_DEVICES:
            connectedDevices(request, std::move(reply));
            return;
        case MIGO_BLUETOOTH_CREATE_BLE_CONNECTION:
            connect(request, std::move(reply));
            return;
        case MIGO_BLUETOOTH_CLOSE_BLE_CONNECTION:
            disconnect(request, std::move(reply));
            return;
        case MIGO_BLUETOOTH_GET_BLE_DEVICE_SERVICES:
            services(request, std::move(reply));
            return;
        case MIGO_BLUETOOTH_GET_BLE_DEVICE_CHARACTERISTICS:
            characteristics(request, std::move(reply));
            return;
        case MIGO_BLUETOOTH_READ_BLE_CHARACTERISTIC_VALUE:
            read(request, std::move(reply));
            return;
        case MIGO_BLUETOOTH_WRITE_BLE_CHARACTERISTIC_VALUE:
            write(request, std::move(reply));
            return;
        case MIGO_BLUETOOTH_NOTIFY_BLE_CHARACTERISTIC_VALUE_CHANGE:
            notify(request, std::move(reply));
            return;
        case MIGO_BLUETOOTH_GET_BLE_MTU:
            mtu(request, std::move(reply));
            return;
        default:
            // A connected device's RSSI, which BlueZ gives no app; pairing,
            // asking whether a device is paired and setting the MTU, the
            // reference's for Android only.
            (void)reply.fail(QStringLiteral("not supported"));
            return;
    }
}

// ---- the adapter --------------------------------------------------------------------------------

void Bluetooth::open(const QJsonObject &request, MigoQtHostServiceReply reply) {
    if (request.value(QStringLiteral("mode")).toString() == QLatin1String("peripheral")) {
        (void)reply.fail(QStringLiteral("not supported"));
        return;
    }
    opened_ = true;
    if (loaded_) {
        answerOpen(reply);
        return;
    }
    opening_.push_back(std::move(reply));
    if (opening_.size() > 1) return;
    // Listened to before asking, so no change falls between the two.
    connectSignals(true);
    auto *loading = new QDBusPendingCallWatcher(
        bus_.asyncCall(method(QStringLiteral("/"), kObjectManager, QStringLiteral("GetManagedObjects"))), this);
    QObject::connect(loading, &QDBusPendingCallWatcher::finished, this, [this](QDBusPendingCallWatcher *call) {
        call->deleteLater();
        const QDBusPendingReply<BluezObjects> objects = *call;
        std::vector<MigoQtHostServiceReply> waiting;
        waiting.swap(opening_);
        if (!opened_) return;
        if (objects.isError()) {
            // No BlueZ: no Bluetooth to open. Asked again at the next open.
            connectSignals(false);
            for (MigoQtHostServiceReply &reply : waiting) fail(reply, kUnavailable);
            return;
        }
        objects_ = objects.value();
        loaded_ = true;
        // The powered adapter, or the first.
        for (auto object = objects_.cbegin(); object != objects_.cend(); ++object) {
            if (!object.value().contains(kAdapter)) continue;
            if (adapter_.isEmpty() || object.value().value(kAdapter).value(QStringLiteral("Powered")).toBool()) {
                adapter_ = object.key().path();
            }
        }
        for (MigoQtHostServiceReply &reply : waiting) answerOpen(reply);
    });
}

void Bluetooth::answerOpen(MigoQtHostServiceReply &reply) {
    if (powered()) {
        (void)reply.succeed();
    } else {
        fail(reply, kUnavailable);
    }
}

void Bluetooth::close() {
    if (opened_) stopDiscovery();
    for (auto &[address, connection] : connections_) {
        if (connection.timeout != nullptr) connection.timeout->deleteLater();
        if (connection.connecting != nullptr) fail(*connection.connecting, kNotConnected);
        bus_.asyncCall(method(connection.device, kDevice, QStringLiteral("Disconnect")));
    }
    connections_.clear();
    std::vector<MigoQtHostServiceReply> waiting;
    waiting.swap(opening_);
    for (MigoQtHostServiceReply &reply : waiting) fail(reply, kNotOpened);
    if (loaded_ || opened_) connectSignals(false);
    objects_.clear();
    loaded_ = false;
    opened_ = false;
    adapter_.clear();
    seen_.clear();
    reported_.clear();
    notifying_.clear();
    batch_.clear();
}

bool Bluetooth::powered() const {
    return !adapter_.isEmpty() && properties(adapter_, "org.bluez.Adapter1").value(QStringLiteral("Powered")).toBool();
}

QVariantMap Bluetooth::properties(const QString &path, const char *interface) const {
    return objects_.value(QDBusObjectPath(path)).value(QLatin1String(interface));
}

void Bluetooth::postAdapterState() {
    post_(MIGO_BLUETOOTH_EVENT_ADAPTER_STATE_CHANGE,
          {{QStringLiteral("available"), powered()}, {QStringLiteral("discovering"), discovering_}});
}

void Bluetooth::adapterChanged() {
    if (!powered()) {
        discovering_ = false;
        flush_->stop();
        batch_.clear();
        for (auto connection = connections_.begin(); connection != connections_.end();) {
            const auto next = std::next(connection);
            if (!connection->second.ready) connectionFailed(connection->first, kConnectionFailed);
            connection = next;
        }
    }
    if (opened_) postAdapterState();
}

void Bluetooth::connectSignals(bool on) {
    const auto signal = [this, on](const QString &path, const QString &interface, const QString &name, const char *slot) {
        if (on) {
            bus_.connect(kBluez, path, interface, name, this, slot);
        } else {
            bus_.disconnect(kBluez, path, interface, name, this, slot);
        }
    };
    signal(QStringLiteral("/"), kObjectManager, QStringLiteral("InterfacesAdded"),
           SLOT(interfacesAdded(QDBusObjectPath,migo::linux_host::qt6::BluezInterfaces)));
    signal(QStringLiteral("/"), kObjectManager, QStringLiteral("InterfacesRemoved"),
           SLOT(interfacesRemoved(QDBusObjectPath,QStringList)));
    // Every object's: an empty path is any.
    signal(QString(), kProperties, QStringLiteral("PropertiesChanged"),
           SLOT(propertiesChanged(QString,QVariantMap,QStringList)));
}

// ---- BlueZ's reports ----------------------------------------------------------------------------

void Bluetooth::interfacesAdded(const QDBusObjectPath &path, const BluezInterfaces &interfaces) {
    if (!loaded_) return;
    BluezInterfaces &object = objects_[path];
    for (auto added = interfaces.cbegin(); added != interfaces.cend(); ++added) object.insert(added.key(), added.value());
    if (interfaces.contains(kAdapter) && adapter_.isEmpty()) {
        adapter_ = path.path();
        adapterChanged();
    }
    if (interfaces.contains(kDevice)) deviceChanged(path.path(), interfaces.value(kDevice));
}

void Bluetooth::interfacesRemoved(const QDBusObjectPath &path, const QStringList &interfaces) {
    if (!loaded_ || !objects_.contains(path)) return;
    BluezInterfaces &object = objects_[path];
    for (const QString &interface : interfaces) object.remove(interface);
    if (interfaces.contains(kDevice)) {
        // A device BlueZ forgets is gone, connected or not.
        for (auto connection = connections_.begin(); connection != connections_.end(); ++connection) {
            if (connection->second.device != path.path()) continue;
            deviceChanged(path.path(), {{QStringLiteral("Connected"), false}});
            break;
        }
        seen_.remove(path.path());
    }
    if (object.isEmpty()) objects_.remove(path);
    if (interfaces.contains(kAdapter) && path.path() == adapter_) {
        adapter_.clear();
        adapterChanged();
    }
}

void Bluetooth::propertiesChanged(const QString &interface, const QVariantMap &changed, const QStringList &invalidated) {
    if (!loaded_) return;
    const QString path = message().path();
    const QDBusObjectPath key(path);
    if (!objects_.contains(key) || !objects_.value(key).contains(interface)) return;
    QVariantMap &properties = objects_[key][interface];
    for (auto value = changed.cbegin(); value != changed.cend(); ++value) properties.insert(value.key(), value.value());
    for (const QString &gone : invalidated) properties.remove(gone);
    if (interface == kAdapter && path == adapter_ && changed.contains(QStringLiteral("Powered"))) {
        adapterChanged();
    } else if (interface == kDevice) {
        deviceChanged(path, changed);
    } else if (interface == kCharacteristic && changed.contains(QStringLiteral("Value")) && notifying_.contains(path)) {
        // Notified: the value BlueZ keeps for the characteristic, changed.
        const QString service = properties.value(QStringLiteral("Service")).value<QDBusObjectPath>().path();
        const QVariantMap holder = this->properties(service, "org.bluez.GattService1");
        const QString device = holder.value(QStringLiteral("Device")).value<QDBusObjectPath>().path();
        post_value_(this->properties(device, "org.bluez.Device1").value(QStringLiteral("Address")).toString(),
                    fullUuid(holder.value(QStringLiteral("UUID")).toString()),
                    fullUuid(properties.value(QStringLiteral("UUID")).toString()),
                    demarshal<QByteArray>(changed.value(QStringLiteral("Value"))));
    }
}

void Bluetooth::deviceChanged(const QString &path, const QVariantMap &changed) {
    if (!path.startsWith(adapter_ + QLatin1Char('/'))) return;
    const QVariantMap device = properties(path, "org.bluez.Device1");
    // Found: heard advertising while content scans.
    if (discovering_ && device.contains(QStringLiteral("RSSI")) &&
        (changed.contains(QStringLiteral("RSSI")) || changed.contains(QStringLiteral("ManufacturerData")) ||
         changed.contains(QStringLiteral("ServiceData")) || changed.contains(QStringLiteral("Name")))) {
        found(path);
    }
    const QString address = device.value(QStringLiteral("Address")).toString().toUpper();
    const auto connection = connections_.find(address);
    if (connection == connections_.end()) return;
    if (changed.value(QStringLiteral("Connected"), true).toBool() == false) {
        if (!connection->second.ready) {
            connectionFailed(address, kConnectionFailed);
            return;
        }
        connections_.erase(connection);
        for (auto listened = notifying_.begin(); listened != notifying_.end();) {
            listened = listened->startsWith(path + QLatin1Char('/')) ? notifying_.erase(listened) : std::next(listened);
        }
        post_(MIGO_BLUETOOTH_EVENT_BLE_CONNECTION_STATE_CHANGE,
              {{QStringLiteral("deviceId"), address}, {QStringLiteral("connected"), false}});
        return;
    }
    if (!connection->second.ready && device.value(QStringLiteral("ServicesResolved")).toBool()) ready(address);
}

// ---- discovery ----------------------------------------------------------------------------------

void Bluetooth::startDiscovery(const QJsonObject &request, MigoQtHostServiceReply reply) {
    if (!opened_ || adapter_.isEmpty()) return fail(reply, kNotOpened);
    if (!powered()) return fail(reply, kUnavailable);
    QStringList services;
    for (const QJsonValue &named : request.value(QStringLiteral("services")).toArray()) {
        const QString uuid = fullUuid(named.toString());
        if (uuid.isEmpty()) return fail(reply, kInvalidData);
        services << uuid.toLower();
    }
    services_.clear();
    for (const QString &service : std::as_const(services)) services_.insert(service.toUpper());
    allow_duplicates_ = request.value(QStringLiteral("allowDuplicatesKey")).toBool(false);
    interval_ms_ = qMax(0, request.value(QStringLiteral("interval")).toInt(0));
    reported_.clear();
    // BLE only, with the services asked for; sent before starting, and so
    // applied before it.
    QDBusMessage filter = method(adapter_, kAdapter, QStringLiteral("SetDiscoveryFilter"));
    filter << QVariantMap{{QStringLiteral("UUIDs"), services},
                          {QStringLiteral("Transport"), QStringLiteral("le")},
                          {QStringLiteral("DuplicateData"), allow_duplicates_}};
    bus_.asyncCall(filter);
    auto *starting = new QDBusPendingCallWatcher(bus_.asyncCall(method(adapter_, kAdapter, QStringLiteral("StartDiscovery"))), this);
    QObject::connect(starting, &QDBusPendingCallWatcher::finished, this,
                     [this, reply](QDBusPendingCallWatcher *call) mutable {
                         call->deleteLater();
                         const QDBusPendingReply<> started = *call;
                         // Another app scanning already is a scan that runs.
                         if (started.isError() && started.error().name() != QLatin1String("org.bluez.Error.InProgress")) {
                             fail(reply, kSystemError);
                             return;
                         }
                         if (!opened_) return fail(reply, kNotOpened);
                         discovering_ = true;
                         postAdapterState();
                         (void)reply.succeed();
                         // What BlueZ heard before, as content would have heard it now.
                         for (auto object = objects_.cbegin(); object != objects_.cend(); ++object) {
                             const QVariantMap device = object.value().value(kDevice);
                             if (device.contains(QStringLiteral("RSSI"))) found(object.key().path());
                         }
                     });
}

void Bluetooth::stopDiscovery() {
    flush_->stop();
    sendBatch();
    if (!discovering_) return;
    discovering_ = false;
    bus_.asyncCall(method(adapter_, kAdapter, QStringLiteral("StopDiscovery")));
    postAdapterState();
}

void Bluetooth::found(const QString &path) {
    if (!path.startsWith(adapter_ + QLatin1Char('/'))) return;
    if (!services_.isEmpty()) {
        bool offers = false;
        for (const QString &uuid : demarshal<QStringList>(properties(path, "org.bluez.Device1").value(QStringLiteral("UUIDs")))) {
            offers = offers || services_.contains(fullUuid(uuid));
        }
        if (!offers) return;
    }
    seen_.insert(path);
    if (!allow_duplicates_ && reported_.contains(path)) return;
    reported_.insert(path);
    batch_.insert(path, describe(path));
    if (interval_ms_ == 0) {
        sendBatch();
    } else if (!flush_->isActive()) {
        flush_->start(interval_ms_);
    }
}

void Bluetooth::sendBatch() {
    if (batch_.isEmpty()) return;
    QJsonArray devices;
    for (const QJsonObject &device : std::as_const(batch_)) devices.append(device);
    batch_.clear();
    post_(MIGO_BLUETOOTH_EVENT_DEVICE_FOUND, {{QStringLiteral("devices"), devices}});
}

QJsonObject Bluetooth::describe(const QString &path) const {
    const QVariantMap device = properties(path, "org.bluez.Device1");
    QJsonArray uuids;
    for (const QString &uuid : demarshal<QStringList>(device.value(QStringLiteral("UUIDs")))) uuids.append(fullUuid(uuid));
    QJsonObject service_data;
    const QVariantMap data = demarshal<QVariantMap>(device.value(QStringLiteral("ServiceData")));
    for (auto entry = data.cbegin(); entry != data.cend(); ++entry) {
        service_data.insert(fullUuid(entry.key()), hexOf(demarshal<QByteArray>(entry.value())));
    }
    // BlueZ keeps one name: the one advertised, or read once connected.
    const QString name = device.value(QStringLiteral("Name")).toString();
    return {
        {QStringLiteral("deviceId"), device.value(QStringLiteral("Address")).toString().toUpper()},
        {QStringLiteral("name"), name},
        {QStringLiteral("RSSI"), device.value(QStringLiteral("RSSI")).toInt()},
        {QStringLiteral("advertisData"), hexOf(manufacturerData(device.value(QStringLiteral("ManufacturerData"))))},
        {QStringLiteral("advertisServiceUUIDs"), uuids},
        {QStringLiteral("localName"), name},
        {QStringLiteral("serviceData"), service_data},
    };
}

QString Bluetooth::deviceAt(const QString &address) const {
    for (auto object = objects_.cbegin(); object != objects_.cend(); ++object) {
        const QVariantMap device = object.value().value(kDevice);
        if (!device.isEmpty() && object.key().path().startsWith(adapter_ + QLatin1Char('/')) &&
            device.value(QStringLiteral("Address")).toString().compare(address, Qt::CaseInsensitive) == 0) {
            return object.key().path();
        }
    }
    return {};
}

void Bluetooth::connectedDevices(const QJsonObject &request, MigoQtHostServiceReply reply) {
    if (!opened_) return fail(reply, kNotOpened);
    QSet<QString> wanted;
    for (const QJsonValue &named : request.value(QStringLiteral("services")).toArray()) {
        const QString uuid = fullUuid(named.toString());
        if (uuid.isEmpty()) return fail(reply, kInvalidData);
        wanted.insert(uuid);
    }
    if (wanted.isEmpty()) return fail(reply, kInvalidData);
    QJsonArray devices;
    for (auto object = objects_.cbegin(); object != objects_.cend(); ++object) {
        const QVariantMap device = object.value().value(kDevice);
        if (!device.value(QStringLiteral("Connected")).toBool() ||
            !object.key().path().startsWith(adapter_ + QLatin1Char('/'))) {
            continue;
        }
        bool offers = false;
        for (const QString &uuid : demarshal<QStringList>(device.value(QStringLiteral("UUIDs")))) {
            offers = offers || wanted.contains(fullUuid(uuid));
        }
        if (!offers) continue;
        seen_.insert(object.key().path());
        devices.append(QJsonObject{{QStringLiteral("name"), device.value(QStringLiteral("Name")).toString()},
                                   {QStringLiteral("deviceId"), device.value(QStringLiteral("Address")).toString().toUpper()}});
    }
    (void)reply.succeed({{QStringLiteral("devices"), devices}});
}

// ---- connections --------------------------------------------------------------------------------

void Bluetooth::connect(const QJsonObject &request, MigoQtHostServiceReply reply) {
    if (!opened_) return fail(reply, kNotOpened);
    if (!powered()) return fail(reply, kUnavailable);
    const QString address = request.value(QStringLiteral("deviceId")).toString().toUpper();
    const QString device = deviceAt(address);
    if (device.isEmpty()) return fail(reply, kNoDevice);
    if (const auto known = connections_.find(address); known != connections_.end()) {
        return fail(reply, known->second.ready ? kAlreadyConnected : kConnectionFailed);
    }
    seen_.insert(device);
    Connection &connection = connections_[address];
    connection.device = device;
    connection.connecting = std::make_unique<MigoQtHostServiceReply>(std::move(reply));
    if (const double timeout = request.value(QStringLiteral("timeout")).toDouble(0); timeout >= 1) {
        connection.timeout = new QTimer(this);
        connection.timeout->setSingleShot(true);
        QObject::connect(connection.timeout, &QTimer::timeout, this, [this, address] { connectionFailed(address, kTimedOut); });
        connection.timeout->start(static_cast<int>(qMin(timeout, 3.6e6)));
    }
    const QVariantMap known = properties(device, "org.bluez.Device1");
    if (known.value(QStringLiteral("Connected")).toBool() && known.value(QStringLiteral("ServicesResolved")).toBool()) {
        ready(address);
        return;
    }
    auto *connecting = new QDBusPendingCallWatcher(bus_.asyncCall(method(device, kDevice, QStringLiteral("Connect"))), this);
    QObject::connect(connecting, &QDBusPendingCallWatcher::finished, this, [this, address](QDBusPendingCallWatcher *call) {
        call->deleteLater();
        const QDBusPendingReply<> connected = *call;
        const auto connection = connections_.find(address);
        if (connection == connections_.end() || connection->second.ready) return;
        if (connected.isError() && connected.error().name() != QLatin1String("org.bluez.Error.AlreadyConnected")) {
            connectionFailed(address, kConnectionFailed);
            return;
        }
        // Answered once its services are resolved, which may have come first.
        if (this->properties(connection->second.device, "org.bluez.Device1")
                .value(QStringLiteral("ServicesResolved"))
                .toBool()) {
            ready(address);
        }
    });
}

void Bluetooth::ready(const QString &address) {
    const auto connection = connections_.find(address);
    if (connection == connections_.end()) return;
    connection->second.ready = true;
    if (connection->second.timeout != nullptr) {
        connection->second.timeout->deleteLater();
        connection->second.timeout = nullptr;
    }
    if (connection->second.connecting != nullptr) {
        (void)connection->second.connecting->succeed();
        connection->second.connecting.reset();
    }
    post_(MIGO_BLUETOOTH_EVENT_BLE_CONNECTION_STATE_CHANGE,
          {{QStringLiteral("deviceId"), address}, {QStringLiteral("connected"), true}});
}

void Bluetooth::connectionFailed(const QString &address, std::int32_t code) {
    const auto connection = connections_.find(address);
    if (connection == connections_.end()) return;
    if (connection->second.timeout != nullptr) connection->second.timeout->deleteLater();
    std::unique_ptr<MigoQtHostServiceReply> waiting = std::move(connection->second.connecting);
    const QString device = connection->second.device;
    connections_.erase(connection);
    bus_.asyncCall(method(device, kDevice, QStringLiteral("Disconnect")));
    if (waiting != nullptr) fail(*waiting, code);
}

void Bluetooth::disconnect(const QJsonObject &request, MigoQtHostServiceReply reply) {
    if (!opened_) return fail(reply, kNotOpened);
    const auto connection = connections_.find(request.value(QStringLiteral("deviceId")).toString().toUpper());
    if (connection == connections_.end()) return fail(reply, kNotConnected);
    bus_.asyncCall(method(connection->second.device, kDevice, QStringLiteral("Disconnect")));
    (void)reply.succeed();
}

QString Bluetooth::connection(const QJsonObject &request, MigoQtHostServiceReply &reply) {
    if (!opened_) {
        fail(reply, kNotOpened);
        return {};
    }
    const auto connection = connections_.find(request.value(QStringLiteral("deviceId")).toString().toUpper());
    if (connection == connections_.end() || !connection->second.ready) {
        fail(reply, kNotConnected);
        return {};
    }
    return connection->second.device;
}

QString Bluetooth::service(const QString &device, const QJsonObject &request, MigoQtHostServiceReply &reply) {
    const QString uuid = fullUuid(request.value(QStringLiteral("serviceId")).toString());
    for (auto object = objects_.cbegin(); object != objects_.cend(); ++object) {
        const QVariantMap service = object.value().value(kService);
        if (!service.isEmpty() && !uuid.isEmpty() &&
            service.value(QStringLiteral("Device")).value<QDBusObjectPath>().path() == device &&
            fullUuid(service.value(QStringLiteral("UUID")).toString()) == uuid) {
            return object.key().path();
        }
    }
    fail(reply, kNoService);
    return {};
}

QString Bluetooth::characteristic(const QJsonObject &request, MigoQtHostServiceReply &reply) {
    const QString device = connection(request, reply);
    if (device.isEmpty()) return {};
    const QString service = this->service(device, request, reply);
    if (service.isEmpty()) return {};
    const QString uuid = fullUuid(request.value(QStringLiteral("characteristicId")).toString());
    for (auto object = objects_.cbegin(); object != objects_.cend(); ++object) {
        const QVariantMap characteristic = object.value().value(kCharacteristic);
        if (!characteristic.isEmpty() && !uuid.isEmpty() &&
            characteristic.value(QStringLiteral("Service")).value<QDBusObjectPath>().path() == service &&
            fullUuid(characteristic.value(QStringLiteral("UUID")).toString()) == uuid) {
            return object.key().path();
        }
    }
    fail(reply, kNoCharacteristic);
    return {};
}

void Bluetooth::services(const QJsonObject &request, MigoQtHostServiceReply reply) {
    const QString device = connection(request, reply);
    if (device.isEmpty()) return;
    QJsonArray services;
    for (auto object = objects_.cbegin(); object != objects_.cend(); ++object) {
        const QVariantMap service = object.value().value(kService);
        if (service.isEmpty() || service.value(QStringLiteral("Device")).value<QDBusObjectPath>().path() != device) continue;
        services.append(QJsonObject{{QStringLiteral("uuid"), fullUuid(service.value(QStringLiteral("UUID")).toString())},
                                    {QStringLiteral("isPrimary"), service.value(QStringLiteral("Primary")).toBool()}});
    }
    (void)reply.succeed({{QStringLiteral("services"), services}});
}

void Bluetooth::characteristics(const QJsonObject &request, MigoQtHostServiceReply reply) {
    const QString device = connection(request, reply);
    if (device.isEmpty()) return;
    const QString service = this->service(device, request, reply);
    if (service.isEmpty()) return;
    QJsonArray characteristics;
    for (auto object = objects_.cbegin(); object != objects_.cend(); ++object) {
        const QVariantMap characteristic = object.value().value(kCharacteristic);
        if (characteristic.isEmpty() ||
            characteristic.value(QStringLiteral("Service")).value<QDBusObjectPath>().path() != service) {
            continue;
        }
        const QStringList flags = demarshal<QStringList>(characteristic.value(QStringLiteral("Flags")));
        const bool write = flags.contains(QStringLiteral("write"));
        const bool command = flags.contains(QStringLiteral("write-without-response"));
        characteristics.append(QJsonObject{
            {QStringLiteral("uuid"), fullUuid(characteristic.value(QStringLiteral("UUID")).toString())},
            {QStringLiteral("properties"),
             QJsonObject{{QStringLiteral("read"), flags.contains(QStringLiteral("read"))},
                         {QStringLiteral("write"), write || command},
                         {QStringLiteral("notify"), flags.contains(QStringLiteral("notify"))},
                         {QStringLiteral("indicate"), flags.contains(QStringLiteral("indicate"))},
                         {QStringLiteral("writeNoResponse"), command},
                         {QStringLiteral("writeDefault"), write}}},
        });
    }
    (void)reply.succeed({{QStringLiteral("characteristics"), characteristics}});
}

// ---- characteristics ----------------------------------------------------------------------------

void Bluetooth::read(const QJsonObject &request, MigoQtHostServiceReply reply) {
    const QString path = characteristic(request, reply);
    if (path.isEmpty()) return;
    if (!demarshal<QStringList>(properties(path, "org.bluez.GattCharacteristic1").value(QStringLiteral("Flags")))
             .contains(QStringLiteral("read"))) {
        return fail(reply, kNotSupported);
    }
    QDBusMessage reading = method(path, kCharacteristic, QStringLiteral("ReadValue"));
    reading << QVariantMap();
    auto *call = new QDBusPendingCallWatcher(bus_.asyncCall(reading), this);
    const QString device = request.value(QStringLiteral("deviceId")).toString().toUpper();
    const QString service = fullUuid(request.value(QStringLiteral("serviceId")).toString());
    const QString characteristic = fullUuid(request.value(QStringLiteral("characteristicId")).toString());
    QObject::connect(call, &QDBusPendingCallWatcher::finished, this,
                     [this, device, service, characteristic](QDBusPendingCallWatcher *read) {
                         read->deleteLater();
                         const QDBusPendingReply<QByteArray> value = *read;
                         // The value arrives as a characteristic value of its own.
                         if (!value.isError()) post_value_(device, service, characteristic, value.value());
                     });
    (void)reply.succeed();
}

void Bluetooth::write(const QJsonObject &request, MigoQtHostServiceReply reply) {
    const QString path = characteristic(request, reply);
    if (path.isEmpty()) return;
    const std::optional<QByteArray> value = bytesOf(request.value(QStringLiteral("value")).toString());
    if (!value.has_value()) return fail(reply, kInvalidData);
    const QStringList flags =
        demarshal<QStringList>(properties(path, "org.bluez.GattCharacteristic1").value(QStringLiteral("Flags")));
    const bool write = flags.contains(QStringLiteral("write"));
    const bool command = flags.contains(QStringLiteral("write-without-response"));
    const bool without_response =
        request.value(QStringLiteral("writeType")).toString() == QLatin1String("writeNoResponse") || (!write && command);
    if (without_response ? !command : !write) return fail(reply, kNotSupported);
    QDBusMessage writing = method(path, kCharacteristic, QStringLiteral("WriteValue"));
    writing << *value
            << QVariantMap{{QStringLiteral("type"), without_response ? QStringLiteral("command") : QStringLiteral("request")}};
    if (without_response) {
        bus_.asyncCall(writing);
        (void)reply.succeed();
        return;
    }
    auto *call = new QDBusPendingCallWatcher(bus_.asyncCall(writing), this);
    QObject::connect(call, &QDBusPendingCallWatcher::finished, this, [reply](QDBusPendingCallWatcher *wrote) mutable {
        wrote->deleteLater();
        const QDBusPendingReply<> written = *wrote;
        if (written.isError()) {
            fail(reply, codeOf(written.error()));
        } else {
            (void)reply.succeed();
        }
    });
}

void Bluetooth::notify(const QJsonObject &request, MigoQtHostServiceReply reply) {
    const QString path = characteristic(request, reply);
    if (path.isEmpty()) return;
    const QStringList flags =
        demarshal<QStringList>(properties(path, "org.bluez.GattCharacteristic1").value(QStringLiteral("Flags")));
    if (!flags.contains(QStringLiteral("notify")) && !flags.contains(QStringLiteral("indicate"))) {
        return fail(reply, kNotSupported);
    }
    const bool on = request.value(QStringLiteral("state")).toBool(false);
    auto *call = new QDBusPendingCallWatcher(
        bus_.asyncCall(method(path, kCharacteristic, on ? QStringLiteral("StartNotify") : QStringLiteral("StopNotify"))), this);
    QObject::connect(call, &QDBusPendingCallWatcher::finished, this, [this, path, on, reply](QDBusPendingCallWatcher *done) mutable {
        done->deleteLater();
        const QDBusPendingReply<> answered = *done;
        if (!on) {
            // Stopping what was not started is stopped.
            notifying_.remove(path);
            (void)reply.succeed();
            return;
        }
        if (answered.isError()) {
            fail(reply, codeOf(answered.error()));
            return;
        }
        notifying_.insert(path);
        (void)reply.succeed();
    });
}

void Bluetooth::mtu(const QJsonObject &request, MigoQtHostServiceReply reply) {
    const QString device = connection(request, reply);
    if (device.isEmpty()) return;
    // One MTU for the connection, which BlueZ (5.62 on) shows on each characteristic.
    for (auto object = objects_.cbegin(); object != objects_.cend(); ++object) {
        const QVariantMap characteristic = object.value().value(kCharacteristic);
        if (!characteristic.contains(QStringLiteral("MTU")) || !object.key().path().startsWith(device + QLatin1Char('/'))) {
            continue;
        }
        (void)reply.succeed({{QStringLiteral("mtu"), characteristic.value(QStringLiteral("MTU")).toInt()}});
        return;
    }
    (void)reply.fail(QStringLiteral("not supported"));
}

}  // namespace migo::linux_host::qt6

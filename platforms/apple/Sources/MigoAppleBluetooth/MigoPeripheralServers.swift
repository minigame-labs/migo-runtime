import CoreBluetooth
import Foundation

/// One characteristic of this device's GATT database, by its service's UUID and its own, each in full: a device
/// has one database, whichever of content's servers a service was added through.
struct MigoGattAddress: Hashable {
    let service: String
    let characteristic: String

    init(service: CBUUID, characteristic: CBUUID) {
        self.service = MigoBluetoothFormat.fullUUID(service)
        self.characteristic = MigoBluetoothFormat.fullUUID(characteristic)
    }
}

struct MigoGattCharacteristic {
    let uuid: CBUUID
    let properties: CBCharacteristicProperties
    let permissions: CBAttributePermissions
    let descriptors: [(uuid: CBUUID, value: Data)]
}

struct MigoGattService {
    let uuid: CBUUID
    let characteristics: [MigoGattCharacteristic]
}

/// One part of a write a central made: the characteristic, where in its value, and the bytes. `request` is what
/// the radio answers it by.
struct MigoGattWrite {
    let request: AnyObject
    let address: MigoGattAddress
    let offset: Int
    let value: Data
}

/// The Bluetooth radio as a peripheral, as much of it as the servers use: CoreBluetooth's peripheral manager
/// (`MigoPeripheralManagerRadio`), and in tests one that plays the centrals. Its requests are opaque here -- the
/// system's own cannot be made by anything but the system.
protocol MigoPeripheralRadio: AnyObject {
    var events: MigoPeripheralRadioEvents? { get set }
    /// Whether Bluetooth is on; `nil` until the system has said.
    var isOn: Bool? { get }
    /// `service` into the database; `done` with whether the system took it.
    func add(_ service: MigoGattService, done: @escaping (Bool) -> Void)
    func remove(service: String)
    func removeAll()
    /// Advertises, in place of what it advertised; `done` with whether it does.
    func advertise(name: String?, services: [CBUUID], done: @escaping (Bool) -> Void)
    func stopAdvertising()
    /// Answers the read or the write an event handed over as `request`.
    func respond(to request: AnyObject, with value: Data?, _ result: CBATTError.Code)
    /// `value` to the centrals subscribed to the characteristic. `false` when the system has no room for it now:
    /// `readyToNotify` follows.
    func notify(_ value: Data, for address: MigoGattAddress) -> Bool
}

/// What the radio tells the servers. Main thread.
protocol MigoPeripheralRadioEvents: AnyObject {
    func radioChanged()
    func read(_ request: AnyObject, by central: UUID, of address: MigoGattAddress, offset: Int)
    /// One write, whole: its parts in the order the central sent them, answered once, by the first.
    func write(_ parts: [MigoGattWrite], by central: UUID)
    func subscribed(_ central: UUID, to address: MigoGattAddress)
    func unsubscribed(_ central: UUID, from address: MigoGattAddress)
    func readyToNotify()
}

/// `createBLEPeripheralServer`'s servers: this device as a Bluetooth LE peripheral, as the peripheral service
/// (`MIGO_HOST_SERVICE_BLE_PERIPHERAL`) asks for it and reports it.
///
/// A device has one GATT database and one advertisement, so content's servers share them: each service belongs
/// to the server it was added through, and an advertisement replaces the one before it, whichever server's --
/// the reference says the same of iOS.
///
/// A central's read reaches content as a request, answered when content writes the value with its `callbackId`;
/// one content does not answer gets no response, as the reference says. The servers answer only what content
/// never sees: the rest of a long read, from the value content gave. A write is reported whole -- a long one's
/// parts joined -- one request to a characteristic, and answered once every one of them is.
///
/// What the system does not offer is refused or left: an advertisement carries a name and service UUIDs only
/// (the reference says of iOS that manufacturer data cannot be set), and one as a beacon is refused -- the
/// system makes a beacon's advertisement in its location framework, which a Bluetooth product does not link.
/// Nor does the system tell a peripheral that a central connected or left: one is reported connected when it
/// first reads, writes or subscribes, and disconnected when the last of its subscriptions ends, which is how a
/// central that goes away is seen. Main thread.
final class MigoPeripheralServers: MigoPeripheralRadioEvents {
    enum Method: UInt32 {
        case createServer = 0
        case addService, removeService, startAdvertising, stopAdvertising, writeCharacteristicValue
    }

    enum Event: UInt32 {
        case readRequest = 0
        case writeRequest, subscribed, unsubscribed, connectionChanged
    }

    /// Why a call failed: one of the central role's Bluetooth codes, and the reason in words.
    struct Failure: Error, Equatable {
        let code: Int32
        let message: String

        init(_ error: MigoBluetoothError, _ message: String? = nil) {
            code = error.rawValue
            self.message = message ?? error.message
        }
    }

    typealias Answer = (Result<[String: Any], Failure>) -> Void

    private struct Characteristic {
        let server: Int
        var value: Data
    }

    /// A write's one response, owed once content answered every request the write was reported as.
    private final class WriteResponse {
        let request: AnyObject
        var owed: Int

        init(request: AnyObject, owed: Int) {
            self.request = request
            self.owed = owed
        }
    }

    private enum Waiting {
        case read(AnyObject)
        case write(WriteResponse)
    }

    private let makeRadio: () -> MigoPeripheralRadio
    private let isOpen: () -> Bool
    private let post: (Event, [String: Any]) -> Void
    private var radio: MigoPeripheralRadio?
    /// Calls waiting for the system to say whether Bluetooth is on.
    private var waitingForRadio: [(Bool) -> Void] = []
    private var servers: Set<Int> = []
    /// Each served service's server.
    private var services: [String: Int] = [:]
    private var characteristics: [MigoGattAddress: Characteristic] = [:]
    /// The server whose advertisement is up.
    private var advertiser: Int?
    /// The requests content has yet to answer, by the `callbackId` each was reported with.
    private var waiting: [Int: Waiting] = [:]
    private var nextCallback = 1
    /// Notifications the system had no room for, oldest first.
    private var unsent: [(value: Data, address: MigoGattAddress)] = []
    /// The centrals seen, each with what it subscribed to.
    private var centrals: [UUID: Set<MigoGattAddress>] = [:]

    /// `radio` makes the radio when the first server needs it: making the system's is what turns the peripheral
    /// role on. `isOpen` says whether content opened the Bluetooth adapter; `post` reports an event.
    init(
        radio: @escaping () -> MigoPeripheralRadio, isOpen: @escaping () -> Bool,
        post: @escaping (Event, [String: Any]) -> Void
    ) {
        makeRadio = radio
        self.isOpen = isOpen
        self.post = post
    }

    /// One of the peripheral service's calls.
    func handle(_ method: Method, _ options: [String: Any], answer: @escaping Answer) {
        guard isOpen() else { return answer(.failure(Failure(.notOpened))) }
        guard let server = (options["serverId"] as? NSNumber)?.intValue else {
            return answer(.failure(Failure(.invalidData, "no serverId")))
        }
        if method == .createServer {
            whenTheRadioIsKnown { [weak self] on in
                guard let self, self.isOpen() else { return answer(.failure(Failure(.notOpened))) }
                guard on else { return answer(.failure(Failure(.unavailable))) }
                self.servers.insert(server)
                answer(.success([:]))
            }
            return
        }
        guard servers.contains(server) else { return answer(.failure(Failure(.systemError, "no such server"))) }
        guard let radio, radio.isOn == true else { return answer(.failure(Failure(.unavailable))) }
        switch method {
        case .createServer: break
        case .addService: addService(options, to: server, radio, answer)
        case .removeService:
            guard let uuid = (options["serviceId"] as? String).flatMap(MigoBluetoothFormat.uuid),
                services[MigoBluetoothFormat.fullUUID(uuid)] == server
            else { return answer(.failure(Failure(.noService))) }
            let service = MigoBluetoothFormat.fullUUID(uuid)
            radio.remove(service: service)
            services[service] = nil
            characteristics = characteristics.filter { $0.key.service != service }
            unsent.removeAll { $0.address.service == service }
            answer(.success([:]))
        case .startAdvertising: advertise(options, for: server, radio, answer)
        case .stopAdvertising:
            if advertiser == server {
                radio.stopAdvertising()
                advertiser = nil
            }
            answer(.success([:]))
        case .writeCharacteristicValue: write(options, for: server, radio, answer)
        }
    }

    /// The adapter closed, or the session did: the servers go, with what they served, advertised and were asked.
    func close() {
        radio?.stopAdvertising()
        radio?.removeAll()
        forget()
        servers = []
        let calls = waitingForRadio
        waitingForRadio = []
        calls.forEach { $0(false) }
    }

    /// What the radio no longer holds once Bluetooth goes off, or the servers close.
    private func forget() {
        services = [:]
        characteristics = [:]
        advertiser = nil
        waiting = [:]
        unsent = []
        centrals = [:]
    }

    private func whenTheRadioIsKnown(_ then: @escaping (Bool) -> Void) {
        let radio = self.radio ?? makeRadio()
        if self.radio == nil {
            radio.events = self
            self.radio = radio
        }
        if let on = radio.isOn { then(on) } else { waitingForRadio.append(then) }
    }

    // MARK: - the database

    private func addService(_ options: [String: Any], to server: Int, _ radio: MigoPeripheralRadio, _ answer: @escaping Answer)
    {
        guard let described = options["service"] as? [String: Any],
            let uuid = (described["uuid"] as? String).flatMap(MigoBluetoothFormat.uuid)
        else { return answer(.failure(Failure(.invalidData, "no service uuid"))) }
        let name = MigoBluetoothFormat.fullUUID(uuid)
        guard services[name] == nil else { return answer(.failure(Failure(.systemError, "the service is served already"))) }
        var served: [MigoGattCharacteristic] = []
        var values: [MigoGattAddress: Data] = [:]
        for described in described["characteristics"] as? [[String: Any]] ?? [] {
            guard let characteristic = (described["uuid"] as? String).flatMap(MigoBluetoothFormat.uuid),
                let value = MigoBluetoothFormat.bytes(described["value"] as? String ?? "")
            else { return answer(.failure(Failure(.invalidData, "a characteristic's uuid or value is not one"))) }
            var descriptors: [(uuid: CBUUID, value: Data)] = []
            for described in described["descriptors"] as? [[String: Any]] ?? [] {
                guard let descriptor = (described["uuid"] as? String).flatMap(MigoBluetoothFormat.uuid),
                    let value = MigoBluetoothFormat.bytes(described["value"] as? String ?? "")
                else { return answer(.failure(Failure(.invalidData, "a descriptor's uuid or value is not one"))) }
                descriptors.append((descriptor, value))
            }
            let flags = described["properties"] as? [String: Any] ?? [:]
            let allowed = described["permission"] as? [String: Any] ?? [:]
            func set(_ flags: [String: Any], _ key: String) -> Bool { flags[key] as? Bool ?? false }
            var properties: CBCharacteristicProperties = []
            if set(flags, "read") { properties.insert(.read) }
            if set(flags, "write") { properties.insert(.write) }
            if set(flags, "writeNoResponse") { properties.insert(.writeWithoutResponse) }
            if set(flags, "notify") { properties.insert(.notify) }
            if set(flags, "indicate") { properties.insert(.indicate) }
            var permissions: CBAttributePermissions = []
            if set(allowed, "readable") { permissions.insert(.readable) }
            if set(allowed, "writeable") { permissions.insert(.writeable) }
            if set(allowed, "readEncryptionRequired") { permissions.insert(.readEncryptionRequired) }
            if set(allowed, "writeEncryptionRequired") { permissions.insert(.writeEncryptionRequired) }
            served.append(
                MigoGattCharacteristic(
                    uuid: characteristic, properties: properties, permissions: permissions, descriptors: descriptors))
            values[MigoGattAddress(service: uuid, characteristic: characteristic)] = value
        }
        // Held while the system takes it, so the same service asked for twice is refused the second time.
        services[name] = server
        radio.add(MigoGattService(uuid: uuid, characteristics: served)) { [weak self] added in
            guard let self, self.services[name] == server else { return answer(.failure(Failure(.notOpened))) }
            guard added else {
                self.services[name] = nil
                return answer(.failure(Failure(.systemError, "service not added")))
            }
            for (address, value) in values { self.characteristics[address] = Characteristic(server: server, value: value) }
            answer(.success([:]))
        }
    }

    private func advertise(_ options: [String: Any], for server: Int, _ radio: MigoPeripheralRadio, _ answer: @escaping Answer)
    {
        let request = options["advertiseRequest"] as? [String: Any] ?? [:]
        guard request["beacon"] == nil else {
            return answer(.failure(Failure(.notSupported, "advertising as a beacon is not supported on this system")))
        }
        var uuids: [CBUUID] = []
        for named in request["serviceUuids"] as? [String] ?? [] {
            guard let uuid = MigoBluetoothFormat.uuid(named) else {
                return answer(.failure(Failure(.invalidData, "a service uuid is not one")))
            }
            uuids.append(uuid)
        }
        let name = (request["deviceName"] as? String).flatMap { $0.isEmpty ? nil : $0 }
        // One advertisement to a device: this one takes its place, whichever server's it was.
        if advertiser != nil { radio.stopAdvertising() }
        advertiser = server
        radio.advertise(name: name, services: uuids) { [weak self] advertising in
            guard let self, self.advertiser == server else { return answer(.failure(Failure(.notOpened))) }
            guard advertising else {
                self.advertiser = nil
                return answer(.failure(Failure(.systemError, "the advertisement did not start")))
            }
            answer(.success([:]))
        }
    }

    private func write(_ options: [String: Any], for server: Int, _ radio: MigoPeripheralRadio, _ answer: @escaping Answer) {
        guard let service = (options["serviceId"] as? String).flatMap(MigoBluetoothFormat.uuid),
            services[MigoBluetoothFormat.fullUUID(service)] == server
        else { return answer(.failure(Failure(.noService))) }
        guard let characteristic = (options["characteristicId"] as? String).flatMap(MigoBluetoothFormat.uuid),
            characteristics[MigoGattAddress(service: service, characteristic: characteristic)] != nil
        else { return answer(.failure(Failure(.noCharacteristic))) }
        guard let value = MigoBluetoothFormat.bytes(options["value"] as? String ?? "") else {
            return answer(.failure(Failure(.invalidData)))
        }
        let address = MigoGattAddress(service: service, characteristic: characteristic)
        characteristics[address]?.value = value
        if let callback = (options["callbackId"] as? NSNumber)?.intValue, let request = waiting.removeValue(forKey: callback) {
            switch request {
            case .read(let read):
                radio.respond(to: read, with: value, .success)
            case .write(let response):
                response.owed -= 1
                if response.owed == 0 { radio.respond(to: response.request, with: nil, .success) }
            }
        }
        if options["needNotify"] as? Bool ?? false {
            // In order: behind what already waits for room.
            if !unsent.isEmpty || !radio.notify(value, for: address) { unsent.append((value, address)) }
        }
        answer(.success([:]))
    }

    // MARK: - what the radio tells

    func radioChanged() {
        guard let on = radio?.isOn else { return }
        // Off, the system has dropped what was served and advertised; content's servers remain to serve again.
        if !on { forget() }
        let calls = waitingForRadio
        waitingForRadio = []
        calls.forEach { $0(on) }
    }

    func read(_ request: AnyObject, by central: UUID, of address: MigoGattAddress, offset: Int) {
        guard let characteristic = characteristics[address] else {
            radio?.respond(to: request, with: nil, .attributeNotFound)
            return
        }
        seen(central, at: characteristic.server)
        guard offset == 0 else {
            // The rest of a long read: of the value content gave for its start.
            guard offset <= characteristic.value.count else {
                radio?.respond(to: request, with: nil, .invalidOffset)
                return
            }
            radio?.respond(to: request, with: characteristic.value.subdata(in: offset..<characteristic.value.count), .success)
            return
        }
        post(.readRequest, described(address, of: characteristic, ["callbackId": hold(.read(request))]))
    }

    func write(_ parts: [MigoGattWrite], by central: UUID) {
        guard let first = parts.first else { return }
        var order: [MigoGattAddress] = []
        var values: [MigoGattAddress: Data] = [:]
        for part in parts {
            guard characteristics[part.address] != nil else {
                radio?.respond(to: first.request, with: nil, .attributeNotFound)
                return
            }
            var value = values[part.address] ?? Data()
            // A long write's parts follow one another; one that leaves a gap is no write.
            guard part.offset <= value.count else {
                radio?.respond(to: first.request, with: nil, .invalidOffset)
                return
            }
            value.replaceSubrange(part.offset..<value.count, with: part.value)
            if values[part.address] == nil { order.append(part.address) }
            values[part.address] = value
        }
        let response = WriteResponse(request: first.request, owed: order.count)
        for address in order {
            guard let characteristic = characteristics[address], let value = values[address] else { continue }
            seen(central, at: characteristic.server)
            post(
                .writeRequest,
                described(
                    address, of: characteristic,
                    ["value": MigoBluetoothFormat.hex(value), "callbackId": hold(.write(response))]))
        }
    }

    func subscribed(_ central: UUID, to address: MigoGattAddress) {
        guard let characteristic = characteristics[address] else { return }
        seen(central, at: characteristic.server)
        centrals[central]?.insert(address)
        post(.subscribed, described(address, of: characteristic, [:]))
    }

    func unsubscribed(_ central: UUID, from address: MigoGattAddress) {
        guard let characteristic = characteristics[address] else { return }
        post(.unsubscribed, described(address, of: characteristic, [:]))
        guard var subscriptions = centrals[central], subscriptions.remove(address) != nil else { return }
        centrals[central] = subscriptions
        if subscriptions.isEmpty {
            centrals[central] = nil
            post(
                .connectionChanged,
                ["serverId": characteristic.server, "deviceId": central.uuidString, "connected": false])
        }
    }

    func readyToNotify() {
        guard let radio else { return }
        while let next = unsent.first, radio.notify(next.value, for: next.address) { unsent.removeFirst() }
    }

    /// A central at one of `server`'s characteristics: connected, if it was not known to be.
    private func seen(_ central: UUID, at server: Int) {
        guard centrals[central] == nil else { return }
        centrals[central] = []
        post(.connectionChanged, ["serverId": server, "deviceId": central.uuidString, "connected": true])
    }

    private func hold(_ request: Waiting) -> Int {
        let callback = nextCallback
        nextCallback += 1
        waiting[callback] = request
        return callback
    }

    private func described(_ address: MigoGattAddress, of characteristic: Characteristic, _ more: [String: Any]) -> [String: Any] {
        var event: [String: Any] = [
            "serverId": characteristic.server, "serviceId": address.service, "characteristicId": address.characteristic,
        ]
        event.merge(more) { _, new in new }
        return event
    }
}

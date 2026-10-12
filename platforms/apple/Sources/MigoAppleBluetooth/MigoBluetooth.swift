import CoreBluetooth
import Foundation
import MigoAppleRenderer
import MigoEngine

/// Bluetooth for games as a BLE central, through CoreBluetooth: the adapter,
/// scanning, the connected devices, and GATT -- connections with their
/// services and characteristics discovered, reads, writes, notifications, RSSI
/// -- with the app's Bluetooth grant, the Bluetooth switch, and
/// `scope.bluetooth` decided by the system's own prompt.
///
/// An app opts in by linking this product, passing a `MigoBluetooth()` among
/// its game view's capabilities, and declaring
/// `NSBluetoothAlwaysUsageDescription`; without it the system shows no prompt,
/// so the scope is refused without asking.
///
/// What the platform documents for Android only -- pairing, asking whether a
/// device is paired, setting the MTU -- is refused, as is the peripheral mode.
/// iBeacons are CoreLocation's: ranged by `MigoLocation` on iOS when it is
/// linked too, and refused otherwise.
///
/// Create it on the main thread; it is used there.
public final class MigoBluetooth: MigoSystemCapability {
    /// `scripts/gen-api-platform-matrix.py` reads `services` below to count
    /// these APIs as provided on the Apple platforms: one service per line.
    public static let services: Set<MigoHostServiceID> = {
        var builtIn: Set<MigoHostServiceID> = []
        builtIn.insert(.bluetooth)
        return builtIn
    }()

    public var services: Set<MigoHostServiceID> { Self.services }
    public var scopes: Set<MigoScope> { [MigoScope(MIGO_SCOPE_BLUETOOTH)] }

    private typealias Method = MigoHostServiceID.Bluetooth
    private typealias Event = MigoHostServiceID.Bluetooth.Event

    private let delegate = Delegate()
    /// Made when the adapter opens, or to read the switch once the app may use
    /// Bluetooth; making one is what asks the player.
    private var central: CBCentralManager?
    private var reports: MigoCapabilityReports?
    /// Whether content opened the adapter, and is scanning.
    private var opened = false
    private var discovering = false
    /// Calls waiting for the central to know its state.
    private var opening: [MigoHostServiceReply] = []
    private var scopeWaiters: [(Bool) -> Void] = []
    /// Every device found or reached since the adapter opened.
    private var devices: [UUID: Device] = [:]
    /// Found devices waiting for the report interval.
    private var batch: [[String: Any]] = []
    private var flush: Timer?
    private var interval: TimeInterval = 0
    private var connections: [UUID: Connection] = [:]

    public init() {
        delegate.owner = self
    }

    deinit {
        flush?.invalidate()
        if let central {
            if central.isScanning { central.stopScan() }
            for connection in connections.values { central.cancelPeripheralConnection(connection.peripheral) }
        }
    }

    // MARK: - reports

    public func report(to reports: MigoCapabilityReports?) {
        self.reports = reports
        guard reports != nil else {
            close()
            let waiters = scopeWaiters
            scopeWaiters = []
            waiters.forEach { $0(false) }
            return
        }
        // The switch is read only once the app may use Bluetooth: a central
        // made before that would ask the player.
        if CBManager.authorization == .allowedAlways, central == nil { makeCentral() }
        reportState()
    }

    private func reportState() {
        guard let reports else { return }
        let authorization = CBManager.authorization
        reports.updateAppAuthorization { $0.bluetooth = Self.authorization(authorization) }
        reports.setScopeState(MigoScope(MIGO_SCOPE_BLUETOOTH), Self.scopeState(authorization))
        reports.setSystemSetting(
            MigoSystemSettingFlags(MIGO_SYSTEM_SETTING_FLAG_BLUETOOTH_ENABLED), enabled: central?.state == .poweredOn)
    }

    static func authorization(_ authorization: CBManagerAuthorization) -> UInt8 {
        switch authorization {
        case .allowedAlways: return UInt8(MIGO_AUTHORIZATION_AUTHORIZED)
        case .denied, .restricted: return UInt8(MIGO_AUTHORIZATION_DENIED)
        default: return UInt8(MIGO_AUTHORIZATION_NOT_DETERMINED)
        }
    }

    static func scopeState(_ authorization: CBManagerAuthorization) -> MigoScopeState {
        switch authorization {
        case .allowedAlways: return MigoScopeState(MIGO_SCOPE_STATE_GRANTED)
        case .denied, .restricted: return MigoScopeState(MIGO_SCOPE_STATE_DENIED)
        default: return MigoScopeState(MIGO_SCOPE_STATE_UNKNOWN)
        }
    }

    // MARK: - the scope

    public func requestScope(_ scope: MigoScope, completion: @escaping (Bool) -> Void) {
        guard scope == MigoScope(MIGO_SCOPE_BLUETOOTH) else {
            completion(false)
            return
        }
        let authorization = CBManager.authorization
        guard authorization == .notDetermined else {
            reportState()
            completion(authorization == .allowedAlways)
            return
        }
        guard Self.declaresPurpose else {
            NSLog(
                "MigoBluetooth: the app declares no NSBluetoothAlwaysUsageDescription in its Info.plist, so the "
                    + "system shows no prompt and scope.bluetooth is refused")
            completion(false)
            return
        }
        scopeWaiters.append(completion)
        // Making the central is what asks; its first state follows the answer.
        if central == nil { makeCentral() }
    }

    static var declaresPurpose: Bool {
        (Bundle.main.object(forInfoDictionaryKey: "NSBluetoothAlwaysUsageDescription") as? String)?.isEmpty == false
    }

    private func makeCentral() {
        central = CBCentralManager(
            delegate: delegate, queue: .main, options: [CBCentralManagerOptionShowPowerAlertKey: false])
    }

    fileprivate func stateChanged() {
        guard let central else { return }
        if central.state != .poweredOn {
            discovering = false
            failConnections(.notConnected)
        }
        reportState()
        if central.state != .unknown, central.state != .resetting, !scopeWaiters.isEmpty {
            let waiters = scopeWaiters
            scopeWaiters = []
            waiters.forEach { $0(CBManager.authorization == .allowedAlways) }
        }
        guard central.state != .unknown, central.state != .resetting else { return }
        let replies = opening
        opening = []
        for reply in replies { answerOpen(reply) }
        if opened { postAdapterState() }
    }

    private func postAdapterState() {
        reports?.postEvent(
            .bluetooth, event: Event.adapterStateChange,
            object: ["available": central?.state == .poweredOn, "discovering": discovering])
    }

    // MARK: - calls

    public func handle(_ request: MigoHostServiceRequest) {
        guard let reply = request.reply else { return }
        let options = request.object
        switch request.method {
        case Method.openAdapter: openAdapter(options, reply)
        case Method.closeAdapter:
            close()
            reply.succeed()
        case Method.getAdapterState:
            guard opened else { return fail(reply, .notOpened) }
            reply.succeed(["available": central?.state == .poweredOn, "discovering": discovering])
        case Method.startDevicesDiscovery: startDiscovery(options, reply)
        case Method.stopDevicesDiscovery:
            guard opened else { return fail(reply, .notOpened) }
            stopDiscovery()
            reply.succeed()
        case Method.getDevices:
            guard opened else { return fail(reply, .notOpened) }
            reply.succeed(["devices": devices.values.compactMap(\.description)])
        case Method.getConnectedDevices: connectedDevices(options, reply)
        case Method.createBleConnection: connect(options, reply)
        case Method.closeBleConnection: disconnect(options, reply)
        case Method.getBleDeviceServices:
            guard let connection = connection(options, reply) else { return }
            reply.succeed([
                "services": (connection.peripheral.services ?? []).map {
                    ["uuid": MigoBluetoothFormat.fullUUID($0.uuid), "isPrimary": $0.isPrimary] as [String: Any]
                }
            ])
        case Method.getBleDeviceCharacteristics:
            guard let connection = connection(options, reply), let service = service(options, connection, reply) else {
                return
            }
            reply.succeed([
                "characteristics": (service.characteristics ?? []).map {
                    ["uuid": MigoBluetoothFormat.fullUUID($0.uuid), "properties": MigoBluetoothFormat.properties($0.properties)]
                        as [String: Any]
                }
            ])
        case Method.readBleCharacteristicValue: read(options, reply)
        case Method.writeBleCharacteristicValue: write(options, reply)
        case Method.notifyBleCharacteristicValueChange: notify(options, reply)
        case Method.getBleDeviceRssi:
            guard let connection = connection(options, reply) else { return }
            connection.rssi.append(reply)
            connection.peripheral.readRSSI()
        case Method.getBleMtu:
            guard let connection = connection(options, reply) else { return }
            let type: CBCharacteristicWriteType =
                options["writeType"] as? String == "writeNoResponse" ? .withoutResponse : .withResponse
            // The ATT header the payload length leaves out.
            reply.succeed(["mtu": connection.peripheral.maximumWriteValueLength(for: type) + 3])
        case Method.startBeaconDiscovery, Method.stopBeaconDiscovery, Method.getBeacons:
            // Reached only when no linked capability ranges beacons
            // (`MigoBeaconRanging`, which `MigoLocation` is on iOS).
            reply.fail(MigoBeaconError.unsupported)
        default:
            // Pairing, asking whether a device is paired and setting the MTU are
            // Android's.
            reply.fail("not supported")
        }
    }

    private func fail(_ reply: MigoHostServiceReply, _ error: MigoBluetoothError) {
        reply.fail(error.message, code: error.rawValue)
    }

    private func openAdapter(_ options: [String: Any], _ reply: MigoHostServiceReply) {
        if let mode = options["mode"] as? String, mode == "peripheral" {
            reply.fail("not supported")
            return
        }
        opened = true
        if central == nil { makeCentral() }
        if let central, central.state != .unknown, central.state != .resetting {
            answerOpen(reply)
        } else {
            opening.append(reply)
        }
    }

    private func answerOpen(_ reply: MigoHostServiceReply) {
        if central?.state == .poweredOn {
            reply.succeed()
        } else {
            fail(reply, .unavailable)
        }
    }

    /// The adapter closes: scanning stops, connections end, what waited fails.
    private func close() {
        stopDiscovery()
        failConnections(.notConnected)
        if let central {
            for connection in connections.values { central.cancelPeripheralConnection(connection.peripheral) }
        }
        connections = [:]
        devices = [:]
        opened = false
        let replies = opening
        opening = []
        for reply in replies { fail(reply, .notOpened) }
    }

    private func startDiscovery(_ options: [String: Any], _ reply: MigoHostServiceReply) {
        guard opened, let central else { return fail(reply, .notOpened) }
        guard central.state == .poweredOn else { return fail(reply, .unavailable) }
        let names = (options["services"] as? [Any] ?? []).compactMap { $0 as? String }
        let uuids = names.compactMap(MigoBluetoothFormat.uuid)
        guard uuids.count == names.count else { return fail(reply, .invalidData) }
        interval = max(0, (options["interval"] as? NSNumber)?.doubleValue ?? 0) / 1000
        central.scanForPeripherals(
            withServices: uuids.isEmpty ? nil : uuids,
            options: [CBCentralManagerScanOptionAllowDuplicatesKey: options["allowDuplicatesKey"] as? Bool ?? false])
        discovering = true
        postAdapterState()
        reply.succeed()
    }

    private func stopDiscovery() {
        if let central, central.isScanning { central.stopScan() }
        flush?.invalidate()
        flush = nil
        sendBatch()
        if discovering {
            discovering = false
            postAdapterState()
        }
    }

    fileprivate func found(_ peripheral: CBPeripheral, advertisement: [String: Any], rssi: Int) {
        let device = devices[peripheral.identifier] ?? Device(peripheral: peripheral)
        device.advertisement = advertisement
        device.rssi = rssi
        devices[peripheral.identifier] = device
        guard let description = device.description else { return }
        batch.append(description)
        if interval == 0 {
            sendBatch()
        } else if flush == nil {
            flush = Timer.scheduledTimer(withTimeInterval: interval, repeats: false) { [weak self] _ in
                self?.flush = nil
                self?.sendBatch()
            }
        }
    }

    private func sendBatch() {
        guard !batch.isEmpty else { return }
        reports?.postEvent(.bluetooth, event: Event.deviceFound, object: ["devices": batch])
        batch = []
    }

    private func connectedDevices(_ options: [String: Any], _ reply: MigoHostServiceReply) {
        guard opened, let central else { return fail(reply, .notOpened) }
        let names = (options["services"] as? [Any] ?? []).compactMap { $0 as? String }
        let uuids = names.compactMap(MigoBluetoothFormat.uuid)
        guard !uuids.isEmpty, uuids.count == names.count else { return fail(reply, .invalidData) }
        let peripherals = central.retrieveConnectedPeripherals(withServices: uuids)
        for peripheral in peripherals where devices[peripheral.identifier] == nil {
            devices[peripheral.identifier] = Device(peripheral: peripheral)
        }
        reply.succeed([
            "devices": peripherals.map { ["name": $0.name ?? "", "deviceId": $0.identifier.uuidString] as [String: Any] }
        ])
    }

    // MARK: - connections

    private func connect(_ options: [String: Any], _ reply: MigoHostServiceReply) {
        guard opened, let central else { return fail(reply, .notOpened) }
        guard central.state == .poweredOn else { return fail(reply, .unavailable) }
        guard let id = (options["deviceId"] as? String).flatMap(UUID.init(uuidString:)) else {
            return fail(reply, .noDevice)
        }
        if let connection = connections[id] {
            return fail(reply, connection.ready ? .alreadyConnected : .connectionFailed)
        }
        guard let peripheral = devices[id]?.peripheral ?? central.retrievePeripherals(withIdentifiers: [id]).first else {
            return fail(reply, .noDevice)
        }
        devices[id] = devices[id] ?? Device(peripheral: peripheral)
        let connection = Connection(peripheral: peripheral, reply: reply)
        connections[id] = connection
        peripheral.delegate = delegate
        let timeout = (options["timeout"] as? NSNumber)?.doubleValue ?? 0
        if timeout > 0 {
            connection.timeout = Timer.scheduledTimer(withTimeInterval: timeout / 1000, repeats: false) {
                [weak self] _ in self?.connectionFailed(id, .timedOut)
            }
        }
        central.connect(peripheral)
    }

    private func connectionFailed(_ id: UUID, _ error: MigoBluetoothError) {
        guard let connection = connections.removeValue(forKey: id) else { return }
        connection.timeout?.invalidate()
        central?.cancelPeripheralConnection(connection.peripheral)
        if let reply = connection.connecting {
            connection.connecting = nil
            fail(reply, error)
        }
        connection.failAll(.notConnected)
    }

    fileprivate func connected(_ peripheral: CBPeripheral) {
        guard connections[peripheral.identifier] != nil else { return }
        // Answered once every service's characteristics are known.
        peripheral.discoverServices(nil)
    }

    fileprivate func failedToConnect(_ peripheral: CBPeripheral) {
        connectionFailed(peripheral.identifier, .connectionFailed)
    }

    fileprivate func discoveredServices(_ peripheral: CBPeripheral, error: Error?) {
        guard let connection = connections[peripheral.identifier], !connection.ready else { return }
        guard error == nil else { return connectionFailed(peripheral.identifier, .connectionFailed) }
        let services = peripheral.services ?? []
        connection.awaitingCharacteristics = Set(services.map { ObjectIdentifier($0) })
        if services.isEmpty { return ready(connection) }
        for service in services { peripheral.discoverCharacteristics(nil, for: service) }
    }

    fileprivate func discoveredCharacteristics(_ service: CBService, of peripheral: CBPeripheral) {
        guard let connection = connections[peripheral.identifier], !connection.ready else { return }
        connection.awaitingCharacteristics.remove(ObjectIdentifier(service))
        if connection.awaitingCharacteristics.isEmpty { ready(connection) }
    }

    private func ready(_ connection: Connection) {
        connection.ready = true
        connection.timeout?.invalidate()
        connection.timeout = nil
        connection.connecting?.succeed()
        connection.connecting = nil
        reports?.postEvent(
            .bluetooth, event: Event.bleConnectionStateChange,
            object: ["deviceId": connection.peripheral.identifier.uuidString, "connected": true])
    }

    fileprivate func disconnected(_ peripheral: CBPeripheral) {
        guard let connection = connections.removeValue(forKey: peripheral.identifier) else { return }
        connection.timeout?.invalidate()
        if let reply = connection.connecting {
            connection.connecting = nil
            fail(reply, .connectionFailed)
        }
        connection.failAll(.notConnected)
        reports?.postEvent(
            .bluetooth, event: Event.bleConnectionStateChange,
            object: ["deviceId": peripheral.identifier.uuidString, "connected": false])
    }

    private func disconnect(_ options: [String: Any], _ reply: MigoHostServiceReply) {
        guard opened, let central else { return fail(reply, .notOpened) }
        guard let id = (options["deviceId"] as? String).flatMap(UUID.init(uuidString:)),
            let connection = connections[id]
        else { return fail(reply, .notConnected) }
        central.cancelPeripheralConnection(connection.peripheral)
        reply.succeed()
    }

    private func failConnections(_ error: MigoBluetoothError) {
        for (id, connection) in connections where !connection.ready {
            connectionFailed(id, .connectionFailed)
        }
        for connection in connections.values { connection.failAll(error) }
    }

    /// The ready connection a call names, or the call failed as why not.
    private func connection(_ options: [String: Any], _ reply: MigoHostServiceReply) -> Connection? {
        guard opened else {
            fail(reply, .notOpened)
            return nil
        }
        guard let id = (options["deviceId"] as? String).flatMap(UUID.init(uuidString:)),
            let connection = connections[id], connection.ready
        else {
            fail(reply, .notConnected)
            return nil
        }
        return connection
    }

    private func service(_ options: [String: Any], _ connection: Connection, _ reply: MigoHostServiceReply)
        -> CBService?
    {
        let name = options["serviceId"] as? String ?? ""
        guard let service = connection.peripheral.services?.first(where: { MigoBluetoothFormat.same($0.uuid, name) })
        else {
            fail(reply, .noService)
            return nil
        }
        return service
    }

    private func characteristic(_ options: [String: Any], _ reply: MigoHostServiceReply)
        -> (Connection, CBCharacteristic)?
    {
        guard let connection = connection(options, reply), let service = service(options, connection, reply) else {
            return nil
        }
        let name = options["characteristicId"] as? String ?? ""
        guard
            let characteristic = service.characteristics?.first(where: { MigoBluetoothFormat.same($0.uuid, name) })
        else {
            fail(reply, .noCharacteristic)
            return nil
        }
        return (connection, characteristic)
    }

    // MARK: - characteristics

    private func read(_ options: [String: Any], _ reply: MigoHostServiceReply) {
        guard let (connection, characteristic) = characteristic(options, reply) else { return }
        guard characteristic.properties.contains(.read) else { return fail(reply, .notSupported) }
        connection.peripheral.readValue(for: characteristic)
        // The value arrives as a characteristic value of its own.
        reply.succeed()
    }

    private func write(_ options: [String: Any], _ reply: MigoHostServiceReply) {
        guard let (connection, characteristic) = characteristic(options, reply) else { return }
        guard let value = (options["value"] as? String).flatMap(MigoBluetoothFormat.bytes) else {
            return fail(reply, .invalidData)
        }
        let properties = characteristic.properties
        let withoutResponse =
            options["writeType"] as? String == "writeNoResponse"
            || (!properties.contains(.write) && properties.contains(.writeWithoutResponse))
        if withoutResponse {
            guard properties.contains(.writeWithoutResponse) else { return fail(reply, .notSupported) }
            connection.peripheral.writeValue(value, for: characteristic, type: .withoutResponse)
            reply.succeed()
        } else {
            guard properties.contains(.write) else { return fail(reply, .notSupported) }
            connection.writes[ObjectIdentifier(characteristic), default: []].append(reply)
            connection.peripheral.writeValue(value, for: characteristic, type: .withResponse)
        }
    }

    private func notify(_ options: [String: Any], _ reply: MigoHostServiceReply) {
        guard let (connection, characteristic) = characteristic(options, reply) else { return }
        guard characteristic.properties.contains(.notify) || characteristic.properties.contains(.indicate) else {
            return fail(reply, .notSupported)
        }
        connection.notifications[ObjectIdentifier(characteristic), default: []].append(reply)
        connection.peripheral.setNotifyValue(options["state"] as? Bool ?? false, for: characteristic)
    }

    fileprivate func valueUpdated(_ characteristic: CBCharacteristic, of peripheral: CBPeripheral, error: Error?) {
        guard error == nil, let value = characteristic.value, let service = characteristic.service else { return }
        reports?.postBleCharacteristicValue(
            deviceId: peripheral.identifier.uuidString, serviceId: MigoBluetoothFormat.fullUUID(service.uuid),
            characteristicId: MigoBluetoothFormat.fullUUID(characteristic.uuid), value: value)
    }

    fileprivate func wrote(_ characteristic: CBCharacteristic, of peripheral: CBPeripheral, error: Error?) {
        guard let connection = connections[peripheral.identifier],
            let reply = connection.writes[ObjectIdentifier(characteristic)]?.first
        else { return }
        connection.writes[ObjectIdentifier(characteristic)]?.removeFirst()
        if error == nil { reply.succeed() } else { fail(reply, .systemError) }
    }

    fileprivate func notifying(_ characteristic: CBCharacteristic, of peripheral: CBPeripheral, error: Error?) {
        guard let connection = connections[peripheral.identifier],
            let reply = connection.notifications[ObjectIdentifier(characteristic)]?.first
        else { return }
        connection.notifications[ObjectIdentifier(characteristic)]?.removeFirst()
        if error == nil { reply.succeed() } else { fail(reply, .systemError) }
    }

    fileprivate func readRSSI(_ rssi: NSNumber, of peripheral: CBPeripheral, error: Error?) {
        guard let connection = connections[peripheral.identifier], !connection.rssi.isEmpty else { return }
        let reply = connection.rssi.removeFirst()
        if error == nil { reply.succeed(["RSSI": rssi.intValue]) } else { fail(reply, .systemError) }
    }

    // MARK: - state per device

    private final class Device {
        let peripheral: CBPeripheral
        var advertisement: [String: Any] = [:]
        var rssi = 0
        /// Seen advertising: a device only reached is not one discovery found.
        var description: [String: Any]? {
            advertisement.isEmpty && rssi == 0
                ? nil
                : MigoBluetoothFormat.device(
                    id: peripheral.identifier.uuidString, name: peripheral.name, rssi: rssi,
                    advertisement: advertisement)
        }

        init(peripheral: CBPeripheral) { self.peripheral = peripheral }
    }

    private final class Connection {
        let peripheral: CBPeripheral
        /// The connect call, until its services and characteristics are known.
        var connecting: MigoHostServiceReply?
        var timeout: Timer?
        var ready = false
        var awaitingCharacteristics: Set<ObjectIdentifier> = []
        var writes: [ObjectIdentifier: [MigoHostServiceReply]] = [:]
        var notifications: [ObjectIdentifier: [MigoHostServiceReply]] = [:]
        var rssi: [MigoHostServiceReply] = []

        init(peripheral: CBPeripheral, reply: MigoHostServiceReply) {
            self.peripheral = peripheral
            connecting = reply
        }

        func failAll(_ error: MigoBluetoothError) {
            let waiting = writes.values.flatMap { $0 } + notifications.values.flatMap { $0 } + rssi
            writes = [:]
            notifications = [:]
            rssi = []
            for reply in waiting { reply.fail(error.message, code: error.rawValue) }
        }
    }

    /// CoreBluetooth's delegate, apart so the capability's interface is its own.
    private final class Delegate: NSObject, CBCentralManagerDelegate, CBPeripheralDelegate {
        weak var owner: MigoBluetooth?

        func centralManagerDidUpdateState(_ central: CBCentralManager) { owner?.stateChanged() }

        func centralManager(
            _ central: CBCentralManager, didDiscover peripheral: CBPeripheral, advertisementData: [String: Any],
            rssi RSSI: NSNumber
        ) {
            owner?.found(peripheral, advertisement: advertisementData, rssi: RSSI.intValue)
        }

        func centralManager(_ central: CBCentralManager, didConnect peripheral: CBPeripheral) {
            owner?.connected(peripheral)
        }

        func centralManager(_ central: CBCentralManager, didFailToConnect peripheral: CBPeripheral, error: Error?) {
            owner?.failedToConnect(peripheral)
        }

        func centralManager(
            _ central: CBCentralManager, didDisconnectPeripheral peripheral: CBPeripheral, error: Error?
        ) {
            owner?.disconnected(peripheral)
        }

        func peripheral(_ peripheral: CBPeripheral, didDiscoverServices error: Error?) {
            owner?.discoveredServices(peripheral, error: error)
        }

        func peripheral(
            _ peripheral: CBPeripheral, didDiscoverCharacteristicsFor service: CBService, error: Error?
        ) {
            owner?.discoveredCharacteristics(service, of: peripheral)
        }

        func peripheral(
            _ peripheral: CBPeripheral, didUpdateValueFor characteristic: CBCharacteristic, error: Error?
        ) {
            owner?.valueUpdated(characteristic, of: peripheral, error: error)
        }

        func peripheral(
            _ peripheral: CBPeripheral, didWriteValueFor characteristic: CBCharacteristic, error: Error?
        ) {
            owner?.wrote(characteristic, of: peripheral, error: error)
        }

        func peripheral(
            _ peripheral: CBPeripheral, didUpdateNotificationStateFor characteristic: CBCharacteristic,
            error: Error?
        ) {
            owner?.notifying(characteristic, of: peripheral, error: error)
        }

        func peripheral(_ peripheral: CBPeripheral, didReadRSSI RSSI: NSNumber, error: Error?) {
            owner?.readRSSI(RSSI, of: peripheral, error: error)
        }
    }
}

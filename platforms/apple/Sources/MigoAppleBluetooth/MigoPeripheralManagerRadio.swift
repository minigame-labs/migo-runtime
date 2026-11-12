import CoreBluetooth
import Foundation

/// The radio the servers use, as CoreBluetooth's peripheral manager is one. Making it is what turns the
/// peripheral role on -- and, the first time, what the system asks the player's leave for. Main thread.
final class MigoPeripheralManagerRadio: NSObject, MigoPeripheralRadio, CBPeripheralManagerDelegate {
    weak var events: MigoPeripheralRadioEvents?
    private var manager: CBPeripheralManager?
    private var services: [String: CBMutableService] = [:]
    private var characteristics: [MigoGattAddress: CBMutableCharacteristic] = [:]
    /// Services the system has yet to say it took.
    private var adding: [String: (Bool) -> Void] = [:]
    private var advertising: ((Bool) -> Void)?

    override init() {
        super.init()
        manager = CBPeripheralManager(delegate: self, queue: .main, options: [CBPeripheralManagerOptionShowPowerAlertKey: false])
    }

    var isOn: Bool? {
        switch manager?.state {
        case .none, .unknown, .resetting: return nil
        case .poweredOn: return true
        default: return false
        }
    }

    func add(_ service: MigoGattService, done: @escaping (Bool) -> Void) {
        let name = MigoBluetoothFormat.fullUUID(service.uuid)
        let made = CBMutableService(type: service.uuid, primary: true)
        var served: [CBMutableCharacteristic] = []
        for described in service.characteristics {
            // With no value of its own: a characteristic made with one is answered by the system from it, and
            // content would never hear a read.
            let characteristic = CBMutableCharacteristic(
                type: described.uuid, properties: described.properties, value: nil, permissions: described.permissions)
            let descriptors = described.descriptors.compactMap(Self.descriptor)
            if !descriptors.isEmpty { characteristic.descriptors = descriptors }
            served.append(characteristic)
            characteristics[MigoGattAddress(service: service.uuid, characteristic: described.uuid)] = characteristic
        }
        made.characteristics = served
        services[name] = made
        adding[name] = done
        manager?.add(made)
    }

    /// A descriptor as the system serves one: a user description, which is a string; a presentation format, which
    /// is its seven bytes; or one of an app's own. The others in Bluetooth's own range -- the client configuration
    /// a subscription sets among them -- are the system's to make, and it refuses them made for it.
    static func descriptor(_ described: (uuid: CBUUID, value: Data)) -> CBMutableDescriptor? {
        let name = MigoBluetoothFormat.fullUUID(described.uuid)
        if name.hasPrefix("00002901-") {
            return CBMutableDescriptor(type: described.uuid, value: String(decoding: described.value, as: UTF8.self))
        }
        if name.hasPrefix("00002904-") {
            return described.value.count == 7 ? CBMutableDescriptor(type: described.uuid, value: described.value) : nil
        }
        if name.hasSuffix("-0000-1000-8000-00805F9B34FB") { return nil }
        return CBMutableDescriptor(type: described.uuid, value: described.value)
    }

    func remove(service: String) {
        guard let made = services.removeValue(forKey: service) else { return }
        characteristics = characteristics.filter { $0.key.service != service }
        adding.removeValue(forKey: service)?(false)
        manager?.remove(made)
    }

    func removeAll() {
        services = [:]
        characteristics = [:]
        let waiting = adding
        adding = [:]
        waiting.values.forEach { $0(false) }
        manager?.removeAllServices()
    }

    func advertise(name: String?, services: [CBUUID], done: @escaping (Bool) -> Void) {
        var data: [String: Any] = [:]
        if let name { data[CBAdvertisementDataLocalNameKey] = name }
        if !services.isEmpty { data[CBAdvertisementDataServiceUUIDsKey] = services }
        advertising?(false)
        advertising = done
        manager?.startAdvertising(data)
    }

    func stopAdvertising() {
        let waiting = advertising
        advertising = nil
        waiting?(false)
        manager?.stopAdvertising()
    }

    func respond(to request: AnyObject, with value: Data?, _ result: CBATTError.Code) {
        guard let request = request as? CBATTRequest else { return }
        if let value { request.value = value }
        manager?.respond(to: request, withResult: result)
    }

    func notify(_ value: Data, for address: MigoGattAddress) -> Bool {
        // A characteristic no longer served has nobody to tell: nothing waits for room.
        guard let manager, let characteristic = characteristics[address] else { return true }
        return manager.updateValue(value, for: characteristic, onSubscribedCentrals: nil)
    }

    private static func address(of characteristic: CBCharacteristic) -> MigoGattAddress? {
        guard let service = characteristic.service else { return nil }
        return MigoGattAddress(service: service.uuid, characteristic: characteristic.uuid)
    }

    // MARK: - CBPeripheralManagerDelegate

    func peripheralManagerDidUpdateState(_ peripheral: CBPeripheralManager) {
        if peripheral.state != .poweredOn {
            // The system dropped what was served and advertised.
            services = [:]
            characteristics = [:]
        }
        events?.radioChanged()
    }

    func peripheralManager(_ peripheral: CBPeripheralManager, didAdd service: CBService, error: Error?) {
        let name = MigoBluetoothFormat.fullUUID(service.uuid)
        if error != nil {
            services[name] = nil
            characteristics = characteristics.filter { $0.key.service != name }
        }
        adding.removeValue(forKey: name)?(error == nil)
    }

    func peripheralManagerDidStartAdvertising(_ peripheral: CBPeripheralManager, error: Error?) {
        let waiting = advertising
        advertising = nil
        waiting?(error == nil)
    }

    func peripheralManager(_ peripheral: CBPeripheralManager, didReceiveRead request: CBATTRequest) {
        guard let address = Self.address(of: request.characteristic) else {
            peripheral.respond(to: request, withResult: .attributeNotFound)
            return
        }
        events?.read(request, by: request.central.identifier, of: address, offset: request.offset)
    }

    func peripheralManager(_ peripheral: CBPeripheralManager, didReceiveWrite requests: [CBATTRequest]) {
        guard let first = requests.first else { return }
        var parts: [MigoGattWrite] = []
        for request in requests {
            guard let address = Self.address(of: request.characteristic) else {
                peripheral.respond(to: first, withResult: .attributeNotFound)
                return
            }
            parts.append(MigoGattWrite(request: request, address: address, offset: request.offset, value: request.value ?? Data()))
        }
        events?.write(parts, by: first.central.identifier)
    }

    func peripheralManager(
        _ peripheral: CBPeripheralManager, central: CBCentral, didSubscribeTo characteristic: CBCharacteristic
    ) {
        guard let address = Self.address(of: characteristic) else { return }
        events?.subscribed(central.identifier, to: address)
    }

    func peripheralManager(
        _ peripheral: CBPeripheralManager, central: CBCentral, didUnsubscribeFrom characteristic: CBCharacteristic
    ) {
        guard let address = Self.address(of: characteristic) else { return }
        events?.unsubscribed(central.identifier, from: address)
    }

    func peripheralManagerIsReady(toUpdateSubscribers peripheral: CBPeripheralManager) {
        events?.readyToNotify()
    }
}

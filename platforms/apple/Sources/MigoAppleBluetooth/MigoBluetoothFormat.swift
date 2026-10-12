import CoreBluetooth
import Foundation

/// How the Bluetooth service words what CoreBluetooth reports: UUIDs in full,
/// upper case, as the common platform shows them; bytes as lower-case hex.
enum MigoBluetoothFormat {
    /// The Bluetooth base UUID a 16- or 32-bit UUID abbreviates.
    private static let base = "-0000-1000-8000-00805F9B34FB"

    /// `uuid` in its full 128-bit form.
    static func fullUUID(_ uuid: CBUUID) -> String {
        let string = uuid.uuidString.uppercased()
        switch string.count {
        case 4: return "0000" + string + base
        case 8: return string + base
        default: return string
        }
    }

    /// A UUID content named, in any of its forms; `nil` when it is none.
    static func uuid(_ string: String) -> CBUUID? {
        let trimmed = string.trimmingCharacters(in: .whitespaces)
        let hex = trimmed.replacingOccurrences(of: "-", with: "")
        guard [4, 8, 32].contains(hex.count), hex.allSatisfy(\.isHexDigit) else { return nil }
        return CBUUID(string: trimmed)
    }

    /// Whether `a` and `b` name the same UUID, whatever form each is in.
    static func same(_ a: CBUUID, _ b: String) -> Bool {
        guard let other = uuid(b) else { return false }
        return fullUUID(a) == fullUUID(other)
    }

    static func hex(_ data: Data) -> String {
        let digits = Array("0123456789abcdef".utf8)
        var out = [UInt8]()
        out.reserveCapacity(data.count * 2)
        for byte in data {
            out.append(digits[Int(byte >> 4)])
            out.append(digits[Int(byte & 0x0f)])
        }
        return String(decoding: out, as: UTF8.self)
    }

    /// The bytes a hex string spells; `nil` when it spells none.
    static func bytes(_ hex: String) -> Data? {
        let digits = Array(hex.utf8)
        guard digits.count % 2 == 0 else { return nil }
        var data = Data(capacity: digits.count / 2)
        var index = 0
        while index < digits.count {
            guard let high = nibble(digits[index]), let low = nibble(digits[index + 1]) else { return nil }
            data.append(high << 4 | low)
            index += 2
        }
        return data
    }

    private static func nibble(_ digit: UInt8) -> UInt8? {
        switch digit {
        case UInt8(ascii: "0")...UInt8(ascii: "9"): return digit - UInt8(ascii: "0")
        case UInt8(ascii: "a")...UInt8(ascii: "f"): return digit - UInt8(ascii: "a") + 10
        case UInt8(ascii: "A")...UInt8(ascii: "F"): return digit - UInt8(ascii: "A") + 10
        default: return nil
        }
    }

    /// A found device as DEVICE_FOUND and GET_DEVICES describe it, from what
    /// it advertised: the manufacturer segment (company identifier first), its
    /// services and their data, its local name.
    static func device(id: String, name: String?, rssi: Int, advertisement: [String: Any]) -> [String: Any] {
        let localName = advertisement[CBAdvertisementDataLocalNameKey] as? String ?? ""
        let services = (advertisement[CBAdvertisementDataServiceUUIDsKey] as? [CBUUID] ?? []).map(fullUUID)
        var serviceData: [String: String] = [:]
        for (uuid, data) in advertisement[CBAdvertisementDataServiceDataKey] as? [CBUUID: Data] ?? [:] {
            serviceData[fullUUID(uuid)] = hex(data)
        }
        return [
            "deviceId": id,
            "name": name ?? localName,
            "RSSI": rssi,
            "advertisData": hex(advertisement[CBAdvertisementDataManufacturerDataKey] as? Data ?? Data()),
            "advertisServiceUUIDs": services,
            "localName": localName,
            "serviceData": serviceData,
        ]
    }

    /// What a characteristic allows, as GET_BLE_DEVICE_CHARACTERISTICS says it.
    static func properties(_ properties: CBCharacteristicProperties) -> [String: Bool] {
        [
            "read": properties.contains(.read),
            "write": properties.contains(.write) || properties.contains(.writeWithoutResponse),
            "notify": properties.contains(.notify),
            "indicate": properties.contains(.indicate),
            "writeNoResponse": properties.contains(.writeWithoutResponse),
            "writeDefault": properties.contains(.write),
        ]
    }
}

/// The common platform's codes for a Bluetooth call that failed.
enum MigoBluetoothError: Int32 {
    case notOpened = 10000
    case unavailable = 10001
    case noDevice = 10002
    case connectionFailed = 10003
    case noService = 10004
    case noCharacteristic = 10005
    case notConnected = 10006
    case notSupported = 10007
    case systemError = 10008
    case timedOut = 10012
    case invalidData = 10013
    case alreadyConnected = -1

    var message: String {
        switch self {
        case .notOpened: return "not init"
        case .unavailable: return "not available"
        case .noDevice: return "no device"
        case .connectionFailed: return "connection fail"
        case .noService: return "no service"
        case .noCharacteristic: return "no characteristic"
        case .notConnected: return "no connection"
        case .notSupported: return "property not support"
        case .systemError: return "system error"
        case .timedOut: return "operate time out"
        case .invalidData: return "invalid data"
        case .alreadyConnected: return "already connect"
        }
    }
}

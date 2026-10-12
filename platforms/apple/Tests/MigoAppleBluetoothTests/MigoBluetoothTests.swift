import CoreBluetooth
import Foundation
import MigoEngine
import XCTest

@testable import MigoAppleBluetooth
@testable import MigoAppleRenderer

/// Bluetooth without a radio: how CoreBluetooth's reports are worded, the
/// grant in the engine's words, and what is refused before the adapter opens
/// or because the platform documents it for Android only.
final class MigoBluetoothTests: XCTestCase {
    private final class Recorder: @unchecked Sendable {
        var outcomes: [MigoHostServiceReply.Outcome] = []
    }

    func testUUIDsAreFullAndUpperCaseWhateverFormContentUses() {
        XCTAssertEqual(MigoBluetoothFormat.fullUUID(CBUUID(string: "180F")), "0000180F-0000-1000-8000-00805F9B34FB")
        XCTAssertEqual(
            MigoBluetoothFormat.fullUUID(CBUUID(string: "6e400001-b5a3-f393-e0a9-e50e24dcca9e")),
            "6E400001-B5A3-F393-E0A9-E50E24DCCA9E")
        XCTAssertTrue(MigoBluetoothFormat.same(CBUUID(string: "180F"), "0000180f-0000-1000-8000-00805f9b34fb"))
        XCTAssertTrue(MigoBluetoothFormat.same(CBUUID(string: "0000180F-0000-1000-8000-00805F9B34FB"), "180f"))
        XCTAssertNil(MigoBluetoothFormat.uuid("battery"))
        XCTAssertNil(MigoBluetoothFormat.uuid("18"))
    }

    func testBytesAreLowerCaseHexBothWays() {
        let data = Data([0x00, 0x0f, 0xa5, 0xff])
        XCTAssertEqual(MigoBluetoothFormat.hex(data), "000fa5ff")
        XCTAssertEqual(MigoBluetoothFormat.bytes("000FA5ff"), data)
        XCTAssertNil(MigoBluetoothFormat.bytes("abc"))
        XCTAssertNil(MigoBluetoothFormat.bytes("zz"))
        XCTAssertEqual(MigoBluetoothFormat.bytes(""), Data())
    }

    func testAFoundDeviceSaysWhatItAdvertised() {
        let device = MigoBluetoothFormat.device(
            id: "A", name: nil, rssi: -60,
            advertisement: [
                CBAdvertisementDataLocalNameKey: "Pad",
                CBAdvertisementDataManufacturerDataKey: Data([0x4c, 0x00, 0x02]),
                CBAdvertisementDataServiceUUIDsKey: [CBUUID(string: "180D")],
                CBAdvertisementDataServiceDataKey: [CBUUID(string: "180F"): Data([0x64])],
            ])
        XCTAssertEqual(device["deviceId"] as? String, "A")
        // No GAP name: the advertised local name stands in.
        XCTAssertEqual(device["name"] as? String, "Pad")
        XCTAssertEqual(device["RSSI"] as? Int, -60)
        XCTAssertEqual(device["advertisData"] as? String, "4c0002")
        XCTAssertEqual(device["advertisServiceUUIDs"] as? [String], ["0000180D-0000-1000-8000-00805F9B34FB"])
        XCTAssertEqual(device["serviceData"] as? [String: String], ["0000180F-0000-1000-8000-00805F9B34FB": "64"])
    }

    func testACharacteristicsPropertiesInTheAPIsWords() {
        let both = MigoBluetoothFormat.properties([.read, .write, .writeWithoutResponse, .indicate])
        XCTAssertEqual(
            both,
            ["read": true, "write": true, "notify": false, "indicate": true, "writeNoResponse": true, "writeDefault": true])
        XCTAssertEqual(MigoBluetoothFormat.properties([.writeWithoutResponse])["writeDefault"], false)
        XCTAssertEqual(MigoBluetoothFormat.properties([.writeWithoutResponse])["write"], true)
    }

    func testTheGrantInTheEnginesWords() {
        XCTAssertEqual(MigoBluetooth.authorization(.notDetermined), UInt8(MIGO_AUTHORIZATION_NOT_DETERMINED))
        XCTAssertEqual(MigoBluetooth.authorization(.denied), UInt8(MIGO_AUTHORIZATION_DENIED))
        XCTAssertEqual(MigoBluetooth.authorization(.allowedAlways), UInt8(MIGO_AUTHORIZATION_AUTHORIZED))
        XCTAssertEqual(MigoBluetooth.scopeState(.allowedAlways), MigoScopeState(MIGO_SCOPE_STATE_GRANTED))
        XCTAssertEqual(MigoBluetooth.scopeState(.restricted), MigoScopeState(MIGO_SCOPE_STATE_DENIED))
        XCTAssertEqual(MigoBluetooth.scopeState(.notDetermined), MigoScopeState(MIGO_SCOPE_STATE_UNKNOWN))
    }

    func testWhatIsRefusedAndWhy() {
        let bluetooth = MigoBluetooth()
        XCTAssertEqual(bluetooth.services, [.bluetooth])
        XCTAssertEqual(bluetooth.scopes, [MigoScope(MIGO_SCOPE_BLUETOOTH)])
        let recorder = Recorder()
        func call(_ method: UInt32, _ object: [String: Any] = [:]) {
            bluetooth.handle(
                MigoHostServiceRequest(
                    service: .bluetooth, method: method, payload: try! JSONSerialization.data(withJSONObject: object),
                    reply: MigoHostServiceReply { recorder.outcomes.append($0) }))
        }
        typealias Method = MigoHostServiceID.Bluetooth
        // Nothing opened yet.
        call(Method.getAdapterState)
        call(Method.startDevicesDiscovery)
        call(Method.getBleDeviceServices, ["deviceId": UUID().uuidString])
        // Android's, and the peripheral mode.
        call(Method.makePair, ["deviceId": "A"])
        call(Method.setBleMtu, ["deviceId": "A", "mtu": 100])
        call(Method.openAdapter, ["mode": "peripheral"])
        // CoreLocation's.
        call(Method.getBeacons)
        XCTAssertEqual(
            recorder.outcomes,
            [
                .failure(message: "not init", code: 10000), .failure(message: "not init", code: 10000),
                .failure(message: "not init", code: 10000), .failure(message: "not supported", code: nil),
                .failure(message: "not supported", code: nil), .failure(message: "not supported", code: nil),
                .failure(message: "unsupport", code: 11000),
            ])
    }
}

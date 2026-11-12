import CoreBluetooth
import XCTest

@testable import MigoAppleBluetooth

/// A radio that plays the system and the centrals: it records what the servers ask of it, and the test says what
/// a central does.
private final class Radio: MigoPeripheralRadio {
    final class Request {}

    weak var events: MigoPeripheralRadioEvents?
    var isOn: Bool?
    var refusesServices = false
    var refusesAdvertising = false
    /// How many notifications the system has room for.
    var room = Int.max
    var added: [MigoGattService] = []
    var removed: [String] = []
    var removedAll = 0
    var advertisements: [(name: String?, services: [CBUUID])] = []
    var stops = 0
    var responses: [(request: Request, value: Data?, result: CBATTError.Code)] = []
    var notified: [(value: Data, address: MigoGattAddress)] = []

    func add(_ service: MigoGattService, done: @escaping (Bool) -> Void) {
        if !refusesServices { added.append(service) }
        done(!refusesServices)
    }

    func remove(service: String) { removed.append(service) }
    func removeAll() { removedAll += 1 }

    func advertise(name: String?, services: [CBUUID], done: @escaping (Bool) -> Void) {
        if !refusesAdvertising { advertisements.append((name, services)) }
        done(!refusesAdvertising)
    }

    func stopAdvertising() { stops += 1 }

    func respond(to request: AnyObject, with value: Data?, _ result: CBATTError.Code) {
        responses.append((request as! Request, value, result))
    }

    func notify(_ value: Data, for address: MigoGattAddress) -> Bool {
        guard room > 0 else { return false }
        room -= 1
        notified.append((value, address))
        return true
    }
}

final class MigoPeripheralServersTests: XCTestCase {
    private typealias Failure = MigoPeripheralServers.Failure

    private let radio = Radio()
    private var open = true
    private var made = 0
    private var events: [(event: MigoPeripheralServers.Event, object: NSDictionary)] = []
    private lazy var servers = MigoPeripheralServers(
        radio: { [unowned self] in
            self.made += 1
            return self.radio
        }, isOpen: { [unowned self] in self.open },
        post: { [unowned self] event, object in self.events.append((event, object as NSDictionary)) })

    private let service = "0000FFE0-0000-1000-8000-00805F9B34FB"
    private let characteristic = "0000FFE1-0000-1000-8000-00805F9B34FB"
    private let other = "0000FFE2-0000-1000-8000-00805F9B34FB"
    private var address: MigoGattAddress {
        MigoGattAddress(service: CBUUID(string: "FFE0"), characteristic: CBUUID(string: "FFE1"))
    }
    private var otherAddress: MigoGattAddress {
        MigoGattAddress(service: CBUUID(string: "FFE0"), characteristic: CBUUID(string: "FFE2"))
    }
    private let central = UUID()

    /// The call's answer; every call here is answered before it returns.
    private func call(_ method: MigoPeripheralServers.Method, _ options: [String: Any]) -> Result<[String: Any], Failure>? {
        var answer: Result<[String: Any], Failure>?
        servers.handle(method, options) { answer = $0 }
        return answer
    }

    private func failure(_ result: Result<[String: Any], Failure>?) -> Failure? {
        if case .failure(let failure)? = result { return failure }
        return nil
    }

    private func succeeded(_ result: Result<[String: Any], Failure>?, file: StaticString = #filePath, line: UInt = #line) {
        guard case .success? = result else { return XCTFail("\(String(describing: result))", file: file, line: line) }
    }

    /// Server 1, serving FFE0 with FFE1 (everything allowed, value 01 02) and FFE2.
    private func serve() {
        radio.isOn = true
        succeeded(call(.createServer, ["serverId": 1]))
        succeeded(call(.addService, ["serverId": 1, "service": described()]))
    }

    private func described() -> [String: Any] {
        let everything: [String: Any] = [
            "properties": ["write": true, "writeNoResponse": true, "read": true, "notify": true, "indicate": true],
            "permission": [
                "readable": true, "writeable": true, "readEncryptionRequired": false, "writeEncryptionRequired": false,
            ],
        ]
        return [
            "uuid": "FFE0",
            "characteristics": [
                everything.merging(["uuid": "FFE1", "value": "0102", "descriptors": [["uuid": "2901", "value": "6869"]]]) { $1 },
                everything.merging(["uuid": "ffe2", "value": ""]) { $1 },
            ],
        ]
    }

    private func told(_ event: MigoPeripheralServers.Event) -> [NSDictionary] {
        events.filter { $0.event == event }.map(\.object)
    }

    // MARK: - servers

    func testAServerNeedsTheAdapterOpenAndBluetoothOn() {
        open = false
        XCTAssertEqual(failure(call(.createServer, ["serverId": 1])), Failure(.notOpened))
        XCTAssertEqual(made, 0, "a closed adapter turns no peripheral role on")
        open = true
        // The system has not said yet: the call waits for it.
        var answer: Result<[String: Any], Failure>?
        servers.handle(.createServer, ["serverId": 1]) { answer = $0 }
        XCTAssertNil(answer)
        radio.isOn = false
        servers.radioChanged()
        XCTAssertEqual(failure(answer), Failure(.unavailable))
        radio.isOn = true
        succeeded(call(.createServer, ["serverId": 1]))
        XCTAssertEqual(made, 1, "one radio, however many servers")
        XCTAssertEqual(failure(call(.createServer, [:])), Failure(.invalidData, "no serverId"))
    }

    func testACallForAServerThatWasNotCreatedFails() {
        radio.isOn = true
        succeeded(call(.createServer, ["serverId": 1]))
        XCTAssertEqual(failure(call(.stopAdvertising, ["serverId": 2])), Failure(.systemError, "no such server"))
    }

    // MARK: - services

    func testAServiceIsServedWithWhatContentSaidOfIt() throws {
        serve()
        let added = try XCTUnwrap(radio.added.first)
        XCTAssertEqual(added.uuid, CBUUID(string: "FFE0"))
        XCTAssertEqual(added.characteristics.map(\.uuid), [CBUUID(string: "FFE1"), CBUUID(string: "FFE2")])
        XCTAssertEqual(added.characteristics[0].properties, [.read, .write, .writeWithoutResponse, .notify, .indicate])
        XCTAssertEqual(added.characteristics[0].permissions, [.readable, .writeable])
        XCTAssertEqual(added.characteristics[0].descriptors.map(\.uuid), [CBUUID(string: "2901")])
        XCTAssertEqual(added.characteristics[0].descriptors.map(\.value), [Data([0x68, 0x69])])
        // A device has one database: the service is served, whichever server asks again.
        succeeded(call(.createServer, ["serverId": 2]))
        XCTAssertEqual(
            failure(call(.addService, ["serverId": 2, "service": described()])),
            Failure(.systemError, "the service is served already"))
    }

    func testAServiceTheSystemRefusesIsNotServedAndCanBeAddedAgain() {
        radio.isOn = true
        succeeded(call(.createServer, ["serverId": 1]))
        radio.refusesServices = true
        XCTAssertEqual(
            failure(call(.addService, ["serverId": 1, "service": described()])), Failure(.systemError, "service not added"))
        radio.refusesServices = false
        succeeded(call(.addService, ["serverId": 1, "service": described()]))
    }

    func testWhatIsNotAUuidOrHexIsInvalidData() {
        radio.isOn = true
        succeeded(call(.createServer, ["serverId": 1]))
        XCTAssertEqual(failure(call(.addService, ["serverId": 1, "service": ["uuid": "nope"]]))?.code, 10013)
        let bad: [String: Any] = ["uuid": "FFE0", "characteristics": [["uuid": "FFE1", "value": "0g"]]]
        XCTAssertEqual(failure(call(.addService, ["serverId": 1, "service": bad]))?.code, 10013)
        XCTAssertTrue(radio.added.isEmpty)
    }

    func testAServiceIsRemovedByItsServerOnly() {
        serve()
        succeeded(call(.createServer, ["serverId": 2]))
        XCTAssertEqual(failure(call(.removeService, ["serverId": 2, "serviceId": service])), Failure(.noService))
        succeeded(call(.removeService, ["serverId": 1, "serviceId": "ffe0"]))
        XCTAssertEqual(radio.removed, [service])
        let write: [String: Any] = ["serverId": 1, "serviceId": service, "characteristicId": characteristic, "value": "00"]
        XCTAssertEqual(failure(call(.writeCharacteristicValue, write)), Failure(.noService))
        // Gone, it can be served again.
        succeeded(call(.addService, ["serverId": 2, "service": described()]))
    }

    // MARK: - reads and writes

    func testAReadIsContentsToAnswerAndOneItDoesNotAnswerGetsNoResponse() throws {
        serve()
        let first = Radio.Request()
        let second = Radio.Request()
        servers.read(first, by: central, of: address, offset: 0)
        servers.read(second, by: central, of: address, offset: 0)
        let requests = told(.readRequest)
        XCTAssertEqual(requests.count, 2)
        XCTAssertEqual(requests[0]["serverId"] as? Int, 1)
        XCTAssertEqual(requests[0]["serviceId"] as? String, service)
        XCTAssertEqual(requests[0]["characteristicId"] as? String, characteristic)
        let callback = try XCTUnwrap(requests[1]["callbackId"] as? Int)
        XCTAssertNotEqual(requests[0]["callbackId"] as? Int, callback)
        XCTAssertTrue(radio.responses.isEmpty, "nothing is answered for content")
        succeeded(
            call(
                .writeCharacteristicValue,
                [
                    "serverId": 1, "serviceId": service, "characteristicId": characteristic, "value": "0a0b0c",
                    "needNotify": false, "callbackId": callback,
                ]))
        XCTAssertEqual(radio.responses.count, 1)
        XCTAssertTrue(radio.responses[0].request === second, "the request the callbackId names")
        XCTAssertEqual(radio.responses[0].value, Data([0x0a, 0x0b, 0x0c]))
        XCTAssertEqual(radio.responses[0].result, .success)
        XCTAssertTrue(radio.notified.isEmpty)
    }

    func testTheRestOfALongReadIsAnsweredFromTheValueContentGave() {
        serve()
        servers.read(Radio.Request(), by: central, of: address, offset: 1)
        XCTAssertTrue(told(.readRequest).isEmpty, "content never sees the rest of a read")
        XCTAssertEqual(radio.responses.last?.value, Data([0x02]))
        servers.read(Radio.Request(), by: central, of: address, offset: 2)
        XCTAssertEqual(radio.responses.last?.value, Data())
        servers.read(Radio.Request(), by: central, of: address, offset: 3)
        XCTAssertEqual(radio.responses.last?.result, .invalidOffset)
        let unknown = MigoGattAddress(service: CBUUID(string: "FFE0"), characteristic: CBUUID(string: "FFEF"))
        servers.read(Radio.Request(), by: central, of: unknown, offset: 0)
        XCTAssertEqual(radio.responses.last?.result, .attributeNotFound)
    }

    func testAWriteIsReportedWholeAndAnsweredOnceEveryRequestOfItIs() throws {
        serve()
        let first = Radio.Request()
        // A long write to FFE1 in two parts, and one to FFE2, in one transaction.
        servers.write(
            [
                MigoGattWrite(request: first, address: address, offset: 0, value: Data([1, 2, 3])),
                MigoGattWrite(request: Radio.Request(), address: address, offset: 3, value: Data([4, 5])),
                MigoGattWrite(request: Radio.Request(), address: otherAddress, offset: 0, value: Data([9])),
            ], by: central)
        let requests = told(.writeRequest)
        XCTAssertEqual(requests.map { $0["characteristicId"] as? String }, [characteristic, other])
        XCTAssertEqual(requests.map { $0["value"] as? String }, ["0102030405", "09"])
        let callbacks = try requests.map { try XCTUnwrap($0["callbackId"] as? Int) }
        func answer(_ callback: Int, _ characteristic: String) {
            succeeded(
                call(
                    .writeCharacteristicValue,
                    ["serverId": 1, "serviceId": service, "characteristicId": characteristic, "value": "", "callbackId": callback]))
        }
        answer(callbacks[0], characteristic)
        XCTAssertTrue(radio.responses.isEmpty, "one of two answered: the write is not")
        answer(callbacks[1], other)
        XCTAssertEqual(radio.responses.count, 1)
        XCTAssertTrue(radio.responses[0].request === first, "a write is answered by its first part")
        XCTAssertEqual(radio.responses[0].result, .success)
        // Answered twice, a callbackId answers nothing more.
        answer(callbacks[1], other)
        XCTAssertEqual(radio.responses.count, 1)
    }

    func testAWriteWithAGapOrToNothingIsRefused() {
        serve()
        servers.write([MigoGattWrite(request: Radio.Request(), address: address, offset: 4, value: Data([1]))], by: central)
        XCTAssertEqual(radio.responses.last?.result, .invalidOffset)
        let unknown = MigoGattAddress(service: CBUUID(string: "FFE0"), characteristic: CBUUID(string: "FFEF"))
        servers.write([MigoGattWrite(request: Radio.Request(), address: unknown, offset: 0, value: Data([1]))], by: central)
        XCTAssertEqual(radio.responses.last?.result, .attributeNotFound)
        XCTAssertTrue(told(.writeRequest).isEmpty)
    }

    func testAValueIsNotifiedInOrderWhenTheSystemHasRoom() {
        serve()
        func notify(_ value: String) {
            succeeded(
                call(
                    .writeCharacteristicValue,
                    ["serverId": 1, "serviceId": service, "characteristicId": characteristic, "value": value, "needNotify": true]))
        }
        radio.room = 1
        notify("01")
        notify("02")
        notify("03")
        XCTAssertEqual(radio.notified.map(\.value), [Data([1])], "what the system had room for")
        radio.room = 1
        servers.readyToNotify()
        XCTAssertEqual(radio.notified.map(\.value), [Data([1]), Data([2])])
        radio.room = 5
        // One sent now goes behind the one still waiting.
        notify("04")
        XCTAssertEqual(radio.notified.map(\.value), [Data([1]), Data([2])])
        servers.readyToNotify()
        XCTAssertEqual(radio.notified.map(\.value), [Data([1]), Data([2]), Data([3]), Data([4])])
        XCTAssertEqual(radio.notified.last?.address, address)
    }

    // MARK: - centrals

    func testACentralIsConnectedWhenFirstSeenAndGoneWhenItsLastSubscriptionEnds() {
        serve()
        servers.subscribed(central, to: address)
        servers.subscribed(central, to: otherAddress)
        servers.read(Radio.Request(), by: central, of: address, offset: 0)
        XCTAssertEqual(told(.subscribed).map { $0["characteristicId"] as? String }, [characteristic, other])
        var changes = told(.connectionChanged)
        XCTAssertEqual(changes.count, 1, "connected once, however much it does")
        XCTAssertEqual(changes[0]["deviceId"] as? String, central.uuidString)
        XCTAssertEqual(changes[0]["connected"] as? Bool, true)
        XCTAssertEqual(changes[0]["serverId"] as? Int, 1)
        servers.unsubscribed(central, from: address)
        XCTAssertEqual(told(.connectionChanged).count, 1, "one subscription of two ended")
        servers.unsubscribed(central, from: otherAddress)
        changes = told(.connectionChanged)
        XCTAssertEqual(changes.count, 2)
        XCTAssertEqual(changes[1]["connected"] as? Bool, false)
        XCTAssertEqual(told(.unsubscribed).count, 2)
        // Back, it is connected again.
        servers.read(Radio.Request(), by: central, of: address, offset: 0)
        XCTAssertEqual(told(.connectionChanged).last?["connected"] as? Bool, true)
    }

    // MARK: - advertising

    func testAnAdvertisementCarriesANameAndServicesAndReplacesTheOneBefore() {
        serve()
        succeeded(call(.createServer, ["serverId": 2]))
        let request: [String: Any] = [
            "connectable": true, "deviceName": "Migo", "serviceUuids": ["FFE0"],
            "manufacturerData": [["manufacturerId": "0x004c", "manufacturerSpecificData": "01"]],
        ]
        succeeded(call(.startAdvertising, ["serverId": 1, "advertiseRequest": request, "powerLevel": "high"]))
        XCTAssertEqual(radio.advertisements.count, 1)
        XCTAssertEqual(radio.advertisements[0].name, "Migo")
        XCTAssertEqual(radio.advertisements[0].services, [CBUUID(string: "FFE0")])
        XCTAssertEqual(radio.stops, 0)
        // Another server's stop is not this one's.
        succeeded(call(.stopAdvertising, ["serverId": 2]))
        XCTAssertEqual(radio.stops, 0)
        // One advertisement to a device: the second server's takes its place.
        succeeded(call(.startAdvertising, ["serverId": 2, "advertiseRequest": ["deviceName": ""]]))
        XCTAssertEqual(radio.stops, 1)
        XCTAssertNil(radio.advertisements[1].name)
        succeeded(call(.stopAdvertising, ["serverId": 1]))
        XCTAssertEqual(radio.stops, 1, "the first no longer advertises")
        succeeded(call(.stopAdvertising, ["serverId": 2]))
        XCTAssertEqual(radio.stops, 2)
    }

    func testABeaconAndAnAdvertisementTheSystemRefusesFail() {
        serve()
        let beacon: [String: Any] = ["beacon": ["uuid": "E2C56DB5-DFFB-48D2-B060-D0F5A71096E0", "major": 1, "minor": 2]]
        XCTAssertEqual(failure(call(.startAdvertising, ["serverId": 1, "advertiseRequest": beacon]))?.code, 10007)
        XCTAssertEqual(
            failure(call(.startAdvertising, ["serverId": 1, "advertiseRequest": ["serviceUuids": ["zz"]]]))?.code, 10013)
        radio.refusesAdvertising = true
        XCTAssertEqual(
            failure(call(.startAdvertising, ["serverId": 1, "advertiseRequest": [:] as [String: Any]])),
            Failure(.systemError, "the advertisement did not start"))
        XCTAssertTrue(radio.advertisements.isEmpty)
    }

    // MARK: - going away

    func testClosingTakesTheServersAndWhatTheyServed() {
        serve()
        succeeded(call(.startAdvertising, ["serverId": 1, "advertiseRequest": [:] as [String: Any]]))
        servers.read(Radio.Request(), by: central, of: address, offset: 0)
        servers.close()
        XCTAssertEqual(radio.removedAll, 1)
        XCTAssertEqual(radio.stops, 1)
        XCTAssertEqual(failure(call(.stopAdvertising, ["serverId": 1])), Failure(.systemError, "no such server"))
        // What a central asks of a closed server is nothing content hears.
        let before = events.count
        servers.read(Radio.Request(), by: central, of: address, offset: 0)
        XCTAssertEqual(events.count, before)
        XCTAssertEqual(radio.responses.last?.result, .attributeNotFound)
    }

    func testBluetoothGoingOffDropsWhatWasServedAndKeepsTheServers() {
        serve()
        radio.isOn = false
        servers.radioChanged()
        XCTAssertEqual(failure(call(.addService, ["serverId": 1, "service": described()])), Failure(.unavailable))
        radio.isOn = true
        servers.radioChanged()
        // The server is content's still; what it served is to be served again.
        succeeded(call(.addService, ["serverId": 1, "service": described()]))
        XCTAssertEqual(radio.added.count, 2)
    }
}

final class MigoPeripheralManagerRadioTests: XCTestCase {
    func testADescriptorIsMadeAsTheSystemServesOne() throws {
        let description = try XCTUnwrap(
            MigoPeripheralManagerRadio.descriptor((CBUUID(string: "2901"), Data("speed".utf8))))
        XCTAssertEqual(description.value as? String, "speed", "a user description is a string")
        let format = try XCTUnwrap(
            MigoPeripheralManagerRadio.descriptor((CBUUID(string: "2904"), Data([0x04, 0, 0x27, 0x27, 1, 0, 0]))))
        XCTAssertEqual((format.value as? Data)?.count, 7)
        XCTAssertNil(MigoPeripheralManagerRadio.descriptor((CBUUID(string: "2904"), Data([1]))), "not a format's seven bytes")
        let own = try XCTUnwrap(
            MigoPeripheralManagerRadio.descriptor((CBUUID(string: "6E400004-B5A3-F393-E0A9-E50E24DCCA9E"), Data([7]))))
        XCTAssertEqual(own.value as? Data, Data([7]))
        // The client configuration a subscription sets, and the others of Bluetooth's own: the system's to make.
        for uuid in ["2900", "2902", "2903", "2905"] {
            XCTAssertNil(MigoPeripheralManagerRadio.descriptor((CBUUID(string: uuid), Data([0, 0]))), uuid)
        }
    }
}

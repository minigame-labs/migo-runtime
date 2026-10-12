import Foundation
import MigoEngine
import XCTest

@testable import MigoAppleRenderer

#if os(macOS)
    import AppKit
#endif

/// The SDK's own services, and how they sit beside the app's.
final class MigoBuiltInHostServiceTests: XCTestCase {
    private final class Recorder: @unchecked Sendable {
        var outcomes: [MigoHostServiceReply.Outcome] = []
    }

    private final class AppProvider: MigoHostServiceProvider {
        let services: Set<MigoHostServiceID>
        var handled: [MigoHostServiceID] = []
        init(_ services: Set<MigoHostServiceID>) { self.services = services }
        func handle(_ request: MigoHostServiceRequest) {
            handled.append(request.service)
            request.reply?.succeed(["from": "app"])
        }
    }

    private func request(
        _ service: MigoHostServiceID, _ method: UInt32, _ object: [String: Any] = [:], into recorder: Recorder
    ) -> MigoHostServiceRequest {
        MigoHostServiceRequest(
            service: service, method: method, payload: try! JSONSerialization.data(withJSONObject: object),
            reply: MigoHostServiceReply { recorder.outcomes.append($0) })
    }

    func testTheSessionDeclaresTheAppsServicesAndTheSdksOwn() {
        let composed = MigoComposedHostServices(app: AppProvider([.ad, .payment]))
        XCTAssertEqual(composed.services, MigoBuiltInHostServices.services.union([.ad, .payment]))
        XCTAssertTrue(MigoBuiltInHostServices.services.isSuperset(of: [.clipboard, .interaction, .screen]))
        XCTAssertEqual(MigoComposedHostServices(app: nil).services, MigoBuiltInHostServices.services)
    }

    func testAServiceTheAppNamesIsTheAppsEvenWhereTheSdkHasOne() {
        let app = AppProvider([.clipboard])
        let composed = MigoComposedHostServices(app: app)
        let recorder = Recorder()
        composed.handle(request(.clipboard, MigoHostServiceID.Clipboard.getClipboardData, into: recorder))
        XCTAssertEqual(app.handled, [.clipboard])
        guard case .success(let json) = recorder.outcomes.first else { return XCTFail("\(recorder.outcomes)") }
        XCTAssertEqual(
            (try? JSONSerialization.jsonObject(with: json)) as? [String: String], ["from": "app"])
    }

    /// A capability that ranges beacons answers the Bluetooth service's three
    /// beacon methods -- and declares the service, so they reach it -- while
    /// the rest of the service stays with whoever answers it, and a service the
    /// app names stays the app's.
    func testTheBeaconMethodsGoToTheCapabilityThatRangesBeacons() {
        class Capability: MigoSystemCapability {
            let services: Set<MigoHostServiceID>
            let scopes: Set<MigoScope> = []
            var handled: [UInt32] = []
            init(_ services: Set<MigoHostServiceID>) { self.services = services }
            func report(to reports: MigoCapabilityReports?) {}
            func requestScope(_ scope: MigoScope, completion: @escaping (Bool) -> Void) { completion(false) }
            func handle(_ request: MigoHostServiceRequest) {
                handled.append(request.method)
                request.reply?.succeed()
            }
        }
        final class Ranging: Capability, MigoBeaconRanging {
            var ranged: [UInt32] = []
            func handleBeacons(_ request: MigoHostServiceRequest) {
                ranged.append(request.method)
                request.reply?.succeed()
            }
        }
        let ranging = Ranging([.location])
        let bluetooth = Capability([.bluetooth])
        let composed = MigoComposedHostServices(app: nil, capabilities: [bluetooth, ranging])
        XCTAssertTrue(composed.services.contains(.bluetooth))
        let recorder = Recorder()
        typealias Bluetooth = MigoHostServiceID.Bluetooth
        for method in [Bluetooth.startBeaconDiscovery, Bluetooth.getBeacons, Bluetooth.openAdapter, Bluetooth.stopBeaconDiscovery] {
            composed.handle(request(.bluetooth, method, into: recorder))
        }
        XCTAssertEqual(ranging.ranged, [Bluetooth.startBeaconDiscovery, Bluetooth.getBeacons, Bluetooth.stopBeaconDiscovery])
        XCTAssertEqual(bluetooth.handled, [Bluetooth.openAdapter])

        // Ranging alone still offers the service; the rest of it is nobody's.
        let alone = MigoComposedHostServices(app: nil, capabilities: [Ranging([.location])])
        XCTAssertTrue(alone.services.contains(.bluetooth))
        let refused = Recorder()
        alone.handle(request(.bluetooth, Bluetooth.openAdapter, into: refused))
        XCTAssertEqual(refused.outcomes, [.failure(message: "not supported", code: nil)])

        // The app's Bluetooth is the app's, beacons included.
        let app = AppProvider([.bluetooth])
        let appsOwn = MigoComposedHostServices(app: app, capabilities: [ranging])
        appsOwn.handle(request(.bluetooth, Bluetooth.getBeacons, into: Recorder()))
        XCTAssertEqual(app.handled, [.bluetooth])
        XCTAssertEqual(ranging.ranged.count, 3, "not handed to the capability")
    }

    func testAServiceNobodySuppliesIsRefused() {
        let recorder = Recorder()
        MigoComposedHostServices(app: nil).handle(request(.scanCode, MigoHostServiceID.ScanCode.scanCode, into: recorder))
        XCTAssertEqual(recorder.outcomes, [.failure(message: "not supported", code: nil)])
    }

    #if os(macOS)
        func testTheClipboardIsTheSystemPasteboard() throws {
            // The pasteboard is the person's: what was there goes back.
            let saved = NSPasteboard.general.string(forType: .string)
            defer {
                NSPasteboard.general.clearContents()
                if let saved { NSPasteboard.general.setString(saved, forType: .string) }
            }
            let composed = MigoComposedHostServices(app: nil)
            let recorder = Recorder()
            composed.handle(
                request(.clipboard, MigoHostServiceID.Clipboard.setClipboardData, ["data": "migo-built-in"], into: recorder))
            XCTAssertEqual(NSPasteboard.general.string(forType: .string), "migo-built-in")
            composed.handle(request(.clipboard, MigoHostServiceID.Clipboard.getClipboardData, into: recorder))
            composed.handle(
                request(.clipboard, MigoHostServiceID.Clipboard.setClipboardData, ["data": 7], into: recorder))
            XCTAssertEqual(recorder.outcomes.count, 3)
            XCTAssertEqual(recorder.outcomes[0], .success(json: Data("{}".utf8)))
            guard case .success(let json) = recorder.outcomes[1] else { return XCTFail("\(recorder.outcomes)") }
            XCTAssertEqual(
                (try JSONSerialization.jsonObject(with: json)) as? [String: String], ["data": "migo-built-in"])
            XCTAssertEqual(recorder.outcomes[2], .failure(message: "data is not a string", code: nil))
        }
    #endif
}

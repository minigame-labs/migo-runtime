import Foundation
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

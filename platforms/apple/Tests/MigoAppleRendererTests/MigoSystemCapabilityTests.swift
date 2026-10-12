import Foundation
import MigoEngine
import XCTest

@testable import MigoAppleRenderer

/// The optional parts of the system an app links: where their services sit
/// among the app's and the SDK's, how content's scope requests reach the one
/// that decides each, and what they report through and when.
final class MigoSystemCapabilityTests: XCTestCase {
    private final class Recorder: @unchecked Sendable {
        var outcomes: [MigoHostServiceReply.Outcome] = []
    }

    private final class Capability: MigoSystemCapability {
        let services: Set<MigoHostServiceID>
        let scopes: Set<MigoScope>
        var decision = false
        var handled: [MigoHostServiceID] = []
        var asked: [MigoScope] = []
        var reports: [MigoCapabilityReports?] = []

        init(_ services: Set<MigoHostServiceID>, scopes: Set<MigoScope> = []) {
            self.services = services
            self.scopes = scopes
        }

        func handle(_ request: MigoHostServiceRequest) {
            handled.append(request.service)
            request.reply?.succeed()
        }

        func report(to reports: MigoCapabilityReports?) { self.reports.append(reports) }

        func requestScope(_ scope: MigoScope, completion: @escaping (Bool) -> Void) {
            asked.append(scope)
            completion(decision)
        }
    }

    private final class App: MigoHostServiceProvider {
        let services: Set<MigoHostServiceID>
        var handled: [MigoHostServiceID] = []
        init(_ services: Set<MigoHostServiceID>) { self.services = services }
        func handle(_ request: MigoHostServiceRequest) { handled.append(request.service) }
    }

    private let location = MigoScope(MIGO_SCOPE_USER_LOCATION)
    private let camera = MigoScope(MIGO_SCOPE_CAMERA)

    private func request(
        _ service: MigoHostServiceID, _ method: UInt32, _ object: [String: Any] = [:], into recorder: Recorder
    ) -> MigoHostServiceRequest {
        MigoHostServiceRequest(
            service: service, method: method, payload: try! JSONSerialization.data(withJSONObject: object),
            reply: MigoHostServiceReply { recorder.outcomes.append($0) })
    }

    func testTheAppComesFirstThenTheCapabilityThenTheSDK() {
        let app = App([.ad])
        let capability = Capability([.location, .ad])
        let composed = MigoComposedHostServices(app: app, capabilities: [capability])
        XCTAssertEqual(composed.services, MigoBuiltInHostServices.services.union([.ad, .location]))
        let recorder = Recorder()
        composed.handle(request(.ad, 0, into: recorder))
        composed.handle(request(.location, MigoHostServiceID.Location.getLocation, into: recorder))
        composed.handle(request(.clipboard, MigoHostServiceID.Clipboard.getClipboardData, into: recorder))
        XCTAssertEqual(app.handled, [.ad])
        XCTAssertEqual(capability.handled, [.location])
    }

    func testThePermissionServiceIsTheSDKsOnlyWhenACapabilityDecidesAScope() {
        XCTAssertFalse(
            MigoComposedHostServices(app: nil, capabilities: [Capability([.location])]).services.contains(.permission))
        XCTAssertTrue(
            MigoComposedHostServices(app: nil, capabilities: [Capability([.location], scopes: [location])]).services
                .contains(.permission))
    }

    func testAScopeRequestReachesTheCapabilityThatDecidesIt() {
        let capability = Capability([.location], scopes: [location])
        let composed = MigoComposedHostServices(app: nil, capabilities: [capability])
        let recorder = Recorder()
        let ask = MigoHostServiceID.Permission.requestScope
        capability.decision = true
        composed.handle(request(.permission, ask, ["scope": "scope.userLocation", "scopeId": location], into: recorder))
        capability.decision = false
        composed.handle(request(.permission, ask, ["scope": "scope.userLocation", "scopeId": location], into: recorder))
        // A scope no capability decides is nobody's here, and an unnumbered
        // request names none.
        composed.handle(request(.permission, ask, ["scope": "scope.camera", "scopeId": camera], into: recorder))
        composed.handle(request(.permission, ask, ["scope": "scope.userLocation"], into: recorder))
        XCTAssertEqual(capability.asked, [location, location])
        XCTAssertEqual(
            recorder.outcomes,
            [.success(json: Data("{}".utf8))]
                + Array(repeating: .failure(message: "auth deny", code: nil), count: 3))
    }

    func testTheAppsPermissionServiceDecidesEvenAScopeACapabilityCould() {
        let app = App([.permission])
        let capability = Capability([.location], scopes: [location])
        let composed = MigoComposedHostServices(app: app, capabilities: [capability])
        let recorder = Recorder()
        composed.handle(
            request(
                .permission, MigoHostServiceID.Permission.requestScope, ["scopeId": location], into: recorder))
        XCTAssertEqual(app.handled, [.permission])
        XCTAssertEqual(capability.asked, [])
    }

    private func session(app: MigoHostServiceProvider?, capabilities: [MigoSystemCapability]) throws
        -> (MigoEngineSession, URL)
    {
        let root = FileManager.default.temporaryDirectory
            .appendingPathComponent("migo-capabilities-\(UUID().uuidString)", isDirectory: true)
        let session = try MigoEngineSession(
            directories: .init(
                files: root.appendingPathComponent("files"), cache: root.appendingPathComponent("cache"),
                codeCache: root.appendingPathComponent("code-cache")),
            contentSigning: .unsigned, hostServices: app, capabilities: capabilities)
        return (session, root)
    }

    func testACapabilityReportsWhileTheSessionIsOpenAndDecidesScopesUnlessTheAppDoes() throws {
        let mine = Capability([.location], scopes: [location])
        let (session, root) = try session(app: nil, capabilities: [mine])
        defer { try? FileManager.default.removeItem(at: root) }
        XCTAssertEqual(mine.reports.count, 1)
        XCTAssertEqual(mine.reports.first??.decidesScopes, true)
        session.close()
        XCTAssertEqual(mine.reports.count, 2)
        XCTAssertNil(mine.reports.last!)

        let theirs = Capability([.location], scopes: [location])
        let (other, otherRoot) = try self.session(app: App([.permission]), capabilities: [theirs])
        defer { try? FileManager.default.removeItem(at: otherRoot) }
        XCTAssertEqual(theirs.reports.first??.decidesScopes, false)
        other.close()
    }
}

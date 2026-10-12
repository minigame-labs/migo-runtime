import Foundation
import XCTest

@testable import MigoAppleRenderer

/// The settings pages: the system's page for this app, opened for
/// openAppAuthorizeSetting always and for openSetting only while the SDK
/// decides content's scopes, each answered when the player comes back -- after
/// the capabilities report what the player left.
final class MigoSystemSettingsTests: XCTestCase {
    private final class Recorder: @unchecked Sendable {
        var outcomes: [MigoHostServiceReply.Outcome] = []
        var opened: [URL] = []
        var events: [String] = []
    }

    private let returned = Notification.Name("MigoSystemSettingsTests.returned")

    private func settings(decidesScopes: Bool, opens: Bool = true, _ recorder: Recorder) -> MigoSystemSettings {
        let settings = MigoSystemSettings(
            decidesScopes: decidesScopes,
            open: { url, completion in
                recorder.opened.append(url)
                completion(opens)
            }, returned: returned)
        settings.refresh = { recorder.events.append("refresh") }
        return settings
    }

    private func request(_ method: UInt32, _ recorder: Recorder) -> MigoHostServiceRequest {
        MigoHostServiceRequest(
            service: .setting, method: method, payload: Data("{}".utf8),
            reply: MigoHostServiceReply {
                recorder.outcomes.append($0)
                recorder.events.append("answer")
            })
    }

    func testTheAppsPageIsOpenedAndAnsweredWhenThePlayerReturns() {
        let recorder = Recorder()
        let settings = settings(decidesScopes: false, recorder)
        settings.handle(request(MigoHostServiceID.Setting.openAppAuthorizeSetting, recorder))
        XCTAssertEqual(recorder.opened, [MigoSystemSettings.appSettings])
        XCTAssertTrue(recorder.outcomes.isEmpty, "answered before the player came back")
        NotificationCenter.default.post(name: returned, object: nil)
        XCTAssertEqual(recorder.outcomes, [.success(json: Data("{}".utf8))])
        // What the player changed is reported before content reads it.
        XCTAssertEqual(recorder.events, ["refresh", "answer"])
        NotificationCenter.default.post(name: returned, object: nil)
        XCTAssertEqual(recorder.outcomes.count, 1)
    }

    func testOpenSettingIsTheSystemsPageOnlyWhileTheSDKDecidesTheScopes() {
        let recorder = Recorder()
        settings(decidesScopes: false, recorder).handle(request(MigoHostServiceID.Setting.openSetting, recorder))
        XCTAssertEqual(recorder.outcomes, [.failure(message: "not supported", code: nil)])
        XCTAssertTrue(recorder.opened.isEmpty)

        let deciding = settings(decidesScopes: true, recorder)
        deciding.handle(request(MigoHostServiceID.Setting.openSetting, recorder))
        XCTAssertEqual(recorder.opened.count, 1)
        NotificationCenter.default.post(name: returned, object: nil)
        XCTAssertEqual(recorder.outcomes.last, .success(json: Data("{}".utf8)))
    }

    func testBluetoothSettingsAndAPageThatWouldNotOpenFail() {
        let recorder = Recorder()
        settings(decidesScopes: true, recorder)
            .handle(request(MigoHostServiceID.Setting.openSystemBluetoothSetting, recorder))
        settings(decidesScopes: true, opens: false, recorder)
            .handle(request(MigoHostServiceID.Setting.openAppAuthorizeSetting, recorder))
        XCTAssertEqual(
            recorder.outcomes,
            [
                .failure(message: "not supported", code: nil),
                .failure(message: "the settings could not be opened", code: nil),
            ])
    }
}

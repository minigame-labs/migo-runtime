import Foundation
import MigoEngine
import XCTest

@testable import MigoAppleRenderer

/// The Swift half of the host-service channel: what a reply hands the engine,
/// the numbers it declares, and a session that declares them.
final class MigoHostServiceTests: XCTestCase {
    private final class Recorder: @unchecked Sendable {
        var outcomes: [MigoHostServiceReply.Outcome] = []
    }

    private func reply(_ recorder: Recorder) -> MigoHostServiceReply {
        MigoHostServiceReply { recorder.outcomes.append($0) }
    }

    func testASuccessIsTheResultObjectAndOnlyTheFirstAnswerCounts() throws {
        let recorder = Recorder()
        let reply = reply(recorder)
        reply.succeed(["data": "copied", "count": 2])
        reply.fail("too late")
        XCTAssertEqual(recorder.outcomes.count, 1)
        guard case .success(let json) = recorder.outcomes[0] else {
            return XCTFail("\(recorder.outcomes)")
        }
        let object = try XCTUnwrap(JSONSerialization.jsonObject(with: json) as? [String: Any])
        XCTAssertEqual(object["data"] as? String, "copied")
        XCTAssertEqual(object["count"] as? Int, 2)
    }

    func testAFailureCarriesItsReasonAndTheApiCodeWhenThereIsOne() {
        let recorder = Recorder()
        reply(recorder).fail("cancel")
        reply(recorder).fail("template not found", code: 20001)
        // A failure must say why; an empty reason is not one.
        reply(recorder).fail("")
        XCTAssertEqual(
            recorder.outcomes,
            [
                .failure(message: "cancel", code: nil),
                .failure(message: "template not found", code: 20001),
                .failure(message: "failed", code: nil),
            ])
    }

    func testAReplyDroppedUnansweredAnswersItselfAsAFailure() {
        let recorder = Recorder()
        do {
            _ = reply(recorder)
        }
        XCTAssertEqual(recorder.outcomes, [.failure(message: "the host did not answer", code: nil)])
    }

    func testAResultThatIsNotJsonFailsTheCallRatherThanCrossing() {
        let recorder = Recorder()
        reply(recorder).succeed(["when": Date()])
        XCTAssertEqual(recorder.outcomes, [.failure(message: "the host's result is not JSON", code: nil)])
    }

    func testTheServicesAreTheHeadersNumbersOneBitEach() {
        // Every number up to the header's newest service, none missing: a count
        // written here went stale three services ago.
        let count = Int(MIGO_HOST_SERVICE_BLE_PERIPHERAL) + 1
        XCTAssertEqual(MigoHostServiceID.all.count, count)
        XCTAssertEqual(MigoHostServiceID.all.map(\.rawValue), Array(0..<count).map(UInt32.init))
        XCTAssertEqual(MigoHostServiceID.update.rawValue, UInt32(MIGO_HOST_SERVICE_UPDATE))
        XCTAssertEqual(MigoHostServiceID.blePeripheral.rawValue, UInt32(MIGO_HOST_SERVICE_BLE_PERIPHERAL))
        XCTAssertEqual(Set(MigoHostServiceID.all.map(\.bit)).count, count)
    }

    private final class Provider: MigoHostServiceProvider {
        let services: Set<MigoHostServiceID>
        init(_ services: Set<MigoHostServiceID>) { self.services = services }
        func handle(_ request: MigoHostServiceRequest) {}
    }

    func testASessionDeclaresTheServicesItsProviderAnswers() throws {
        let root = FileManager.default.temporaryDirectory
            .appendingPathComponent("migo-host-services-\(UUID().uuidString)", isDirectory: true)
        defer { try? FileManager.default.removeItem(at: root) }
        let session = try MigoEngineSession(
            directories: .init(
                files: root.appendingPathComponent("files"), cache: root.appendingPathComponent("cache"),
                codeCache: root.appendingPathComponent("code-cache")),
            contentSigning: .unsigned,
            hostServices: Provider([.clipboard, .interaction, .update]))
        // The engine accepted the declaration; posting an event of a declared
        // service is something it takes from this host.
        session.postHostServiceEvent(.update, event: UInt32(MIGO_UPDATE_EVENT_STATE), json: Data(#"{"event":"updateReady"}"#.utf8))
        session.close()
    }
}

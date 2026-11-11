import AVFoundation
import MigoEngine
import XCTest

@testable import MigoAppleMicrophone
@testable import MigoAppleRenderer

final class MigoMicrophoneTests: XCTestCase {
    func testItAnswersTheRecorderAndDecidesTheRecordScope() {
        let microphone = MigoMicrophone()
        XCTAssertEqual(microphone.services, [.recorder])
        XCTAssertEqual(MigoMicrophone.services, [.recorder])
        XCTAssertEqual(microphone.scopes, [MigoScope(MIGO_SCOPE_RECORD)])
    }

    func testTheSystemsGrantIsReportedInTheEnginesWords() {
        XCTAssertEqual(MigoMicrophone.authorization(.authorized), UInt8(MIGO_AUTHORIZATION_AUTHORIZED))
        XCTAssertEqual(MigoMicrophone.authorization(.denied), UInt8(MIGO_AUTHORIZATION_DENIED))
        XCTAssertEqual(MigoMicrophone.authorization(.restricted), UInt8(MIGO_AUTHORIZATION_DENIED))
        XCTAssertEqual(MigoMicrophone.authorization(.notDetermined), UInt8(MIGO_AUTHORIZATION_NOT_DETERMINED))
        XCTAssertEqual(MigoMicrophone.scopeState(.authorized), MigoScopeState(MIGO_SCOPE_STATE_GRANTED))
        XCTAssertEqual(MigoMicrophone.scopeState(.denied), MigoScopeState(MIGO_SCOPE_STATE_DENIED))
        XCTAssertEqual(MigoMicrophone.scopeState(.notDetermined), MigoScopeState(MIGO_SCOPE_STATE_UNKNOWN))
    }

    func testAnotherScopeIsNotItsToDecide() {
        let refused = expectation(description: "refused at once")
        MigoMicrophone().requestScope(MigoScope(MIGO_SCOPE_CAMERA)) { granted in
            XCTAssertFalse(granted)
            refused.fulfill()
        }
        wait(for: [refused], timeout: 1)
    }

    func testACallIsNotOneOfTheRecordersCommands() {
        // The recorder's methods take no answer; a request that waits for one is told so, not left waiting.
        let answered = expectation(description: "answered")
        let reply = MigoHostServiceReply { outcome in
            XCTAssertEqual(outcome, .failure(message: "not supported", code: nil))
            answered.fulfill()
        }
        MigoMicrophone().handle(
            MigoHostServiceRequest(service: .recorder, method: MigoHostServiceID.Recorder.start, payload: Data("{}".utf8), reply: reply))
        wait(for: [answered], timeout: 1)
    }
}

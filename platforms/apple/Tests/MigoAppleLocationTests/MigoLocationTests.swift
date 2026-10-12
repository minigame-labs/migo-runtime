import CoreLocation
import Foundation
import MigoEngine
import XCTest

@testable import MigoAppleLocation
@testable import MigoAppleRenderer

/// Location without a location: when each waiting call is answered from the
/// fixes CoreLocation sends, what the answer says, and the system's grant in
/// the engine's words.
final class MigoLocationTests: XCTestCase {
    private final class Recorder: @unchecked Sendable {
        var outcomes: [MigoHostServiceReply.Outcome] = []
    }

    private let now = Date(timeIntervalSinceReferenceDate: 800_000_000)

    private func fix(accuracy: CLLocationAccuracy, age: TimeInterval = 0, latitude: Double = 39.9) -> CLLocation {
        CLLocation(
            coordinate: CLLocationCoordinate2D(latitude: latitude, longitude: 116.4), altitude: 50,
            horizontalAccuracy: accuracy, verticalAccuracy: 8, course: -1, speed: -1,
            timestamp: now.addingTimeInterval(-age))
    }

    private func request(goal: CLLocationAccuracy, in seconds: TimeInterval, _ recorder: Recorder)
        -> MigoLocationRequests.Request
    {
        MigoLocationRequests.Request(
            reply: MigoHostServiceReply { recorder.outcomes.append($0) }, goal: goal,
            deadline: now.addingTimeInterval(seconds), altitude: false)
    }

    func testAFreshFixAsAccurateAsAskedAnswersAtOnce() {
        let recorder = Recorder()
        var requests = MigoLocationRequests()
        requests.add(request(goal: 100, in: 10, recorder))
        requests.add(request(goal: 20, in: 10, recorder))
        let answered = requests.offer(fix(accuracy: 65), now: now)
        XCTAssertEqual(answered.map(\.1.horizontalAccuracy), [65])
        XCTAssertEqual(requests.pending.map(\.goal), [20])
        // An invalid fix is no fix at all.
        XCTAssertTrue(requests.offer(fix(accuracy: -1), now: now).isEmpty)
        XCTAssertEqual(requests.pending.first?.best?.horizontalAccuracy, 65)
    }

    func testARememberedFixAnswersNothingButIsKeptForWantOfAFreshOne() {
        let recorder = Recorder()
        var requests = MigoLocationRequests()
        requests.add(request(goal: 100, in: 10, recorder))
        XCTAssertTrue(requests.offer(fix(accuracy: 5, age: 600), now: now).isEmpty)
        XCTAssertEqual(requests.pending.first?.best?.horizontalAccuracy, 5)
        // A fresh one is better, however much less accurate.
        XCTAssertTrue(requests.offer(fix(accuracy: 300), now: now).isEmpty)
        XCTAssertEqual(requests.pending.first?.best?.horizontalAccuracy, 300)
        XCTAssertTrue(requests.offer(fix(accuracy: 5, age: 600), now: now).isEmpty)
        XCTAssertEqual(requests.pending.first?.best?.horizontalAccuracy, 300)
    }

    func testAtItsDeadlineACallTakesTheBestItSawOrNothing() {
        let recorder = Recorder()
        var requests = MigoLocationRequests()
        // Waits it out: the best in its time, not the first good one.
        requests.add(request(goal: 0, in: 3, recorder))
        requests.add(request(goal: 0, in: 10, recorder))
        XCTAssertTrue(requests.offer(fix(accuracy: 30), now: now).isEmpty)
        XCTAssertTrue(requests.offer(fix(accuracy: 10), now: now).isEmpty)
        XCTAssertEqual(requests.nextDeadline, now.addingTimeInterval(3))
        XCTAssertTrue(requests.expire(now: now.addingTimeInterval(2)).isEmpty)
        let expired = requests.expire(now: now.addingTimeInterval(3))
        XCTAssertEqual(expired.map { $0.1?.horizontalAccuracy }, [10])
        XCTAssertEqual(requests.pending.count, 1)

        var empty = MigoLocationRequests()
        empty.add(request(goal: 100, in: 1, recorder))
        XCTAssertEqual(empty.expire(now: now.addingTimeInterval(1)).map { $0.1 == nil }, [true])
        XCTAssertTrue(empty.isEmpty)
    }

    func testTheAnswersSayWhatCoreLocationKnowsAndNoMore() {
        let answer = MigoLocationRequests.answer(fix(accuracy: 12), altitude: false)
        XCTAssertEqual(answer["latitude"] as? Double, 39.9)
        XCTAssertEqual(answer["longitude"] as? Double, 116.4)
        // An unknown speed is 0, and altitude only when asked.
        XCTAssertEqual(answer["speed"] as? Double, 0)
        XCTAssertEqual(answer["altitude"] as? Double, 0)
        XCTAssertEqual(answer["verticalAccuracy"] as? Double, 0)
        XCTAssertEqual(answer["accuracy"] as? Double, 12)
        XCTAssertEqual(answer["horizontalAccuracy"] as? Double, 12)
        let high = MigoLocationRequests.answer(fix(accuracy: 12), altitude: true)
        XCTAssertEqual(high["altitude"] as? Double, 50)
        XCTAssertEqual(high["verticalAccuracy"] as? Double, 8)
        XCTAssertEqual(Set(MigoLocationRequests.fuzzyAnswer(fix(accuracy: 12)).keys), ["latitude", "longitude"])
    }

    func testTheGrantInTheEnginesWords() {
        XCTAssertEqual(MigoLocation.authorization(.notDetermined), UInt8(MIGO_AUTHORIZATION_NOT_DETERMINED))
        XCTAssertEqual(MigoLocation.authorization(.denied), UInt8(MIGO_AUTHORIZATION_DENIED))
        XCTAssertEqual(MigoLocation.authorization(.restricted), UInt8(MIGO_AUTHORIZATION_DENIED))
        XCTAssertEqual(MigoLocation.authorization(.authorizedAlways), UInt8(MIGO_AUTHORIZATION_AUTHORIZED))
        #if os(iOS)
            XCTAssertEqual(MigoLocation.authorization(.authorizedWhenInUse), UInt8(MIGO_AUTHORIZATION_AUTHORIZED))
        #endif
        XCTAssertEqual(MigoLocation.scopeState(.notDetermined), MigoScopeState(MIGO_SCOPE_STATE_UNKNOWN))
        XCTAssertEqual(MigoLocation.scopeState(.denied), MigoScopeState(MIGO_SCOPE_STATE_DENIED))
        XCTAssertEqual(MigoLocation.scopeState(.authorizedAlways), MigoScopeState(MIGO_SCOPE_STATE_GRANTED))
    }

    func testItDecidesOnlyTheLocationScopeAndAnswersOnlyLocation() {
        let location = MigoLocation()
        var answers: [Bool] = []
        location.requestScope(MigoScope(MIGO_SCOPE_CAMERA)) { answers.append($0) }
        XCTAssertEqual(answers, [false])
        XCTAssertEqual(location.scopes, [MigoScope(MIGO_SCOPE_USER_LOCATION)])
        XCTAssertEqual(location.services, [.location])
    }
}

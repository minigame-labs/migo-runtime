import QuartzCore
import XCTest

@testable import MigoAppleCore

/// Does the link actually deliver a tick?
///
/// Nothing in this repository had ever asked. `MigoDisplayLinkPolicyTests` covers
/// what the policy decides and how the link handles its own lifetime, and its one
/// start test says so in its own comment: it is "honest about being one: on a
/// machine with no active display no link is created, so this exercises the paths
/// that lead to the attempt rather than the link itself." Nothing then observed
/// the delivery path -- the callback, the timestamp arithmetic, the hop to the
/// main queue -- and it shipped that way.
///
/// It is observable, and on more machines than the note implies. Measured
/// 2026-09-10: on the Intel Mac this project uses, reached over ssh, a
/// `CVDisplayLink` created with `CreateWithActiveCGDisplays` delivered 120
/// callbacks in 1.983 s -- 60.0 Hz. This test then ran, rather than skipped, on a
/// GitHub `macos-15` runner in 0.635 s. So the assumption that CI is headless was
/// itself wrong, and the delivery path is now covered on every pull request.
///
/// The skip stays, for the machine that genuinely has no display. It states which
/// machine could not answer; a passing assertion that never ran states nothing.
final class MigoDisplayLinkDeliveryTests: XCTestCase {

    /// Ticks arrive on the main queue and are read from the test's own thread,
    /// which XCTest also runs on -- but only between run-loop turns, so the
    /// recorder is still crossing a boundary and is locked accordingly.
    private final class Ticks: @unchecked Sendable {
        private let lock = NSLock()
        private var frames: [MigoDisplayLink.Frame] = []
        /// `CACurrentMediaTime` as each frame was delivered, to hold its times against.
        private var deliveredAt: [CFTimeInterval] = []
        private var expectation: XCTestExpectation?
        private var wanted = 0

        func expect(_ count: Int, _ expectation: XCTestExpectation) {
            lock.lock()
            defer { lock.unlock() }
            wanted = count
            self.expectation = expectation
        }

        func record(_ frame: MigoDisplayLink.Frame) {
            let now = CACurrentMediaTime()
            lock.lock()
            frames.append(frame)
            deliveredAt.append(now)
            let reached = frames.count == wanted
            let pending = expectation
            if reached { expectation = nil }
            lock.unlock()
            if reached { pending?.fulfill() }
        }

        var snapshot: (frames: [MigoDisplayLink.Frame], deliveredAt: [CFTimeInterval]) {
            lock.lock()
            defer { lock.unlock() }
            return (frames, deliveredAt)
        }
    }

    func testTheLinkDeliversTicksWhereADisplayExists() throws {
        let ticks = Ticks()
        // The real OS major, so what this exercises is what this machine ships
        // rather than a branch chosen to be convenient. Without a view the macOS
        // path falls back to `CVDisplayLink` on every version, which is the arm
        // that had never delivered a tick under test.
        let major = ProcessInfo.processInfo.operatingSystemVersion.majorVersion
        #if os(macOS)
            let platform = MigoDisplayLinkPolicy.Platform.macOS
        #else
            let platform = MigoDisplayLinkPolicy.Platform.iOS
        #endif
        let link = MigoDisplayLink(
            decision: MigoDisplayLinkPolicy.decide(.init(platform: platform, osMajor: major)),
            onTick: { frame in ticks.record(frame) })

        // Twenty is a third of a second at 60 Hz and a sixth at 120: enough to see
        // a cadence, short enough that a slow machine still finishes inside the
        // wait below.
        let wanted = 20
        let arrived = expectation(description: "\(wanted) ticks")
        ticks.expect(wanted, arrived)

        link.start()
        try XCTSkipUnless(
            link.isRunning,
            "no display link started on this machine, so there is no delivery to observe. That is "
                + "the headless case and not a failure; run this where a display is active")
        defer { link.stop() }

        // `wait` spins the run loop, which is not incidental: the CVDisplayLink
        // callback hops to the main queue precisely so renderer work stays off the
        // display's real-time thread, and a test that slept instead would never
        // see a tick and would report the link as dead.
        wait(for: [arrived], timeout: 5)

        let (frames, deliveredAt) = ticks.snapshot
        let targets = frames.map(\.targetTimestamp)
        XCTAssertGreaterThanOrEqual(targets.count, wanted)

        // Both times are on the clock `CACurrentMediaTime` reads, whichever
        // mechanism delivered them: a frame began before it was delivered, and not
        // long before. A frame stamped on another clock -- the display's video
        // time, which `CVDisplayLink` also reports -- is nowhere near the moment it
        // arrived, and an engine that converts it to `performance.now()`'s timeline
        // would put the frame anywhere at all.
        for (frame, arrived) in zip(frames, deliveredAt) {
            XCTAssertLessThanOrEqual(
                frame.timestamp, arrived,
                "a frame began at \(frame.timestamp), after it was delivered at \(arrived)")
            XCTAssertLessThan(
                arrived - frame.timestamp, 1,
                "a frame began \(arrived - frame.timestamp) s before it was delivered: not on the uptime clock")
            XCTAssertGreaterThanOrEqual(frame.targetTimestamp, frame.timestamp)
        }
        for (index, pair) in zip(frames, frames.dropFirst()).enumerated() {
            XCTAssertGreaterThan(
                pair.1.timestamp, pair.0.timestamp,
                "frame start timestamps must strictly increase; tick \(index + 1) did not advance")
        }

        // Strictly increasing, because `targetTimestamp` is when the frame being
        // drawn is due to appear. A repeated or reversed value would mean a
        // presenter pacing against it draws two frames for one slot or steps
        // backwards, and the arithmetic that produces it -- a host time over the
        // host clock's frequency -- is exactly where a zero frequency or a unit
        // mistake would show up.
        for (index, pair) in zip(targets, targets.dropFirst()).enumerated() {
            XCTAssertGreaterThan(
                pair.1, pair.0,
                "target timestamps must strictly increase; tick \(index + 1) did not advance")
        }

        // A frame's worth of time, bounded loosely on both sides: the point is
        // that the value is a duration in seconds and not milliseconds, nanoseconds
        // or a raw video time. 1 Hz to 1000 Hz covers every display Apple ships and
        // every stall a loaded machine can introduce.
        for duration in frames.map(\.duration) {
            XCTAssertGreaterThanOrEqual(duration, 0)
            XCTAssertLessThan(
                duration, 1,
                "a tick reported \(duration) s until the frame is due, which is not a frame")
        }

        let span = targets[targets.count - 1] - targets[0]
        XCTAssertGreaterThan(span, 0)
        let hertz = Double(targets.count - 1) / span
        XCTAssertGreaterThan(hertz, 1, "an implausible cadence of \(hertz) Hz")
        XCTAssertLessThan(hertz, 1000, "an implausible cadence of \(hertz) Hz")

        link.stop()
        XCTAssertFalse(link.isRunning)

        // Stopping means stopping: a link that keeps firing after stop is a
        // battery complaint with no owner, which is the failure `MigoDisplayLink`'s
        // proxy exists to prevent on the retain side and this checks on the
        // delivery side.
        let afterStop = ticks.snapshot.frames.count
        let settle = expectation(description: "settle")
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.25) { settle.fulfill() }
        wait(for: [settle], timeout: 2)
        XCTAssertEqual(
            ticks.snapshot.frames.count, afterStop,
            "ticks kept arriving after stop()")
    }
}

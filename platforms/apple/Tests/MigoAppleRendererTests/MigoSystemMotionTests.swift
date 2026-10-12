import Foundation
import MigoEngine
import XCTest

@testable import MigoAppleRenderer

#if os(iOS)
    import CoreMotion
#endif

/// The motion service's arithmetic against the conventions it promises: the
/// header's axes and units, and the W3C DeviceOrientation angles, from the
/// vectors CoreMotion reports in the device's frame (gravity toward the earth;
/// the magnetic field pointing north and, here, down).
final class MigoSystemMotionTests: XCTestCase {
    private func assertAngles(
        _ angles: (alpha: Double, beta: Double, gamma: Double), _ alpha: Double, _ beta: Double, _ gamma: Double,
        _ what: String, file: StaticString = #filePath, line: UInt = #line
    ) {
        XCTAssertEqual(angles.alpha, alpha, accuracy: 1e-6, "alpha, \(what)", file: file, line: line)
        XCTAssertEqual(angles.beta, beta, accuracy: 1e-6, "beta, \(what)", file: file, line: line)
        XCTAssertEqual(angles.gamma, gamma, accuracy: 1e-6, "gamma, \(what)", file: file, line: line)
    }

    func testTheIntervalsAreTheSensorsOwn() {
        XCTAssertEqual(MigoMotionMath.interval(["interval": "game"]), 0.02)
        XCTAssertEqual(MigoMotionMath.interval(["interval": "ui"]), 0.06)
        XCTAssertEqual(MigoMotionMath.interval(["interval": "normal"]), 0.2)
        XCTAssertEqual(MigoMotionMath.interval([:]), 0.2)
    }

    func testAFaceUpDeviceReadsPlusOneG() {
        // CoreMotion reports the pull of gravity: face up is z = -1.
        let (x, y, z) = MigoMotionMath.accelerometer(x: 0, y: 0, z: -1)
        XCTAssertEqual([x, y, z], [-0.0, -0.0, 1])
    }

    func testOrientationFollowsTheW3CAngles() {
        // Flat, face up, its top toward north: the field points along +y and down.
        assertAngles(
            MigoMotionMath.orientation(gravity: (0, 0, -1), field: (0, 20, -40), heading: nil),
            0, 0, 0, "flat, top north")
        // Turned a quarter counterclockwise seen from above: top west, right side north.
        assertAngles(
            MigoMotionMath.orientation(gravity: (0, 0, -1), field: (20, 0, -40), heading: nil),
            90, 0, 0, "flat, top west")
        // Turned a quarter clockwise: top east, right side south.
        assertAngles(
            MigoMotionMath.orientation(gravity: (0, 0, -1), field: (-20, 0, -40), heading: nil),
            270, 0, 0, "flat, top east")
        // Upright, its screen facing south: top tilted up a quarter turn from flat-top-north.
        assertAngles(
            MigoMotionMath.orientation(gravity: (0, -1, 0), field: (0, -40, -20), heading: nil),
            0, 90, 0, "upright, back to north")
        // Face down, its top south: the half turn about x from flat-top-north, kept in [-180, 180).
        assertAngles(
            MigoMotionMath.orientation(gravity: (0, 0, 1), field: (0, -20, 40), heading: nil),
            0, -180, 0, "face down, top south")
        // Tilted, the field is north and down as the device now sees them.
        let s = sin(Double.pi / 6)
        let c = cos(Double.pi / 6)
        func field(north: (Double, Double, Double), gravity: (Double, Double, Double)) -> (Double, Double, Double) {
            (20 * north.0 + 40 * gravity.0, 20 * north.1 + 40 * gravity.1, 20 * north.2 + 40 * gravity.2)
        }
        // Right side down by 30 degrees, top still north: gamma positive.
        let rightDown = (s, 0.0, -c)
        assertAngles(
            MigoMotionMath.orientation(
                gravity: rightDown, field: field(north: (0, 1, 0), gravity: rightDown), heading: nil),
            0, 0, 30, "right side down")
        // Top raised by 30 degrees toward north: beta positive.
        let topUp = (0.0, -s, -c)
        assertAngles(
            MigoMotionMath.orientation(gravity: topUp, field: field(north: (0, c, -s), gravity: topUp), heading: nil),
            0, 30, 0, "top raised")
        // Both, turned to face west: every angle at once.
        // R = Rz(90) Rx(30) Ry(20): the device's view of north and of gravity.
        let (a, b, g) = (Double.pi / 2, Double.pi / 6, Double.pi / 9)
        let r = [
            [cos(a) * cos(g) - sin(a) * sin(b) * sin(g), -cos(b) * sin(a), cos(g) * sin(a) * sin(b) + cos(a) * sin(g)],
            [cos(g) * sin(a) + cos(a) * sin(b) * sin(g), cos(a) * cos(b), sin(a) * sin(g) - cos(a) * cos(g) * sin(b)],
            [-cos(b) * sin(g), sin(b), cos(b) * cos(g)],
        ]
        let north = (r[1][0], r[1][1], r[1][2])
        let down = (-r[2][0], -r[2][1], -r[2][2])
        assertAngles(
            MigoMotionMath.orientation(gravity: down, field: field(north: north, gravity: down), heading: nil),
            90, 30, 20, "turned and tilted")
    }

    func testWithNoUsableFieldNorthComesFromTheHeading() {
        // A heading is clockwise from north; alpha is counterclockwise.
        assertAngles(
            MigoMotionMath.orientation(gravity: (0, 0, -1), field: (0, 0, 0), heading: 90), 270, 0, 0,
            "heading east")
        assertAngles(
            MigoMotionMath.orientation(gravity: (0, 0, -1), field: (0, 0, 0), heading: nil), 0, 0, 0,
            "no north at all")
    }

    func testTheCompassAccuracyIsTheCalibration() {
        XCTAssertEqual(MigoMotionMath.compassAccuracy(2), MigoCompassAccuracy(MIGO_COMPASS_ACCURACY_HIGH))
        XCTAssertEqual(MigoMotionMath.compassAccuracy(1), MigoCompassAccuracy(MIGO_COMPASS_ACCURACY_MEDIUM))
        XCTAssertEqual(MigoMotionMath.compassAccuracy(0), MigoCompassAccuracy(MIGO_COMPASS_ACCURACY_LOW))
        XCTAssertEqual(MigoMotionMath.compassAccuracy(-1), MigoCompassAccuracy(MIGO_COMPASS_ACCURACY_UNRELIABLE))
    }

    #if os(iOS)
        /// A start the device has no sensor for fails, and content is told; the
        /// simulator has none.
        func testAStartWithoutTheSensorIsRefused() {
            final class Recorder: @unchecked Sendable {
                var outcomes: [MigoHostServiceReply.Outcome] = []
            }
            XCTAssertTrue(MigoBuiltInHostServices.services.contains(.motion))
            let manager = CMMotionManager()
            let motion = MigoSystemMotion(post: { _, _, _ in })
            let recorder = Recorder()
            let request = MigoHostServiceRequest(
                service: .motion, method: MigoHostServiceID.Motion.startAccelerometer,
                payload: Data(#"{"interval":"game"}"#.utf8),
                reply: MigoHostServiceReply { recorder.outcomes.append($0) })
            motion.handle(request)
            motion.handle(
                MigoHostServiceRequest(
                    service: .motion, method: MigoHostServiceID.Motion.stopAccelerometer, payload: Data("{}".utf8),
                    reply: nil))
            if manager.isAccelerometerAvailable {
                XCTAssertEqual(recorder.outcomes, [.success(json: Data("{}".utf8))])
            } else {
                XCTAssertEqual(recorder.outcomes, [.failure(message: "not supported", code: nil)])
            }
        }
    #endif
}

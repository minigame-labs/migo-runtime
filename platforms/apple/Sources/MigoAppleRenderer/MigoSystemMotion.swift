import Foundation
import MigoEngine

#if os(iOS)
    import CoreMotion
#endif

/// The arithmetic of the motion service, apart from CoreMotion so it can be
/// held to the conventions it promises: `include/migo/host_services.h`'s axes
/// and units, and the W3C DeviceOrientation angles content reads.
enum MigoMotionMath {
    /// `{"interval"}`: "game" 20 ms, "ui" 60 ms, anything else "normal" 200 ms.
    static func interval(_ options: [String: Any]) -> TimeInterval {
        switch options["interval"] as? String {
        case "game": return 0.02
        case "ui": return 0.06
        default: return 0.2
        }
    }

    /// How often the compass reads: it takes no interval.
    static let compassInterval: TimeInterval = 0.2

    /// CoreMotion's acceleration (g, the pull of gravity: a device face up
    /// reads z = -1) as the header has it -- the device's reaction to it, face
    /// up reading z = +1, on the same axes.
    static func accelerometer(x: Double, y: Double, z: Double) -> (Double, Double, Double) {
        (-x, -y, -z)
    }

    /// The device's orientation as W3C DeviceOrientation angles in degrees --
    /// alpha [0, 360), beta [-180, 180), gamma [-90, 90) -- from two vectors
    /// CoreMotion documents in the device's own frame: `gravity` (toward the
    /// earth, in g) and the calibrated magnetic `field`. No attitude matrix
    /// convention is assumed.
    ///
    /// The earth frame is east-north-up. Up is against gravity; north is the
    /// field's horizontal part; east is north x up. The rows of R, which takes
    /// the device's coordinates to the earth's, are those axes in the device's
    /// coordinates, and R = Rz(alpha) Rx(beta) Ry(gamma) is decomposed with
    /// gamma kept in [-90, 90), as the specification does. A field with no
    /// horizontal part -- uncalibrated, or none -- leaves north to `heading`,
    /// the clockwise degrees from north CoreMotion reports when it has one.
    static func orientation(
        gravity: (Double, Double, Double), field: (Double, Double, Double), heading: Double?
    ) -> (alpha: Double, beta: Double, gamma: Double) {
        let up = normalized((-gravity.0, -gravity.1, -gravity.2)) ?? (0, 0, 1)
        // beta and gamma need only the vertical.
        let (r20, r21, r22) = up
        var beta: Double
        var gamma: Double
        let flipped = r22 < 0
        if flipped {
            let asinBeta = asin(clamp(r21))
            beta = asinBeta >= 0 ? Double.pi - asinBeta : -Double.pi - asinBeta
            gamma = atan2(r20, -r22)
        } else {
            beta = asin(clamp(r21))
            gamma = atan2(-r20, r22)
        }
        var alpha = 0.0
        let along = dot(field, up)
        let horizontal = (field.0 - along * up.0, field.1 - along * up.1, field.2 - along * up.2)
        if let north = normalized(horizontal) {
            let east = cross(north, up)
            // R01 = east.y, R11 = north.y (row 0 east, row 1 north).
            if abs(cos(beta)) < 1e-6 {
                // Upright: alpha and gamma turn about the same axis, and R00 =
                // cos(alpha +- gamma), R10 = sin(alpha +- gamma). gamma is taken
                // as 0 and alpha is the whole turn.
                alpha = atan2(north.0, east.0)
                gamma = 0
            } else {
                // R01 = -cos(beta) sin(alpha), R11 = cos(beta) cos(alpha).
                alpha = flipped ? atan2(east.1, -north.1) : atan2(-east.1, north.1)
            }
        } else if let heading {
            alpha = -heading * Double.pi / 180
        }
        return (degrees360(alpha), degreesSigned(beta, range: 180), degreesSigned(gamma, range: 90))
    }

    /// CoreMotion's magnetic-field calibration as the header's compass accuracy.
    static func compassAccuracy(_ calibration: Int32) -> MigoCompassAccuracy {
        switch calibration {
        case 2: return MigoCompassAccuracy(MIGO_COMPASS_ACCURACY_HIGH)
        case 1: return MigoCompassAccuracy(MIGO_COMPASS_ACCURACY_MEDIUM)
        case 0: return MigoCompassAccuracy(MIGO_COMPASS_ACCURACY_LOW)
        default: return MigoCompassAccuracy(MIGO_COMPASS_ACCURACY_UNRELIABLE)
        }
    }

    private static func clamp(_ value: Double) -> Double { min(1, max(-1, value)) }

    private static func dot(_ a: (Double, Double, Double), _ b: (Double, Double, Double)) -> Double {
        a.0 * b.0 + a.1 * b.1 + a.2 * b.2
    }

    private static func cross(_ a: (Double, Double, Double), _ b: (Double, Double, Double)) -> (Double, Double, Double) {
        (a.1 * b.2 - a.2 * b.1, a.2 * b.0 - a.0 * b.2, a.0 * b.1 - a.1 * b.0)
    }

    private static func normalized(_ v: (Double, Double, Double)) -> (Double, Double, Double)? {
        let length = sqrt(dot(v, v))
        guard length > 1e-9, length.isFinite else { return nil }
        return (v.0 / length, v.1 / length, v.2 / length)
    }

    private static func degrees360(_ radians: Double) -> Double {
        var degrees = radians * 180 / Double.pi
        degrees = degrees.truncatingRemainder(dividingBy: 360)
        if degrees < 0 { degrees += 360 }
        return degrees >= 360 ? 0 : degrees
    }

    /// Degrees in [-range, range).
    private static func degreesSigned(_ radians: Double, range: Double) -> Double {
        let degrees = radians * 180 / Double.pi
        return degrees >= range ? degrees - 2 * range : degrees
    }
}

#if os(iOS)
    /// The motion service, answered by CoreMotion: the accelerometer, the
    /// gyroscope, the device's orientation and the compass, each started by
    /// content at its interval and read on the main queue into typed sensor
    /// samples. Raw motion needs no purpose string, so it is the SDK's own.
    ///
    /// The orientation and the compass are one CoreMotion stream, referenced
    /// to magnetic north where the device has a magnetometer; without one the
    /// orientation's alpha is relative and the compass is not supported.
    final class MigoSystemMotion {
        typealias Sample = (_ kind: MigoSensorKind, _ values: (Double, Double, Double), _ accuracy: MigoCompassAccuracy) -> Void

        private typealias Method = MigoHostServiceID.Motion

        private let manager = CMMotionManager()
        private let post: Sample
        private var deviceMotionInterval: TimeInterval?
        private var compassOn = false
        private var lastCompass: TimeInterval = -.infinity
        /// The stream's interval and frame as started, to restart it only when
        /// either changes.
        private var running: (interval: TimeInterval, frame: CMAttitudeReferenceFrame)?

        init(post: @escaping Sample) {
            self.post = post
        }

        deinit {
            manager.stopAccelerometerUpdates()
            manager.stopGyroUpdates()
            manager.stopDeviceMotionUpdates()
        }

        private var magneticNorth: Bool {
            CMMotionManager.availableAttitudeReferenceFrames().contains(.xMagneticNorthZVertical)
        }

        func handle(_ request: MigoHostServiceRequest) {
            switch request.method {
            case Method.startAccelerometer:
                guard manager.isAccelerometerAvailable else { return refuse(request) }
                manager.accelerometerUpdateInterval = MigoMotionMath.interval(request.object)
                manager.startAccelerometerUpdates(to: .main) { [weak self] data, _ in
                    guard let self, let a = data?.acceleration else { return }
                    self.post(
                        MigoSensorKind(MIGO_SENSOR_ACCELEROMETER),
                        MigoMotionMath.accelerometer(x: a.x, y: a.y, z: a.z), 0)
                }
                request.reply?.succeed()
            case Method.stopAccelerometer:
                manager.stopAccelerometerUpdates()
            case Method.startGyroscope:
                guard manager.isGyroAvailable else { return refuse(request) }
                manager.gyroUpdateInterval = MigoMotionMath.interval(request.object)
                manager.startGyroUpdates(to: .main) { [weak self] data, _ in
                    guard let self, let r = data?.rotationRate else { return }
                    // Radians a second about the device's axes, right-handed:
                    // CoreMotion's and the header's are the same.
                    self.post(MigoSensorKind(MIGO_SENSOR_GYROSCOPE), (r.x, r.y, r.z), 0)
                }
                request.reply?.succeed()
            case Method.stopGyroscope:
                manager.stopGyroUpdates()
            case Method.startDeviceMotion:
                guard manager.isDeviceMotionAvailable else { return refuse(request) }
                deviceMotionInterval = MigoMotionMath.interval(request.object)
                streamDeviceMotion()
                request.reply?.succeed()
            case Method.stopDeviceMotion:
                deviceMotionInterval = nil
                streamDeviceMotion()
            case Method.startCompass:
                guard manager.isDeviceMotionAvailable, magneticNorth else { return refuse(request) }
                compassOn = true
                streamDeviceMotion()
                request.reply?.succeed()
            case Method.stopCompass:
                compassOn = false
                streamDeviceMotion()
            default:
                request.reply?.fail("not supported")
            }
        }

        /// The device has no such sensor: the start fails, and content is told.
        private func refuse(_ request: MigoHostServiceRequest) {
            request.reply?.fail("not supported")
        }

        /// Run the one device-motion stream at what the orientation and the
        /// compass need, or stop it when neither is wanted.
        private func streamDeviceMotion() {
            guard deviceMotionInterval != nil || compassOn else {
                manager.stopDeviceMotionUpdates()
                running = nil
                return
            }
            let interval = deviceMotionInterval ?? MigoMotionMath.compassInterval
            let frame: CMAttitudeReferenceFrame = magneticNorth ? .xMagneticNorthZVertical : .xArbitraryCorrectedZVertical
            if let running, running.interval == interval, running.frame == frame { return }
            manager.stopDeviceMotionUpdates()
            manager.deviceMotionUpdateInterval = interval
            running = (interval, frame)
            manager.startDeviceMotionUpdates(using: frame, to: .main) { [weak self] motion, _ in
                guard let self, let motion else { return }
                self.deliver(motion)
            }
        }

        private func deliver(_ motion: CMDeviceMotion) {
            let referencedToNorth = running?.frame == .xMagneticNorthZVertical
            let calibrated = motion.magneticField.accuracy != .uncalibrated
            if deviceMotionInterval != nil {
                let g = motion.gravity
                let b = motion.magneticField.field
                let angles = MigoMotionMath.orientation(
                    gravity: (g.x, g.y, g.z),
                    field: referencedToNorth && calibrated ? (b.x, b.y, b.z) : (0, 0, 0),
                    heading: referencedToNorth ? motion.heading : nil)
                post(MigoSensorKind(MIGO_SENSOR_DEVICE_MOTION), (angles.alpha, angles.beta, angles.gamma), 0)
            }
            if compassOn, motion.timestamp - lastCompass >= MigoMotionMath.compassInterval - 0.005 {
                lastCompass = motion.timestamp
                let accuracy = MigoMotionMath.compassAccuracy(motion.magneticField.accuracy.rawValue)
                post(MigoSensorKind(MIGO_SENSOR_COMPASS), (motion.heading, 0, 0), accuracy)
            }
        }
    }
#endif

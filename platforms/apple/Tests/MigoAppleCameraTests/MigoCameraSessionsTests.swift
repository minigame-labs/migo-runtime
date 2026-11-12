import CoreGraphics
import XCTest

@testable import MigoAppleCamera

/// A camera that records what it is asked and answers as the test says.
private final class Device: MigoCameraDevice {
    var stopped: (() -> Void)?
    var frame: ((MigoCameraPixels) -> Void)?
    var asked: [String] = []
    /// Why it does not open, when it does not.
    var refusal: MigoCameraFailure?
    /// Held until the test lets the camera finish opening.
    var opening: ((MigoCameraFailure?) -> Void)?
    var opensAtOnce = true

    func open(_ settings: MigoCameraSettings, done: @escaping (MigoCameraFailure?) -> Void) {
        asked.append("open \(settings.position.rawValue) \(settings.flash.rawValue) \(settings.size.rawValue) \(settings.frame)")
        if opensAtOnce { done(refusal) } else { opening = done }
    }

    func apply(_ settings: MigoCameraSettings) {
        asked.append("apply \(settings.position.rawValue) \(settings.flash.rawValue) \(settings.size.rawValue) \(settings.frame)")
    }

    func takePhoto(quality: String, done: @escaping (Result<MigoCameraPhoto, MigoCameraFailure>) -> Void) {
        asked.append("photo \(quality)")
        done(.success(MigoCameraPhoto(path: "/tmp/p.jpg", width: 3, height: 4)))
    }

    func startRecord(done: @escaping (MigoCameraFailure?) -> Void) {
        asked.append("record")
        done(nil)
    }

    func stopRecord(compressed: Bool, done: @escaping (Result<MigoCameraVideo, MigoCameraFailure>) -> Void) {
        asked.append("stop \(compressed)")
        done(.success(MigoCameraVideo(video: "/tmp/v.mov", cover: "/tmp/c.jpg")))
    }

    func zoom(to factor: Double) -> Double {
        asked.append("zoom \(factor)")
        return min(max(factor, 1), 4)
    }

    func listen(_ on: Bool) { asked.append("listen \(on)") }
    func close() { asked.append("close") }
}

final class MigoCameraSessionsTests: XCTestCase {
    private var devices: [Device] = []
    private var next = Device()
    private var granted = true
    /// Held until the test answers the system's prompt for the player.
    private var prompt: ((Bool) -> Void)?
    private var promptsAtOnce = true
    private var events: [String] = []
    private var frames: [(camera: Int, width: Int, first: UInt8)] = []
    private lazy var cameras = MigoCameraSessions(
        grant: { [unowned self] answer in
            if self.promptsAtOnce { answer(self.granted) } else { self.prompt = answer }
        },
        device: { [unowned self] in
            self.devices.append(self.next)
            return self.next
        },
        post: { [unowned self] event, object in
            self.events.append("\(event) \((object["cameraId"] as? Int) ?? -1)")
        },
        frame: { [unowned self] camera, pixels in
            self.frames.append((camera, pixels.width, pixels.pixels.load(as: UInt8.self)))
        })

    private typealias Outcome = Result<[String: Any], MigoCameraFailure>

    private func call(_ method: MigoCameraSessions.Method, _ object: [String: Any]) -> Outcome? {
        var outcome: Outcome?
        cameras.handle(method, object) { outcome = $0 }
        return outcome
    }

    private func reason(_ outcome: Outcome?) -> String? {
        if case .failure(let failure)? = outcome { return failure.reason }
        return nil
    }

    private func value(_ outcome: Outcome?) -> NSDictionary? {
        if case .success(let value)? = outcome { return value as NSDictionary }
        return nil
    }

    // MARK: - settings

    func testAnOptionLeftOutTakesTheReferencesDefaultAndASetChangesOnlyWhatItNames() {
        var settings = MigoCameraSettings([:])
        XCTAssertEqual(settings.frame, CGRect(x: 0, y: 0, width: 300, height: 150))
        XCTAssertEqual(settings.position, .back)
        XCTAssertEqual(settings.flash, .auto)
        XCTAssertEqual(settings.size, .small)
        settings.take(["x": 12, "flash": "off", "size": "enormous", "width": -4])
        XCTAssertEqual(settings.frame, CGRect(x: 12, y: 0, width: 300, height: 150), "a width that is none is not taken")
        XCTAssertEqual(settings.flash, .off)
        XCTAssertEqual(settings.size, .small, "a size it does not know changes nothing")
        XCTAssertEqual(MigoCameraSettings(["devicePosition": "front", "size": "large"]).position, .front)
    }

    // MARK: - opening

    func testACameraOpensOnceTheAppMayUseItAndIsShownWhereContentPutIt() {
        let created = call(.create, ["cameraId": 7, "x": 10, "y": 20, "width": 160, "height": 120, "devicePosition": "front", "flash": "on", "size": "medium"])
        XCTAssertNotNil(value(created))
        XCTAssertEqual(devices.count, 1)
        XCTAssertEqual(next.asked, ["open front on medium (10.0, 20.0, 160.0, 120.0)"])
        XCTAssertEqual(reason(call(.create, ["cameraId": 7])), "the camera is open already")
        XCTAssertTrue(events.isEmpty)
    }

    func testARefusedCameraFailsTheCreateAndIsReportedAsCancelled() {
        granted = false
        XCTAssertEqual(reason(call(.create, ["cameraId": 7])), "auth deny, the app may not use the camera")
        XCTAssertEqual(events, ["authCancel 7"])
        XCTAssertTrue(devices.isEmpty, "no camera is made for an app that may not use one")
        XCTAssertEqual(reason(call(.takePhoto, ["cameraId": 7])), "camera not open")
    }

    func testACameraDestroyedWhileThePlayerIsAskedNeverOpens() {
        promptsAtOnce = false
        var outcome: Outcome?
        cameras.handle(.create, ["cameraId": 7]) { outcome = $0 }
        XCTAssertNil(outcome, "the call waits on the player")
        cameras.handle(.destroy, ["cameraId": 7], answer: nil)
        prompt?(true)
        XCTAssertEqual(reason(outcome), "camera destroyed")
        XCTAssertTrue(devices.isEmpty)
    }

    func testACameraThatDoesNotOpenIsLetGoAndOneDestroyedWhileOpeningIsClosed() {
        next.refusal = MigoCameraFailure("no back camera")
        XCTAssertEqual(reason(call(.create, ["cameraId": 7])), "no back camera")
        XCTAssertEqual(next.asked.last, "close")
        XCTAssertEqual(reason(call(.setZoom, ["cameraId": 7, "zoom": 2])), "camera not open")

        // Another, destroyed before it finished opening: closed once, and its CREATE fails.
        next = Device()
        next.opensAtOnce = false
        var outcome: Outcome?
        cameras.handle(.create, ["cameraId": 8]) { outcome = $0 }
        cameras.handle(.destroy, ["cameraId": 8], answer: nil)
        next.opening?(nil)
        XCTAssertEqual(reason(outcome), "camera destroyed")
        XCTAssertEqual(next.asked.filter { $0 == "close" }.count, 1)
    }

    // MARK: - a camera's calls

    func testWhatContentAsksOfACameraReachesItAndIsAnsweredInTheServicesWords() {
        XCTAssertNotNil(value(call(.create, ["cameraId": 7])))
        cameras.handle(.set, ["cameraId": 7, "y": 30, "devicePosition": "front"], answer: nil)
        XCTAssertEqual(next.asked.last, "apply front auto small (0.0, 30.0, 300.0, 150.0)", "the whole of what it is now")

        let photo = value(call(.takePhoto, ["cameraId": 7, "quality": "low"]))
        XCTAssertEqual(photo, ["tempImagePath": "/tmp/p.jpg", "width": 3, "height": 4])
        XCTAssertEqual(next.asked.last, "photo low")
        _ = call(.takePhoto, ["cameraId": 7])
        XCTAssertEqual(next.asked.last, "photo normal")

        XCTAssertEqual(value(call(.startRecord, ["cameraId": 7])), [:])
        XCTAssertEqual(
            value(call(.stopRecord, ["cameraId": 7, "compressed": true])),
            ["tempVideoPath": "/tmp/v.mov", "tempThumbPath": "/tmp/c.jpg"])
        XCTAssertEqual(next.asked.last, "stop true")

        XCTAssertEqual(value(call(.setZoom, ["cameraId": 7, "zoom": 9])), ["zoom": 4.0], "the zoom the camera took")
        XCTAssertEqual(reason(call(.setZoom, ["cameraId": 7])), "no zoom to set")

        cameras.handle(.listenFrameChange, ["cameraId": 7], answer: nil)
        cameras.handle(.closeFrameChange, ["cameraId": 7], answer: nil)
        XCTAssertEqual(Array(next.asked.suffix(2)), ["listen true", "listen false"])
        // Another camera's id reaches no camera.
        XCTAssertEqual(reason(call(.takePhoto, ["cameraId": 70])), "camera not open")
    }

    func testACamerasFramesAndItsStopAreReportedByItsId() {
        XCTAssertNotNil(value(call(.create, ["cameraId": 7])))
        var picture: [UInt8] = [9, 8, 7, 255]
        picture.withUnsafeBytes { bytes in
            next.frame?(MigoCameraPixels(width: 1, height: 1, rowStride: 4, bgra: true, pixels: bytes.baseAddress!))
        }
        XCTAssertEqual(frames.count, 1)
        XCTAssertEqual(frames[0].camera, 7)
        XCTAssertEqual(frames[0].first, 9)
        next.stopped?()
        XCTAssertEqual(events, ["stop 7"])
    }

    // MARK: - going away

    func testDestroyClosesTheCameraAndAClosedSessionClosesThemAll() {
        XCTAssertNotNil(value(call(.create, ["cameraId": 7])))
        let first = next
        next = Device()
        XCTAssertNotNil(value(call(.create, ["cameraId": 8])))
        cameras.handle(.destroy, ["cameraId": 7], answer: nil)
        XCTAssertEqual(first.asked.last, "close")
        XCTAssertNotEqual(next.asked.last, "close")
        cameras.close()
        XCTAssertEqual(next.asked.last, "close")
        XCTAssertEqual(reason(call(.takePhoto, ["cameraId": 8])), "camera not open")
        XCTAssertEqual(first.asked.filter { $0 == "close" }.count, 1, "closed once")
    }
}

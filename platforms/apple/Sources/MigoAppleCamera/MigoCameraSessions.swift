import CoreGraphics
import Foundation

/// What content asked a camera to be: where its preview stands over the game, which camera, the flash, and the
/// size of the frames it hands over.
struct MigoCameraSettings: Equatable {
    enum Position: String {
        case back, front
    }

    enum Flash: String {
        case auto, on, off
    }

    enum Size: String {
        case small, medium, large
    }

    /// In content's coordinates: the game view's points from its top left.
    var frame: CGRect
    var position: Position
    var flash: Flash
    var size: Size

    /// CREATE's options, one content left out taking the reference's default.
    init(_ object: [String: Any]) {
        frame = CGRect(x: 0, y: 0, width: 300, height: 150)
        position = .back
        flash = .auto
        size = .small
        take(object)
    }

    /// What `object` names, changed: a SET carries only what content changed.
    mutating func take(_ object: [String: Any]) {
        func number(_ key: String) -> CGFloat? {
            (object[key] as? NSNumber).map { CGFloat($0.doubleValue) }
        }
        if let x = number("x") { frame.origin.x = x }
        if let y = number("y") { frame.origin.y = y }
        if let width = number("width"), width > 0 { frame.size.width = width }
        if let height = number("height"), height > 0 { frame.size.height = height }
        if let position = (object["devicePosition"] as? String).flatMap(Position.init(rawValue:)) { self.position = position }
        if let flash = (object["flash"] as? String).flatMap(Flash.init(rawValue:)) { self.flash = flash }
        if let size = (object["size"] as? String).flatMap(Size.init(rawValue:)) { self.size = size }
    }
}

/// Why a camera's call failed, in the words content hears.
struct MigoCameraFailure: Error, Equatable {
    let reason: String

    init(_ reason: String) {
        self.reason = reason
    }
}

/// One frame, lent for the call that hands it over: `height` rows of `width` pixels, top row first, `rowStride`
/// bytes apart.
struct MigoCameraPixels {
    let width: Int
    let height: Int
    let rowStride: Int
    /// Blue first, as the system's cameras give them; else red first.
    let bgra: Bool
    let pixels: UnsafeRawPointer
}

struct MigoCameraPhoto: Equatable {
    let path: String
    let width: Int
    let height: Int
}

struct MigoCameraVideo: Equatable {
    let video: String
    let cover: String
}

/// One camera, open or opening: the system's (`MigoCaptureCamera`), and in tests one that makes its own pictures.
/// Main thread, and its answers arrive there.
protocol MigoCameraDevice: AnyObject {
    /// Called when the camera stops on its own: the app left the foreground, another app took it.
    var stopped: (() -> Void)? { get set }
    /// Called with each frame while it is listened to.
    var frame: ((MigoCameraPixels) -> Void)? { get set }
    /// Opens it and shows its preview; `done` once it shows, or with why not.
    func open(_ settings: MigoCameraSettings, done: @escaping (MigoCameraFailure?) -> Void)
    /// What content changed: the whole of what it is to be now.
    func apply(_ settings: MigoCameraSettings)
    func takePhoto(quality: String, done: @escaping (Result<MigoCameraPhoto, MigoCameraFailure>) -> Void)
    func startRecord(done: @escaping (MigoCameraFailure?) -> Void)
    func stopRecord(compressed: Bool, done: @escaping (Result<MigoCameraVideo, MigoCameraFailure>) -> Void)
    /// Zooms as near `factor` as it can; the zoom it took.
    func zoom(to factor: Double) -> Double
    func listen(_ on: Bool)
    /// Closes it and takes its preview away; a call still waiting fails.
    func close()
}

/// `createCamera`'s cameras, as the camera service (`MIGO_HOST_SERVICE_CAMERA`) asks for them and reports them:
/// each opened once the app may use the camera -- the player asked when the system has not decided, and a refusal
/// reported as the service's AUTH_CANCEL beside the failed CREATE -- shown where content put it, changed, used and
/// closed by its id. Main thread.
final class MigoCameraSessions {
    enum Method: UInt32 {
        case create = 0
        case destroy, set, takePhoto, startRecord, stopRecord, setZoom, listenFrameChange, closeFrameChange
    }

    enum Event: UInt32 {
        case stop = 0
        case authCancel
    }

    typealias Answer = (Result<[String: Any], MigoCameraFailure>) -> Void

    private struct Camera {
        let device: MigoCameraDevice
        var settings: MigoCameraSettings
    }

    private let grant: (@escaping (Bool) -> Void) -> Void
    private let make: () -> MigoCameraDevice
    private let post: (Event, [String: Any]) -> Void
    private let frame: (Int, MigoCameraPixels) -> Void
    private var cameras: [Int: Camera] = [:]
    /// Cameras whose CREATE waits on the player's answer.
    private var asking: Set<Int> = []

    /// `grant` asks whether the app may use the camera, answering on the main thread; `device` makes a camera to
    /// open; `post` reports an event, `frame` hands content a camera's frame.
    init(
        grant: @escaping (@escaping (Bool) -> Void) -> Void, device: @escaping () -> MigoCameraDevice,
        post: @escaping (Event, [String: Any]) -> Void, frame: @escaping (Int, MigoCameraPixels) -> Void
    ) {
        self.grant = grant
        make = device
        self.post = post
        self.frame = frame
    }

    /// One of the camera service's requests; `answer` is `nil` for a command.
    func handle(_ method: Method, _ object: [String: Any], answer: Answer?) {
        let id = (object["cameraId"] as? NSNumber)?.intValue ?? 0
        switch method {
        case .create:
            create(id, MigoCameraSettings(object), answer)
            return
        case .destroy:
            asking.remove(id)
            cameras.removeValue(forKey: id)?.device.close()
            return
        default:
            break
        }
        guard var camera = cameras[id] else {
            answer?(.failure(MigoCameraFailure("camera not open")))
            return
        }
        switch method {
        case .create, .destroy:
            break
        case .set:
            camera.settings.take(object)
            cameras[id] = camera
            camera.device.apply(camera.settings)
        case .takePhoto:
            camera.device.takePhoto(quality: object["quality"] as? String ?? "normal") { photo in
                answer?(photo.map { ["tempImagePath": $0.path, "width": $0.width, "height": $0.height] })
            }
        case .startRecord:
            camera.device.startRecord { failure in
                answer?(failure.map { .failure($0) } ?? .success([:]))
            }
        case .stopRecord:
            camera.device.stopRecord(compressed: object["compressed"] as? Bool ?? false) { video in
                answer?(video.map { ["tempVideoPath": $0.video, "tempThumbPath": $0.cover] })
            }
        case .setZoom:
            guard let asked = (object["zoom"] as? NSNumber)?.doubleValue, asked.isFinite else {
                answer?(.failure(MigoCameraFailure("no zoom to set")))
                return
            }
            answer?(.success(["zoom": camera.device.zoom(to: asked)]))
        case .listenFrameChange:
            camera.device.listen(true)
        case .closeFrameChange:
            camera.device.listen(false)
        }
    }

    /// The session closed: its cameras go, with their previews.
    func close() {
        asking = []
        let open = cameras
        cameras = [:]
        for camera in open.values { camera.device.close() }
    }

    private func create(_ id: Int, _ settings: MigoCameraSettings, _ answer: Answer?) {
        guard cameras[id] == nil, !asking.contains(id) else {
            answer?(.failure(MigoCameraFailure("the camera is open already")))
            return
        }
        asking.insert(id)
        grant { [weak self] granted in
            guard let self, self.asking.remove(id) != nil else {
                // Destroyed, or the session closed, while the player was asked.
                answer?(.failure(MigoCameraFailure("camera destroyed")))
                return
            }
            guard granted else {
                self.post(.authCancel, ["cameraId": id])
                answer?(.failure(MigoCameraFailure("auth deny, the app may not use the camera")))
                return
            }
            let device = self.make()
            device.stopped = { [weak self] in self?.post(.stop, ["cameraId": id]) }
            device.frame = { [weak self] pixels in self?.frame(id, pixels) }
            self.cameras[id] = Camera(device: device, settings: settings)
            device.open(settings) { [weak self] failure in
                guard let self, self.cameras[id]?.device === device else {
                    answer?(.failure(MigoCameraFailure("camera destroyed")))
                    return
                }
                guard let failure else {
                    answer?(.success([:]))
                    return
                }
                // A camera that did not open is let go; content's DESTROY for it finds none.
                self.cameras[id] = nil
                device.close()
                answer?(.failure(failure))
            }
        }
    }
}

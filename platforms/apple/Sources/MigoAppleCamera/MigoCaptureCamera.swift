#if os(iOS)
    import AVFoundation
    import ImageIO
    import UIKit

    /// One of the phone's cameras, through AVFoundation: its preview a view of its own over the game where content
    /// put it, covering that rectangle; photos as JPEGs; video as the system records it, with the app's sound only
    /// when the app may already use the microphone -- nothing here asks for it; and frames as the camera makes
    /// them, upright as the preview shows them.
    ///
    /// Opening a camera and changing which one it is wait on the hardware, so they happen on a queue of the
    /// camera's own; everything is asked, and answered, on the main thread. Only the newest frame is handed over:
    /// one that arrives while the last is still on its way to content is let go.
    final class MigoCaptureCamera: NSObject, MigoCameraDevice, AVCapturePhotoCaptureDelegate,
        AVCaptureFileOutputRecordingDelegate, AVCaptureVideoDataOutputSampleBufferDelegate
    {
        /// The preview: a layer that shows what the session sees. It takes no touches, which are the game's.
        private final class Preview: UIView {
            override class var layerClass: AnyClass { AVCaptureVideoPreviewLayer.self }
            var shown: AVCaptureVideoPreviewLayer { layer as! AVCaptureVideoPreviewLayer }
        }

        var stopped: (() -> Void)?
        var frame: ((MigoCameraPixels) -> Void)?

        private weak var stage: UIView?
        private let session = AVCaptureSession()
        /// The session is set up, started and stopped here: each waits on the hardware.
        private let work = DispatchQueue(label: "com.migo.camera")
        private let frames = DispatchQueue(label: "com.migo.camera.frames")
        private let photos = AVCapturePhotoOutput()
        private let movie = AVCaptureMovieFileOutput()
        private let video = AVCaptureVideoDataOutput()
        private var preview: Preview?
        private var settings = MigoCameraSettings([:])
        /// The camera in use: set on the work queue as the session is set up, read on the main thread to zoom.
        /// Behind `gate`.
        private var camera: AVCaptureDevice?
        private var photoWaiters: [Int64: (Result<MigoCameraPhoto, MigoCameraFailure>) -> Void] = [:]
        private var starting: ((MigoCameraFailure?) -> Void)?
        private var stopping: (compressed: Bool, done: (Result<MigoCameraVideo, MigoCameraFailure>) -> Void)?
        private var observers: [NSObjectProtocol] = []
        private var closed = false
        /// What more than one thread reads: the camera in use, whether content listens, and whether a frame is on
        /// its way to it.
        private let gate = NSLock()
        private var listening = false
        private var delivering = false

        init(stage: UIView?) {
            self.stage = stage
        }

        // MARK: - opening and changing

        func open(_ settings: MigoCameraSettings, done: @escaping (MigoCameraFailure?) -> Void) {
            guard let stage else {
                done(MigoCameraFailure("there is no game view to show the camera in"))
                return
            }
            self.settings = settings
            let preview = Preview(frame: settings.frame)
            preview.isUserInteractionEnabled = false
            preview.shown.session = session
            preview.shown.videoGravity = .resizeAspectFill
            stage.addSubview(preview)
            self.preview = preview
            let rotation = Self.rotation(of: stage)
            for name in [AVCaptureSession.wasInterruptedNotification, AVCaptureSession.runtimeErrorNotification] {
                observers.append(
                    NotificationCenter.default.addObserver(forName: name, object: session, queue: .main) {
                        [weak self] _ in
                        guard let self, !self.closed else { return }
                        self.stopped?()
                    })
            }
            work.async { [self] in
                let failure = configure(settings, rotation: rotation)
                if failure == nil { session.startRunning() }
                DispatchQueue.main.async { [self] in
                    guard !closed else { return }
                    if failure == nil { orient(preview.shown.connection, rotation, mirrored: settings.position == .front) }
                    done(failure)
                }
            }
        }

        func apply(_ settings: MigoCameraSettings) {
            let before = self.settings
            self.settings = settings
            preview?.frame = settings.frame
            guard before.position != settings.position || before.size != settings.size, let stage else { return }
            let rotation = Self.rotation(of: stage)
            work.async { [self] in
                // The camera content asked for, or the one it had: a phone without that camera keeps the other.
                if configure(settings, rotation: rotation) != nil { _ = configure(before, rotation: rotation) }
                DispatchQueue.main.async { [self] in
                    guard !closed, let preview else { return }
                    orient(preview.shown.connection, rotation, mirrored: settings.position == .front)
                }
            }
        }

        /// The session's input and outputs for `settings`: `nil` once set, else why not. On the work queue.
        private func configure(_ settings: MigoCameraSettings, rotation: CGFloat) -> MigoCameraFailure? {
            let position: AVCaptureDevice.Position = settings.position == .front ? .front : .back
            guard let device = AVCaptureDevice.default(.builtInWideAngleCamera, for: .video, position: position),
                let input = try? AVCaptureDeviceInput(device: device)
            else {
                return MigoCameraFailure("there is no \(settings.position.rawValue) camera")
            }
            session.beginConfiguration()
            defer { session.commitConfiguration() }
            for input in session.inputs { session.removeInput(input) }
            guard session.canAddInput(input) else { return MigoCameraFailure("the camera could not be opened") }
            session.addInput(input)
            gate.lock()
            camera = device
            gate.unlock()
            // The app's sound with its video, when the app may already record: asking is not a camera's to do.
            if AVCaptureDevice.authorizationStatus(for: .audio) == .authorized,
                let microphone = AVCaptureDevice.default(for: .audio),
                let sound = try? AVCaptureDeviceInput(device: microphone), session.canAddInput(sound)
            {
                session.addInput(sound)
            }
            // The reference sets no pixel counts; these are the presets nearest Android's 320 x 240, 1280 x 720
            // and 1920 x 1080.
            let presets: [AVCaptureSession.Preset]
            switch settings.size {
            case .small: presets = [.cif352x288, .vga640x480, .high]
            case .medium: presets = [.hd1280x720, .high]
            case .large: presets = [.hd1920x1080, .hd1280x720, .high]
            }
            if let preset = presets.first(where: session.canSetSessionPreset) { session.sessionPreset = preset }
            if session.outputs.isEmpty {
                video.videoSettings = [kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_32BGRA]
                video.alwaysDiscardsLateVideoFrames = true
                video.setSampleBufferDelegate(self, queue: frames)
                for output in [photos, movie, video] as [AVCaptureOutput] where session.canAddOutput(output) {
                    session.addOutput(output)
                }
            }
            for output in session.outputs {
                orient(output.connection(with: .video), rotation, mirrored: settings.position == .front)
            }
            return nil
        }

        /// The quarter turn that stands the camera's picture upright in the game's window, as it is held now.
        private static func rotation(of stage: UIView) -> CGFloat {
            switch stage.window?.windowScene?.interfaceOrientation {
            case .landscapeRight: return 0
            case .landscapeLeft: return 180
            case .portraitUpsideDown: return 270
            default: return 90
            }
        }

        private func orient(_ connection: AVCaptureConnection?, _ rotation: CGFloat, mirrored: Bool) {
            guard let connection else { return }
            if #available(iOS 17.0, *) {
                if connection.isVideoRotationAngleSupported(rotation) { connection.videoRotationAngle = rotation }
            } else if connection.isVideoOrientationSupported {
                switch rotation {
                case 0: connection.videoOrientation = .landscapeRight
                case 180: connection.videoOrientation = .landscapeLeft
                case 270: connection.videoOrientation = .portraitUpsideDown
                default: connection.videoOrientation = .portrait
                }
            }
            if connection.isVideoMirroringSupported {
                connection.automaticallyAdjustsVideoMirroring = false
                connection.isVideoMirrored = mirrored
            }
        }

        // MARK: - photos

        func takePhoto(quality: String, done: @escaping (Result<MigoCameraPhoto, MigoCameraFailure>) -> Void) {
            guard photos.connection(with: .video) != nil else {
                done(.failure(MigoCameraFailure("the camera takes no photos")))
                return
            }
            let asked = AVCapturePhotoSettings(format: [AVVideoCodecKey: AVVideoCodecType.jpeg])
            let flash: AVCaptureDevice.FlashMode
            switch settings.flash {
            case .auto: flash = .auto
            case .on: flash = .on
            case .off: flash = .off
            }
            if photos.supportedFlashModes.contains(flash) { asked.flashMode = flash }
            // How much the system works on the picture: the reference's three qualities, as far as this camera goes.
            let wanted: AVCapturePhotoOutput.QualityPrioritization
            switch quality {
            case "high": wanted = .quality
            case "low": wanted = .speed
            default: wanted = .balanced
            }
            asked.photoQualityPrioritization =
                wanted.rawValue > photos.maxPhotoQualityPrioritization.rawValue ? photos.maxPhotoQualityPrioritization : wanted
            photoWaiters[asked.uniqueID] = done
            photos.capturePhoto(with: asked, delegate: self)
        }

        func photoOutput(_ output: AVCapturePhotoOutput, didFinishProcessingPhoto photo: AVCapturePhoto, error: Error?) {
            let id = photo.resolvedSettings.uniqueID
            var taken: Result<MigoCameraPhoto, MigoCameraFailure> = .failure(MigoCameraFailure("the photo could not be taken"))
            if error == nil, let data = photo.fileDataRepresentation() {
                let url = Self.staged("jpg")
                if (try? data.write(to: url)) != nil {
                    var size = photo.resolvedSettings.photoDimensions
                    // The file holds the sensor's picture and says how it is turned; content is told its size as shown.
                    if let turned = photo.metadata[kCGImagePropertyOrientation as String] as? UInt32, (5...8).contains(turned) {
                        size = CMVideoDimensions(width: size.height, height: size.width)
                    }
                    taken = .success(MigoCameraPhoto(path: url.path, width: Int(size.width), height: Int(size.height)))
                }
            }
            DispatchQueue.main.async { [self] in photoWaiters.removeValue(forKey: id)?(taken) }
        }

        // MARK: - video

        func startRecord(done: @escaping (MigoCameraFailure?) -> Void) {
            guard movie.connection(with: .video) != nil else { return done(MigoCameraFailure("the camera records no video")) }
            guard !movie.isRecording, starting == nil else { return done(MigoCameraFailure("already recording")) }
            starting = done
            movie.startRecording(to: Self.staged("mov"), recordingDelegate: self)
        }

        func fileOutput(_ output: AVCaptureFileOutput, didStartRecordingTo fileURL: URL, from connections: [AVCaptureConnection]) {
            DispatchQueue.main.async { [self] in
                let done = starting
                starting = nil
                done?(nil)
            }
        }

        func stopRecord(compressed: Bool, done: @escaping (Result<MigoCameraVideo, MigoCameraFailure>) -> Void) {
            guard movie.isRecording, stopping == nil else { return done(.failure(MigoCameraFailure("not recording"))) }
            stopping = (compressed, done)
            movie.stopRecording()
        }

        func fileOutput(
            _ output: AVCaptureFileOutput, didFinishRecordingTo outputFileURL: URL, from connections: [AVCaptureConnection],
            error: Error?
        ) {
            // A recording that ended on its own -- the disk filled, the app left -- may still have written its video.
            let written = (error as NSError?)?.userInfo[AVErrorRecordingSuccessfullyFinishedKey] as? Bool ?? (error == nil)
            DispatchQueue.main.async { [self] in
                if let done = starting {
                    // It ended before it began.
                    starting = nil
                    done(MigoCameraFailure("the recording did not start"))
                }
                guard let (compressed, done) = stopping else {
                    try? FileManager.default.removeItem(at: outputFileURL)
                    return
                }
                stopping = nil
                guard written else { return done(.failure(MigoCameraFailure("the video could not be written"))) }
                work.async { Self.finish(outputFileURL, compressed: compressed, done: done) }
            }
        }

        /// The recording's cover, and with `compressed` the video again at a size to send; answered on the main thread.
        private static func finish(
            _ recorded: URL, compressed: Bool, done: @escaping (Result<MigoCameraVideo, MigoCameraFailure>) -> Void
        ) {
            let asset = AVURLAsset(url: recorded)
            let pictures = AVAssetImageGenerator(asset: asset)
            pictures.appliesPreferredTrackTransform = true
            let cover = staged("jpg")
            guard let first = try? pictures.copyCGImage(at: .zero, actualTime: nil),
                let data = UIImage(cgImage: first).jpegData(compressionQuality: 0.8), (try? data.write(to: cover)) != nil
            else {
                return DispatchQueue.main.async { done(.failure(MigoCameraFailure("the video's cover could not be made"))) }
            }
            guard compressed, let export = AVAssetExportSession(asset: asset, presetName: AVAssetExportPresetMediumQuality)
            else {
                return DispatchQueue.main.async { done(.success(MigoCameraVideo(video: recorded.path, cover: cover.path))) }
            }
            let smaller = staged("mp4")
            export.outputURL = smaller
            export.outputFileType = .mp4
            export.exportAsynchronously {
                let exported = export.status == .completed
                // The one content is handed; the other goes.
                try? FileManager.default.removeItem(at: exported ? recorded : smaller)
                DispatchQueue.main.async {
                    done(.success(MigoCameraVideo(video: (exported ? smaller : recorded).path, cover: cover.path)))
                }
            }
        }

        // MARK: - zoom and frames

        func zoom(to factor: Double) -> Double {
            gate.lock()
            let camera = self.camera
            gate.unlock()
            guard let camera, (try? camera.lockForConfiguration()) != nil else { return 1 }
            defer { camera.unlockForConfiguration() }
            // From 1 to the camera's largest, to a tenth.
            let largest = Double(camera.activeFormat.videoMaxZoomFactor)
            let taken = (min(max(factor, 1), largest) * 10).rounded(.down) / 10
            camera.videoZoomFactor = CGFloat(max(taken, 1))
            return max(taken, 1)
        }

        func listen(_ on: Bool) {
            gate.lock()
            listening = on
            gate.unlock()
        }

        func captureOutput(_ output: AVCaptureOutput, didOutput sampleBuffer: CMSampleBuffer, from connection: AVCaptureConnection) {
            gate.lock()
            let wanted = listening && !delivering
            if wanted { delivering = true }
            gate.unlock()
            guard wanted, let picture = CMSampleBufferGetImageBuffer(sampleBuffer) else {
                if wanted { delivered() }
                return
            }
            // Read where it is handed over: the picture is kept until then, and copied nowhere on the way.
            DispatchQueue.main.async { [self] in
                defer { delivered() }
                guard !closed, let frame, CVPixelBufferLockBaseAddress(picture, .readOnly) == kCVReturnSuccess else { return }
                defer { CVPixelBufferUnlockBaseAddress(picture, .readOnly) }
                guard let pixels = CVPixelBufferGetBaseAddress(picture) else { return }
                frame(
                    MigoCameraPixels(
                        width: CVPixelBufferGetWidth(picture), height: CVPixelBufferGetHeight(picture),
                        rowStride: CVPixelBufferGetBytesPerRow(picture), bgra: true, pixels: pixels))
            }
        }

        private func delivered() {
            gate.lock()
            delivering = false
            gate.unlock()
        }

        // MARK: - closing

        func close() {
            guard !closed else { return }
            closed = true
            observers.forEach { NotificationCenter.default.removeObserver($0) }
            observers = []
            listen(false)
            stopped = nil
            frame = nil
            preview?.removeFromSuperview()
            preview = nil
            let failure = MigoCameraFailure("camera destroyed")
            let waiting = photoWaiters
            photoWaiters = [:]
            waiting.values.forEach { $0(.failure(failure)) }
            starting?(failure)
            starting = nil
            stopping?.done(.failure(failure))
            stopping = nil
            work.async { [self] in
                if movie.isRecording { movie.stopRecording() }
                session.stopRunning()
            }
        }

        /// A new file's place in the system's temporary directory: the engine takes the file as the call completes.
        private static func staged(_ kind: String) -> URL {
            FileManager.default.temporaryDirectory.appendingPathComponent("camera-\(UUID().uuidString).\(kind)")
        }
    }
#endif

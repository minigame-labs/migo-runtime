#if os(iOS)
    import MigoAppleRenderer
    import UIKit
    import UniformTypeIdentifiers

    /// The system's camera screen, up for one photo or one video: what `chooseImage` and `chooseMedia` show a player
    /// when content allows the camera only. The screen, its shutter and its "use" step are the system's
    /// (`UIImagePickerController`); what the player keeps is written under the image service's staging directory --
    /// a photo as a JPEG, a video as the system recorded it -- and handed over once.
    final class MigoCameraScreen: NSObject, UIImagePickerControllerDelegate, UINavigationControllerDelegate {
        private let picker = UIImagePickerController()
        private let staging: URL
        private var completion: ((MigoCaptureOutcome) -> Void)?

        init(
            video: Bool, maxDuration: TimeInterval, front: Bool, staging: URL,
            completion: @escaping (MigoCaptureOutcome) -> Void
        ) {
            self.staging = staging
            self.completion = completion
            super.init()
            picker.sourceType = .camera
            // The kinds first: the capture mode must be one of them.
            picker.mediaTypes = [(video ? UTType.movie : UTType.image).identifier]
            picker.cameraCaptureMode = video ? .video : .photo
            if video {
                picker.videoMaximumDuration = maxDuration
                picker.videoQuality = .typeHigh
            }
            let device: UIImagePickerController.CameraDevice = front ? .front : .rear
            if UIImagePickerController.isCameraDeviceAvailable(device) { picker.cameraDevice = device }
            // Over the game, which stays in its window: a game view that leaves its window ends its game.
            picker.modalPresentationStyle = .overFullScreen
            picker.delegate = self
            // The screen's own controls are the system's, which shows a UI test none of them: this is how one
            // knows the screen is up.
            picker.view.accessibilityIdentifier = Self.identifier
        }

        /// What the screen's view is named to a UI test, whatever the system's language.
        static let identifier = "migo_camera_screen"

        func show() {
            MigoSystemPresentation.present(picker, shown: { [weak self] up in
                if !up { self?.finish(.unavailable("there is no window to show it in")) }
            })
        }

        /// Takes the screen down, if it is up, and answers -- once.
        func finish(_ outcome: MigoCaptureOutcome) {
            guard let completion else { return }
            self.completion = nil
            if picker.presentingViewController != nil { picker.dismiss(animated: true) }
            completion(outcome)
        }

        func imagePickerControllerDidCancel(_ picker: UIImagePickerController) {
            finish(.cancelled)
        }

        func imagePickerController(
            _ picker: UIImagePickerController,
            didFinishPickingMediaWithInfo info: [UIImagePickerController.InfoKey: Any]
        ) {
            let staging = staging
            let taken = info[.mediaURL] as? URL
            let photo = info[.originalImage] as? UIImage
            // Encoding a photo takes a while, and moving a video may: neither is the main thread's.
            DispatchQueue.global(qos: .userInitiated).async { [weak self] in
                let outcome = Self.keep(video: taken, photo: photo, under: staging)
                DispatchQueue.main.async { self?.finish(outcome) }
            }
        }

        private static func keep(video: URL?, photo: UIImage?, under staging: URL) -> MigoCaptureOutcome {
            let name = "camera-\(UUID().uuidString)"
            do {
                try FileManager.default.createDirectory(at: staging, withIntermediateDirectories: true)
                if let video {
                    let kept = staging.appendingPathComponent(name).appendingPathExtension(
                        video.pathExtension.isEmpty ? "mov" : video.pathExtension)
                    try FileManager.default.moveItem(at: video, to: kept)
                    return .captured(kept)
                }
                guard let data = photo?.jpegData(compressionQuality: 0.92) else {
                    return .unavailable("the camera took nothing")
                }
                let kept = staging.appendingPathComponent(name).appendingPathExtension("jpg")
                try data.write(to: kept)
                return .captured(kept)
            } catch {
                return .unavailable("the file could not be kept: \(error.localizedDescription)")
            }
        }
    }
#endif

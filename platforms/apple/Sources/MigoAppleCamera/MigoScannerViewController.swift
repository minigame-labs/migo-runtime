#if os(iOS)
    import AVFoundation
    import PhotosUI
    import UIKit
    import Vision

    /// scanCode's scanner: the back camera's picture under a frame, the code in the frame read as it comes, and --
    /// unless content asked for the camera only -- a picture from the photo library read instead. The camera reads
    /// codes itself (`AVCaptureMetadataOutput`, limited to the frame); a picture is read by Vision. Its buttons are
    /// named for UI tests: `migo_scan_album`, `migo_scan_cancel`. Main thread.
    final class MigoScannerViewController: UIViewController {
        enum Outcome {
            case code([String: Any])
            case cancelled
            case failed(String)
        }

        /// The frame's side, as a share of the screen's shorter side.
        static let frameFraction: CGFloat = 0.7

        private let kinds: MigoCodeKinds
        private let cameraOnly: Bool
        private var completion: ((Outcome) -> Void)?
        private let session = AVCaptureSession()
        /// Where the session is configured, started and stopped: off the main thread, as AVFoundation asks.
        private let sessionQueue = DispatchQueue(label: "dev.migo.scan.session")
        private var preview: AVCaptureVideoPreviewLayer?
        private var output: AVCaptureMetadataOutput?
        private let dim = CAShapeLayer()
        private let corners = CAShapeLayer()
        private let hint = UILabel()

        init(kinds: MigoCodeKinds, cameraOnly: Bool, completion: @escaping (Outcome) -> Void) {
            self.kinds = kinds
            self.cameraOnly = cameraOnly
            self.completion = completion
            super.init(nibName: nil, bundle: nil)
            // Over the game, which stays in its window: a full-screen presentation takes the game view out of it,
            // and a game view out of its window is one the lane tears down and loads again.
            modalPresentationStyle = .overFullScreen
        }

        @available(*, unavailable)
        required init?(coder: NSCoder) { fatalError("not from a storyboard") }

        override var prefersStatusBarHidden: Bool { true }

        override func viewDidLoad() {
            super.viewDidLoad()
            view.backgroundColor = .black
            dim.fillRule = .evenOdd
            dim.fillColor = UIColor(white: 0, alpha: 0.6).cgColor
            corners.strokeColor = UIColor(red: 0.13, green: 0.75, blue: 0.38, alpha: 1).cgColor
            corners.fillColor = nil
            corners.lineWidth = 4
            view.layer.addSublayer(dim)
            view.layer.addSublayer(corners)

            hint.text = MigoScannerText.hint
            hint.textColor = .white
            hint.font = .preferredFont(forTextStyle: .subheadline)
            hint.textAlignment = .center
            hint.numberOfLines = 0
            hint.translatesAutoresizingMaskIntoConstraints = false
            view.addSubview(hint)

            let cancel = UIButton(type: .system)
            cancel.setImage(UIImage(systemName: "xmark"), for: .normal)
            cancel.tintColor = .white
            cancel.accessibilityIdentifier = "migo_scan_cancel"
            cancel.accessibilityLabel = MigoScannerText.cancel
            cancel.addTarget(self, action: #selector(cancelTapped), for: .touchUpInside)
            cancel.translatesAutoresizingMaskIntoConstraints = false
            view.addSubview(cancel)

            var constraints = [
                hint.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 24),
                hint.trailingAnchor.constraint(equalTo: view.trailingAnchor, constant: -24),
                hint.topAnchor.constraint(
                    equalTo: view.centerYAnchor,
                    constant: min(UIScreen.main.bounds.width, UIScreen.main.bounds.height) * Self.frameFraction / 2 + 24),
                cancel.leadingAnchor.constraint(equalTo: view.safeAreaLayoutGuide.leadingAnchor, constant: 16),
                cancel.topAnchor.constraint(equalTo: view.safeAreaLayoutGuide.topAnchor, constant: 8),
                cancel.widthAnchor.constraint(equalToConstant: 44),
                cancel.heightAnchor.constraint(equalToConstant: 44),
            ]
            if !cameraOnly {
                var configuration = UIButton.Configuration.gray()
                configuration.image = UIImage(systemName: "photo.on.rectangle")
                configuration.title = MigoScannerText.album
                configuration.imagePadding = 8
                let album = UIButton(configuration: configuration)
                album.accessibilityIdentifier = "migo_scan_album"
                album.addTarget(self, action: #selector(albumTapped), for: .touchUpInside)
                album.translatesAutoresizingMaskIntoConstraints = false
                view.addSubview(album)
                constraints += [
                    album.centerXAnchor.constraint(equalTo: view.centerXAnchor),
                    album.bottomAnchor.constraint(equalTo: view.safeAreaLayoutGuide.bottomAnchor, constant: -32),
                ]
            }
            NSLayoutConstraint.activate(constraints)
        }

        override func viewDidLayoutSubviews() {
            super.viewDidLayoutSubviews()
            preview?.frame = view.bounds
            let frame = frameRect()
            let outside = UIBezierPath(rect: view.bounds)
            outside.append(UIBezierPath(rect: frame))
            dim.path = outside.cgPath
            corners.path = Self.cornerPath(frame).cgPath
            limitToFrame()
        }

        override func viewWillAppear(_ animated: Bool) {
            super.viewWillAppear(animated)
            switch AVCaptureDevice.authorizationStatus(for: .video) {
            case .authorized:
                startCamera()
            case .notDetermined where Self.declaresPurpose:
                AVCaptureDevice.requestAccess(for: .video) { granted in
                    DispatchQueue.main.async { [weak self] in
                        if granted { self?.startCamera() } else { self?.cameraUnavailable() }
                    }
                }
            default:
                cameraUnavailable()
            }
        }

        override func viewWillDisappear(_ animated: Bool) {
            super.viewWillDisappear(animated)
            sessionQueue.async { [session] in session.stopRunning() }
        }

        /// The Info.plist key the system's prompt needs: without it the prompt never comes and asking crashes.
        static var declaresPurpose: Bool {
            (Bundle.main.object(forInfoDictionaryKey: "NSCameraUsageDescription") as? String)?.isEmpty == false
        }

        // MARK: - the camera

        private func startCamera() {
            guard preview == nil else { return }
            guard let device = AVCaptureDevice.default(.builtInWideAngleCamera, for: .video, position: .back)
                ?? AVCaptureDevice.default(for: .video),
                let input = try? AVCaptureDeviceInput(device: device), session.canAddInput(input)
            else {
                cameraUnavailable()
                return
            }
            let output = AVCaptureMetadataOutput()
            session.beginConfiguration()
            session.sessionPreset = .high
            session.addInput(input)
            guard session.canAddOutput(output) else {
                session.commitConfiguration()
                cameraUnavailable()
                return
            }
            session.addOutput(output)
            output.setMetadataObjectsDelegate(self, queue: .main)
            output.metadataObjectTypes = kinds.metadataTypes.filter(output.availableMetadataObjectTypes.contains)
            session.commitConfiguration()
            self.output = output

            let preview = AVCaptureVideoPreviewLayer(session: session)
            preview.videoGravity = .resizeAspectFill
            preview.frame = view.bounds
            view.layer.insertSublayer(preview, at: 0)
            self.preview = preview
            sessionQueue.async { [weak self, session] in
                session.startRunning()
                DispatchQueue.main.async { self?.limitToFrame() }
            }
        }

        /// The camera reads only the frame: its rectangle in the preview's terms, once the preview runs.
        private func limitToFrame() {
            guard let preview, let output, session.isRunning else { return }
            output.rectOfInterest = preview.metadataOutputRectConverted(fromLayerRect: frameRect())
        }

        /// No camera: the album alone, or -- the camera being all content allowed -- the end.
        private func cameraUnavailable() {
            if cameraOnly {
                finish(.failed("system permission denied"))
            } else {
                hint.text = MigoScannerText.noCamera
            }
        }

        private func frameRect() -> CGRect {
            let side = min(view.bounds.width, view.bounds.height) * Self.frameFraction
            return CGRect(x: view.bounds.midX - side / 2, y: view.bounds.midY - side / 2, width: side, height: side)
        }

        private static func cornerPath(_ frame: CGRect) -> UIBezierPath {
            let arm = frame.width / 8
            let path = UIBezierPath()
            for (corner, dx, dy) in [
                (CGPoint(x: frame.minX, y: frame.minY), arm, arm), (CGPoint(x: frame.maxX, y: frame.minY), -arm, arm),
                (CGPoint(x: frame.minX, y: frame.maxY), arm, -arm), (CGPoint(x: frame.maxX, y: frame.maxY), -arm, -arm),
            ] {
                path.move(to: CGPoint(x: corner.x + dx, y: corner.y))
                path.addLine(to: corner)
                path.addLine(to: CGPoint(x: corner.x, y: corner.y + dy))
            }
            return path
        }

        // MARK: - the album

        @objc private func albumTapped() {
            var configuration = PHPickerConfiguration()
            configuration.selectionLimit = 1
            configuration.filter = .images
            let picker = PHPickerViewController(configuration: configuration)
            picker.delegate = self
            present(picker, animated: true)
        }

        /// The code in `image`, read by Vision on a queue of its own; a picture with none says so.
        private func read(_ image: CGImage, orientation: CGImagePropertyOrientation) {
            DispatchQueue.global(qos: .userInitiated).async { [weak self, kinds] in
                let found = kinds.read(image, orientation: orientation)
                DispatchQueue.main.async {
                    if let found { self?.finish(.code(found)) } else { self?.hint.text = MigoScannerText.noCode }
                }
            }
        }

        // MARK: - the end

        @objc private func cancelTapped() { finish(.cancelled) }

        /// The scanner ends, answered once: it leaves, and then its caller hears.
        func finish(_ outcome: Outcome) {
            guard let completion else { return }
            self.completion = nil
            output?.setMetadataObjectsDelegate(nil, queue: nil)
            if presentingViewController != nil {
                dismiss(animated: true) { completion(outcome) }
            } else {
                completion(outcome)
            }
        }
    }

    extension MigoScannerViewController: AVCaptureMetadataOutputObjectsDelegate {
        func metadataOutput(
            _ output: AVCaptureMetadataOutput, didOutput objects: [AVMetadataObject], from connection: AVCaptureConnection
        ) {
            for case let code as AVMetadataMachineReadableCodeObject in objects {
                guard let text = code.stringValue, let name = MigoCodeKinds.name(of: code.type) else { continue }
                finish(.code(MigoCodeKinds.answer(text: text, scanType: name)))
                return
            }
        }
    }

    extension MigoScannerViewController: PHPickerViewControllerDelegate {
        func picker(_ picker: PHPickerViewController, didFinishPicking results: [PHPickerResult]) {
            picker.dismiss(animated: true)
            guard let provider = results.first?.itemProvider, provider.canLoadObject(ofClass: UIImage.self) else { return }
            provider.loadObject(ofClass: UIImage.self) { object, _ in
                DispatchQueue.main.async { [weak self] in
                    guard let image = object as? UIImage, let cgImage = image.cgImage else {
                        self?.hint.text = MigoScannerText.noCode
                        return
                    }
                    self?.read(cgImage, orientation: CGImagePropertyOrientation(image.imageOrientation))
                }
            }
        }
    }

    extension CGImagePropertyOrientation {
        init(_ orientation: UIImage.Orientation) {
            switch orientation {
            case .up: self = .up
            case .down: self = .down
            case .left: self = .left
            case .right: self = .right
            case .upMirrored: self = .upMirrored
            case .downMirrored: self = .downMirrored
            case .leftMirrored: self = .leftMirrored
            case .rightMirrored: self = .rightMirrored
            @unknown default: self = .up
            }
        }
    }

    /// The scanner's words, in the player's language.
    enum MigoScannerText {
        static var hint: String { localized("scan.hint") }
        static var album: String { localized("scan.album") }
        static var cancel: String { localized("scan.cancel") }
        static var noCode: String { localized("scan.noCode") }
        static var noCamera: String { localized("scan.noCamera") }

        /// The product's resources, where the words are.
        static var bundle: Bundle { .module }

        private static func localized(_ key: String) -> String {
            NSLocalizedString(key, bundle: bundle, comment: "")
        }
    }
#endif

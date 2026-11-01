import Foundation
import UniformTypeIdentifiers

/// previewImage and previewMedia, answered with the system's viewer,
/// QuickLook: images and videos alike, from the one content asked for.
///
/// The engine lends a request's files only until the call is answered, and the
/// call is answered once the viewer shows -- while it still does. So every item
/// is the host's own first: a file is copied into the staging directory (a
/// clone on APFS, which costs no space), an http(s) URL downloaded there, with
/// no referrer whatever `referrerPolicy` says, since the host has none to send.
/// The copies go when the viewer closes. The viewer's own controls are shown
/// whatever `showmenu` says.
final class MigoSystemPreview {
    private let presenter: MigoPreviewPresenter
    private let staging: URL
    private let downloads: URLSession

    init(
        presenter: MigoPreviewPresenter = MigoSystemPreviewPresenter(),
        staging: URL = MigoSystemImages.defaultStaging,
        configuration: URLSessionConfiguration = .ephemeral
    ) {
        self.presenter = presenter
        self.staging = staging
        downloads = URLSession(configuration: configuration)
    }

    deinit {
        downloads.invalidateAndCancel()
    }

    func handle(_ request: MigoHostServiceRequest) {
        guard let reply = request.reply else { return }
        let options = request.object
        let urls: [String]
        let current: Int
        switch request.method {
        case MigoHostServiceID.Image.previewImage:
            urls = (options["urls"] as? [Any] ?? []).compactMap { $0 as? String }
            current = (options["current"] as? String).flatMap(urls.firstIndex(of:)) ?? 0
        case MigoHostServiceID.Image.previewMedia:
            urls = (options["sources"] as? [Any] ?? []).compactMap { ($0 as? [String: Any])?["url"] as? String }
            current = (options["current"] as? NSNumber)?.intValue ?? 0
        default:
            reply.fail("not supported")
            return
        }
        guard !urls.isEmpty else {
            reply.fail("nothing to preview")
            return
        }
        prepare(urls) { [weak self] prepared in
            guard let self else { return }
            switch prepared {
            case .failure(let error):
                reply.fail(MigoSystemImages.reason(error))
            case .success(let files):
                let index = min(max(current, 0), files.count - 1)
                self.presenter.present(
                    files, at: index,
                    shown: { shown in
                        if shown {
                            reply.succeed()
                        } else {
                            Self.remove(files)
                            reply.fail("there is no window to show it in")
                        }
                    },
                    dismissed: { Self.remove(files) })
            }
        }
    }

    // MARK: - the host's copies

    /// Every item as a file of the host's, in order, on the main thread.
    private func prepare(_ urls: [String], completion: @escaping (Result<[URL], Error>) -> Void) {
        var files = [URL?](repeating: nil, count: urls.count)
        var failure: Error?
        let group = DispatchGroup()
        let lock = NSLock()
        func settle(_ index: Int, _ result: Result<URL, Error>) {
            lock.lock()
            switch result {
            case .success(let file): files[index] = file
            case .failure(let error): failure = failure ?? error
            }
            lock.unlock()
            group.leave()
        }
        for (index, string) in urls.enumerated() {
            group.enter()
            guard let url = URL(string: string), let scheme = url.scheme?.lowercased(),
                scheme == "http" || scheme == "https"
            else {
                settle(index, Result { try self.copy(URL(fileURLWithPath: string)) })
                continue
            }
            downloads.downloadTask(with: url) { location, response, error in
                settle(
                    index,
                    Result {
                        guard let location else { throw error ?? MigoImageError("download failed") }
                        if let status = (response as? HTTPURLResponse)?.statusCode, !(200..<300).contains(status) {
                            throw MigoImageError("download failed: HTTP \(status)")
                        }
                        return try self.keep(location, as: Self.fileExtension(of: url, response: response))
                    })
            }.resume()
        }
        group.notify(queue: .main) {
            let kept = files.compactMap { $0 }
            if let failure {
                Self.remove(kept)
                completion(.failure(failure))
            } else {
                completion(.success(kept))
            }
        }
    }

    private func copy(_ file: URL) throws -> URL {
        guard FileManager.default.isReadableFile(atPath: file.path) else {
            throw MigoImageError("\(file.lastPathComponent) cannot be read")
        }
        let copy = try destination(extension: file.pathExtension)
        try FileManager.default.copyItem(at: file, to: copy)
        return copy
    }

    private func keep(_ download: URL, as ext: String) throws -> URL {
        let kept = try destination(extension: ext)
        try FileManager.default.moveItem(at: download, to: kept)
        return kept
    }

    private func destination(extension ext: String) throws -> URL {
        try FileManager.default.createDirectory(at: staging, withIntermediateDirectories: true)
        let name = UUID().uuidString
        return ext.isEmpty
            ? staging.appendingPathComponent(name) : staging.appendingPathComponent(name).appendingPathExtension(ext)
    }

    /// The viewer tells a file's kind by its extension: the URL's, or else the
    /// one the response's type has.
    static func fileExtension(of url: URL, response: URLResponse?) -> String {
        if !url.pathExtension.isEmpty { return url.pathExtension }
        if let mime = response?.mimeType, let ext = UTType(mimeType: mime)?.preferredFilenameExtension { return ext }
        return ""
    }

    private static func remove(_ files: [URL]) {
        for file in files { try? FileManager.default.removeItem(at: file) }
    }
}

/// Shows files in the system's viewer, on the main thread.
protocol MigoPreviewPresenter: AnyObject {
    /// Show `files` from the one at `index`. `shown` says whether it could;
    /// `dismissed` follows when the player closes it.
    func present(
        _ files: [URL], at index: Int, shown: @escaping (Bool) -> Void, dismissed: @escaping () -> Void)
}

#if os(iOS)
    import QuickLook
    import UIKit

    /// iOS: QuickLook's preview controller, full screen over the game. A
    /// preview asked for while one shows goes over it; each keeps its own files
    /// until it closes.
    final class MigoSystemPreviewPresenter: MigoPreviewPresenter {
        /// The viewers up, kept until they close.
        private var showing: [ObjectIdentifier: Viewer] = [:]

        func present(
            _ files: [URL], at index: Int, shown: @escaping (Bool) -> Void, dismissed: @escaping () -> Void
        ) {
            let viewer = Viewer(files: files)
            let key = ObjectIdentifier(viewer)
            viewer.closed = { [weak self] in
                self?.showing[key] = nil
                dismissed()
            }
            showing[key] = viewer
            let controller = QLPreviewController()
            controller.dataSource = viewer
            controller.delegate = viewer
            controller.currentPreviewItemIndex = index
            // Over the game, not in place of it: a full-screen presentation takes
            // the views below out of the window when it is up, and the game view
            // leaving its window is the app ending the game.
            controller.modalPresentationStyle = .overFullScreen
            MigoSystemPresentation.present(controller) { [weak self] up in
                if !up { self?.showing[key] = nil }
                shown(up)
            }
        }

        private final class Viewer: NSObject, QLPreviewControllerDataSource, QLPreviewControllerDelegate {
            let files: [URL]
            var closed: (() -> Void)?

            init(files: [URL]) { self.files = files }

            func numberOfPreviewItems(in controller: QLPreviewController) -> Int { files.count }

            func previewController(_ controller: QLPreviewController, previewItemAt index: Int) -> QLPreviewItem {
                files[index] as NSURL
            }

            func previewControllerDidDismiss(_ controller: QLPreviewController) {
                let closed = self.closed
                self.closed = nil
                closed?()
            }
        }
    }

#elseif os(macOS)
    import AppKit
    import Quartz

    /// A Mac: a window of QuickLook's preview view over the game's, with two
    /// buttons to step through several items. A preview asked for while one
    /// shows replaces it.
    final class MigoSystemPreviewPresenter: NSObject, MigoPreviewPresenter, NSWindowDelegate {
        private var window: NSWindow?
        private var preview: QLPreviewView?
        private var stepper: NSSegmentedControl?
        private var files: [URL] = []
        private var index = 0
        private var dismissed: (() -> Void)?

        func present(
            _ files: [URL], at index: Int, shown: @escaping (Bool) -> Void, dismissed: @escaping () -> Void
        ) {
            window?.close()
            let owner = NSApp.keyWindow ?? NSApp.mainWindow
            let screen = owner?.screen ?? NSScreen.main
            guard let screen, let preview = QLPreviewView(frame: .zero, style: .normal) else {
                shown(false)
                return
            }
            self.files = files
            self.index = index
            self.dismissed = dismissed
            let visible = screen.visibleFrame
            let frame = visible.insetBy(dx: visible.width * 0.15, dy: visible.height * 0.1)
            let window = NSWindow(
                contentRect: frame, styleMask: [.titled, .closable, .resizable, .miniaturizable], backing: .buffered,
                defer: false)
            window.isReleasedWhenClosed = false
            window.delegate = self
            let content = NSView()
            preview.translatesAutoresizingMaskIntoConstraints = false
            content.addSubview(preview)
            var constraints = [
                preview.leadingAnchor.constraint(equalTo: content.leadingAnchor),
                preview.trailingAnchor.constraint(equalTo: content.trailingAnchor),
                preview.topAnchor.constraint(equalTo: content.topAnchor),
            ]
            if files.count > 1 {
                let stepper = NSSegmentedControl(
                    images: [
                        NSImage(named: NSImage.goLeftTemplateName)!, NSImage(named: NSImage.goRightTemplateName)!,
                    ], trackingMode: .momentary, target: self, action: #selector(step(_:)))
                stepper.translatesAutoresizingMaskIntoConstraints = false
                content.addSubview(stepper)
                constraints += [
                    stepper.centerXAnchor.constraint(equalTo: content.centerXAnchor),
                    stepper.bottomAnchor.constraint(equalTo: content.bottomAnchor, constant: -8),
                    preview.bottomAnchor.constraint(equalTo: stepper.topAnchor, constant: -8),
                ]
                self.stepper = stepper
            } else {
                constraints.append(preview.bottomAnchor.constraint(equalTo: content.bottomAnchor))
            }
            NSLayoutConstraint.activate(constraints)
            window.contentView = content
            self.window = window
            self.preview = preview
            show()
            window.makeKeyAndOrderFront(nil)
            shown(true)
        }

        private func show() {
            guard files.indices.contains(index) else { return }
            preview?.previewItem = files[index] as NSURL
            window?.title =
                files.count > 1
                ? "\(files[index].lastPathComponent) (\(index + 1)/\(files.count))" : files[index].lastPathComponent
            stepper?.setEnabled(index > 0, forSegment: 0)
            stepper?.setEnabled(index < files.count - 1, forSegment: 1)
        }

        @objc private func step(_ sender: NSSegmentedControl) {
            index = min(max(index + (sender.selectedSegment == 0 ? -1 : 1), 0), files.count - 1)
            show()
        }

        func windowWillClose(_ notification: Notification) {
            preview?.close()
            preview = nil
            stepper = nil
            window = nil
            files = []
            let dismissed = self.dismissed
            self.dismissed = nil
            dismissed?()
        }
    }
#endif

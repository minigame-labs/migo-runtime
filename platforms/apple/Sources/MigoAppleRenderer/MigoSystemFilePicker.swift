import Foundation
import UniformTypeIdentifiers

#if os(iOS)
    import PhotosUI
    import UIKit
#elseif os(macOS)
    import AppKit
#endif

/// What a picker is asked to show.
enum MigoPickKind: Hashable {
    case image
    case video
    /// Any file.
    case file
}

struct MigoPickRequest {
    enum Source {
        /// The photo library where there is one -- iOS -- and files elsewhere.
        case library
        /// The file system.
        case files
    }

    /// The most the player may pick.
    var count: Int
    var kinds: Set<MigoPickKind>
    var source: Source
    /// Where the picked files are copied to.
    var staging: URL
    /// Only files with these extensions, lowercased; none for any.
    var extensions: Set<String> = []

    /// Between the copy's own prefix and the name the player saw.
    static let namePrefixSeparator = "__"

    /// The types the system's picker filters by.
    var contentTypes: [UTType] {
        if !extensions.isEmpty {
            let named = extensions.compactMap { UTType(filenameExtension: $0) }
            if !named.isEmpty { return named }
        }
        if kinds.contains(.file) { return [.item] }
        var types: [UTType] = []
        if kinds.contains(.image) { types.append(.image) }
        if kinds.contains(.video) { types.append(.movie) }
        return types.isEmpty ? [.item] : types
    }

    /// A copy of the picked file at `url`, in `staging`, under a name of its own
    /// that keeps the player's.
    func copy(_ url: URL) throws -> URL {
        try FileManager.default.createDirectory(at: staging, withIntermediateDirectories: true)
        let copy = staging.appendingPathComponent(
            UUID().uuidString + Self.namePrefixSeparator + url.lastPathComponent)
        try FileManager.default.copyItem(at: url, to: copy)
        return copy
    }
}

enum MigoPickOutcome {
    /// Copies of what the player picked, the host's own.
    case picked([URL])
    case cancelled
    /// There was nowhere to show the picker, with why.
    case unavailable(String)
}

/// Shows the system's picker and answers once, on the main thread.
protocol MigoFilePicker: AnyObject {
    func pick(_ request: MigoPickRequest, completion: @escaping (MigoPickOutcome) -> Void)
}

#if os(iOS)
    /// iOS: the photo library through PHPicker, which needs no access to the
    /// library and no purpose string, and files through the document picker.
    final class MigoSystemFilePicker: NSObject, MigoFilePicker {
        /// The one picker up, with what answers it.
        private var pending: (request: MigoPickRequest, completion: (MigoPickOutcome) -> Void)?

        func pick(_ request: MigoPickRequest, completion: @escaping (MigoPickOutcome) -> Void) {
            guard pending == nil else {
                completion(.unavailable("another picker is showing"))
                return
            }
            pending = (request, completion)
            let shown: (Bool) -> Void = { [weak self] up in
                if !up { self?.finish(.unavailable("there is no window to show it in")) }
            }
            switch request.source {
            case .library:
                var configuration = PHPickerConfiguration()
                configuration.selectionLimit = request.count
                // What content can draw and play: HEIC arrives as JPEG.
                configuration.preferredAssetRepresentationMode = .compatible
                var filters: [PHPickerFilter] = []
                if request.kinds.contains(.image) { filters.append(.images) }
                if request.kinds.contains(.video) { filters.append(.videos) }
                configuration.filter = .any(of: filters)
                let picker = PHPickerViewController(configuration: configuration)
                picker.delegate = self
                MigoSystemPresentation.present(picker, shown: shown)
            case .files:
                let picker = UIDocumentPickerViewController(forOpeningContentTypes: request.contentTypes, asCopy: true)
                picker.allowsMultipleSelection = request.count > 1
                picker.delegate = self
                MigoSystemPresentation.present(picker, shown: shown)
            }
        }

        private func finish(_ outcome: MigoPickOutcome) {
            let completion = pending?.completion
            pending = nil
            completion?(outcome)
        }

        private func copies(_ urls: [URL]) {
            guard let request = pending?.request else { return }
            do {
                finish(.picked(try urls.prefix(request.count).map(request.copy)))
            } catch {
                finish(.unavailable(error.localizedDescription))
            }
        }
    }

    extension MigoSystemFilePicker: PHPickerViewControllerDelegate {
        func picker(_ picker: PHPickerViewController, didFinishPicking results: [PHPickerResult]) {
            picker.dismiss(animated: true)
            guard let request = pending?.request else { return }
            guard !results.isEmpty else {
                finish(.cancelled)
                return
            }
            // Each item's file lives only for its callback: copied there.
            var copies = [URL?](repeating: nil, count: results.count)
            var failure: Error?
            let group = DispatchGroup()
            let lock = NSLock()
            for (index, result) in results.prefix(request.count).enumerated() {
                let provider = result.itemProvider
                let type: UTType =
                    provider.hasItemConformingToTypeIdentifier(UTType.movie.identifier) ? .movie : .image
                group.enter()
                provider.loadFileRepresentation(forTypeIdentifier: type.identifier) { url, error in
                    defer { group.leave() }
                    lock.lock()
                    defer { lock.unlock() }
                    do {
                        guard let url else { throw error ?? MigoImageError("the picked item could not be read") }
                        copies[index] = try request.copy(url)
                    } catch {
                        failure = error
                    }
                }
            }
            group.notify(queue: .main) { [weak self] in
                if let failure {
                    copies.compactMap { $0 }.forEach { try? FileManager.default.removeItem(at: $0) }
                    self?.finish(.unavailable(failure.localizedDescription))
                } else {
                    self?.finish(.picked(copies.compactMap { $0 }))
                }
            }
        }
    }

    extension MigoSystemFilePicker: UIDocumentPickerDelegate {
        func documentPicker(_ controller: UIDocumentPickerViewController, didPickDocumentsAt urls: [URL]) {
            copies(urls)
        }

        func documentPickerWasCancelled(_ controller: UIDocumentPickerViewController) {
            finish(.cancelled)
        }
    }

#elseif os(macOS)
    /// A Mac: the open panel, for images, media and files alike.
    final class MigoSystemFilePicker: MigoFilePicker {
        func pick(_ request: MigoPickRequest, completion: @escaping (MigoPickOutcome) -> Void) {
            let panel = NSOpenPanel()
            panel.canChooseFiles = true
            panel.canChooseDirectories = false
            panel.allowsMultipleSelection = request.count > 1
            panel.allowedContentTypes = request.contentTypes
            let finish: (NSApplication.ModalResponse) -> Void = { response in
                guard response == .OK else {
                    completion(.cancelled)
                    return
                }
                do {
                    // The panel cannot cap a selection: the first `count` are taken.
                    completion(.picked(try panel.urls.prefix(request.count).map(request.copy)))
                } catch {
                    completion(.unavailable(error.localizedDescription))
                }
            }
            if let window = NSApp.keyWindow ?? NSApp.mainWindow {
                panel.beginSheetModal(for: window, completionHandler: finish)
            } else {
                panel.begin(completionHandler: finish)
            }
        }
    }
#endif

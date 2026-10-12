import AVFoundation
import Foundation
import ImageIO
import UniformTypeIdentifiers

#if os(iOS)
    import UIKit
#elseif os(macOS)
    import AppKit
#endif

/// The image service, answered with what the system offers every app without a
/// purpose string: compressing an image (ImageIO), the system's pickers for
/// images, media and files, its viewer (`MigoSystemPreview`), and on a Mac
/// saving a file where the player chooses. Saving to the photo library is the
/// linked capability's that does it (`MigoPhotoLibrarySaving`) and refused
/// without one; the camera is not offered here.
///
/// A file this answers with is the host's own copy, in its staging directory:
/// the engine moves it into the session when the call completes, so a file the
/// player picked is never handed over itself.
final class MigoSystemImages {
    private let picker: MigoFilePicker
    private let preview: MigoSystemPreview
    private let photoLibrary: MigoPhotoLibrarySaving?
    private let staging: URL
    /// Where the slow parts -- decoding, encoding, describing a video -- run.
    private let work = DispatchQueue(label: "dev.migo.images", qos: .userInitiated, attributes: .concurrent)

    init(
        picker: MigoFilePicker = MigoSystemFilePicker(), preview: MigoSystemPreview? = nil,
        photoLibrary: MigoPhotoLibrarySaving? = nil, staging: URL = MigoSystemImages.defaultStaging
    ) {
        self.picker = picker
        self.photoLibrary = photoLibrary
        self.preview = preview ?? MigoSystemPreview(staging: staging)
        self.staging = staging
    }

    static var defaultStaging: URL {
        FileManager.default.temporaryDirectory.appendingPathComponent("migo-host-files", isDirectory: true)
    }

    func handle(_ request: MigoHostServiceRequest) {
        guard let reply = request.reply else { return }
        typealias Method = MigoHostServiceID.Image
        let options = request.object
        switch request.method {
        case Method.previewImage, Method.previewMedia:
            preview.handle(request)
        case Method.saveImageToPhotosAlbum:
            guard let photoLibrary else {
                reply.fail("not supported")
                return
            }
            guard let path = options["filePath"] as? String else {
                reply.fail("filePath is required")
                return
            }
            photoLibrary.saveImageToPhotoLibrary(URL(fileURLWithPath: path)) { failure in
                if let failure { reply.fail(failure) } else { reply.succeed() }
            }
        case Method.compressImage:
            work.async { self.compress(options, reply) }
        case Method.chooseImage:
            choose(kinds: [.image], sources: options["sourceType"], count: options["count"], reply) { files in
                try self.chosenImages(files)
            }
        case Method.chooseMedia:
            let kinds = Self.mediaKinds(options["mediaType"])
            choose(kinds: kinds, sources: options["sourceType"], count: options["count"], reply) { files in
                try self.chosenMedia(files)
            }
        case Method.chooseMessageFile:
            chooseMessageFile(options, reply)
        #if os(macOS)
            case Method.saveFileToDisk:
                saveFileToDisk(options, reply)
        #endif
        default:
            reply.fail("not supported")
        }
    }

    // MARK: - compressImage

    /// The ceiling on what one compression decodes and draws, in pixels: the
    /// Android SDK's, so the same call fails the same way on both.
    static let maxPixels = 50_000_000

    private func compress(_ options: [String: Any], _ reply: MigoHostServiceReply) {
        guard let path = options["src"] as? String else {
            reply.fail("src is required")
            return
        }
        let quality = min(max((options["quality"] as? NSNumber)?.doubleValue ?? 80, 0), 100)
        let width = (options["compressedWidth"] as? NSNumber)?.intValue ?? 0
        let height = (options["compressedHeight"] as? NSNumber)?.intValue ?? 0
        do {
            let output = try stage(extension: "jpg")
            let written = try Self.compress(
                URL(fileURLWithPath: path), to: output, quality: quality, width: width, height: height)
            reply.succeed(["tempFilePath": written.path])
        } catch {
            reply.fail(Self.reason(error))
        }
    }

    /// Re-encode the image at `source` into `output` -- its extension corrected
    /// to the format written -- at `quality` (0...100), scaled to `width` x
    /// `height` when both are given, to one of them with the aspect kept when
    /// one is, as it is otherwise. A PNG stays a PNG; anything else becomes a
    /// JPEG. The image is upright: its orientation is applied, not carried.
    static func compress(_ source: URL, to output: URL, quality: Double, width: Int, height: Int) throws -> URL {
        guard let image = CGImageSourceCreateWithURL(source as CFURL, nil),
            let properties = CGImageSourceCopyPropertiesAtIndex(image, 0, nil) as? [CFString: Any],
            let pixelWidth = (properties[kCGImagePropertyPixelWidth] as? NSNumber)?.intValue,
            let pixelHeight = (properties[kCGImagePropertyPixelHeight] as? NSNumber)?.intValue,
            pixelWidth > 0, pixelHeight > 0
        else { throw MigoImageError("invalid image") }
        // Orientations 5 to 8 turn the image a quarter.
        let turned = ((properties[kCGImagePropertyOrientation] as? NSNumber)?.intValue ?? 1) >= 5
        let (uprightWidth, uprightHeight) = turned ? (pixelHeight, pixelWidth) : (pixelWidth, pixelHeight)
        let target = targetSize(width: uprightWidth, height: uprightHeight, asked: (width, height))
        guard target.width * target.height <= maxPixels else { throw MigoImageError("image too large") }

        // Decoded no larger than the target needs, with the orientation applied.
        let thumbnail: [CFString: Any] = [
            kCGImageSourceCreateThumbnailFromImageAlways: true,
            kCGImageSourceCreateThumbnailWithTransform: true,
            kCGImageSourceThumbnailMaxPixelSize: max(target.width, target.height),
        ]
        guard var decoded = CGImageSourceCreateThumbnailAtIndex(image, 0, thumbnail as CFDictionary) else {
            throw MigoImageError("decode failed")
        }
        if decoded.width != target.width || decoded.height != target.height {
            decoded = try redraw(decoded, width: target.width, height: target.height)
        }
        let png = (CGImageSourceGetType(image) as String?) == UTType.png.identifier
        let written = output.deletingPathExtension().appendingPathExtension(png ? "png" : "jpg")
        try write(decoded, to: written, type: png ? .png : .jpeg, quality: quality / 100)
        return written
    }

    /// The size `asked` describes for an image `width` x `height`.
    static func targetSize(width: Int, height: Int, asked: (Int, Int)) -> (width: Int, height: Int) {
        switch (asked.0 > 0, asked.1 > 0) {
        case (true, true): return (asked.0, asked.1)
        case (true, false): return (asked.0, max(1, Int((Double(height) * Double(asked.0) / Double(width)).rounded())))
        case (false, true): return (max(1, Int((Double(width) * Double(asked.1) / Double(height)).rounded())), asked.1)
        case (false, false): return (width, height)
        }
    }

    private static func redraw(_ image: CGImage, width: Int, height: Int) throws -> CGImage {
        guard
            let context = CGContext(
                data: nil, width: width, height: height, bitsPerComponent: 8, bytesPerRow: 0,
                space: CGColorSpace(name: CGColorSpace.sRGB)!,
                bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)
        else { throw MigoImageError("image too large") }
        context.interpolationQuality = .high
        context.draw(image, in: CGRect(x: 0, y: 0, width: width, height: height))
        guard let drawn = context.makeImage() else { throw MigoImageError("compress failed") }
        return drawn
    }

    static func write(_ image: CGImage, to url: URL, type: UTType, quality: Double) throws {
        guard let destination = CGImageDestinationCreateWithURL(url as CFURL, type.identifier as CFString, 1, nil)
        else { throw MigoImageError("compress failed") }
        CGImageDestinationAddImage(
            destination, image, [kCGImageDestinationLossyCompressionQuality: quality] as CFDictionary)
        guard CGImageDestinationFinalize(destination) else {
            try? FileManager.default.removeItem(at: url)
            throw MigoImageError("compress failed")
        }
    }

    // MARK: - the pickers

    /// What chooseMedia's mediaType asks for; both when it names neither.
    static func mediaKinds(_ value: Any?) -> Set<MigoPickKind> {
        let names = Set((value as? [Any] ?? []).compactMap { $0 as? String })
        if names.contains("mix") { return [.image, .video] }
        var kinds = Set<MigoPickKind>()
        if names.contains("image") { kinds.insert(.image) }
        if names.contains("video") { kinds.insert(.video) }
        return kinds.isEmpty ? [.image, .video] : kinds
    }

    /// The camera is a capability of its own; a call that allows only it is
    /// refused, one that allows the album too is shown the album.
    private static func albumAllowed(_ sources: Any?) -> Bool {
        guard let names = sources as? [Any], !names.isEmpty else { return true }
        return names.contains { ($0 as? String) == "album" }
    }

    private static func count(_ value: Any?, default fallback: Int) -> Int {
        max(1, (value as? NSNumber)?.intValue ?? fallback)
    }

    private func choose(
        kinds: Set<MigoPickKind>, sources: Any?, count: Any?, _ reply: MigoHostServiceReply,
        describe: @escaping ([URL]) throws -> [String: Any]
    ) {
        guard Self.albumAllowed(sources) else {
            reply.fail("not supported")
            return
        }
        let request = MigoPickRequest(
            count: Self.count(count, default: 9), kinds: kinds, source: .library, staging: staging)
        picker.pick(request) { [work] outcome in
            switch outcome {
            case .cancelled:
                reply.fail("cancel")
            case .unavailable(let reason):
                reply.fail(reason)
            case .picked(let files):
                work.async {
                    do {
                        reply.succeed(try describe(Array(files.prefix(request.count))))
                    } catch {
                        reply.fail(Self.reason(error))
                    }
                }
            }
        }
    }

    private func chosenImages(_ files: [URL]) throws -> [String: Any] {
        let images = try files.map(usableImage)
        return [
            "tempFilePaths": images.map(\.path),
            "tempFiles": images.map { ["path": $0.path, "size": Self.size($0)] as [String: Any] },
        ]
    }

    private func chosenMedia(_ files: [URL]) throws -> [String: Any] {
        var items: [[String: Any]] = []
        var sawImage = false
        var sawVideo = false
        for file in files {
            if Self.isVideo(file) {
                sawVideo = true
                items.append(try describeVideo(file))
            } else {
                sawImage = true
                let image = try usableImage(file)
                let (width, height) = Self.dimensions(image)
                items.append([
                    "tempFilePath": image.path, "size": Self.size(image), "width": width, "height": height,
                    "fileType": "image",
                ])
            }
        }
        return ["type": sawImage && sawVideo ? "mix" : sawVideo ? "video" : "image", "tempFiles": items]
    }

    /// An image content can draw: a JPEG, PNG, GIF, BMP or WebP as it is, and
    /// anything else the system reads -- HEIC, TIFF -- written as a JPEG.
    private func usableImage(_ file: URL) throws -> URL {
        let type = UTType(filenameExtension: file.pathExtension)
        let drawable: [UTType] = [.jpeg, .png, .gif, .bmp, .webP]
        if let type, drawable.contains(where: { type.conforms(to: $0) }) { return file }
        let jpeg = file.deletingPathExtension().appendingPathExtension("jpg")
        let written = try Self.compress(file, to: jpeg, quality: 92, width: 0, height: 0)
        try? FileManager.default.removeItem(at: file)
        return written
    }

    private func describeVideo(_ file: URL) throws -> [String: Any] {
        let asset = AVURLAsset(url: file)
        var item: [String: Any] = [
            "tempFilePath": file.path, "size": Self.size(file), "fileType": "video",
            "duration": CMTimeGetSeconds(asset.duration).isFinite ? CMTimeGetSeconds(asset.duration) : 0,
        ]
        if let track = asset.tracks(withMediaType: .video).first {
            let shown = track.naturalSize.applying(track.preferredTransform)
            item["width"] = Int(abs(shown.width).rounded())
            item["height"] = Int(abs(shown.height).rounded())
        } else {
            item["width"] = 0
            item["height"] = 0
        }
        // The cover: its first frame, upright.
        let generator = AVAssetImageGenerator(asset: asset)
        generator.appliesPreferredTrackTransform = true
        if let frame = try? generator.copyCGImage(at: .zero, actualTime: nil) {
            let cover = try stage(extension: "jpg")
            try Self.write(frame, to: cover, type: .jpeg, quality: 0.8)
            item["thumbTempFilePath"] = cover.path
        }
        return item
    }

    private func chooseMessageFile(_ options: [String: Any], _ reply: MigoHostServiceReply) {
        let type = options["type"] as? String ?? "all"
        let extensions = Set((options["extension"] as? [Any] ?? []).compactMap { ($0 as? String)?.lowercased() })
        let kinds: Set<MigoPickKind>
        switch type {
        case "image": kinds = [.image]
        case "video": kinds = [.video]
        default: kinds = [.file]
        }
        let request = MigoPickRequest(
            count: Self.count(options["count"], default: 1), kinds: kinds, source: .files, staging: staging,
            extensions: extensions)
        picker.pick(request) { outcome in
            switch outcome {
            case .cancelled:
                reply.fail("cancel")
            case .unavailable(let reason):
                reply.fail(reason)
            case .picked(let files):
                // "file" is anything but an image or a video.
                let kept = files.filter { type != "file" || Self.kind(of: $0) == "file" }
                    .filter { extensions.isEmpty || extensions.contains($0.pathExtension.lowercased()) }
                let now = Date()
                reply.succeed([
                    "tempFiles": kept.prefix(request.count).map { file -> [String: Any] in
                        let modified =
                            (try? file.resourceValues(forKeys: [.contentModificationDateKey]))?
                            .contentModificationDate ?? now
                        return [
                            "path": file.path, "size": Self.size(file), "name": Self.originalName(file),
                            "type": Self.kind(of: file), "time": Int(modified.timeIntervalSince1970),
                        ]
                    }
                ])
            }
        }
    }

    // MARK: - saveFileToDisk

    #if os(macOS)
        /// The platform's save dialog, and the file copied to where the player
        /// chose: a desktop's.
        private func saveFileToDisk(_ options: [String: Any], _ reply: MigoHostServiceReply) {
            guard let path = options["filePath"] as? String else {
                reply.fail("filePath is required")
                return
            }
            let source = URL(fileURLWithPath: path)
            let panel = NSSavePanel()
            panel.nameFieldStringValue = source.lastPathComponent
            panel.canCreateDirectories = true
            let finish: (NSApplication.ModalResponse) -> Void = { response in
                guard response == .OK, let destination = panel.url else {
                    reply.fail("cancel")
                    return
                }
                do {
                    // The panel asked before replacing a file.
                    if FileManager.default.fileExists(atPath: destination.path) {
                        try FileManager.default.removeItem(at: destination)
                    }
                    try FileManager.default.copyItem(at: source, to: destination)
                    reply.succeed()
                } catch {
                    reply.fail(Self.reason(error))
                }
            }
            if let window = NSApp.keyWindow ?? NSApp.mainWindow {
                panel.beginSheetModal(for: window, completionHandler: finish)
            } else {
                panel.begin(completionHandler: finish)
            }
        }
    #endif

    // MARK: - files

    /// A new file of the host's own, in the staging directory.
    func stage(extension ext: String) throws -> URL {
        try FileManager.default.createDirectory(at: staging, withIntermediateDirectories: true)
        return staging.appendingPathComponent(UUID().uuidString).appendingPathExtension(ext)
    }

    static func size(_ file: URL) -> Int {
        (try? file.resourceValues(forKeys: [.fileSizeKey]))?.fileSize ?? 0
    }

    static func dimensions(_ file: URL) -> (Int, Int) {
        guard let source = CGImageSourceCreateWithURL(file as CFURL, nil),
            let properties = CGImageSourceCopyPropertiesAtIndex(source, 0, nil) as? [CFString: Any]
        else { return (0, 0) }
        let width = (properties[kCGImagePropertyPixelWidth] as? NSNumber)?.intValue ?? 0
        let height = (properties[kCGImagePropertyPixelHeight] as? NSNumber)?.intValue ?? 0
        let turned = ((properties[kCGImagePropertyOrientation] as? NSNumber)?.intValue ?? 1) >= 5
        return turned ? (height, width) : (width, height)
    }

    static func isVideo(_ file: URL) -> Bool {
        UTType(filenameExtension: file.pathExtension)?.conforms(to: .movie) ?? false
    }

    /// "image", "video" or "file", as chooseMessageFile words it.
    static func kind(of file: URL) -> String {
        guard let type = UTType(filenameExtension: file.pathExtension) else { return "file" }
        if type.conforms(to: .image) { return "image" }
        if type.conforms(to: .movie) { return "video" }
        return "file"
    }

    /// The name the player saw: the picker's copy keeps it after a prefix of
    /// its own (`MigoSystemFilePicker`).
    static func originalName(_ file: URL) -> String {
        let name = file.lastPathComponent
        guard let separator = name.range(of: MigoPickRequest.namePrefixSeparator) else { return name }
        return String(name[separator.upperBound...])
    }

    static func reason(_ error: Error) -> String {
        (error as? MigoImageError)?.reason ?? error.localizedDescription
    }
}

struct MigoImageError: Error {
    let reason: String
    init(_ reason: String) { self.reason = reason }
}

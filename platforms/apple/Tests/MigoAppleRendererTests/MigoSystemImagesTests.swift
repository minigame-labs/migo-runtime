import Foundation
import ImageIO
import UniformTypeIdentifiers
import XCTest

@testable import MigoAppleRenderer

/// The image service the SDK answers itself: compressing with ImageIO, and the
/// pickers' answers built from the copies the host owns -- against a fake
/// picker, since the system's needs a player.
final class MigoSystemImagesTests: XCTestCase {
    private final class Recorder: @unchecked Sendable {
        var outcomes: [MigoHostServiceReply.Outcome] = []
        let done: XCTestExpectation
        init(_ done: XCTestExpectation) { self.done = done }
    }

    /// Answers with copies of `files`, as the system's picker does.
    private final class FakePicker: MigoFilePicker {
        var files: [URL] = []
        var cancels = false
        var asked: [MigoPickRequest] = []

        func pick(_ request: MigoPickRequest, completion: @escaping (MigoPickOutcome) -> Void) {
            asked.append(request)
            if cancels {
                completion(.cancelled)
            } else {
                completion(.picked(try! files.map(request.copy)))
            }
        }
    }

    private var root: URL!

    override func setUpWithError() throws {
        root = FileManager.default.temporaryDirectory.appendingPathComponent("migo-images-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
    }

    override func tearDownWithError() throws {
        try? FileManager.default.removeItem(at: root)
    }

    /// A `width` x `height` image of type `type`, written with `orientation`.
    private func image(_ name: String, _ width: Int, _ height: Int, _ type: UTType, orientation: Int = 1) -> URL {
        let context = CGContext(
            data: nil, width: width, height: height, bitsPerComponent: 8, bytesPerRow: 0,
            space: CGColorSpace(name: CGColorSpace.sRGB)!, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
        context.setFillColor(red: 0, green: 0.5, blue: 1, alpha: 1)
        context.fill(CGRect(x: 0, y: 0, width: width, height: height))
        let url = root.appendingPathComponent(name)
        let destination = CGImageDestinationCreateWithURL(url as CFURL, type.identifier as CFString, 1, nil)!
        CGImageDestinationAddImage(
            destination, context.makeImage()!, [kCGImagePropertyOrientation: orientation] as CFDictionary)
        XCTAssertTrue(CGImageDestinationFinalize(destination))
        return url
    }

    private func call(
        _ images: MigoSystemImages, _ method: UInt32, _ object: [String: Any]
    ) -> MigoHostServiceReply.Outcome? {
        let done = expectation(description: "answered")
        let recorder = Recorder(done)
        images.handle(
            MigoHostServiceRequest(
                service: .image, method: method, payload: try! JSONSerialization.data(withJSONObject: object),
                reply: MigoHostServiceReply {
                    recorder.outcomes.append($0)
                    recorder.done.fulfill()
                }))
        wait(for: [done], timeout: 10)
        return recorder.outcomes.first
    }

    private func result(_ outcome: MigoHostServiceReply.Outcome?) -> [String: Any] {
        guard case .success(let json)? = outcome else {
            XCTFail("not a success: \(String(describing: outcome))")
            return [:]
        }
        return (try? JSONSerialization.jsonObject(with: json)) as? [String: Any] ?? [:]
    }

    private typealias Method = MigoHostServiceID.Image

    // MARK: - compressImage

    func testTheSizeAskedForWithTheAspectKeptWhenOneSideIsGiven() {
        XCTAssertTrue(MigoSystemImages.targetSize(width: 400, height: 200, asked: (0, 0)) == (400, 200))
        XCTAssertTrue(MigoSystemImages.targetSize(width: 400, height: 200, asked: (100, 0)) == (100, 50))
        XCTAssertTrue(MigoSystemImages.targetSize(width: 400, height: 200, asked: (0, 100)) == (200, 100))
        XCTAssertTrue(MigoSystemImages.targetSize(width: 400, height: 200, asked: (30, 30)) == (30, 30))
    }

    func testACompressedImageIsUprightTheSizeAskedForAndAPNGStaysOne() throws {
        let images = MigoSystemImages(picker: FakePicker(), staging: root.appendingPathComponent("staging"))
        // Stored 40 x 20 and turned a quarter: 20 x 40 upright.
        let turned = image("turned.jpg", 40, 20, .jpeg, orientation: 6)
        let upright = result(call(images, Method.compressImage, ["src": turned.path, "quality": 50]))
        let uprightPath = try XCTUnwrap(upright["tempFilePath"] as? String)
        XCTAssertEqual(URL(fileURLWithPath: uprightPath).pathExtension, "jpg")
        XCTAssertTrue(MigoSystemImages.dimensions(URL(fileURLWithPath: uprightPath)) == (20, 40))

        let png = image("flat.png", 64, 32, .png)
        let scaled = result(call(images, Method.compressImage, ["src": png.path, "compressedWidth": 16]))
        let scaledPath = try XCTUnwrap(scaled["tempFilePath"] as? String)
        XCTAssertEqual(URL(fileURLWithPath: scaledPath).pathExtension, "png")
        XCTAssertTrue(MigoSystemImages.dimensions(URL(fileURLWithPath: scaledPath)) == (16, 8))
        XCTAssertTrue(scaledPath.hasPrefix(root.appendingPathComponent("staging").path))
    }

    func testWhatCannotBeCompressedFailsWithWhy() {
        let images = MigoSystemImages(picker: FakePicker(), staging: root)
        let text = root.appendingPathComponent("note.txt")
        try? Data("not an image".utf8).write(to: text)
        let png = image("small.png", 8, 8, .png)
        XCTAssertEqual(
            call(images, Method.compressImage, ["src": text.path]), .failure(message: "invalid image", code: nil))
        XCTAssertEqual(
            call(images, Method.compressImage, ["src": png.path, "compressedWidth": 10_000, "compressedHeight": 10_000]),
            .failure(message: "image too large", code: nil))
    }

    // MARK: - the pickers

    func testChosenImagesAreTheHostsCopiesAndWhatContentCannotDrawBecomesAJPEG() throws {
        let picker = FakePicker()
        picker.files = [image("a.png", 4, 4, .png), image("b.tiff", 6, 3, .tiff), image("c.png", 2, 2, .png)]
        let images = MigoSystemImages(picker: picker, staging: root.appendingPathComponent("staging"))
        let chosen = result(call(images, Method.chooseImage, ["count": 2, "sourceType": ["album", "camera"]]))
        let paths = try XCTUnwrap(chosen["tempFilePaths"] as? [String])
        XCTAssertEqual(paths.count, 2, "at most the count asked for")
        XCTAssertEqual(paths.map { URL(fileURLWithPath: $0).pathExtension }, ["png", "jpg"])
        XCTAssertTrue(paths.allSatisfy { $0.hasPrefix(root.appendingPathComponent("staging").path) })
        let files = try XCTUnwrap(chosen["tempFiles"] as? [[String: Any]])
        XCTAssertEqual(files.map { $0["path"] as? String }, paths)
        XCTAssertTrue(files.allSatisfy { ($0["size"] as? Int ?? 0) > 0 })
        XCTAssertEqual(picker.asked.first?.kinds, [.image])
        XCTAssertEqual(picker.asked.first?.count, 2)
    }

    func testChosenMediaDescribesEachItem() throws {
        let picker = FakePicker()
        picker.files = [image("photo.jpg", 30, 10, .jpeg, orientation: 8)]
        let images = MigoSystemImages(picker: picker, staging: root)
        let chosen = result(call(images, Method.chooseMedia, ["count": 9, "mediaType": ["mix"]]))
        XCTAssertEqual(chosen["type"] as? String, "image")
        let item = try XCTUnwrap((chosen["tempFiles"] as? [[String: Any]])?.first)
        XCTAssertEqual(item["fileType"] as? String, "image")
        XCTAssertEqual(item["width"] as? Int, 10)
        XCTAssertEqual(item["height"] as? Int, 30)
        XCTAssertEqual(picker.asked.first?.kinds, [.image, .video])
    }

    func testACancelledPickerAndACameraOnlyCallFail() {
        let picker = FakePicker()
        picker.cancels = true
        let images = MigoSystemImages(picker: picker, staging: root)
        XCTAssertEqual(call(images, Method.chooseImage, [:]), .failure(message: "cancel", code: nil))
        // The camera is a capability of its own.
        XCTAssertEqual(
            call(images, Method.chooseMedia, ["sourceType": ["camera"]]),
            .failure(message: "not supported", code: nil))
        XCTAssertEqual(picker.asked.count, 1)
    }

    func testAChosenMessageFileKeepsItsNameAndOnlyTheKindAskedFor() throws {
        let picker = FakePicker()
        let report = root.appendingPathComponent("report.pdf")
        try Data("%PDF".utf8).write(to: report)
        picker.files = [image("shot.png", 2, 2, .png), report]
        let images = MigoSystemImages(picker: picker, staging: root.appendingPathComponent("staging"))
        let chosen = result(call(images, Method.chooseMessageFile, ["count": 5, "type": "file"]))
        let files = try XCTUnwrap(chosen["tempFiles"] as? [[String: Any]])
        XCTAssertEqual(files.count, 1, "an image is not a file here")
        XCTAssertEqual(files.first?["name"] as? String, "report.pdf")
        XCTAssertEqual(files.first?["type"] as? String, "file")
        XCTAssertNotNil(files.first?["time"] as? Int)
        XCTAssertEqual(picker.asked.first?.source, .files)

        let filtered = result(
            call(images, Method.chooseMessageFile, ["count": 5, "type": "all", "extension": ["png"]]))
        XCTAssertEqual((filtered["tempFiles"] as? [[String: Any]])?.map { $0["name"] as? String }, ["shot.png"])
    }
}

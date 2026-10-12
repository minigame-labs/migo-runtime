import Foundation
import XCTest

@testable import MigoAppleRenderer

/// previewImage and previewMedia: every item the host's own copy before the
/// viewer shows it -- a file copied, a URL downloaded -- the call answered once
/// it shows, and the copies gone when it closes.
final class MigoSystemPreviewTests: XCTestCase {
    private final class Recorder: @unchecked Sendable {
        var outcomes: [MigoHostServiceReply.Outcome] = []
    }

    private final class FakePresenter: MigoPreviewPresenter {
        var shows = true
        var shown: [(files: [URL], index: Int)] = []
        var dismiss: (() -> Void)?

        func present(
            _ files: [URL], at index: Int, shown: @escaping (Bool) -> Void, dismissed: @escaping () -> Void
        ) {
            if shows { self.shown.append((files, index)) }
            dismiss = dismissed
            shown(shows)
        }
    }

    /// Answers http://migo.test/<name> from `responses`.
    private final class Stub: URLProtocol {
        static var responses: [String: (status: Int, mime: String, body: Data)] = [:]

        override class func canInit(with request: URLRequest) -> Bool { request.url?.host == "migo.test" }
        override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

        override func startLoading() {
            let answer = Self.responses[request.url!.lastPathComponent] ?? (404, "text/plain", Data())
            let response = HTTPURLResponse(
                url: request.url!, statusCode: answer.status, httpVersion: "HTTP/1.1",
                headerFields: ["Content-Type": answer.mime])!
            client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
            client?.urlProtocol(self, didLoad: answer.body)
            client?.urlProtocolDidFinishLoading(self)
        }

        override func stopLoading() {}
    }

    private var root: URL!
    private var staging: URL { root.appendingPathComponent("staging") }

    override func setUpWithError() throws {
        root = FileManager.default.temporaryDirectory.appendingPathComponent("migo-preview-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
    }

    override func tearDownWithError() throws {
        try? FileManager.default.removeItem(at: root)
        Stub.responses = [:]
    }

    private func preview(_ presenter: FakePresenter) -> MigoSystemPreview {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [Stub.self]
        return MigoSystemPreview(presenter: presenter, staging: staging, configuration: configuration)
    }

    private func file(_ name: String) throws -> URL {
        let url = root.appendingPathComponent(name)
        try Data(name.utf8).write(to: url)
        return url
    }

    private func call(_ preview: MigoSystemPreview, _ method: UInt32, _ object: [String: Any])
        -> MigoHostServiceReply.Outcome?
    {
        let recorder = Recorder()
        let done = expectation(description: "answered")
        preview.handle(
            MigoHostServiceRequest(
                service: .image, method: method, payload: try! JSONSerialization.data(withJSONObject: object),
                reply: MigoHostServiceReply {
                    recorder.outcomes.append($0)
                    done.fulfill()
                }))
        wait(for: [done], timeout: 10)
        return recorder.outcomes.first
    }

    private typealias Method = MigoHostServiceID.Image

    func testFilesAreShownAsTheHostsCopiesFromTheCurrentOneAndGoWhenItCloses() throws {
        let presenter = FakePresenter()
        let a = try file("a.png")
        let b = try file("b.jpg")
        let outcome = call(preview(presenter), Method.previewImage, ["urls": [a.path, b.path], "current": b.path])
        XCTAssertEqual(outcome, .success(json: Data("{}".utf8)))
        let shown = try XCTUnwrap(presenter.shown.first)
        XCTAssertEqual(shown.index, 1)
        XCTAssertEqual(shown.files.map(\.pathExtension), ["png", "jpg"])
        XCTAssertTrue(shown.files.allSatisfy { $0.path.hasPrefix(staging.path) })
        XCTAssertEqual(try shown.files.map { try String(contentsOf: $0) }, ["a.png", "b.jpg"])
        presenter.dismiss?()
        XCTAssertTrue(shown.files.allSatisfy { !FileManager.default.fileExists(atPath: $0.path) })
        // The game's own files are untouched.
        XCTAssertTrue(FileManager.default.fileExists(atPath: a.path))
    }

    func testAURLIsDownloadedAndKnownByItsType() throws {
        Stub.responses["picture"] = (200, "image/png", Data("png".utf8))
        let presenter = FakePresenter()
        let video = try file("clip.mp4")
        let outcome = call(
            preview(presenter), Method.previewMedia,
            [
                "sources": [["url": "http://migo.test/picture", "type": "image"], ["url": video.path, "type": "video"]],
                "current": 1,
            ])
        XCTAssertEqual(outcome, .success(json: Data("{}".utf8)))
        let shown = try XCTUnwrap(presenter.shown.first)
        XCTAssertEqual(shown.index, 1)
        XCTAssertEqual(shown.files.map(\.pathExtension), ["png", "mp4"])
        XCTAssertEqual(try String(contentsOf: shown.files[0]), "png")
    }

    func testAFailedDownloadOrNowhereToShowFailsAndLeavesNoCopies() throws {
        let presenter = FakePresenter()
        let a = try file("a.png")
        XCTAssertEqual(
            call(preview(presenter), Method.previewImage, ["urls": [a.path, "https://migo.test/missing.png"]]),
            .failure(message: "download failed: HTTP 404", code: nil))
        XCTAssertTrue(presenter.shown.isEmpty)

        presenter.shows = false
        XCTAssertEqual(
            call(preview(presenter), Method.previewImage, ["urls": [a.path]]),
            .failure(message: "there is no window to show it in", code: nil))
        XCTAssertEqual(
            call(preview(presenter), Method.previewImage, ["urls": [root.appendingPathComponent("gone.png").path]]),
            .failure(message: "gone.png cannot be read", code: nil))
        let left = (try? FileManager.default.contentsOfDirectory(atPath: staging.path)) ?? []
        XCTAssertEqual(left, [])
    }
}

import XCTest

@testable import MigoAppleCore

/// The content origin's three decisions, run against a real directory.
///
/// A traversal check that has never executed is not a traversal check, and the
/// interesting cases are the ones a textual check gets wrong: a `..` that Foundation
/// resolves for you, and a symlink inside the package pointing out of it.
final class MigoWebKitOriginRulesTests: XCTestCase {

    private typealias Rules = MigoWebKitOriginRules

    private var root: URL!
    private var outside: URL!
    private var engine: URL!

    override func setUpWithError() throws {
        let base = URL(fileURLWithPath: NSTemporaryDirectory())
            .appendingPathComponent("migo-origin-\(UUID().uuidString)")
        root = base.appendingPathComponent("package")
        outside = base.appendingPathComponent("elsewhere")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        try FileManager.default.createDirectory(at: outside, withIntermediateDirectories: true)
        try Data("<html></html>".utf8).write(to: root.appendingPathComponent("index.html"))
        try FileManager.default.createDirectory(
            at: root.appendingPathComponent("assets"), withIntermediateDirectories: true)
        try Data("body{}".utf8).write(
            to: root.appendingPathComponent("assets/style.css"))
        try Data("secret".utf8).write(to: outside.appendingPathComponent("keys.txt"))
        engine = base.appendingPathComponent("engine")
        try FileManager.default.createDirectory(at: engine, withIntermediateDirectories: true)
        try Data("export const x = 1;".utf8).write(
            to: engine.appendingPathComponent("page-entry.mjs"))
    }

    override func tearDownWithError() throws {
        if let base = root?.deletingLastPathComponent() {
            try? FileManager.default.removeItem(at: base)
        }
    }

    // MARK: - containment

    func testTheRootRequestBecomesTheIndex() {
        for path in ["", "/"] {
            let resolution = Rules.resolve(requestPath: path, host: Rules.host, root: root)
            XCTAssertEqual(
                resolution, .file(path: root.appendingPathComponent("index.html").path))
        }
    }

    func testANestedFileResolvesInsideThePackage() {
        XCTAssertEqual(
            Rules.resolve(requestPath: "/assets/style.css", host: Rules.host, root: root),
            .file(path: root.appendingPathComponent("assets/style.css").path))
    }

    func testATraversalIsRefusedAndNotClamped() {
        // Foundation resolves `..` happily, so the check has to be on the result. A
        // clamp -- answering with the index page -- would be a different answer than
        // the one asked for, and content could not tell that it got one.
        for path in [
            "/../elsewhere/keys.txt",
            "/assets/../../elsewhere/keys.txt",
            "/./../elsewhere/keys.txt",
        ] {
            XCTAssertEqual(
                Rules.resolve(requestPath: path, host: Rules.host, root: root), .outsidePackage,
                "\(path) was not refused")
        }
    }

    func testASymlinkOutOfThePackageIsRefused() throws {
        // The case a textual check passes and a filesystem check catches: the path
        // is inside the package and the bytes are not.
        try FileManager.default.createSymbolicLink(
            at: root.appendingPathComponent("escape"), withDestinationURL: outside)
        XCTAssertEqual(
            Rules.resolve(requestPath: "/escape/keys.txt", host: Rules.host, root: root),
            .outsidePackage)
    }

    func testAPackageThatIsItselfReachedThroughASymlinkStillServesItsOwnFiles() throws {
        // The other half of the same rule. Resolving symlinks on the candidate and
        // not on the root would put every file outside a root reached by a link.
        let linked = root.deletingLastPathComponent().appendingPathComponent("linked")
        try FileManager.default.createSymbolicLink(at: linked, withDestinationURL: root)
        XCTAssertEqual(
            Rules.resolve(requestPath: "/index.html", host: Rules.host, root: linked),
            .file(path: root.appendingPathComponent("index.html").path))
    }

    func testAnotherAuthorityOnTheSameSchemeIsNotThisOrigin() {
        XCTAssertEqual(
            Rules.resolve(requestPath: "/index.html", host: "elsewhere", root: root),
            .wrongHost("elsewhere"))
        XCTAssertEqual(
            Rules.resolve(requestPath: "/index.html", host: nil, root: root), .wrongHost(nil))
    }

    // MARK: - the reserved engine prefix

    func testAnEngineModuleIsServedFromTheEngineRootAndNotTheContentRoot() throws {
        // The same request twice, differing only in which root exists, is the
        // whole claim: the prefix selects the root rather than being a directory
        // that happens to live somewhere.
        XCTAssertEqual(
            Rules.resolve(
                requestPath: "\(Rules.engineAssetPrefix)page-entry.mjs", host: "content",
                root: root, engineRoot: engine),
            .file(path: engine.appendingPathComponent("page-entry.mjs").standardizedFileURL
                .resolvingSymlinksInPath().path))
    }

    /// A game cannot replace the engine's own modules by shipping files with
    /// their names, which is the reason the prefix is reserved rather than
    /// conventional.
    func testAContentPackageCannotShadowTheEnginePrefix() throws {
        let decoy = root.appendingPathComponent("__migo")
        try FileManager.default.createDirectory(at: decoy, withIntermediateDirectories: true)
        try Data("globalThis.pwned = true;".utf8).write(
            to: decoy.appendingPathComponent("page-entry.mjs"))

        // With no engine root: refused by name, NOT served from the decoy.
        XCTAssertEqual(
            Rules.resolve(
                requestPath: "\(Rules.engineAssetPrefix)page-entry.mjs", host: "content",
                root: root),
            .reservedPathWithoutEngineRoot)

        // With one: the engine's file, while the decoy sits at the same path
        // inside the content root.
        guard
            case .file(let path) = Rules.resolve(
                requestPath: "\(Rules.engineAssetPrefix)page-entry.mjs", host: "content",
                root: root, engineRoot: engine)
        else { return XCTFail("the engine module did not resolve") }
        XCTAssertEqual(try String(contentsOf: URL(fileURLWithPath: path), encoding: .utf8),
            "export const x = 1;")
    }

    func testAPrefixLookalikeIsOrdinaryContent() {
        // `/__migo` without the trailing slash is a different directory, and
        // treating it as the prefix would take a real content path away from the
        // package that owns it. The engine root is supplied so a wrong answer
        // would be a resolvable one rather than a refusal.
        XCTAssertEqual(
            Rules.resolve(
                requestPath: "/__migovault/a.js", host: "content", root: root, engineRoot: engine),
            .file(path: root.appendingPathComponent("__migovault/a.js").standardizedFileURL
                .resolvingSymlinksInPath().path),
            "it must have been looked for in the CONTENT package")
    }

    func testATraversalOutOfTheEngineRootIsRefused() {
        XCTAssertEqual(
            Rules.resolve(
                requestPath: "\(Rules.engineAssetPrefix)../elsewhere/keys.txt", host: "content",
                root: root, engineRoot: engine),
            .outsidePackage)
    }

    func testThePrefixItselfHasNoIndex() {
        // The engine root holds modules, not a page. Answering this with an index
        // would be answering a question nobody asked with a file that is not there.
        XCTAssertEqual(
            Rules.resolve(
                requestPath: Rules.engineAssetPrefix, host: "content", root: root,
                engineRoot: engine),
            .outsidePackage)
    }

    // MARK: - MIME types

    func testTheTypesThatMustBeRightForMiniGameContent() {
        XCTAssertEqual(Rules.mimeType(forPathExtension: "wasm"), "application/wasm")
        XCTAssertEqual(Rules.mimeType(forPathExtension: "WASM"), "application/wasm")
        XCTAssertEqual(Rules.mimeType(forPathExtension: "js"), "text/javascript")
        XCTAssertEqual(Rules.mimeType(forPathExtension: "mjs"), "text/javascript")
        XCTAssertEqual(Rules.mimeType(forPathExtension: "json"), "application/json")
    }

    func testTheSystemTableAnswersForOrdinaryAssets() {
        XCTAssertEqual(Rules.mimeType(forPathExtension: "png"), "image/png")
        XCTAssertEqual(Rules.mimeType(forPathExtension: "html"), "text/html")
        XCTAssertTrue(Rules.mimeType(forPathExtension: "mp3").hasPrefix("audio/"))
    }

    func testAnUnknownExtensionIsBytesRatherThanAGuess() {
        XCTAssertEqual(
            Rules.mimeType(forPathExtension: "migoatlas"), "application/octet-stream")
        XCTAssertEqual(Rules.mimeType(forPathExtension: ""), "application/octet-stream")
    }

    // MARK: - ranges

    func testNoRangeHeaderIsTheWholeFile() {
        XCTAssertEqual(Rules.byteRange(fromHeader: nil, size: 100), .whole)
    }

    func testTheThreeFormsWebKitSends() {
        XCTAssertEqual(
            Rules.byteRange(fromHeader: "bytes=0-99", size: 1000), .partial(start: 0, length: 100))
        XCTAssertEqual(
            Rules.byteRange(fromHeader: "bytes=500-", size: 1000), .partial(start: 500, length: 500))
        XCTAssertEqual(
            Rules.byteRange(fromHeader: "bytes=-200", size: 1000), .partial(start: 800, length: 200))
    }

    func testAnEndPastTheFileIsClampedAndAStartPastItIsNot() {
        // Different answers on purpose. An end beyond the file is a client asking
        // for "the rest", which is answerable; a start beyond it is a request for
        // bytes that do not exist, and answering it with something would be
        // answering a question nobody asked.
        XCTAssertEqual(
            Rules.byteRange(fromHeader: "bytes=900-5000", size: 1000),
            .partial(start: 900, length: 100))
        XCTAssertEqual(Rules.byteRange(fromHeader: "bytes=1000-", size: 1000), .unsatisfiable)
        XCTAssertEqual(Rules.byteRange(fromHeader: "bytes=4096-8191", size: 1000), .unsatisfiable)
    }

    func testASuffixLongerThanTheFileIsTheFile() {
        XCTAssertEqual(
            Rules.byteRange(fromHeader: "bytes=-5000", size: 1000),
            .partial(start: 0, length: 1000))
    }

    func testAHeaderThisOriginDoesNotUnderstandServesTheWholeFile() {
        // Refusing an unrecognised header with a 416 breaks a load that would have
        // worked. A multi-range request is the case that matters: legal, rare, and
        // answering only its first range would be wrong rather than partial.
        for header in [
            "bytes=0-99,200-299", "items=0-99", "bytes=", "bytes=abc-def", "bytes=99-0",
            "nonsense",
        ] {
            XCTAssertEqual(
                Rules.byteRange(fromHeader: header, size: 1000), .whole, "\(header) was not served whole")
        }
    }

    func testAnEmptyFileHasNoRangeToServe() {
        XCTAssertEqual(Rules.byteRange(fromHeader: "bytes=0-", size: 0), .unsatisfiable)
        XCTAssertEqual(Rules.byteRange(fromHeader: "bytes=-10", size: 0), .whole)
    }

    // MARK: - headers

    func testAPartialResponseNamesTheRangeItActuallySends() {
        let headers = Rules.responseHeaders(
            pathExtension: "mp3", start: 500, length: 500, totalSize: 1000, partial: true)
        XCTAssertEqual(headers["Content-Range"], "bytes 500-999/1000")
        XCTAssertEqual(headers["Content-Length"], "500")
        XCTAssertEqual(headers["Accept-Ranges"], "bytes")
    }

    func testAWholeResponseStillAdvertisesRanges() {
        // WebKit's media loader only issues ranges against a source that says it
        // accepts them, so an audio element served without this cannot seek however
        // well the range code above works.
        let headers = Rules.responseHeaders(
            pathExtension: "mp3", start: 0, length: 1000, totalSize: 1000, partial: false)
        XCTAssertEqual(headers["Accept-Ranges"], "bytes")
        XCTAssertNil(headers["Content-Range"])
        XCTAssertEqual(headers["Content-Length"], "1000")
    }

    func testTheOriginIsAUsableURL() {
        let url = URL(string: Rules.baseURLString)
        XCTAssertEqual(url?.scheme, Rules.scheme)
        XCTAssertEqual(url?.host, Rules.host)
    }
}

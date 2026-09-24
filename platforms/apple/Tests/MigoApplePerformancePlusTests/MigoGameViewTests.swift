import CryptoKit
import MigoAppleRenderer
import XCTest

@testable import MigoApplePerformancePlus

#if os(iOS)
    import UIKit

    /// The product surface, the way an app uses it: install a package, put the
    /// view in a window, load the game, and hear from it.
    ///
    /// The lane underneath has its own acceptance in `MigoFrameAcceptanceTests`,
    /// which drives each part through the harness. What only this can show is
    /// that the view wires those parts to each other with nothing supplied by a
    /// test: its own engine session, its own nonce, its own layer, its own
    /// display clock answering the engine's frame requests, and a teardown that
    /// leaves the layer free for the next game.
    final class MigoGameViewTests: XCTestCase {

        private var root: URL!
        private var window: UIWindow!

        override func setUpWithError() throws {
            try super.setUpWithError()
            setenv("MIGO_CAPI_LOG", "info", 1)
            root = URL(fileURLWithPath: NSTemporaryDirectory())
                .appendingPathComponent("migo-game-view-\(UUID().uuidString)")
            try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        }

        override func tearDownWithError() throws {
            window?.isHidden = true
            window = nil
            try? FileManager.default.removeItem(at: root)
            try super.tearDownWithError()
        }

        private var directories: MigoGameDirectories {
            MigoGameDirectories(
                files: root.appendingPathComponent("files"), cache: root.appendingPathComponent("cache"),
                codeCache: root.appendingPathComponent("code-cache"))
        }

        /// A package on disk, the way an app ships one in its bundle.
        private func package(named name: String, game: String) throws -> URL {
            let package = root.appendingPathComponent("packages/\(name)")
            try FileManager.default.createDirectory(at: package, withIntermediateDirectories: true)
            try Data(game.utf8).write(to: package.appendingPathComponent("game.js"))
            try Data(#"{"deviceOrientation":"portrait"}"#.utf8).write(
                to: package.appendingPathComponent("game.json"))
            return package
        }

        private func mount(_ view: MigoGameView) {
            let window = UIWindow(frame: CGRect(x: 0, y: 0, width: 390, height: 844))
            let controller = UIViewController()
            window.rootViewController = controller
            view.frame = controller.view.bounds
            view.autoresizingMask = [.flexibleWidth, .flexibleHeight]
            controller.view.addSubview(view)
            window.makeKeyAndVisible()
            self.window = window
        }

        /// A game draws through the engine's WebGL on the display's clock, reads
        /// the window it was told about, and exits; the view then runs a second
        /// game on the same layer.
        func testAGameInstalledAndLoadedRunsOnTheDisplayClockExitsAndTheViewRunsTheNext() throws {
            let drawing = try package(
                named: "drawing",
                game: """
                    const info = migo.getWindowInfo();
                    const canvas = migo.createCanvas();
                    const gl = canvas.getContext("webgl");
                    let frames = 0;
                    const draw = () => {
                      frames += 1;
                      gl.clearColor(1, 0, 0, 1);
                      gl.clear(gl.COLOR_BUFFER_BIT);
                      if (frames < 30) { requestAnimationFrame(draw); return; }
                      console.log("drawn " + JSON.stringify({ frames, width: canvas.width,
                        windowWidth: info.windowWidth, pixelRatio: info.pixelRatio }));
                      setTimeout(() => migo.exitMiniProgram(), 0);
                    };
                    requestAnimationFrame(draw);
                    """)
            let second = try package(named: "second", game: #"console.log("second-game");"#)
            try MigoGameInstaller.install(package: drawing, id: "drawing", version: "1", into: directories)
            try MigoGameInstaller.install(package: second, id: "second", into: directories)

            let view = MigoGameView(configuration: .init(directories: directories, contentSigning: .unsigned))
            var events: [String] = []
            var drawn: [String: Any]?
            var failure: String?
            var clock: MigoSessionFrameClock.Statistics?
            var channel: MigoFrameChannel.Statistics?
            let exited = expectation(description: "the game exited")
            let secondRan = expectation(description: "the second game ran")
            view.onEvent = { event in
                switch event {
                case .ready: events.append("ready")
                case .exitRequested:
                    events.append("exit")
                    exited.fulfill()
                case .failed(let reason):
                    failure = reason
                    exited.fulfill()
                case .console(_, let message):
                    if message.hasPrefix("drawn ") {
                        drawn = (try? JSONSerialization.jsonObject(with: Data(message.dropFirst(6).utf8)))
                            as? [String: Any]
                        // Read now, not by polling: the game logs this from its
                        // 30th frame callback, which ran only after the view's
                        // clock delivered the 30th tick -- so this instant is
                        // causally after every delivery counted below, and before
                        // exit tears the view's session down.
                        clock = view.frameClockStatistics
                        channel = view.frameChannelStatistics
                    }
                    if message == "second-game" { secondRan.fulfill() }
                default: break
                }
            }
            mount(view)
            view.loadGame(id: "drawing")

            wait(for: [exited], timeout: 240)
            XCTAssertNil(failure)
            XCTAssertEqual(events, ["ready", "exit"])
            XCTAssertFalse(view.isRunning, "exit stops the game")

            let answered = try XCTUnwrap(drawn, "the game never reported its frames")
            XCTAssertEqual(answered["frames"] as? Int, 30)
            XCTAssertEqual(answered["windowWidth"] as? Double, 390, "the view's width, in points")
            let scale = Double(window.screen.scale)
            XCTAssertEqual(answered["pixelRatio"] as? Double, scale)
            XCTAssertEqual(answered["width"] as? Int, Int(390 * scale), "the main canvas is the surface")

            // The frames were paced by the view's own display clock, not by the
            // engine timing itself: the clock delivered them.
            let ticks = try XCTUnwrap(clock)
            XCTAssertGreaterThanOrEqual(ticks.delivered, 30, "\(ticks)")
            XCTAssertEqual(ticks.refused, 0, "\(ticks)")
            // Frames 1..29 were submitted before the 30th callback ran; the
            // host may still be admitting the last of them, so this asserts
            // that frames crossed and none was refused, not a count a race decides.
            let frames = try XCTUnwrap(channel)
            XCTAssertGreaterThan(frames.framesAccepted, 0, "\(frames)")
            XCTAssertEqual(frames.framesRefused, 0, "\(frames)")

            // Loaded while the first is still releasing the layer: it waits for
            // the release and then starts on the same layer.
            view.loadGame(id: "second")
            wait(for: [secondRan], timeout: 240)
            view.stop()
        }

        /// The soft keyboard, round trip: content opens it with its options, the
        /// player types and presses Return, and content hears the whole text as
        /// input, then confirm, then complete -- the three events the mini-game
        /// API promises, carried by the host's keyboard and not by a test double.
        func testContentOpensTheKeyboardAndHearsWhatThePlayerTypes() throws {
            let game = try package(
                named: "keyboard",
                game: """
                    const say = (kind, res) => console.log("kb " + JSON.stringify({ kind, value: res.value }));
                    migo.onKeyboardInput((res) => say("input", res));
                    migo.onKeyboardConfirm((res) => say("confirm", res));
                    migo.onKeyboardComplete((res) => say("complete", res));
                    migo.showKeyboard({ defaultValue: "hi", maxLength: 8, confirmType: "send",
                      success() { console.log("kb-open"); },
                      fail(error) { console.log("kb-fail " + error.errMsg); } });
                    """)
            try MigoGameInstaller.install(package: game, id: "keyboard", into: directories)
            let view = MigoGameView(configuration: .init(directories: directories, contentSigning: .unsigned))
            let opened = expectation(description: "the keyboard opened")
            let completed = expectation(description: "content heard complete")
            var heard: [[String: String]] = []
            var failure: String?
            view.onEvent = { event in
                switch event {
                case .console(_, let message):
                    if message == "kb-open" { opened.fulfill() }
                    if message.hasPrefix("kb-fail ") {
                        failure = message
                        opened.fulfill()
                    }
                    if message.hasPrefix("kb "),
                        let entry = (try? JSONSerialization.jsonObject(with: Data(message.dropFirst(3).utf8)))
                            as? [String: String]
                    {
                        heard.append(entry)
                        if entry["kind"] == "complete" { completed.fulfill() }
                    }
                case .failed(let reason):
                    failure = reason
                    opened.fulfill()
                default: break
                }
            }
            mount(view)
            view.loadGame(id: "keyboard")
            wait(for: [opened], timeout: 240)
            XCTAssertNil(failure)

            // The field content asked for, reached as the system reaches it: the
            // first responder that accepts text.
            let deadline = Date().addingTimeInterval(10)
            var responder: UITextView?
            while responder == nil, Date() < deadline {
                responder = view.subviews.compactMap { $0 as? UITextView }.first { $0.isFirstResponder }
                RunLoop.current.run(until: Date().addingTimeInterval(0.05))
            }
            let field = try XCTUnwrap(responder, "showKeyboard made nothing first responder")
            XCTAssertEqual(field.text, "hi", "the default value")
            XCTAssertEqual(field.returnKeyType, .send)
            field.insertText("!")
            field.insertText("0123456789")  // past maxLength 8: truncated
            field.insertText("\n")
            wait(for: [completed], timeout: 30)
            view.stop()

            XCTAssertEqual(
                heard,
                [
                    ["kind": "input", "value": "hi!"],
                    ["kind": "input", "value": "hi!01234"],
                    ["kind": "confirm", "value": "hi!01234"],
                    ["kind": "complete", "value": "hi!01234"],
                ])
        }

        /// A copy of `package`, signed: every file's SHA-256 in manifest.json and
        /// the raw Ed25519 signature of its bytes in manifest.sig.
        private func signed(_ package: URL, with key: Curve25519.Signing.PrivateKey, tamper: Bool = false) throws -> URL {
            let copy = root.appendingPathComponent("signed-\(UUID().uuidString)")
            try FileManager.default.copyItem(at: package, to: copy)
            var files: [String: String] = [:]
            for name in try FileManager.default.contentsOfDirectory(atPath: copy.path) {
                let bytes = try Data(contentsOf: copy.appendingPathComponent(name))
                files[name] = SHA256.hash(data: bytes).map { String(format: "%02x", $0) }.joined()
            }
            let manifest = try JSONSerialization.data(
                withJSONObject: ["version": 1, "entry": "game.js", "timestamp": 1_700_000_000, "files": files],
                options: [.sortedKeys])
            try manifest.write(to: copy.appendingPathComponent("manifest.json"))
            try key.signature(for: manifest).write(to: copy.appendingPathComponent("manifest.sig"))
            if tamper {
                try Data("console.log('changed');".utf8).write(to: copy.appendingPathComponent("game.js"))
            }
            return copy
        }

        /// Run `id` once in a fresh view and answer with its first console line,
        /// or the failure it reported.
        private func firstLine(of id: String, signing: MigoContentSigning) -> String {
            let view = MigoGameView(configuration: .init(directories: directories, contentSigning: signing))
            let said = expectation(description: "\(id) said something")
            var line = ""
            view.onEvent = { event in
                switch event {
                case .console(_, let message) where line.isEmpty:
                    line = message
                    said.fulfill()
                case .failed(let reason) where line.isEmpty:
                    line = "failed: " + reason
                    said.fulfill()
                default: break
                }
            }
            mount(view)
            view.loadGame(id: id)
            wait(for: [said], timeout: 240)
            // Each run has its own view and layer; stop() retires this one's
            // surface and the view waits for the release itself.
            view.stop()
            return line
        }

        /// Signed content on this lane: verified before the producer is served a
        /// byte, sealed once verified, updated by a new install without touching
        /// the sealed tree, and refused when a file changed after signing.
        func testSignedContentIsVerifiedSealedUpdatedAndRefusedWhenTampered() throws {
            let key = Curve25519.Signing.PrivateKey()
            let signing = MigoContentSigning.verified(publicKey: key.publicKey.rawRepresentation)
            let v1 = try signed(try package(named: "v1", game: "console.log('v1');"), with: key)
            try MigoGameInstaller.install(package: v1, id: "signed", version: "1", into: directories)
            XCTAssertEqual(firstLine(of: "signed", signing: signing), "v1")

            let code = directories.installedCode(contentID: "signed")
            let mode = try FileManager.default.attributesOfItem(atPath: code.path)[.posixPermissions] as? Int
            XCTAssertEqual((mode ?? 0o777) & 0o222, 0, "the verified tree is sealed read-only")

            let v2 = try signed(try package(named: "v2", game: "console.log('v2');"), with: key)
            try MigoGameInstaller.install(package: v2, id: "signed", version: "2", into: directories)
            XCTAssertEqual(firstLine(of: "signed", signing: signing), "v2", "the update replaced the sealed tree")
            let leftovers = try FileManager.default.contentsOfDirectory(atPath: code.deletingLastPathComponent().path)
                .filter { $0.hasPrefix(".staging") || $0.hasPrefix(".retired") }
            XCTAssertEqual(leftovers, [], "the retired tree was removed")

            let bad = try signed(try package(named: "bad", game: "console.log('bad');"), with: key, tamper: true)
            try MigoGameInstaller.install(package: bad, id: "tampered", into: directories)
            let refused = firstLine(of: "tampered", signing: signing)
            XCTAssertTrue(refused.hasPrefix("failed: "), "a package changed after signing ran: \(refused)")
            XCTAssertTrue(refused.contains("migo_session_load_content"), refused)
        }

        /// A game that is not installed is reported, not a black screen.
        func testAGameThatIsNotInstalledIsReportedFailed() throws {
            let view = MigoGameView(configuration: .init(directories: directories, contentSigning: .unsigned))
            let failed = expectation(description: "reported")
            var reason: String?
            view.onEvent = { event in
                if case .failed(let text) = event {
                    reason = text
                    failed.fulfill()
                }
            }
            mount(view)
            view.loadGame(id: "missing")
            wait(for: [failed], timeout: 60)
            XCTAssertTrue(reason?.contains("migo_session_load_content") == true, reason ?? "")
            XCTAssertFalse(view.isRunning)
        }

        func testTheInstallerRefusesIdsTheEngineWouldRefuseAndSkipsAnInstalledVersion() throws {
            for bad in ["", "Game", "a/b", "con", "com1", String(repeating: "a", count: 65)] {
                XCTAssertFalse(MigoGameInstaller.isValidID(bad), bad)
            }
            for good in ["game", "my-game_2", "com10", "console"] {
                XCTAssertTrue(MigoGameInstaller.isValidID(good), good)
            }
            let first = try package(named: "v1", game: "// one")
            let code = try MigoGameInstaller.install(package: first, id: "g", version: "1", into: directories)
            XCTAssertEqual(try String(contentsOf: code.appendingPathComponent("game.js")), "// one")

            let second = try package(named: "v2", game: "// two")
            try MigoGameInstaller.install(package: second, id: "g", version: "1", into: directories)
            XCTAssertEqual(
                try String(contentsOf: code.appendingPathComponent("game.js")), "// one",
                "the same version is not copied again")
            try MigoGameInstaller.install(package: second, id: "g", version: "2", into: directories)
            XCTAssertEqual(try String(contentsOf: code.appendingPathComponent("game.js")), "// two")
            let leftovers = try FileManager.default.contentsOfDirectory(
                atPath: code.deletingLastPathComponent().path
            ).filter { $0.hasPrefix(".staging") }
            XCTAssertEqual(leftovers, [], "the staging directory became the install")

            XCTAssertThrowsError(
                try MigoGameInstaller.install(
                    package: root.appendingPathComponent("nowhere"), id: "g", into: directories))
        }
    }
#endif

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

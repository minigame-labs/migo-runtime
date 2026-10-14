import Foundation
import MigoAppleCore
import MigoAppleRenderer
import MigoEngine

/// Where a game view keeps installed games, saves and caches. Named here so
/// an app that imports only this product can name it.
public typealias MigoGameDirectories = MigoEngineSession.Directories

/// Installs a game package where the engine loads it from; see
/// `MigoContentInstaller`.
public typealias MigoGameInstaller = MigoContentInstaller

/// Whether game packages must be signed; see `MigoEngineSession.ContentSigning`.
public typealias MigoContentSigning = MigoEngineSession.ContentSigning

/// A service the app supplies through the host-service channel; see
/// `MigoHostServiceID` and `MigoHostServiceProvider`. Named here so an app that
/// imports only this product can name them.
public typealias MigoHostServiceID = MigoAppleRenderer.MigoHostServiceID
public typealias MigoHostServiceRequest = MigoAppleRenderer.MigoHostServiceRequest
public typealias MigoHostServiceReply = MigoAppleRenderer.MigoHostServiceReply
public typealias MigoHostServiceProvider = MigoAppleRenderer.MigoHostServiceProvider
public typealias MigoSystemCapability = MigoAppleRenderer.MigoSystemCapability

#if os(iOS)
    import AVFoundation
    import AudioToolbox
    import GameController
    import Metal
    import QuartzCore
    import UIKit

    /// A view that runs one mini-game: the iOS product surface of Migo.
    ///
    /// Add it to a view controller, install the game once with
    /// `MigoGameInstaller`, and call `loadGame(id:entry:)`. Everything between
    /// that and pixels is this view's: the engine session, the `CAMetalLayer`
    /// it renders into, the WebContent producer that runs the game's JavaScript
    /// with JIT, the display clock, touches, the app's lifecycle, the audio
    /// session and recovery from a WebContent crash. The Android SDK's
    /// `MigoGameView.loadGame(gameId, entryPoint)` is the same call, on purpose.
    ///
    /// ## The size follows the view
    ///
    /// The game is told its window size before its first line runs and lays
    /// itself out to it -- that is how the mini-game API works on every
    /// platform -- so the session starts at the view's first non-empty layout.
    /// When the view is laid out at another size afterwards -- the interface
    /// turned, a scene resized beside another app -- the session's surface
    /// follows, and content hears `onWindowResize` with what `getWindowInfo()`
    /// now answers, as it does on every platform. A game that supports one
    /// orientation wants the hosting controller locked to it before this view's
    /// first layout, or it starts in a window of the wrong shape and is told
    /// to turn at once.
    ///
    /// Content's `setDeviceOrientation` asks the scene to turn (iOS 16 and
    /// later): it succeeds when the interface has turned, and fails when the
    /// view controller showing this view does not support the orientation
    /// asked for.
    ///
    /// ## The web view is not a view you see
    ///
    /// Content JavaScript runs in WebKit's content process because that is the
    /// only process iOS gives JIT to; its frames come back to this process and
    /// are drawn here. The `WKWebView` that owns that process is attached to the
    /// window, off screen: a web view that is not in a window is killed, and one
    /// that is occluded stops running JavaScript.
    ///
    /// ## Keyboard, mouse and trackpad
    ///
    /// A hardware keyboard's keys reach content (`onKeyDown`/`onKeyUp`) while
    /// the view is first responder, which it becomes when the game starts and
    /// when it is touched. On an iPad, the pointer -- a mouse or a trackpad --
    /// reaches it as the mouse (`onMouseMove`/`onMouseDown`/`onMouseUp`) and
    /// the wheel (`onWheel`), beside the touches iPadOS makes of its primary
    /// clicks, as on every platform where a mouse drives a touch screen. Set
    /// `UIApplicationSupportsIndirectInputEvents` in the app's Info.plist: without
    /// it iPadOS reports a click as a finger, and content hears no button.
    ///
    /// Content's cursor (`setCursor`) is drawn in the pointer's own styles --
    /// iPadOS shows no image as the pointer. Content's pointer lock
    /// (`requestPointerLock`) is the view controller's to grant: the one
    /// showing this view returns `prefersPointerLocked` below from its own
    /// (or names a child that does from `childViewControllerForPointerLock`),
    /// and iPadOS locks only a scene that fills the screen. While locked, the
    /// mouse's raw motion moves content's pointer; Esc releases it.
    public final class MigoGameView: UIView {

        /// What the view needs beyond the game.
        public struct Configuration {
            /// Where the engine keeps installed games, saves and caches.
            public var directories: MigoGameDirectories
            /// Whether packages must be signed. No default: see `MigoContentSigning`.
            public var contentSigning: MigoContentSigning
            /// Frames per second to ask the display for; `nil` is the system's
            /// default cadence. Above 60 needs ProMotion and
            /// `CADisableMinimumFrameDurationOnPhone` in the app's Info.plist.
            public var preferredFramesPerSecond: Int?
            /// Set the audio session's category and follow its interruptions.
            /// Turn off only if the app manages `AVAudioSession` itself; the
            /// game is then paused and resumed by the app, not by this view.
            public var managesAudioSession: Bool
            /// How many WebContent crashes before the game is reported failed
            /// instead of restarted, counted since the game last became ready.
            public var webContentTerminationBudget: Int
            /// What the app supplies of the host-service channel -- ads,
            /// payments, sign-in, sharing, its dialogs -- for the services it
            /// names. A service it does not name the SDK answers where the
            /// system can -- the pasteboard, its dialogs, the screen -- and
            /// content is told the rest is not supported. Held for as long as a
            /// game runs.
            public var hostServices: MigoHostServiceProvider?

            /// The optional parts of the system the app linked and opts into --
            /// `MigoLocation` from MigoAppleLocation, say -- each answering its
            /// services when `hostServices` does not name them, and deciding
            /// its scopes with the system's prompt when the app answers no
            /// permission service itself. The app declares their purpose
            /// strings. Held for as long as a game runs.
            public var capabilities: [MigoSystemCapability]

            public init(
                directories: MigoGameDirectories, contentSigning: MigoContentSigning,
                preferredFramesPerSecond: Int? = nil, managesAudioSession: Bool = true,
                webContentTerminationBudget: Int = 2, hostServices: MigoHostServiceProvider? = nil,
                capabilities: [MigoSystemCapability] = []
            ) {
                self.directories = directories
                self.contentSigning = contentSigning
                self.preferredFramesPerSecond = preferredFramesPerSecond
                self.managesAudioSession = managesAudioSession
                self.webContentTerminationBudget = webContentTerminationBudget
                self.hostServices = hostServices
                self.capabilities = capabilities
            }

            /// The standard directories under Application Support and Caches.
            public static func standard(contentSigning: MigoContentSigning) throws -> Configuration {
                Configuration(directories: try .standard(), contentSigning: contentSigning)
            }
        }

        /// What happened to the game. Delivered on the main queue.
        public enum Event {
            /// The game's entry has been evaluated.
            case ready
            /// The game asked to exit (`migo.exitMiniProgram()`). The view stops
            /// the game; what the app shows next is the app's.
            case exitRequested
            /// The game could not start or cannot continue.
            case failed(String)
            /// A runtime error the engine reported; `recoverable` errors are
            /// pressure notices and the game keeps running.
            case error(code: Int32, message: String, recoverable: Bool)
            /// WebKit's content process died and the game was restarted from its
            /// entry, as a web page would reload. Saves on disk survive; state
            /// in memory does not.
            case restarted
            /// A line the game wrote to `console`: the engine's level (1 info,
            /// 2 warn, 3 error, anything else debug) and the text.
            case console(level: Int, message: String)
            /// An entry from the game's log (`getGameLogManager().log`), as the
            /// JSON object the engine merged -- level, key, value, commonInfo --
            /// for the app to keep or upload.
            case gameLog(String)
        }

        /// Called on the main queue.
        public var onEvent: ((Event) -> Void)?

        /// Whether the game holds the pointer locked or has asked to. Return it
        /// from `prefersPointerLocked` of the view controller showing this
        /// view: iPadOS asks a controller, not a view, and the view has that
        /// controller read it again whenever it changes.
        public var prefersPointerLocked: Bool { windowService?.prefersLocked ?? false }

        /// Whether a game is loaded and not stopped.
        public private(set) var isRunning = false

        /// What the frame channel has carried, for a test or a diagnostics
        /// screen explaining a frame rate. `nil` while no game runs.
        public var frameChannelStatistics: MigoFrameChannel.Statistics? { host?.channel.currentStatistics }

        /// What the display clock has done. `nil` while no game runs.
        public var frameClockStatistics: MigoSessionFrameClock.Statistics? { engine?.frameStatistics }

        /// Why the game cannot run on this device, or `nil` if it can.
        ///
        /// The lane needs a WebContent JIT and a Metal device; the checks that
        /// can be made before starting are made here so an app can choose not
        /// to show the view at all.
        public static var unavailabilityReason: String? {
            if MTLCreateSystemDefaultDevice() == nil { return "this device has no Metal" }
            do {
                let preflight = try MigoEngineCapabilities.query().preflight()
                return preflight == .ready ? nil : "the linked engine cannot drive this view: \(preflight)"
            } catch {
                return "the linked engine did not answer its capability query: \(error)"
            }
        }

        private let configuration: Configuration
        private var metalView = MetalView()
        /// Layers a session never released. Kept, never reused: see `tearDown`.
        private var abandonedLayers: [MetalView] = []
        private var game: (id: String, entry: String)?
        // Readable by `@testable` tests, which report device changes through it.
        private(set) var engine: MigoEngineSession?
        private var host: MigoPerformancePlusHost?
        /// What the session was last told the view is: its surface and device.
        private var laidOut: Layout?

        private struct Layout: Equatable {
            var size: MigoEngineSession.SurfaceSize
            var device: MigoPerformancePlusHost.DeviceProfile
        }
        private var deviceStatus: MigoDeviceStatusReporter?
        /// The app's own idle-timer setting while the game holds the display
        /// awake, `nil` while it does not; put back when the game lets go or
        /// ends.
        private var idleTimerBeforeGame: Bool?
        private var recovery: MigoWebContentRecovery
        private var touches = TouchIdentities()
        /// The pointer's touches that began with its primary button: the ones
        /// iPadOS makes touches of. One begun with another button is the
        /// mouse's alone, as a right click is on the Mac and on Android.
        private var primaryClicks: Set<ObjectIdentifier> = []
        private var pointerButtons = MigoPointerButtons()
        /// Physical keys down, by DOM code: a second press of one is a repeat.
        private var keysDown: Set<String> = []
        /// An Esc that released the pointer lock is the player's, not the
        /// game's: its release is held back too.
        private var escapeReleasedLock = false
        /// The window service for the game that runs: the pointer's look, the
        /// pointer lock, the window's state. Readable by `@testable` tests.
        private(set) var windowService: MigoWindowService?
        /// The debugging panel while the debugging aids are on
        /// (`migo.setEnableDebug`). Readable by `@testable` tests.
        private(set) var debugPanel: MigoDebugPanel?
        /// The mice followed while the pointer is locked, and their arrivals.
        private var lockedMice: [FollowedMouse] = []
        private var mouseArrivals: NSObjectProtocol?
        /// The system keyboard's responder: zero-sized and never drawn -- the
        /// game draws its own field, as it does on every platform.
        private lazy var keyboard = KeyboardResponder(owner: self)
        private var observers: [NSObjectProtocol] = []
        /// The synchronous ecosystem getters' values the app reported, given to
        /// each game this view runs.
        private var ecosystemValues: [String: Data] = [:]
        private var pausedByApp = false
        private var pausedByAudio = false
        /// Sessions still releasing the layer. A new session must not attach
        /// the layer while an old one's retirement is PENDING: the driver still
        /// holds it, and two sessions presenting to one layer is the
        /// use-after-free `surface.h` warns about.
        private var closing = 0

        public init(configuration: Configuration) {
            self.configuration = configuration
            self.recovery = MigoWebContentRecovery(terminationBudget: configuration.webContentTerminationBudget)
            super.init(frame: .zero)
            backgroundColor = .black
            isMultipleTouchEnabled = true
            metalView.frame = bounds
            metalView.autoresizingMask = [.flexibleWidth, .flexibleHeight]
            addSubview(metalView)
            // The pointer hovering, which UIKit reports to no touch method.
            let hover = UIHoverGestureRecognizer(target: self, action: #selector(hovered(_:)))
            hover.allowedTouchTypes = [NSNumber(value: UITouch.TouchType.indirectPointer.rawValue)]
            addGestureRecognizer(hover)
            // The wheel and the trackpad's scroll, and no finger: a pan with no
            // touch types recognizes scrolling alone.
            let scroll = UIPanGestureRecognizer(target: self, action: #selector(scrolled(_:)))
            scroll.allowedScrollTypesMask = .all
            scroll.allowedTouchTypes = []
            scroll.cancelsTouchesInView = false
            addGestureRecognizer(scroll)
        }

        @available(*, unavailable)
        required init?(coder: NSCoder) { fatalError("MigoGameView is created in code") }

        deinit {
            observers.forEach(NotificationCenter.default.removeObserver)
        }

        /// Run an installed game. The game starts at the view's first layout in
        /// a window; calling this again replaces the running game.
        public func loadGame(id: String, entry: String = "game.js") {
            precondition(Thread.isMainThread, "MigoGameView is main-thread only")
            stop()
            game = (id, entry)
            startIfPossible()
        }

        /// Post one of a service's events to the game -- a `MIGO_<SERVICE>_EVENT_*`
        /// number with the JSON object `host_services.h` documents for it: an
        /// advert's progress, a package update's steps. Nothing while no game
        /// runs. Main thread.
        public func postHostServiceEvent(_ service: MigoHostServiceID, event: UInt32, json: Data) {
            engine?.postHostServiceEvent(service, event: event, json: json)
        }

        /// Record the app's standing decision for one of content's scopes -- a
        /// `MIGO_SCOPE_*` number and a `MIGO_SCOPE_STATE_*` -- when the app
        /// answers the permission service itself: after deciding a request,
        /// and whenever the decision changes. Content's calls that need the
        /// scope are checked against it. Nothing while no game runs. Main
        /// thread.
        public func setScopeState(_ scope: MigoScope, _ state: MigoScopeState) {
            engine?.setScopeState(scope, state)
        }

        /// Report the value a synchronous ecosystem getter answers with --
        /// `name` one of the contract's values (`getExtConfigSync`,
        /// `isChatTool`, ...), `json` what the API returns -- whenever it
        /// changes; `nil` withdraws it. The view keeps what was reported and
        /// gives it to each game it runs, before the game's first line. Needs
        /// the ecosystem service in `hostServices`. Main thread.
        public func setEcosystemValue(_ name: String, json: Data?) {
            ecosystemValues[name] = json
            engine?.setEcosystemValue(name, json: json)
        }

        /// Stop the game and release everything it held. The view can load
        /// another game afterwards.
        public func stop() {
            game = nil
            tearDown(then: nil)
            if isFirstResponder { resignFirstResponder() }
            observers.forEach(NotificationCenter.default.removeObserver)
            observers = []
            if configuration.managesAudioSession {
                try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
            }
        }

        // MARK: - view

        public override func didMoveToWindow() {
            super.didMoveToWindow()
            if window == nil {
                // Leaving the window is where the app ends the game's screen;
                // the web view must not be stranded in a window it no longer
                // belongs to.
                if isRunning { tearDown(then: nil) }
                return
            }
            startIfPossible()
        }

        public override func layoutSubviews() {
            super.layoutSubviews()
            startIfPossible()
            followLayout()
            // A scene resized beside another app lays this view out again.
            windowService?.reportState()
        }

        /// The surface and the device as this view and its window are now: the
        /// view's size in pixels, and the window, screen and safe area as they
        /// are turned.
        private func layout(in window: UIWindow) -> Layout {
            let scale = window.screen.scale
            let points = bounds.size
            let size = MigoEngineSession.SurfaceSize(
                widthPixels: UInt32((points.width * scale).rounded()),
                heightPixels: UInt32((points.height * scale).rounded()),
                scale: Float(scale))
            let insets = window.safeAreaInsets
            let device = MigoPerformancePlusHost.DeviceProfile(
                screenWidth: Double(window.screen.bounds.width),
                screenHeight: Double(window.screen.bounds.height),
                windowWidth: Double(points.width), windowHeight: Double(points.height),
                pixelRatio: Double(scale),
                statusBarHeight: Double(window.windowScene?.statusBarManager?.statusBarFrame.height ?? 0),
                safeAreaInsets: .init(
                    left: Double(insets.left), top: Double(insets.top), right: Double(insets.right),
                    bottom: Double(insets.bottom)),
                brand: "Apple", model: Self.hardwareModel(),
                system: "iOS \(UIDevice.current.systemVersion)", platform: "ios")
            return Layout(size: size, device: device)
        }

        /// The view resized or turned while the game runs: the engine's surface
        /// first, then the producer, which tells content -- so the frames it
        /// draws for the new size meet a surface of that size. A change of the
        /// safe area alone reaches content's next `getWindowInfo()` and no
        /// resize, as the engine reports a resize only for a new size.
        private func followLayout() {
            guard isRunning, let engine, let host, let window, bounds.width >= 1, bounds.height >= 1 else { return }
            let now = layout(in: window)
            guard now != laidOut else { return }
            do {
                try engine.resize(to: now.size)
            } catch {
                onEvent?(.error(code: MIGO_ERROR_INTERNAL, message: String(describing: error), recoverable: true))
                return
            }
            laidOut = now
            // The web view keeps the window's size, off its left edge (see `start`).
            host.view.frame = window.bounds.offsetBy(dx: -window.bounds.width - 1, dy: 0)
            host.resize(
                surfaceWidthPixels: Int(now.size.widthPixels), surfaceHeightPixels: Int(now.size.heightPixels),
                device: now.device)
        }

        public override var canBecomeFirstResponder: Bool { true }

        /// Take the hardware keyboard's keys, when the game runs in the window
        /// the player is at and nothing of the game's has them -- the system
        /// keyboard content opened keeps them while it is open, so touching the
        /// game's own Send button does not close it.
        fileprivate func takeKeys() {
            guard isRunning, window?.isKeyWindow == true, !isFirstResponder, !keyboard.isOpen else { return }
            becomeFirstResponder()
        }

        // MARK: - start and stop

        private func startIfPossible() {
            guard let game, !isRunning, closing == 0, let window, bounds.width >= 1, bounds.height >= 1 else { return }
            do {
                try start(id: game.id, entry: game.entry, in: window)
            } catch {
                tearDown(then: nil)
                self.game = nil
                onEvent?(.failed(String(describing: error)))
            }
        }

        private func start(id: String, entry: String, in window: UIWindow) throws {
            let started = layout(in: window)
            let size = started.size
            metalView.configure(scale: window.screen.scale)

            let windowService = MigoWindowService(view: self) { [weak self] service, event, object in
                guard let json = try? JSONSerialization.data(withJSONObject: object) else { return }
                self?.engine?.postHostServiceEvent(service, event: event, json: json)
            }
            windowService.onLockChange = { [weak self] locked in self?.followMice(locked) }
            self.windowService = windowService
            // The app's services, then what only this view can answer; the
            // session adds the SDK's own behind them.
            let engine = try MigoEngineSession(
                directories: configuration.directories, contentSigning: configuration.contentSigning,
                preferredFramesPerSecond: configuration.preferredFramesPerSecond,
                hostServices: MigoGameViewHostServices(app: configuration.hostServices, window: windowService),
                capabilities: configuration.capabilities)
            self.engine = engine
            engine.onEvent = { [weak self] event in self?.engineEvent(event) }
            for (name, json) in ecosystemValues { engine.setEcosystemValue(name, json: json) }
            // Before content runs, so its first `getNetworkType` has a report.
            let deviceStatus = MigoDeviceStatusReporter(session: engine)
            self.deviceStatus = deviceStatus
            deviceStatus.start()
            try engine.attach(layer: metalView.metalLayer, size: size)
            try engine.loadContent(id: id, entry: entry)
            let root = try Self.contentRoot(of: engine.session)

            let device = started.device
            laidOut = started
            let host = try MigoPerformancePlusHost(
                configuration: .init(
                    contentRoot: root, gameEntry: "/" + entry,
                    engineSession: .init(
                        launchNonce: engine.launchNonce, surfaceGeneration: engine.surfaceGeneration,
                        surfaceWidthPixels: Int(size.widthPixels), surfaceHeightPixels: Int(size.heightPixels),
                        device: device)),
                channel: MigoFrameChannel(session: engine.session))
            self.host = host
            host.onReport = { [weak self] report in self?.producerReport(report) }
            host.onConsole = { [weak self] level, message in
                self?.debugPanel?.append(level: level, message: message)
                self?.onEvent?(.console(level: level, message: message))
            }

            // Off the window's left edge and the full window's size: attached
            // (an unattached web view is killed), neither hidden nor zero-sized
            // (both stop it), not occluded (occlusion stops JavaScript), never
            // seen, and not clipped by wherever the app put this view.
            //
            // Not smaller, although WebKit keeps a backing store for the page at
            // the view's size -- 11 MiB of WebContent's memory at the window's
            // size on an iPhone 15 Pro. A one-point view (v0.9.13) presented
            // more late frames on the Canvas2D bench game in every one of six
            // interleaved pairs, and frame pacing outranks memory (README,
            // "host shape").
            host.view.frame = window.bounds.offsetBy(dx: -window.bounds.width - 1, dy: 0)
            window.addSubview(host.view)
            try host.start()

            if configuration.managesAudioSession { activateAudioSession() }
            observeApplication()
            isRunning = true
            pausedByApp = UIApplication.shared.applicationState != .active
            applyRunning()
            takeKeys()
        }

        /// Stop the producer, then close the engine. In that order: the frame
        /// channel belongs to the session, and a producer still submitting into
        /// a session being destroyed is submitting into freed memory.
        private func tearDown(then next: (() -> Void)?) {
            isRunning = false
            keyboard.hide(reportingTo: nil)
            touches.removeAll()
            primaryClicks.removeAll()
            pointerButtons = MigoPointerButtons()
            keysDown.removeAll()
            escapeReleasedLock = false
            followMice(false)
            windowService?.close()
            windowService = nil
            showDebugPanel(false)
            deviceStatus?.stop()
            deviceStatus = nil
            keepScreenOn(false)
            if let host {
                host.stop()
                host.view.removeFromSuperview()
            }
            host = nil
            laidOut = nil
            guard let engine else {
                next?()
                return
            }
            self.engine = nil
            closing += 1
            // The layer stays with this view, and the view outlives the close.
            engine.close { [weak self] released in
                guard let self else { return }
                self.closing -= 1
                if !released {
                    // The driver may still hold the old layer, so it is neither
                    // freed nor attached again: the next session gets a new one.
                    self.abandonedLayers.append(self.metalView)
                    self.metalView.removeFromSuperview()
                    let fresh = MetalView(frame: self.bounds)
                    fresh.autoresizingMask = [.flexibleWidth, .flexibleHeight]
                    self.insertSubview(fresh, at: 0)
                    self.metalView = fresh
                }
                next?()
                // A game loaded while this one was closing starts now.
                self.startIfPossible()
            }
        }

        private func restart() {
            guard let game else { return }
            tearDown { [weak self] in
                guard let self, self.game?.id == game.id else { return }
                self.startIfPossible()
                if self.isRunning { self.onEvent?(.restarted) }
            }
        }

        // MARK: - what the engine and the producer say

        private func engineEvent(_ event: MigoEngineSession.Event) {
            switch event {
            case .ready:
                break  // The producer's `ready` is the game's; see below.
            case .exitRequested:
                stop()
                onEvent?(.exitRequested)
            case .error(let code, let message, let recoverable):
                onEvent?(.error(code: code, message: message, recoverable: recoverable))
            case .surfaceLost:
                // The layer is this view's and it is still here; a lost surface
                // on iOS is the GPU going away, which a restart is the answer to.
                restart()
            case .showKeyboard(let request):
                keyboard.show(request)
            case .hideKeyboard:
                keyboard.hide()
                takeKeys()
            case .updateKeyboard(let text):
                keyboard.replaceText(text)
            case .vibrate(let vibration):
                Self.vibrate(vibration)
            case .keepScreenOn(let on):
                keepScreenOn(on)
            case .gameLog(let entry):
                onEvent?(.gameLog(entry))
            case .debugAids(let on):
                showDebugPanel(on)
            }
        }

        /// The debugging panel over the top of the game, from the moment the
        /// aids are turned on: what content writes from then is what it shows.
        private func showDebugPanel(_ on: Bool) {
            if on, debugPanel == nil {
                let panel = MigoDebugPanel(frames: { [weak self] in self?.engine?.frameStatistics })
                panel.frame = CGRect(x: 0, y: 0, width: bounds.width, height: (bounds.height * 0.4).rounded())
                panel.autoresizingMask = [.flexibleWidth, .flexibleHeight, .flexibleBottomMargin]
                addSubview(panel)
                debugPanel = panel
            } else if !on, let panel = debugPanel {
                panel.removeFromSuperview()
                debugPanel = nil
            }
        }

        /// A short vibration is the Taptic Engine's impact at the strength
        /// content named; a long one is the system vibration, about the 400 ms
        /// the API describes.
        private static func vibrate(_ vibration: MigoVibration) {
            switch vibration {
            case MigoVibration(MIGO_VIBRATION_LONG):
                AudioServicesPlaySystemSound(kSystemSoundID_Vibrate)
            case MigoVibration(MIGO_VIBRATION_SHORT_LIGHT):
                UIImpactFeedbackGenerator(style: .light).impactOccurred()
            case MigoVibration(MIGO_VIBRATION_SHORT_HEAVY):
                UIImpactFeedbackGenerator(style: .heavy).impactOccurred()
            default:
                UIImpactFeedbackGenerator(style: .medium).impactOccurred()
            }
        }

        /// The idle timer is the app's; the game borrows it and gives back
        /// whatever the app had set.
        private func keepScreenOn(_ on: Bool) {
            let application = UIApplication.shared
            if on {
                if idleTimerBeforeGame == nil { idleTimerBeforeGame = application.isIdleTimerDisabled }
                application.isIdleTimerDisabled = true
            } else if let before = idleTimerBeforeGame {
                application.isIdleTimerDisabled = before
                idleTimerBeforeGame = nil
            }
        }

        private func producerReport(_ report: MigoPerformancePlusHost.Report) {
            switch report["type"] as? String {
            case "ready":
                _ = recovery.contentBecameReady(generation: recovery.generation)
                onEvent?(.ready)
            case "failed":
                let stage = report["stage"] as? String ?? "?"
                let detail = report["detail"] as? String ?? "?"
                stop()
                onEvent?(.failed("the game failed at \(stage): \(detail)"))
            case "content-process-terminated":
                switch recovery.webContentTerminated() {
                case .rebuild:
                    restart()
                case .stop(let count):
                    stop()
                    onEvent?(
                        .failed("WebKit's content process died \(count) times before the game became ready"))
                }
            default:
                break
            }
        }

        // MARK: - lifecycle

        private func observeApplication() {
            guard observers.isEmpty else { return }
            let center = NotificationCenter.default
            observers.append(
                center.addObserver(forName: UIApplication.willResignActiveNotification, object: nil, queue: .main) {
                    [weak self] _ in
                    self?.pausedByApp = true
                    self?.applyRunning()
                })
            observers.append(
                center.addObserver(forName: UIApplication.didBecomeActiveNotification, object: nil, queue: .main) {
                    [weak self] _ in
                    self?.pausedByApp = false
                    self?.applyRunning()
                })
            guard configuration.managesAudioSession else { return }
            observers.append(
                center.addObserver(
                    forName: AVAudioSession.interruptionNotification, object: AVAudioSession.sharedInstance(),
                    queue: .main
                ) { [weak self] notification in
                    self?.audioInterruption(notification)
                })
        }

        private func applyRunning() {
            guard let engine, isRunning else { return }
            let running = !pausedByApp && !pausedByAudio
            engine.setFocused(!pausedByApp)
            engine.setVisible(!pausedByApp)
            engine.setRunning(running)
            if running { engine.startFrames() } else { engine.stopFrames() }
        }

        /// Ambient: a mini-game's sound mixes with the user's music and obeys
        /// the silent switch, which is what the platforms that defined the API
        /// do by default.
        private func activateAudioSession() {
            let session = AVAudioSession.sharedInstance()
            try? session.setCategory(.ambient, mode: .default, options: [])
            try? session.setActive(true)
        }

        /// A call or an alarm takes the audio hardware. The game pauses with it
        /// and resumes when the system says it may.
        private func audioInterruption(_ notification: Notification) {
            guard let raw = notification.userInfo?[AVAudioSessionInterruptionTypeKey] as? UInt,
                let type = AVAudioSession.InterruptionType(rawValue: raw)
            else { return }
            switch type {
            case .began:
                pausedByAudio = true
            case .ended:
                pausedByAudio = false
                try? AVAudioSession.sharedInstance().setActive(true)
            @unknown default:
                return
            }
            applyRunning()
        }

        // MARK: - touches

        public override func touchesBegan(_ touches: Set<UITouch>, with event: UIEvent?) {
            takeKeys()
            for touch in touches where touch.type == .indirectPointer {
                if event?.buttonMask.contains(.primary) == true { primaryClicks.insert(ObjectIdentifier(touch)) }
            }
            send(MIGO_TOUCH_START, changed: touches, event: event)
            pointerTouches(touches, event: event)
        }

        public override func touchesMoved(_ touches: Set<UITouch>, with event: UIEvent?) {
            send(MIGO_TOUCH_MOVE, changed: touches, event: event)
            pointerTouches(touches, event: event)
        }

        public override func touchesEnded(_ touches: Set<UITouch>, with event: UIEvent?) {
            send(MIGO_TOUCH_END, changed: touches, event: event)
            pointerTouches(touches, event: event)
        }

        public override func touchesCancelled(_ touches: Set<UITouch>, with event: UIEvent?) {
            send(MIGO_TOUCH_CANCEL, changed: touches, event: event)
            pointerTouches(touches, event: event)
        }

        /// One event with every finger on the view, `changed` flagged, the
        /// lifted ones flagged removed -- the shape `touches`/`changedTouches`
        /// have in the mini-game API. Coordinates in points, which are CSS
        /// pixels here because the surface's scale is the screen's.
        private func send(_ type: MigoTouchType, changed: Set<UITouch>, event: UIEvent?) {
            guard let engine, isRunning else { return }
            let lifting = type == MIGO_TOUCH_END || type == MIGO_TOUCH_CANCEL
            let all = event?.touches(for: self) ?? changed
            var points: [MigoTouchPoint] = []
            var timestamp: TimeInterval = 0
            for touch in all {
                // The pointer's other buttons are the mouse's alone.
                if touch.type == .indirectPointer, !primaryClicks.contains(ObjectIdentifier(touch)) { continue }
                let isChanged = changed.contains(touch)
                if isChanged { timestamp = max(timestamp, touch.timestamp) }
                // Fingers that ended in an earlier event can still be listed
                // while UIKit finishes the gesture; they are not on the surface.
                if !isChanged && (touch.phase == .ended || touch.phase == .cancelled) { continue }
                let location = touch.location(in: self)
                var flags = MIGO_TOUCH_FLAG_NONE
                if isChanged { flags |= MIGO_TOUCH_FLAG_CHANGED }
                if isChanged && lifting { flags |= MIGO_TOUCH_FLAG_REMOVED }
                let force = touch.maximumPossibleForce > 0 ? touch.force / touch.maximumPossibleForce : 0
                points.append(
                    MigoTouchPoint(
                        id: self.touches.id(for: touch), x: Float(location.x), y: Float(location.y),
                        pressure: Float(min(max(force, 0), 1)), flags: flags))
                if points.count == Int(MIGO_TOUCH_MAX_POINTS) { break }
            }
            if lifting {
                changed.forEach {
                    self.touches.forget($0)
                    primaryClicks.remove(ObjectIdentifier($0))
                }
            }
            guard !points.isEmpty else { return }
            // UITouch timestamps are seconds of system uptime; the engine wants
            // milliseconds on one monotonic clock, which this is.
            engine.sendTouch(type, points: points, timestampMilliseconds: Int64(timestamp * 1000))
        }

        // MARK: - mouse and trackpad

        /// The pointer's press, drag and release, as UIKit reports them: one
        /// touch from the first button down to the last up, with the buttons
        /// held in the event.
        private func pointerTouches(_ changed: Set<UITouch>, event: UIEvent?) {
            guard let touch = changed.first(where: { $0.type == .indirectPointer }) else { return }
            let lifted = touch.phase == .ended || touch.phase == .cancelled
            pointer(
                at: touch.location(in: self),
                holding: lifted ? [] : MigoPointerButtons.buttons(in: event?.buttonMask ?? []),
                moved: touch.phase == .moved, timestamp: touch.timestamp)
        }

        @objc private func hovered(_ recognizer: UIHoverGestureRecognizer) {
            switch recognizer.state {
            case .began, .changed:
                pointer(at: recognizer.location(in: self), holding: nil, moved: true, timestamp: CACurrentMediaTime())
            default:
                // Gone from the game, unless a button holds it there.
                if pointerButtons.held.isEmpty, windowService?.pointerLocked != true {
                    windowService?.pointerLocation = nil
                }
            }
        }

        /// The pointer at `location` -- points, which are CSS pixels here --
        /// holding `held` buttons, or the ones it held when `nil`; `moved` when
        /// it moved there. Locked, it stands where content's pointer is and
        /// only its buttons are heard: the mouse's raw motion moves it.
        /// `timestamp` is seconds of system uptime, as UIKit's are.
        func pointer(at location: CGPoint, holding held: Set<UInt32>?, moved: Bool, timestamp: TimeInterval) {
            guard let engine, isRunning else { return }
            let milliseconds = timestamp * 1000
            let locked = windowService?.lockedPosition
            if locked == nil { windowService?.pointerLocation = location }
            let at = locked ?? location
            if moved, locked == nil {
                engine.sendPointer(
                    MIGO_POINTER_EVENT_MOVE, button: pointerButtons.moving, x: Float(at.x), y: Float(at.y),
                    timestampMilliseconds: milliseconds)
            }
            guard let held else { return }
            let (released, pressed) = pointerButtons.hold(held)
            for button in released {
                engine.sendPointer(
                    MIGO_POINTER_EVENT_UP, button: button, x: Float(at.x), y: Float(at.y),
                    timestampMilliseconds: milliseconds)
            }
            for button in pressed {
                engine.sendPointer(
                    MIGO_POINTER_EVENT_DOWN, button: button, x: Float(at.x), y: Float(at.y),
                    timestampMilliseconds: milliseconds)
            }
        }

        @objc private func scrolled(_ recognizer: UIPanGestureRecognizer) {
            guard recognizer.state == .began || recognizer.state == .changed else { return }
            let translation = recognizer.translation(in: self)
            recognizer.setTranslation(.zero, in: self)
            scroll(by: translation, timestamp: CACurrentMediaTime())
        }

        /// A scroll that moved content by `translation`, in points. UIKit's
        /// translation is the way content moves, already in the player's
        /// scrolling direction; the DOM's delta is the way the view scrolls.
        func scroll(by translation: CGPoint, timestamp: TimeInterval) {
            guard let engine, isRunning, translation != .zero else { return }
            engine.sendWheel(
                deltaX: -Double(translation.x), deltaY: -Double(translation.y),
                timestampMilliseconds: timestamp * 1000)
        }

        /// While the pointer is locked it stands still, and the mice report
        /// their own motion and buttons through GameController, on the main
        /// queue. Their buttons and UIKit's are one set: a press is heard once
        /// whichever reports it first.
        private func followMice(_ locked: Bool) {
            lockedMice.forEach { $0.restore() }
            lockedMice = []
            if let mouseArrivals { NotificationCenter.default.removeObserver(mouseArrivals) }
            mouseArrivals = nil
            guard locked else { return }
            GCMouse.mice().forEach(follow)
            mouseArrivals = NotificationCenter.default.addObserver(
                forName: .GCMouseDidConnect, object: nil, queue: .main
            ) { [weak self] note in
                guard let mouse = note.object as? GCMouse else { return }
                self?.follow(mouse)
            }
        }

        private func follow(_ mouse: GCMouse) {
            guard let followed = FollowedMouse(
                mouse,
                moved: { [weak self] delta in self?.lockedMouseMoved(by: delta) },
                button: { [weak self] button, down in self?.lockedMouseButton(button, down: down) })
            else { return }
            lockedMice.append(followed)
        }

        /// The locked pointer moved by a mouse's raw motion.
        func lockedMouseMoved(by delta: CGVector) {
            guard let engine, isRunning, let at = windowService?.movePointer(by: delta) else { return }
            engine.sendPointer(
                MIGO_POINTER_EVENT_MOVE, button: pointerButtons.moving, x: Float(at.x), y: Float(at.y),
                timestampMilliseconds: CACurrentMediaTime() * 1000)
        }

        /// A mouse's button while the pointer is locked.
        func lockedMouseButton(_ button: UInt32, down: Bool) {
            guard let engine, isRunning, let at = windowService?.lockedPosition,
                pointerButtons.set(button, down: down)
            else { return }
            engine.sendPointer(
                down ? MIGO_POINTER_EVENT_DOWN : MIGO_POINTER_EVENT_UP, button: button, x: Float(at.x),
                y: Float(at.y), timestampMilliseconds: CACurrentMediaTime() * 1000)
        }

        // MARK: - hardware keys

        public override func pressesBegan(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
            let unheard = presses.filter { !key($0, down: true) }
            if !unheard.isEmpty { super.pressesBegan(unheard, with: event) }
        }

        public override func pressesEnded(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
            let unheard = presses.filter { !key($0, down: false) }
            if !unheard.isEmpty { super.pressesEnded(unheard, with: event) }
        }

        public override func pressesCancelled(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
            let unheard = presses.filter { !key($0, down: false) }
            if !unheard.isEmpty { super.pressesCancelled(unheard, with: event) }
        }

        /// One hardware key down or up; whether the game heard it. A key down
        /// again before it came up is a repeat, however iPadOS reports holding
        /// it. A key the game did not hear go down is not the game's to hear
        /// come up.
        private func key(_ press: UIPress, down: Bool) -> Bool {
            guard let key = press.key else { return false }
            return self.key(
                usage: key.keyCode.rawValue, characters: key.characters,
                charactersIgnoringModifiers: key.charactersIgnoringModifiers, modifiers: key.modifierFlags,
                down: down, timestamp: press.timestamp)
        }

        func key(
            usage: Int, characters: String, charactersIgnoringModifiers: String, modifiers: UIKeyModifierFlags,
            down: Bool, timestamp: TimeInterval
        ) -> Bool {
            guard let engine, isRunning,
                let named = MigoKeyboardHIDCodes.key(
                    usage: usage, characters: characters, charactersIgnoringModifiers: charactersIgnoringModifiers,
                    modifiers: modifiers)
            else { return false }
            let (key, code) = named
            if down {
                // Esc is the player's way out of a pointer lock, as in a browser.
                if code == "Escape", let windowService, windowService.pointerLocked {
                    windowService.exitPointerLock()
                    escapeReleasedLock = true
                    return true
                }
                let isRepeat = !keysDown.insert(code).inserted
                engine.sendKey(
                    MIGO_KEY_EVENT_DOWN, key: key, code: code, modifiers: MigoKeyboardHIDCodes.modifiers(modifiers),
                    isRepeat: isRepeat, timestampMilliseconds: timestamp * 1000)
            } else {
                if code == "Escape", escapeReleasedLock {
                    escapeReleasedLock = false
                    return true
                }
                guard keysDown.remove(code) != nil else { return false }
                engine.sendKey(
                    MIGO_KEY_EVENT_UP, key: key, code: code, modifiers: MigoKeyboardHIDCodes.modifiers(modifiers),
                    isRepeat: false, timestampMilliseconds: timestamp * 1000)
            }
            return true
        }

        // MARK: - keyboard

        fileprivate func keyboardInput(_ input: MigoEngineSession.KeyboardInput) {
            engine?.sendKeyboard(input)
        }

        // MARK: - helpers

        private static func contentRoot(of session: OpaquePointer) throws -> URL {
            var length = 0
            _ = migo_session_copy_content_root(session, nil, 0, &length)
            var buffer = [CChar](repeating: 0, count: length + 1)
            let copied = migo_session_copy_content_root(session, &buffer, buffer.count, &length)
            guard copied == MIGO_OK else {
                throw MigoEngineSession.Failure.call("migo_session_copy_content_root", copied)
            }
            return URL(fileURLWithPath: String(cString: buffer), isDirectory: true)
        }

        /// `iPhone14,2`, not `iPhone`: games key quality settings on it.
        private static func hardwareModel() -> String {
            var info = utsname()
            uname(&info)
            return withUnsafeBytes(of: &info.machine) { raw in
                String(decoding: raw.prefix(while: { $0 != 0 }), as: UTF8.self)
            }
        }
    }

    /// Drives the system keyboard for content's `migo.showKeyboard`.
    ///
    /// A text view rather than a field because it serves both shapes: a
    /// single-line request turns Return into confirm, a multi-line one keeps
    /// it. It is zero-sized and transparent -- the system only needs a first
    /// responder that accepts text; what the player sees is the game's field.
    private final class KeyboardResponder: NSObject, UITextViewDelegate {
        private weak var owner: MigoGameView?
        private let field = UITextView(frame: .zero)
        private var request: MigoEngineSession.KeyboardRequest?
        /// The text content last heard, so a change that ends where it began
        /// (a stripped newline, a truncated paste) is not reported twice.
        private var reported = ""
        private var observers: [NSObjectProtocol] = []

        init(owner: MigoGameView) {
            self.owner = owner
            super.init()
            field.delegate = self
            field.autocorrectionType = .no
            field.spellCheckingType = .no
            field.backgroundColor = .clear
            field.textColor = .clear
            field.tintColor = .clear
            owner.addSubview(field)
            let center = NotificationCenter.default
            observers.append(
                center.addObserver(
                    forName: UIResponder.keyboardWillChangeFrameNotification, object: nil, queue: .main
                ) { [weak self] note in self?.keyboardFrameChanged(note) })
            observers.append(
                center.addObserver(forName: UIResponder.keyboardWillHideNotification, object: nil, queue: .main) {
                    [weak self] _ in
                    guard self?.request != nil else { return }
                    self?.owner?.keyboardInput(.heightChange(0))
                })
        }

        deinit { observers.forEach(NotificationCenter.default.removeObserver) }

        func show(_ request: MigoEngineSession.KeyboardRequest) {
            self.request = request
            field.keyboardType = request.numeric ? .numberPad : .default
            field.returnKeyType = Self.returnKey(request.confirmType)
            field.text = request.defaultValue
            reported = request.defaultValue
            if field.isFirstResponder {
                field.reloadInputViews()
            } else {
                field.becomeFirstResponder()
            }
        }

        /// Whether content's keyboard is open.
        var isOpen: Bool { request != nil }

        func hide() { hide(reportingTo: owner) }

        /// Closing reports `complete` with the final text, as a player closing
        /// it does -- unless the view is going away and there is no one to tell.
        func hide(reportingTo target: MigoGameView?) {
            guard request != nil else { return }
            let text = field.text ?? ""
            request = nil
            field.resignFirstResponder()
            target?.keyboardInput(.complete(text))
        }

        func replaceText(_ text: String) {
            guard request != nil else { return }
            field.text = text
            reported = text
        }

        /// Every edit lands here -- typed, pasted, dictated or inserted -- which
        /// is why the rules live here and not in `shouldChangeTextIn`, which
        /// only some of those paths consult. A single-line field turns its
        /// newline into confirm; a limit truncates, as the macOS field does.
        func textViewDidChange(_ textView: UITextView) {
            guard let request else { return }
            var text = textView.text ?? ""
            let confirmed = !request.multiline && text.contains("\n")
            if confirmed { text = text.replacingOccurrences(of: "\n", with: "") }
            if request.maxLength > 0, text.count > request.maxLength {
                text = String(text.prefix(request.maxLength))
            }
            if text != textView.text { textView.text = text }
            if text != reported {
                reported = text
                owner?.keyboardInput(.input(text))
            }
            if confirmed {
                owner?.keyboardInput(.confirm(text))
                if !request.confirmHold {
                    hide()
                    owner?.takeKeys()
                }
            }
        }

        func textViewDidEndEditing(_ textView: UITextView) {
            // The player dismissed it (a swipe, another responder).
            hide()
        }

        private func keyboardFrameChanged(_ note: Notification) {
            guard request != nil, let owner, let window = owner.window,
                let frame = note.userInfo?[UIResponder.keyboardFrameEndUserInfoKey] as? CGRect
            else { return }
            // Points are CSS pixels here; the overlap with this view is what
            // content lays itself out around.
            let overlap = owner.convert(owner.bounds, to: window).intersection(window.convert(frame, from: nil))
            owner.keyboardInput(.heightChange(overlap.isNull ? 0 : Double(overlap.height)))
        }

        private static func returnKey(_ type: MigoKeyboardConfirmType) -> UIReturnKeyType {
            switch type {
            case MIGO_KEYBOARD_CONFIRM_NEXT: return .next
            case MIGO_KEYBOARD_CONFIRM_SEARCH: return .search
            case MIGO_KEYBOARD_CONFIRM_GO: return .go
            case MIGO_KEYBOARD_CONFIRM_SEND: return .send
            default: return .done
            }
        }
    }

    /// The layer the engine renders into.
    private final class MetalView: UIView {
        override class var layerClass: AnyClass { CAMetalLayer.self }
        var metalLayer: CAMetalLayer { layer as! CAMetalLayer }

        override init(frame: CGRect) {
            super.init(frame: frame)
            isUserInteractionEnabled = false
            isOpaque = true
        }

        @available(*, unavailable)
        required init?(coder: NSCoder) { fatalError() }

        /// Core Animation scales the drawable to the layer's bounds. The
        /// drawable's size is the engine's alone: its window surface is
        /// fixed-size and follows the game's canvas when that is smaller than
        /// the screen, which is how a browser composites a canvas, so a
        /// DPR-naive game presents a ninth of the pixels and its three
        /// drawables shrink with it. A later bounds change is scaled the same
        /// way.
        func configure(scale: CGFloat) {
            contentScaleFactor = scale
            metalLayer.contentsScale = scale
            metalLayer.contentsGravity = .resize
        }
    }

    /// A mouse followed while the pointer is locked. GameController gives each
    /// of a mouse's inputs one handler, and the app may have set its own: the
    /// app's are kept, still called, and put back when the lock ends.
    private final class FollowedMouse {
        private let mouse: GCMouse
        private let queue: DispatchQueue
        private let moved: GCMouseMoved?
        private let buttons: [(GCControllerButtonInput, GCControllerButtonValueChangedHandler?)]

        /// `moved` hears the motion as content's y runs, downwards; `button`
        /// a DOM button number and whether it is down. `nil` for a mouse with
        /// no input to follow.
        init?(
            _ mouse: GCMouse, moved onMove: @escaping (CGVector) -> Void,
            button onButton: @escaping (UInt32, Bool) -> Void
        ) {
            guard let input = mouse.mouseInput else { return nil }
            self.mouse = mouse
            queue = mouse.handlerQueue
            moved = input.mouseMovedHandler
            var numbered: [(GCControllerButtonInput, UInt32)] = [(input.leftButton, 0)]
            if let middle = input.middleButton { numbered.append((middle, 1)) }
            if let right = input.rightButton { numbered.append((right, 2)) }
            for (index, auxiliary) in (input.auxiliaryButtons ?? []).prefix(2).enumerated() {
                numbered.append((auxiliary, UInt32(3 + index)))
            }
            buttons = numbered.map { ($0.0, $0.0.pressedChangedHandler) }
            mouse.handlerQueue = .main
            let appMoved = moved
            input.mouseMovedHandler = { input, deltaX, deltaY in
                appMoved?(input, deltaX, deltaY)
                // GameController's y grows upwards.
                onMove(CGVector(dx: CGFloat(deltaX), dy: -CGFloat(deltaY)))
            }
            for ((button, number), (_, appHandler)) in zip(numbered, buttons) {
                button.pressedChangedHandler = { button, value, pressed in
                    appHandler?(button, value, pressed)
                    onButton(number, pressed)
                }
            }
        }

        /// The app's handlers and queue, as they were.
        func restore() {
            mouse.mouseInput?.mouseMovedHandler = moved
            for (button, handler) in buttons { button.pressedChangedHandler = handler }
            mouse.handlerQueue = queue
        }
    }

    /// Small, stable ids for the fingers on the view. The ABI's `id` is the
    /// web's `Touch.identifier`: stable from start to end and reused after,
    /// which is what games that index arrays by it expect.
    private struct TouchIdentities {
        private var ids: [ObjectIdentifier: UInt32] = [:]

        mutating func id(for touch: UITouch) -> UInt32 {
            let key = ObjectIdentifier(touch)
            if let id = ids[key] { return id }
            let used = Set(ids.values)
            var next: UInt32 = 0
            while used.contains(next) { next += 1 }
            ids[key] = next
            return next
        }

        mutating func forget(_ touch: UITouch) { ids[ObjectIdentifier(touch)] = nil }
        mutating func removeAll() { ids.removeAll() }
    }
#endif

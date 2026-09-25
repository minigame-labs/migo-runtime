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

#if os(iOS)
    import AVFoundation
    import AudioToolbox
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
    /// ## The size is fixed per session
    ///
    /// The game is told its window size once, before its first line runs, and
    /// lays itself out to it -- that is how the mini-game API works on every
    /// platform. So the session starts at the view's first non-empty layout and
    /// keeps that size: a later bounds change scales the rendered frame to the
    /// new bounds rather than lying to a game that has already laid itself out.
    /// Lock the hosting controller's orientation to the game's.
    ///
    /// ## The web view is not a view you see
    ///
    /// Content JavaScript runs in WebKit's content process because that is the
    /// only process iOS gives JIT to; its frames come back to this process and
    /// are drawn here. The `WKWebView` that owns that process is attached to the
    /// window, off screen: a web view that is not in a window is killed, and one
    /// that is occluded stops running JavaScript.
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

            public init(
                directories: MigoGameDirectories, contentSigning: MigoContentSigning,
                preferredFramesPerSecond: Int? = nil, managesAudioSession: Bool = true,
                webContentTerminationBudget: Int = 2
            ) {
                self.directories = directories
                self.contentSigning = contentSigning
                self.preferredFramesPerSecond = preferredFramesPerSecond
                self.managesAudioSession = managesAudioSession
                self.webContentTerminationBudget = webContentTerminationBudget
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
        private var deviceStatus: MigoDeviceStatusReporter?
        /// The app's own idle-timer setting while the game holds the display
        /// awake, `nil` while it does not; put back when the game lets go or
        /// ends.
        private var idleTimerBeforeGame: Bool?
        private var recovery: MigoWebContentRecovery
        private var touches = TouchIdentities()
        /// The system keyboard's responder: zero-sized and never drawn -- the
        /// game draws its own field, as it does on every platform.
        private lazy var keyboard = KeyboardResponder(owner: self)
        private var observers: [NSObjectProtocol] = []
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

        /// Stop the game and release everything it held. The view can load
        /// another game afterwards.
        public func stop() {
            game = nil
            tearDown(then: nil)
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
            let scale = window.screen.scale
            let points = bounds.size
            let size = MigoEngineSession.SurfaceSize(
                widthPixels: UInt32((points.width * scale).rounded()),
                heightPixels: UInt32((points.height * scale).rounded()),
                scale: Float(scale))
            metalView.configure(scale: scale, drawableSize: size)

            let engine = try MigoEngineSession(
                directories: configuration.directories, contentSigning: configuration.contentSigning,
                preferredFramesPerSecond: configuration.preferredFramesPerSecond)
            self.engine = engine
            engine.onEvent = { [weak self] event in self?.engineEvent(event) }
            // Before content runs, so its first `getNetworkType` has a report.
            let deviceStatus = MigoDeviceStatusReporter(session: engine)
            self.deviceStatus = deviceStatus
            deviceStatus.start()
            try engine.attach(layer: metalView.metalLayer, size: size)
            try engine.loadContent(id: id, entry: entry)
            let root = try Self.contentRoot(of: engine.session)

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
            host.onConsole = { [weak self] level, message in self?.onEvent?(.console(level: level, message: message)) }

            // Off the window's left edge and the full window's size: attached,
            // not occluded, never seen, and not clipped by wherever the app put
            // this view.
            host.view.frame = window.bounds.offsetBy(dx: -window.bounds.width - 1, dy: 0)
            window.addSubview(host.view)
            try host.start()

            if configuration.managesAudioSession { activateAudioSession() }
            observeApplication()
            isRunning = true
            pausedByApp = UIApplication.shared.applicationState != .active
            applyRunning()
        }

        /// Stop the producer, then close the engine. In that order: the frame
        /// channel belongs to the session, and a producer still submitting into
        /// a session being destroyed is submitting into freed memory.
        private func tearDown(then next: (() -> Void)?) {
            isRunning = false
            keyboard.hide(reportingTo: nil)
            touches.removeAll()
            deviceStatus?.stop()
            deviceStatus = nil
            keepScreenOn(false)
            if let host {
                host.stop()
                host.view.removeFromSuperview()
            }
            host = nil
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
            case .updateKeyboard(let text):
                keyboard.replaceText(text)
            case .vibrate(let vibration):
                Self.vibrate(vibration)
            case .keepScreenOn(let on):
                keepScreenOn(on)
            case .gameLog(let entry):
                onEvent?(.gameLog(entry))
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
            send(MIGO_TOUCH_START, changed: touches, event: event)
        }

        public override func touchesMoved(_ touches: Set<UITouch>, with event: UIEvent?) {
            send(MIGO_TOUCH_MOVE, changed: touches, event: event)
        }

        public override func touchesEnded(_ touches: Set<UITouch>, with event: UIEvent?) {
            send(MIGO_TOUCH_END, changed: touches, event: event)
        }

        public override func touchesCancelled(_ touches: Set<UITouch>, with event: UIEvent?) {
            send(MIGO_TOUCH_CANCEL, changed: touches, event: event)
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
            if lifting { changed.forEach { self.touches.forget($0) } }
            guard !points.isEmpty else { return }
            // UITouch timestamps are seconds of system uptime; the engine wants
            // milliseconds on one monotonic clock, which this is.
            engine.sendTouch(type, points: points, timestampMilliseconds: Int64(timestamp * 1000))
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
                if !request.confirmHold { hide() }
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

        /// The drawable is sized once per session; a later bounds change is
        /// scaled by Core Animation. `framebufferOnly` stays on: nothing samples
        /// the drawable, and off costs bandwidth on every frame.
        func configure(scale: CGFloat, drawableSize: MigoEngineSession.SurfaceSize) {
            contentScaleFactor = scale
            metalLayer.contentsScale = scale
            metalLayer.drawableSize = CGSize(
                width: CGFloat(drawableSize.widthPixels), height: CGFloat(drawableSize.heightPixels))
            metalLayer.contentsGravity = .resize
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

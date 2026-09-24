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

#if os(macOS)
    import AppKit
    import QuartzCore

    /// A view that runs one mini-game: the macOS product surface of Migo.
    ///
    /// Install the game once with `MigoGameInstaller`, add the view to a window
    /// and call `loadGame(id:entry:)`. The game's JavaScript runs in this
    /// process on V8 with JIT and draws into this view's `CAMetalLayer`
    /// through ANGLE on Metal. The Android SDK's
    /// `MigoGameView.loadGame(gameId, entryPoint)` is the same call, on purpose.
    ///
    /// ## What the app must do
    ///
    /// Sign with the hardened runtime and `com.apple.security.cs.allow-jit`.
    /// Without the entitlement the view refuses to start and says why
    /// (`unavailabilityReason`): V8 under a hardened runtime without it dies
    /// at its first compile, and a jitless V8 has no WebAssembly, so neither is
    /// offered in its place. Embed ANGLE's two dylibs with the package's
    /// `embed-apple-angle.sh` build phase.
    ///
    /// ## Input
    ///
    /// Mini-game content written for phones listens for touches; content for PC
    /// mini-game platforms listens for the mouse. The engine synthesises
    /// neither from the other -- only the host knows its content -- so
    /// `Configuration.mouse` says which to send. Physical keys are always sent
    /// while the view is first responder.
    public final class MigoGameView: NSView {

        /// Which stream a mouse feeds.
        public enum MouseInput: Sendable {
            /// One touch, id 0: phone-first content.
            case touch
            /// Mouse and wheel events: PC-first content.
            case pointer
            /// Both. Only for content known to listen to exactly one.
            case touchAndPointer
        }

        public struct Configuration {
            public var directories: MigoGameDirectories
            /// Whether packages must be signed. No default: see `MigoContentSigning`.
            public var contentSigning: MigoContentSigning
            public var preferredFramesPerSecond: Int?
            public var mouse: MouseInput

            public init(
                directories: MigoGameDirectories, contentSigning: MigoContentSigning,
                preferredFramesPerSecond: Int? = nil, mouse: MouseInput = .touch
            ) {
                self.directories = directories
                self.contentSigning = contentSigning
                self.preferredFramesPerSecond = preferredFramesPerSecond
                self.mouse = mouse
            }

            public static func standard(contentSigning: MigoContentSigning) throws -> Configuration {
                Configuration(directories: try .standard(), contentSigning: contentSigning)
            }
        }

        /// What happened to the game. Delivered on the main queue.
        public enum Event {
            case ready
            case exitRequested
            case failed(String)
            case error(code: Int32, message: String, recoverable: Bool)
        }

        public var onEvent: ((Event) -> Void)?

        public private(set) var isRunning = false

        /// What the display clock has done. `nil` while no game runs.
        public var frameClockStatistics: MigoSessionFrameClock.Statistics? { engine?.frameStatistics }

        /// Why this process cannot run the V8 lane, or `nil` if it can.
        public static var unavailabilityReason: String? {
            let decision = MigoMacV8Availability.resolve()
            if decision.profile != .macosV8Native { return decision.explanation }
            do {
                let preflight = try MigoEngineCapabilities.query().preflight()
                return preflight == .ready ? nil : "the linked engine cannot drive this view: \(preflight)"
            } catch {
                return "the linked engine did not answer its capability query: \(error)"
            }
        }

        private let configuration: Configuration
        private var game: (id: String, entry: String)?
        private var engine: MigoEngineSession?
        private var metalLayer: CAMetalLayer
        private var abandonedLayers: [CAMetalLayer] = []
        private var closing = 0
        private var observers: [NSObjectProtocol] = []
        private var mouseDown = false
        private var trackingArea: NSTrackingArea?
        /// Content's `migo.showKeyboard` on a machine with no soft keyboard: a
        /// text field along the game's bottom edge while content asks for one.
        private lazy var keyboard = KeyboardField(owner: self)

        public init(configuration: Configuration) {
            self.configuration = configuration
            self.metalLayer = Self.makeLayer()
            super.init(frame: .zero)
            wantsLayer = true
            layer = metalLayer
        }

        @available(*, unavailable)
        required init?(coder: NSCoder) { fatalError("MigoGameView is created in code") }

        deinit {
            observers.forEach(NotificationCenter.default.removeObserver)
        }

        public override var acceptsFirstResponder: Bool { true }
        public override var isFlipped: Bool { true }
        public override var wantsUpdateLayer: Bool { true }

        /// Run an installed game. It starts once the view is in a window with a
        /// non-empty size; calling this again replaces the running game.
        public func loadGame(id: String, entry: String = "game.js") {
            precondition(Thread.isMainThread, "MigoGameView is main-thread only")
            stop()
            if let reason = Self.unavailabilityReason {
                onEvent?(.failed(reason))
                return
            }
            game = (id, entry)
            startIfPossible()
        }

        /// Stop the game and release everything it held.
        public func stop() {
            game = nil
            tearDown()
            observers.forEach(NotificationCenter.default.removeObserver)
            observers = []
        }

        // MARK: - view

        public override func viewDidMoveToWindow() {
            super.viewDidMoveToWindow()
            if window == nil {
                if isRunning { tearDown() }
                return
            }
            window?.acceptsMouseMovedEvents = true
            startIfPossible()
        }

        public override func setFrameSize(_ newSize: NSSize) {
            super.setFrameSize(newSize)
            surfaceChanged()
        }

        public override func viewDidChangeBackingProperties() {
            super.viewDidChangeBackingProperties()
            surfaceChanged()
        }

        public override func updateTrackingAreas() {
            super.updateTrackingAreas()
            if let trackingArea { removeTrackingArea(trackingArea) }
            let area = NSTrackingArea(
                rect: bounds, options: [.mouseMoved, .activeInKeyWindow, .inVisibleRect], owner: self)
            addTrackingArea(area)
            trackingArea = area
        }

        private var currentSize: MigoEngineSession.SurfaceSize? {
            guard let window, bounds.width >= 1, bounds.height >= 1 else { return nil }
            let scale = window.backingScaleFactor
            return MigoEngineSession.SurfaceSize(
                widthPixels: UInt32((bounds.width * scale).rounded()),
                heightPixels: UInt32((bounds.height * scale).rounded()),
                scale: Float(scale))
        }

        /// ANGLE sizes its window surface from the layer's bounds times
        /// `contentsScale`, so both are kept true, and the engine is told.
        private func surfaceChanged() {
            guard let size = currentSize else { return }
            metalLayer.contentsScale = CGFloat(size.scale)
            metalLayer.drawableSize = CGSize(width: CGFloat(size.widthPixels), height: CGFloat(size.heightPixels))
            if let engine {
                do {
                    try engine.resize(to: size)
                } catch {
                    onEvent?(.error(code: MIGO_ERROR_INTERNAL, message: String(describing: error), recoverable: true))
                }
            } else {
                startIfPossible()
            }
        }

        // MARK: - start and stop

        private func startIfPossible() {
            guard let game, !isRunning, closing == 0, let size = currentSize else { return }
            do {
                metalLayer.contentsScale = CGFloat(size.scale)
                metalLayer.drawableSize = CGSize(width: CGFloat(size.widthPixels), height: CGFloat(size.heightPixels))
                let engine = try MigoEngineSession(
                    directories: configuration.directories, contentSigning: configuration.contentSigning,
                    preferredFramesPerSecond: configuration.preferredFramesPerSecond)
                self.engine = engine
                engine.onEvent = { [weak self] event in self?.engineEvent(event) }
                try engine.attach(layer: metalLayer, size: size)
                try engine.loadContent(id: game.id, entry: game.entry)
                isRunning = true
                observeWindow()
                applyState()
            } catch {
                tearDown()
                self.game = nil
                onEvent?(.failed(String(describing: error)))
            }
        }

        private func tearDown() {
            isRunning = false
            keyboard.hide(reportingTo: nil)
            mouseDown = false
            guard let engine else { return }
            self.engine = nil
            closing += 1
            engine.close { [weak self] released in
                guard let self else { return }
                self.closing -= 1
                if !released {
                    // The driver may still hold it: never attached again.
                    self.abandonedLayers.append(self.metalLayer)
                    self.metalLayer = Self.makeLayer()
                    self.layer = self.metalLayer
                }
                self.startIfPossible()
            }
        }

        private static func makeLayer() -> CAMetalLayer {
            let layer = CAMetalLayer()
            layer.isOpaque = true
            layer.backgroundColor = NSColor.black.cgColor
            return layer
        }

        private func engineEvent(_ event: MigoEngineSession.Event) {
            switch event {
            case .ready:
                onEvent?(.ready)
            case .exitRequested:
                stop()
                onEvent?(.exitRequested)
            case .error(let code, let message, let recoverable):
                onEvent?(.error(code: code, message: message, recoverable: recoverable))
            case .surfaceLost:
                // The view's layer is still here; start over on it.
                guard let game else { return }
                tearDown()
                self.game = game
            case .showKeyboard(let request):
                keyboard.show(request)
            case .hideKeyboard:
                keyboard.hide()
            case .updateKeyboard(let text):
                keyboard.replaceText(text)
            }
        }

        // MARK: - lifecycle

        private func observeWindow() {
            guard observers.isEmpty, let window else { return }
            let center = NotificationCenter.default
            let names: [Notification.Name] = [
                NSWindow.didBecomeKeyNotification, NSWindow.didResignKeyNotification,
                NSWindow.didMiniaturizeNotification, NSWindow.didDeminiaturizeNotification,
                NSWindow.didChangeOcclusionStateNotification,
            ]
            for name in names {
                observers.append(
                    center.addObserver(forName: name, object: window, queue: .main) { [weak self] _ in
                        self?.applyState()
                    })
            }
            for name in [NSApplication.didHideNotification, NSApplication.didUnhideNotification] {
                observers.append(
                    center.addObserver(forName: name, object: nil, queue: .main) { [weak self] _ in
                        self?.applyState()
                    })
            }
        }

        /// Paused when nobody can bring it back without acting -- the window is
        /// in the Dock or the app is hidden. Occlusion is visibility, not
        /// lifecycle: a covered window keeps its game running, as a background
        /// browser tab keeps its script running, and the display link simply
        /// stops delivering frames to a view that is not on screen.
        private func applyState() {
            guard let engine, isRunning, let window else { return }
            let running = !window.isMiniaturized && !NSApp.isHidden
            engine.setFocused(window.isKeyWindow)
            engine.setVisible(running && window.occlusionState.contains(.visible))
            engine.setRunning(running)
            if running { engine.startFrames(view: self) } else { engine.stopFrames() }
        }

        // MARK: - mouse

        private func cssPoint(_ event: NSEvent) -> CGPoint {
            // Points are CSS pixels: the surface's scale is the backing scale.
            convert(event.locationInWindow, from: nil)
        }

        private func milliseconds(_ event: NSEvent) -> Double { event.timestamp * 1000 }

        public override func mouseDown(with event: NSEvent) {
            window?.makeFirstResponder(self)
            mouseDown = true
            mouse(MIGO_TOUCH_START, MIGO_POINTER_EVENT_DOWN, button: 0, event)
        }

        public override func mouseDragged(with event: NSEvent) {
            mouse(MIGO_TOUCH_MOVE, MIGO_POINTER_EVENT_MOVE, button: 0, event)
        }

        public override func mouseUp(with event: NSEvent) {
            mouseDown = false
            mouse(MIGO_TOUCH_END, MIGO_POINTER_EVENT_UP, button: 0, event)
        }

        public override func mouseMoved(with event: NSEvent) {
            guard configuration.mouse != .touch, let engine, isRunning else { return }
            let point = cssPoint(event)
            engine.sendPointer(
                MIGO_POINTER_EVENT_MOVE, button: 0, x: Float(point.x), y: Float(point.y),
                timestampMilliseconds: milliseconds(event))
        }

        public override func rightMouseDown(with event: NSEvent) {
            pointerOnly(MIGO_POINTER_EVENT_DOWN, button: 2, event)
        }

        public override func rightMouseUp(with event: NSEvent) {
            pointerOnly(MIGO_POINTER_EVENT_UP, button: 2, event)
        }

        public override func otherMouseDown(with event: NSEvent) {
            pointerOnly(MIGO_POINTER_EVENT_DOWN, button: 1, event)
        }

        public override func otherMouseUp(with event: NSEvent) {
            pointerOnly(MIGO_POINTER_EVENT_UP, button: 1, event)
        }

        public override func scrollWheel(with event: NSEvent) {
            guard configuration.mouse != .touch, let engine, isRunning else { return }
            // AppKit's deltas point the way content moves; the DOM's point the
            // way the view scrolls, and line-based wheels report lines.
            let factor: Double = event.hasPreciseScrollingDeltas ? 1 : 40
            engine.sendWheel(
                deltaX: -Double(event.scrollingDeltaX) * factor, deltaY: -Double(event.scrollingDeltaY) * factor,
                timestampMilliseconds: milliseconds(event))
        }

        private func mouse(
            _ touch: MigoTouchType, _ pointer: MigoPointerEventType, button: UInt32, _ event: NSEvent
        ) {
            guard let engine, isRunning else { return }
            let point = cssPoint(event)
            if configuration.mouse != .pointer {
                var flags = MIGO_TOUCH_FLAG_CHANGED
                if touch == MIGO_TOUCH_END { flags |= MIGO_TOUCH_FLAG_REMOVED }
                engine.sendTouch(
                    touch,
                    points: [MigoTouchPoint(id: 0, x: Float(point.x), y: Float(point.y), pressure: 0, flags: flags)],
                    timestampMilliseconds: Int64(milliseconds(event)))
            }
            if configuration.mouse != .touch {
                engine.sendPointer(
                    pointer, button: button, x: Float(point.x), y: Float(point.y),
                    timestampMilliseconds: milliseconds(event))
            }
        }

        private func pointerOnly(_ type: MigoPointerEventType, button: UInt32, _ event: NSEvent) {
            guard configuration.mouse != .touch, let engine, isRunning else { return }
            let point = cssPoint(event)
            engine.sendPointer(
                type, button: button, x: Float(point.x), y: Float(point.y),
                timestampMilliseconds: milliseconds(event))
        }

        fileprivate func keyboardInput(_ input: MigoEngineSession.KeyboardInput) {
            engine?.sendKeyboard(input)
        }

        // MARK: - keys

        public override func keyDown(with event: NSEvent) {
            key(MIGO_KEY_EVENT_DOWN, event)
        }

        public override func keyUp(with event: NSEvent) {
            key(MIGO_KEY_EVENT_UP, event)
        }

        public override func flagsChanged(with event: NSEvent) {
            guard let code = MigoMacKeyCodes.code(forKeyCode: event.keyCode),
                let name = MigoMacKeyCodes.modifierKey(forCode: code)
            else { return }
            let down = MigoMacKeyCodes.modifierIsDown(code: code, flags: event.modifierFlags)
            send(key: name, code: code, down: down, isRepeat: false, event)
        }

        private func key(_ type: MigoKeyEventType, _ event: NSEvent) {
            guard let code = MigoMacKeyCodes.code(forKeyCode: event.keyCode) else { return }
            let key = MigoMacKeyCodes.namedKey(forCode: code) ?? event.characters ?? ""
            send(key: key, code: code, down: type == MIGO_KEY_EVENT_DOWN, isRepeat: event.isARepeat, event)
        }

        private func send(key: String, code: String, down: Bool, isRepeat: Bool, _ event: NSEvent) {
            guard let engine, isRunning else { return }
            var modifiers = MIGO_KEY_MODIFIER_NONE
            let flags = event.modifierFlags
            if flags.contains(.control) { modifiers |= MIGO_KEY_MODIFIER_CONTROL }
            if flags.contains(.shift) { modifiers |= MIGO_KEY_MODIFIER_SHIFT }
            if flags.contains(.option) { modifiers |= MIGO_KEY_MODIFIER_ALT }
            if flags.contains(.command) { modifiers |= MIGO_KEY_MODIFIER_META }
            engine.sendKey(
                down ? MIGO_KEY_EVENT_DOWN : MIGO_KEY_EVENT_UP, key: key, code: code, modifiers: modifiers,
                isRepeat: isRepeat, timestampMilliseconds: milliseconds(event))
        }
    }

    /// The desktop's answer to `migo.showKeyboard`: a real field, because a
    /// Mac has a keyboard and no soft one, pinned along the game's bottom edge
    /// and reported to content as the keyboard's height.
    private final class KeyboardField: NSObject, NSTextFieldDelegate {
        private weak var owner: MigoGameView?
        private let field = NSTextField()
        private var request: MigoEngineSession.KeyboardRequest?
        private static let height: CGFloat = 28

        init(owner: MigoGameView) {
            self.owner = owner
            super.init()
            field.delegate = self
            field.isHidden = true
            field.translatesAutoresizingMaskIntoConstraints = false
            owner.addSubview(field)
            NSLayoutConstraint.activate([
                field.leadingAnchor.constraint(equalTo: owner.leadingAnchor),
                field.trailingAnchor.constraint(equalTo: owner.trailingAnchor),
                field.bottomAnchor.constraint(equalTo: owner.bottomAnchor),
                field.heightAnchor.constraint(equalToConstant: Self.height),
            ])
        }

        func show(_ request: MigoEngineSession.KeyboardRequest) {
            self.request = request
            field.stringValue = request.defaultValue
            field.usesSingleLineMode = !request.multiline
            field.isHidden = false
            owner?.window?.makeFirstResponder(field)
            owner?.keyboardInput(.heightChange(Double(Self.height)))
        }

        func hide() { hide(reportingTo: owner) }

        func hide(reportingTo target: MigoGameView?) {
            guard request != nil else { return }
            request = nil
            let text = field.stringValue
            field.isHidden = true
            if let owner, owner.window?.firstResponder === field.currentEditor() {
                owner.window?.makeFirstResponder(owner)
            }
            target?.keyboardInput(.heightChange(0))
            target?.keyboardInput(.complete(text))
        }

        func replaceText(_ text: String) {
            guard request != nil else { return }
            field.stringValue = text
        }

        func controlTextDidChange(_ notification: Notification) {
            guard let request else { return }
            var text = field.stringValue
            if request.maxLength > 0, text.count > request.maxLength {
                text = String(text.prefix(request.maxLength))
                field.stringValue = text
            }
            owner?.keyboardInput(.input(text))
        }

        func control(_ control: NSControl, textView: NSTextView, doCommandBy selector: Selector) -> Bool {
            guard let request, selector == #selector(NSResponder.insertNewline(_:)), !request.multiline else {
                return false
            }
            owner?.keyboardInput(.confirm(field.stringValue))
            if !request.confirmHold { hide() }
            return true
        }

        func controlTextDidEndEditing(_ notification: Notification) {
            hide()
        }
    }
#endif

import Foundation
import MigoAppleRenderer

#if os(iOS)
    import UIKit

    /// The game view's own part of the host-service channel: what only the
    /// scene the game is shown in can answer -- the window service, and the
    /// screen's orientation, which is the scene's to turn. It sits behind the
    /// app's provider, so what the app answers is the app's, and in front of
    /// the SDK's built-in services, which answer the rest of the screen.
    ///
    /// `scripts/gen-api-platform-matrix.py` reads `services` below to count
    /// these APIs as provided on iOS: one service per line.
    final class MigoGameViewHostServices: MigoHostServiceProvider {
        static let services: Set<MigoHostServiceID> = {
            var builtIn: Set<MigoHostServiceID> = []
            builtIn.insert(.window)
            return builtIn
        }()

        private let app: MigoHostServiceProvider?
        private let appServices: Set<MigoHostServiceID>
        private let window: MigoWindowService
        let services: Set<MigoHostServiceID>

        init(app: MigoHostServiceProvider?, window: MigoWindowService) {
            self.app = app
            self.window = window
            appServices = app?.services ?? []
            services = appServices.union(Self.services)
        }

        func answers(_ request: MigoHostServiceRequest) -> Bool {
            if let app, app.answers(request) { return true }
            return request.service == .window || Self.isOrientation(request)
        }

        func handle(_ request: MigoHostServiceRequest) {
            if let app, app.answers(request) {
                app.handle(request)
            } else if request.service == .window {
                window.handle(request)
            } else if Self.isOrientation(request) {
                window.setOrientation(request)
            } else {
                request.reply?.fail("not supported")
            }
        }

        private static func isOrientation(_ request: MigoHostServiceRequest) -> Bool {
            request.service == .screen && request.method == MigoHostServiceID.Screen.setDeviceOrientation
        }
    }

    /// The window service on iOS, answered by the game view and its scene: the
    /// pointer's look over the game, pointer lock, and the window's state. The
    /// size is the system's to give, so `setWindowSize` is not supported.
    ///
    /// iPadOS draws the pointer itself: an app picks a style -- the system's,
    /// none, a text beam, a shape -- never an image, so an image cursor leaves
    /// the pointer as it was. It locks the pointer for a scene that fills the
    /// screen, on the word of the view controller showing it
    /// (`prefersPointerLocked`), and says so on the scene: a lock is asked of
    /// the controller and taken when the scene reports it, and while it is held
    /// the mouse's raw motion moves content's pointer (`movePointer(by:)`) from
    /// where it stood. It is taken only with the pointer over the game, and
    /// released by Esc, by the system (the app leaving the screen, the scene
    /// leaving full screen), and when the game ends.
    final class MigoWindowService: NSObject, UIPointerInteractionDelegate {
        /// The system's part of a pointer lock, apart so a test can lock
        /// without a scene.
        struct PointerLockSystem {
            /// Whether the view's scene can lock the pointer at all: an iPad's.
            var isAvailable: () -> Bool
            /// Have the controller showing the game read the view's preference
            /// again; whether it now prefers what the view does -- an app that
            /// does not pass the view's preference on never locks.
            var update: (_ prefersLocked: Bool) -> Bool

            /// The view's scene and the controller that shows the view.
            static func system(for view: UIView) -> PointerLockSystem {
                PointerLockSystem(
                    isAvailable: { [weak view] in view?.window?.windowScene?.pointerLockState != nil },
                    update: { [weak view] prefersLocked in
                        guard let controller = view.flatMap(Self.controller(showing:)) else { return false }
                        controller.setNeedsUpdateOfPrefersPointerLocked()
                        return controller.prefersPointerLocked == prefersLocked
                    })
            }

            private static func controller(showing view: UIView) -> UIViewController? {
                var responder: UIResponder? = view
                while let next = responder?.next {
                    if let controller = next as? UIViewController { return controller }
                    responder = next
                }
                return nil
            }
        }

        /// What iPadOS can show for a CSS cursor keyword.
        enum PointerAppearance: Equatable {
            case system
            case hidden
            /// The I-beam over horizontal text.
            case textBeam
            /// The beam over vertical text.
            case verticalTextBeam
            case crosshair

            /// What iPadOS has for a keyword: none, the text beams and a cross;
            /// everything else -- the hand, the resizes, the waits -- is the
            /// system's pointer.
            static func forKeyword(_ keyword: String) -> PointerAppearance {
                switch keyword {
                case "none": return .hidden
                case "text": return .textBeam
                case "vertical-text": return .verticalTextBeam
                case "crosshair": return .crosshair
                default: return .system
                }
            }
        }

        private weak var view: UIView?
        private let system: PointerLockSystem
        /// Where the service's events go -- the window's, and the screen's
        /// turning -- the session's channel.
        private let post: (MigoHostServiceID, UInt32, [String: Any]) -> Void
        private var interaction: UIPointerInteraction?
        private var observers: [NSObjectProtocol] = []
        private var geometry: NSKeyValueObservation?
        /// The state last reported, or the one the game started in, which is
        /// no change.
        private var reportedState: String?
        /// The screen's orientation last reported, or the one the game started
        /// in.
        private var reportedOrientation: String?
        /// A turn content asked for that the scene has not made yet, and its
        /// number: a refusal UIKit sends late names the request it was for.
        private var pendingTurn: (number: Int, shape: String, reply: MigoHostServiceReply, deadline: DispatchWorkItem)?
        private var turns = 0
        /// How long the scene has to turn: its rotation takes a fraction of a
        /// second, and a request UIKit neither makes nor refuses must still be
        /// answered.
        static let turnBudget: TimeInterval = 3

        /// The pointer's look over the game, as content last asked.
        private(set) var appearance: PointerAppearance = .system
        /// Whether content holds the pointer locked or has asked to: what the
        /// view's `prefersPointerLocked` answers.
        private(set) var prefersLocked = false
        /// Where content's pointer is while the lock is held, in the view's
        /// coordinates; `nil` while it is not.
        private(set) var lockedPosition: CGPoint?
        /// Where the pointer stands over the game, `nil` while it is elsewhere:
        /// the view's to keep, from the pointer's hover and presses.
        var pointerLocation: CGPoint?
        /// Told when the lock is taken or released, after content is.
        var onLockChange: ((Bool) -> Void)?

        var pointerLocked: Bool { lockedPosition != nil }

        /// `view` is the game's, in its window.
        init(
            view: UIView, system: PointerLockSystem? = nil,
            post: @escaping (MigoHostServiceID, UInt32, [String: Any]) -> Void
        ) {
            self.view = view
            self.system = system ?? .system(for: view)
            self.post = post
            super.init()
            let interaction = UIPointerInteraction(delegate: self)
            view.addInteraction(interaction)
            self.interaction = interaction
            guard let scene = view.window?.windowScene else { return }
            reportedState = Self.state(of: scene)
            reportedOrientation = Self.orientation(of: scene)
            let center = NotificationCenter.default
            for name in [UIScene.didEnterBackgroundNotification, UIScene.willEnterForegroundNotification] {
                observers.append(
                    center.addObserver(forName: name, object: scene, queue: .main) { [weak self] _ in
                        self?.reportState()
                    })
            }
            observers.append(
                center.addObserver(forName: UIPointerLockState.didChangeNotification, object: nil, queue: .main) {
                    [weak self] note in
                    guard let self, let scene = note.userInfo?[UIPointerLockState.sceneUserInfoKey] as? UIScene,
                        scene === self.view?.window?.windowScene
                    else { return }
                    self.systemLockChanged(scene.pointerLockState?.isLocked ?? false)
                })
            if #available(iOS 16.0, *) {
                // Split View, Slide Over and Stage Manager resize the scene
                // without moving it to the background.
                geometry = scene.observe(\.effectiveGeometry) { [weak self] _, _ in
                    DispatchQueue.main.async { self?.reportState() }
                }
            }
        }

        deinit {
            observers.forEach(NotificationCenter.default.removeObserver)
        }

        /// The game ended: the lock is given back, the pointer is the system's
        /// again, and a request still on its way finds no view.
        func close() {
            observers.forEach(NotificationCenter.default.removeObserver)
            observers = []
            geometry = nil
            settleTurn(failing: "the game ended")
            if prefersLocked {
                prefersLocked = false
                _ = system.update(false)
            }
            lockedPosition = nil
            appearance = .system
            if let interaction {
                interaction.invalidate()
                view?.removeInteraction(interaction)
            }
            interaction = nil
            view = nil
        }

        func handle(_ request: MigoHostServiceRequest) {
            typealias Method = MigoHostServiceID.Window
            switch request.method {
            case Method.setCursor:
                setCursor(request.object)
            case Method.requestPointerLock:
                requestPointerLock()
            case Method.exitPointerLock:
                exitPointerLock()
            default:
                request.reply?.fail("not supported")
            }
        }

        // MARK: - state

        /// The platform's words: a scene in the background is minimized, one
        /// that fills its screen maximized, any other -- beside another app, in
        /// a Stage Manager window -- normal.
        static func state(of scene: UIWindowScene) -> String {
            if scene.activationState == .background { return "minimize" }
            let window = scene.coordinateSpace.bounds.size
            let screen = scene.screen.bounds.size
            return window.width >= screen.width && window.height >= screen.height ? "maximize" : "normalize"
        }

        /// Report the scene's state and its screen's orientation if they
        /// changed; the view calls this on layout too, which a resize or a turn
        /// of the scene reaches.
        func reportState() {
            guard let scene = view?.window?.windowScene else { return }
            let state = Self.state(of: scene)
            if state != reportedState {
                reportedState = state
                post(.window, MigoHostServiceID.Window.Event.windowStateChange, ["state": state])
            }
            if let orientation = Self.orientation(of: scene), orientation != reportedOrientation {
                reportedOrientation = orientation
                post(.screen, MigoHostServiceID.Screen.Event.deviceOrientationChange, ["value": orientation])
            }
            if let turn = pendingTurn, Self.shape(of: reportedOrientation) == turn.shape {
                turn.deadline.cancel()
                pendingTurn = nil
                turn.reply.succeed()
            }
        }

        /// `"portrait"` or `"landscape"`: the shape an orientation is.
        static func shape(of orientation: String?) -> String? {
            switch orientation {
            case "portrait": return "portrait"
            case "landscape", "landscapeReverse": return "landscape"
            default: return nil
            }
        }

        /// `setDeviceOrientation`: the scene asked to turn to the shape content
        /// wants -- either landscape, as the device is held -- answered when
        /// the interface has turned (`reportState`), or with UIKit's refusal: the
        /// view controller showing the game does not support that orientation.
        /// Already that shape, it succeeds at once. Before iOS 16 a scene cannot
        /// be asked.
        func setOrientation(_ request: MigoHostServiceRequest) {
            guard let reply = request.reply else { return }
            guard let value = request.object["value"] as? String, value == "portrait" || value == "landscape" else {
                reply.fail("value must be portrait or landscape")
                return
            }
            guard let scene = view?.window?.windowScene else {
                reply.fail("there is no window to turn")
                return
            }
            guard #available(iOS 16.0, *) else {
                reply.fail("turning the screen needs iOS 16")
                return
            }
            if Self.shape(of: Self.orientation(of: scene)) == value {
                reply.succeed()
                return
            }
            settleTurn(failing: "another turn was asked for")
            turns += 1
            let number = turns
            let deadline = DispatchWorkItem { [weak self] in
                self?.settleTurn(failing: "the screen did not turn", only: number)
            }
            pendingTurn = (number, value, reply, deadline)
            DispatchQueue.main.asyncAfter(deadline: .now() + Self.turnBudget, execute: deadline)
            let mask: UIInterfaceOrientationMask = value == "portrait" ? .portrait : .landscape
            scene.requestGeometryUpdate(.iOS(interfaceOrientations: mask)) { [weak self] error in
                let message = error.localizedDescription
                DispatchQueue.main.async { self?.settleTurn(failing: message, only: number) }
            }
        }

        /// Fail the pending turn -- `only` the one numbered, when given.
        private func settleTurn(failing message: String, only number: Int? = nil) {
            guard let turn = pendingTurn, number == nil || turn.number == number else { return }
            turn.deadline.cancel()
            pendingTurn = nil
            turn.reply.fail(message)
        }

        /// The screen's orientation in the platform's words: the interface's,
        /// not the device's -- a game locked to landscape hears the turn
        /// between the two landscapes and nothing when the device is stood up.
        /// Landscape is the home side to the right.
        static func orientation(of scene: UIWindowScene) -> String? {
            if #available(iOS 16.0, *) {
                return orientationName(scene.effectiveGeometry.interfaceOrientation)
            }
            return orientationName(scene.interfaceOrientation)
        }

        static func orientationName(_ orientation: UIInterfaceOrientation) -> String? {
            switch orientation {
            case .portrait, .portraitUpsideDown: return "portrait"
            case .landscapeRight: return "landscape"
            case .landscapeLeft: return "landscapeReverse"
            default: return nil
            }
        }

        // MARK: - cursor

        private func setCursor(_ options: [String: Any]) {
            if let keyword = options["keyword"] as? String {
                appearance = .forKeyword(keyword)
                interaction?.invalidate()
            } else if options["path"] != nil {
                NSLog("MigoGameView: setCursor: iPadOS draws no image as the pointer; the pointer is unchanged")
            }
        }

        func pointerInteraction(_ interaction: UIPointerInteraction, styleFor region: UIPointerRegion)
            -> UIPointerStyle?
        {
            Self.style(for: appearance)
        }

        static func style(for appearance: PointerAppearance) -> UIPointerStyle {
            // A beam as long as a line of body text, as text views draw theirs.
            let line = UIFont.preferredFont(forTextStyle: .body).lineHeight
            switch appearance {
            case .system:
                return .system()
            case .hidden:
                return .hidden()
            case .textBeam:
                return UIPointerStyle(shape: .verticalBeam(length: line), constrainedAxes: [])
            case .verticalTextBeam:
                return UIPointerStyle(shape: .horizontalBeam(length: line), constrainedAxes: [])
            case .crosshair:
                let arm: CGFloat = 10
                let width: CGFloat = 2
                let cross = UIBezierPath(rect: CGRect(x: -arm, y: -width / 2, width: arm * 2, height: width))
                cross.append(UIBezierPath(rect: CGRect(x: -width / 2, y: -arm, width: width, height: arm * 2)))
                return UIPointerStyle(shape: .path(cross), constrainedAxes: [])
            }
        }

        // MARK: - pointer lock

        private func requestPointerLock() {
            guard !prefersLocked else { return }
            guard pointerLocation != nil, system.isAvailable() else {
                report(locked: false)
                return
            }
            prefersLocked = true
            guard system.update(true) else {
                prefersLocked = false
                _ = system.update(false)
                NSLog(
                    "MigoGameView: requestPointerLock: the view controller showing the game does not return the "
                        + "view's prefersPointerLocked from its own, so iPadOS is never asked for the lock")
                report(locked: false)
                return
            }
            // The scene says whether the system took it: `systemLockChanged`.
        }

        func exitPointerLock() {
            guard prefersLocked else { return }
            prefersLocked = false
            _ = system.update(false)
            guard lockedPosition != nil else { return }
            lockedPosition = nil
            report(locked: false)
        }

        /// The scene's pointer lock, as the system decided it.
        func systemLockChanged(_ locked: Bool) {
            if locked, prefersLocked, lockedPosition == nil {
                lockedPosition = pointerLocation ?? view.map { CGPoint(x: $0.bounds.midX, y: $0.bounds.midY) } ?? .zero
                report(locked: true)
            } else if !locked, lockedPosition != nil {
                // Taken back by the system: content must ask again.
                lockedPosition = nil
                prefersLocked = false
                _ = system.update(false)
                report(locked: false)
            }
        }

        /// Add the mouse's raw motion to the locked pointer: where content sees
        /// it now. `nil` while no lock is held.
        func movePointer(by delta: CGVector) -> CGPoint? {
            guard var position = lockedPosition else { return nil }
            position.x += delta.dx
            position.y += delta.dy
            lockedPosition = position
            return position
        }

        private func report(locked: Bool) {
            post(.window, MigoHostServiceID.Window.Event.pointerLockChange, ["locked": locked])
            onLockChange?(locked)
        }
    }
#endif

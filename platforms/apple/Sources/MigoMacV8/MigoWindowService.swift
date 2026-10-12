import Foundation
import MigoAppleRenderer

#if os(macOS)
    import AppKit

    /// The game view's own part of the host-service channel: what only the
    /// window the game is shown in can answer -- the window service. It sits
    /// behind the app's provider, so a service the app names is the app's, and
    /// in front of the SDK's built-in services.
    ///
    /// `scripts/gen-api-platform-matrix.py` reads `services` below to count
    /// these APIs as provided on macOS: one service per line.
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

        func handle(_ request: MigoHostServiceRequest) {
            if let app, appServices.contains(request.service) {
                app.handle(request)
            } else if request.service == .window {
                window.handle(request)
            } else {
                request.reply?.fail("not supported")
            }
        }
    }

    /// The window service, answered by the game view's window: its size, the
    /// cursor over the game, pointer lock, and the window's state.
    ///
    /// A pointer lock freezes the cursor where it stands and hides it; the
    /// mouse keeps reporting its raw motion, which the view adds to where the
    /// cursor stood (`movePointer(by:)`), so content's movementX/movementY keep
    /// moving. It is taken only while the player is at the window with the
    /// cursor over the game -- a lock taken with the cursor elsewhere would
    /// send the player's clicks wherever it stands -- and it is released by
    /// Esc, by the window losing key, and when the game ends.
    final class MigoWindowService {
        /// The system's part of a pointer lock, apart so a test can lock
        /// without taking the machine's mouse.
        struct PointerCapture {
            /// Whether the player is at `window` now: it is key in the active app.
            var isFrontmost: (NSWindow) -> Bool
            /// Where the cursor is, in `window`'s coordinates.
            var location: (NSWindow) -> NSPoint
            /// Freeze the cursor where it stands and hide it.
            var engage: () -> Void
            /// Let it move and show it again.
            var release: () -> Void

            static let system = PointerCapture(
                isFrontmost: { $0.isKeyWindow && NSApp.isActive },
                location: { $0.mouseLocationOutsideOfEventStream },
                engage: {
                    CGAssociateMouseAndMouseCursorPosition(0)
                    NSCursor.hide()
                },
                release: {
                    CGAssociateMouseAndMouseCursorPosition(1)
                    NSCursor.unhide()
                })
        }

        private weak var view: NSView?
        private let capture: PointerCapture
        /// Where the service's events go: the session's channel.
        private let post: (UInt32, [String: Any]) -> Void
        private var observers: [NSObjectProtocol] = []
        /// The state last reported, or the one the game started in, which is
        /// no change.
        private var reportedState: String?

        /// The cursor content asked for over the game.
        private(set) var cursor: NSCursor = .arrow
        /// Where content's pointer is while the lock is held, in the view's
        /// coordinates; `nil` while it is not.
        private(set) var lockedPosition: CGPoint?

        var pointerLocked: Bool { lockedPosition != nil }

        /// `view` is the game's, in its window.
        init(view: NSView, capture: PointerCapture = .system, post: @escaping (UInt32, [String: Any]) -> Void) {
            self.view = view
            self.capture = capture
            self.post = post
            guard let window = view.window else { return }
            reportedState = Self.state(of: window)
            let center = NotificationCenter.default
            let changes: [Notification.Name] = [
                NSWindow.didMiniaturizeNotification, NSWindow.didDeminiaturizeNotification,
                NSWindow.didResizeNotification, NSWindow.didEnterFullScreenNotification,
                NSWindow.didExitFullScreenNotification,
            ]
            for name in changes {
                observers.append(
                    center.addObserver(forName: name, object: window, queue: .main) { [weak self] _ in
                        self?.reportState()
                    })
            }
            observers.append(
                center.addObserver(forName: NSWindow.didResignKeyNotification, object: window, queue: .main) {
                    [weak self] _ in self?.exitPointerLock()
                })
        }

        deinit {
            observers.forEach(NotificationCenter.default.removeObserver)
            // The machine's mouse is never left frozen, whatever happened.
            if lockedPosition != nil { capture.release() }
        }

        /// The game ended: the lock is released, the cursor is the system's
        /// again, and a request still on its way finds no window.
        func close() {
            observers.forEach(NotificationCenter.default.removeObserver)
            observers = []
            if lockedPosition != nil {
                lockedPosition = nil
                capture.release()
            }
            cursor = .arrow
            if let view { view.window?.invalidateCursorRects(for: view) }
            view = nil
        }

        func handle(_ request: MigoHostServiceRequest) {
            typealias Method = MigoHostServiceID.Window
            switch request.method {
            case Method.setWindowSize:
                setWindowSize(request)
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

        // MARK: - size

        /// The size asked for is the game's, in pixels: the window grows or
        /// shrinks by what the game's view must, keeping its title bar where it
        /// is and on screen.
        private func setWindowSize(_ request: MigoHostServiceRequest) {
            guard let reply = request.reply else { return }
            let options = request.object
            guard let width = (options["width"] as? NSNumber)?.doubleValue,
                let height = (options["height"] as? NSNumber)?.doubleValue,
                width.isFinite, height.isFinite, width > 0, height > 0
            else {
                reply.fail("width and height must be positive numbers")
                return
            }
            guard let view, let window = view.window else {
                reply.fail("there is no window to size")
                return
            }
            guard !window.styleMask.contains(.fullScreen) else {
                reply.fail("the window is full screen")
                return
            }
            let scale = window.backingScaleFactor
            let game = view.convert(view.bounds, to: nil).size
            let content = window.contentRect(forFrameRect: window.frame)
            let size = NSSize(
                width: max(1, content.width + CGFloat(width) / scale - game.width),
                height: max(1, content.height + CGFloat(height) / scale - game.height))
            var frame = window.frameRect(forContentRect: NSRect(origin: content.origin, size: size))
            frame.origin.y = window.frame.maxY - frame.height
            window.setFrame(window.constrainFrameRect(frame, to: window.screen), display: true)
            reply.succeed()
        }

        // MARK: - state

        /// The platform's words: a window in the Dock is minimized, a zoomed or
        /// full-screen one maximized.
        static func state(of window: NSWindow) -> String {
            if window.isMiniaturized { return "minimize" }
            return window.styleMask.contains(.fullScreen) || window.isZoomed ? "maximize" : "normalize"
        }

        private func reportState() {
            guard let window = view?.window else { return }
            let state = Self.state(of: window)
            guard state != reportedState else { return }
            reportedState = state
            post(MigoHostServiceID.Window.Event.windowStateChange, ["state": state])
        }

        // MARK: - cursor

        private func setCursor(_ options: [String: Any]) {
            let next: NSCursor
            if let keyword = options["keyword"] as? String {
                next = Self.cursor(forKeyword: keyword)
            } else if let path = options["path"] as? String {
                let hotSpot = CGPoint(
                    x: (options["x"] as? NSNumber)?.doubleValue ?? 0, y: (options["y"] as? NSNumber)?.doubleValue ?? 0)
                guard let image = Self.cursor(atPath: path, hotSpot: hotSpot) else {
                    NSLog("MigoGameView: setCursor: \(path) is not an image macOS reads; the cursor is unchanged")
                    return
                }
                next = image
            } else {
                return
            }
            cursor = next
            guard let view, let window = view.window else { return }
            window.invalidateCursorRects(for: view)
            // Cursor rects are rebuilt when the mouse next moves; one standing
            // over the game changes now.
            if window.isKeyWindow, Self.isOver(view, view.convert(capture.location(window), from: nil)) {
                next.set()
            }
        }

        /// A CSS cursor keyword as the system's cursor. What macOS has no public
        /// cursor for -- help, progress, wait, cell, move, all-scroll, and before
        /// macOS 15 the diagonal resizes and the zooms -- is the arrow.
        static func cursor(forKeyword keyword: String) -> NSCursor {
            if #available(macOS 15, *), let cursor = frameCursor(forKeyword: keyword) {
                return cursor
            }
            switch keyword {
            case "none": return hidden
            case "pointer": return .pointingHand
            case "text": return .iBeam
            case "vertical-text": return .iBeamCursorForVerticalLayout
            case "crosshair": return .crosshair
            case "context-menu": return .contextualMenu
            case "alias": return .dragLink
            case "copy": return .dragCopy
            case "no-drop", "not-allowed": return .operationNotAllowed
            case "grab": return .openHand
            case "grabbing": return .closedHand
            case "n-resize": return .resizeUp
            case "s-resize": return .resizeDown
            case "e-resize": return .resizeRight
            case "w-resize": return .resizeLeft
            case "ns-resize", "row-resize": return .resizeUpDown
            case "ew-resize", "col-resize": return .resizeLeftRight
            default: return .arrow
            }
        }

        /// macOS 15's resize and zoom cursors, which name every CSS resize.
        @available(macOS 15, *)
        private static func frameCursor(forKeyword keyword: String) -> NSCursor? {
            let position: NSCursor.FrameResizePosition
            switch keyword {
            case "n-resize", "ns-resize": position = .top
            case "s-resize": position = .bottom
            case "e-resize", "ew-resize": position = .right
            case "w-resize": position = .left
            case "ne-resize", "nesw-resize": position = .topRight
            case "sw-resize": position = .bottomLeft
            case "nw-resize", "nwse-resize": position = .topLeft
            case "se-resize": position = .bottomRight
            case "row-resize": return .rowResize
            case "col-resize": return .columnResize
            case "zoom-in": return .zoomIn
            case "zoom-out": return .zoomOut
            default: return nil
            }
            return .frameResize(position: position, directions: .all)
        }

        /// `none`: a cursor with nothing to show, so the game draws its own.
        private static let hidden = NSCursor(
            image: NSImage(size: NSSize(width: 16, height: 16), flipped: false) { _ in true }, hotSpot: .zero)

        /// The image at `path` as a cursor, its hotspot given in the image's
        /// pixels and clamped into it; `nil` when it is no image macOS reads.
        static func cursor(atPath path: String, hotSpot: CGPoint) -> NSCursor? {
            guard let image = NSImage(contentsOfFile: path), image.size.width >= 1, image.size.height >= 1 else {
                return nil
            }
            let pixelsWide = image.representations.first?.pixelsWide ?? 0
            let scale = pixelsWide > 0 ? image.size.width / CGFloat(pixelsWide) : 1
            let point = CGPoint(
                x: min(max(0, hotSpot.x * scale), image.size.width - 1),
                y: min(max(0, hotSpot.y * scale), image.size.height - 1))
            return NSCursor(image: image, hotSpot: point)
        }

        // MARK: - pointer lock

        private func requestPointerLock() {
            guard lockedPosition == nil else { return }
            guard let view, let window = view.window, capture.isFrontmost(window) else {
                report(locked: false)
                return
            }
            let point = view.convert(capture.location(window), from: nil)
            guard Self.isOver(view, point) else {
                report(locked: false)
                return
            }
            capture.engage()
            lockedPosition = point
            report(locked: true)
        }

        func exitPointerLock() {
            guard lockedPosition != nil else { return }
            lockedPosition = nil
            capture.release()
            report(locked: false)
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

        /// Whether `point` is on the part of `view` that shows: its visible
        /// rect alone reaches past its bounds where nothing clips it.
        private static func isOver(_ view: NSView, _ point: CGPoint) -> Bool {
            view.visibleRect.intersection(view.bounds).contains(point)
        }

        private func report(locked: Bool) {
            post(MigoHostServiceID.Window.Event.pointerLockChange, ["locked": locked])
        }
    }
#endif

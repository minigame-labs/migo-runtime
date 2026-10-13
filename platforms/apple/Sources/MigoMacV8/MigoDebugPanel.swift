#if os(macOS)
    import AppKit
    import MigoAppleRenderer
    import QuartzCore

    /// The game view's debugging panel, shown while the debugging aids are on
    /// (`migo.setEnableDebug`): the frames a second the display clock
    /// delivered and the latest lines content wrote to its console, read from
    /// the session, over the top of the game. It takes no clicks -- the game
    /// stays playable under it -- and follows the newest line.
    final class MigoDebugPanel: NSView {
        private let scroll = NSScrollView()
        private let text = NSTextView()
        private(set) var aids = MigoDebugAids()
        private var cursor: UInt64 = 0
        private var timer: Timer?
        private weak var session: MigoEngineSession?

        init(session: MigoEngineSession) {
            self.session = session
            super.init(frame: .zero)
            wantsLayer = true
            layer?.backgroundColor = NSColor(white: 0.08, alpha: 0.78).cgColor
            text.isEditable = false
            text.isSelectable = false
            text.drawsBackground = false
            text.textContainerInset = NSSize(width: 6, height: 4)
            scroll.documentView = text
            scroll.drawsBackground = false
            scroll.hasVerticalScroller = false
            scroll.frame = bounds
            scroll.autoresizingMask = [.width, .height]
            text.autoresizingMask = [.width]
            addSubview(scroll)
            setAccessibilityIdentifier("MigoDebugPanel")
        }

        @available(*, unavailable)
        required init?(coder: NSCoder) { fatalError() }

        /// Clicks fall through to the game.
        override func hitTest(_ point: NSPoint) -> NSView? { nil }

        override func viewDidMoveToWindow() {
            super.viewDidMoveToWindow()
            timer?.invalidate()
            timer = nil
            guard window != nil else { return }
            refresh()
            let timer = Timer(timeInterval: 0.5, repeats: true) { [weak self] _ in self?.refresh() }
            RunLoop.main.add(timer, forMode: .common)
            self.timer = timer
        }

        /// Read what content wrote since the last read, sample the clock and
        /// redraw: twice a second, however fast content writes.
        func refresh() {
            guard let session else { return }
            if let read = session.consoleLogs(since: cursor) {
                aids.append(read.entries)
                cursor = read.next
            }
            aids.sample(delivered: session.frameStatistics.delivered, at: CACurrentMediaTime())
            let font = NSFont.monospacedSystemFont(ofSize: 10, weight: .regular)
            let shown = NSMutableAttributedString(
                string: aids.header + "\n",
                attributes: [
                    .font: NSFont.monospacedSystemFont(ofSize: 11, weight: .semibold), .foregroundColor: NSColor.white,
                ])
            for line in aids.lines {
                shown.append(
                    NSAttributedString(
                        string: MigoDebugAids.text(of: line) + "\n",
                        attributes: [.font: font, .foregroundColor: Self.color(line.level)]))
            }
            text.textStorage?.setAttributedString(shown)
            text.scrollToEndOfDocument(nil)
        }

        private static func color(_ level: Int) -> NSColor {
            switch level {
            case 3: return NSColor(red: 0.94, green: 0.33, blue: 0.31, alpha: 1)
            case 2: return NSColor(red: 1, green: 0.72, blue: 0.30, alpha: 1)
            case 0: return NSColor(white: 0.62, alpha: 1)
            default: return NSColor(white: 0.88, alpha: 1)
            }
        }
    }
#endif

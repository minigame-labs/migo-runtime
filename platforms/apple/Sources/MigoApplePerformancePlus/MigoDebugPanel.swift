#if os(iOS)
    import MigoAppleRenderer
    import QuartzCore
    import UIKit

    /// The game view's debugging panel, shown while the debugging aids are on
    /// (`migo.setEnableDebug`): the frames a second the display clock
    /// delivered and the latest lines content wrote to its console, over the
    /// top of the game. It takes no touches -- the game stays playable under
    /// it -- and follows the newest line.
    final class MigoDebugPanel: UIView {
        private let text = UITextView()
        private(set) var aids = MigoDebugAids()
        private var timer: Timer?
        /// The display clock's statistics, read each refresh.
        private let frames: () -> MigoSessionFrameClock.Statistics?

        init(frames: @escaping () -> MigoSessionFrameClock.Statistics?) {
            self.frames = frames
            super.init(frame: .zero)
            isUserInteractionEnabled = false
            backgroundColor = UIColor(white: 0.08, alpha: 0.78)
            text.isEditable = false
            text.isSelectable = false
            text.isScrollEnabled = true
            text.backgroundColor = .clear
            text.textContainerInset = UIEdgeInsets(top: 4, left: 6, bottom: 4, right: 6)
            text.frame = bounds
            text.autoresizingMask = [.flexibleWidth, .flexibleHeight]
            addSubview(text)
            accessibilityIdentifier = "MigoDebugPanel"
        }

        @available(*, unavailable)
        required init?(coder: NSCoder) { fatalError() }

        /// Lines content wrote, as the producer forwarded them.
        func append(level: Int, message: String) {
            aids.append([MigoEngineSession.ConsoleEntry(level: level, date: Date(), message: message)])
        }

        override func didMoveToWindow() {
            super.didMoveToWindow()
            timer?.invalidate()
            timer = nil
            guard window != nil else { return }
            refresh()
            let timer = Timer(timeInterval: 0.5, repeats: true) { [weak self] _ in self?.refresh() }
            RunLoop.main.add(timer, forMode: .common)
            self.timer = timer
        }

        /// Sample the clock and redraw: twice a second, however fast content
        /// writes.
        func refresh() {
            if let statistics = frames() {
                aids.sample(delivered: statistics.delivered, at: CACurrentMediaTime())
            }
            let font = UIFont.monospacedSystemFont(ofSize: 10, weight: .regular)
            let shown = NSMutableAttributedString(
                string: aids.header + "\n",
                attributes: [.font: UIFont.monospacedSystemFont(ofSize: 11, weight: .semibold), .foregroundColor: UIColor.white])
            for line in aids.lines {
                shown.append(
                    NSAttributedString(
                        string: MigoDebugAids.text(of: line) + "\n",
                        attributes: [.font: font, .foregroundColor: Self.color(line.level)]))
            }
            text.attributedText = shown
            text.scrollRangeToVisible(NSRange(location: max(shown.length - 1, 0), length: 1))
        }

        private static func color(_ level: Int) -> UIColor {
            switch level {
            case 3: return UIColor(red: 0.94, green: 0.33, blue: 0.31, alpha: 1)
            case 2: return UIColor(red: 1, green: 0.72, blue: 0.30, alpha: 1)
            case 0: return UIColor(white: 0.62, alpha: 1)
            default: return UIColor(white: 0.88, alpha: 1)
            }
        }
    }
#endif

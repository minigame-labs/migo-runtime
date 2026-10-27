import Foundation

#if os(iOS)
    import UIKit
    private typealias Button = UIButton
    private typealias Label = UILabel
    private typealias Slider = UISlider
    private typealias Color = UIColor
#elseif os(macOS)
    import AppKit
    private typealias Button = NSButton
    private typealias Label = NSTextField
    private typealias Slider = NSSlider
    private typealias Color = NSColor
#endif

/// What a player's controls drive.
protocol MigoVideoControlled: AnyObject {
    var isPlaying: Bool { get }
    /// Seconds; 0 before it knows.
    var position: Double { get }
    /// Seconds; 0 before it knows.
    var duration: Double { get }
    var isFullScreen: Bool { get }
    func togglePlay()
    func seek(to seconds: Double)
    func toggleFullScreen()
}

/// A video's own controls, over its picture, as content's options ask -- the
/// Android SDK's, one for one:
///
///   controls               a bar along the bottom -- play or pause, the time,
///                          a progress bar (showProgressInControlMode), the
///                          duration, full screen. A tap shows it or hides it;
///                          while playing it hides again after three seconds.
///   showProgress           while the bar is hidden, a thin progress line along
///                          the bottom.
///   showCenterPlayBtn      a play button in the middle while it is not playing.
///   enablePlayGesture      a double tap plays or pauses.
///   enableProgressGesture  a sideways drag seeks, the whole width being the
///                          whole video, the time shown as it goes.
///
/// What the player does through them content hears as it hears its own calls.
/// With none of them asked for, the video takes no touch, and a touch on it is
/// the game's. The buttons carry the labels assistive technology reads --
/// "play", "play or pause", "full screen" -- which are the Android SDK's too.
/// Main thread.
final class MigoVideoControls: MigoVideoStage {
    static let optionKeys: Set<String> = [
        "controls", "showProgress", "showProgressInControlMode", "showCenterPlayBtn", "enablePlayGesture",
        "enableProgressGesture",
    ]
    private static let barShownSeconds = 3.0
    private static let barHeight: CGFloat = 36
    private static let centerSize: CGFloat = 56

    private weak var player: MigoVideoControlled?
    private var controls = true
    private var showProgress = true
    private var showProgressInBar = true
    private var showCenter = true
    private var playGesture = false
    private var progressGesture = true

    private var barShown = false
    private var hideBar: DispatchWorkItem?
    /// A drag across the picture, or along the bar's progress, is under way:
    /// where it would seek to.
    private var seekTarget: Double?
    private var dragFrom = 0.0

    private let centerPlay: Button
    private let bar = MigoVideoStage()
    private let barPlay: Button
    private let time = MigoVideoControls.label()
    private let total = MigoVideoControls.label()
    private let progress = Slider()
    private let fullScreen: Button
    private let line = CALayer()
    private let lineFill = CALayer()
    private let dragTime = MigoVideoControls.label()

    init(player: MigoVideoControlled) {
        self.player = player
        centerPlay = Self.button(symbol: "play.circle.fill", label: "play", pointSize: Self.centerSize)
        barPlay = Self.button(symbol: "play.fill", label: "play or pause", pointSize: 16)
        fullScreen = Self.button(symbol: "arrow.up.left.and.arrow.down.right", label: "full screen", pointSize: 14)
        super.init(frame: .zero)
        #if os(macOS)
            wantsLayer = true
            bar.wantsLayer = true
        #endif
        Self.layer(of: bar).backgroundColor = CGColor(gray: 0, alpha: 0.5)
        line.backgroundColor = CGColor(gray: 1, alpha: 0.3)
        lineFill.backgroundColor = CGColor(gray: 1, alpha: 0.9)
        line.addSublayer(lineFill)
        Self.layer(of: self).addSublayer(line)
        #if os(iOS)
            dragTime.textAlignment = .center
        #elseif os(macOS)
            dragTime.alignment = .center
        #endif
        for view in [barPlay, time, progress, total, fullScreen] as [MigoVideoStage] { bar.addSubview(view) }
        for view in [bar, centerPlay, dragTime] as [MigoVideoStage] { addSubview(view) }
        wire()
        refresh()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("MigoVideoControls is created in code") }

    func configure(_ options: [String: Any]) {
        let flag = { (key: String, fallback: Bool) in options[key] as? Bool ?? fallback }
        controls = flag("controls", true)
        showProgress = flag("showProgress", true)
        showProgressInBar = flag("showProgressInControlMode", true)
        showCenter = flag("showCenterPlayBtn", true)
        playGesture = flag("enablePlayGesture", false)
        progressGesture = flag("enableProgressGesture", true)
        #if os(iOS)
            doubleTap.isEnabled = playGesture
        #elseif os(macOS)
            doubleClick.isEnabled = playGesture
        #endif
        if !controls { setBarShown(false) }
        refresh()
    }

    /// Whether any touch on the video is the controls'.
    var takesTouches: Bool {
        controls || playGesture || progressGesture || (showCenter && player?.isPlaying == false)
    }

    /// The player changed: playing, paused, a new time, a new size.
    func refresh() {
        guard let player else { return }
        let playing = player.isPlaying
        let duration = player.duration
        let now = seekTarget ?? player.position
        let fraction = duration > 0 ? min(1, now / duration) : 0
        centerPlay.isHidden = !(showCenter && !playing && seekTarget == nil)
        bar.isHidden = !(controls && barShown)
        progress.isHidden = !showProgressInBar
        dragTime.isHidden = seekTarget == nil
        Self.setSymbol(barPlay, playing ? "pause.fill" : "play.fill", pointSize: 16)
        Self.setSymbol(
            fullScreen,
            player.isFullScreen ? "arrow.down.right.and.arrow.up.left" : "arrow.up.left.and.arrow.down.right",
            pointSize: 14)
        Self.setText(time, Self.clock(now))
        Self.setText(total, Self.clock(duration))
        Self.setText(dragTime, "\(Self.clock(now)) / \(Self.clock(duration))")
        if !draggingSlider { Self.setValue(progress, fraction) }
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        line.isHidden = !(showProgress && bar.isHidden && duration > 0)
        lineFill.frame = CGRect(x: 0, y: 0, width: line.bounds.width * fraction, height: line.bounds.height)
        CATransaction.commit()
        if playing && barShown && hideBar == nil { scheduleHide() }
    }

    func close() {
        hideBar?.cancel()
        hideBar = nil
    }

    // MARK: - the bar

    private func toggleBar() {
        guard controls else { return }
        setBarShown(!barShown)
    }

    private func setBarShown(_ shown: Bool) {
        barShown = shown
        hideBar?.cancel()
        hideBar = nil
        refresh()
    }

    private func scheduleHide() {
        let work = DispatchWorkItem { [weak self] in
            guard let self else { return }
            self.hideBar = nil
            guard self.player?.isPlaying == true, self.seekTarget == nil else { return }
            self.setBarShown(false)
        }
        hideBar = work
        DispatchQueue.main.asyncAfter(deadline: .now() + Self.barShownSeconds, execute: work)
    }

    // MARK: - layout

    private func place() {
        let size = bounds.size
        let barHeight = Self.barHeight
        centerPlay.frame = CGRect(
            x: (size.width - Self.centerSize) / 2, y: (size.height - Self.centerSize) / 2, width: Self.centerSize,
            height: Self.centerSize)
        bar.frame = CGRect(x: 0, y: size.height - barHeight, width: size.width, height: barHeight)
        let clockWidth: CGFloat = 52
        barPlay.frame = CGRect(x: 0, y: 0, width: barHeight, height: barHeight)
        fullScreen.frame = CGRect(x: size.width - barHeight, y: 0, width: barHeight, height: barHeight)
        time.frame = CGRect(x: barHeight, y: (barHeight - 16) / 2, width: clockWidth, height: 16)
        total.frame = CGRect(x: size.width - barHeight - clockWidth, y: (barHeight - 16) / 2, width: clockWidth, height: 16)
        progress.frame = CGRect(
            x: barHeight + clockWidth, y: 0, width: max(0, size.width - 2 * (barHeight + clockWidth)),
            height: barHeight)
        dragTime.frame = CGRect(x: 0, y: (size.height - 20) / 2, width: size.width, height: 20)
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        line.frame = CGRect(x: 0, y: size.height - 2, width: size.width, height: 2)
        CATransaction.commit()
        refresh()
    }

    // MARK: - input

    @objc private func pressedCenter() { player?.togglePlay() }
    @objc private func pressedBarPlay() { player?.togglePlay() }
    @objc private func pressedFullScreen() { player?.toggleFullScreen() }

    private var draggingSlider = false

    private func slid(to value: Double, done: Bool) {
        guard let player else { return }
        let duration = player.duration
        guard duration > 0 else { return }
        draggingSlider = !done
        seekTarget = done ? nil : value * duration
        if done { player.seek(to: value * duration) }
        refresh()
    }

    private func dragBegan() -> Bool {
        guard progressGesture, let player, player.duration > 0, bounds.width > 0 else { return false }
        dragFrom = player.position
        return true
    }

    private func dragged(across: CGFloat) {
        guard let player else { return }
        let duration = player.duration
        seekTarget = max(0, min(duration, dragFrom + Double(across / bounds.width) * duration))
        refresh()
    }

    private func dragEnded() {
        guard let target = seekTarget else { return }
        seekTarget = nil
        player?.seek(to: target)
        refresh()
    }

    // MARK: - the platform's views

    #if os(iOS)
        private let singleTap = UITapGestureRecognizer()
        private let doubleTap = UITapGestureRecognizer()
        private let drag = UIPanGestureRecognizer()

        private func wire() {
            centerPlay.addTarget(self, action: #selector(pressedCenter), for: .touchUpInside)
            barPlay.addTarget(self, action: #selector(pressedBarPlay), for: .touchUpInside)
            fullScreen.addTarget(self, action: #selector(pressedFullScreen), for: .touchUpInside)
            progress.minimumTrackTintColor = .white
            progress.addTarget(self, action: #selector(sliding), for: .valueChanged)
            progress.addTarget(self, action: #selector(slidDone), for: [.touchUpInside, .touchUpOutside, .touchCancel])
            singleTap.addTarget(self, action: #selector(tapped))
            doubleTap.addTarget(self, action: #selector(doubleTapped))
            doubleTap.numberOfTapsRequired = 2
            drag.addTarget(self, action: #selector(dragging(_:)))
            for recognizer in [singleTap, doubleTap, drag] as [UIGestureRecognizer] {
                recognizer.delegate = self
                addGestureRecognizer(recognizer)
            }
        }

        override func layoutSubviews() {
            super.layoutSubviews()
            place()
        }

        @objc private func tapped() { toggleBar() }
        @objc private func doubleTapped() { if playGesture { player?.togglePlay() } }

        /// UIKit asks the view, and the view is its recognizers' delegate too.
        override func gestureRecognizerShouldBegin(_ recognizer: UIGestureRecognizer) -> Bool {
            guard recognizer === drag else { return true }
            // Sideways only: a drag that is more up than across is not a seek.
            let velocity = drag.velocity(in: self)
            return abs(velocity.x) > abs(velocity.y) && dragBegan()
        }
        @objc private func sliding() { slid(to: Double(progress.value), done: false) }
        @objc private func slidDone() { slid(to: Double(progress.value), done: true) }

        @objc private func dragging(_ recognizer: UIPanGestureRecognizer) {
            switch recognizer.state {
            case .changed: dragged(across: recognizer.translation(in: self).x)
            case .ended, .cancelled, .failed: dragEnded()
            default: break
            }
        }

        private static func button(symbol: String, label: String, pointSize: CGFloat) -> Button {
            let button = UIButton(type: .custom)
            button.tintColor = .white
            button.accessibilityLabel = label
            setSymbol(button, symbol, pointSize: pointSize)
            return button
        }

        private static func setSymbol(_ button: Button, _ symbol: String, pointSize: CGFloat) {
            let configuration = UIImage.SymbolConfiguration(pointSize: pointSize)
            let image = UIImage(systemName: symbol, withConfiguration: configuration)?
                .withTintColor(.white, renderingMode: .alwaysOriginal)
            button.setImage(image, for: .normal)
        }

        private static func label() -> Label {
            let label = UILabel()
            label.textColor = .white
            label.font = .monospacedDigitSystemFont(ofSize: 12, weight: .regular)
            return label
        }

        private static func setText(_ label: Label, _ text: String) { label.text = text }
        private static func setValue(_ slider: Slider, _ value: Double) { slider.value = Float(value) }
        private static func layer(of view: MigoVideoStage) -> CALayer { view.layer }
    #elseif os(macOS)
        private let singleClick = NSClickGestureRecognizer()
        private let doubleClick = NSClickGestureRecognizer()
        private let drag = NSPanGestureRecognizer()

        private func wire() {
            centerPlay.target = self
            centerPlay.action = #selector(pressedCenter)
            barPlay.target = self
            barPlay.action = #selector(pressedBarPlay)
            fullScreen.target = self
            fullScreen.action = #selector(pressedFullScreen)
            progress.minValue = 0
            progress.maxValue = 1
            progress.isContinuous = true
            progress.target = self
            progress.action = #selector(sliding)
            singleClick.target = self
            singleClick.action = #selector(clicked)
            doubleClick.target = self
            doubleClick.action = #selector(doubleClicked)
            doubleClick.numberOfClicksRequired = 2
            drag.target = self
            drag.action = #selector(dragging(_:))
            for recognizer in [singleClick, doubleClick, drag] as [NSGestureRecognizer] {
                recognizer.delegate = self
                addGestureRecognizer(recognizer)
            }
        }

        override var isFlipped: Bool { true }

        override func layout() {
            super.layout()
            place()
        }

        @objc private func clicked() { toggleBar() }
        @objc private func doubleClicked() { if playGesture { player?.togglePlay() } }

        @objc private func sliding() {
            let done = NSApp.currentEvent?.type == .leftMouseUp
            slid(to: progress.doubleValue, done: done)
        }

        @objc private func dragging(_ recognizer: NSPanGestureRecognizer) {
            switch recognizer.state {
            case .changed: dragged(across: recognizer.translation(in: self).x)
            case .ended, .cancelled, .failed: dragEnded()
            default: break
            }
        }

        private static func button(symbol: String, label: String, pointSize: CGFloat) -> Button {
            let button = NSButton(title: "", target: nil, action: nil)
            button.isBordered = false
            button.imagePosition = .imageOnly
            button.contentTintColor = .white
            button.setAccessibilityLabel(label)
            setSymbol(button, symbol, pointSize: pointSize)
            return button
        }

        private static func setSymbol(_ button: Button, _ symbol: String, pointSize: CGFloat) {
            let image = NSImage(systemSymbolName: symbol, accessibilityDescription: nil)
            button.image = image?.withSymbolConfiguration(NSImage.SymbolConfiguration(pointSize: pointSize, weight: .regular))
        }

        private static func label() -> Label {
            let label = NSTextField(labelWithString: "")
            label.textColor = .white
            label.font = .monospacedDigitSystemFont(ofSize: 12, weight: .regular)
            return label
        }

        private static func setText(_ label: Label, _ text: String) { label.stringValue = text }
        private static func setValue(_ slider: Slider, _ value: Double) { slider.doubleValue = value }
        private static func layer(of view: MigoVideoStage) -> CALayer {
            view.wantsLayer = true
            return view.layer!
        }
    #endif

    /// `m:ss`, or `h:mm:ss` from an hour, as the Android SDK shows it.
    static func clock(_ seconds: Double) -> String {
        let whole = Int(max(0, seconds.isFinite ? seconds.rounded(.down) : 0))
        let hours = whole / 3600
        let minutes = whole / 60 % 60
        let secs = whole % 60
        return hours > 0
            ? String(format: "%d:%02d:%02d", hours, minutes, secs) : String(format: "%d:%02d", minutes, secs)
    }
}

#if os(iOS)
    extension MigoVideoControls: UIGestureRecognizerDelegate {
        /// The buttons and the progress bar take their own touches.
        func gestureRecognizer(_ recognizer: UIGestureRecognizer, shouldReceive touch: UITouch) -> Bool {
            !(touch.view is UIControl)
        }

        func gestureRecognizer(
            _ recognizer: UIGestureRecognizer, shouldRequireFailureOf other: UIGestureRecognizer
        ) -> Bool {
            // With the play gesture, a single tap waits until it is not one of two.
            recognizer === singleTap && other === doubleTap && playGesture
        }
    }
#elseif os(macOS)
    extension MigoVideoControls: NSGestureRecognizerDelegate {
        /// The buttons and the progress bar take their own clicks.
        func gestureRecognizer(_ recognizer: NSGestureRecognizer, shouldAttemptToRecognizeWith event: NSEvent) -> Bool {
            let point = convert(event.locationInWindow, from: nil)
            return !(hitTest(convert(point, to: superview)) is NSControl)
        }

        func gestureRecognizer(
            _ recognizer: NSGestureRecognizer, shouldRequireFailureOf other: NSGestureRecognizer
        ) -> Bool {
            // With the play gesture, a single click waits until it is not one of two.
            recognizer === singleClick && other === doubleClick && playGesture
        }

        func gestureRecognizerShouldBegin(_ recognizer: NSGestureRecognizer) -> Bool {
            guard recognizer === drag else { return true }
            let velocity = drag.velocity(in: self)
            return abs(velocity.x) > abs(velocity.y) && dragBegan()
        }
    }
#endif

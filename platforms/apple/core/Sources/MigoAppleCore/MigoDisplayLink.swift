import Foundation

#if canImport(QuartzCore)
    import QuartzCore
#endif
#if os(iOS)
    import UIKit
#elseif os(macOS)
    import AppKit
    import CoreVideo
#endif

/// The presenter's vsync source, on whichever API this OS has.
///
/// It lives in the engine-free package for the reason that decided where every other
/// Apple decision went: `apple-ci.yml` builds and tests this package natively on
/// macOS and compiles it for iOS on every pull request, so **both** branches below
/// meet a compiler every time. The shipping package resolves only after an
/// xcframework exists, and its macOS targets are built by no lane at all -- a
/// `#if os(macOS)` branch there would be the second arm this repository has already
/// shipped wrong twice.
///
/// Three mechanisms, two of them on macOS, because `NSView.displayLink` arrived in
/// macOS 14 and the deployment floor is macOS 11.
///
/// **What it does not decide.** The cadence comes from `MigoDisplayLinkPolicy`, which
/// takes the host's target and says what is admissible. And this is the presenter's
/// clock, not the Performance+ frame clock: G0 has to choose that one between a
/// Worker rAF, a Window rAF relay and a host relay, and that choice is a measurement
/// nobody has taken.
public final class MigoDisplayLink {

    /// One vsync. `targetTimestamp` is when the frame being drawn is due to appear,
    /// which is the one a presenter should pace against -- `timestamp` is when the
    /// previous frame appeared, and pacing against it is pacing one frame late.
    public typealias Tick = (_ targetTimestamp: CFTimeInterval, _ duration: CFTimeInterval) -> Void

    public let decision: MigoDisplayLinkPolicy.Decision
    private let onTick: Tick

    /// Whether a tick is currently being delivered, so `stop()` from inside a tick
    /// cannot tear down the object delivering it.
    private var isRunning = false

    #if os(iOS)
        private var link: CADisplayLink?
    #elseif os(macOS)
        private var link: CADisplayLink?
        private var legacyLink: CVDisplayLink?
        /// Retained for the C callback's lifetime. A `CVDisplayLink` callback receives
        /// an opaque pointer, and the object it points at has to outlive the link or
        /// the callback runs against freed memory on a real-time thread -- a crash with
        /// no Swift frame to blame.
        private var legacyContext: Unmanaged<MigoDisplayLink>?
    #endif

    public init(decision: MigoDisplayLinkPolicy.Decision, onTick: @escaping Tick) {
        self.decision = decision
        self.onTick = onTick
    }

    deinit {
        stop()
    }

    /// Start delivering ticks.
    ///
    /// `view` is used only on macOS 14 and later, where the display link belongs to
    /// the view whose display it should follow. Passing nil there falls back to the
    /// `CVDisplayLink` branch rather than guessing a display: a link driven by the
    /// wrong display presents at the wrong cadence on a two-monitor Mac, which is a
    /// stutter with no error attached.
    #if os(iOS)
        public func start() {
            guard !isRunning else { return }
            let link = CADisplayLink(target: self, selector: #selector(fire(_:)))
            apply(cadence: decision.cadence, to: link)
            link.add(to: .main, forMode: .common)
            self.link = link
            isRunning = true
        }
    #elseif os(macOS)
        public func start(view: NSView? = nil) {
            guard !isRunning else { return }
            if decision.mechanism == .caDisplayLink, #available(macOS 14.0, *), let view {
                let link = view.displayLink(target: self, selector: #selector(fire(_:)))
                apply(cadence: decision.cadence, to: link)
                link.add(to: .main, forMode: .common)
                self.link = link
                isRunning = true
                return
            }
            startLegacy()
        }
    #endif

    public func stop() {
        #if os(iOS)
            link?.invalidate()
            link = nil
        #elseif os(macOS)
            link?.invalidate()
            link = nil
            if let legacyLink {
                CVDisplayLinkStop(legacyLink)
                self.legacyLink = nil
            }
            // Released after the link is stopped, never before: the callback may be
            // running on the display's own thread at the moment stop is called.
            legacyContext?.release()
            legacyContext = nil
        #endif
        isRunning = false
    }

    // MARK: - CADisplayLink

    #if canImport(QuartzCore)
        @objc private func fire(_ link: CADisplayLink) {
            onTick(link.targetTimestamp, link.duration)
        }

        private func apply(cadence: MigoDisplayLinkPolicy.Cadence, to link: CADisplayLink) {
            switch cadence {
            case .systemDefault:
                // Deliberately nothing. Setting a range equal to the default is not
                // the same as setting none: it tells the system a rate was requested,
                // and the system then has less freedom to lower it.
                break
            case .range(let minimum, let preferred, let maximum):
                link.preferredFrameRateRange = CAFrameRateRange(
                    minimum: Float(minimum), maximum: Float(maximum), preferred: Float(preferred))
            }
        }
    #endif

    // MARK: - CVDisplayLink, for macOS 11 to 13

    #if os(macOS)
        private func startLegacy() {
            var created: CVDisplayLink?
            guard CVDisplayLinkCreateWithActiveCGDisplays(&created) == kCVReturnSuccess,
                let link = created
            else { return }

            // `passUnretained` plus an explicit retain, rather than
            // `passRetained`: the retain has to be paired with a release in `stop`,
            // and pairing it here makes both halves visible in one file.
            let context = Unmanaged.passUnretained(self)
            _ = context.retain()
            legacyContext = context
            legacyLink = link

            CVDisplayLinkSetOutputCallback(
                link,
                { _, inNow, inOutputTime, _, _, pointer in
                    guard let pointer else { return kCVReturnSuccess }
                    let link = Unmanaged<MigoDisplayLink>.fromOpaque(pointer).takeUnretainedValue()
                    // CVDisplayLink reports host time in a media timebase; the
                    // presenter wants seconds, and the interval between this frame's
                    // output time and the previous one is the duration.
                    let target = CFTimeInterval(inOutputTime.pointee.videoTime)
                        / CFTimeInterval(inOutputTime.pointee.videoTimeScale)
                    let now = CFTimeInterval(inNow.pointee.videoTime)
                        / CFTimeInterval(inNow.pointee.videoTimeScale)
                    let duration = max(0, target - now)
                    // Onto the main queue. The callback runs on a real-time thread
                    // the system will drop frames to protect, and doing renderer work
                    // there is how a display link becomes a glitch source.
                    DispatchQueue.main.async { link.onTick(target, duration) }
                    return kCVReturnSuccess
                }, context.toOpaque())

            if CVDisplayLinkStart(link) == kCVReturnSuccess {
                isRunning = true
            } else {
                stop()
            }
        }
    #endif
}

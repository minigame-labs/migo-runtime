import Foundation

#if os(iOS)
    import QuartzCore
    import UIKit
#elseif os(macOS)
    import AppKit
    import CoreVideo
    import QuartzCore
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
/// That paid for itself on the first build. `CADisplayLink` exists on macOS only from
/// macOS 14, and `contracts/apple/deployment-floor.json` puts the floor at macOS 11 --
/// so a stored `CADisplayLink?` property does not compile for this package's own
/// deployment target, and neither does `CAFrameRateRange`. The first version of this
/// file had both. On iOS the type has been there since iOS 3, which is why the two
/// platforms are written separately rather than shared and guarded.
///
/// **What it does not decide.** The cadence comes from `MigoDisplayLinkPolicy`, which
/// takes the host's target and says what is admissible. And this is the presenter's
/// clock, not the Performance+ frame clock: G0 has to choose that one between a
/// Worker rAF, a Window rAF relay and a host relay, and that choice is a measurement
/// nobody has taken.
public final class MigoDisplayLink {

    /// One vsync. `targetTimestamp` is when the frame being drawn is due to appear,
    /// which is the one a presenter should pace against -- `timestamp` is when the
    /// previous frame appeared, and pacing against that is pacing one frame late.
    public typealias Tick = (_ targetTimestamp: CFTimeInterval, _ duration: CFTimeInterval) -> Void

    public let decision: MigoDisplayLinkPolicy.Decision
    private let onTick: Tick
    public private(set) var isRunning = false

    public init(decision: MigoDisplayLinkPolicy.Decision, onTick: @escaping Tick) {
        self.decision = decision
        self.onTick = onTick
    }

    deinit {
        stop()
    }

    // MARK: - iOS

    #if os(iOS)
        private var link: CADisplayLink?

        public func start() {
            guard !isRunning else { return }
            let link = CADisplayLink(target: self, selector: #selector(fire(_:)))
            switch decision.cadence {
            case .systemDefault:
                // Deliberately nothing. A range equal to the default is not the same
                // as no range: it tells the system a rate was requested, and the
                // system then has less freedom to lower it under thermal pressure.
                break
            case .range(let minimum, let preferred, let maximum):
                link.preferredFrameRateRange = CAFrameRateRange(
                    minimum: Float(minimum), maximum: Float(maximum), preferred: Float(preferred))
            }
            // `.common` so the link keeps firing while a scroll or a modal is
            // tracking. A game that stops presenting because a system gesture began
            // is a game that looks frozen for the length of the gesture.
            link.add(to: .main, forMode: .common)
            self.link = link
            isRunning = true
        }

        public func stop() {
            link?.invalidate()
            link = nil
            isRunning = false
        }

        @objc private func fire(_ link: CADisplayLink) {
            onTick(link.targetTimestamp, link.duration)
        }
    #endif

    // MARK: - macOS

    #if os(macOS)
        /// `AnyObject` because a stored property cannot carry an availability
        /// annotation, and `CADisplayLink` does not exist at this package's macOS
        /// deployment target. The cast happens inside the one `#available` block that
        /// can name the type.
        private var modernLink: AnyObject?
        private var legacyLink: CVDisplayLink?
        /// Retained for the C callback's lifetime. A `CVDisplayLink` callback receives
        /// an opaque pointer, and the object it points at has to outlive the link, or
        /// the callback runs against freed memory on a real-time thread -- a crash with
        /// no Swift frame to blame it on.
        private var legacyContext: Unmanaged<MigoDisplayLink>?

        /// Start delivering ticks.
        ///
        /// `view` is used only on macOS 14 and later, where the display link belongs
        /// to the view whose display it should follow. Without one, this falls back to
        /// `CVDisplayLink` rather than guessing a display: a link driven by the wrong
        /// display presents at the wrong cadence on a two-monitor Mac, which is a
        /// stutter with nothing attached to it.
        public func start(view: NSView? = nil) {
            guard !isRunning else { return }
            if decision.mechanism == .caDisplayLink, let view {
                if #available(macOS 14.0, *) {
                    let link = view.displayLink(target: self, selector: #selector(fireModern(_:)))
                    switch decision.cadence {
                    case .systemDefault:
                        break
                    case .range(let minimum, let preferred, let maximum):
                        link.preferredFrameRateRange = CAFrameRateRange(
                            minimum: Float(minimum), maximum: Float(maximum),
                            preferred: Float(preferred))
                    }
                    link.add(to: .main, forMode: .common)
                    modernLink = link
                    isRunning = true
                    return
                }
            }
            startLegacy()
        }

        public func stop() {
            if #available(macOS 14.0, *) {
                (modernLink as? CADisplayLink)?.invalidate()
            }
            modernLink = nil
            if let legacyLink {
                CVDisplayLinkStop(legacyLink)
                self.legacyLink = nil
            }
            // Released after the link is stopped, never before: the callback may be
            // running on the display's own thread at the moment stop is called.
            legacyContext?.release()
            legacyContext = nil
            isRunning = false
        }

        @available(macOS 14.0, *)
        @objc private func fireModern(_ link: CADisplayLink) {
            onTick(link.targetTimestamp, link.duration)
        }

        private func startLegacy() {
            var created: CVDisplayLink?
            guard CVDisplayLinkCreateWithActiveCGDisplays(&created) == kCVReturnSuccess,
                let link = created
            else { return }

            // `passUnretained` plus an explicit retain rather than `passRetained`: the
            // retain has to be paired with a release in `stop`, and pairing it this way
            // puts both halves in one file where a reader can see they match.
            let context = Unmanaged.passUnretained(self)
            _ = context.retain()
            legacyContext = context
            legacyLink = link

            CVDisplayLinkSetOutputCallback(
                link,
                { _, inNow, inOutputTime, _, _, pointer in
                    guard let pointer else { return kCVReturnSuccess }
                    let owner = Unmanaged<MigoDisplayLink>.fromOpaque(pointer)
                        .takeUnretainedValue()
                    let outputTime = inOutputTime.pointee
                    let nowTime = inNow.pointee
                    guard outputTime.videoTimeScale != 0, nowTime.videoTimeScale != 0 else {
                        return kCVReturnSuccess
                    }
                    let target =
                        CFTimeInterval(outputTime.videoTime)
                        / CFTimeInterval(outputTime.videoTimeScale)
                    let now =
                        CFTimeInterval(nowTime.videoTime) / CFTimeInterval(nowTime.videoTimeScale)
                    let duration = max(0, target - now)
                    // Hopped to the main queue. This callback runs on a real-time
                    // thread the system drops frames to protect, and doing renderer
                    // work there is how a display link becomes the glitch source.
                    DispatchQueue.main.async { owner.onTick(target, duration) }
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

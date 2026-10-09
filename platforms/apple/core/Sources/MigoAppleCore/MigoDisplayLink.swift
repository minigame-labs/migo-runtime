import Foundation

#if os(iOS)
    import QuartzCore
    import UIKit
#elseif os(macOS)
    import AppKit
    import CoreVideo
    import QuartzCore
#endif

/// A weak stand-in between a link and its owner.
///
/// `CADisplayLink` **retains its target**, and a `CVDisplayLink` callback needs a
/// pointer that stays valid for as long as the link runs. Doing either directly
/// against the owner keeps the owner alive forever: the owner holds the link, the
/// link holds the owner, and `deinit` -- the one place that stops it -- can never
/// run. A host that forgets to call `stop()` then has a display link firing for the
/// life of the process, which on a phone is a battery complaint with no owner.
///
/// So the link holds this, this holds the owner weakly, and the owner's `deinit` is
/// reachable again.
/// Internal rather than private so a test can construct one. The lifetime property
/// that matters -- that this does not keep its owner alive -- is otherwise only
/// observable when a link really starts, and a link does not start on a machine with
/// no display: the first version of that test passed against a deliberately
/// reintroduced strong reference, on a Mac reached over ssh.
final class MigoDisplayLinkProxy: NSObject {
    weak var owner: MigoDisplayLink?

    init(owner: MigoDisplayLink) {
        self.owner = owner
        super.init()
    }

    /// Typed `Any` rather than `CADisplayLink`, because on macOS that type does not
    /// exist below macOS 14 and a selector's signature cannot carry an availability
    /// annotation.
    @objc func fire(_ sender: Any) {
        guard let owner else { return }
        #if os(iOS)
            if let link = sender as? CADisplayLink {
                owner.deliver(.init(timestamp: link.timestamp, targetTimestamp: link.targetTimestamp))
            }
        #elseif os(macOS)
            if #available(macOS 14.0, *), let link = sender as? CADisplayLink {
                owner.deliver(.init(timestamp: link.timestamp, targetTimestamp: link.targetTimestamp))
            }
        #endif
    }
}

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

    /// One vsync, both of its times on the uptime clock `CACurrentMediaTime` reads -- whichever mechanism
    /// delivered it.
    public struct Frame: Equatable, Sendable {
        /// When the vsync this frame begins at happened: the frame's start. It is what an engine stamps the
        /// frame with, as AChoreographer's frame time is on Android, so content handed it reads a time at or
        /// before the moment its callback runs.
        public let timestamp: CFTimeInterval
        /// When the frame being drawn is due to appear: what a presenter paces against. Pacing against
        /// `timestamp`, the previous frame's appearance, is pacing one frame late.
        public let targetTimestamp: CFTimeInterval

        public init(timestamp: CFTimeInterval, targetTimestamp: CFTimeInterval) {
            self.timestamp = timestamp
            self.targetTimestamp = targetTimestamp
        }

        /// The time between the two: a frame's worth, at the link's current rate.
        public var duration: CFTimeInterval { max(0, targetTimestamp - timestamp) }
    }

    public typealias Tick = (Frame) -> Void

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

    /// One delivery point, so both platforms and both macOS mechanisms hand the tick
    /// over the same way.
    ///
    /// Checked here rather than only where a tick is scheduled: `CVDisplayLinkStop`
    /// stops the real-time callback, but a callback that had already fired before
    /// that moment hops to the main queue with `DispatchQueue.main.async`, and that
    /// hop can still be sitting unscheduled when `stop()` runs. `owner` in that
    /// closure stays alive -- `stop()` tears down the link, not this object -- so
    /// without this guard the hop delivers anyway once the main queue gets to it,
    /// which is a tick after `stop()` and `isRunning == false` already said there
    /// would not be one.
    fileprivate func deliver(_ frame: Frame) {
        guard isRunning else { return }
        onTick(frame)
    }

    // MARK: - iOS

    #if os(iOS)
        private var link: CADisplayLink?
        private var proxy: MigoDisplayLinkProxy?

        public func start() {
            guard !isRunning else { return }
            let proxy = MigoDisplayLinkProxy(owner: self)
            let link = CADisplayLink(target: proxy, selector: #selector(MigoDisplayLinkProxy.fire(_:)))
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
            self.proxy = proxy
            isRunning = true
        }

        public func stop() {
            link?.invalidate()
            link = nil
            proxy = nil
            isRunning = false
        }
    #endif

    // MARK: - macOS

    #if os(macOS)
        /// `AnyObject` because a stored property cannot carry an availability
        /// annotation, and `CADisplayLink` does not exist at this package's macOS
        /// deployment target. The cast happens inside the one `#available` block that
        /// can name the type.
        private var modernLink: AnyObject?
        private var proxy: MigoDisplayLinkProxy?
        private var legacyLink: CVDisplayLink?
        /// Retained for the C callback's lifetime, and it is the **proxy** that is
        /// retained rather than the owner. A `CVDisplayLink` callback receives an
        /// opaque pointer whose target has to outlive the link, so something must be
        /// kept alive -- and keeping the owner alive is what made `deinit`
        /// unreachable, so the link ran until the process ended.
        private var legacyContext: Unmanaged<MigoDisplayLinkProxy>?

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
                    let proxy = MigoDisplayLinkProxy(owner: self)
                    let link = view.displayLink(
                        target: proxy, selector: #selector(MigoDisplayLinkProxy.fire(_:)))
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
                    self.proxy = proxy
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
            proxy = nil
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

        private func startLegacy() {
            var created: CVDisplayLink?
            guard CVDisplayLinkCreateWithActiveCGDisplays(&created) == kCVReturnSuccess,
                let link = created
            else { return }

            // The proxy is what the callback reaches, and what is retained. Paired
            // with the release in `stop`, which `deinit` also calls -- reachable now
            // that nothing retains the owner.
            let proxy = MigoDisplayLinkProxy(owner: self)
            let context = Unmanaged.passRetained(proxy)
            legacyContext = context
            legacyLink = link
            self.proxy = proxy

            CVDisplayLinkSetOutputCallback(
                link,
                { _, inNow, inOutputTime, _, _, pointer in
                    guard let pointer else { return kCVReturnSuccess }
                    let proxy = Unmanaged<MigoDisplayLinkProxy>.fromOpaque(pointer)
                        .takeUnretainedValue()
                    // The owner may already be gone: the link is stopped by `deinit`,
                    // and a callback can be in flight on the display's own thread at
                    // that moment. A weak read that comes back nil is the answer.
                    guard let owner = proxy.owner else { return kCVReturnSuccess }
                    // On the host clock, not the video clock: a video time counts from a
                    // base of the display's own, and a frame stamped with one is on no
                    // clock anything else reads. A host time is mach_absolute_time, which
                    // over the host clock's frequency is `CACurrentMediaTime` -- the clock
                    // `CADisplayLink` reports on, so both mechanisms hand over one kind of
                    // time. The output's host time is the target. The frame's start is the
                    // vsync `inNow` names, taken back from the target by the video times'
                    // difference -- a duration, which needs only their scale -- rather than
                    // `inNow`'s own host time, which is when the callback fired and moves
                    // with the scheduler instead of keeping to the display's grid.
                    let output = inOutputTime.pointee
                    let now = inNow.pointee
                    let frequency = CVGetHostClockFrequency()
                    let hostValid = CVTimeStampFlags.hostTimeValid.rawValue
                    let videoValid = CVTimeStampFlags.videoTimeValid.rawValue
                    guard output.flags & hostValid != 0, output.flags & videoValid != 0,
                        now.flags & videoValid != 0, output.videoTimeScale > 0,
                        output.videoTimeScale == now.videoTimeScale, frequency > 0
                    else {
                        return kCVReturnSuccess
                    }
                    let target = CFTimeInterval(output.hostTime) / frequency
                    let ahead =
                        CFTimeInterval(output.videoTime - now.videoTime)
                        / CFTimeInterval(output.videoTimeScale)
                    let frame = MigoDisplayLink.Frame(
                        timestamp: target - max(0, ahead), targetTimestamp: target)
                    // Hopped to the main queue. This callback runs on a real-time
                    // thread the system drops frames to protect, and doing renderer
                    // work there is how a display link becomes the glitch source.
                    DispatchQueue.main.async {
                        owner.deliver(frame)
                    }
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

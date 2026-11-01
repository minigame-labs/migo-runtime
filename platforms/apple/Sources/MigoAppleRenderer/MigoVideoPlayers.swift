import AVFoundation
import Foundation
import ImageIO

#if os(iOS)
    import UIKit
    /// The view content's coordinates are in: the game view, its bounds the
    /// game's surface in points from the top left.
    package typealias MigoVideoStage = UIView
#elseif os(macOS)
    import AppKit
    package typealias MigoVideoStage = NSView
#endif

/// `createVideo`'s players: each an `AVPlayer` shown over the game where content
/// put it -- x, y, width x height in content's coordinates, which are the game
/// view's points from its top left -- with its poster until it plays and the
/// controls content's options ask for (`MigoVideoControls`). The video service
/// of the host-service channel (`migo/host_services.h`), answered by the game
/// view that shows the game, as the Android SDK's players answer it there.
///
/// A call is answered once what it asked has happened, as the reference's
/// promises settle: a play once it plays, a seek once it is there. What a
/// player does as it plays is posted as the service's events. While the game is
/// paused -- the app in the background, its window in the Dock -- its players
/// hold still, and the ones that were playing play on when it runs again. Main
/// thread.
package final class MigoVideoPlayers {
    /// One of the service's events for the player `videoId`, its object
    /// without the id.
    package typealias Post = (_ event: UInt32, _ object: [String: Any]) -> Void

    private weak var stage: MigoVideoStage?
    private let post: Post
    private var players: [Int: MigoVideoPlayer] = [:]
    private var running = true

    package init(stage: MigoVideoStage, post: @escaping Post) {
        self.stage = stage
        self.post = post
    }

    package func handle(_ request: MigoHostServiceRequest) {
        typealias Method = MigoHostServiceID.Video
        let object = request.object
        let id = (object["videoId"] as? NSNumber)?.intValue ?? 0
        switch request.method {
        case Method.create:
            guard players[id] == nil, let stage else { return }
            let post = self.post
            let player = MigoVideoPlayer(id: id, options: object, stage: stage) { event, payload in
                var payload = payload
                payload["videoId"] = id
                post(event, payload)
            }
            players[id] = player
            if !running { player.suspend() }
        case Method.destroy:
            players.removeValue(forKey: id)?.release("video destroyed")
        case Method.set:
            players[id]?.set(object)
        default:
            guard let reply = request.reply else { return }
            guard let player = players[id] else {
                reply.fail("video not found")
                return
            }
            switch request.method {
            case Method.play: player.play(reply)
            case Method.pause: player.pause(reply)
            case Method.stop: player.stop(reply)
            case Method.seek: player.seek(to: (object["time"] as? NSNumber)?.doubleValue ?? 0, reply)
            case Method.requestFullScreen:
                player.requestFullScreen(direction: (object["direction"] as? NSNumber)?.intValue ?? 0, reply)
            case Method.exitFullScreen: player.exitFullScreen(reply)
            default: reply.fail("not supported")
            }
        }
    }

    /// The game paused or running again: its players hold still with it, and
    /// the ones that were playing play on when it runs.
    package func setRunning(_ running: Bool) {
        guard running != self.running else { return }
        self.running = running
        for player in players.values {
            if running { player.resume() } else { player.suspend() }
        }
    }

    /// The stage changed size or turned: full-screen players fill it again.
    package func stageChanged() {
        for player in players.values { player.place() }
    }

    /// The game ended: every player goes, and its calls still unanswered fail.
    package func close() {
        for player in players.values { player.release("video destroyed") }
        players.removeAll()
    }
}

/// One of `createVideo`'s players. Main thread.
final class MigoVideoPlayer: MigoVideoControlled {
    /// Seconds between position reports while it plays, as the Android SDK
    /// reports them.
    private static let timeUpdateInterval = CMTime(value: 1, timescale: 4)
    /// The largest poster read, from a file or the network.
    static let maxPosterBytes = 8 << 20

    private let id: Int
    private var options: [String: Any]
    private weak var stage: MigoVideoStage?
    private let post: MigoVideoPlayers.Post
    private let view = MigoVideoView()
    private lazy var controls = MigoVideoControls(player: self)

    private var player: AVPlayer?
    private var observations: [NSKeyValueObservation] = []
    private var timeObserver: Any?
    private var itemObservers: [NSObjectProtocol] = []
    private var posterDownload: MigoPosterDownload?
    private var posterGeneration = 0

    /// Calls waiting to be answered: plays until it plays, seeks until it is
    /// there.
    private var pendingPlays: [MigoHostServiceReply] = []
    private var pendingSeeks: [MigoHostServiceReply] = []
    /// The item can play.
    private var prepared = false
    /// What content was last told: playing since its last PLAY.
    private(set) var isPlaying = false
    /// Content, or the player's own controls, asked it to play.
    private var playWanted = false
    /// Where to go once prepared: initialTime, or a seek asked before.
    private var startAt: Double
    private var suspended = false
    private var resumeAfterSuspend = false
    private var waiting = false
    private var lastBuffered = -1
    private(set) var isFullScreen = false
    private var direction = 0
    private var released = false

    init(id: Int, options: [String: Any], stage: MigoVideoStage, post: @escaping MigoVideoPlayers.Post) {
        self.id = id
        self.options = options
        self.stage = stage
        self.post = post
        startAt = (options["initialTime"] as? NSNumber)?.doubleValue ?? 0
        playWanted = bool("autoplay", false)
        view.takesTouches = { [weak self] in self?.controls.takesTouches ?? false }
        view.addControls(controls)
        controls.configure(options)
        applyBackground()
        applyFit()
        stage.addSubview(view)
        place()
        loadPoster()
        openSource()
    }

    // MARK: - calls

    func play(_ reply: MigoHostServiceReply) {
        pendingPlays.append(reply)
        guard player != nil else {
            failPlays(string("src").isEmpty ? "no src" : "the src cannot be played")
            return
        }
        playWanted = true
        if isPlaying {
            failPlays(nil)
        } else if prepared && !suspended {
            start()
        }
    }

    func pause(_ reply: MigoHostServiceReply) {
        // A play waiting for the player never plays: it is overtaken.
        playWanted = false
        failPlays("paused before it played")
        pauseNow()
        reply.succeed()
    }

    func stop(_ reply: MigoHostServiceReply) {
        playWanted = false
        failPlays("stopped before it played")
        if isPlaying {
            player?.pause()
            isPlaying = false
        }
        resumeAfterSuspend = false
        if prepared {
            player?.seek(to: .zero, toleranceBefore: .zero, toleranceAfter: .zero)
        } else {
            startAt = 0
        }
        controls.refresh()
        reply.succeed()
    }

    func seek(to seconds: Double, _ reply: MigoHostServiceReply) {
        pendingSeeks.append(reply)
        if prepared {
            seekPlayer(to: seconds)
        } else {
            startAt = seconds
        }
    }

    func requestFullScreen(direction: Int, _ reply: MigoHostServiceReply) {
        isFullScreen = true
        self.direction = direction
        raise()
        place()
        controls.refresh()
        // Answered once laid out at its new size.
        DispatchQueue.main.async { reply.succeed() }
    }

    func exitFullScreen(_ reply: MigoHostServiceReply) {
        isFullScreen = false
        direction = 0
        place()
        controls.refresh()
        DispatchQueue.main.async { reply.succeed() }
    }

    // MARK: - what content changed

    func set(_ changed: [String: Any]) {
        guard !released else { return }
        for (key, value) in changed where key != "videoId" { options[key] = value }
        let keys = Set(changed.keys)
        if !keys.isDisjoint(with: ["x", "y", "width", "height"]) { place() }
        if keys.contains("objectFit") { applyFit() }
        if keys.contains("backgroundColor") { applyBackground() }
        if keys.contains("muted") { player?.isMuted = bool("muted", false) }
        if keys.contains("playbackRate"), isPlaying { player?.rate = rate }
        if !keys.isDisjoint(with: MigoVideoControls.optionKeys) { controls.configure(options) }
        if keys.contains("poster") { loadPoster() }
        if keys.contains("src") {
            playWanted = false
            failPending("src changed")
            startAt = (options["initialTime"] as? NSNumber)?.doubleValue ?? 0
            openSource()
        }
    }

    // MARK: - the player's own controls

    var position: Double {
        guard prepared, let seconds = player?.currentTime().seconds, seconds.isFinite else { return 0 }
        return max(0, seconds)
    }

    var duration: Double {
        guard prepared, let seconds = player?.currentItem?.duration.seconds, seconds.isFinite else { return 0 }
        return max(0, seconds)
    }

    func togglePlay() {
        guard !released, player != nil else { return }
        if isPlaying {
            playWanted = false
            pauseNow()
        } else {
            playWanted = true
            if prepared && !suspended { start() }
        }
    }

    func seek(to seconds: Double) {
        guard !released, prepared else { return }
        seekPlayer(to: seconds)
        controls.refresh()
    }

    func toggleFullScreen() {
        guard !released else { return }
        isFullScreen.toggle()
        // A landscape picture turns a quarter, as content's requestFullScreen
        // with direction 90 does.
        let size = player?.currentItem?.presentationSize ?? .zero
        direction = isFullScreen && size.width > size.height ? 90 : 0
        if isFullScreen { raise() }
        place()
        controls.refresh()
    }

    // MARK: - placing it

    /// Where content put it, or the whole of the stage's window when full
    /// screen -- turned a quarter for a direction of 90 or -90 on iOS, where
    /// the screen can be held either way; a Mac's window is not turned.
    func place() {
        guard !released, let stage else { return }
        guard isFullScreen else {
            setTransform(identity: true)
            view.frame = CGRect(
                x: number("x", 0), y: number("y", 0), width: number("width", 300), height: number("height", 150))
            return
        }
        #if os(iOS)
            let screen = stage.window.map { stage.convert($0.bounds, from: $0) } ?? stage.bounds
            let turned = direction == 90 || direction == -90
            let size = turned ? CGSize(width: screen.height, height: screen.width) : screen.size
            setTransform(identity: true)
            view.bounds = CGRect(origin: .zero, size: size)
            view.center = CGPoint(x: screen.midX, y: screen.midY)
            view.transform = CGAffineTransform(rotationAngle: CGFloat(direction) * .pi / 180)
        #elseif os(macOS)
            let content = stage.window?.contentView
            view.frame = content.map { stage.convert($0.bounds, from: $0) } ?? stage.bounds
        #endif
    }

    /// On top of whatever else the stage shows, as it starts to fill it. Once,
    /// not at every placing: moving a view among its siblings lays the stage
    /// out again, and the stage's layout is what places it.
    private func raise() {
        guard let stage else { return }
        #if os(iOS)
            stage.bringSubviewToFront(view)
        #elseif os(macOS)
            stage.addSubview(view, positioned: .above, relativeTo: nil)
        #endif
    }

    private func setTransform(identity: Bool) {
        #if os(iOS)
            if identity { view.transform = .identity }
        #endif
    }

    private func applyFit() {
        let gravity: AVLayerVideoGravity
        switch string("objectFit") {
        case "fill": gravity = .resize
        case "cover": gravity = .resizeAspectFill
        default: gravity = .resizeAspect
        }
        view.playerLayer.videoGravity = gravity
    }

    private func applyBackground() {
        view.setBackground(Self.color(string("backgroundColor")) ?? CGColor(gray: 0, alpha: 1))
    }

    // MARK: - the source

    private func openSource() {
        releasePlayer()
        let src = string("src")
        guard !src.isEmpty else { return }
        guard let url = Self.url(src) else {
            report(error: "the src is not a file or a URL")
            return
        }
        let item = AVPlayerItem(url: url)
        let player = AVPlayer(playerItem: item)
        player.actionAtItemEnd = .pause
        player.isMuted = bool("muted", false)
        self.player = player
        view.playerLayer.player = player

        observations.append(
            item.observe(\.status) { [weak self] item, _ in
                MigoVideoPlayer.onMain { self?.statusChanged(item) }
            })
        observations.append(
            item.observe(\.loadedTimeRanges) { [weak self] _, _ in
                MigoVideoPlayer.onMain { self?.reportBuffered() }
            })
        observations.append(
            player.observe(\.timeControlStatus) { [weak self] _, _ in
                MigoVideoPlayer.onMain { self?.timeControlChanged() }
            })
        timeObserver = player.addPeriodicTimeObserver(forInterval: Self.timeUpdateInterval, queue: .main) {
            [weak self] _ in
            self?.reportTime()
        }
        let center = NotificationCenter.default
        itemObservers.append(
            center.addObserver(forName: .AVPlayerItemDidPlayToEndTime, object: item, queue: .main) {
                [weak self] _ in
                self?.ended()
            })
        itemObservers.append(
            center.addObserver(forName: .AVPlayerItemFailedToPlayToEndTime, object: item, queue: .main) {
                [weak self] notification in
                let error = notification.userInfo?[AVPlayerItemFailedToPlayToEndTimeErrorKey] as? Error
                self?.report(error: error?.localizedDescription ?? "the video stopped before its end")
            })
    }

    private func releasePlayer() {
        observations.removeAll()
        for observer in itemObservers { NotificationCenter.default.removeObserver(observer) }
        itemObservers.removeAll()
        if let timeObserver, let player { player.removeTimeObserver(timeObserver) }
        timeObserver = nil
        player?.pause()
        view.playerLayer.player = nil
        player = nil
        prepared = false
        isPlaying = false
        waiting = false
        lastBuffered = -1
    }

    private func statusChanged(_ item: AVPlayerItem) {
        guard !released, item === player?.currentItem else { return }
        switch item.status {
        case .readyToPlay:
            guard !prepared else { return }
            prepared = true
            if startAt > 0 {
                seekPlayer(to: startAt)
                startAt = 0
            } else {
                answerSeeks(nil)
            }
            if playWanted && !suspended { start() }
            controls.refresh()
        case .failed:
            report(error: item.error?.localizedDescription ?? "the src cannot be played")
        default:
            break
        }
    }

    private func start() {
        guard let player else { return }
        player.rate = rate
        isPlaying = true
        view.showPoster(false)
        answerPlays(nil)
        emit(MigoHostServiceID.Video.Event.play)
        controls.refresh()
    }

    /// Pause, as content's call or the player's own control does.
    private func pauseNow() {
        if isPlaying {
            player?.pause()
            isPlaying = false
            emit(MigoHostServiceID.Video.Event.pause)
        }
        resumeAfterSuspend = false
        controls.refresh()
    }

    private func seekPlayer(to seconds: Double) {
        let time = CMTime(seconds: max(0, seconds), preferredTimescale: 600)
        // A seek a later one overtakes finishes unfinished; the later one's
        // answer is the answer to both.
        player?.seek(to: time, toleranceBefore: .zero, toleranceAfter: .zero) { [weak self] finished in
            guard finished else { return }
            MigoVideoPlayer.onMain {
                self?.answerSeeks(nil)
                self?.controls.refresh()
            }
        }
    }

    private func ended() {
        guard !released else { return }
        if bool("loop", false) {
            player?.seek(to: .zero)
            if isPlaying { player?.rate = rate }
            return
        }
        isPlaying = false
        playWanted = false
        emit(MigoHostServiceID.Video.Event.ended)
        controls.refresh()
    }

    private func timeControlChanged() {
        guard !released, let player else { return }
        let stalled = isPlaying && player.timeControlStatus == .waitingToPlayAtSpecifiedRate
        if stalled && !waiting { emit(MigoHostServiceID.Video.Event.waiting) }
        waiting = stalled
    }

    private func reportTime() {
        guard !released, isPlaying else { return }
        emit(MigoHostServiceID.Video.Event.timeUpdate, ["position": position, "duration": duration])
        controls.refresh()
    }

    private func reportBuffered() {
        guard !released, prepared, let item = player?.currentItem else { return }
        let total = duration
        guard total > 0 else { return }
        let end = item.loadedTimeRanges.map { $0.timeRangeValue.end.seconds }.filter(\.isFinite).max() ?? 0
        let buffered = Int((min(1, max(0, end / total)) * 100).rounded())
        guard buffered != lastBuffered else { return }
        lastBuffered = buffered
        emit(MigoHostServiceID.Video.Event.progress, ["buffered": buffered, "duration": total])
    }

    private func report(error reason: String) {
        failPending(reason)
        if isPlaying { player?.pause() }
        isPlaying = false
        playWanted = false
        emit(MigoHostServiceID.Video.Event.error, ["errMsg": "video:fail \(reason)"])
        controls.refresh()
    }

    // MARK: - the poster

    private func loadPoster() {
        posterGeneration += 1
        let generation = posterGeneration
        posterDownload?.cancel()
        posterDownload = nil
        view.setPoster(nil)
        let poster = string("poster")
        guard !poster.isEmpty, let url = Self.url(poster) else { return }
        let show: (CGImage?) -> Void = { [weak self] image in
            MigoVideoPlayer.onMain {
                guard let self, !self.released, generation == self.posterGeneration, let image else { return }
                self.view.setPoster(image)
                self.view.showPoster(!self.isPlaying)
            }
        }
        if url.isFileURL {
            DispatchQueue.global(qos: .userInitiated).async {
                show(Self.decode(try? Data(contentsOf: url, options: .mappedIfSafe)))
            }
        } else {
            posterDownload = MigoPosterDownload(url: url, limit: Self.maxPosterBytes) { data in
                show(Self.decode(data))
            }
        }
    }

    static func decode(_ data: Data?) -> CGImage? {
        guard let data, data.count <= maxPosterBytes,
            let source = CGImageSourceCreateWithData(data as CFData, nil)
        else { return nil }
        return CGImageSourceCreateImageAtIndex(source, 0, [kCGImageSourceShouldCacheImmediately: true] as CFDictionary)
    }

    // MARK: - lifecycle

    func suspend() {
        suspended = true
        guard isPlaying else { return }
        player?.pause()
        isPlaying = false
        resumeAfterSuspend = true
        controls.refresh()
    }

    func resume() {
        suspended = false
        guard !released, resumeAfterSuspend || playWanted else { return }
        resumeAfterSuspend = false
        if prepared && !isPlaying { start() }
    }

    func release(_ reason: String) {
        guard !released else { return }
        failPending(reason)
        released = true
        posterDownload?.cancel()
        posterDownload = nil
        releasePlayer()
        controls.close()
        view.removeFromSuperview()
    }

    // MARK: - answers and events

    private func answerPlays(_ failure: String?) {
        let plays = pendingPlays
        pendingPlays.removeAll()
        for reply in plays { if let failure { reply.fail(failure) } else { reply.succeed() } }
    }

    private func answerSeeks(_ failure: String?) {
        let seeks = pendingSeeks
        pendingSeeks.removeAll()
        for reply in seeks { if let failure { reply.fail(failure) } else { reply.succeed() } }
    }

    /// Answer every play waiting: done when `reason` is nil, failed with it
    /// otherwise.
    private func failPlays(_ reason: String?) { answerPlays(reason) }

    private func failPending(_ reason: String) {
        answerPlays(reason)
        answerSeeks(reason)
    }

    private func emit(_ event: UInt32, _ object: [String: Any] = [:]) {
        guard !released else { return }
        post(event, object)
    }

    // MARK: - options

    private var rate: Float {
        let rate = Float(number("playbackRate", 1))
        return rate > 0 ? rate : 1
    }

    private func string(_ key: String) -> String { options[key] as? String ?? "" }

    private func number(_ key: String, _ fallback: Double) -> Double {
        (options[key] as? NSNumber)?.doubleValue ?? fallback
    }

    private func bool(_ key: String, _ fallback: Bool) -> Bool { options[key] as? Bool ?? fallback }

    /// A file content may open, or an http(s) URL the engine allowed.
    static func url(_ source: String) -> URL? {
        if source.hasPrefix("/") { return URL(fileURLWithPath: source) }
        guard let url = URL(string: source), let scheme = url.scheme?.lowercased() else { return nil }
        return ["http", "https", "file"].contains(scheme) ? url : nil
    }

    /// `#RRGGBB` or `#AARRGGBB`, as the Android SDK reads `backgroundColor`.
    static func color(_ text: String) -> CGColor? {
        guard text.hasPrefix("#"), let value = UInt64(text.dropFirst(), radix: 16) else { return nil }
        let channel = { (shift: UInt64) in CGFloat((value >> shift) & 0xFF) / 255 }
        switch text.count {
        case 7: return CGColor(red: channel(16), green: channel(8), blue: channel(0), alpha: 1)
        case 9: return CGColor(red: channel(16), green: channel(8), blue: channel(0), alpha: channel(24))
        default: return nil
        }
    }

    /// AVFoundation reports on threads of its own; everything here is the main
    /// thread's.
    private static func onMain(_ work: @escaping () -> Void) {
        if Thread.isMainThread { work() } else { DispatchQueue.main.async(execute: work) }
    }
}

/// A player's view: its background, the picture, the poster over it until it
/// plays, and its controls on top. It takes a touch only where its controls
/// want one; anywhere else the touch is the game's.
final class MigoVideoView: MigoVideoStage {
    let playerLayer = AVPlayerLayer()
    private let posterLayer = CALayer()
    var takesTouches: () -> Bool = { false }

    init() {
        super.init(frame: .zero)
        #if os(macOS)
            wantsLayer = true
        #endif
        let root = rootLayer
        root.masksToBounds = true
        root.addSublayer(playerLayer)
        root.addSublayer(posterLayer)
        posterLayer.contentsGravity = .resizeAspect
        posterLayer.isHidden = true
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("MigoVideoView is created in code") }

    func addControls(_ controls: MigoVideoControls) {
        controls.frame = bounds
        #if os(iOS)
            controls.autoresizingMask = [.flexibleWidth, .flexibleHeight]
        #elseif os(macOS)
            controls.autoresizingMask = [.width, .height]
        #endif
        addSubview(controls)
    }

    func setBackground(_ color: CGColor) { rootLayer.backgroundColor = color }

    /// The view's own layer: UIKit's always has one, AppKit's once it wants
    /// one, which `init` asks for.
    private var rootLayer: CALayer {
        #if os(iOS)
            return layer
        #elseif os(macOS)
            return layer!
        #endif
    }

    func setPoster(_ image: CGImage?) {
        posterLayer.contents = image
        if image == nil { posterLayer.isHidden = true }
    }

    func showPoster(_ shown: Bool) { posterLayer.isHidden = !shown || posterLayer.contents == nil }

    private func fitLayers() {
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        playerLayer.frame = bounds
        posterLayer.frame = bounds
        CATransaction.commit()
    }

    #if os(iOS)
        override func layoutSubviews() {
            super.layoutSubviews()
            fitLayers()
        }

        override func point(inside point: CGPoint, with event: UIEvent?) -> Bool {
            takesTouches() && super.point(inside: point, with: event)
        }
    #elseif os(macOS)
        override var isFlipped: Bool { true }

        override func layout() {
            super.layout()
            fitLayers()
        }

        override func hitTest(_ point: NSPoint) -> NSView? {
            takesTouches() ? super.hitTest(point) : nil
        }
    #endif
}

/// A poster from the network, read only up to `limit` bytes: a response that
/// says it is longer, or turns out to be, is abandoned without holding more.
final class MigoPosterDownload: NSObject, URLSessionDataDelegate {
    private let limit: Int
    private let completion: (Data?) -> Void
    private var data = Data()
    private var session: URLSession?
    private var finished = false

    init(url: URL, limit: Int, completion: @escaping (Data?) -> Void) {
        self.limit = limit
        self.completion = completion
        super.init()
        let configuration = URLSessionConfiguration.ephemeral
        configuration.timeoutIntervalForRequest = 10
        // The session holds its delegate until it is invalidated, which
        // finishing does.
        let session = URLSession(configuration: configuration, delegate: self, delegateQueue: nil)
        self.session = session
        session.dataTask(with: url).resume()
    }

    func cancel() { finish(nil) }

    func urlSession(
        _ session: URLSession, dataTask: URLSessionDataTask, didReceive response: URLResponse,
        completionHandler: @escaping (URLSession.ResponseDisposition) -> Void
    ) {
        let length = response.expectedContentLength
        let status = (response as? HTTPURLResponse)?.statusCode ?? 200
        guard (200..<300).contains(status), length <= Int64(limit) else {
            completionHandler(.cancel)
            finish(nil)
            return
        }
        completionHandler(.allow)
    }

    func urlSession(_ session: URLSession, dataTask: URLSessionDataTask, didReceive chunk: Data) {
        guard data.count + chunk.count <= limit else {
            dataTask.cancel()
            finish(nil)
            return
        }
        data.append(chunk)
    }

    func urlSession(_ session: URLSession, task: URLSessionTask, didCompleteWithError error: Error?) {
        finish(error == nil ? data : nil)
    }

    private func finish(_ result: Data?) {
        objc_sync_enter(self)
        let first = !finished
        finished = true
        let session = self.session
        self.session = nil
        objc_sync_exit(self)
        guard first else { return }
        session?.invalidateAndCancel()
        completion(result)
    }
}

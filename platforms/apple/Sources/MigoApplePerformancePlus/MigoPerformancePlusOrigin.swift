import Foundation
import MigoAppleCore
import MigoAppleWebKit
import WebKit

/// The lane's origin: the content origin, plus the one path that carries frames.
///
/// A `WKWebViewConfiguration` accepts exactly one handler per scheme, and the
/// producer's large frames have to arrive on the same origin the page was loaded
/// from -- so this composes rather than replaces. Everything that is not the
/// frame endpoint is delegated to `MigoWebKitContentOrigin`, which keeps path
/// containment, MIME typing, `Range` and bounded streaming in the one place that
/// is tested for them, and keeps frame knowledge out of the lane that has no
/// frames.
///
/// ## Why there is a second uplink at all
///
/// G0's P3 measured both channels and neither wins outright: a loopback socket
/// has a low fixed cost and grows with bytes, a scheme request has a higher
/// fixed cost and is nearly flat. `MigoFrameChannelPolicy` carries the table and
/// the crossover. Commands over the socket, textures over the scheme.
public final class MigoPerformancePlusOrigin: NSObject, WKURLSchemeHandler {

    /// Where the producer POSTs a frame too large for the socket.
    ///
    /// Under the reserved engine prefix, so a content package cannot serve it
    /// and cannot reach it by shipping a file: the prefix is resolved before the
    /// content root is consulted at all.
    public static var framePath: String { MigoWebKitOriginRules.engineAssetPrefix + "frame" }

    /// Where the producer sends it, as an absolute URL on this origin.
    public static var frameURL: URL {
        MigoWebKitContentOrigin.baseURL.appendingPathComponent(String(framePath.dropFirst()))
    }

    /// The largest frame this origin will assemble from a request body.
    ///
    /// The wire format's own ceiling (`MAX_TOTAL_BYTES`, 4 MiB) plus nothing: a
    /// packet larger than the format allows is refused here rather than
    /// assembled and then refused by the decoder, because the point of a bound
    /// on the host side is to cap what an unbounded producer can make the host
    /// hold.
    public static let maximumFrameBytes = 4 * 1024 * 1024

    /// What this origin did with the frames it was POSTed.
    public struct Activity: Sendable, Equatable {
        /// Bodies read in full and handed to the channel.
        public var framesDelivered: Int = 0
        /// Requests refused: wrong method, oversize, or a body that did not read.
        public var framesRefused: Int = 0
        /// Why the last refusal happened.
        public var lastRefusal: String?
    }

    public var activity: Activity {
        lock.lock()
        defer { lock.unlock() }
        return record
    }

    /// Hand one packet to the channel. Returns whether the engine accepted it.
    ///
    /// The same closure shape as `MigoFrameChannel.Submit`, and for the same
    /// reason: what is worth testing here is the routing and the body assembly,
    /// and a test that had to stand up an engine to reach them would be testing
    /// the engine.
    public typealias Deliver = (Data) -> Bool

    private let content: MigoWebKitContentOrigin
    private let deliver: Deliver
    private let lock = NSLock()
    private var record = Activity()

    /// Body assembly happens here, never on WebKit's calling thread.
    ///
    /// Stated because the probe's handler does the opposite and says so: it
    /// answers a capability question where blocking the caller costs nothing.
    /// This is the latency path, and a megabyte read synchronously on whatever
    /// thread WebKit chose is a stall in the middle of a frame.
    private let queue = DispatchQueue(
        label: "dev.migo.performance-plus.origin", qos: .userInteractive)

    public init(content: MigoWebKitContentOrigin, deliver: @escaping Deliver) {
        self.content = content
        self.deliver = deliver
        super.init()
    }

    public func webView(_ webView: WKWebView, start task: WKURLSchemeTask) {
        let request = task.request
        guard request.url?.path == Self.framePath else {
            content.webView(webView, start: task)
            return
        }
        guard request.httpMethod == "POST" else {
            // A GET on the frame endpoint is not a file that happens to be
            // missing -- 405 says which of the two it is, and a 404 here would
            // send whoever reads it looking for a packaging fault.
            refuse(task, status: 405, reason: "the frame endpoint takes POST")
            return
        }
        queue.async { [weak self] in
            guard let self else { return }
            switch Self.readBody(from: request, limit: Self.maximumFrameBytes) {
            case .complete(let body):
                _ = self.deliver(body)
                self.note { $0.framesDelivered += 1 }
                // 204: the verdict comes back on the downlink, which is the only
                // channel that carries one. A body here would be a second place
                // the producer could learn its credit level, and two sources for
                // one absolute number is how they disagree.
                self.finish(task, status: 204, mime: "application/octet-stream", body: Data())
            case .failed(let reason):
                self.refuse(task, status: 400, reason: reason)
            }
        }
    }

    public func webView(_ webView: WKWebView, stop task: WKURLSchemeTask) {
        // Delegated unconditionally: the content origin tracks task liveness by
        // identity and a stop it never hears about leaves a task in its live set
        // forever. A stop for a frame task is not in that set and removing
        // nothing is what should happen.
        content.webView(webView, stop: task)
    }

    // MARK: - the body

    enum BodyOutcome: Equatable {
        case complete(Data)
        case failed(String)
    }

    /// The body of a scheme task, from whichever of the two places WebKit put it.
    ///
    /// WHY THE LOOP IS NOT GATED ON `hasBytesAvailable`. That property is not an
    /// EOF indicator for anything but a memory-backed stream: for a stream fed
    /// incrementally it answers false whenever the next bytes have not arrived
    /// yet, so a loop conditioned on it returns a PREFIX of the body. A frame
    /// short of its own header would be refused by the decoder as corrupt, which
    /// is the correct answer to the wrong question.
    ///
    /// The capacity is reserved up front from `Content-Length` when there is one,
    /// so a four-megabyte texture is one allocation rather than the dozen a
    /// growing `Data` would do while the frame is waiting.
    static func readBody(from request: URLRequest, limit: Int) -> BodyOutcome {
        if let inline = request.httpBody {
            guard inline.count <= limit else {
                return .failed("the frame is \(inline.count) bytes; the limit is \(limit)")
            }
            return .complete(inline)
        }
        guard let stream = request.httpBodyStream else {
            return .failed("the request carries no body")
        }
        stream.open()
        defer { stream.close() }

        var collected = Data()
        if let promised = request.value(forHTTPHeaderField: "Content-Length").flatMap(Int.init),
            promised > 0, promised <= limit
        {
            collected.reserveCapacity(promised)
        }
        var buffer = [UInt8](repeating: 0, count: 64 * 1024)
        while true {
            let read = buffer.withUnsafeMutableBufferPointer { pointer -> Int in
                guard let base = pointer.baseAddress else { return -1 }
                return stream.read(base, maxLength: pointer.count)
            }
            if read == 0 { break }
            if read < 0 {
                let reason = stream.streamError?.localizedDescription ?? "unknown stream error"
                return .failed("the body stream failed after \(collected.count) byte(s): \(reason)")
            }
            collected.append(contentsOf: buffer[0..<read])
            if collected.count > limit {
                return .failed("the frame exceeds \(limit) bytes, which is the wire format's own ceiling")
            }
        }
        return .complete(collected)
    }

    // MARK: - answering

    private func note(_ change: (inout Activity) -> Void) {
        lock.lock()
        change(&record)
        lock.unlock()
    }

    private func refuse(_ task: WKURLSchemeTask, status: Int, reason: String) {
        note {
            $0.framesRefused += 1
            $0.lastRefusal = "\(status): \(reason)"
        }
        finish(task, status: status, mime: "text/plain; charset=utf-8", body: Data(reason.utf8))
    }

    private func finish(_ task: WKURLSchemeTask, status: Int, mime: String, body: Data) {
        // On the main queue, and the liveness question is WebKit's own: calling
        // into a task WebKit has reclaimed traps rather than throwing, and the
        // window between a check on another queue and the call is exactly the
        // crash the check was for. `didReceive` on a stopped task is the failure
        // mode; the content origin solves it with a live set it owns, and this
        // handler is on the same main queue when it answers.
        DispatchQueue.main.async {
            guard
                let response = HTTPURLResponse(
                    url: task.request.url ?? MigoWebKitContentOrigin.baseURL, statusCode: status,
                    httpVersion: "HTTP/1.1",
                    headerFields: [
                        "Content-Type": mime,
                        "Content-Length": String(body.count),
                        // The producer's own origin, and only it. A frame
                        // endpoint reachable from another origin is a native
                        // surface any page could post to.
                        "Access-Control-Allow-Origin": MigoWebKitContentOrigin.baseURL
                            .absoluteString,
                    ])
            else { return }
            task.didReceive(response)
            if !body.isEmpty { task.didReceive(body) }
            task.didFinish()
        }
    }
}

import Foundation
import WebKit

/// The custom-scheme arm of gate 1.
///
/// A23 and A5 are what this settles, and both were recorded as settled before
/// anybody had asked a device:
///
///   * A23 said a custom scheme needs private API to be a secure context. The
///     Secure Contexts specification allows a user agent to designate schemes as
///     potentially trustworthy, and WebKit is reported to treat a registered
///     scheme-handler scheme that way. Reported is not measured, and the answer
///     may differ per iOS build, which is exactly what the capability gate is
///     for.
///   * A5 said a POST body is lost on the way to the handler, citing WebKit
///     191362. That bug is RESOLVED FIXED. Carrying a fixed bug forward as a
///     design constraint is how a candidate gets eliminated without a
///     measurement, so the handler echoes the body it receives and the page
///     compares bytes.
///
/// The handler answers exactly two paths, and anything else is a 404 rather
/// than a plausible default: a probe that got a 200 with an empty body from a
/// path nobody implemented would record an available capability.
public final class MigoProbeSchemeHandler: NSObject, WKURLSchemeHandler {

    /// The scheme the probe registers. Not `migo:` -- a scheme this short risks
    /// colliding with something the system already knows, and a collision would
    /// show up as an unexplained failure to load rather than as a conflict.
    public static let scheme = "migo-probe"
    public static let origin = "migo-probe://probe"

    private let resources: [String: (mime: String, body: Data)]
    private let lock = NSLock()
    private var active: Set<ObjectIdentifier> = []

    public init(resources: [String: (mime: String, body: Data)]) {
        self.resources = resources
    }

    public func webView(_ webView: WKWebView, start task: WKURLSchemeTask) {
        lock.lock()
        active.insert(ObjectIdentifier(task))
        lock.unlock()

        guard let url = task.request.url else {
            finish(task, status: 400, mime: "text/plain", body: Data())
            return
        }
        let path = url.path.isEmpty ? "/" : url.path

        if task.request.httpMethod == "POST", path == "/echo-body" {
            // WKURLSchemeTask delivers a body either inline or as a stream, and
            // which one depends on size and on the caller. Reading only
            // `httpBody` is how "the body was lost" gets reported for a body
            // that arrived: the stream form is the common one for a fetch with
            // a typed array.
            switch Self.readBody(from: task.request) {
            case .complete(let body):
                finish(task, status: 200, mime: "application/octet-stream", body: body)
            case .failed(let reason):
                // 500 with the reason, not a short body with a 200. The page compares
                // bytes, so a truncated echo would be recorded as a lost body -- which
                // is the finding A5 already got wrong once.
                finish(
                    task, status: 500, mime: "text/plain", body: Data(reason.utf8))
            }
            return
        }

        let name = path == "/" ? "capability-probe.html" : String(path.dropFirst())
        guard let resource = resources[name] else {
            finish(task, status: 404, mime: "text/plain", body: Data("not found\n".utf8))
            return
        }
        finish(task, status: 200, mime: resource.mime, body: resource.body)
    }

    public func webView(_ webView: WKWebView, stop task: WKURLSchemeTask) {
        lock.lock()
        active.remove(ObjectIdentifier(task))
        lock.unlock()
    }

    /// What came out of the body stream.
    ///
    /// Two outcomes and not one `Data`, because a short read and a complete small
    /// body are the same value. This handler echoes what it read and the page
    /// compares bytes, so a truncation is reported by the page as "the other side
    /// received N of M bytes" -- which is A5's own failure mode, restated. A5 is
    /// the assumption that a POST body is lost on the way to the handler, citing a
    /// WebKit bug that is RESOLVED FIXED; a truncated read here would resurrect a
    /// fixed bug as a measurement and eliminate the custom-scheme transport.
    enum BodyOutcome: Equatable {
        case complete(Data)
        case failed(String)
    }

    /// The largest body this handler will assemble. Same bound and same reason as
    /// the loopback listener's: four times the largest payload class the matrix
    /// measures, so a measurement never meets it and a runaway claim always does.
    static let maximumBodyBytes = 16 * 1024 * 1024

    /// The body of a scheme task, from whichever of the two places WebKit put it.
    ///
    /// WHY THE LOOP IS NOT GATED ON `hasBytesAvailable`. That property is not an EOF
    /// indicator for anything but a memory-backed stream: for a stream fed
    /// incrementally it answers false whenever the next bytes have not arrived yet,
    /// so a loop conditioned on it stops early and returns a *prefix* of the body.
    /// With the eight-byte payload the probe sends today that can never happen, which
    /// is exactly why it would have gone unnoticed until the transport matrix posted
    /// four megabytes. `read` returning 0 is the end of the stream and -1 is an
    /// error, and those are the two conditions that end the loop.
    ///
    /// It reads synchronously on WebKit's calling thread, which is acceptable *here*
    /// and stated so it is not copied: this answers a capability question, not a
    /// latency one. A transport arm that times a round trip must not reuse it.
    static func readBody(from request: URLRequest) -> BodyOutcome {
        if let inline = request.httpBody {
            return .complete(inline)
        }
        guard let stream = request.httpBodyStream else {
            return .complete(Data())
        }
        stream.open()
        defer { stream.close() }
        var collected = Data()
        var buffer = [UInt8](repeating: 0, count: 64 * 1024)
        while true {
            let read = stream.read(&buffer, maxLength: buffer.count)
            if read == 0 { break }
            if read < 0 {
                let reason = stream.streamError?.localizedDescription ?? "unknown stream error"
                return .failed(
                    "the body stream failed after \(collected.count) byte(s): \(reason)")
            }
            collected.append(contentsOf: buffer[0..<read])
            if collected.count > maximumBodyBytes {
                return .failed(
                    "the body exceeded \(maximumBodyBytes) bytes, which is four times the largest "
                        + "payload class the matrix measures")
            }
        }
        return .complete(collected)
    }

    private func finish(_ task: WKURLSchemeTask, status: Int, mime: String, body: Data) {
        // A task the web view already stopped raises an Objective-C exception
        // when it is written to, which is an app kill and not an error a probe
        // can report. The set is what keeps a completing probe from taking the
        // whole run with it when the page navigates away mid-request.
        lock.lock()
        let stillActive = active.contains(ObjectIdentifier(task))
        active.remove(ObjectIdentifier(task))
        lock.unlock()
        guard stillActive else { return }

        var headers = [
            "Content-Type": mime,
            "Content-Length": String(body.count),
            "Cache-Control": "no-store",
            // Same reason as the loopback listener: A7's question cannot be
            // asked of this origin unless the headers are actually sent.
            "Cross-Origin-Opener-Policy": "same-origin",
            "Cross-Origin-Embedder-Policy": "require-corp",
            "Cross-Origin-Resource-Policy": "same-origin",
        ]
        // The page fetches this origin from the loopback origin too, to ask A5
        // without reloading. Without this the fetch fails as CORS and the answer
        // would be recorded against the body-delivery question it is not about.
        headers["Access-Control-Allow-Origin"] = "*"

        guard
            let url = task.request.url,
            let response = HTTPURLResponse(
                url: url, statusCode: status, httpVersion: "HTTP/1.1", headerFields: headers)
        else {
            task.didFailWithError(
                NSError(
                    domain: "migo.probe", code: 1,
                    userInfo: [NSLocalizedDescriptionKey: "the response could not be constructed"]))
            return
        }
        task.didReceive(response)
        task.didReceive(body)
        task.didFinish()
    }
}

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
            let body = Self.readBody(from: task.request)
            finish(task, status: 200, mime: "application/octet-stream", body: body)
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

    /// The body of a scheme task, from whichever of the two places WebKit put it.
    static func readBody(from request: URLRequest) -> Data {
        if let inline = request.httpBody {
            return inline
        }
        guard let stream = request.httpBodyStream else {
            return Data()
        }
        stream.open()
        defer { stream.close() }
        var collected = Data()
        var buffer = [UInt8](repeating: 0, count: 16 * 1024)
        while stream.hasBytesAvailable {
            let read = stream.read(&buffer, maxLength: buffer.count)
            if read <= 0 { break }
            collected.append(contentsOf: buffer[0..<read])
        }
        return collected
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

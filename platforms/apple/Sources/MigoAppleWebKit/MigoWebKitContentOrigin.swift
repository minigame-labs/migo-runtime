import Foundation
import MigoAppleCore
import WebKit

/// The origin the WebKit Full lane loads content from.
///
/// `contracts/apple/webkit-full-surface.json` calls this `content_origin` with
/// provenance `host_origin`: host code answers every request content makes to it,
/// which makes it a native surface reached by loading a URL rather than by calling a
/// method. It replaced a `content.read` bridge method, because content loaded from an
/// origin the host serves can already fetch every file in its package, and a second
/// path to the same bytes is one more native method to declare under 4.7.2.
///
/// A custom scheme rather than `loadFileURL(_:allowingReadAccessTo:)`, for a reason
/// the capability probe measured rather than assumed: a page on a custom scheme is a
/// secure context, so content gets the APIs the platform gates on that. What it does
/// *not* get is cross-origin isolation, which is why `SharedArrayBuffer` is a
/// Performance+ question and not one this lane answers.
///
/// The decisions -- path containment, MIME typing, `Range` parsing -- are in
/// `MigoWebKitOriginRules` in the engine-free package, where a pull request runs them
/// against a test. This file is the delivery, and the two things it has to get right
/// are the ones that only exist here:
///
///   * **Bounded memory.** One chunk is in flight at a time, and the next read is
///     scheduled by the delivery of the previous one. Reading a file in a loop and
///     posting every chunk to the main queue would hold the whole file in pending
///     blocks -- streaming in shape and resident in fact, at the moment the page is
///     already at its widest, on the platform that kills for exactly that.
///   * **Stopped tasks.** WebKit reclaims a task when the page navigates or the
///     element goes away, and calling into a stopped task traps rather than
///     erroring. The liveness check and the call happen in the same main-queue step,
///     because a check on one queue and a call on another is the crash it was meant
///     to prevent.
public final class MigoWebKitContentOrigin: NSObject, WKURLSchemeHandler {

    public static var scheme: String { MigoWebKitOriginRules.scheme }
    public static var host: String { MigoWebKitOriginRules.host }

    /// The origin content is loaded from.
    public static var baseURL: URL {
        // The components are constants in the engine-free package and a test asserts
        // this parses; a nil here would be a scheme this project itself wrote.
        guard let url = URL(string: MigoWebKitOriginRules.baseURLString) else {
            preconditionFailure(
                "\(MigoWebKitOriginRules.baseURLString) is not a URL, so the lane has no origin")
        }
        return url
    }

    /// Where the content package is, symlinks resolved once so containment does not
    /// depend on the filesystem's shape at request time.
    private let root: URL
    private let indexPath: String

    /// Reads happen here, never on the main thread: a page load that stalls the main
    /// queue on a file read stalls the frame the content is trying to present.
    private let queue = DispatchQueue(label: "com.migo.webkit.content-origin", qos: .userInitiated)

    /// Tasks WebKit has not stopped, by identity -- `WKURLSchemeTask` is not
    /// `Hashable` and identity is exactly the question being asked.
    private var live = Set<ObjectIdentifier>()
    private let liveLock = NSLock()

    /// How much is read and handed over at a time.
    ///
    /// 256 KiB has a failure mode on each side: smaller costs a main-queue hop per
    /// chunk on a file that may be tens of megabytes, larger holds more resident for
    /// no gain, because WebKit consumes chunks as they arrive.
    public static let chunkSize = 256 * 1024

    public init(root: URL, indexPath: String = "index.html") {
        self.root = root.resolvingSymlinksInPath().standardizedFileURL
        self.indexPath = indexPath
        super.init()
    }

    // MARK: - WKURLSchemeHandler

    public func webView(_ webView: WKWebView, start task: WKURLSchemeTask) {
        liveLock.lock()
        live.insert(ObjectIdentifier(task))
        liveLock.unlock()

        let request = task.request
        guard let url = request.url else {
            refuse(task, status: 400, reason: "the request carries no URL")
            return
        }

        let resolution = MigoWebKitOriginRules.resolve(
            requestPath: url.path, host: url.host, root: root, indexPath: indexPath)

        switch resolution {
        case .wrongHost(let named):
            refuse(
                task, status: 400,
                reason: "this origin serves \(Self.host) and the request names \(named ?? "nothing")")
        case .outsidePackage:
            refuse(task, status: 403, reason: "the path resolves outside the content package")
        case .file(let path):
            queue.async { [weak self] in
                self?.begin(path: path, url: url, request: request, task: task)
            }
        }
    }

    public func webView(_ webView: WKWebView, stop task: WKURLSchemeTask) {
        retire(task)
    }

    // MARK: - liveness

    private func isLive(_ task: WKURLSchemeTask) -> Bool {
        liveLock.lock()
        defer { liveLock.unlock() }
        return live.contains(ObjectIdentifier(task))
    }

    private func retire(_ task: WKURLSchemeTask) {
        liveLock.lock()
        live.remove(ObjectIdentifier(task))
        liveLock.unlock()
    }

    /// Run `body` against the task on the main queue, if it is still live.
    ///
    /// The check and the call are one step here on purpose: WebKit stops tasks from
    /// the main queue, so a check performed anywhere else can be true and stale by
    /// the time the call lands.
    private func onTask(_ task: WKURLSchemeTask, _ body: @escaping (WKURLSchemeTask) -> Void) {
        DispatchQueue.main.async { [weak self] in
            guard let self, self.isLive(task) else { return }
            body(task)
        }
    }

    // MARK: - responses

    private func refuse(_ task: WKURLSchemeTask, status: Int, reason: String) {
        onTask(task) { [weak self] task in
            let body = Data(reason.utf8)
            if let response = HTTPURLResponse(
                url: task.request.url ?? Self.baseURL, statusCode: status, httpVersion: "HTTP/1.1",
                headerFields: [
                    "Content-Type": "text/plain; charset=utf-8",
                    "Content-Length": String(body.count),
                ])
            {
                task.didReceive(response)
                task.didReceive(body)
                task.didFinish()
            } else {
                task.didFailWithError(
                    NSError(
                        domain: "com.migo.webkit.content-origin", code: status,
                        userInfo: [NSLocalizedDescriptionKey: reason]))
            }
            self?.retire(task)
        }
    }

    private func begin(path: String, url: URL, request: URLRequest, task: WKURLSchemeTask) {
        let file = URL(fileURLWithPath: path)
        let attributes = try? FileManager.default.attributesOfItem(atPath: path)
        guard (attributes?[.type] as? FileAttributeType) == .typeRegular,
            let size = (attributes?[.size] as? NSNumber)?.intValue
        else {
            refuse(task, status: 404, reason: "no file at \(url.path)")
            return
        }

        switch MigoWebKitOriginRules.byteRange(
            fromHeader: request.value(forHTTPHeaderField: "Range"), size: size)
        {
        case .unsatisfiable:
            onTask(task) { [weak self] task in
                if let response = HTTPURLResponse(
                    url: url, statusCode: 416, httpVersion: "HTTP/1.1",
                    headerFields: ["Content-Range": "bytes */\(size)"])
                {
                    task.didReceive(response)
                    task.didFinish()
                } else {
                    task.didFailWithError(
                        NSError(
                            domain: "com.migo.webkit.content-origin", code: 416,
                            userInfo: [NSLocalizedDescriptionKey: "range past the end of the file"]))
                }
                self?.retire(task)
            }
        case .whole:
            send(file: file, url: url, start: 0, length: size, total: size, partial: false, task: task)
        case .partial(let start, let length):
            send(
                file: file, url: url, start: start, length: length, total: size, partial: true,
                task: task)
        }
    }

    private func send(
        file: URL, url: URL, start: Int, length: Int, total: Int, partial: Bool,
        task: WKURLSchemeTask
    ) {
        guard let handle = try? FileHandle(forReadingFrom: file) else {
            refuse(task, status: 404, reason: "unreadable file at \(url.path)")
            return
        }
        if start > 0 {
            do {
                try handle.seek(toOffset: UInt64(start))
            } catch {
                try? handle.close()
                onTask(task) { [weak self] task in
                    task.didFailWithError(error)
                    self?.retire(task)
                }
                return
            }
        }

        let headers = MigoWebKitOriginRules.responseHeaders(
            pathExtension: file.pathExtension, start: start, length: length, totalSize: total,
            partial: partial)
        guard
            let response = HTTPURLResponse(
                url: url, statusCode: partial ? 206 : 200, httpVersion: "HTTP/1.1",
                headerFields: headers)
        else {
            try? handle.close()
            refuse(task, status: 500, reason: "the response could not be formed")
            return
        }

        onTask(task) { task in task.didReceive(response) }
        pump(handle: handle, remaining: length, task: task)
    }

    /// One chunk in flight. The delivery of a chunk schedules the read of the next,
    /// so peak residency is a chunk and not a file however large the asset is.
    private func pump(handle: FileHandle, remaining: Int, task: WKURLSchemeTask) {
        guard remaining > 0 else {
            try? handle.close()
            onTask(task) { [weak self] task in
                task.didFinish()
                self?.retire(task)
            }
            return
        }
        guard isLive(task) else {
            try? handle.close()
            return
        }

        let chunk = handle.readData(ofLength: min(Self.chunkSize, remaining))
        guard !chunk.isEmpty else {
            // The file is shorter than its own reported size, which means it changed
            // under us. Finishing here delivers a truncated body under a
            // Content-Length that promised more, and WebKit reports that as a
            // network error rather than as a silently short asset.
            try? handle.close()
            onTask(task) { [weak self] task in
                task.didFailWithError(
                    NSError(
                        domain: "com.migo.webkit.content-origin", code: 500,
                        userInfo: [
                            NSLocalizedDescriptionKey:
                                "the file ended \(remaining) bytes before its length said it would"
                        ]))
                self?.retire(task)
            }
            return
        }

        DispatchQueue.main.async { [weak self] in
            guard let self, self.isLive(task) else {
                try? handle.close()
                return
            }
            task.didReceive(chunk)
            self.queue.async { [weak self] in
                self?.pump(handle: handle, remaining: remaining - chunk.count, task: task)
            }
        }
    }
}

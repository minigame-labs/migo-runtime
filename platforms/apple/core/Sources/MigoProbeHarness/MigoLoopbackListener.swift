import CryptoKit
import Foundation
import Network

/// The loopback origin gate 1 measures against.
///
/// A6 is the assumption this exists to settle: `127.0.0.1` is a potentially
/// trustworthy origin by specification, `localhost` as a *name* has been
/// reported not to resolve inside WKWebView, and a loopback listener may or may
/// not trip the local-network permission alert on a given iOS version. None of
/// that can be answered without a listener actually running on the device.
///
/// It serves three things and no more:
///
///   * the probe page and its two scripts, with the COOP/COEP headers that
///     `crossOriginIsolated` requires -- served rather than assumed, because
///     A7's question is whether WebKit honours them here, and it cannot be
///     asked without sending them;
///   * `POST /echo-body`, which the synchronous-XHR probe blocks on; and
///   * `GET /echo` upgraded to a WebSocket that echoes binary frames, which is
///     A22's arm.
///
/// WHY IT IS WRITTEN OUT RATHER THAN TAKEN FROM A PACKAGE. This package is the
/// one that must resolve with nothing fetched and nothing built -- that
/// property is a gate, and it is what keeps any Swift in this repository being
/// compiled at all. A WebSocket server is a handshake and a frame header; the
/// dependency would cost more than the code.
public final class MigoLoopbackListener {

    public enum StartError: Error, CustomStringConvertible {
        case listenerFailed(String)
        case noPortAssigned

        public var description: String {
            switch self {
            case .listenerFailed(let reason): return "the loopback listener failed: \(reason)"
            case .noPortAssigned: return "the loopback listener came up without a port"
            }
        }
    }

    private let queue = DispatchQueue(label: "migo.probe.loopback")
    private var listener: NWListener?
    private var connections: [ObjectIdentifier: NWConnection] = [:]
    private let resources: [String: (mime: String, body: Data)]

    /// The origin the page is loaded from once the listener is up, for example
    /// `http://127.0.0.1:52341`. Literal address on purpose: A6 records that the
    /// `localhost` name has been observed not to resolve in WKWebView, and a
    /// probe that silently fell back to a name would answer a different
    /// question than the one asked.
    public private(set) var origin: String?

    public init(resources: [String: (mime: String, body: Data)]) {
        self.resources = resources
    }

    public func start() throws {
        let parameters = NWParameters.tcp
        parameters.requiredInterfaceType = .loopback
        // Deliberate: the design's whole latency argument is about small writes
        // at frame cadence, and A31 says the client half of that cannot be seen
        // from here. What can be controlled is this half, and leaving Nagle on
        // would put an unknown into the one measurement gate 2 exists to make.
        if let tcp = parameters.defaultProtocolStack.internetProtocol as? NWProtocolTCP.Options {
            tcp.noDelay = true
        }
        parameters.allowLocalEndpointReuse = true

        let listener: NWListener
        do {
            listener = try NWListener(using: parameters, on: .any)
        } catch {
            throw StartError.listenerFailed(String(describing: error))
        }
        self.listener = listener

        let ready = DispatchSemaphore(value: 0)
        var failure: String?
        listener.stateUpdateHandler = { state in
            switch state {
            case .ready: ready.signal()
            case .failed(let error):
                failure = String(describing: error)
                ready.signal()
            case .cancelled:
                ready.signal()
            default:
                break
            }
        }
        listener.newConnectionHandler = { [weak self] connection in
            self?.accept(connection)
        }
        listener.start(queue: queue)

        if ready.wait(timeout: .now() + 5) == .timedOut {
            throw StartError.listenerFailed("it did not become ready within 5 s")
        }
        if let failure {
            throw StartError.listenerFailed(failure)
        }
        guard let port = listener.port?.rawValue, port != 0 else {
            throw StartError.noPortAssigned
        }
        origin = "http://127.0.0.1:\(port)"
    }

    public func stop() {
        queue.sync {
            for connection in connections.values {
                connection.cancel()
            }
            connections.removeAll()
        }
        listener?.cancel()
        listener = nil
        origin = nil
    }

    // MARK: - connections

    private func accept(_ connection: NWConnection) {
        connections[ObjectIdentifier(connection)] = connection
        connection.stateUpdateHandler = { [weak self, weak connection] state in
            guard let connection else { return }
            if case .cancelled = state {
                self?.connections.removeValue(forKey: ObjectIdentifier(connection))
            }
            if case .failed = state {
                self?.connections.removeValue(forKey: ObjectIdentifier(connection))
            }
        }
        connection.start(queue: queue)
        receiveRequest(on: connection, accumulated: Data())
    }

    /// Read until the headers are complete, then read exactly `Content-Length`
    /// more. A server that assumed one read per request would answer the
    /// synchronous-XHR probe with a truncated body and report it as a transport
    /// failure -- the probe would be measuring this file.
    ///
    /// It parses what it already holds BEFORE reading again. The bytes after a
    /// completed request stay in the buffer, and one TCP segment can carry two
    /// requests -- the page fetches its two scripts back to back. Going
    /// straight to `receive` with a whole request already buffered waits for
    /// bytes the client has no reason to send.
    private func receiveRequest(on connection: NWConnection, accumulated: Data) {
        if handleBufferedRequest(on: connection, buffer: accumulated) {
            return
        }
        connection.receive(minimumIncompleteLength: 1, maximumLength: 64 * 1024) {
            [weak self] chunk, _, isComplete, error in
            guard let self else { return }
            if error != nil || (isComplete && chunk == nil) {
                connection.cancel()
                return
            }
            var buffer = accumulated
            if let chunk { buffer.append(chunk) }

            if self.handleBufferedRequest(on: connection, buffer: buffer) {
                return
            }
            if buffer.count > 64 * 1024 {
                self.respond(on: connection, status: "431 Request Header Fields Too Large")
                return
            }
            self.receiveRequest(on: connection, accumulated: buffer)
        }
    }

    /// Routes one whole request out of `buffer`, or reports that there is not
    /// one yet. Returns true when it took responsibility for the connection.
    private func handleBufferedRequest(on connection: NWConnection, buffer: Data) -> Bool {
        guard let headerEnd = buffer.range(of: Data("\r\n\r\n".utf8)) else {
            return false
        }

        let head = String(decoding: buffer[..<headerEnd.lowerBound], as: UTF8.self)
        let lines = head.split(separator: "\r\n", omittingEmptySubsequences: false)
        guard let requestLine = lines.first else {
            respond(on: connection, status: "400 Bad Request")
            return true
        }
        let parts = requestLine.split(separator: " ")
        guard parts.count >= 2 else {
            respond(on: connection, status: "400 Bad Request")
            return true
        }
        let method = String(parts[0])
        let path = String(parts[1])

        var headers: [String: String] = [:]
        for line in lines.dropFirst() where line.contains(":") {
            let pair = line.split(separator: ":", maxSplits: 1)
            if pair.count == 2 {
                headers[pair[0].lowercased().trimmingCharacters(in: .whitespaces)] =
                    pair[1].trimmingCharacters(in: .whitespaces)
            }
        }

        if headers["upgrade"]?.lowercased() == "websocket" {
            completeWebSocketHandshake(on: connection, path: path, headers: headers)
            return true
        }

        let declared = Int(headers["content-length"] ?? "0") ?? 0
        let bodyStart = headerEnd.upperBound
        guard buffer.count - bodyStart >= declared else {
            return false
        }
        let body = Data(buffer[bodyStart..<(bodyStart + declared)])
        let leftover = Data(buffer[(bodyStart + declared)...])
        route(on: connection, method: method, path: path, body: body, leftover: leftover)
        return true
    }

    private func route(
        on connection: NWConnection, method: String, path: String, body: Data, leftover: Data
    ) {
        let route = path.split(separator: "?").first.map(String.init) ?? path

        if method == "POST", route == "/echo-body" {
            // The probe compares what comes back with what it sent, so this
            // echoes the bytes exactly and never a summary of them.
            respond(
                on: connection, status: "200 OK", mime: "application/octet-stream", body: body,
                leftover: leftover)
            return
        }

        let name = route == "/" ? "capability-probe.html" : String(route.dropFirst())
        guard let resource = resources[name] else {
            respond(on: connection, status: "404 Not Found", leftover: leftover)
            return
        }
        respond(
            on: connection, status: "200 OK", mime: resource.mime, body: resource.body,
            leftover: leftover)
    }

    private func respond(
        on connection: NWConnection,
        status: String,
        mime: String = "text/plain; charset=utf-8",
        body: Data = Data(),
        leftover: Data = Data()
    ) {
        var head = "HTTP/1.1 \(status)\r\n"
        head += "Content-Type: \(mime)\r\n"
        head += "Content-Length: \(body.count)\r\n"
        // A7 asks whether WebKit grants cross-origin isolation here. It cannot
        // be asked without these two, and a listener that omitted them would
        // return `crossOriginIsolated === false` for a reason that has nothing
        // to do with the platform.
        head += "Cross-Origin-Opener-Policy: same-origin\r\n"
        head += "Cross-Origin-Embedder-Policy: require-corp\r\n"
        head += "Cross-Origin-Resource-Policy: same-origin\r\n"
        head += "Cache-Control: no-store\r\n"
        // A page served from the custom scheme opens its Worker's socket and
        // its synchronous request against this listener, which is a different
        // origin. Without this, that arm records a CORS refusal in the field
        // that is supposed to be about the transport.
        head += "Access-Control-Allow-Origin: *\r\n"
        head += "Connection: keep-alive\r\n\r\n"

        var packet = Data(head.utf8)
        packet.append(body)
        connection.send(
            content: packet,
            completion: .contentProcessed { [weak self] _ in
                // Keep-alive: the page fetches three resources and then POSTs,
                // and a server that closed after each would make the
                // synchronous probe measure connection setup.
                self?.receiveRequest(on: connection, accumulated: leftover)
            })
    }

    // MARK: - WebSocket

    private func completeWebSocketHandshake(
        on connection: NWConnection, path: String, headers: [String: String]
    ) {
        guard path.hasPrefix("/echo"), let key = headers["sec-websocket-key"] else {
            respond(on: connection, status: "400 Bad Request")
            return
        }
        let magic = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"
        let digest = Insecure.SHA1.hash(data: Data((key + magic).utf8))
        let accept = Data(digest).base64EncodedString()

        var head = "HTTP/1.1 101 Switching Protocols\r\n"
        head += "Upgrade: websocket\r\n"
        head += "Connection: Upgrade\r\n"
        head += "Sec-WebSocket-Accept: \(accept)\r\n\r\n"
        connection.send(
            content: Data(head.utf8),
            completion: .contentProcessed { [weak self] _ in
                self?.receiveFrame(on: connection, accumulated: Data())
            })
    }

    /// Read client frames and echo their payloads back unmasked.
    ///
    /// Only what the probe sends has to be understood: a single binary frame,
    /// masked as every client frame must be, and a close. Continuation frames
    /// and fragmentation are not produced by the probe, so they are refused
    /// explicitly rather than mishandled quietly -- a server that echoed a
    /// fragment as a whole message would answer A22 with a corrupted round trip
    /// and blame the platform.
    private func receiveFrame(on connection: NWConnection, accumulated: Data) {
        connection.receive(minimumIncompleteLength: 1, maximumLength: 64 * 1024) {
            [weak self] chunk, _, isComplete, error in
            guard let self else { return }
            if error != nil || (isComplete && chunk == nil) {
                connection.cancel()
                return
            }
            var buffer = accumulated
            if let chunk { buffer.append(chunk) }

            while true {
                guard let frame = Self.parseFrame(buffer) else { break }
                buffer.removeFirst(frame.consumed)

                switch frame.opcode {
                case 0x8:  // close
                    connection.cancel()
                    return
                case 0x9:  // ping
                    connection.send(
                        content: Self.encodeFrame(opcode: 0xA, payload: frame.payload),
                        completion: .idempotent)
                case 0x1, 0x2:
                    connection.send(
                        content: Self.encodeFrame(opcode: frame.opcode, payload: frame.payload),
                        completion: .idempotent)
                case 0x0:
                    // A fragment. The probe never sends one; echoing it as a
                    // whole message would corrupt the round trip silently.
                    connection.cancel()
                    return
                default:
                    connection.cancel()
                    return
                }
            }
            self.receiveFrame(on: connection, accumulated: buffer)
        }
    }

    struct Frame {
        let opcode: UInt8
        let payload: Data
        let consumed: Int
    }

    /// Returns nil when the buffer does not yet hold a whole frame.
    static func parseFrame(_ buffer: Data) -> Frame? {
        let bytes = [UInt8](buffer)
        guard bytes.count >= 2 else { return nil }
        let opcode = bytes[0] & 0x0F
        let masked = (bytes[1] & 0x80) != 0
        var length = Int(bytes[1] & 0x7F)
        var cursor = 2

        if length == 126 {
            guard bytes.count >= cursor + 2 else { return nil }
            length = Int(bytes[cursor]) << 8 | Int(bytes[cursor + 1])
            cursor += 2
        } else if length == 127 {
            guard bytes.count >= cursor + 8 else { return nil }
            var wide = 0
            for index in 0..<8 {
                wide = wide << 8 | Int(bytes[cursor + index])
            }
            length = wide
            cursor += 8
        }

        var mask = [UInt8]()
        if masked {
            guard bytes.count >= cursor + 4 else { return nil }
            mask = Array(bytes[cursor..<(cursor + 4)])
            cursor += 4
        }
        guard bytes.count >= cursor + length else { return nil }

        var payload = Array(bytes[cursor..<(cursor + length)])
        if masked {
            for index in 0..<payload.count {
                payload[index] ^= mask[index % 4]
            }
        }
        return Frame(opcode: opcode, payload: Data(payload), consumed: cursor + length)
    }

    /// Server frames are never masked, and this only ever writes whole messages.
    static func encodeFrame(opcode: UInt8, payload: Data) -> Data {
        var frame = Data([0x80 | opcode])
        let count = payload.count
        if count < 126 {
            frame.append(UInt8(count))
        } else if count <= 0xFFFF {
            frame.append(126)
            frame.append(UInt8((count >> 8) & 0xFF))
            frame.append(UInt8(count & 0xFF))
        } else {
            frame.append(127)
            for shift in stride(from: 56, through: 0, by: -8) {
                frame.append(UInt8((count >> shift) & 0xFF))
            }
        }
        frame.append(payload)
        return frame
    }
}

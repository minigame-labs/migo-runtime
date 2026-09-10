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

    /// A request's headers may not exceed this. Only the headers: a body is
    /// bounded by `maximumRequestBytes`, and conflating the two is what made a
    /// 64 KiB POST look like a transport that could not carry one.
    static let maximumHeaderBytes = 64 * 1024

    /// A whole request may not exceed this.
    ///
    /// Sized from what the measurement carries rather than from a round number:
    /// `contracts/apple/transport-probe.schema.json` requires payload classes up
    /// to 1 MiB, and the performance matrix runs to 4 MiB. Eight leaves room for
    /// both plus headers, and still refuses a client that has lost its mind.
    static let maximumRequestBytes = 8 * 1024 * 1024

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
            // The header cap applies to HEADERS, which is what it says and what
            // it did not do.
            //
            // It used to bound the whole accumulated buffer at 64 KiB and answer
            // `431 Request Header Fields Too Large` -- so a POST whose BODY was
            // 64 KiB or more was refused, with a status naming a cause that was
            // not the cause. P3's synchronous-XHR arm measured 200 clean round
            // trips at 4 KiB, died partway through 64 KiB depending on how the
            // chunks landed, and could not complete one at a mebibyte. That
            // would have been recorded as "the blocking transport cannot carry a
            // frame" -- an architectural verdict produced by this listener's own
            // cap.
            //
            // So: before the headers are complete, a buffer this large really is
            // a header problem. Once they are, the body has a bound of its own
            // and a status of its own, because a client that sent too much needs
            // to be told which too much it sent.
            let headersComplete = buffer.range(of: Data("\r\n\r\n".utf8)) != nil
            if !headersComplete, buffer.count > Self.maximumHeaderBytes {
                self.respond(on: connection, status: "431 Request Header Fields Too Large")
                return
            }
            if headersComplete, buffer.count > Self.maximumRequestBytes {
                self.respond(on: connection, status: "413 Payload Too Large")
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
    /// WHAT CHANGED HERE AND WHY IT MATTERED. This loop used to accept a single
    /// unfragmented frame and cancel on anything else, with a comment saying that
    /// fragments were "refused explicitly rather than mishandled quietly". Three
    /// things were wrong with that, and all three would have surfaced as a platform
    /// verdict rather than as a bug in this file:
    ///
    ///   1. **It did not look at FIN.** A fragmented message's *first* frame carries
    ///      opcode 0x1 or 0x2 with FIN clear, so it was echoed as a whole message
    ///      carrying only the first fragment -- precisely the corrupted round trip
    ///      the comment claimed to prevent. Only continuation frames were refused.
    ///   2. **Refusing fragments is the wrong answer anyway.** The performance matrix
    ///      runs payloads up to 4 MiB, and whether WebKit's WebSocket fragments a
    ///      send that large is not something this project has measured. If it does,
    ///      a refusal cancels the connection and the record says the loopback
    ///      transport failed at 4 MiB -- an architectural conclusion drawn from this
    ///      function's limitation. So fragments are assembled.
    ///   3. **A malformed frame and an incomplete one were the same answer.** Both
    ///      returned nil, so a broken client waited forever instead of being told.
    ///      A lab tool that hangs reports nothing.
    ///
    /// The assembly state travels as parameters rather than living on the listener:
    /// two connections must not share a half-received message, and the compiler
    /// enforces that when there is nothing shared to get wrong.
    private func receiveFrame(
        on connection: NWConnection, accumulated: Data, pendingOpcode: UInt8? = nil,
        pending: Data = Data()
    ) {
        connection.receive(minimumIncompleteLength: 1, maximumLength: 64 * 1024) {
            [weak self] chunk, _, isComplete, error in
            guard let self else { return }
            if error != nil || (isComplete && chunk == nil) {
                connection.cancel()
                return
            }
            var buffer = accumulated
            if let chunk { buffer.append(chunk) }

            var offset = 0
            var pendingOpcode = pendingOpcode
            var pending = pending

            loop: while true {
                switch Self.parse(buffer, at: offset) {
                case .needMoreBytes:
                    break loop
                case .protocolError(let reason):
                    // Told, not dropped. The close code is 1002 (protocol error) so
                    // the page's `onclose` carries the reason into the record instead
                    // of an opaque 1006.
                    connection.send(
                        content: Self.encodeClose(code: 1002, reason: reason),
                        completion: .contentProcessed { _ in connection.cancel() })
                    return
                case .frame(let frame):
                    offset += frame.consumed

                    switch frame.opcode {
                    case 0x8:
                        // Echo the close before cancelling. Cancelling outright makes
                        // a clean shutdown arrive at the page as 1006 (abnormal),
                        // which a probe records as a transport failure.
                        connection.send(
                            content: Self.encodeFrame(opcode: 0x8, payload: frame.payload),
                            completion: .contentProcessed { _ in connection.cancel() })
                        return
                    case 0x9:
                        connection.send(
                            content: Self.encodeFrame(opcode: 0xA, payload: frame.payload),
                            completion: .idempotent)
                    case 0xA:
                        // A pong. Nothing to answer, and cancelling on it would kill a
                        // connection whose client was checking liveness.
                        break
                    case 0x1, 0x2:
                        guard pendingOpcode == nil else {
                            connection.send(
                                content: Self.encodeClose(
                                    code: 1002,
                                    reason: "a new message began while one was still fragmented"),
                                completion: .contentProcessed { _ in connection.cancel() })
                            return
                        }
                        if frame.fin {
                            connection.send(
                                content: Self.encodeFrame(
                                    opcode: frame.opcode, payload: frame.payload),
                                completion: .idempotent)
                        } else {
                            pendingOpcode = frame.opcode
                            pending = frame.payload
                        }
                    case 0x0:
                        guard let opcode = pendingOpcode else {
                            connection.send(
                                content: Self.encodeClose(
                                    code: 1002, reason: "a continuation frame began a message"),
                                completion: .contentProcessed { _ in connection.cancel() })
                            return
                        }
                        guard pending.count + frame.payload.count <= Self.maximumMessageBytes else {
                            connection.send(
                                content: Self.encodeClose(
                                    code: 1009,
                                    reason: "a fragmented message exceeded "
                                        + "\(Self.maximumMessageBytes) bytes"),
                                completion: .contentProcessed { _ in connection.cancel() })
                            return
                        }
                        pending.append(frame.payload)
                        if frame.fin {
                            connection.send(
                                content: Self.encodeFrame(opcode: opcode, payload: pending),
                                completion: .idempotent)
                            pendingOpcode = nil
                            pending = Data()
                        }
                    default:
                        connection.send(
                            content: Self.encodeClose(
                                code: 1002, reason: "opcode \(frame.opcode) is not a frame type"),
                            completion: .contentProcessed { _ in connection.cancel() })
                        return
                    }
                }
            }

            // Compacted once per receive rather than per frame. `removeFirst` is
            // linear, and a 4 MiB message arriving in 64 KiB chunks would pay that
            // cost sixty-four times over -- the harness's own copying attributed to
            // the transport it is measuring.
            if offset > 0 { buffer.removeFirst(offset) }
            self.receiveFrame(
                on: connection, accumulated: buffer, pendingOpcode: pendingOpcode,
                pending: pending)
        }
    }

    /// The largest message this listener will assemble.
    ///
    /// The performance matrix's largest payload class is 4 MiB; this is four times
    /// that, so a measurement never meets the bound and a runaway length claim
    /// always does. Without it a client claiming a 64-bit length would have this
    /// process buffer until the system killed it, which on a bench looks exactly
    /// like the platform refusing to carry the payload.
    public static let maximumMessageBytes = 16 * 1024 * 1024

    struct Frame {
        let opcode: UInt8
        let fin: Bool
        let payload: Data
        let consumed: Int
    }

    enum ParseOutcome {
        case frame(Frame)
        /// The buffer does not hold a whole frame yet.
        case needMoreBytes
        /// It never will: this is not a frame. Distinguished from `needMoreBytes`
        /// because waiting for more bytes after a malformed header is a hang.
        case protocolError(String)
    }

    /// Returns nil when the buffer does not yet hold a whole frame.
    ///
    /// Kept because the tests written against it describe real wire cases; it
    /// answers nil for both "incomplete" and "invalid", which is why the loop above
    /// uses `parse` instead.
    static func parseFrame(_ buffer: Data) -> Frame? {
        if case .frame(let frame) = parse(buffer, at: 0) { return frame }
        return nil
    }

    /// Parse one frame starting `offset` bytes into `buffer`.
    ///
    /// Indexed rather than copied. The previous version began with
    /// `[UInt8](buffer)`, which copies the whole accumulated buffer on every parse
    /// attempt -- so a 4 MiB message arriving in 64 KiB chunks copied up to 4 MiB
    /// sixty-four times before the frame was complete. That is quadratic in the
    /// payload size, in the one function whose cost is subtracted from nothing when
    /// the transport is timed.
    static func parse(_ buffer: Data, at offset: Int) -> ParseOutcome {
        let base = buffer.startIndex + offset
        let available = buffer.count - offset
        guard available >= 2 else { return .needMoreBytes }

        let first = buffer[base]
        let second = buffer[base + 1]
        let fin = (first & 0x80) != 0
        // RFC 6455 reserves these; a peer setting one without a negotiated extension
        // is speaking a protocol this listener does not implement, and guessing which
        // one would corrupt the payload rather than fail.
        guard (first & 0x70) == 0 else {
            return .protocolError("a reserved frame bit was set")
        }
        let opcode = first & 0x0F
        let masked = (second & 0x80) != 0
        var length = Int(second & 0x7F)
        var cursor = 2

        let isControl = (opcode & 0x08) != 0
        if isControl {
            // Control frames carry at most 125 bytes and are never fragmented.
            // Accepting a fragmented one would leave the assembler holding a message
            // that can never complete.
            guard length <= 125 else {
                return .protocolError("a control frame claimed \(length) bytes")
            }
            guard fin else { return .protocolError("a control frame was fragmented") }
        }

        if length == 126 {
            guard available >= cursor + 2 else { return .needMoreBytes }
            length = Int(buffer[base + cursor]) << 8 | Int(buffer[base + cursor + 1])
            cursor += 2
        } else if length == 127 {
            guard available >= cursor + 8 else { return .needMoreBytes }
            var wide: UInt64 = 0
            for index in 0..<8 {
                wide = wide << 8 | UInt64(buffer[base + cursor + index])
            }
            // The high bit must be clear per RFC 6455, and the value must fit both
            // Int and the bound below. Reading it into an Int first is what the
            // previous version did, and a value above Int.max landed as a negative
            // length that passed the "enough bytes?" check and then crashed slicing
            // a range whose lower bound exceeded its upper.
            guard wide <= UInt64(Self.maximumMessageBytes) else {
                return .protocolError("a frame claimed \(wide) bytes")
            }
            length = Int(wide)
            cursor += 8
        }
        guard length <= Self.maximumMessageBytes else {
            return .protocolError("a frame claimed \(length) bytes")
        }

        var mask = [UInt8](repeating: 0, count: 4)
        if masked {
            guard available >= cursor + 4 else { return .needMoreBytes }
            for index in 0..<4 { mask[index] = buffer[base + cursor + index] }
            cursor += 4
        }
        guard available >= cursor + length else { return .needMoreBytes }

        var payload = Data(buffer[(base + cursor)..<(base + cursor + length)])
        if masked {
            // Eight bytes per XOR, with the mask word loaded the same way the
            // payload is.
            //
            // Every client-to-server frame is masked, so this runs over every
            // byte the page ever sends, and P3 sends a mebibyte at a time two
            // hundred times at four payload classes. The first version read
            // `mask[index % 4]`: a division and a bounds-checked subscript per
            // byte, in an app built Debug because that is what a probe is built
            // as. This is one XOR per eight bytes.
            //
            // The mask word is built with `loadUnaligned` from the mask's own
            // four bytes rather than by shifting them into place, so it is
            // assembled in the same byte order the payload is read in and the
            // XOR lines up on either endianness. Shifting would have been
            // correct on every Apple CPU and correct for the wrong reason.
            //
            // The stride is a multiple of the mask's four-byte period, so the
            // word stays in phase for the whole run and only the tail -- at
            // most seven bytes -- needs a position within the period.
            //
            // `loadUnaligned` and unaligned `storeBytes` are SE-0349, emitted
            // into the client, and compile and run at this package's macOS 11
            // floor: checked rather than assumed, because the first version of
            // this comment asserted the opposite and used it to justify a
            // slower loop.
            let maskWord = mask.withUnsafeBytes { $0.loadUnaligned(as: UInt32.self) }
            let maskPair = UInt64(maskWord) | (UInt64(maskWord) << 32)
            payload.withUnsafeMutableBytes { raw in
                var index = 0
                while index + 8 <= length {
                    let word = raw.loadUnaligned(fromByteOffset: index, as: UInt64.self)
                    raw.storeBytes(of: word ^ maskPair, toByteOffset: index, as: UInt64.self)
                    index += 8
                }
                guard let bytes = raw.bindMemory(to: UInt8.self).baseAddress else { return }
                while index < length {
                    bytes[index] ^= mask[index & 3]
                    index += 1
                }
            }
        }
        return .frame(
            Frame(opcode: opcode, fin: fin, payload: payload, consumed: cursor + length))
    }

    /// A close frame carrying a status code and a reason.
    ///
    /// The reason is what reaches the page's `onclose`, and it is the difference
    /// between a record that says "the transport failed" and one that says which
    /// frame this listener could not read.
    static func encodeClose(code: UInt16, reason: String) -> Data {
        var payload = Data([UInt8(code >> 8), UInt8(code & 0xFF)])
        // 125 total, two of which are the code. Truncating on a scalar boundary
        // rather than a byte, because a half-written UTF-8 sequence makes the whole
        // reason unreadable instead of shorter.
        var text = ""
        for scalar in reason.unicodeScalars {
            if text.utf8.count + String(scalar).utf8.count > 123 { break }
            text.unicodeScalars.append(scalar)
        }
        payload.append(Data(text.utf8))
        return encodeFrame(opcode: 0x8, payload: payload)
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

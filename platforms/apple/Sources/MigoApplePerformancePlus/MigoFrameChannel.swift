import Foundation
import MigoAppleCore
import MigoAppleWebKit
import MigoEngine

/// The producer's frames in, the host's answers out.
///
/// ## The socket is the engine's
///
/// The producer -- content JavaScript in WebKit's WebContent process -- sends
/// frames, control and service messages over a loopback WebSocket and hears its
/// verdicts, frame-clock ticks and answers on it. The engine terminates that
/// socket itself (`migo_session_start_frame_endpoint`): a message is read and
/// submitted on the engine's own thread, and a tick is written the moment the
/// frame clock queues it. This channel used to terminate it with
/// Network.framework and pump the downlink from a GCD queue, and on devices
/// that was about a quarter of the App process's CPU samples while a game ran,
/// with three queue hops between the frame clock and the socket.
///
/// ## What is still here
///
/// The other uplink. Frames and service messages too large for the socket, a
/// Worker's synchronous calls, parked answers and content modules arrive at the
/// host's content origin -- a `WKURLSchemeHandler`, which only a host can
/// install -- and this hands them to the engine. Their answers leave on the
/// engine's socket, the only channel that carries a verdict.
///
/// ## Ownership
///
/// The channel does not own the session and does not extend its life. Stop it
/// before `migo_session_destroy`: that closes the producer's socket before the
/// page goes, and keeps the statistics readable afterwards, from a snapshot
/// taken at `stop` rather than from a session that no longer exists.
public final class MigoFrameChannel {

    /// What the session's transports have done, for a host that has to explain
    /// a frame rate or a stall. Frames and service messages are counted where
    /// the engine admits them, so both uplinks are counted alike.
    public struct Statistics: Equatable, Sendable {
        /// Packets the producer sent, over either uplink.
        public var framesReceived: Int = 0
        /// Packets the engine accepted.
        public var framesAccepted: Int = 0
        /// Packets that arrived ahead of their predecessor and were held until
        /// it came. Not refused and not yet accepted: the uplink is two
        /// independent streams, and ingress executes them in order.
        public var framesDeferred: Int = 0
        /// Packets the engine refused, for any of its four reasons. Not an
        /// error here: `WOULD_BLOCK` is the credit window doing its job.
        public var framesRefused: Int = 0
        /// Downlink messages -- verdicts and ticks -- sent to the producer.
        public var messagesSent: Int = 0
        /// Downlink records the engine dropped because nothing drained them in
        /// time. Every record is absolute, so the producer recovers on the next.
        public var recordsDropped: Int = 0
        /// Messages drained while no producer was connected. Between a
        /// WebContent termination and the rebuilt producer's connection this is
        /// the expected state, not a fault.
        public var sendsWithoutProducer: Int = 0
        /// Synchronous calls answered. Every answer counts, including one that
        /// tells the producer its call failed: that is the engine doing its job.
        public var syncCallsAnswered: Int = 0
        /// Synchronous calls this channel could not produce an answer for at
        /// all -- the session was gone, or the engine refused the arguments.
        /// Non-zero means a producer was blocked on a response that said
        /// nothing, which it reports as a transport failure.
        public var syncCallsUnanswered: Int = 0
        /// Control messages the producer sent: its requests for a frame.
        public var controlMessagesReceived: Int = 0
        /// Control messages the engine refused. Non-zero is a producer writing a
        /// format the engine does not read, waiting for a tick that will not come.
        public var controlMessagesRefused: Int = 0
        /// The code of the most recent refusal, from 3001 up; 0 when there has
        /// been none.
        public var lastControlRefusalCode: UInt32 = 0
        /// Drains the engine asked for: ticks, urgent verdicts, answers.
        public var downlinkWakes: Int = 0
        public var serviceMessagesReceived: Int = 0
        public var serviceMessagesRefused: Int = 0
        public var lastServiceRefusalCode: UInt32 = 0
        public var serviceMessagesSent: Int = 0
        public var parkedRepliesTaken: Int = 0
        /// Producers that completed the WebSocket handshake.
        public var producersConnected: Int = 0
    }

    /// What the producer needs to connect.
    public struct Endpoint: Equatable, Sendable {
        public let port: UInt16
        /// The literal address, not a hostname: `localhost` does not resolve
        /// inside `WKWebView`, which is a measured fact rather than a style
        /// choice, and `127.0.0.0/8` is potentially trustworthy so the origin
        /// keeps the powers a secure context has.
        public var url: URL { URL(string: "ws://127.0.0.1:\(port)/")! }
    }

    /// Why the channel would not start.
    public enum StartFailure: Error, Equatable {
        /// The engine would not start its endpoint: `MIGO_ERROR_INVALID_STATE`
        /// when no surface is attached yet or the channel was already started,
        /// `MIGO_ERROR_INTERNAL` when no loopback port could be bound.
        case endpointRefused(MigoResult)
    }

    /// What the engine did with one packet, as far as a host needs to know.
    public enum Disposition: Sendable, Equatable {
        case accepted
        /// Held for the packet before it; see `Statistics.framesDeferred`.
        case deferred
        /// Refused, for any of the engine's reasons -- including `WOULD_BLOCK`,
        /// which is the credit window doing its job.
        case refused
    }

    /// What the engine did with one service message.
    public enum ServiceDisposition: Sendable, Equatable {
        /// Admitted, held until the message before it arrives, or ignored as
        /// another generation's -- the three the producer needs no answer for.
        case admitted
        /// Refused, with the engine's code. The engine has told the producer on
        /// the return stream; the stream is broken from here.
        case refused(code: UInt32)
        /// The session has no engine, or has ended.
        case unavailable
    }

    /// One synchronous call's answer: the header, then the reply when there is
    /// one. Sent in that order they are one response body.
    ///
    /// Two parts because the reply is the engine's own readback buffer, wrapped
    /// rather than copied, and joining it to a header would copy it.
    public struct SyncAnswer {
        public let header: Data
        public let reply: Data?
    }

    /// Hand one packet to the engine and report what became of it.
    typealias Submit = (Data) -> Disposition
    typealias SubmitService = (Data) -> ServiceDisposition
    /// A parked answer by generation and request id, handed over without a copy.
    typealias TakeParkedReply = (UInt32, UInt32) -> Data?
    /// A content module's source as the engine evaluates it; see
    /// `MigoWebKitContentOrigin.ModuleSource`.
    typealias ReadContentModule = (String) -> MigoWebKitContentOrigin.ModuleLookup
    /// Answer one synchronous call body, or `nil` when no answer could be
    /// produced at all. Blocks.
    typealias AnswerSync = (Data) -> SyncAnswer?

    private let session: OpaquePointer
    private let submit: Submit
    private let answerSync: AnswerSync
    private let submitService: SubmitService
    private let takeParked: TakeParkedReply
    private let readContentModule: ReadContentModule
    private let lock = NSLock()
    private var syncCallsAnswered = 0
    private var syncCallsUnanswered = 0
    private var parkedRepliesTaken = 0
    /// The engine's counters as they stood at `stop`. Read from here after it,
    /// because the host may destroy the session next and a handle read after
    /// that names freed memory.
    private var finalEngineStatistics: MigoFrameTransportStatistics?

    /// A channel for a live session with a surface attached.
    public convenience init(session: OpaquePointer) {
        self.init(session: session, submit: MigoFrameChannel.engineSubmit(session))
    }

    /// The content origin's half, replaceable for the tests that assert what
    /// WebKit delivers to it; the socket is always the session's endpoint.
    init(
        session: OpaquePointer,
        submit: @escaping Submit,
        answerSync: AnswerSync? = nil,
        submitService: SubmitService? = nil,
        takeParked: TakeParkedReply? = nil,
        readContentModule: ReadContentModule? = nil
    ) {
        self.session = session
        self.submit = submit
        self.answerSync = answerSync ?? MigoFrameChannel.engineAnswerSync(session)
        self.submitService = submitService ?? MigoFrameChannel.engineSubmitService(session)
        self.takeParked = takeParked ?? MigoFrameChannel.engineTakeParked(session)
        self.readContentModule = readContentModule ?? MigoFrameChannel.engineReadContentModule(session)
    }

    /// Start the engine's endpoint and return what the producer needs to
    /// connect. Once per channel.
    public func start() throws -> Endpoint {
        var port: UInt16 = 0
        let result = migo_session_start_frame_endpoint(session, &port)
        guard result == MIGO_OK else { throw StartFailure.endpointRefused(result) }
        return Endpoint(port: port)
    }

    /// Close the producer's socket and stop accepting. Idempotent; returns
    /// without waiting for the engine's threads, and nothing is sent to the
    /// producer after it returns.
    public func stop() {
        lock.lock()
        let stopped = finalEngineStatistics != nil
        lock.unlock()
        guard !stopped else { return }
        _ = migo_session_stop_frame_endpoint(session)
        var final = queryEngineStatistics() ?? MigoFrameTransportStatistics()
        final.producer_connected = 0
        lock.lock()
        finalEngineStatistics = final
        lock.unlock()
    }

    /// A snapshot of what the session's transports and this channel have done.
    public var currentStatistics: Statistics {
        var statistics = Statistics()
        if let engine = engineStatistics() {
            statistics.framesReceived = Int(engine.frames_received)
            statistics.framesAccepted = Int(engine.frames_accepted)
            statistics.framesDeferred = Int(engine.frames_deferred)
            statistics.framesRefused = Int(engine.frames_refused)
            statistics.messagesSent = Int(engine.downlink_messages_sent)
            statistics.recordsDropped = Int(engine.downlink_records_dropped)
            statistics.sendsWithoutProducer = Int(engine.sends_without_producer)
            statistics.controlMessagesReceived = Int(engine.control_messages_received)
            statistics.controlMessagesRefused = Int(engine.control_messages_refused)
            statistics.lastControlRefusalCode = engine.last_control_refusal_code
            statistics.downlinkWakes = Int(engine.downlink_wakes)
            statistics.serviceMessagesReceived = Int(engine.service_messages_received)
            statistics.serviceMessagesRefused = Int(engine.service_messages_refused)
            statistics.lastServiceRefusalCode = engine.last_service_refusal_code
            statistics.serviceMessagesSent = Int(engine.service_messages_sent)
            statistics.producersConnected = Int(engine.producers_connected)
        }
        lock.lock()
        statistics.syncCallsAnswered = syncCallsAnswered
        statistics.syncCallsUnanswered = syncCallsUnanswered
        statistics.parkedRepliesTaken = parkedRepliesTaken
        lock.unlock()
        return statistics
    }

    /// Whether a producer is connected to the engine's endpoint.
    public var isConnected: Bool { engineStatistics()?.producer_connected == 1 }

    /// One frame that arrived on the other uplink.
    ///
    /// Above `MigoFrameChannelPolicy.socketCeilingBytes` the producer POSTs to
    /// the content origin instead of sending on the socket, because G0's P3
    /// measured the scheme 4.4x faster and 4.6x cheaper at 1 MiB. The two
    /// uplinks meet in the engine: same ingress, same counters, and the verdict
    /// goes back on the socket -- the only channel that carries one.
    @discardableResult
    public func submitFromOrigin(_ packet: Data) -> Disposition {
        submit(packet)
    }

    /// A service message that arrived as a POST to the content origin: the ones
    /// too large for the socket. The engine restores the order the two paths
    /// lost, and answers a refusal on the socket itself.
    @discardableResult
    public func submitServiceFromOrigin(_ message: Data) -> ServiceDisposition {
        submitService(message)
    }

    /// A content module's source, as the engine evaluates it. Reads the file on
    /// the calling thread.
    public func contentModule(path: String) -> MigoWebKitContentOrigin.ModuleLookup {
        readContentModule(path)
    }

    /// A parked answer the producer asked the content origin for. Taken once.
    public func takeParkedReply(generation: UInt32, requestId: UInt32) -> Data? {
        let reply = takeParked(generation, requestId)
        if reply != nil {
            lock.lock()
            parkedRepliesTaken += 1
            lock.unlock()
        }
        return reply
    }

    /// One synchronous call, answered.
    ///
    /// The producer is a Worker blocked in a synchronous request to the content
    /// origin, because that origin has no SharedArrayBuffer to block on. The
    /// body goes to the engine unchanged and the answer comes back unchanged;
    /// the engine waits for the frame the call names before it reads, and
    /// writes the answer under the lock that settled it, so nothing here orders
    /// or matches anything. Blocks for as long as the call takes.
    public func answerSyncCall(_ call: Data) -> SyncAnswer? {
        let answer = answerSync(call)
        lock.lock()
        if answer == nil {
            syncCallsUnanswered += 1
        } else {
            syncCallsAnswered += 1
        }
        lock.unlock()
        return answer
    }

    // MARK: - The engine's side

    private func engineStatistics() -> MigoFrameTransportStatistics? {
        lock.lock()
        let final = finalEngineStatistics
        lock.unlock()
        return final ?? queryEngineStatistics()
    }

    private func queryEngineStatistics() -> MigoFrameTransportStatistics? {
        var statistics = MigoFrameTransportStatistics()
        statistics.struct_size = UInt32(MemoryLayout<MigoFrameTransportStatistics>.size)
        statistics.abi_version = MIGO_ABI_VERSION_CURRENT
        guard migo_session_get_frame_transport_statistics(session, &statistics) == MIGO_OK else {
            return nil
        }
        return statistics
    }

    static func engineSubmit(_ session: OpaquePointer) -> Submit {
        { packet in
            var outcome = MigoFrameIngressOutcome()
            outcome.struct_size = UInt32(MemoryLayout<MigoFrameIngressOutcome>.size)
            outcome.abi_version = MIGO_ABI_VERSION_CURRENT
            let result = packet.withUnsafeBytes { bytes -> MigoResult in
                migo_session_submit_external_frame(
                    session, bytes.bindMemory(to: UInt8.self).baseAddress, packet.count, &outcome)
            }
            guard result == MIGO_OK else { return .refused }
            switch outcome.decision {
            case MigoFrameIngressDecision(MIGO_FRAME_INGRESS_ACCEPTED): return .accepted
            case MigoFrameIngressDecision(MIGO_FRAME_INGRESS_DEFERRED): return .deferred
            default: return .refused
            }
        }
    }

    static func engineAnswerSync(_ session: OpaquePointer) -> AnswerSync {
        { call in
            var header = Data(count: Int(MIGO_SYNC_ANSWER_HEADER_BYTES))
            var reply: OpaquePointer?
            let result = call.withUnsafeBytes { body in
                header.withUnsafeMutableBytes { out in
                    migo_session_call_sync(
                        session, body.bindMemory(to: UInt8.self).baseAddress, call.count,
                        // Only ever differenced by the engine, against this same
                        // reading: any monotonic clock is correct, and this one
                        // does not step.
                        DispatchTime.now().uptimeNanoseconds,
                        out.bindMemory(to: UInt8.self).baseAddress, out.count, &reply)
                }
            }
            guard result == MIGO_OK else {
                // Nothing was handed over on failure, but a handle that did come
                // back must not leak on a path that reports none.
                if let reply { _ = migo_sync_reply_release(reply) }
                return nil
            }
            guard let reply else { return SyncAnswer(header: header, reply: nil) }

            var bytes: UnsafePointer<UInt8>?
            var length = 0
            guard migo_sync_reply_bytes(reply, &bytes, &length) == MIGO_OK, let bytes, length > 0
            else {
                _ = migo_sync_reply_release(reply)
                return nil
            }
            // The renderer's buffer, owned by `Data` from here: no copy, and
            // released when WebKit has taken the bytes and the last reference
            // goes. `Data` never writes through a no-copy buffer it did not
            // allocate -- a mutation copies first -- so handing it a pointer the
            // engine considers read-only is sound.
            let wrapped = Data(
                bytesNoCopy: UnsafeMutableRawPointer(mutating: bytes), count: length,
                deallocator: .custom { _, _ in _ = migo_sync_reply_release(reply) })
            return SyncAnswer(header: header, reply: wrapped)
        }
    }

    static func engineSubmitService(_ session: OpaquePointer) -> SubmitService {
        { message in
            var refusal: UInt32 = 0
            let result = message.withUnsafeBytes { bytes -> MigoResult in
                migo_session_submit_service(
                    session, bytes.bindMemory(to: UInt8.self).baseAddress, message.count, &refusal)
            }
            guard result == MIGO_OK else { return .unavailable }
            return refusal == 0 ? .admitted : .refused(code: refusal)
        }
    }

    static func engineTakeParked(_ session: OpaquePointer) -> TakeParkedReply {
        { generation, requestId in
            var owned: OpaquePointer?
            guard migo_session_take_parked_reply(session, generation, requestId, &owned) == MIGO_OK
            else { return nil }
            return MigoFrameChannel.adopt(owned)
        }
    }

    static func engineReadContentModule(_ session: OpaquePointer) -> ReadContentModule {
        { path in
            var owned: OpaquePointer?
            var status: UInt32 = 0
            let result = path.utf8CString.withUnsafeBufferPointer { text -> MigoResult in
                // Without the NUL: the engine takes a length.
                migo_session_read_content_module(
                    session, text.baseAddress, text.count - 1, &owned, &status)
            }
            guard result == MIGO_OK else {
                return .unavailable("the engine has no content loaded to serve \(path) from")
            }
            // An empty module is a module: adopted bytes of length zero come
            // back as nil, and are served as empty source.
            let bytes = MigoFrameChannel.adopt(owned) ?? Data()
            let reason = { String(decoding: bytes, as: UTF8.self) }
            switch status {
            case UInt32(MIGO_CONTENT_MODULE_SERVED): return .source(bytes)
            case UInt32(MIGO_CONTENT_MODULE_NOT_FOUND): return .notFound(reason())
            case UInt32(MIGO_CONTENT_MODULE_REFUSED): return .refused(reason())
            default: return .unreadable(reason())
            }
        }
    }

    /// The engine's bytes, owned by `Data` from here: no copy, released when the
    /// last reference goes. `Data` never writes through a no-copy buffer it did
    /// not allocate -- a mutation copies first -- so a pointer the engine treats
    /// as read-only is sound to hand it.
    static func adopt(_ owned: OpaquePointer?) -> Data? {
        guard let owned else { return nil }
        var bytes: UnsafePointer<UInt8>?
        var length = 0
        guard migo_owned_bytes_view(owned, &bytes, &length) == MIGO_OK, let bytes, length > 0 else {
            _ = migo_owned_bytes_release(owned)
            return nil
        }
        return Data(
            bytesNoCopy: UnsafeMutableRawPointer(mutating: bytes), count: length,
            deallocator: .custom { _, _ in _ = migo_owned_bytes_release(owned) })
    }
}

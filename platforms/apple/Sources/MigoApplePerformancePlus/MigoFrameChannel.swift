import Foundation
import MigoAppleCore
import MigoEngine

/// The producer's frames in, the host's answers out.
///
/// [`MigoFrameTransport`] carries bytes and knows nothing about frames; the
/// engine validates frames and knows nothing about sockets. This is the piece
/// between them, and it is deliberately the only place in the Swift package that
/// touches both.
///
/// ## What one frame costs here
///
/// A packet arrives on the transport's queue, goes straight into
/// `migo_session_submit_external_frame`, and whatever the engine queued in reply
/// goes straight back out. No hop to another queue, no copy beyond the one the
/// ABI requires: validation and crediting happen on the connection's thread
/// because that is what `include/migo/external_frames.h` says the entry point is
/// for, and a channel hop here would put a scheduling delay on the latency path
/// this lane exists to shorten.
///
/// ## Why the host never builds a reply, or reads a message's kind
///
/// `migo_session_take_downlink` writes whole downlink messages -- verdicts for
/// submitted frames, ticks from the frame clock -- and this copies them onto the
/// socket. A host that assembled records itself would be a third implementation
/// of a wire format that already has two, in a language neither the golden
/// corpus nor the interop gate checks. The same goes the other way: the socket
/// carries frames and control messages, and `migo_uplink_message_kind` says
/// which is which rather than this comparing magic numbers of its own.
///
/// ## Ticks the transport did not cause
///
/// A verdict is queued inside a submit this channel made, and this drains right
/// after. A tick is queued by the engine's frame clock on its own thread, so the
/// engine calls the waker this channel installs, and the drain is scheduled on
/// a queue of this channel's -- never performed inside the engine's call.
///
/// ## Ownership
///
/// The channel does not own the session and does not extend its life. Stop the
/// channel before `migo_session_destroy`; `deinit` stops it too, but a channel
/// still pumping when its session is destroyed has already lost that race --
/// the same rule `MigoSessionFrameClock` documents, for the same reason.
public final class MigoFrameChannel {

    /// What a run of this channel did, for a host that has to explain a frame
    /// rate or a stall.
    public struct Statistics: Equatable, Sendable {
        /// Packets the producer sent.
        public var framesReceived: Int = 0
        /// Packets the engine accepted.
        public var framesAccepted: Int = 0
        /// Packets that arrived ahead of their predecessor and were held until
        /// it came. Not refused and not yet accepted: the uplink is two
        /// independent streams, and ingress executes them in order. The verdict
        /// for each reaches the producer on the downlink when it is admitted;
        /// that admission happens inside a later submit and is not counted
        /// again here.
        public var framesDeferred: Int = 0
        /// Packets the engine refused, for any of its four reasons. Not an
        /// error here: `WOULD_BLOCK` is the credit window doing its job.
        public var framesRefused: Int = 0
        /// Downlink messages sent back.
        public var messagesSent: Int = 0
        /// Downlink records the engine dropped because this channel was not
        /// draining. Non-zero means the transport fell behind; every record is
        /// absolute, so the producer recovers on the next one it reads.
        public var recordsDropped: Int = 0
        /// Sends that failed because no producer was connected. Counted rather
        /// than raised: between a WebContent termination and the rebuilt
        /// producer's connection this is the expected state, not a fault.
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
        /// Control messages the engine refused, counted as a refused frame is.
        /// Non-zero is a producer writing a format the engine does not read, and
        /// a producer whose request was refused is waiting for a tick.
        public var controlMessagesRefused: Int = 0
        /// The code of the most recent refusal, from 3001 up; 0 when there has
        /// been none, or the message could not be delivered at all.
        public var lastControlRefusalCode: UInt32 = 0
        /// Times the engine woke this channel to send a tick.
        public var downlinkWakes: Int = 0
        public var serviceMessagesReceived: Int = 0
        public var serviceMessagesRefused: Int = 0
        public var lastServiceRefusalCode: UInt32 = 0
        public var serviceMessagesSent: Int = 0
        public var parkedRepliesTaken: Int = 0
    }

    /// Why the channel would not start.
    public enum StartFailure: Error, Equatable {
        /// The engine would not install the downlink waker -- in practice, no
        /// surface is attached yet. Refused rather than started without one:
        /// without it a tick waits in the queue until the producer sends
        /// something, and a producer waiting for a tick sends nothing.
        case downlinkWakerRefused
    }

    /// The largest downlink message this channel will carry in one send.
    ///
    /// The queue holds at most `QUEUE_CAPACITY` records of at most eight words,
    /// so 4 KiB cannot be exceeded; it is a ceiling on the buffer rather than a
    /// guess at the traffic. A message that did not fit would be sent as two,
    /// because `migo_session_take_downlink` drains whole records and keeps the
    /// rest -- so this number is a latency choice, not a correctness one.
    private static let downlinkBufferBytes = 4096

    /// What the engine did with one packet, as far as this channel counts it.
    public enum Disposition: Sendable, Equatable {
        case accepted
        /// Held for the packet before it; see `Statistics.framesDeferred`.
        case deferred
        /// Refused, for any of the engine's reasons -- including `WOULD_BLOCK`,
        /// which is the credit window doing its job.
        case refused
    }

    /// Which door a message from the producer's socket goes through.
    public enum UplinkKind: Sendable, Equatable {
        case frame
        case control
        /// The service stream: files, storage, images, audio, network.
        case service
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

    /// What the engine did with one control message.
    public enum ControlDisposition: Sendable, Equatable {
        case read
        /// Refused, with the engine's code; 0 when the call could not be made.
        case refused(code: UInt32)
    }

    /// Hand one packet to the engine and report what became of it.
    public typealias Submit = (Data) -> Disposition
    public typealias SubmitService = (Data) -> ServiceDisposition
    /// The next message of answers and events, handed over without a copy.
    public typealias TakeServiceMessage = () -> Data?
    /// A parked answer by generation and request id, handed over without a copy.
    public typealias TakeParkedReply = (UInt32, UInt32) -> Data?
    /// Hand one control message to the engine and report what became of it.
    public typealias SubmitControl = (Data) -> ControlDisposition
    /// Ask the engine which kind a socket message is.
    public typealias Classify = (Data) -> UplinkKind
    /// Install the engine's downlink waker, or clear it with `nil`; `false` when
    /// the engine would not. Clearing returns once no call is in progress.
    public typealias SetDownlinkWaker = ((() -> Void)?) -> Bool
    /// Fill the buffer with the next downlink message and return its length.
    public typealias TakeDownlink = (UnsafeMutableBufferPointer<UInt8>) -> Int
    /// One synchronous call's answer: the header, then the reply when there is
    /// one. Sent in that order they are one response body.
    ///
    /// Two parts because the reply is the engine's own readback buffer, wrapped
    /// rather than copied, and joining it to a header would copy it.
    public struct SyncAnswer {
        public let header: Data
        public let reply: Data?
    }

    /// Answer one synchronous call body, or `nil` when no answer could be
    /// produced at all. Blocks.
    public typealias AnswerSync = (Data) -> SyncAnswer?

    private let submit: Submit
    private let takeDownlink: TakeDownlink
    private let answerSync: AnswerSync
    private let classify: Classify
    private let submitControl: SubmitControl
    private let submitService: SubmitService
    private let takeServiceMessage: TakeServiceMessage
    private let takeParked: TakeParkedReply
    private let setDownlinkWaker: SetDownlinkWaker
    private let transport: MigoFrameTransport
    private let lock = NSLock()
    private var statistics = Statistics()
    private var downlink = [UInt8](repeating: 0, count: MigoFrameChannel.downlinkBufferBytes)

    /// Where a wake-up's drain runs. Serial and its own, because the engine's
    /// call must return at once and the transport's queue is where frames
    /// arrive; `userInteractive`, because a tick is the start of a frame.
    private let wakeQueue = DispatchQueue(
        label: "dev.migo.frame-channel.downlink", qos: .userInteractive)
    /// A drain is already scheduled. Coalesces wake-ups that arrive faster than
    /// the queue runs, so a busy frame clock costs one pending block, not many.
    private var wakePending = false
    private let wakeLock = NSLock()

    /// The production shape: a live session.
    ///
    /// A convenience over the closure initialiser below, which is the same
    /// arrangement `MigoSessionFrameClock` uses and for the same reason -- the
    /// behaviour worth testing is the pumping, and a test that had to stand up a
    /// renderer to reach it would be testing the renderer.
    public convenience init(
        session: OpaquePointer, transport: MigoFrameTransport = MigoFrameTransport()
    ) {
        self.init(
            transport: transport,
            submit: { packet in
                var outcome = MigoFrameIngressOutcome()
                outcome.struct_size = UInt32(MemoryLayout<MigoFrameIngressOutcome>.size)
                outcome.abi_version = MIGO_ABI_VERSION_CURRENT
                let result = packet.withUnsafeBytes { bytes -> MigoResult in
                    migo_session_submit_external_frame(
                        session, bytes.bindMemory(to: UInt8.self).baseAddress, packet.count,
                        &outcome)
                }
                guard result == MIGO_OK else { return .refused }
                switch outcome.decision {
                case MigoFrameIngressDecision(MIGO_FRAME_INGRESS_ACCEPTED): return .accepted
                case MigoFrameIngressDecision(MIGO_FRAME_INGRESS_DEFERRED): return .deferred
                default: return .refused
                }
            },
            takeDownlink: { buffer in
                var written = 0
                let result = migo_session_take_downlink(
                    session, buffer.baseAddress, buffer.count, &written)
                return result == MIGO_OK ? written : 0
            },
            answerSync: { call in
                var header = Data(count: Int(MIGO_SYNC_ANSWER_HEADER_BYTES))
                var reply: OpaquePointer?
                let result = call.withUnsafeBytes { body in
                    header.withUnsafeMutableBytes { out in
                        migo_session_call_sync(
                            session, body.bindMemory(to: UInt8.self).baseAddress, call.count,
                            // Only ever differenced by the engine, against this
                            // same reading: any monotonic clock is correct, and
                            // this one does not step.
                            DispatchTime.now().uptimeNanoseconds,
                            out.bindMemory(to: UInt8.self).baseAddress, out.count, &reply)
                    }
                }
                guard result == MIGO_OK else {
                    // Nothing was handed over on failure, but a handle that did
                    // come back must not leak on a path that reports none.
                    if let reply { _ = migo_sync_reply_release(reply) }
                    return nil
                }
                guard let reply else { return SyncAnswer(header: header, reply: nil) }

                var bytes: UnsafePointer<UInt8>?
                var length = 0
                guard migo_sync_reply_bytes(reply, &bytes, &length) == MIGO_OK, let bytes,
                    length > 0
                else {
                    _ = migo_sync_reply_release(reply)
                    return nil
                }
                // The renderer's buffer, owned by `Data` from here: no copy, and
                // released when WebKit has taken the bytes and the last reference
                // goes. `Data` never writes through a no-copy buffer it did not
                // allocate -- a mutation copies first -- so handing it a pointer
                // the engine considers read-only is sound.
                let wrapped = Data(
                    bytesNoCopy: UnsafeMutableRawPointer(mutating: bytes), count: length,
                    deallocator: .custom { _, _ in _ = migo_sync_reply_release(reply) })
                return SyncAnswer(header: header, reply: wrapped)
            },
            submitControl: { message in
                var refusal: UInt32 = 0
                let result = message.withUnsafeBytes { bytes -> MigoResult in
                    migo_session_submit_uplink_control(
                        session, bytes.bindMemory(to: UInt8.self).baseAddress, message.count,
                        &refusal)
                }
                guard result == MIGO_OK else { return .refused(code: 0) }
                return refusal == 0 ? .read : .refused(code: refusal)
            },
            setDownlinkWaker: MigoDownlinkWakerSlot(session: session).install,
            submitService: { message in
                var refusal: UInt32 = 0
                let result = message.withUnsafeBytes { bytes -> MigoResult in
                    migo_session_submit_service(
                        session, bytes.bindMemory(to: UInt8.self).baseAddress, message.count,
                        &refusal)
                }
                guard result == MIGO_OK else { return .unavailable }
                return refusal == 0 ? .admitted : .refused(code: refusal)
            },
            takeServiceMessage: {
                var owned: OpaquePointer?
                guard migo_session_take_service_message(session, &owned) == MIGO_OK else { return nil }
                return MigoFrameChannel.adopt(owned)
            },
            takeParked: { generation, requestId in
                var owned: OpaquePointer?
                guard
                    migo_session_take_parked_reply(session, generation, requestId, &owned) == MIGO_OK
                else { return nil }
                return MigoFrameChannel.adopt(owned)
            })
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

    /// The engine's answer to "which door": the default for every channel,
    /// because it needs no session and a test that faked it would be testing a
    /// copy of the rule.
    public static func engineClassify(_ message: Data) -> UplinkKind {
        var kind: MigoUplinkMessageKind = 0
        let result = message.withUnsafeBytes { bytes -> MigoResult in
            migo_uplink_message_kind(
                bytes.bindMemory(to: UInt8.self).baseAddress, message.count, &kind)
        }
        guard result == MIGO_OK else { return .frame }
        switch kind {
        case MigoUplinkMessageKind(MIGO_UPLINK_MESSAGE_CONTROL): return .control
        case MigoUplinkMessageKind(MIGO_UPLINK_MESSAGE_SERVICE): return .service
        default: return .frame
        }
    }

    init(
        transport: MigoFrameTransport = MigoFrameTransport(),
        submit: @escaping Submit,
        takeDownlink: @escaping TakeDownlink,
        answerSync: @escaping AnswerSync = { _ in nil },
        classify: @escaping Classify = MigoFrameChannel.engineClassify,
        submitControl: @escaping SubmitControl = { _ in .read },
        setDownlinkWaker: @escaping SetDownlinkWaker = { _ in true },
        submitService: @escaping SubmitService = { _ in .unavailable },
        takeServiceMessage: @escaping TakeServiceMessage = { nil },
        takeParked: @escaping TakeParkedReply = { _, _ in nil }
    ) {
        self.transport = transport
        self.submit = submit
        self.takeDownlink = takeDownlink
        self.answerSync = answerSync
        self.classify = classify
        self.submitControl = submitControl
        self.setDownlinkWaker = setDownlinkWaker
        self.submitService = submitService
        self.takeServiceMessage = takeServiceMessage
        self.takeParked = takeParked
    }

    /// Start listening and return what the producer needs to connect.
    ///
    /// Installs the engine's downlink waker too, and throws
    /// `StartFailure.downlinkWakerRefused` rather than start without it.
    public func start() throws -> MigoFrameTransport.Endpoint {
        guard setDownlinkWaker({ [weak self] in self?.wake() }) else {
            throw StartFailure.downlinkWakerRefused
        }
        transport.onFrame = { [weak self] data in
            self?.receiveFromSocket(data)
        }
        transport.onConnectionChange = { [weak self] connected in
            // A producer that has just connected is a producer that has missed
            // whatever was queued while it was away. Draining here rather than
            // waiting for the next frame means it learns its credit level
            // immediately, which is what it needs before it can send anything.
            if connected { self?.pump() }
        }
        do {
            return try transport.start()
        } catch {
            _ = setDownlinkWaker(nil)
            throw error
        }
    }

    /// Send whatever the engine has queued.
    ///
    /// Call after delivering a vsync, and after anything else that might have
    /// produced a record. It is cheap when there is nothing: the engine reports
    /// an empty queue as zero bytes and this sends nothing.
    public func pump() {
        // Taking and sending happen under one lock, and that is about ORDER
        // rather than about the buffer. Two threads that each took a message and
        // then raced to send would deliver them in whichever order the scheduler
        // chose -- and downlink order is load-bearing: a verdict queued before a
        // tick has to arrive before it, or the producer schedules against a
        // credit level it has not been told about yet. `frame-wire`'s queue goes
        // to some trouble to keep that order; losing it here would waste it.
        //
        // `NWConnection.send` does not block -- it hands the bytes to the
        // connection's own queue -- so the lock is held for a copy and an
        // enqueue.
        lock.lock()
        defer { lock.unlock() }
        let written = downlink.withUnsafeMutableBufferPointer { takeDownlink($0) }
        if written > 0 {
            statistics.messagesSent += 1
            do {
                try transport.send(Data(downlink[0..<written]))
            } catch {
                statistics.sendsWithoutProducer += 1
            }
        }
        // The service stream's answers and events, in the order the engine
        // produced them, behind the frame records: the same waker fires for
        // both, and one drain sends both.
        while let message = takeServiceMessage() {
            statistics.serviceMessagesSent += 1
            do {
                try transport.send(message)
            } catch {
                statistics.sendsWithoutProducer += 1
                break
            }
        }
    }

    /// Stop the transport and clear the engine's waker. Idempotent.
    ///
    /// The waker first: clearing it returns only once the engine is not inside
    /// a call to it, so nothing reaches this channel after `stop` returns.
    public func stop() {
        _ = setDownlinkWaker(nil)
        transport.onFrame = nil
        transport.onConnectionChange = nil
        transport.stop()
    }

    deinit { stop() }

    /// A snapshot of what this channel has done.
    public var currentStatistics: Statistics {
        lock.lock()
        defer { lock.unlock() }
        return statistics
    }

    /// Whether a producer is connected.
    public var isConnected: Bool { transport.isConnected }

    /// One frame that arrived on the other uplink.
    ///
    /// Above `MigoFrameChannelPolicy.socketCeilingBytes` the producer POSTs to
    /// the content origin instead of sending on the socket, because G0's P3
    /// measured the scheme 4.4x faster and 4.6x cheaper at 1 MiB. The two
    /// uplinks meet here: same submit, same statistics, same pump, so a frame is
    /// counted and answered identically whichever way it arrived. A second
    /// accounting path would make the statistics depend on a packet's size.
    ///
    /// The answer still goes back on the downlink socket. It is the only channel
    /// that carries a verdict, and a second source for an absolute credit level
    /// is how two sources disagree.
    @discardableResult
    public func submitFromOrigin(_ packet: Data) -> Disposition {
        receive(packet)
    }

    /// One synchronous call, answered.
    ///
    /// The producer is a Worker blocked in a synchronous request to the content
    /// origin, because that origin has no SharedArrayBuffer to block on. The
    /// body goes to the engine unchanged and the answer comes back unchanged;
    /// the engine waits for the frame the call names before it reads, and writes
    /// the answer under the lock that settled it, so nothing here orders or
    /// matches anything.
    ///
    /// Blocks for as long as the call takes. Call it on a queue that is NOT the
    /// one frames arrive on: the frame a read waits for would otherwise queue
    /// behind the read, and the read would wait out its whole timeout for it.
    /// A service message that arrived as a POST to the content origin: the ones
    /// too large for the socket. The engine restores the order the two paths
    /// lost, so this is answered at once.
    @discardableResult
    public func submitServiceFromOrigin(_ message: Data) -> ServiceDisposition {
        receiveService(message)
    }

    /// A parked answer the producer asked the content origin for. Taken once.
    public func takeParkedReply(generation: UInt32, requestId: UInt32) -> Data? {
        let reply = takeParked(generation, requestId)
        if reply != nil {
            lock.lock()
            statistics.parkedRepliesTaken += 1
            lock.unlock()
        }
        return reply
    }

    public func answerSyncCall(_ call: Data) -> SyncAnswer? {
        let answer = answerSync(call)
        lock.lock()
        if answer == nil {
            statistics.syncCallsUnanswered += 1
        } else {
            statistics.syncCallsAnswered += 1
        }
        lock.unlock()
        return answer
    }

    // MARK: - Private

    /// One message from the socket, to whichever door the engine names.
    private func receiveFromSocket(_ message: Data) {
        switch classify(message) {
        case .frame: receive(message)
        case .control: receiveControl(message)
        case .service: receiveService(message)
        }
    }

    @discardableResult
    private func receiveService(_ message: Data) -> ServiceDisposition {
        // May block while the engine's queue of admitted work is full, which is
        // back-pressure on whichever path carried the message.
        let disposition = submitService(message)
        lock.lock()
        statistics.serviceMessagesReceived += 1
        if case .refused(let code) = disposition {
            statistics.serviceMessagesRefused += 1
            statistics.lastServiceRefusalCode = code
        }
        lock.unlock()
        // A refusal is queued for the producer; send it now rather than behind
        // whatever next wakes the drain.
        if case .refused = disposition { pump() }
        return disposition
    }

    /// A request for a frame. Nothing is queued for the producer by reading one
    /// -- the tick it asks for arrives through the waker -- so there is no pump.
    private func receiveControl(_ message: Data) {
        let disposition = submitControl(message)
        lock.lock()
        statistics.controlMessagesReceived += 1
        if case .refused(let code) = disposition {
            statistics.controlMessagesRefused += 1
            statistics.lastControlRefusalCode = code
        }
        lock.unlock()
    }

    /// Called by the engine, on its thread, when a tick is queued. Schedules a
    /// drain and returns.
    private func wake() {
        wakeLock.lock()
        let alreadyPending = wakePending
        wakePending = true
        wakeLock.unlock()
        guard !alreadyPending else { return }
        wakeQueue.async { [weak self] in
            guard let self else { return }
            // Cleared before the drain, so a tick queued while it runs schedules
            // another rather than waiting for the next one.
            self.wakeLock.lock()
            self.wakePending = false
            self.wakeLock.unlock()
            self.lock.lock()
            self.statistics.downlinkWakes += 1
            self.lock.unlock()
            self.pump()
        }
    }

    @discardableResult
    private func receive(_ packet: Data) -> Disposition {
        let disposition = submit(packet)
        lock.lock()
        statistics.framesReceived += 1
        switch disposition {
        case .accepted: statistics.framesAccepted += 1
        case .deferred: statistics.framesDeferred += 1
        case .refused: statistics.framesRefused += 1
        }
        lock.unlock()

        // Every decision, including the refusals. A producer told nothing about
        // a frame it sent has to time out to find out, and a timeout is
        // indistinguishable from a host that died.
        pump()
        return disposition
    }
}

/// The engine's downlink waker for one session, and the box its pointer names.
///
/// The box is retained while the engine holds its pointer and released only
/// after the engine has let go: `migo_session_set_downlink_waker` returns from a
/// clear or a replacement only once no call to the old waker is in progress, so
/// releasing after it returns cannot free what a call is using.
final class MigoDownlinkWakerSlot {

    private final class Box {
        let wake: () -> Void
        init(_ wake: @escaping () -> Void) { self.wake = wake }
    }

    private let session: OpaquePointer
    private let lock = NSLock()
    private var installed: Unmanaged<Box>?

    init(session: OpaquePointer) {
        self.session = session
    }

    deinit {
        // Unreachable while installed: the closure that owns this slot is held
        // by a channel, whose `stop` clears the waker. A box still here was
        // never cleared, and releasing it could free what the engine calls.
        if installed != nil {
            assertionFailure("a downlink waker was never cleared; stop the channel first")
        }
    }

    func install(_ wake: (() -> Void)?) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        guard let wake else {
            guard let previous = installed else { return true }
            let result = migo_session_set_downlink_waker(session, nil, nil)
            // INVALID_STATE means the session has no engine, so nothing can be
            // calling the waker. Any other failure means the engine may still
            // hold the pointer, and a leaked box is the safe outcome.
            guard result == MIGO_OK || result == MIGO_ERROR_INVALID_STATE else { return false }
            previous.release()
            installed = nil
            return true
        }
        let box = Unmanaged.passRetained(Box(wake))
        let result = migo_session_set_downlink_waker(
            session, { userData in
                guard let userData else { return }
                Unmanaged<Box>.fromOpaque(userData).takeUnretainedValue().wake()
            }, box.toOpaque())
        guard result == MIGO_OK else {
            box.release()
            return false
        }
        installed?.release()
        installed = box
        return true
    }
}

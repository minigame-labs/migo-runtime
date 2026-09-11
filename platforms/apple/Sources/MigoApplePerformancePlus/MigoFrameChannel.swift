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
/// ## Why the host never builds a reply
///
/// `migo_session_take_downlink` writes whole downlink messages -- verdicts for
/// submitted frames, ticks from the frame clock -- and this copies them onto the
/// socket. A host that assembled records itself would be a third implementation
/// of a wire format that already has two, in a language neither the golden
/// corpus nor the interop gate checks.
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
    }

    /// The largest downlink message this channel will carry in one send.
    ///
    /// The queue holds at most `QUEUE_CAPACITY` records of at most seven words,
    /// so 4 KiB cannot be reached; it is a ceiling on the buffer rather than a
    /// guess at the traffic. A message that did not fit would be sent as two,
    /// because `migo_session_take_downlink` drains whole records and keeps the
    /// rest -- so this number is a latency choice, not a correctness one.
    private static let downlinkBufferBytes = 4096

    /// Hand one packet to the engine and report whether it was accepted.
    public typealias Submit = (Data) -> Bool
    /// Fill the buffer with the next downlink message and return its length.
    public typealias TakeDownlink = (UnsafeMutableBufferPointer<UInt8>) -> Int

    private let submit: Submit
    private let takeDownlink: TakeDownlink
    private let transport: MigoFrameTransport
    private let lock = NSLock()
    private var statistics = Statistics()
    private var downlink = [UInt8](repeating: 0, count: MigoFrameChannel.downlinkBufferBytes)

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
                return result == MIGO_OK
                    && outcome.decision == MigoFrameIngressDecision(MIGO_FRAME_INGRESS_ACCEPTED)
            },
            takeDownlink: { buffer in
                var written = 0
                let result = migo_session_take_downlink(
                    session, buffer.baseAddress, buffer.count, &written)
                return result == MIGO_OK ? written : 0
            })
    }

    init(
        transport: MigoFrameTransport = MigoFrameTransport(),
        submit: @escaping Submit,
        takeDownlink: @escaping TakeDownlink
    ) {
        self.transport = transport
        self.submit = submit
        self.takeDownlink = takeDownlink
    }

    /// Start listening and return what the producer needs to connect.
    public func start() throws -> MigoFrameTransport.Endpoint {
        transport.onFrame = { [weak self] data in
            self?.receive(data)
        }
        transport.onConnectionChange = { [weak self] connected in
            // A producer that has just connected is a producer that has missed
            // whatever was queued while it was away. Draining here rather than
            // waiting for the next frame means it learns its credit level
            // immediately, which is what it needs before it can send anything.
            if connected { self?.pump() }
        }
        return try transport.start()
    }

    /// Send whatever the engine has queued.
    ///
    /// Call after delivering a vsync, and after anything else that might have
    /// produced a record. It is cheap when there is nothing: the engine reports
    /// an empty queue as zero bytes and this sends nothing.
    public func pump() {
        lock.lock()
        let written = downlink.withUnsafeMutableBufferPointer { takeDownlink($0) }
        guard written > 0 else {
            lock.unlock()
            return
        }
        let message = Data(downlink[0..<written])
        statistics.messagesSent += 1
        lock.unlock()

        do {
            try transport.send(message)
        } catch {
            lock.lock()
            statistics.sendsWithoutProducer += 1
            lock.unlock()
        }
    }

    /// Stop the transport. Idempotent.
    public func stop() {
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

    // MARK: - Private

    private func receive(_ packet: Data) {
        let accepted = submit(packet)
        lock.lock()
        statistics.framesReceived += 1
        if accepted {
            statistics.framesAccepted += 1
        } else {
            statistics.framesRefused += 1
        }
        lock.unlock()

        // Every decision, including the refusals. A producer told nothing about
        // a frame it sent has to time out to find out, and a timeout is
        // indistinguishable from a host that died.
        pump()
    }
}

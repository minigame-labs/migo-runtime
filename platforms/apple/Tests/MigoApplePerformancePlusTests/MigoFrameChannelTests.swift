import XCTest

@testable import MigoApplePerformancePlus

/// The piece between the socket and the engine, against a real socket.
///
/// The engine half is a closure, which is the arrangement
/// `MigoSessionFrameClock` established: what is worth testing here is the
/// pumping -- a frame in produces a submit and then a send, a reconnect drains
/// what was waiting, a send with no producer is counted rather than thrown --
/// and a test that had to stand up a renderer to reach any of it would be
/// testing the renderer.
final class MigoFrameChannelTests: XCTestCase {

    /// A downlink message the engine could have written: magic "MDL1",
    /// version 1, and one seven-word verdict. Not parsed here; it is the real
    /// shape so the test reads as what the channel carries.
    fileprivate static let verdictMessage: [UInt8] = {
        var words: [UInt32] = [0x4D44_4C31, 1]
        words.append((7 << 12) | 1)  // pack_header(DOWN_FRAME_VERDICT, 7)
        words.append(contentsOf: [1, 1, 0, 2, 0x1234_5678, 0])
        var bytes: [UInt8] = []
        for word in words {
            bytes.append(contentsOf: withUnsafeBytes(of: word.littleEndian, Array.init))
        }
        return bytes
    }()

    private func client(for endpoint: MigoFrameTransport.Endpoint) -> URLSessionWebSocketTask {
        let task = URLSession(configuration: .ephemeral).webSocketTask(with: endpoint.url)
        task.resume()
        return task
    }

    /// A queue that has nothing to say until a frame has been submitted.
    ///
    /// This is the engine's own shape, and getting it wrong made a test race.
    /// `MigoFrameChannel` pumps when a producer CONNECTS -- deliberately, so a
    /// producer that missed a verdict while it was away learns its credit level
    /// before it sends anything -- so a fake that has a message ready from the
    /// start hands it over at connect time, and the message the test then
    /// receives is not the one it was about. `testARefusedFrameStillProducesAnAnswer`
    /// failed exactly that way on the macOS lane while passing on the simulator:
    /// the wait succeeded on the connect-time message and `framesRefused` was
    /// still 0.
    private final class VerdictAfterSubmit {
        private let lock = NSLock()
        private var armed = false
        private var delivered = false

        func armFromSubmit() {
            lock.lock()
            armed = true
            lock.unlock()
        }

        func take(into buffer: UnsafeMutableBufferPointer<UInt8>) -> Int {
            lock.lock()
            defer { lock.unlock() }
            guard armed, !delivered else { return 0 }
            delivered = true
            _ = buffer.update(fromContentsOf: MigoFrameChannelTests.verdictMessage)
            return MigoFrameChannelTests.verdictMessage.count
        }
    }

    func testAFrameIsSubmittedAndTheAnswerGoesBack() throws {
        let submitted = expectation(description: "the engine is handed the packet")
        var seen: Data?
        let queue = VerdictAfterSubmit()

        let channel = MigoFrameChannel(
            submit: { packet in
                seen = packet
                queue.armFromSubmit()
                submitted.fulfill()
                return true
            },
            takeDownlink: { queue.take(into: $0) })
        let endpoint = try channel.start()
        defer { channel.stop() }

        let producer = client(for: endpoint)
        defer { producer.cancel(with: .goingAway, reason: nil) }

        let answered = expectation(description: "the producer receives the verdict")
        producer.receive { result in
            if case .success(.data(let data)) = result {
                XCTAssertEqual(Array(data), Self.verdictMessage)
                answered.fulfill()
            } else {
                XCTFail("expected a binary downlink message, got \(result)")
            }
        }

        let packet = Data([1, 2, 3, 4, 5, 6, 7, 8])
        let sent = expectation(description: "the producer's send completes")
        producer.send(.data(packet)) { error in
            XCTAssertNil(error)
            sent.fulfill()
        }

        wait(for: [sent, submitted, answered], timeout: 10)
        XCTAssertEqual(seen, packet, "the engine must see the producer's bytes unchanged")

        let statistics = channel.currentStatistics
        XCTAssertEqual(statistics.framesReceived, 1)
        XCTAssertEqual(statistics.framesAccepted, 1)
        XCTAssertEqual(statistics.framesRefused, 0)
        XCTAssertEqual(statistics.messagesSent, 1)
    }

    func testARefusedFrameStillProducesAnAnswer() throws {
        // The whole point: a producer told nothing about a frame it sent has to
        // time out to find out, and a timeout is indistinguishable from a host
        // that died.
        let queue = VerdictAfterSubmit()
        let channel = MigoFrameChannel(
            submit: { _ in
                queue.armFromSubmit()
                return false
            },
            takeDownlink: { queue.take(into: $0) })
        let endpoint = try channel.start()
        defer { channel.stop() }

        let producer = client(for: endpoint)
        defer { producer.cancel(with: .goingAway, reason: nil) }
        // Only the verdict can arrive: the queue above is empty until a frame
        // has been submitted, so the connect-time pump sends nothing.
        let answered = expectation(description: "a refusal is still answered")
        producer.receive { result in
            if case .success(.data) = result { answered.fulfill() } else { XCTFail("\(result)") }
        }
        producer.send(.data(Data([9, 9, 9, 9]))) { _ in }
        wait(for: [answered], timeout: 10)
        XCTAssertEqual(channel.currentStatistics.framesRefused, 1)
    }

    func testAProducerThatJustConnectedIsToldWhatIsWaiting() throws {
        // A producer cannot send anything until it knows its credit level, and
        // the level it needs may have been queued while it was away -- after a
        // WebContent termination, that is every time.
        var pending = Self.verdictMessage
        let channel = MigoFrameChannel(
            submit: { _ in true },
            takeDownlink: { buffer in
                guard !pending.isEmpty else { return 0 }
                let count = pending.count
                _ = buffer.update(fromContentsOf: pending)
                pending = []
                return count
            })
        let endpoint = try channel.start()
        defer { channel.stop() }

        let producer = client(for: endpoint)
        defer { producer.cancel(with: .goingAway, reason: nil) }
        let answered = expectation(description: "the backlog arrives without a frame being sent")
        producer.receive { result in
            if case .success(.data) = result { answered.fulfill() } else { XCTFail("\(result)") }
        }
        wait(for: [answered], timeout: 10)
        XCTAssertEqual(
            channel.currentStatistics.framesReceived, 0,
            "nothing was submitted; the message was the connection's own backlog")
    }

    func testAnEmptyQueueSendsNothing() throws {
        let channel = MigoFrameChannel(submit: { _ in true }, takeDownlink: { _ in 0 })
        let endpoint = try channel.start()
        defer { channel.stop() }

        let producer = client(for: endpoint)
        defer { producer.cancel(with: .goingAway, reason: nil) }
        let nothing = expectation(description: "no downlink message")
        nothing.isInverted = true
        producer.receive { _ in nothing.fulfill() }
        wait(for: [nothing], timeout: 1.5)
        XCTAssertEqual(channel.currentStatistics.messagesSent, 0)
    }

    func testPumpingWithNoProducerIsCountedRatherThanThrown() throws {
        // Between a WebContent termination and the rebuilt producer connecting,
        // this is the expected state. A throw here would make every caller
        // handle the normal case as an error.
        var pending = Self.verdictMessage
        let channel = MigoFrameChannel(
            submit: { _ in true },
            takeDownlink: { buffer in
                guard !pending.isEmpty else { return 0 }
                let count = pending.count
                _ = buffer.update(fromContentsOf: pending)
                pending = []
                return count
            })
        _ = try channel.start()
        defer { channel.stop() }

        channel.pump()
        XCTAssertEqual(channel.currentStatistics.sendsWithoutProducer, 1)
        XCTAssertEqual(
            channel.currentStatistics.messagesSent, 1,
            "the message was taken from the engine; it is the send that had nowhere to go")
    }
}

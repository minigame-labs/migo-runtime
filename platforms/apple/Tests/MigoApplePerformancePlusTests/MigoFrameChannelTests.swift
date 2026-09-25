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

    /// A request for a frame, as the producer's `control.mjs` writes one:
    /// magic "MUC1", version 1, and one two-word REQUEST_FRAME for generation 1.
    fileprivate static let requestFrameMessage: [UInt8] = {
        let words: [UInt32] = [0x4D55_4331, 1, (2 << 12) | 1, 1]
        return words.flatMap { word in withUnsafeBytes(of: word.littleEndian, Array.init) }
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
                return .accepted
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
                return .refused
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
            submit: { _ in .accepted },
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
        let channel = MigoFrameChannel(submit: { _ in .accepted }, takeDownlink: { _ in 0 })
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

    /// The engine says which door a message goes through, and a request for a
    /// frame never reaches frame ingress -- which would refuse it as a bad packet
    /// and end the content.
    func testARequestForAFrameGoesToTheControlDoor() throws {
        let received = expectation(description: "the engine is handed the request")
        var frames = 0
        var seen: Data?
        let channel = MigoFrameChannel(
            submit: { _ in
                frames += 1
                return .accepted
            },
            takeDownlink: { _ in 0 },
            submitControl: { message in
                seen = message
                received.fulfill()
                return .read
            })
        let endpoint = try channel.start()
        defer { channel.stop() }
        let producer = client(for: endpoint)
        defer { producer.cancel(with: .goingAway, reason: nil) }

        producer.send(.data(Data(Self.requestFrameMessage))) { XCTAssertNil($0) }
        wait(for: [received], timeout: 10)
        XCTAssertEqual(seen.map(Array.init), Self.requestFrameMessage)
        XCTAssertEqual(frames, 0, "a control message is not a frame")
        let statistics = channel.currentStatistics
        XCTAssertEqual(statistics.controlMessagesReceived, 1)
        XCTAssertEqual(statistics.controlMessagesRefused, 0)
        XCTAssertEqual(statistics.framesReceived, 0)
    }

    func testARefusedControlMessageIsCountedWithItsCode() throws {
        let refused = expectation(description: "the engine refuses the message")
        let channel = MigoFrameChannel(
            submit: { _ in .accepted },
            takeDownlink: { _ in 0 },
            submitControl: { _ in
                defer { refused.fulfill() }
                return .refused(code: 3001)
            })
        let endpoint = try channel.start()
        defer { channel.stop() }
        let producer = client(for: endpoint)
        defer { producer.cancel(with: .goingAway, reason: nil) }

        producer.send(.data(Data(Self.requestFrameMessage))) { _ in }
        wait(for: [refused], timeout: 10)
        // The statistics are written after the closure returns.
        let deadline = Date().addingTimeInterval(5)
        while channel.currentStatistics.controlMessagesRefused == 0, Date() < deadline {
            RunLoop.current.run(until: Date().addingTimeInterval(0.005))
        }
        XCTAssertEqual(channel.currentStatistics.controlMessagesRefused, 1)
        XCTAssertEqual(channel.currentStatistics.lastControlRefusalCode, 3001)
    }

    /// A tick is queued on the engine's thread, and nothing the transport did
    /// caused it. The engine's wake-up is what sends it.
    func testAWakeFromTheEngineSendsWhatIsQueued() throws {
        let tickQueued = NSLock()
        var queued = false
        var waker: (() -> Void)?
        let channel = MigoFrameChannel(
            submit: { _ in .accepted },
            takeDownlink: { buffer in
                tickQueued.lock()
                defer { tickQueued.unlock() }
                guard queued else { return 0 }
                queued = false
                _ = buffer.update(fromContentsOf: Self.verdictMessage)
                return Self.verdictMessage.count
            },
            setDownlinkWaker: { wake in
                waker = wake
                return true
            })
        let endpoint = try channel.start()
        defer { channel.stop() }
        XCTAssertNotNil(waker, "starting installs the waker")

        let producer = client(for: endpoint)
        defer { producer.cancel(with: .goingAway, reason: nil) }
        let connected = expectation(description: "the producer is connected")
        let deadline = Date().addingTimeInterval(10)
        DispatchQueue.global().async {
            while !channel.isConnected, Date() < deadline { usleep(1_000) }
            connected.fulfill()
        }
        wait(for: [connected], timeout: 11)

        let delivered = expectation(description: "the woken channel sends the tick")
        producer.receive { result in
            if case .success(.data(let data)) = result, Array(data) == Self.verdictMessage {
                delivered.fulfill()
            } else {
                XCTFail("expected the queued message, got \(result)")
            }
        }
        // As the engine does: queue, then wake, from a thread of its own.
        DispatchQueue.global().async {
            tickQueued.lock()
            queued = true
            tickQueued.unlock()
            waker?()
        }
        wait(for: [delivered], timeout: 10)
        XCTAssertGreaterThanOrEqual(channel.currentStatistics.downlinkWakes, 1)
    }

    /// Input the engine queued with a tick reaches the producer before the
    /// tick: one wake drains both, the service stream first, so the frame the
    /// tick starts is drawn against that input and not the input before it.
    func testInputQueuedWithATickIsSentAheadOfIt() throws {
        let queueLock = NSLock()
        var queued = false
        var eventQueued = false
        var waker: (() -> Void)?
        let event = Data([0x31, 0x53, 0x44, 0x4D, 1, 0, 0, 0])
        let channel = MigoFrameChannel(
            submit: { _ in .accepted },
            takeDownlink: { buffer in
                queueLock.lock()
                defer { queueLock.unlock() }
                guard queued else { return 0 }
                queued = false
                _ = buffer.update(fromContentsOf: Self.verdictMessage)
                return Self.verdictMessage.count
            },
            setDownlinkWaker: { wake in
                waker = wake
                return true
            },
            takeServiceMessage: {
                queueLock.lock()
                defer { queueLock.unlock() }
                guard eventQueued else { return nil }
                eventQueued = false
                return event
            })
        let endpoint = try channel.start()
        defer { channel.stop() }

        let producer = client(for: endpoint)
        defer { producer.cancel(with: .goingAway, reason: nil) }
        let connected = expectation(description: "the producer is connected")
        let deadline = Date().addingTimeInterval(10)
        DispatchQueue.global().async {
            while !channel.isConnected, Date() < deadline { usleep(1_000) }
            connected.fulfill()
        }
        wait(for: [connected], timeout: 11)

        var received: [Data] = []
        let both = expectation(description: "the event and the tick arrive")
        func receive() {
            producer.receive { result in
                guard case .success(.data(let data)) = result else {
                    XCTFail("expected a message, got \(result)")
                    both.fulfill()
                    return
                }
                received.append(data)
                if received.count == 2 { both.fulfill() } else { receive() }
            }
        }
        receive()
        DispatchQueue.global().async {
            queueLock.lock()
            queued = true
            eventQueued = true
            queueLock.unlock()
            waker?()
        }
        wait(for: [both], timeout: 10)
        XCTAssertEqual(received.first, event, "the event goes first")
        XCTAssertEqual(received.last.map(Array.init), Self.verdictMessage, "the tick follows it")
    }

    func testStoppingClearsTheWakerAndARefusedWakerStopsTheStart() throws {
        var installs: [Bool] = []
        let channel = MigoFrameChannel(
            submit: { _ in .accepted },
            takeDownlink: { _ in 0 },
            setDownlinkWaker: { wake in
                installs.append(wake != nil)
                return true
            })
        _ = try channel.start()
        channel.stop()
        XCTAssertEqual(installs, [true, false], "installed on start, cleared on stop")

        let refusing = MigoFrameChannel(
            submit: { _ in .accepted },
            takeDownlink: { _ in 0 },
            setDownlinkWaker: { wake in wake == nil })
        XCTAssertThrowsError(try refusing.start()) { error in
            XCTAssertEqual(
                error as? MigoFrameChannel.StartFailure, .downlinkWakerRefused,
                "a channel whose ticks could never be sent does not start")
        }
        XCTAssertFalse(refusing.isConnected)
    }

    func testPumpingWithNoProducerIsCountedRatherThanThrown() throws {
        // Between a WebContent termination and the rebuilt producer connecting,
        // this is the expected state. A throw here would make every caller
        // handle the normal case as an error.
        var pending = Self.verdictMessage
        let channel = MigoFrameChannel(
            submit: { _ in .accepted },
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

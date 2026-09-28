import MigoAppleFrameHarness
import MigoEngine
import XCTest

@testable import MigoApplePerformancePlus

/// The channel against the engine's own endpoint, with Apple's WebSocket client
/// as the producer.
///
/// What the endpoint does with each message is the engine's, and its tests
/// (`frame_endpoint` in migo-core) drive it with a Rust client. What is asserted
/// here is what those cannot reach: that Foundation's WebSocket client
/// completes the handshake the engine answers, that its binary messages arrive
/// whole and its answers come back as binary messages, that both uplinks are
/// counted in one place, and that the app survives a producer that vanishes
/// while the engine is writing to it.
final class MigoFrameChannelTests: XCTestCase {

    private var harness: MigoFrameHarness!
    private var channel: MigoFrameChannel!

    override func setUpWithError() throws {
        try super.setUpWithError()
        harness = try MigoFrameHarness()
        channel = MigoFrameChannel(session: harness.session)
    }

    override func tearDownWithError() throws {
        channel?.stop()
        channel = nil
        if let harness {
            XCTAssertTrue(harness.shutDown(), "the retired surface never reported RELEASED")
        }
        harness = nil
        try super.tearDownWithError()
    }

    /// The first word of a downlink envelope, "MDL1": what the engine writes for
    /// verdicts and ticks. Checked rather than parsed -- the envelope is the
    /// engine's, and a test that decoded it would be a third implementation.
    private static let downlinkMagic: UInt32 = 0x4D44_4C31

    /// Not a frame packet, a control message or a service message: the ingress
    /// refuses it, and a refusal is a verdict the producer hears at once.
    private static let notAPacket = Data([1, 2, 3, 4, 5, 6, 7, 8])

    private func producer(for endpoint: MigoFrameChannel.Endpoint) -> URLSessionWebSocketTask {
        let task = URLSession(configuration: .ephemeral).webSocketTask(with: endpoint.url)
        task.resume()
        return task
    }

    private func waitUntil(
        _ what: String, timeout: TimeInterval = 10, _ condition: () -> Bool
    ) {
        let deadline = Date().addingTimeInterval(timeout)
        while !condition() {
            guard Date() < deadline else { return XCTFail("timed out waiting until \(what)") }
            RunLoop.current.run(until: Date().addingTimeInterval(0.005))
        }
    }

    private func receive(_ task: URLSessionWebSocketTask) -> Data? {
        let arrived = expectation(description: "a message arrives")
        var message: Data?
        task.receive { result in
            if case .success(.data(let data)) = result { message = data }
            arrived.fulfill()
        }
        wait(for: [arrived], timeout: 10)
        return message
    }

    private static func firstWord(_ data: Data) -> UInt32? {
        guard data.count >= 4 else { return nil }
        return data.prefix(4).withUnsafeBytes { UInt32(littleEndian: $0.loadUnaligned(as: UInt32.self)) }
    }

    func testAProducerConnectsToThePortTheEngineReports() throws {
        let endpoint = try channel.start()
        XCTAssertEqual(endpoint.url.host, "127.0.0.1")
        XCTAssertNotEqual(endpoint.port, 0)
        XCTAssertThrowsError(try channel.start(), "one endpoint per session")

        let task = producer(for: endpoint)
        defer { task.cancel(with: .goingAway, reason: nil) }
        waitUntil("the producer is connected") { channel.isConnected }
        XCTAssertEqual(channel.currentStatistics.producersConnected, 1)

        channel.stop()
        XCTAssertFalse(channel.isConnected)
        XCTAssertEqual(
            channel.currentStatistics.producersConnected, 1, "readable after stop, from its snapshot")
    }

    func testARefusedFrameIsCountedAndItsVerdictComesBackOnTheSocket() throws {
        let task = producer(for: try channel.start())
        defer { task.cancel(with: .goingAway, reason: nil) }
        waitUntil("the producer is connected") { channel.isConnected }

        task.send(.data(Self.notAPacket)) { XCTAssertNil($0) }
        let verdict = try XCTUnwrap(receive(task), "the refusal's verdict arrives")
        XCTAssertEqual(Self.firstWord(verdict), Self.downlinkMagic)

        waitUntil("the refusal is counted") { channel.currentStatistics.framesRefused == 1 }
        let statistics = channel.currentStatistics
        XCTAssertEqual(statistics.framesReceived, 1)
        XCTAssertEqual(statistics.framesAccepted, 0)
        XCTAssertGreaterThanOrEqual(statistics.messagesSent, 1)
    }

    /// Frames too large for the socket arrive at the content origin. They are
    /// counted where the socket's are, and answered on the socket.
    func testAFrameFromTheOriginIsCountedWithTheSocketsAndAnsweredOnTheSocket() throws {
        let task = producer(for: try channel.start())
        defer { task.cancel(with: .goingAway, reason: nil) }
        waitUntil("the producer is connected") { channel.isConnected }

        XCTAssertEqual(channel.submitFromOrigin(Self.notAPacket), .refused)
        let verdict = try XCTUnwrap(receive(task), "the verdict leaves on the socket")
        XCTAssertEqual(Self.firstWord(verdict), Self.downlinkMagic)
        XCTAssertEqual(channel.currentStatistics.framesReceived, 1)
        XCTAssertEqual(channel.currentStatistics.framesRefused, 1)
    }

    func testARefusedControlMessageIsCountedWithItsCode() throws {
        let task = producer(for: try channel.start())
        defer { task.cancel(with: .goingAway, reason: nil) }

        // A request for a frame, as the producer's `control.mjs` writes one --
        // magic "MUC1", version 1, one two-word REQUEST_FRAME for generation 1
        // -- and one word it does not account for.
        let words: [UInt32] = [0x4D55_4331, 1, (2 << 12) | 1, 1, 0]
        let malformed = Data(words.flatMap { withUnsafeBytes(of: $0.littleEndian, Array.init) })
        task.send(.data(malformed)) { XCTAssertNil($0) }

        waitUntil("the refusal is counted") { channel.currentStatistics.controlMessagesRefused == 1 }
        XCTAssertEqual(channel.currentStatistics.controlMessagesReceived, 1)
        XCTAssertGreaterThanOrEqual(
            channel.currentStatistics.lastControlRefusalCode, 3001, "control refusals start at 3001")
        XCTAssertEqual(channel.currentStatistics.framesReceived, 0, "not mistaken for a frame")
    }

    /// A producer whose process dies mid-frame leaves the engine writing to a
    /// socket nobody reads. On Apple platforms that write raises SIGPIPE unless
    /// the socket says otherwise, and the default action ends the app -- this
    /// test process, here.
    func testTheAppSurvivesAProducerThatVanishesWhileTheEngineWrites() throws {
        let endpoint = try channel.start()
        for _ in 0..<3 {
            let task = producer(for: endpoint)
            waitUntil("the producer is connected") { channel.isConnected }
            // Gone without a closing handshake, while every refusal below makes
            // the engine write a verdict to it.
            task.cancel()
            for _ in 0..<200 { channel.submitFromOrigin(Self.notAPacket) }
            waitUntil("the engine sees the producer go") { !channel.isConnected }
        }
        XCTAssertEqual(channel.currentStatistics.framesReceived, 600)
        XCTAssertEqual(channel.currentStatistics.producersConnected, 3)
    }
}

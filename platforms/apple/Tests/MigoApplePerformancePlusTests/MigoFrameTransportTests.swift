import Network
import XCTest

@testable import MigoApplePerformancePlus

/// The loopback frame channel, against a real client.
///
/// A test double for the socket would check that this class calls the API it
/// calls. What has to be true is that a WebSocket client can connect to the URL
/// this hands out, that bytes survive both directions unchanged, and that a
/// second producer is refused -- none of which a double can answer, and all of
/// which are cheap against `URLSessionWebSocketTask` on loopback.
final class MigoFrameTransportTests: XCTestCase {

    /// The client half. `URLSessionWebSocketTask` rather than a second
    /// `NWConnection`, deliberately: the real producer is WebKit's `WebSocket`,
    /// and two Network.framework endpoints agreeing with each other would prove
    /// less than one of them agreeing with a different implementation.
    private func connect(to endpoint: MigoFrameTransport.Endpoint) -> URLSessionWebSocketTask {
        let task = URLSession(configuration: .ephemeral).webSocketTask(with: endpoint.url)
        task.resume()
        return task
    }

    func testAProducerCanConnectToThePortTheTransportPublishes() throws {
        let transport = MigoFrameTransport()
        let endpoint = try transport.start()
        defer { transport.stop() }

        XCTAssertGreaterThan(endpoint.port, 0, "a published port of zero is not a port")
        XCTAssertEqual(
            endpoint.url.absoluteString, "ws://127.0.0.1:\(endpoint.port)/",
            "the literal address matters: `localhost` does not resolve inside WKWebView")

        let connected = expectation(description: "the transport reports a producer")
        transport.onConnectionChange = { isConnected in
            if isConnected { connected.fulfill() }
        }
        let client = connect(to: endpoint)
        defer { client.cancel(with: .goingAway, reason: nil) }
        wait(for: [connected], timeout: 10)
        XCTAssertTrue(transport.isConnected)
    }

    func testFrameBytesArriveUnchanged() throws {
        let transport = MigoFrameTransport()
        let endpoint = try transport.start()
        defer { transport.stop() }

        let arrived = expectation(description: "the transport delivers the producer's bytes")
        // A payload with every byte value and a length that is not a multiple of
        // anything convenient, so a transport that framed or padded shows it.
        let sent = Data((0..<256).map { UInt8($0 % 251) })
        var received: Data?
        transport.onFrame = { data in
            received = data
            arrived.fulfill()
        }

        let client = connect(to: endpoint)
        defer { client.cancel(with: .goingAway, reason: nil) }
        let delivered = expectation(description: "the client's send completes")
        client.send(.data(sent)) { error in
            XCTAssertNil(error, "the client could not send: \(String(describing: error))")
            delivered.fulfill()
        }
        wait(for: [delivered, arrived], timeout: 10)
        XCTAssertEqual(received, sent, "bytes must survive the trip unchanged")
    }

    func testTheHostsReplyArrivesAtTheProducer() throws {
        let transport = MigoFrameTransport()
        let endpoint = try transport.start()
        defer { transport.stop() }

        let connected = expectation(description: "connected")
        transport.onConnectionChange = { if $0 { connected.fulfill() } }
        let client = connect(to: endpoint)
        defer { client.cancel(with: .goingAway, reason: nil) }
        wait(for: [connected], timeout: 10)

        // Shaped like a downlink message rather than arbitrary: magic "MDL1",
        // version 1, and nothing else. The transport does not parse it, and the
        // point of using the real shape is that the test reads as what the
        // channel carries.
        let message = Data([0x31, 0x4C, 0x44, 0x4D, 0x01, 0x00, 0x00, 0x00])
        let arrived = expectation(description: "the producer receives it")
        client.receive { result in
            switch result {
            case .success(.data(let data)):
                XCTAssertEqual(data, message)
                arrived.fulfill()
            case .success(let other):
                XCTFail("expected binary, got \(other)")
            case .failure(let error):
                XCTFail("receive failed: \(error)")
            }
        }
        try transport.send(message)
        wait(for: [arrived], timeout: 10)
    }

    func testAnEmptyMessageIsNotSent() throws {
        let transport = MigoFrameTransport()
        let endpoint = try transport.start()
        defer { transport.stop() }
        let connected = expectation(description: "connected")
        transport.onConnectionChange = { if $0 { connected.fulfill() } }
        let client = connect(to: endpoint)
        defer { client.cancel(with: .goingAway, reason: nil) }
        wait(for: [connected], timeout: 10)

        // The engine reports "nothing to send" as zero bytes. Forwarding that
        // would make the producer parse an envelope that says nothing, so it
        // must not reach the wire -- and `send` must not throw for it either,
        // or every caller needs a branch the engine already has.
        XCTAssertNoThrow(try transport.send(Data()))

        let nothing = expectation(description: "no message arrives")
        nothing.isInverted = true
        client.receive { _ in nothing.fulfill() }
        wait(for: [nothing], timeout: 1.5)
    }

    func testSendingWithNoProducerIsRefusedRatherThanDropped() throws {
        let transport = MigoFrameTransport()
        _ = try transport.start()
        defer { transport.stop() }
        XCTAssertThrowsError(try transport.send(Data([1, 2, 3, 4]))) { error in
            XCTAssertEqual(error as? MigoFrameTransport.Failure, .notConnected)
        }
    }

    func testASecondProducerIsRefused() throws {
        let transport = MigoFrameTransport()
        let endpoint = try transport.start()
        defer { transport.stop() }

        let first = expectation(description: "the first producer connects")
        transport.onConnectionChange = { if $0 { first.fulfill() } }
        let one = connect(to: endpoint)
        defer { one.cancel(with: .goingAway, reason: nil) }
        wait(for: [first], timeout: 10)

        // The second connection is accepted by TCP and then cancelled, so what
        // is asserted is the consequence the host cares about: the second
        // producer's frames never arrive.
        let two = connect(to: endpoint)
        defer { two.cancel(with: .goingAway, reason: nil) }
        let intruder = expectation(description: "the second producer's frame arrives")
        intruder.isInverted = true
        transport.onFrame = { _ in intruder.fulfill() }
        two.send(.data(Data([9, 9, 9, 9]))) { _ in }
        wait(for: [intruder], timeout: 2)
        XCTAssertTrue(transport.isConnected, "and the first producer is still the one connected")
    }

    func testStopIsIdempotent() throws {
        let transport = MigoFrameTransport()
        _ = try transport.start()
        transport.stop()
        // Twice, because every error path calls it and a second stop that
        // trapped would turn one failure into a crash.
        transport.stop()
        XCTAssertFalse(transport.isConnected)
    }
}

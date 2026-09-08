import Network
import XCTest

@testable import MigoProbeHarness

/// The loopback listener, driven by a socket rather than by a web view.
///
/// Everything here is a way the listener could answer a probe with a fact about
/// itself. A truncated body reads as a transport that lost bytes; a dropped
/// second request reads as a capability that could not be probed; a response
/// the client cannot parse reads as the platform refusing the origin. None of
/// them needs WebKit to provoke, so none of them should wait for a device.
final class MigoLoopbackListenerTests: XCTestCase {

    private func makeListener() throws -> (MigoLoopbackListener, UInt16) {
        let resources: [String: (mime: String, body: Data)] = [
            "capability-probe.html": ("text/html", Data("<html>hello</html>".utf8)),
            "capability-probe.js": ("text/javascript", Data("// script one\n".utf8)),
        ]
        let listener = MigoLoopbackListener(resources: resources)
        try listener.start()
        let origin = try XCTUnwrap(listener.origin)
        let port = try XCTUnwrap(UInt16(origin.split(separator: ":").last.map(String.init) ?? ""))
        return (listener, port)
    }

    /// Sends `request` as one write and collects bytes until `until` responses
    /// have arrived or the deadline passes.
    private func exchange(port: UInt16, request: Data, expecting responses: Int) throws -> String {
        let connection = NWConnection(
            host: .ipv4(.loopback), port: NWEndpoint.Port(rawValue: port)!, using: .tcp)
        let ready = expectation(description: "connected")
        connection.stateUpdateHandler = { state in
            if case .ready = state { ready.fulfill() }
        }
        connection.start(queue: .global())
        wait(for: [ready], timeout: 5)

        connection.send(content: request, completion: .idempotent)

        let collected = expectation(description: "responses")
        var received = Data()
        func read() {
            connection.receive(minimumIncompleteLength: 1, maximumLength: 64 * 1024) {
                chunk, _, _, error in
                if let chunk { received.append(chunk) }
                let text = String(decoding: received, as: UTF8.self)
                if text.components(separatedBy: "HTTP/1.1").count - 1 >= responses {
                    collected.fulfill()
                    return
                }
                if error != nil { return }
                read()
            }
        }
        read()
        wait(for: [collected], timeout: 10)
        connection.cancel()
        return String(decoding: received, as: UTF8.self)
    }

    func testTwoRequestsInOneSegmentBothGetAnswered() throws {
        // The page fetches its scripts back to back, and one segment can carry
        // both. A server that restarted its read with an empty buffer after
        // responding dropped the second, and the client waited for a reply that
        // was never coming -- which the probe records as a capability it could
        // not measure.
        let (listener, port) = try makeListener()
        defer { listener.stop() }

        let pipelined = Data(
            ("GET / HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n"
                + "GET /capability-probe.js HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").utf8)
        let text = try exchange(port: port, request: pipelined, expecting: 2)

        XCTAssertEqual(
            text.components(separatedBy: "HTTP/1.1 200 OK").count - 1, 2,
            "both pipelined requests must be answered; got:\n\(text.prefix(400))")
        XCTAssertTrue(text.contains("<html>hello</html>"), "the first response is missing")
        XCTAssertTrue(text.contains("// script one"), "the second response is missing")
    }

    func testAPostBodyIsEchoedByteForByte() throws {
        // A5's arm measures whether the bytes arrive. If this server truncated
        // them, the probe would report a lost body and blame WebKit.
        let (listener, port) = try makeListener()
        defer { listener.stop() }

        let payload = "migo\u{0}\u{1}binary-ish"
        var request = Data(
            ("POST /echo-body HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: "
                + "\(payload.utf8.count)\r\n\r\n").utf8)
        request.append(Data(payload.utf8))
        let text = try exchange(port: port, request: request, expecting: 1)

        XCTAssertTrue(text.contains("HTTP/1.1 200 OK"))
        XCTAssertTrue(
            text.contains(payload),
            "the echoed body differs from what was sent:\n\(text.prefix(400))")
    }

    func testTheIsolationHeadersAreActuallySent() throws {
        // A7 asks whether WebKit honours COOP and COEP at this origin. It cannot
        // be asked unless they are sent, and a listener that stopped sending
        // them would answer "crossOriginIsolated === false" for a reason that
        // has nothing to do with the platform.
        let (listener, port) = try makeListener()
        defer { listener.stop() }

        let text = try exchange(
            port: port, request: Data("GET / HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n".utf8),
            expecting: 1)
        for header in [
            "Cross-Origin-Opener-Policy: same-origin",
            "Cross-Origin-Embedder-Policy: require-corp",
        ] {
            XCTAssertTrue(text.contains(header), "\(header) was not sent")
        }
    }

    func testAnUnknownPathIsARefusalAndNotAPlausibleDefault() throws {
        // A probe that received 200 and an empty body from a path nobody
        // implemented would record an available capability.
        let (listener, port) = try makeListener()
        defer { listener.stop() }

        let text = try exchange(
            port: port, request: Data("GET /nope HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n".utf8),
            expecting: 1)
        XCTAssertTrue(text.contains("HTTP/1.1 404 Not Found"), "got:\n\(text.prefix(200))")
    }
}

import XCTest

@testable import MigoProbeHarness
@testable import MigoProbeCore

/// The parts of gate 1's harness that can be wrong without a device.
///
/// None of these needs an iPhone, and every one of them is a way the gate could
/// produce a folder of plausible JSON that answers a different question than the
/// one asked. The device-only half is the measurement; this is the machinery
/// around it, and machinery that has never run is machinery that runs for the
/// first time on lab day.
final class MigoProbeHarnessTests: XCTestCase {

    // MARK: - the WebSocket framing A22's arm rides on

    func testAMaskedClientFrameIsUnmasked() throws {
        // What a browser sends: FIN + binary, masked, four payload bytes.
        let mask: [UInt8] = [0xAA, 0xBB, 0xCC, 0xDD]
        let payload: [UInt8] = [0x01, 0x02, 0x03, 0x04]
        var wire: [UInt8] = [0x82, 0x84]
        wire += mask
        for (index, byte) in payload.enumerated() {
            wire.append(byte ^ mask[index % 4])
        }

        let frame = try XCTUnwrap(MigoLoopbackListener.parseFrame(Data(wire)))
        XCTAssertEqual(frame.opcode, 0x2)
        XCTAssertEqual([UInt8](frame.payload), payload)
        XCTAssertEqual(frame.consumed, wire.count)
    }

    func testAFrameThatHasNotArrivedYetIsNotAFrame() {
        let wire: [UInt8] = [0x82, 0x84, 0xAA, 0xBB]  // header and half a mask
        XCTAssertNil(
            MigoLoopbackListener.parseFrame(Data(wire)),
            "a partial frame parsed as a whole one would echo a truncated payload and the "
                + "probe would record a corrupted round trip as a platform failure")
    }

    func testTheExtendedLengthFormsAreRead() throws {
        for count in [125, 126, 4096, 70000] {
            let payload = [UInt8](repeating: 0x5A, count: count)
            let mask: [UInt8] = [0x01, 0x02, 0x03, 0x04]

            var wire: [UInt8] = [0x82]
            if count < 126 {
                wire.append(0x80 | UInt8(count))
            } else if count <= 0xFFFF {
                wire.append(0x80 | 126)
                wire.append(UInt8((count >> 8) & 0xFF))
                wire.append(UInt8(count & 0xFF))
            } else {
                wire.append(0x80 | 127)
                for shift in stride(from: 56, through: 0, by: -8) {
                    wire.append(UInt8((count >> shift) & 0xFF))
                }
            }
            wire += mask
            for (index, byte) in payload.enumerated() {
                wire.append(byte ^ mask[index % 4])
            }

            let frame = try XCTUnwrap(
                MigoLoopbackListener.parseFrame(Data(wire)), "\(count) bytes did not parse")
            XCTAssertEqual(frame.payload.count, count, "\(count) bytes came back short")
        }
    }

    func testServerFramesAreUnmaskedAndRoundTrip() throws {
        // A server frame must NOT set the mask bit. WebKit closes the connection
        // on a masked server frame, which would surface as "the Worker's
        // WebSocket errored" -- a platform verdict for a bug in this file.
        for count in [0, 1, 125, 126, 65535, 65536] {
            let payload = Data(repeating: 0x7E, count: count)
            let encoded = MigoLoopbackListener.encodeFrame(opcode: 0x2, payload: payload)
            XCTAssertEqual(encoded[0], 0x82, "the FIN and opcode bits are wrong for \(count)")
            XCTAssertEqual(encoded[1] & 0x80, 0, "a server frame must not be masked (\(count))")

            // Re-read it through the parser by masking it as a client would.
            let mask: [UInt8] = [0x11, 0x22, 0x33, 0x44]
            var header = [UInt8](encoded.prefix(encoded.count - count))
            header[1] |= 0x80
            var wire = header + mask
            for (index, byte) in [UInt8](payload).enumerated() {
                wire.append(byte ^ mask[index % 4])
            }
            let frame = try XCTUnwrap(MigoLoopbackListener.parseFrame(Data(wire)))
            XCTAssertEqual(frame.payload, payload, "\(count) bytes did not survive the round trip")
        }
    }

    // MARK: - the environment fields the decision tools hold fixed

    func testTheWebKitBuildIsParsedOutOfTheUserAgent() {
        let agent =
            "Mozilla/5.0 (iPhone; CPU iPhone OS 18_2 like Mac OS X) AppleWebKit/620.1.2 "
            + "(KHTML, like Gecko) Mobile/15E148"
        XCTAssertEqual(MigoProbeEnvironment.webKitBuild(fromUserAgent: agent), "620.1.2")
    }

    func testAUserAgentWithoutTheTokenIsNotGuessedAt() {
        // "unknown" is a held-fixed value like any other. Inventing one would
        // put records from two WebKit builds into a single comparison.
        XCTAssertNil(MigoProbeEnvironment.webKitBuild(fromUserAgent: "curl/8.4.0"))
    }

    func testTheOSBuildComesFromTheSystemAndNotTheMarketingVersion() throws {
        let build = try XCTUnwrap(MigoProbeEnvironment.sysctlString("kern.osversion"))
        XCTAssertFalse(build.isEmpty)
        XCTAssertNotEqual(
            build, MigoProbeEnvironment.currentOSVersion,
            "the build and the marketing version are different fields, and two OS builds "
                + "under one marketing version are two conditions")
    }

    // MARK: - the scheme handler's body, which A5 turns on

    func testAStreamedRequestBodyIsRead() throws {
        // WebKit delivers a fetch body as a stream rather than as `httpBody`,
        // and a handler that read only the inline form would report "the body
        // was lost" for a body that arrived -- which is exactly the WebKit bug
        // A5 cited, recreated locally, against a bug that is RESOLVED FIXED.
        var request = URLRequest(url: URL(string: "migo-probe://probe/echo-body")!)
        request.httpMethod = "POST"
        let payload = Data([0x6d, 0x69, 0x67, 0x6f, 0x00, 0xff, 0x10, 0x20])
        request.httpBodyStream = InputStream(data: payload)

        XCTAssertEqual(MigoProbeSchemeHandler.readBody(from: request), .complete(payload))
    }

    func testAnInlineRequestBodyIsRead() {
        var request = URLRequest(url: URL(string: "migo-probe://probe/echo-body")!)
        request.httpMethod = "POST"
        request.httpBody = Data([0x01, 0x02])
        XCTAssertEqual(
            MigoProbeSchemeHandler.readBody(from: request), .complete(Data([0x01, 0x02])))
    }

    func testABodyThatArrivesInPiecesIsReadWholeAndNotAsAPrefix() throws {
        // The case `hasBytesAvailable` gets wrong. It is not an EOF indicator for
        // anything but a memory-backed stream: fed incrementally, it answers false
        // whenever the next bytes have not arrived yet, so a loop conditioned on it
        // returns a *prefix*. This handler echoes what it read and the page compares
        // bytes, so a prefix is reported as "the other side received N of M bytes" --
        // A5's own failure mode, produced by this file rather than by the platform.
        //
        // A bound stream pair is the only way to build that shape locally: an
        // InputStream(data:) always has its bytes available and cannot fail the way
        // the real one can.
        var input: InputStream?
        var output: OutputStream?
        Stream.getBoundStreams(
            withBufferSize: 512, inputStream: &input, outputStream: &output)
        let inputStream = try XCTUnwrap(input)
        let outputStream = try XCTUnwrap(output)

        let payload = Data((0..<4096).map { UInt8($0 % 251) })
        outputStream.open()
        let writer = Thread {
            var sent = 0
            payload.withUnsafeBytes { raw in
                guard let base = raw.bindMemory(to: UInt8.self).baseAddress else { return }
                while sent < payload.count {
                    // A gap between chunks is the whole point: it is the window in
                    // which hasBytesAvailable is false and the body is not finished.
                    Thread.sleep(forTimeInterval: 0.01)
                    let wrote = outputStream.write(
                        base + sent, maxLength: min(256, payload.count - sent))
                    if wrote <= 0 { break }
                    sent += wrote
                }
            }
            outputStream.close()
        }
        writer.start()

        var request = URLRequest(url: URL(string: "migo-probe://probe/echo-body")!)
        request.httpMethod = "POST"
        request.httpBodyStream = inputStream

        XCTAssertEqual(
            MigoProbeSchemeHandler.readBody(from: request), .complete(payload),
            "the body was read as a prefix, which the page reports as a lost body")
    }

    // MARK: - the resources actually ship

    func testEveryProbeResourceIsInTheBundle() throws {
        let resources = try MigoCapabilityGate.loadResources()
        XCTAssertEqual(
            Set(resources.keys),
            [
                "capability-probe.html", "capability-probe.js", "capability-probe-worker.js",
                "transport-probe.js",
            ],
            "a resource that is not in the bundle is a 404 at run time, and the page reports "
                + "it as a capability that could not be probed")
        for (name, resource) in resources {
            XCTAssertGreaterThan(resource.body.count, 0, "\(name) is empty")
        }
    }

    /// The capability names the probe scripts actually assign.
    ///
    /// Occurrence is not the question -- the scripts mention
    /// `no_local_network_prompt` in the comment that explains why they
    /// deliberately do not write it, and a check that searched for the string
    /// would read that comment as an answer. What matters is assignment, in the
    /// two forms the scripts use: `capabilities.<name> = probe...()` on the
    /// window and `<name>: probe...()` inside the Worker's result object.
    private func capabilitiesWrittenByTheScripts() throws -> Set<String> {
        let resources = try MigoCapabilityGate.loadResources()
        let script =
            String(decoding: resources["capability-probe.js"]!.body, as: UTF8.self)
            + String(decoding: resources["capability-probe-worker.js"]!.body, as: UTF8.self)

        // Two forms, because the scripts genuinely use two: a property
        // assignment onto the result object (`capabilities.x = ...`,
        // `results.x = ...`), and an object-literal key whose value is a probe.
        // Matching only the second missed `request_body_delivery` and
        // `websocket_in_worker`, both of which are assigned a variable holding
        // an already-resolved answer -- a scanner narrow enough to miss a real
        // assignment reports a capability as unanswered when it is answered.
        let patterns = [
            #"(?m)^\s*(?:capabilities|results)\.([a-z][a-z0-9_]{4,})\s*="#,
            #"(?m)^\s*([a-z][a-z0-9_]{4,})\s*:\s*(?:probe|answer|unsupported|available|unavailable)"#,
        ]
        let range = NSRange(script.startIndex..<script.endIndex, in: script)
        var written: Set<String> = []
        for pattern in patterns {
            let regex = try NSRegularExpression(pattern: pattern)
            regex.enumerateMatches(in: script, range: range) { match, _, _ in
                guard let match, let found = Range(match.range(at: 1), in: script) else { return }
                written.insert(String(script[found]))
            }
        }
        return written
    }

    /// The JavaScript and the contract have to name the same capabilities.
    ///
    /// This is the half `scripts/test-apple-probe-record-contract.sh` cannot
    /// check: it compares the contract with the Swift, and the answers are
    /// produced in JavaScript. A capability declared in the contract, spelled in
    /// Swift, and never written by the page encodes as `not_probed` on every
    /// record -- a run that looks complete and answers nothing.
    func testTheProbeScriptsAnswerEveryCapabilityTheContractDeclares() throws {
        let written = try capabilitiesWrittenByTheScripts()
        XCTAssertFalse(
            written.isEmpty,
            "the scanner found no capability assignments at all, so this check is vacuous")

        for capability in MigoProbeCapability.allCases {
            if capability == .noLocalNetworkPrompt {
                // The one answer a page cannot give: no API reports whether the
                // system presented an alert. It is filled in natively, and a
                // page that wrote it would be answering a question it cannot ask.
                XCTAssertFalse(
                    written.contains(capability.rawValue),
                    "the page assigns \(capability.rawValue), which it cannot observe")
                continue
            }
            XCTAssertTrue(
                written.contains(capability.rawValue),
                "no probe script ever writes \(capability.rawValue), so it encodes as "
                    + "not_probed on every record")
        }
    }

    func testTheProbeScriptsInventNoCapabilityTheContractDoesNotDeclare() throws {
        let written = try capabilitiesWrittenByTheScripts()
        let declared = Set(MigoProbeCapability.allCases.map(\.rawValue))

        // An invented name is dropped when the record is assembled, so the probe
        // would run and report nothing.
        XCTAssertEqual(
            written.subtracting(declared), [],
            "the probe scripts write capability names no contract declares; the record "
                + "assembler drops them, so those probes run and report nothing")
    }
}

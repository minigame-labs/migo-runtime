import XCTest

@testable import MigoProbeHarness

/// The wire cases that would be recorded as a platform verdict.
///
/// The loopback listener carries gate 3's transport measurement, so a defect here
/// does not present as a bug in this file: it presents as "the loopback transport
/// failed at 4 MiB", which is an architectural conclusion. Each test below is one
/// such conclusion the framing could have produced.
final class MigoLoopbackFramingTests: XCTestCase {

    private typealias Listener = MigoLoopbackListener

    /// A client frame: masked, as every client frame must be.
    private func clientFrame(opcode: UInt8, fin: Bool, payload: [UInt8]) -> Data {
        var wire: [UInt8] = [(fin ? 0x80 : 0x00) | opcode]
        let mask: [UInt8] = [0x37, 0xFA, 0x21, 0x3D]
        if payload.count < 126 {
            wire.append(0x80 | UInt8(payload.count))
        } else if payload.count <= 0xFFFF {
            wire.append(0x80 | 126)
            wire.append(UInt8((payload.count >> 8) & 0xFF))
            wire.append(UInt8(payload.count & 0xFF))
        } else {
            wire.append(0x80 | 127)
            for shift in stride(from: 56, through: 0, by: -8) {
                wire.append(UInt8((payload.count >> shift) & 0xFF))
            }
        }
        wire += mask
        for (index, byte) in payload.enumerated() { wire.append(byte ^ mask[index % 4]) }
        return Data(wire)
    }

    // MARK: - FIN

    func testTheFirstFrameOfAFragmentedMessageIsNotAWholeMessage() throws {
        // The defect this file was written for. A fragmented message's first frame
        // carries opcode 0x2 with FIN clear, and the previous parser did not read FIN
        // at all -- so it was echoed as a complete message carrying one fragment.
        // Only continuation frames were refused, which is the half that never arrives
        // first.
        let wire = clientFrame(opcode: 0x2, fin: false, payload: [1, 2, 3, 4])
        guard case .frame(let frame) = Listener.parse(wire, at: 0) else {
            return XCTFail("a legal first fragment did not parse")
        }
        XCTAssertFalse(
            frame.fin,
            "the parser reports this as a complete message, so the echo would carry one fragment "
                + "and the round trip would be recorded as corrupted by the platform")
        XCTAssertEqual(frame.opcode, 0x2)
        XCTAssertEqual([UInt8](frame.payload), [1, 2, 3, 4])
    }

    func testAWholeMessageStillSaysSo() throws {
        let wire = clientFrame(opcode: 0x2, fin: true, payload: [9])
        guard case .frame(let frame) = Listener.parse(wire, at: 0) else {
            return XCTFail("a whole message did not parse")
        }
        XCTAssertTrue(frame.fin)
    }

    // MARK: - malformed is not incomplete

    func testAMalformedFrameIsRefusedRatherThanWaitedFor() {
        // Both used to answer nil, so a broken client waited forever. A lab tool
        // that hangs reports nothing at all.
        var reserved = [UInt8](clientFrame(opcode: 0x2, fin: true, payload: [1]))
        reserved[0] |= 0x40  // a reserved bit, with no negotiated extension
        guard case .protocolError(let reason) = Listener.parse(Data(reserved), at: 0) else {
            return XCTFail("a reserved bit was accepted or treated as incomplete")
        }
        XCTAssertTrue(reason.contains("reserved"), reason)

        guard case .needMoreBytes = Listener.parse(Data([0x82]), at: 0) else {
            return XCTFail("half a header is incomplete, not invalid")
        }
    }

    func testAControlFrameCannotBeFragmentedOrLarge() {
        let fragmentedPing = clientFrame(opcode: 0x9, fin: false, payload: [1])
        guard case .protocolError(let reason) = Listener.parse(fragmentedPing, at: 0) else {
            return XCTFail(
                "a fragmented control frame was accepted, which leaves the assembler holding a "
                    + "message that can never complete")
        }
        XCTAssertTrue(reason.contains("fragmented"), reason)

        // 126 bytes in a control frame: the length is legal in the header form and
        // illegal for a control opcode.
        var wire: [UInt8] = [0x89, 0x80 | 126, 0x00, 0x7E]
        wire += [0, 0, 0, 0]
        wire += [UInt8](repeating: 0, count: 126)
        guard case .protocolError = Listener.parse(Data(wire), at: 0) else {
            return XCTFail("an oversized control frame was accepted")
        }
    }

    // MARK: - lengths

    func testALengthAboveTheBoundIsRefusedInsteadOfBuffered() {
        // Without the bound this process buffers until the system kills it, which on
        // a bench looks exactly like the platform refusing to carry the payload.
        var wire: [UInt8] = [0x82, 0x80 | 127]
        let claimed = UInt64(Listener.maximumMessageBytes) + 1
        for shift in stride(from: 56, through: 0, by: -8) {
            wire.append(UInt8((claimed >> UInt64(shift)) & 0xFF))
        }
        wire += [0, 0, 0, 0]
        guard case .protocolError(let reason) = Listener.parse(Data(wire), at: 0) else {
            return XCTFail("a frame larger than the bound was accepted")
        }
        XCTAssertTrue(reason.contains("claimed"), reason)
    }

    func testALengthWithTheHighBitSetDoesNotBecomeANegativeLength() {
        // The previous version read the 64-bit length into an `Int`, so a value above
        // Int.max landed as negative, passed the "enough bytes?" comparison, and then
        // crashed slicing a range whose lower bound exceeded its upper.
        var wire: [UInt8] = [0x82, 0x80 | 127, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]
        wire += [0, 0, 0, 0]
        guard case .protocolError = Listener.parse(Data(wire), at: 0) else {
            return XCTFail("a 64-bit length with the high bit set was not refused")
        }
    }

    func testTheLargestMeasuredPayloadIsWellInsideTheBound() {
        // The matrix's largest payload class is 4 MiB. A bound that a measurement
        // could meet would make the bound part of the result.
        XCTAssertGreaterThan(Listener.maximumMessageBytes, 4 * 1024 * 1024)
    }

    // MARK: - parsing at an offset

    func testFramesParseInPlaceWithoutRecopyingTheBuffer() throws {
        // The loop advances an offset and compacts once per receive; parsing has to
        // work from the middle of a buffer for that to be possible. The previous
        // version began by copying the whole accumulated buffer into an array on
        // every attempt, which is quadratic in the payload size -- the harness's own
        // cost, inside the function whose timing is the measurement.
        var buffer = Data()
        let sizes = [1, 200, 70_000]
        for (index, size) in sizes.enumerated() {
            buffer.append(
                clientFrame(
                    opcode: 0x2, fin: true,
                    payload: [UInt8](repeating: UInt8(index + 1), count: size)))
        }

        var offset = 0
        var seen: [Int] = []
        while offset < buffer.count {
            guard case .frame(let frame) = Listener.parse(buffer, at: offset) else { break }
            seen.append(frame.payload.count)
            XCTAssertEqual(
                frame.payload.first, UInt8(seen.count),
                "the payload read at offset \(offset) belongs to another frame")
            offset += frame.consumed
        }
        XCTAssertEqual(seen, sizes)
        XCTAssertEqual(offset, buffer.count, "the frames did not tile the buffer exactly")
    }

    func testAPartialFrameAtAnOffsetIsIncompleteAndNotInvalid() {
        var buffer = clientFrame(opcode: 0x2, fin: true, payload: [1, 2])
        let whole = buffer.count
        buffer.append(clientFrame(opcode: 0x2, fin: true, payload: [3, 4]).prefix(3))
        guard case .frame = Listener.parse(buffer, at: 0) else {
            return XCTFail("the first frame did not parse")
        }
        guard case .needMoreBytes = Listener.parse(buffer, at: whole) else {
            return XCTFail("a truncated second frame must be incomplete, not invalid")
        }
    }

    // MARK: - close

    func testACloseCarriesACodeAndAReadableReason() throws {
        let wire = Listener.encodeClose(code: 1002, reason: "a reserved frame bit was set")
        // Read it back through the parser as a peer would.
        guard case .frame(let frame) = Listener.parse(wire, at: 0) else {
            return XCTFail("the close frame this listener writes does not parse")
        }
        XCTAssertEqual(frame.opcode, 0x8)
        XCTAssertTrue(frame.fin)
        let bytes = [UInt8](frame.payload)
        XCTAssertEqual(Int(bytes[0]) << 8 | Int(bytes[1]), 1002)
        XCTAssertEqual(
            String(decoding: bytes.dropFirst(2), as: UTF8.self), "a reserved frame bit was set",
            "the reason is what reaches the page's onclose, and it is the difference between "
                + "'the transport failed' and which frame could not be read")
    }

    func testALongReasonIsTruncatedOnACharacterAndStaysAControlFrame() throws {
        let reason = String(repeating: "\u{1F642}", count: 100)  // 4 bytes each
        let wire = Listener.encodeClose(code: 1009, reason: reason)
        guard case .frame(let frame) = Listener.parse(wire, at: 0) else {
            return XCTFail("an over-long reason produced a frame that does not parse")
        }
        XCTAssertLessThanOrEqual(
            frame.payload.count, 125,
            "a control frame over 125 bytes is one the peer must reject")
        let text = String(decoding: [UInt8](frame.payload.dropFirst(2)), as: UTF8.self)
        XCTAssertFalse(
            text.contains("\u{FFFD}"),
            "the reason was cut inside a UTF-8 sequence, which makes the whole message unreadable "
                + "rather than shorter")
    }
}

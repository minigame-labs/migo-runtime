import Foundation

/// Which channel carries a frame packet, and why that is a size.
///
/// Performance+ sends frames from a Worker inside WebKit's WebContent process to
/// the host, and G0's P3 measured what each available channel costs. Neither one
/// wins outright: a loopback WebSocket has a low fixed cost and grows with bytes;
/// a custom-scheme request has a higher fixed cost and barely grows at all. So
/// the transport is a hybrid, and a hybrid needs a threshold.
///
/// **The threshold is measured, not chosen.** iPhone 12 / iOS 17.0.3, release
/// host, 200 samples per class, ratio = scheme ÷ socket, so above one the socket
/// wins:
///
/// | payload | latency | host CPU |
/// |---------|---------|----------|
/// | 64 B    | 1.85    | 4.00     |
/// | 4 KiB   | 2.17    | 3.96     |
/// | 64 KiB  | 0.99    | 1.18     |
/// | 128 KiB | 0.74    | 0.89     |
/// | 256 KiB | 0.50    | 0.56     |
/// | 512 KiB | 0.34    | 0.36     |
/// | 1 MiB   | 0.23    | 0.23     |
///
/// At 64 KiB latency is level and CPU still favours the socket; by 128 KiB both
/// have turned. So the socket carries everything up to and including 64 KiB.
///
/// Two things make that more than a tuned constant. **Both metrics agree on where
/// the crossover is**, so the split is not a trade of latency against CPU that
/// would need a policy argument to settle. And **64 KiB is the size this project
/// already calls "a frame's worth of draw commands"** in
/// `contracts/apple/transport-probe.schema.json`, so the rule reads as *commands
/// over the socket, textures over the scheme* — a sentence somebody can re-derive
/// when the hardware changes, rather than a number they would have to trust.
///
/// Records: `docs/performance/apple/g0/transport/`.
public enum MigoFrameChannel: String, Sendable, Equatable, CaseIterable {
    /// A persistent connection. Low fixed cost, grows with bytes.
    case loopbackWebSocket = "loopback_websocket"

    /// A request per packet. Higher fixed cost, nearly flat in bytes.
    case schemeRequest = "scheme_request"
}

public enum MigoFrameChannelPolicy {

    /// The largest packet the socket carries, in bytes.
    ///
    /// Inclusive: 64 KiB itself goes over the socket, because at exactly that
    /// size latency was level (0.99) and host CPU still favoured the socket
    /// (1.18×). The first size where both metrics favour the scheme is the next
    /// class up.
    public static let socketCeilingBytes = 64 * 1024

    /// Which channel carries a packet of this size.
    ///
    /// A pure function of one number, deliberately. Everything else that could
    /// enter the decision -- how busy the socket is, how large the last packet
    /// was, what the frame rate is -- would make the choice depend on history,
    /// and a transport that changes channel for reasons the producer cannot see
    /// is a transport whose latency nobody can attribute. If a later measurement
    /// shows the threshold should move, it is one number in one place.
    public static func channel(forPacketOf byteCount: Int) -> MigoFrameChannel {
        byteCount <= socketCeilingBytes ? .loopbackWebSocket : .schemeRequest
    }

    /// What the measurement said this packet costs on each channel, in
    /// milliseconds, or nil where no class was measured near it.
    ///
    /// Present so a host can report why it chose what it chose. A transport that
    /// makes an unexplained choice per frame is one nobody can debug from a log,
    /// and the numbers are already in the record -- carrying them here costs a
    /// table and removes a question.
    public static func measuredRoundTripMilliseconds(
        forPacketOf byteCount: Int, on channel: MigoFrameChannel
    ) -> Double? {
        // The measured classes, and only those: interpolating between them would
        // invent numbers no device produced. A packet between two classes gets
        // the answer for the class at or below it, which is the honest reading of
        // "what was measured near here".
        let socket: [(Int, Double)] = [
            (64, 0.216), (4096, 0.180), (65536, 0.425),
            (131_072, 0.603), (262_144, 1.000), (524_288, 1.878), (1_048_576, 4.071),
        ]
        let scheme: [(Int, Double)] = [
            (64, 0.400), (4096, 0.390), (65536, 0.420),
            (131_072, 0.445), (262_144, 0.500), (524_288, 0.635), (1_048_576, 0.935),
        ]
        let table = channel == .loopbackWebSocket ? socket : scheme
        guard byteCount >= table[0].0 else { return nil }
        var answer: Double?
        for (size, milliseconds) in table where size <= byteCount {
            answer = milliseconds
        }
        return answer
    }
}

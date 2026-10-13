import Foundation

/// What a game view's debugging panel shows while the debugging aids are on
/// (`migo.setEnableDebug`, the platform's vConsole): the frames a second the
/// display clock delivered, and the latest lines content wrote to its console.
/// The views draw it; this keeps it, so the two views keep it alike.
package struct MigoDebugAids {
    /// Lines kept: the panel shows the latest, as the session's own buffer
    /// keeps its latest.
    package static let lineLimit = 200

    package private(set) var lines: [MigoEngineSession.ConsoleEntry] = []
    package private(set) var framesPerSecond: Double?
    private var lastSample: (delivered: Int, time: TimeInterval)?

    package init() {}

    /// Lines content wrote, oldest first.
    package mutating func append<Lines: Sequence>(_ entries: Lines)
    where Lines.Element == MigoEngineSession.ConsoleEntry {
        lines.append(contentsOf: entries)
        if lines.count > Self.lineLimit { lines.removeFirst(lines.count - Self.lineLimit) }
    }

    /// The display clock's count of delivered frames at `time` (seconds on a
    /// monotonic clock); the rate is the change since the last sample.
    package mutating func sample(delivered: Int, at time: TimeInterval) {
        if let last = lastSample, time > last.time, delivered >= last.delivered {
            framesPerSecond = Double(delivered - last.delivered) / (time - last.time)
        }
        lastSample = (delivered, time)
    }

    /// The panel's first line.
    package var header: String {
        framesPerSecond.map { String(format: "%.1f frames/s", $0) } ?? "-- frames/s"
    }

    /// A line as the panel writes it: its level's mark, then the text.
    package static func text(of entry: MigoEngineSession.ConsoleEntry) -> String {
        let mark: String
        switch entry.level {
        case 3: mark = "E"
        case 2: mark = "W"
        case 0: mark = "D"
        default: mark = "I"
        }
        return "\(mark) \(entry.message)"
    }
}

import CoreLocation
import Foundation
import MigoAppleRenderer

/// The location calls waiting on CoreLocation's fixes, and when each is
/// answered: at the first fresh fix as accurate as it asked for, or at its
/// deadline with the best fix it saw -- a fresh one before one the system
/// remembered, then the more accurate. Fed fixes and the time, it says what to
/// answer, so it is tested without a location.
struct MigoLocationRequests {
    struct Request {
        let reply: MigoHostServiceReply
        /// A fresh fix at least this accurate, in metres, answers at once; 0
        /// waits for the deadline and the best fix by then.
        let goal: CLLocationAccuracy
        let deadline: Date
        /// The answer carries altitude.
        let altitude: Bool
        fileprivate(set) var best: CLLocation?
    }

    /// Older than this a fix is the system's memory of where the device was,
    /// not where it is: kept only for want of a fresh one.
    static let freshness: TimeInterval = 60

    private(set) var pending: [Request] = []

    var isEmpty: Bool { pending.isEmpty }

    /// When the next request runs out of time.
    var nextDeadline: Date? { pending.map(\.deadline).min() }

    mutating func add(_ request: Request) {
        pending.append(request)
    }

    /// A fix arrived: the requests it answers now, with it. The rest keep it if
    /// it is the best they have seen.
    mutating func offer(_ fix: CLLocation, now: Date) -> [(Request, CLLocation)] {
        guard fix.horizontalAccuracy >= 0 else { return [] }
        let fresh = Self.isFresh(fix, now: now)
        var answered: [(Request, CLLocation)] = []
        var waiting: [Request] = []
        for var request in pending {
            if fresh, request.goal > 0, fix.horizontalAccuracy <= request.goal {
                answered.append((request, fix))
            } else {
                if Self.isBetter(fix, than: request.best, now: now) { request.best = fix }
                waiting.append(request)
            }
        }
        pending = waiting
        return answered
    }

    /// The requests whose time ran out, each with the best fix it saw, if any.
    mutating func expire(now: Date) -> [(Request, CLLocation?)] {
        var expired: [(Request, CLLocation?)] = []
        pending.removeAll { request in
            guard request.deadline <= now else { return false }
            expired.append((request, request.best))
            return true
        }
        return expired
    }

    /// Every request, for an answer that is the same for all.
    mutating func drain() -> [Request] {
        defer { pending = [] }
        return pending
    }

    static func isFresh(_ fix: CLLocation, now: Date) -> Bool {
        now.timeIntervalSince(fix.timestamp) <= freshness
    }

    static func isBetter(_ fix: CLLocation, than best: CLLocation?, now: Date) -> Bool {
        guard let best else { return true }
        let fresh = isFresh(fix, now: now)
        if fresh != isFresh(best, now: now) { return fresh }
        return fix.horizontalAccuracy < best.horizontalAccuracy
    }

    /// getLocation's answer, in WGS-84 as CoreLocation reports it: the engine
    /// converts to GCJ-02 when content asked. A speed CoreLocation does not
    /// know (negative) is 0, as is altitude when it was not asked for.
    static func answer(_ fix: CLLocation, altitude: Bool) -> [String: Any] {
        [
            "latitude": fix.coordinate.latitude,
            "longitude": fix.coordinate.longitude,
            "speed": max(0, fix.speed),
            "accuracy": fix.horizontalAccuracy,
            "altitude": altitude ? fix.altitude : 0.0,
            "verticalAccuracy": altitude ? max(0, fix.verticalAccuracy) : 0.0,
            "horizontalAccuracy": fix.horizontalAccuracy,
        ]
    }

    /// getFuzzyLocation's answer: the position only, which the engine rounds.
    static func fuzzyAnswer(_ fix: CLLocation) -> [String: Any] {
        ["latitude": fix.coordinate.latitude, "longitude": fix.coordinate.longitude]
    }
}

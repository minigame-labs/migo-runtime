import Foundation

/// iBeacons as content's Bluetooth API describes them, kept apart from
/// CoreLocation so the rules are testable: what a start names, and the beacons
/// ranged so far.
enum MigoBeacons {
    /// The proximity UUIDs a `startBeaconDiscovery` names; `nil` when it names
    /// none, or one that is not a UUID.
    static func uuids(_ options: [String: Any]) -> [UUID]? {
        guard let named = options["uuids"] as? [Any], !named.isEmpty else { return nil }
        var uuids: [UUID] = []
        for value in named {
            guard let text = value as? String, let uuid = UUID(uuidString: text) else { return nil }
            if !uuids.contains(uuid) { uuids.append(uuid) }
        }
        return uuids
    }

    /// One beacon in range, as `onBeaconUpdate` and `getBeacons` report it.
    struct Sighting: Equatable {
        var uuid: UUID
        var major: Int
        var minor: Int
        /// CoreLocation's proximity, whose numbers are the platform's: 0
        /// unknown, 1 immediate, 2 near, 3 far.
        var proximity: Int
        /// Metres, or -1 where it could not be estimated.
        var accuracy: Double
        var rssi: Int

        var record: [String: Any] {
            [
                "uuid": uuid.uuidString,
                "major": major,
                "minor": minor,
                "proximity": proximity,
                "accuracy": accuracy,
                "rssi": rssi,
            ]
        }
    }

    /// The beacons in range for each proximity UUID being ranged: each ranging
    /// reports its whole set, so a new report replaces its UUID's.
    struct Table {
        private var byUUID: [UUID: [Sighting]] = [:]
        /// The order the UUIDs were named in, so the list keeps its order.
        private var order: [UUID] = []

        mutating func update(_ uuid: UUID, _ sightings: [Sighting]) {
            if byUUID[uuid] == nil { order.append(uuid) }
            byUUID[uuid] = sightings
        }

        mutating func remove(_ uuid: UUID) {
            byUUID[uuid] = nil
            order.removeAll { $0 == uuid }
        }

        mutating func clear() {
            byUUID = [:]
            order = []
        }

        /// Every beacon in range, as the platform's list.
        var records: [[String: Any]] {
            order.flatMap { byUUID[$0] ?? [] }.map(\.record)
        }
    }
}

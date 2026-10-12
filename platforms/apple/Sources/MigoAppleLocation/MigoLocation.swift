import CoreLocation
import Foundation
import MigoAppleRenderer
import MigoEngine

/// Location for games, read with CoreLocation: `getLocation` and
/// `getFuzzyLocation`, the app's location grant and the location services
/// switch (`getAppAuthorizeSetting`, `getSystemSetting`), and
/// `scope.userLocation` decided by the system's own prompt.
///
/// An app opts in by linking this product, passing a `MigoLocation()` among its
/// game view's capabilities, and declaring why it uses location:
/// `NSLocationWhenInUseUsageDescription`, and on a Mac
/// `NSLocationUsageDescription` (with the location entitlement when
/// sandboxed). Without a purpose string the system shows no prompt, so the
/// scope is refused without asking.
///
/// On iOS it also ranges iBeacons for the Bluetooth service's beacon methods
/// (`MigoBeaconRanging`): CoreLocation ranges them, under the same grant.
///
/// Answers are WGS-84 as CoreLocation reports them; the engine converts to
/// GCJ-02 when content asks and rounds a fuzzy position. A fuzzy position comes
/// from a manager CoreLocation reports reduced accuracy to, so it is coarse
/// even when the app may read a precise one.
///
/// Create it on the main thread; it is used there.
public final class MigoLocation: MigoSystemCapability {
    /// `scripts/gen-api-platform-matrix.py` reads `services` below to count
    /// these APIs as provided on the Apple platforms: one service per line.
    public static let services: Set<MigoHostServiceID> = {
        var builtIn: Set<MigoHostServiceID> = []
        builtIn.insert(.location)
        return builtIn
    }()

    public var services: Set<MigoHostServiceID> { Self.services }
    public var scopes: Set<MigoScope> { [MigoScope(MIGO_SCOPE_USER_LOCATION)] }

    /// How long a call waits for a fix unless it asked for longer.
    static let timeout: TimeInterval = 10

    private let precise = CLLocationManager()
    /// getFuzzyLocation's own manager: CoreLocation reduces what it reports to
    /// it whatever the app is granted.
    private let coarse = CLLocationManager()
    private let delegate = Delegate()
    private var preciseRequests = MigoLocationRequests()
    private var fuzzyRequests = MigoLocationRequests()
    private var deadline: Timer?
    private var reports: MigoCapabilityReports?
    /// Content's scope requests waiting on the system's prompt.
    private var scopeWaiters: [(Bool) -> Void] = []
    /// The location services switch as last read: off fails a call at once.
    private var servicesEnabled = true
    #if os(iOS)
        /// Ranges iBeacons, apart from the managers that read position.
        private let beaconManager = CLLocationManager()
        /// The proximity UUIDs being ranged, and what each has in range.
        private var ranging: [UUID] = []
        private var beacons = MigoBeacons.Table()
    #endif

    public init() {
        precise.desiredAccuracy = kCLLocationAccuracyHundredMeters
        coarse.desiredAccuracy = kCLLocationAccuracyReduced
        delegate.owner = self
        precise.delegate = delegate
        coarse.delegate = delegate
        #if os(iOS)
            beaconManager.delegate = delegate
        #endif
    }

    deinit {
        precise.stopUpdatingLocation()
        coarse.stopUpdatingLocation()
        deadline?.invalidate()
        #if os(iOS)
            stopRanging()
        #endif
    }

    // MARK: - reports

    public func report(to reports: MigoCapabilityReports?) {
        self.reports = reports
        guard reports != nil else {
            // The session closed: what it was waiting for is answered nowhere.
            fail(preciseRequests.drain() + fuzzyRequests.drain(), "the game ended")
            idle()
            let waiters = scopeWaiters
            scopeWaiters = []
            waiters.forEach { $0(false) }
            #if os(iOS)
                stopRanging()
            #endif
            return
        }
        reportState()
    }

    private func reportState() {
        guard let reports else { return }
        let status = precise.authorizationStatus
        let reduced = Self.isAuthorized(status) && precise.accuracyAuthorization == .reducedAccuracy
        reports.updateAppAuthorization {
            $0.location = Self.authorization(status)
            $0.location_reduced_accuracy = reduced ? 1 : 0
        }
        reports.setScopeState(MigoScope(MIGO_SCOPE_USER_LOCATION), Self.scopeState(status))
        // CoreLocation asks that the switch be read off the main thread.
        DispatchQueue.global(qos: .utility).async { [weak self] in
            let on = CLLocationManager.locationServicesEnabled()
            DispatchQueue.main.async {
                guard let self else { return }
                self.servicesEnabled = on
                let flag = MigoSystemSettingFlags(MIGO_SYSTEM_SETTING_FLAG_LOCATION_ENABLED)
                self.reports?.setSystemSetting(flag, enabled: on)
            }
        }
    }

    /// Granted, for as long as the app is used or always. A Mac grants only
    /// "always" (`authorized`).
    static func isAuthorized(_ status: CLAuthorizationStatus) -> Bool {
        #if os(iOS)
            return status == .authorizedAlways || status == .authorizedWhenInUse
        #else
            return status == .authorizedAlways
        #endif
    }

    static func authorization(_ status: CLAuthorizationStatus) -> UInt8 {
        if isAuthorized(status) { return UInt8(MIGO_AUTHORIZATION_AUTHORIZED) }
        switch status {
        case .denied, .restricted: return UInt8(MIGO_AUTHORIZATION_DENIED)
        default: return UInt8(MIGO_AUTHORIZATION_NOT_DETERMINED)
        }
    }

    static func scopeState(_ status: CLAuthorizationStatus) -> MigoScopeState {
        if isAuthorized(status) { return MigoScopeState(MIGO_SCOPE_STATE_GRANTED) }
        switch status {
        case .denied, .restricted: return MigoScopeState(MIGO_SCOPE_STATE_DENIED)
        default: return MigoScopeState(MIGO_SCOPE_STATE_UNKNOWN)
        }
    }

    // MARK: - the scope

    public func requestScope(_ scope: MigoScope, completion: @escaping (Bool) -> Void) {
        guard scope == MigoScope(MIGO_SCOPE_USER_LOCATION) else {
            completion(false)
            return
        }
        let status = precise.authorizationStatus
        guard status == .notDetermined else {
            // Decided already: the player changes it in Settings, not here.
            reportState()
            completion(Self.isAuthorized(status))
            return
        }
        guard Self.declaresPurpose else {
            NSLog(
                "MigoLocation: the app declares no purpose for location in its Info.plist, so the system shows "
                    + "no prompt and scope.userLocation is refused")
            completion(false)
            return
        }
        scopeWaiters.append(completion)
        if scopeWaiters.count == 1 { precise.requestWhenInUseAuthorization() }
    }

    /// The Info.plist keys the system's prompt needs, one of them present.
    static var declaresPurpose: Bool {
        #if os(macOS)
            let keys = ["NSLocationUsageDescription", "NSLocationWhenInUseUsageDescription"]
        #else
            let keys = ["NSLocationWhenInUseUsageDescription"]
        #endif
        return keys.contains { (Bundle.main.object(forInfoDictionaryKey: $0) as? String)?.isEmpty == false }
    }

    fileprivate func authorizationChanged(_ manager: CLLocationManager) {
        // Both managers hear the one grant; the precise one speaks for it.
        guard manager === precise else { return }
        let status = precise.authorizationStatus
        reportState()
        if !Self.isAuthorized(status) {
            fail(preciseRequests.drain() + fuzzyRequests.drain(), "system permission denied")
            idle()
            #if os(iOS)
                // Ranging stops with the grant; content hears the service go.
                if !ranging.isEmpty {
                    stopRanging()
                    postBeaconService(available: false)
                }
            #endif
        }
        guard status != .notDetermined, !scopeWaiters.isEmpty else { return }
        let waiters = scopeWaiters
        scopeWaiters = []
        waiters.forEach { $0(Self.isAuthorized(status)) }
    }

    // MARK: - calls

    public func handle(_ request: MigoHostServiceRequest) {
        guard let reply = request.reply else { return }
        let options = request.object
        let now = Date()
        switch request.method {
        case MigoHostServiceID.Location.getLocation:
            let highAccuracy = options["isHighAccuracy"] as? Bool ?? false
            let expireMs = (options["highAccuracyExpireTime"] as? NSNumber)?.doubleValue ?? 0
            // A high-accuracy call given 3 s or more waits it out for the best
            // fix in that time; any other is answered by the first good one.
            let waitsItOut = highAccuracy && expireMs >= 3000
            start(
                MigoLocationRequests.Request(
                    reply: reply, goal: waitsItOut ? 0 : (highAccuracy ? 20 : 100),
                    deadline: now.addingTimeInterval(waitsItOut ? expireMs / 1000 : Self.timeout),
                    altitude: options["altitude"] as? Bool ?? false),
                fuzzy: false, highAccuracy: highAccuracy)
        case MigoHostServiceID.Location.getFuzzyLocation:
            start(
                MigoLocationRequests.Request(
                    reply: reply, goal: .greatestFiniteMagnitude, deadline: now.addingTimeInterval(Self.timeout),
                    altitude: false),
                fuzzy: true, highAccuracy: false)
        default:
            reply.fail("not supported")
        }
    }

    private func start(_ request: MigoLocationRequests.Request, fuzzy: Bool, highAccuracy: Bool) {
        guard Self.isAuthorized(precise.authorizationStatus) else {
            request.reply.fail("system permission denied")
            return
        }
        guard servicesEnabled else {
            request.reply.fail("location service disabled")
            return
        }
        if fuzzy {
            fuzzyRequests.add(request)
            coarse.startUpdatingLocation()
        } else {
            preciseRequests.add(request)
            if highAccuracy { precise.desiredAccuracy = kCLLocationAccuracyBest }
            precise.startUpdatingLocation()
        }
        scheduleDeadline()
    }

    fileprivate func received(_ fixes: [CLLocation], from manager: CLLocationManager) {
        let now = Date()
        for fix in fixes {
            if manager === coarse {
                for (request, fix) in fuzzyRequests.offer(fix, now: now) {
                    request.reply.succeed(MigoLocationRequests.fuzzyAnswer(fix))
                }
            } else {
                for (request, fix) in preciseRequests.offer(fix, now: now) {
                    request.reply.succeed(MigoLocationRequests.answer(fix, altitude: request.altitude))
                }
            }
        }
        idle()
        scheduleDeadline()
    }

    fileprivate func failed(_ error: Error, from manager: CLLocationManager) {
        // Not yet: CoreLocation keeps trying, and the deadline decides.
        if (error as? CLError)?.code == .locationUnknown { return }
        let reason = (error as? CLError)?.code == .denied ? "system permission denied" : "unable to get location"
        fail(manager === coarse ? fuzzyRequests.drain() : preciseRequests.drain(), reason)
        idle()
        scheduleDeadline()
    }

    private func expire() {
        let now = Date()
        for (request, best) in fuzzyRequests.expire(now: now) {
            if let best {
                request.reply.succeed(MigoLocationRequests.fuzzyAnswer(best))
            } else {
                request.reply.fail("unable to get location")
            }
        }
        for (request, best) in preciseRequests.expire(now: now) {
            if let best {
                request.reply.succeed(MigoLocationRequests.answer(best, altitude: request.altitude))
            } else {
                request.reply.fail("unable to get location")
            }
        }
        idle()
        scheduleDeadline()
    }

    private func fail(_ requests: [MigoLocationRequests.Request], _ reason: String) {
        for request in requests { request.reply.fail(reason) }
    }

    /// A manager with nobody waiting stops: CoreLocation runs only while a call
    /// does.
    private func idle() {
        if preciseRequests.isEmpty {
            precise.stopUpdatingLocation()
            precise.desiredAccuracy = kCLLocationAccuracyHundredMeters
        }
        if fuzzyRequests.isEmpty { coarse.stopUpdatingLocation() }
        if preciseRequests.isEmpty, fuzzyRequests.isEmpty {
            deadline?.invalidate()
            deadline = nil
        }
    }

    private func scheduleDeadline() {
        deadline?.invalidate()
        deadline = nil
        let next = [preciseRequests.nextDeadline, fuzzyRequests.nextDeadline].compactMap { $0 }.min()
        guard let next else { return }
        let timer = Timer(fire: next, interval: 0, repeats: false) { [weak self] _ in self?.expire() }
        RunLoop.main.add(timer, forMode: .common)
        deadline = timer
    }

    #if os(iOS)
        // MARK: - iBeacons

        private typealias Beacon = MigoHostServiceID.Bluetooth

        private func stopRanging() {
            for uuid in ranging {
                beaconManager.stopRangingBeacons(satisfying: CLBeaconIdentityConstraint(uuid: uuid))
            }
            ranging = []
            beacons.clear()
        }

        private func postBeaconService(available: Bool) {
            reports?.postEvent(
                .bluetooth, event: Beacon.Event.beaconServiceChange,
                object: ["available": available, "discovering": !ranging.isEmpty])
        }

        /// Range the beacons `uuids` name, in place of what was being ranged.
        private func range(_ uuids: [UUID]) {
            for uuid in ranging where !uuids.contains(uuid) {
                beaconManager.stopRangingBeacons(satisfying: CLBeaconIdentityConstraint(uuid: uuid))
                beacons.remove(uuid)
            }
            for uuid in uuids where !ranging.contains(uuid) {
                beaconManager.startRangingBeacons(satisfying: CLBeaconIdentityConstraint(uuid: uuid))
            }
            ranging = uuids
            postBeaconService(available: true)
        }

        fileprivate func ranged(_ found: [CLBeacon], for constraint: CLBeaconIdentityConstraint) {
            guard ranging.contains(constraint.uuid) else { return }
            beacons.update(
                constraint.uuid,
                found.map {
                    MigoBeacons.Sighting(
                        uuid: $0.uuid, major: $0.major.intValue, minor: $0.minor.intValue,
                        proximity: $0.proximity.rawValue, accuracy: $0.accuracy, rssi: $0.rssi)
                })
            reports?.postEvent(.bluetooth, event: Beacon.Event.beaconUpdate, object: ["beacons": beacons.records])
        }

        fileprivate func rangingFailed(for constraint: CLBeaconIdentityConstraint, _ error: Error) {
            guard ranging.contains(constraint.uuid) else { return }
            NSLog("MigoLocation: ranging beacons \(constraint.uuid) failed: \(error)")
            beaconManager.stopRangingBeacons(satisfying: constraint)
            ranging.removeAll { $0 == constraint.uuid }
            beacons.remove(constraint.uuid)
            postBeaconService(available: false)
        }
    #endif

    /// CoreLocation's delegate, apart so the capability's interface is its own.
    private final class Delegate: NSObject, CLLocationManagerDelegate {
        weak var owner: MigoLocation?

        func locationManagerDidChangeAuthorization(_ manager: CLLocationManager) {
            owner?.authorizationChanged(manager)
        }

        func locationManager(_ manager: CLLocationManager, didUpdateLocations locations: [CLLocation]) {
            owner?.received(locations, from: manager)
        }

        func locationManager(_ manager: CLLocationManager, didFailWithError error: Error) {
            owner?.failed(error, from: manager)
        }

        #if os(iOS)
            func locationManager(
                _ manager: CLLocationManager, didRange beacons: [CLBeacon],
                satisfying constraint: CLBeaconIdentityConstraint
            ) {
                owner?.ranged(beacons, for: constraint)
            }

            func locationManager(
                _ manager: CLLocationManager, didFailRangingFor constraint: CLBeaconIdentityConstraint,
                error: Error
            ) {
                owner?.rangingFailed(for: constraint, error)
            }
        #endif
    }
}

#if os(iOS)
    extension MigoLocation: MigoBeaconRanging {
        /// `startBeaconDiscovery` ranges the UUIDs it names -- asking for the
        /// location grant first if the player has not decided -- in place of
        /// what was being ranged; `stopBeaconDiscovery` stops; `getBeacons`
        /// answers what is in range. Bluetooth being off is not read here
        /// (that is CoreBluetooth's, and its purpose string): ranging then
        /// finds nothing.
        public func handleBeacons(_ request: MigoHostServiceRequest) {
            guard let reply = request.reply else { return }
            switch request.method {
            case Beacon.startBeaconDiscovery:
                guard let uuids = MigoBeacons.uuids(request.object) else {
                    reply.fail(MigoBeaconError.invalidData)
                    return
                }
                guard CLLocationManager.isRangingAvailable() else {
                    reply.fail(MigoBeaconError.unsupported)
                    return
                }
                guard servicesEnabled else {
                    reply.fail(MigoBeaconError.locationUnavailable)
                    return
                }
                requestScope(MigoScope(MIGO_SCOPE_USER_LOCATION)) { [weak self] granted in
                    guard let self, granted else {
                        reply.fail(MigoBeaconError.locationUnavailable)
                        return
                    }
                    self.range(uuids)
                    reply.succeed()
                }
            case Beacon.stopBeaconDiscovery:
                if !ranging.isEmpty {
                    stopRanging()
                    postBeaconService(available: true)
                }
                reply.succeed()
            case Beacon.getBeacons:
                reply.succeed(["beacons": beacons.records])
            default:
                reply.fail("not supported")
            }
        }
    }
#endif

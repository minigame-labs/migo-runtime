import Foundation
import MigoEngine

/// A part of the system an app opts into by linking the product that reads it
/// -- location, the camera, the photo library -- rather than one the SDK
/// always carries: App Store review asks for a purpose string for every such
/// API a binary references, used or not, so the core SDK references none.
///
/// A capability answers its host services as any provider does, behind the
/// app's own: a service the app names is the app's. It reports what the system
/// says about it -- the app's grant, the system's switch, the standing decision
/// for the scopes it backs -- and decides those scopes with the system's own
/// prompt when content asks (`authorize`), unless the app answers the
/// permission service itself. Main thread.
public protocol MigoSystemCapability: MigoHostServiceProvider {
    /// Content's scopes this capability decides with the system's prompt:
    /// `MIGO_SCOPE_*` numbers.
    var scopes: Set<MigoScope> { get }

    /// Report through `reports` what the system says about this capability --
    /// now, and at each change until the next call. `nil` when the session
    /// closes.
    func report(to reports: MigoCapabilityReports?)

    /// Content asked for one of `scopes`. Ask the player through the system's
    /// prompt if the system has not decided, report the scope's state, then
    /// call `completion` with whether it is granted -- in that order, so
    /// content that acts on the grant finds it recorded.
    func requestScope(_ scope: MigoScope, completion: @escaping (Bool) -> Void)
}

/// A capability that saves to the system's photo library: what the SDK's image
/// service hands `saveImageToPhotosAlbum` to, the rest of that service being
/// the SDK's own. Main thread.
public protocol MigoPhotoLibrarySaving: MigoSystemCapability {
    /// Save the image at `file` -- a file content may read, lent until
    /// `completion` -- to the library: `nil` once saved, else why not.
    func saveImageToPhotoLibrary(_ file: URL, completion: @escaping (String?) -> Void)
}

/// A capability that ranges iBeacons: what the Bluetooth service's beacon
/// methods -- `startBeaconDiscovery`, `stopBeaconDiscovery`, `getBeacons` --
/// are handed to, the rest of that service being Bluetooth's. On Apple
/// platforms a beacon is ranged by CoreLocation, under the location grant, not
/// by CoreBluetooth; content still holds `scope.bluetooth` for it, which the
/// engine checks. Its events are the Bluetooth service's `BEACON_UPDATE` and
/// `BEACON_SERVICE_CHANGE`. Main thread.
public protocol MigoBeaconRanging: MigoSystemCapability {
    /// One of the three beacon methods, answered with the common platform's
    /// codes (`MigoBeaconError`) when it fails.
    func handleBeacons(_ request: MigoHostServiceRequest)
}

/// The common mini-game platform's codes for a beacon call that failed.
public enum MigoBeaconError: Int32 {
    case unsupported = 11000
    case bluetoothUnavailable = 11001
    case locationUnavailable = 11002
    case alreadyStarted = 11003
    case notStarted = 11004
    case systemError = 11005
    case invalidData = 11006

    public var message: String {
        switch self {
        case .unsupported: return "unsupport"
        case .bluetoothUnavailable: return "bluetooth service is unavailable"
        case .locationUnavailable: return "location service is unavailable"
        case .alreadyStarted: return "already start"
        case .notStarted: return "not startBeaconDiscovery"
        case .systemError: return "system error"
        case .invalidData: return "invalid data"
        }
    }
}

extension MigoHostServiceReply {
    /// A beacon call failed with `error`'s code and words.
    public func fail(_ error: MigoBeaconError) {
        fail(error.message, code: error.rawValue)
    }
}

/// What a capability reports through, for one session: the fields of the
/// app's grants and the system switches it owns, and -- while the SDK decides
/// content's scopes, the app answering no permission service of its own -- the
/// standing decision for its scopes. Main thread.
public final class MigoCapabilityReports {
    private weak var session: MigoEngineSession?
    /// Whether the scope decisions reported here are the ones content's calls
    /// are checked against. When the app answers the permission service, its
    /// decisions are, and these are not recorded.
    public let decidesScopes: Bool

    init(session: MigoEngineSession, decidesScopes: Bool) {
        self.session = session
        self.decidesScopes = decidesScopes
    }

    /// One system switch: `MIGO_SYSTEM_SETTING_FLAG_*`.
    public func setSystemSetting(_ flag: MigoSystemSettingFlags, enabled: Bool) {
        session?.setSystemSetting(flag, enabled: enabled)
    }

    /// The app's grant fields this capability owns, each a `MIGO_AUTHORIZATION_*`.
    public func updateAppAuthorization(_ update: (inout MigoAppAuthorizeSetting) -> Void) {
        session?.updateAppAuthorization(update)
    }

    /// The standing decision for one of the capability's scopes.
    public func setScopeState(_ scope: MigoScope, _ state: MigoScopeState) {
        guard decidesScopes else { return }
        session?.setScopeState(scope, state)
    }

    /// One of the capability's service's events -- a `MIGO_<SERVICE>_EVENT_*`
    /// with the JSON object the header documents for it.
    public func postEvent(_ service: MigoHostServiceID, event: UInt32, object: [String: Any]) {
        guard let json = try? JSONSerialization.data(withJSONObject: object) else { return }
        session?.postHostServiceEvent(service, event: event, json: json)
    }

    /// One characteristic value a connected peripheral sent, typed rather than
    /// JSON because a peripheral may notify a hundred times a second.
    public func postBleCharacteristicValue(
        deviceId: String, serviceId: String, characteristicId: String, value: Data
    ) {
        session?.postBleCharacteristicValue(
            deviceId: deviceId, serviceId: serviceId, characteristicId: characteristicId, value: value)
    }
}

/// The permission service, answered for the scopes the linked capabilities
/// decide: each asks the system. A scope no capability decides is the app's to
/// decide, and without the app's own permission service content is refused.
final class MigoSystemPermission {
    private let deciders: [MigoScope: MigoSystemCapability]

    /// `nil` when no capability decides a scope: there is nothing to answer.
    init?(capabilities: [MigoSystemCapability]) {
        var deciders: [MigoScope: MigoSystemCapability] = [:]
        for capability in capabilities {
            for scope in capability.scopes where deciders[scope] == nil {
                deciders[scope] = capability
            }
        }
        guard !deciders.isEmpty else { return nil }
        self.deciders = deciders
    }

    func handle(_ request: MigoHostServiceRequest) {
        guard let reply = request.reply else { return }
        guard request.method == MigoHostServiceID.Permission.requestScope else {
            reply.fail("not supported")
            return
        }
        guard let number = (request.object["scopeId"] as? NSNumber)?.uint32Value,
            let decider = deciders[MigoScope(number)]
        else {
            reply.fail("auth deny")
            return
        }
        decider.requestScope(MigoScope(number)) { granted in
            if granted { reply.succeed() } else { reply.fail("auth deny") }
        }
    }
}

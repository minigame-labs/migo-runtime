import Foundation

/// What content can reach in the WebKit Full lane, and who gives it.
///
/// The mirror of `contracts/apple/webkit-full-surface.json`, checked against it by
/// `scripts/test-apple-webkit-surface-contract.sh`. The contract carries the
/// reasoning; this type carries the decisions, and it is here rather than in the
/// shipping package because the shipping package does not resolve until an hour of
/// Skia has produced its xcframework -- so logic placed there is logic no pull
/// request compiles.
///
/// The distinction the whole type is built around is **provenance**. App Review
/// 4.7.2 is about exposing *native* APIs to mini games, and in this lane most of
/// what content uses is not native: WebKit runs the JavaScript, so canvas, WebGL,
/// Web Audio, storage, fetch and touch events arrive with the web platform whether
/// or not this product has an opinion about them. A surface that treated those as
/// switches would describe controls that do not exist, and would report a
/// compliance posture nothing enforces.
public struct MigoWebKitSurface: Sendable, Equatable {

    /// The lane this surface belongs to, as the contract names it.
    ///
    /// A string and not `MigoRuntimeProfile`'s numeric raw value: this is what the
    /// host reports to content and to its own telemetry, and a number would make
    /// every collected diagnostic depend on a mapping nobody shipped.
    public static let lane = "ios_webkit_full"

    /// How content would reach a capability. Four answers, because they are four
    /// different facts and collapsing any two of them produces a wrong report.
    public enum Provenance: String, Sendable, CaseIterable, Codable {
        /// WebKit gives it to any page. The host has no switch and claims none.
        case webPlatform = "web_platform"
        /// A web-platform capability WebKit asks the host to allow, through a
        /// delegate callback. The host really does hold this one.
        case hostGranted = "host_granted"
        /// A native call across the script-message boundary. 4.7.2 applies here.
        case hostBridge = "host_bridge"
        /// The host serves the origin content loads from, so every request content
        /// makes to it is answered by host code. Native, and reached by loading a
        /// URL rather than by calling a method -- which is why it is not a bridge
        /// row and has no method name.
        case hostOrigin = "host_origin"
        /// Neither route exists. There is no request to refuse.
        case absent
    }

    /// The closed set of capabilities the lane has an answer for.
    ///
    /// Closed on purpose: a capability nobody declared cannot be reached, and a
    /// bridge method nobody declared is refused by name rather than ignored. An
    /// open set would make "content asked for something we never considered"
    /// indistinguishable from "content asked for something we turned off".
    public enum Capability: String, Sendable, CaseIterable, Codable {
        case canvas2D = "canvas_2d"
        case webGL = "webgl"
        case webAudio = "web_audio"
        case pointerInput = "pointer_input"
        case webStorage = "web_storage"
        case webNetwork = "web_network"
        case contentOrigin = "content_origin"
        case lifecycle
        case environment
        case diagnostics
        case camera
        case microphone
        case geolocation
        case clipboardWrite = "clipboard_write"
        case clipboardRead = "clipboard_read"
        case bluetooth
        case payment
        case motionSensors = "motion_sensors"
        case photoLibrary = "photo_library"
        case pushNotifications = "push_notifications"
        case share
        case advertising
        case login
    }

    /// Which direction a bridge method runs in.
    ///
    /// Declared rather than chosen at the call site, because the two are different
    /// shapes at both ends: a call returns a promise the host settles, and a
    /// subscription registers a listener the host later invokes. A method declared
    /// as one and built as the other is a promise nobody settles or a listener
    /// nobody calls, and both present as content that hangs rather than as an error.
    public enum BridgeKind: String, Sendable, CaseIterable, Codable {
        case call
        case subscription
    }

    /// One row of the tier.
    public struct Entry: Sendable, Equatable {
        public let capability: Capability
        public let provenance: Provenance
        /// Whether a host that configured nothing gets it.
        public let defaultEnabled: Bool
        /// The script message this capability answers, for `hostBridge` only.
        public let bridgeMethod: String?
        /// Which direction it runs in, for `hostBridge` only.
        public let bridgeKind: BridgeKind?

        /// Whether a host can decline to offer it.
        ///
        /// What the host holds is what it can withhold: the bridge methods it
        /// installs, and the permission callbacks WebKit asks it to answer. WebKit's
        /// own capabilities and the origin the content is loaded from are neither --
        /// the first is not ours and the second is what a session is.
        public var isWithholdable: Bool {
            provenance == .hostBridge || provenance == .hostGranted
        }

        init(
            _ capability: Capability, _ provenance: Provenance, defaultEnabled: Bool,
            bridgeMethod: String? = nil, bridgeKind: BridgeKind? = nil
        ) {
            self.capability = capability
            self.provenance = provenance
            self.defaultEnabled = defaultEnabled
            self.bridgeMethod = bridgeMethod
            self.bridgeKind = bridgeKind
        }
    }

    /// The tier, in the contract's order.
    public static let entries: [Entry] = [
        Entry(.canvas2D, .webPlatform, defaultEnabled: true),
        Entry(.webGL, .webPlatform, defaultEnabled: true),
        Entry(.webAudio, .webPlatform, defaultEnabled: true),
        Entry(.pointerInput, .webPlatform, defaultEnabled: true),
        Entry(.webStorage, .webPlatform, defaultEnabled: true),
        Entry(.webNetwork, .webPlatform, defaultEnabled: true),
        Entry(.contentOrigin, .hostOrigin, defaultEnabled: true),
        Entry(
            .lifecycle, .hostBridge, defaultEnabled: true,
            bridgeMethod: "lifecycle.observe", bridgeKind: .subscription),
        Entry(
            .environment, .hostBridge, defaultEnabled: true,
            bridgeMethod: "environment.get", bridgeKind: .call),
        Entry(
            .diagnostics, .hostBridge, defaultEnabled: true,
            bridgeMethod: "diagnostics.report", bridgeKind: .call),
        Entry(.camera, .hostGranted, defaultEnabled: false),
        Entry(.microphone, .hostGranted, defaultEnabled: false),
        Entry(.geolocation, .hostGranted, defaultEnabled: false),
        Entry(.clipboardWrite, .webPlatform, defaultEnabled: true),
        Entry(.clipboardRead, .absent, defaultEnabled: false),
        Entry(.bluetooth, .absent, defaultEnabled: false),
        Entry(.payment, .absent, defaultEnabled: false),
        Entry(.motionSensors, .hostGranted, defaultEnabled: false),
        Entry(.photoLibrary, .absent, defaultEnabled: false),
        Entry(.pushNotifications, .absent, defaultEnabled: false),
        Entry(.share, .absent, defaultEnabled: false),
        Entry(.advertising, .absent, defaultEnabled: false),
        Entry(.login, .absent, defaultEnabled: false),
    ]

    public static func entry(for capability: Capability) -> Entry {
        // Total by construction, and checked by a test that walks
        // `Capability.allCases`: a case added to the enum without a row here would
        // otherwise be a capability with no provenance, which every decision below
        // would have to invent an answer for.
        guard let found = entries.first(where: { $0.capability == capability }) else {
            preconditionFailure(
                "\(capability.rawValue) has no row in the WebKit Full surface. Every capability "
                    + "the enum can name must state where content would reach it from")
        }
        return found
    }

    /// Why a surface could not be built as asked.
    ///
    /// Both directions have an impossibility, and neither is a no-op. A host that
    /// asked for the camera and got silence would ship believing it had one; a host
    /// that asked to withhold WebGL and got silence would ship believing content
    /// cannot reach the GPU.
    public enum ConfigurationError: Error, CustomStringConvertible, Equatable {
        case notReachable(Capability)
        case notWithholdable(Capability)
        case contradictory(Capability)

        public var description: String {
            switch self {
            case .notReachable(let capability):
                return
                    "\(capability.rawValue) cannot be enabled: it is absent from this lane, which "
                    + "means neither a web API nor a bridge method reaches it. Turning it on would "
                    + "produce a surface that says yes and a runtime with nothing to call. A "
                    + "capability that needs Apple's permission is default-off and enablable; one "
                    + "that has no mechanism is neither, and contracts/apple/webkit-full-surface."
                    + "json says which each is"
            case .notWithholdable(let capability):
                return
                    "\(capability.rawValue) cannot be withheld: it is not a switch the host holds. "
                    + "WebKit gives its own capabilities to any page it loads, and the content "
                    + "origin is what the lane loads content from -- a session without it loads "
                    + "nothing. Accepting the request would record a "
                    + "compliance posture the runtime does not enforce, which is worse than "
                    + "refusing it -- the content would still have the capability and the surface "
                    + "would say otherwise"
            case .contradictory(let capability):
                return
                    "\(capability.rawValue) was asked for and withheld in the same "
                    + "configuration. Picking one would be picking for the caller"
            }
        }
    }

    /// The capabilities this surface hands to content.
    public let enabled: Set<Capability>

    /// The default surface: everything the web platform gives, plus the four bridge
    /// methods that expose no user permission and no radio, and nothing else.
    public static let `default` = MigoWebKitSurface(
        enabled: Set(entries.filter(\.defaultEnabled).map(\.capability)))

    private init(enabled: Set<Capability>) {
        self.enabled = enabled
    }

    /// A surface the host app has adjusted, in both directions.
    ///
    /// Expressed as changes against the default rather than as a full set, because
    /// a full set invites a host to write one that omits WebGL and believe it has
    /// withheld it. What is removable is what the host actually holds: the bridge
    /// methods it installs, and the permission callbacks WebKit asks it about.
    public static func configured(
        enabling additional: Set<Capability> = [],
        withholding removed: Set<Capability> = []
    ) throws -> MigoWebKitSurface {
        // Sorted so a configuration with several problems reports the same one
        // every time; a message that changes between runs is a message somebody
        // stops trusting.
        for capability in additional.intersection(removed).sorted(by: keyOrder) {
            throw ConfigurationError.contradictory(capability)
        }
        for capability in additional.sorted(by: keyOrder) where entry(for: capability).provenance == .absent {
            throw ConfigurationError.notReachable(capability)
        }
        for capability in removed.sorted(by: keyOrder) where !entry(for: capability).isWithholdable {
            throw ConfigurationError.notWithholdable(capability)
        }
        return MigoWebKitSurface(
            enabled: `default`.enabled.union(additional).subtracting(removed))
    }

    private static func keyOrder(_ lhs: Capability, _ rhs: Capability) -> Bool {
        lhs.rawValue < rhs.rawValue
    }

    public func isEnabled(_ capability: Capability) -> Bool { enabled.contains(capability) }

    // MARK: - the bridge

    /// What the host does with a script message naming a method.
    public enum BridgeDecision: Sendable, Equatable {
        /// A declared, enabled bridge capability. The host serves it.
        case serve(Capability)
        /// A declared bridge capability the host has not enabled. Refused with a
        /// reason, so a host app debugging its content learns it is a
        /// configuration answer rather than a missing feature.
        case refuseDisabled(Capability)
        /// No capability declares this method. Refused by name: a message nobody
        /// declared and a message somebody turned off are different failures, and
        /// silently dropping either is how content waits forever for a reply.
        case refuseUnknown(String)
    }

    public func decision(forBridgeMethod method: String) -> BridgeDecision {
        guard
            let entry = Self.entries.first(where: {
                $0.bridgeMethod == method && $0.provenance == .hostBridge
            })
        else {
            return .refuseUnknown(method)
        }
        return isEnabled(entry.capability) ? .serve(entry.capability) : .refuseDisabled(entry.capability)
    }

    /// Every method this surface will serve. What the host installs message
    /// handlers for, derived rather than listed: a handler installed for a method
    /// no capability declares is a native API reachable from content and absent
    /// from the compliance surface, which is the one failure 4.7.2 punishes.
    public var servedBridgeMethods: [String] {
        Self.entries
            .filter { $0.provenance == .hostBridge && isEnabled($0.capability) }
            .compactMap(\.bridgeMethod)
            .sorted()
    }

    // MARK: - what WebKit asks the host

    /// A capability WebKit will grant only if the host says so.
    public enum HostGrantedRequest: Sendable, Equatable {
        case camera
        case microphone
        case cameraAndMicrophone
        case geolocation
        case motionSensors

        var capabilities: [Capability] {
            switch self {
            case .camera: return [.camera]
            case .microphone: return [.microphone]
            case .cameraAndMicrophone: return [.camera, .microphone]
            case .geolocation: return [.geolocation]
            case .motionSensors: return [.motionSensors]
            }
        }
    }

    /// The answer to a WebKit permission callback.
    ///
    /// A combined camera-and-microphone request needs both, because granting the
    /// pair when only one is enabled hands over a capability the surface says is
    /// off -- and WebKit asks for the pair as one question.
    public func grants(_ request: HostGrantedRequest) -> Bool {
        request.capabilities.allSatisfy(isEnabled)
    }
}

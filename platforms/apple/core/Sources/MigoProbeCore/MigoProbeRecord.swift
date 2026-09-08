import Foundation

/// One measurement from the G0 performance matrix, in the shape
/// `contracts/apple/performance-probe.schema.json` requires.
///
/// WHY THIS TYPE EXISTS RATHER THAN A DICTIONARY. The decision tool rejects the
/// whole run when one field is missing, and the run is a lab session on borrowed
/// devices. A dictionary assembled at the call site fails that check hours after
/// the phones have gone back in the drawer; a struct fails it at compile time.
///
/// WHY THE CODING KEYS ARE WRITTEN OUT. `convertToSnakeCase` would produce the
/// same JSON today and would silently produce different JSON the moment somebody
/// renames a property -- and the failure surfaces as "the decision tool says the
/// field is missing", which reads like a tool bug. Written out, the wire name is
/// the thing being edited, and `scripts/test-apple-probe-record-contract.sh`
/// derives the required list from the schema and checks every one of them
/// appears here. The two halves cannot drift without the gate saying which one
/// moved.
public struct MigoProbeRecord: Codable, Sendable, Equatable {
    // MARK: the run

    public var schemaVersion: Int
    public var runId: String
    public var trialSeed: UInt64
    public var capturedAt: String

    // MARK: what it ran on
    //
    // Every one of these is in the schema's `held_fixed` list, which is what
    // makes "one variable at a time" checkable: two records that differ in any
    // of them are not two samples of the same question, and the decision tool
    // puts them in different comparisons rather than averaging them.

    public var deviceClass: MigoProbeDeviceClass
    public var hardwareIdentifier: String
    public var ramBytes: UInt64
    public var osVersion: String
    public var osBuild: String
    public var webkitBuild: String
    public var appBuild: String

    // MARK: the arm

    public var agent: MigoProbeAgent
    public var layout: MigoProbeLayout
    public var origin: MigoProbeOrigin
    public var transport: MigoProbeTransport
    public var transportOwner: MigoProbeTransportOwner
    public var clock: MigoProbeClock
    public var payloadClassBytes: Int
    public var refreshHz: Int
    public var durationSeconds: Int

    // MARK: the device state during the run

    public var thermalState: MigoProbeThermalState
    public var powerState: MigoProbePowerState

    /// A24: Lockdown Mode disables JIT for web content, so this is measured on
    /// the device rather than assumed from the platform. It is held fixed, so a
    /// Lockdown-Mode sample can never be averaged with a JIT one.
    public var jitEnabled: Bool

    // MARK: what was measured

    public var inputToPresentP50Ms: Double
    public var inputToPresentP95Ms: Double
    public var inputToPresentP99Ms: Double
    public var missedVsyncRatio: Double
    public var cpuSeconds: Double
    public var wakeups: Int
    public var appFootprintBytes: UInt64
    public var gpuBytes: UInt64
    public var copiesPerFrame: Int
    public var allocationsPerFrame: Int

    public var errors: Int
    public var terminations: Int

    /// The hash of what the arm actually drew. An arm whose pixels differ from
    /// the expected hash is excluded rather than ranked: a transport that is
    /// fast because it dropped frames is not a faster transport.
    public var correctnessHash: String
    public var correctnessExpectedHash: String

    // MARK: optional

    public var webcontentFootprintBytes: UInt64?
    public var notes: String?

    public init(
        schemaVersion: Int = 1,
        runId: String,
        trialSeed: UInt64,
        capturedAt: String,
        deviceClass: MigoProbeDeviceClass,
        hardwareIdentifier: String,
        ramBytes: UInt64,
        osVersion: String,
        osBuild: String,
        webkitBuild: String,
        appBuild: String,
        agent: MigoProbeAgent,
        layout: MigoProbeLayout,
        origin: MigoProbeOrigin,
        transport: MigoProbeTransport,
        transportOwner: MigoProbeTransportOwner,
        clock: MigoProbeClock,
        payloadClassBytes: Int,
        refreshHz: Int,
        durationSeconds: Int,
        thermalState: MigoProbeThermalState,
        powerState: MigoProbePowerState,
        jitEnabled: Bool,
        inputToPresentP50Ms: Double,
        inputToPresentP95Ms: Double,
        inputToPresentP99Ms: Double,
        missedVsyncRatio: Double,
        cpuSeconds: Double,
        wakeups: Int,
        appFootprintBytes: UInt64,
        gpuBytes: UInt64,
        copiesPerFrame: Int,
        allocationsPerFrame: Int,
        errors: Int,
        terminations: Int,
        correctnessHash: String,
        correctnessExpectedHash: String,
        webcontentFootprintBytes: UInt64? = nil,
        notes: String? = nil
    ) {
        self.schemaVersion = schemaVersion
        self.runId = runId
        self.trialSeed = trialSeed
        self.capturedAt = capturedAt
        self.deviceClass = deviceClass
        self.hardwareIdentifier = hardwareIdentifier
        self.ramBytes = ramBytes
        self.osVersion = osVersion
        self.osBuild = osBuild
        self.webkitBuild = webkitBuild
        self.appBuild = appBuild
        self.agent = agent
        self.layout = layout
        self.origin = origin
        self.transport = transport
        self.transportOwner = transportOwner
        self.clock = clock
        self.payloadClassBytes = payloadClassBytes
        self.refreshHz = refreshHz
        self.durationSeconds = durationSeconds
        self.thermalState = thermalState
        self.powerState = powerState
        self.jitEnabled = jitEnabled
        self.inputToPresentP50Ms = inputToPresentP50Ms
        self.inputToPresentP95Ms = inputToPresentP95Ms
        self.inputToPresentP99Ms = inputToPresentP99Ms
        self.missedVsyncRatio = missedVsyncRatio
        self.cpuSeconds = cpuSeconds
        self.wakeups = wakeups
        self.appFootprintBytes = appFootprintBytes
        self.gpuBytes = gpuBytes
        self.copiesPerFrame = copiesPerFrame
        self.allocationsPerFrame = allocationsPerFrame
        self.errors = errors
        self.terminations = terminations
        self.correctnessHash = correctnessHash
        self.correctnessExpectedHash = correctnessExpectedHash
        self.webcontentFootprintBytes = webcontentFootprintBytes
        self.notes = notes
    }

    public enum CodingKeys: String, CodingKey, CaseIterable {
        case schemaVersion = "schema_version"
        case runId = "run_id"
        case trialSeed = "trial_seed"
        case capturedAt = "captured_at"
        case deviceClass = "device_class"
        case hardwareIdentifier = "hardware_identifier"
        case ramBytes = "ram_bytes"
        case osVersion = "os_version"
        case osBuild = "os_build"
        case webkitBuild = "webkit_build"
        case appBuild = "app_build"
        case agent
        case layout
        case origin
        case transport
        case transportOwner = "transport_owner"
        case clock
        case payloadClassBytes = "payload_class_bytes"
        case refreshHz = "refresh_hz"
        case durationSeconds = "duration_s"
        case thermalState = "thermal_state"
        case powerState = "power_state"
        case jitEnabled = "jit_enabled"
        case inputToPresentP50Ms = "input_to_present_p50_ms"
        case inputToPresentP95Ms = "input_to_present_p95_ms"
        case inputToPresentP99Ms = "input_to_present_p99_ms"
        case missedVsyncRatio = "missed_vsync_ratio"
        case cpuSeconds = "cpu_seconds"
        case wakeups
        case appFootprintBytes = "app_footprint_bytes"
        case gpuBytes = "gpu_bytes"
        case copiesPerFrame = "copies_per_frame"
        case allocationsPerFrame = "allocations_per_frame"
        case errors
        case terminations
        case correctnessHash = "correctness_hash"
        case correctnessExpectedHash = "correctness_expected_hash"
        case webcontentFootprintBytes = "webcontent_footprint_bytes"
        case notes
    }
}

// MARK: - the closed enums
//
// Each mirrors one entry of the schema's `record.enums`, raw value for raw
// value. They are closed for the reason the schema states: an unrecognised
// value is a run that measured something the decision cannot name, and
// bucketing it with the nearest known value is how a hybrid transport's numbers
// end up attributed to the loopback one.

/// Where the measurement was taken. `simulator` records are written and then
/// excluded by the decision tool -- kept rather than suppressed, because a
/// simulator run is how the harness itself is debugged and deleting the record
/// would hide that the harness ran at all.
public enum MigoProbeDeviceClass: String, Codable, Sendable, CaseIterable {
    case device
    case simulator
}

/// Where content JavaScript runs.
///
/// A21 is why a Window arm is still measured: synchronous XHR is a second
/// public blocking primitive, so "Window cannot serve synchronous content" is a
/// claim the capability gate settles, not a premise this matrix may assume.
public enum MigoProbeAgent: String, Codable, Sendable, CaseIterable {
    case window
    case dedicatedWorker = "dedicated_worker"
}

/// The `WKWebView` host shape.
public enum MigoProbeLayout: String, Codable, Sendable, CaseIterable {
    case attachedVisible = "attached_visible"
    case transparentOverlay = "transparent_overlay"
    case oneByOne = "one_by_one"
    case offScreen = "off_screen"
    case occluded
}

/// The page origin, which A5 constrains and which is not the same question as
/// the data channel.
public enum MigoProbeOrigin: String, Codable, Sendable, CaseIterable {
    case loopback
    case customScheme = "custom_scheme"
}

/// The wire mechanism.
public enum MigoProbeTransport: String, Codable, Sendable, CaseIterable {
    case loopbackWebsocket = "loopback_websocket"
    case schemeRequest = "scheme_request"
    /// A21's arm. Leaving it out of the enum would have made the one
    /// measurement that settles A21 unrecordable.
    case syncXhrRpc = "sync_xhr_rpc"
    case hybrid
}

/// Which JavaScript context owns the transport.
///
/// A22 is why this is a dimension of its own. `WebSocket` is exposed to
/// Workers, so a Window relay is a consequence of a Worker blocked in
/// `Atomics.wait` being unable to service its own socket -- not an API
/// requirement. Without this, all three topologies record as
/// `dedicated_worker` + `loopback_websocket`, and two different architectures
/// are pooled into one arm.
public enum MigoProbeTransportOwner: String, Codable, Sendable, CaseIterable {
    case sameContext = "same_context"
    case windowRelay = "window_relay"
    case ioWorker = "io_worker"
}

/// What drives the frame clock.
public enum MigoProbeClock: String, Codable, Sendable, CaseIterable {
    case workerRaf = "worker_raf"
    case windowRafRelay = "window_raf_relay"
    case displayLinkRelay = "display_link_relay"
}

/// `ProcessInfo.thermalState`, recorded on every sample.
///
/// Deliberately not in the schema's `held_fixed` list: it varies across the
/// samples of one arm by nature, and holding it fixed would split every arm
/// below the sample floor and reject every run.
public enum MigoProbeThermalState: String, Codable, Sendable, CaseIterable {
    case nominal
    case fair
    case serious
    case critical
}

/// Power source and Low Power Mode, which move the CPU governor and are
/// therefore part of the condition a comparison is made under.
public enum MigoProbePowerState: String, Codable, Sendable, CaseIterable {
    case plugged
    case battery
    case lowPower = "low_power"
}

// MARK: - encoding

extension MigoProbeRecord {
    /// The canonical encoder for probe output.
    ///
    /// Sorted keys so two runs of the same harness produce byte-identical files
    /// and a diff between them is a difference in the measurement rather than
    /// in dictionary ordering. No `convertToSnakeCase`: the wire names are in
    /// `CodingKeys`, which is the half the contract gate reads.
    public static func makeEncoder() -> JSONEncoder {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys, .prettyPrinted]
        return encoder
    }
}

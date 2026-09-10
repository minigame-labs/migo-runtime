import Foundation

/// One transport measurement, in the shape
/// `contracts/apple/transport-probe.schema.json` requires.
///
/// A separate type from `MigoCapabilityRecord` because the answers have
/// different shapes and one type holding both would make each field optional
/// for the other's sake. A capability answers with a state and a sentence; this
/// answers with a distribution, and a distribution stored in an `answer` object
/// would make `state` mean "a measurement happened".
///
/// The three latency fields are optional, and null is a load-bearing value here
/// rather than a missing one: measured 2026-09-10 on iPhone 12 / iOS 17.0.3,
/// `performance.now()` steps by 0.020 ms at the loopback origin and 1.000 ms at
/// the custom scheme, which is not cross-origin isolated. A round trip of a few
/// hundred microseconds cannot be resolved per-sample through a 1 ms clock, so
/// the percentiles are withheld rather than reported as the shape of the clock.
/// The mean survives, because a batch of hundreds takes hundreds of
/// milliseconds and the quantisation is a rounding error on the total.
public struct MigoTransportRecord: Codable, Sendable, Equatable {
    public var schemaVersion: Int
    public var runId: String
    public var capturedAt: String

    public var deviceClass: MigoProbeDeviceClass
    public var hardwareIdentifier: String
    public var ramBytes: UInt64
    public var osVersion: String
    public var osBuild: String
    public var webkitBuild: String
    public var appBuild: String
    public var lockdownMode: MigoLockdownMode

    /// `debug` or `release`, and required rather than optional.
    ///
    /// Half of what a transport number measures is the host's own code: the
    /// loopback arm's server parses frames and unmasks every byte the page
    /// sends, in this repository, while the custom-scheme arm's server is
    /// WebKit's machinery plus a copy. Measured 2026-09-10 with a debug host, a
    /// mebibyte round-tripped in 252 ms over the socket and 6.4 ms over the
    /// scheme -- four megabytes a second is not a socket's speed, it is an
    /// unoptimised loop's, and that number would have eliminated an arm.
    ///
    /// A missing field reads as `release` to anyone hoping it does, so there is
    /// no default.
    public var hostBuildConfiguration: String
    public var origin: MigoProbeOrigin

    /// Which channel, by the same names the candidate space uses. A string
    /// rather than an enum, deliberately: the page decides what it could reach,
    /// and a name this build does not know is better recorded than dropped --
    /// the contract's enum is what refuses it, in one place, where the refusal
    /// can say so.
    public var transport: String
    public var payloadBytes: Int
    public var samples: Int
    public var clockStepMs: Double
    public var meanRoundTripMs: Double?
    public var p50RoundTripMs: Double?
    public var p95RoundTripMs: Double?
    public var p99RoundTripMs: Double?

    /// Samples that did not complete. Non-zero with a low `samples` count is a
    /// batch that gave up, and the two together say which.
    public var errors: Int
    public var note: String?

    public init(
        schemaVersion: Int = 1,
        runId: String,
        capturedAt: String,
        deviceClass: MigoProbeDeviceClass,
        hardwareIdentifier: String,
        ramBytes: UInt64,
        osVersion: String,
        osBuild: String,
        webkitBuild: String,
        appBuild: String,
        lockdownMode: MigoLockdownMode,
        hostBuildConfiguration: String,
        origin: MigoProbeOrigin,
        transport: String,
        payloadBytes: Int,
        samples: Int,
        clockStepMs: Double,
        meanRoundTripMs: Double?,
        p50RoundTripMs: Double?,
        p95RoundTripMs: Double?,
        p99RoundTripMs: Double?,
        errors: Int,
        note: String? = nil
    ) {
        self.schemaVersion = schemaVersion
        self.runId = runId
        self.capturedAt = capturedAt
        self.deviceClass = deviceClass
        self.hardwareIdentifier = hardwareIdentifier
        self.ramBytes = ramBytes
        self.osVersion = osVersion
        self.osBuild = osBuild
        self.webkitBuild = webkitBuild
        self.appBuild = appBuild
        self.lockdownMode = lockdownMode
        self.hostBuildConfiguration = hostBuildConfiguration
        self.origin = origin
        self.transport = transport
        self.payloadBytes = payloadBytes
        self.samples = samples
        self.clockStepMs = clockStepMs
        self.meanRoundTripMs = meanRoundTripMs
        self.p50RoundTripMs = p50RoundTripMs
        self.p95RoundTripMs = p95RoundTripMs
        self.p99RoundTripMs = p99RoundTripMs
        self.errors = errors
        self.note = note
    }

    public enum CodingKeys: String, CodingKey, CaseIterable {
        case schemaVersion = "schema_version"
        case runId = "run_id"
        case capturedAt = "captured_at"
        case deviceClass = "device_class"
        case hardwareIdentifier = "hardware_identifier"
        case ramBytes = "ram_bytes"
        case osVersion = "os_version"
        case osBuild = "os_build"
        case webkitBuild = "webkit_build"
        case appBuild = "app_build"
        case lockdownMode = "lockdown_mode"
        case hostBuildConfiguration = "host_build_configuration"
        case origin
        case transport
        case payloadBytes = "payload_bytes"
        case samples
        case clockStepMs = "clock_step_ms"
        case meanRoundTripMs = "mean_round_trip_ms"
        case p50RoundTripMs = "p50_round_trip_ms"
        case p95RoundTripMs = "p95_round_trip_ms"
        case p99RoundTripMs = "p99_round_trip_ms"
        case errors
        case note
    }

    /// The three latency fields encode as explicit nulls rather than being
    /// omitted.
    ///
    /// `encodeIfPresent` would drop them, and an absent key and a null read the
    /// same to a person and differently to a validator -- the schema lists them
    /// as required and nullable, so a dropped key is a record that fails its own
    /// contract. It is the same rule the capability record follows for an
    /// unanswered capability, applied to a number.
    public func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(schemaVersion, forKey: .schemaVersion)
        try container.encode(runId, forKey: .runId)
        try container.encode(capturedAt, forKey: .capturedAt)
        try container.encode(deviceClass, forKey: .deviceClass)
        try container.encode(hardwareIdentifier, forKey: .hardwareIdentifier)
        try container.encode(ramBytes, forKey: .ramBytes)
        try container.encode(osVersion, forKey: .osVersion)
        try container.encode(osBuild, forKey: .osBuild)
        try container.encode(webkitBuild, forKey: .webkitBuild)
        try container.encode(appBuild, forKey: .appBuild)
        try container.encode(lockdownMode, forKey: .lockdownMode)
        try container.encode(hostBuildConfiguration, forKey: .hostBuildConfiguration)
        try container.encode(origin, forKey: .origin)
        try container.encode(transport, forKey: .transport)
        try container.encode(payloadBytes, forKey: .payloadBytes)
        try container.encode(samples, forKey: .samples)
        try container.encode(clockStepMs, forKey: .clockStepMs)
        try container.encode(meanRoundTripMs, forKey: .meanRoundTripMs)
        try container.encode(p50RoundTripMs, forKey: .p50RoundTripMs)
        try container.encode(p95RoundTripMs, forKey: .p95RoundTripMs)
        try container.encode(p99RoundTripMs, forKey: .p99RoundTripMs)
        try container.encode(errors, forKey: .errors)
        try container.encodeIfPresent(note, forKey: .note)
    }
}

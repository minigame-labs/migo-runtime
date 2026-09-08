import Foundation

/// One capability-gate result, in the shape
/// `contracts/apple/capability-probe.schema.json` requires.
///
/// Measurement gate 1 runs with no renderer and no validator, so it produces
/// none of the numbers the performance record is built around. It produces
/// answers, and the answers cut the candidate set every later gate draws from:
/// an arm no device supports has to leave the matrix before it is benchmarked,
/// because benchmarking an unavailable arm benchmarks whatever ran instead.
public struct MigoCapabilityRecord: Codable, Sendable, Equatable {
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

    /// A24. iOS exposes no public query for Lockdown Mode, so a probe that
    /// cannot establish it says `unknown` rather than assuming the common case.
    /// `unknown` and `off` are different answers and the difference decides
    /// whether a JIT result generalises.
    public var lockdownMode: MigoLockdownMode

    /// Which origin this record is about.
    ///
    /// Half the questions below are answered by the origin and not by the
    /// device -- secure context, cross-origin isolation, and therefore whether
    /// SharedArrayBuffer constructs at all. One record per device would hold
    /// two answers under one key, and the way that resolves in practice is that
    /// whichever origin ran last wins silently.
    public var origin: MigoProbeOrigin

    /// Every capability in the schema's closed set, answered. A capability that
    /// was not probed is present with `.notProbed`, never absent: an absent key
    /// and a `false` read the same to a human and differently to a decision.
    public var capabilities: [MigoProbeCapability: MigoCapabilityAnswer]

    public var notes: String?

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
        origin: MigoProbeOrigin,
        capabilities: [MigoProbeCapability: MigoCapabilityAnswer],
        notes: String? = nil
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
        self.origin = origin
        self.capabilities = capabilities
        self.notes = notes
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
        case origin
        case capabilities
        case notes
    }

    /// `capabilities` is written as a plain object keyed by the capability's
    /// wire name. Swift encodes a `[Enum: Value]` dictionary as a flat array of
    /// alternating keys and values, which is valid JSON, is not what the schema
    /// describes, and would be found on lab day.
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
        try container.encode(origin, forKey: .origin)
        try container.encodeIfPresent(notes, forKey: .notes)

        var answers = container.nestedContainer(
            keyedBy: MigoProbeCapability.self, forKey: .capabilities)
        for capability in MigoProbeCapability.allCases {
            let answer = capabilities[capability] ?? MigoCapabilityAnswer.notProbed(
                evidence: "the harness did not include this capability in the run")
            try answers.encode(answer, forKey: capability)
        }
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        schemaVersion = try container.decode(Int.self, forKey: .schemaVersion)
        runId = try container.decode(String.self, forKey: .runId)
        capturedAt = try container.decode(String.self, forKey: .capturedAt)
        deviceClass = try container.decode(MigoProbeDeviceClass.self, forKey: .deviceClass)
        hardwareIdentifier = try container.decode(String.self, forKey: .hardwareIdentifier)
        ramBytes = try container.decode(UInt64.self, forKey: .ramBytes)
        osVersion = try container.decode(String.self, forKey: .osVersion)
        osBuild = try container.decode(String.self, forKey: .osBuild)
        webkitBuild = try container.decode(String.self, forKey: .webkitBuild)
        appBuild = try container.decode(String.self, forKey: .appBuild)
        lockdownMode = try container.decode(MigoLockdownMode.self, forKey: .lockdownMode)
        origin = try container.decode(MigoProbeOrigin.self, forKey: .origin)
        notes = try container.decodeIfPresent(String.self, forKey: .notes)

        let answers = try container.nestedContainer(
            keyedBy: MigoProbeCapability.self, forKey: .capabilities)
        var decoded: [MigoProbeCapability: MigoCapabilityAnswer] = [:]
        for capability in MigoProbeCapability.allCases {
            decoded[capability] = try answers.decode(
                MigoCapabilityAnswer.self, forKey: capability)
        }
        capabilities = decoded
    }
}

/// Whether Lockdown Mode was in force. See A24.
public enum MigoLockdownMode: String, Codable, Sendable, CaseIterable {
    case off
    case on
    case unknown
}

/// The closed set of questions gate 1 asks.
///
/// Each one settles a numbered assumption in the plan's ledger. The set is
/// closed for the same reason the performance enums are: a capability name the
/// decision cannot recognise is a probe measuring something nothing consumes,
/// and accepting it silently reports coverage that does not exist.
public enum MigoProbeCapability: String, CodingKey, Codable, Sendable, CaseIterable {
    /// A24. Probed, not inferred: Lockdown Mode can turn JIT off under an
    /// otherwise identical OS and device. Origin-independent, and recorded in
    /// both origins' records so that a disagreement is visible rather than
    /// averaged away.
    case jitEnabled = "jit_enabled"
    /// A5/A6/A23. Whether this origin is a secure context.
    case secureContext = "secure_context"
    /// A7/A23. `crossOriginIsolated === true` with COOP and COEP actually served.
    case crossOriginIsolated = "cross_origin_isolated"
    /// A15. Constructible in a Worker at this origin. Downstream of
    /// cross-origin isolation and recorded separately, because the two have
    /// been observed to disagree.
    case sharedArrayBuffer = "shared_array_buffer"
    /// A21. That `Atomics.wait` blocks and wakes, not merely that it exists.
    case atomicsWait = "atomics_wait"
    /// A21. A synchronous XHR in a Worker returning a binary body. This is what
    /// decides whether Window is excluded on capability grounds at all.
    case syncXhrBinary = "sync_xhr_binary"
    /// A22. `WebSocket` constructible and connectable from a Worker.
    case websocketInWorker = "websocket_in_worker"
    /// A18. Feature detection with a measured answer instead of a guess.
    case workerRaf = "worker_raf"
    /// A5. A POST body arriving intact at the other side of this origin's
    /// transport. WebKit 191362 is RESOLVED FIXED, so this measures shipping
    /// behaviour rather than restating a bug report.
    case requestBodyDelivery = "request_body_delivery"
    /// A6/G0.3. That this origin works WITHOUT the local-network alert.
    /// Phrased as the absence of the prompt so that `available` means "usable"
    /// for every capability in the set, with no entry whose polarity an
    /// admission rule has to special-case.
    case noLocalNetworkPrompt = "no_local_network_prompt"
}

/// One capability's answer.
///
/// `evidence` is required on every answer including the yeses. "true" with no
/// evidence is indistinguishable from a probe that returned a default, and this
/// repository has already recorded a lane that reported success while checking
/// nothing.
public struct MigoCapabilityAnswer: Codable, Sendable, Equatable {
    public var state: MigoCapabilityState
    /// What was observed, in one sentence. Not a restatement of `state`: the
    /// expression that was evaluated, the error that came back, or the reason
    /// the question could not be asked here.
    public var evidence: String
    /// The observed value where the answer has one -- a latency, a byte count,
    /// a build string. Absent where the question is a yes or no.
    public var value: String?

    public init(state: MigoCapabilityState, evidence: String, value: String? = nil) {
        self.state = state
        self.evidence = evidence
        self.value = value
    }

    public static func available(evidence: String, value: String? = nil) -> MigoCapabilityAnswer {
        MigoCapabilityAnswer(state: .available, evidence: evidence, value: value)
    }

    public static func unavailable(evidence: String) -> MigoCapabilityAnswer {
        MigoCapabilityAnswer(state: .unavailable, evidence: evidence)
    }

    public static func unsupported(evidence: String) -> MigoCapabilityAnswer {
        MigoCapabilityAnswer(state: .unsupported, evidence: evidence)
    }

    public static func notProbed(evidence: String) -> MigoCapabilityAnswer {
        MigoCapabilityAnswer(state: .notProbed, evidence: evidence)
    }

    public enum CodingKeys: String, CodingKey, CaseIterable {
        case state
        case evidence
        case value
    }
}

/// How an answer came out.
///
/// The three ways of saying no are kept apart on purpose. `unavailable` is a
/// probe that ran and got no. `unsupported` is an API that is not there to
/// fail. `notProbed` is nobody asking -- the one that must never be quietly
/// read as no.
public enum MigoCapabilityState: String, Codable, Sendable, CaseIterable {
    case available
    case unavailable
    case unsupported
    case notProbed = "not_probed"
}

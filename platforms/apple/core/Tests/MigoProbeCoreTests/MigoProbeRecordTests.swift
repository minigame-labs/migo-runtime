import XCTest

@testable import MigoProbeCore

/// The probe records are checked against the contracts themselves, not against
/// a second copy of them written here.
///
/// A test that lists the expected field names is a third place the field names
/// live, and it agrees with whichever of the other two it was copied from. These
/// read `contracts/apple/*.json` off disk and compare, so the only way to pass
/// is for the encoder and the contract to actually agree.
final class MigoProbeRecordTests: XCTestCase {

    /// The repository root, derived from this file's own path.
    ///
    /// `swift test` does not promise a working directory, and the one it uses is
    /// not the repository root when the package is nested. Deriving it from
    /// `#filePath` is the only form that is correct from both `swift test` in
    /// the package directory and `xcodebuild` from anywhere else.
    private static var repositoryRoot: URL {
        var url = URL(fileURLWithPath: #filePath)
        // .../platforms/apple/core/Tests/MigoProbeCoreTests/<this file>
        for _ in 0..<6 { url.deleteLastPathComponent() }
        return url
    }

    private func loadContract(_ name: String) throws -> [String: Any] {
        let url = Self.repositoryRoot
            .appendingPathComponent("contracts/apple")
            .appendingPathComponent(name)
        let data = try Data(contentsOf: url)
        guard let object = try JSONSerialization.jsonObject(with: data) as? [String: Any] else {
            XCTFail("\(name) is not a JSON object")
            return [:]
        }
        return object
    }

    private func sampleRecord() -> MigoProbeRecord {
        MigoProbeRecord(
            runId: "r1",
            trialSeed: 20260909,
            capturedAt: "2026-09-09T00:00:00Z",
            deviceClass: .device,
            hardwareIdentifier: "iPhone14,5",
            ramBytes: 4 * 1024 * 1024 * 1024,
            osVersion: "18.2",
            osBuild: "22C150",
            webkitBuild: "620.1.2",
            appBuild: "1",
            agent: .dedicatedWorker,
            layout: .attachedVisible,
            origin: .loopback,
            transport: .loopbackWebsocket,
            transportOwner: .ioWorker,
            clock: .displayLinkRelay,
            payloadClassBytes: 32768,
            refreshHz: 60,
            durationSeconds: 1800,
            thermalState: .nominal,
            powerState: .plugged,
            jitEnabled: true,
            inputToPresentP50Ms: 8.0,
            inputToPresentP95Ms: 12.0,
            inputToPresentP99Ms: 16.0,
            missedVsyncRatio: 0.001,
            cpuSeconds: 30.0,
            wakeups: 1000,
            appFootprintBytes: 200 * 1024 * 1024,
            gpuBytes: 60 * 1024 * 1024,
            copiesPerFrame: 1,
            allocationsPerFrame: 0,
            errors: 0,
            terminations: 0,
            correctnessHash: "abc",
            correctnessExpectedHash: "abc"
        )
    }

    // MARK: - the performance record

    func testEncodedRecordCarriesEveryRequiredField() throws {
        let schema = try loadContract("performance-probe.schema.json")
        let record = schema["record"] as! [String: Any]
        let required = Set(record["required"] as! [String])

        let data = try MigoProbeRecord.makeEncoder().encode(sampleRecord())
        let encoded = try JSONSerialization.jsonObject(with: data) as! [String: Any]
        let produced = Set(encoded.keys)

        XCTAssertEqual(
            required.subtracting(produced), [],
            "the encoder omits fields the schema requires, so every record it writes "
                + "would reject the whole run")
    }

    func testEncoderInventsNothingTheContractDoesNotName() throws {
        let schema = try loadContract("performance-probe.schema.json")
        let record = schema["record"] as! [String: Any]
        let known = Set(record["required"] as! [String]).union(record["optional"] as! [String])

        let data = try MigoProbeRecord.makeEncoder().encode(sampleRecord())
        let encoded = try JSONSerialization.jsonObject(with: data) as! [String: Any]

        // An extra field is not harmless: the decision tool ignores it, so a
        // measurement written into one is silently dropped rather than refused.
        XCTAssertEqual(
            Set(encoded.keys).subtracting(known), [],
            "the encoder writes fields no contract names; a measurement placed in one "
                + "is read by nothing")
    }

    func testEveryEnumMatchesTheContractExactly() throws {
        let schema = try loadContract("performance-probe.schema.json")
        let record = schema["record"] as! [String: Any]
        let enums = record["enums"] as! [String: [String]]

        let swiftEnums: [String: [String]] = [
            "device_class": MigoProbeDeviceClass.allCases.map(\.rawValue),
            "agent": MigoProbeAgent.allCases.map(\.rawValue),
            "layout": MigoProbeLayout.allCases.map(\.rawValue),
            "origin": MigoProbeOrigin.allCases.map(\.rawValue),
            "transport": MigoProbeTransport.allCases.map(\.rawValue),
            "transport_owner": MigoProbeTransportOwner.allCases.map(\.rawValue),
            "clock": MigoProbeClock.allCases.map(\.rawValue),
            "thermal_state": MigoProbeThermalState.allCases.map(\.rawValue),
            "power_state": MigoProbePowerState.allCases.map(\.rawValue),
        ]

        // Both directions. A missing Swift case makes an arm unrecordable; a
        // missing contract entry makes a recorded arm rejected by the tool.
        XCTAssertEqual(
            Set(enums.keys), Set(swiftEnums.keys),
            "the contract and the Swift side do not agree on which fields are closed enums")
        for (field, allowed) in enums {
            XCTAssertEqual(
                Set(swiftEnums[field] ?? []), Set(allowed),
                "the \(field) enum differs between the contract and Swift")
        }
    }

    func testHeldFixedFieldsAreAllEncoded() throws {
        let schema = try loadContract("performance-probe.schema.json")
        let record = schema["record"] as! [String: Any]
        let held = Set(record["held_fixed"] as! [String])

        let data = try MigoProbeRecord.makeEncoder().encode(sampleRecord())
        let encoded = try JSONSerialization.jsonObject(with: data) as! [String: Any]

        // A held-fixed field the harness does not write is worse than one the
        // schema never named: the decision tool groups on `nil`, so every
        // record lands in the same condition and the check goes quiet.
        XCTAssertEqual(
            held.subtracting(Set(encoded.keys)), [],
            "a field the decision groups by is not written, so every record would group "
                + "together as if measured under one condition")
    }

    // MARK: - the capability record

    private func sampleCapabilityRecord() -> MigoCapabilityRecord {
        MigoCapabilityRecord(
            runId: "c1",
            capturedAt: "2026-09-09T00:00:00Z",
            deviceClass: .device,
            hardwareIdentifier: "iPhone14,5",
            ramBytes: 4 * 1024 * 1024 * 1024,
            osVersion: "18.2",
            osBuild: "22C150",
            webkitBuild: "620.1.2",
            appBuild: "1",
            lockdownMode: .off,
            origin: .loopback,
            capabilities: [
                .jitEnabled: .available(
                    evidence: "a 3e7-iteration loop ran 41x faster after warmup", value: "41.2")
            ]
        )
    }

    func testCapabilityRecordCarriesEveryRequiredField() throws {
        let schema = try loadContract("capability-probe.schema.json")
        let record = schema["record"] as! [String: Any]
        let required = Set(record["required"] as! [String])

        let data = try MigoProbeRecord.makeEncoder().encode(sampleCapabilityRecord())
        let encoded = try JSONSerialization.jsonObject(with: data) as! [String: Any]

        XCTAssertEqual(required.subtracting(Set(encoded.keys)), [])
    }

    func testEveryCapabilityIsAnsweredEvenWhenTheHarnessSkippedIt() throws {
        let schema = try loadContract("capability-probe.schema.json")
        let declared = Set(
            (schema["capabilities"] as! [String: Any]).keys.filter { !$0.hasPrefix("_") })

        let data = try MigoProbeRecord.makeEncoder().encode(sampleCapabilityRecord())
        let encoded = try JSONSerialization.jsonObject(with: data) as! [String: Any]
        let answers = encoded["capabilities"] as! [String: Any]

        // The sample answered exactly one. All eleven must still be present,
        // because an absent key and a `false` read the same to a human.
        XCTAssertEqual(
            Set(answers.keys), declared,
            "the record does not answer every capability the contract declares")

        let skipped = answers["shared_array_buffer"] as! [String: Any]
        XCTAssertEqual(
            skipped["state"] as? String, "not_probed",
            "a capability the harness skipped must say so, not read as a no")
        XCTAssertNotNil(skipped["evidence"], "an answer with no evidence is not an answer")
    }

    func testCapabilitiesEncodeAsAnObjectAndNotAnArray() throws {
        let data = try MigoProbeRecord.makeEncoder().encode(sampleCapabilityRecord())
        let encoded = try JSONSerialization.jsonObject(with: data) as! [String: Any]

        // Swift's default encoding for a dictionary with a non-String key is a
        // flat array of alternating keys and values. It is valid JSON, it is not
        // the shape the contract describes, and nothing before lab day would
        // have said so.
        XCTAssertTrue(
            encoded["capabilities"] is [String: Any],
            "capabilities encoded as \(type(of: encoded["capabilities"])), not an object")
    }

    func testCapabilityStatesAndLockdownMatchTheContract() throws {
        let schema = try loadContract("capability-probe.schema.json")
        let recordEnums = (schema["record"] as! [String: Any])["enums"] as! [String: [String]]
        let answerEnums = (schema["answer"] as! [String: Any])["enums"] as! [String: [String]]

        XCTAssertEqual(
            Set(MigoLockdownMode.allCases.map(\.rawValue)), Set(recordEnums["lockdown_mode"]!))
        XCTAssertEqual(
            Set(MigoCapabilityState.allCases.map(\.rawValue)), Set(answerEnums["state"]!))
        XCTAssertEqual(
            Set(MigoProbeCapability.allCases.map(\.rawValue)),
            Set((schema["capabilities"] as! [String: Any]).keys.filter { !$0.hasPrefix("_") }))
    }

    // MARK: - determinism

    func testEncodingIsStableAcrossRuns() throws {
        let encoder = MigoProbeRecord.makeEncoder()
        let first = try encoder.encode(sampleRecord())
        let second = try encoder.encode(sampleRecord())
        XCTAssertEqual(
            first, second,
            "two encodings of one record differ, so a diff between two runs would report "
                + "dictionary ordering as a change in the measurement")
    }
}

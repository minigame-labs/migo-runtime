import XCTest

@testable import MigoAppleCore

/// The WebKit Full lane's compliance surface, checked as behaviour.
///
/// `scripts/test-apple-webkit-surface-contract.sh` checks that this table says the
/// same thing as `contracts/apple/webkit-full-surface.json`. These tests check the
/// properties the table has to hold whatever it says -- the ones that make the
/// surface a control rather than a description of one.
final class MigoWebKitSurfaceTests: XCTestCase {

    private typealias Surface = MigoWebKitSurface
    private typealias Capability = MigoWebKitSurface.Capability

    func testEveryCapabilityTheEnumCanNameHasAProvenance() {
        // Walks the enum rather than the table. A case added without a row is a
        // capability every decision below would have to invent an answer for, and
        // the answer it would invent is whatever the lookup's default happened to
        // be.
        for capability in Capability.allCases {
            let entry = Surface.entry(for: capability)
            XCTAssertEqual(entry.capability, capability)
        }
        XCTAssertEqual(
            Surface.entries.count, Capability.allCases.count,
            "the table has a row the enum cannot name, so nothing can reach it and nothing "
                + "removes it")
    }

    func testOnlyBridgedCapabilitiesHaveABridgeMethod() {
        for entry in Surface.entries {
            if entry.provenance == .hostBridge {
                XCTAssertNotNil(
                    entry.bridgeMethod,
                    "\(entry.capability.rawValue) is a native bridge with no method name, so "
                        + "nothing can call it and nothing can be refused by name")
            } else {
                XCTAssertNil(
                    entry.bridgeMethod,
                    "\(entry.capability.rawValue) is \(entry.provenance.rawValue) and names a "
                        + "bridge method. A native call wearing a web capability's name is a "
                        + "native API outside the compliance surface, which is the one thing "
                        + "4.7.2 punishes")
            }
        }
    }

    func testBridgeMethodsAreUnique() {
        let methods = Surface.entries.compactMap(\.bridgeMethod)
        XCTAssertEqual(
            methods.count, Set(methods).count,
            "two capabilities answer one method name, so which one a message reaches depends on "
                + "table order")
    }

    func testWhatTheWebPlatformGivesIsNeverMarkedOff() {
        // A web-platform row marked default-off would be a claim to withhold
        // something WebKit hands to any page. The runtime cannot do it, so the
        // surface must not say it.
        for entry in Surface.entries where entry.provenance == .webPlatform {
            XCTAssertTrue(
                entry.defaultEnabled,
                "\(entry.capability.rawValue) comes from WebKit and is marked off by default")
        }
    }

    func testTheDefaultSurfaceExposesNoPermissionAndNoRadio() {
        for capability in Surface.default.enabled {
            let provenance = Surface.entry(for: capability).provenance
            XCTAssertTrue(
                provenance == .webPlatform || provenance == .hostBridge,
                "\(capability.rawValue) is \(provenance.rawValue) and is on by default. The lane's "
                    + "review position is that its default surface carries no capability needing a "
                    + "user permission or a device radio")
        }
    }

    func testAnUndeclaredMethodIsRefusedByNameAndNotServed() {
        // Not `.serve` and not silence. Content that gets neither a reply nor a
        // refusal waits for one forever, and the bug presents as a hang in the
        // game rather than as a message about a method.
        XCTAssertEqual(
            Surface.default.decision(forBridgeMethod: "filesystem.read"),
            .refuseUnknown("filesystem.read"))
        XCTAssertEqual(
            Surface.default.decision(forBridgeMethod: ""), .refuseUnknown(""))
    }

    func testADeclaredButWithheldMethodIsRefusedAsWithheldAndNotAsUnknown() throws {
        let surface = try Surface.configured(withholding: [.diagnostics])
        XCTAssertEqual(
            surface.decision(forBridgeMethod: "diagnostics.report"),
            .refuseDisabled(.diagnostics),
            "a host that turned a bridge off and a method nobody declared are different answers; "
                + "collapsing them sends a host app looking for a feature it already has")
        XCTAssertEqual(
            Surface.default.decision(forBridgeMethod: "diagnostics.report"), .serve(.diagnostics))
    }

    func testTheServedMethodListIsWhatTheHostShouldInstall() throws {
        XCTAssertEqual(
            Surface.default.servedBridgeMethods,
            ["content.read", "diagnostics.report", "environment.get", "lifecycle.observe"])
        let surface = try Surface.configured(withholding: [.contentBundleRead, .diagnostics])
        XCTAssertEqual(surface.servedBridgeMethods, ["environment.get", "lifecycle.observe"])
        for method in surface.servedBridgeMethods {
            guard case .serve = surface.decision(forBridgeMethod: method) else {
                return XCTFail("\(method) is listed as served and is not served")
            }
        }
    }

    func testACapabilityWithNoMechanismCannotBeTurnedOn() {
        // The difference between `absent` and `host_granted` in one assertion: one
        // is a decision a host app can make after talking to Apple, the other is
        // not a decision at all.
        for capability in Capability.allCases
        where Surface.entry(for: capability).provenance == .absent {
            XCTAssertThrowsError(try Surface.configured(enabling: [capability])) { error in
                XCTAssertEqual(
                    error as? Surface.ConfigurationError, .notReachable(capability),
                    "enabling \(capability.rawValue) has to fail loudly: a host that asked for it "
                        + "and got silence would ship believing content has it")
            }
        }
    }

    func testACapabilityWebKitGrantsAnywayCannotBeWithheld() {
        for capability in Capability.allCases
        where Surface.entry(for: capability).provenance == .webPlatform {
            XCTAssertThrowsError(try Surface.configured(withholding: [capability])) { error in
                XCTAssertEqual(
                    error as? Surface.ConfigurationError, .notWithholdable(capability))
            }
        }
    }

    func testAConfigurationThatSaysBothIsRefusedRatherThanResolved() {
        XCTAssertThrowsError(
            try Surface.configured(enabling: [.camera], withholding: [.camera])
        ) { error in
            XCTAssertEqual(error as? Surface.ConfigurationError, .contradictory(.camera))
        }
    }

    func testAHostGrantedCapabilityCanBeTurnedOnAndIsOffUntilThen() throws {
        XCTAssertFalse(Surface.default.grants(.camera))
        XCTAssertFalse(Surface.default.grants(.geolocation))
        let withCamera = try Surface.configured(enabling: [.camera])
        XCTAssertTrue(withCamera.grants(.camera))
        XCTAssertFalse(
            withCamera.grants(.cameraAndMicrophone),
            "WebKit asks for the pair as one question, and answering yes when only one is enabled "
                + "hands over a capability the surface says is off")
        let withBoth = try Surface.configured(enabling: [.camera, .microphone])
        XCTAssertTrue(withBoth.grants(.cameraAndMicrophone))
    }

    func testEnablingABridgeCapabilityDoesNotSilentlyEnableAPermission() throws {
        // The surface is additive over the default, so a host asking for one thing
        // must not receive two.
        let surface = try Surface.configured(enabling: [.motionSensors])
        XCTAssertTrue(surface.grants(.motionSensors))
        XCTAssertFalse(surface.grants(.camera))
        XCTAssertFalse(surface.grants(.microphone))
        XCTAssertFalse(surface.grants(.geolocation))
        XCTAssertEqual(surface.servedBridgeMethods, Surface.default.servedBridgeMethods)
    }
}

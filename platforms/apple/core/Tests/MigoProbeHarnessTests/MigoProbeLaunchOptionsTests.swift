import XCTest

@testable import MigoProbeCore
@testable import MigoProbeHarness

/// The launch arguments an unattended run is driven by.
///
/// Every test here is a way the run could produce a plausible record that answers
/// a different question. The gate's two attested answers are device state no API
/// reports, so the only thing standing between "the operator declared Lockdown
/// Mode off" and "nobody was watching" is this parser -- and the difference
/// decides whether a JIT result generalises at all.
final class MigoProbeLaunchOptionsTests: XCTestCase {

    private typealias Options = MigoProbeLaunchOptions
    private typealias Failure = MigoProbeLaunchOptions.Failure

    func testNoArgumentsDeclaresNothing() throws {
        let options = try Options.parse(arguments: [])
        XCTAssertFalse(options.autorun)
        XCTAssertEqual(options.lockdownMode, .unknown)
        XCTAssertNil(
            options.localNetworkPromptObserved,
            "nobody watching for the alert has to stay a third answer; a false here would "
                + "attest an absence of prompts that nobody observed")
        XCTAssertNil(options.runId)
    }

    func testTheArgumentsOtherToolingInjectsAreNotOurs() throws {
        // XCTest, simctl, devicectl and Xcode all add arguments of their own. An
        // unrecognised one outside the namespace is not a mistake to refuse.
        let options = try Options.parse(arguments: [
            "/path/to/MigoProbe", "-NSTreatUnknownArgumentsAsOpen", "NO", "--", "-ApplePersistence",
            Options.Flag.autorun,
        ])
        XCTAssertTrue(options.autorun)
    }

    func testBothAttestationsArrive() throws {
        for (text, expected) in [("off", MigoLockdownMode.off), ("on", .on)] {
            let options = try Options.parse(arguments: ["\(Options.Flag.lockdown)=\(text)"])
            XCTAssertEqual(options.lockdownMode, expected)
        }
        for (text, expected) in [("none", false), ("shown", true)] {
            let options = try Options.parse(arguments: [
                "\(Options.Flag.localNetworkPrompt)=\(text)"
            ])
            XCTAssertEqual(options.localNetworkPromptObserved, expected)
        }
    }

    func testAMisspelledAttestationIsRefusedAndNotIgnored() {
        // The failure this exists for. `--migo-lockdown-mode=off` is a plausible
        // typing of a real flag; ignoring it records `unknown`, and admit.py then
        // reports the arms that needed the attestation as unmeasured -- which on
        // the page reads as though the device had ruled them out.
        assertFails(
            ["--migo-lockdown-mode=off"],
            with: .flagNotRecognised("--migo-lockdown-mode"))
    }

    func testAnUnusableValueIsRefusedRatherThanReadAsUnknown() {
        assertFails(
            ["\(Options.Flag.lockdown)=maybe"],
            with: .valueNotAccepted(
                flag: Options.Flag.lockdown, value: "maybe", expected: ["off", "on"]))
        assertFails(
            ["\(Options.Flag.localNetworkPrompt)=yes"],
            with: .valueNotAccepted(
                flag: Options.Flag.localNetworkPrompt, value: "yes", expected: ["none", "shown"]))
    }

    func testABareAttestationIsNotADeclaration() {
        assertFails(
            [Options.Flag.lockdown],
            with: .valueMissing(flag: Options.Flag.lockdown, expected: ["off", "on"]))
    }

    func testASwitchGivenAValueIsRefused() {
        // `--migo-autorun=false` reads as "do not autorun" and would do the
        // opposite, which is worse than not being understood.
        assertFails(
            ["\(Options.Flag.autorun)=false"],
            with: .valueNotAccepted(flag: Options.Flag.autorun, value: "false", expected: []))
    }

    func testTwoDeclarationsOfOneStateAreRefused() {
        assertFails(
            ["\(Options.Flag.lockdown)=off", "\(Options.Flag.lockdown)=on"],
            with: .flagRepeated(Options.Flag.lockdown))
    }

    func testARunIdThatCouldNameAnotherFileIsRefused() {
        for value in ["../evil", "a/b", "", String(repeating: "x", count: 65), "with space"] {
            assertFails(
                ["\(Options.Flag.runId)=\(value)"], with: .runIdNotUsable(value),
                "the run id becomes the records filename")
        }
        let options = try? Options.parse(arguments: [
            "\(Options.Flag.runId)=probe-20260909T010203Z-device"
        ])
        XCTAssertEqual(options?.runId, "probe-20260909T010203Z-device")
    }

    func testEveryFlagInTheTableActuallyDoesSomething() throws {
        // Derived from the table rather than listed here. A row added to
        // `accepted` without a case in the parser passes validation and then
        // parses to nothing, which is the silent half of the same failure the
        // misspelling test covers -- except the runner would be passing a flag
        // the app genuinely accepts and genuinely ignores.
        let untouched = Options()
        for row in Options.accepted {
            let argument: String
            switch row.values {
            case nil: argument = row.name
            case let values? where values.isEmpty: argument = "\(row.name)=probe"
            case let values?: argument = "\(row.name)=\(values[0])"
            }
            let options = try Options.parse(arguments: [argument])
            XCTAssertNotEqual(
                options, untouched,
                "\(row.name) parsed to the same options as passing nothing at all")
        }
    }

    func testTheAttestationCarriesTheDeclarationAndNamesTheRun() throws {
        let options = try Options.parse(arguments: [
            Options.Flag.autorun,
            "\(Options.Flag.lockdown)=on",
            "\(Options.Flag.localNetworkPrompt)=shown",
            "\(Options.Flag.runId)=named-run",
        ])
        let attestation = options.attestation()
        XCTAssertEqual(attestation.lockdownMode, .on)
        XCTAssertEqual(attestation.localNetworkPromptObserved, true)
        XCTAssertEqual(
            attestation.runId, "named-run",
            "the collector asks for capability-<run id>.json; a run that named itself something "
                + "else is a run whose records the collector finds only by globbing, and a glob "
                + "finds the previous run's file when this one wrote none")

        let unnamed = try Options.parse(arguments: []).attestation()
        XCTAssertFalse(unnamed.runId.isEmpty)
        XCTAssertEqual(unnamed.lockdownMode, .unknown)
        XCTAssertNil(unnamed.localNetworkPromptObserved)
    }

    private func assertFails(
        _ arguments: [String], with expected: Failure, _ note: String = "",
        file: StaticString = #filePath, line: UInt = #line
    ) {
        do {
            let options = try Options.parse(arguments: arguments)
            XCTFail(
                "\(arguments) parsed to \(options) instead of failing. \(note)", file: file,
                line: line)
        } catch let failure as Failure {
            XCTAssertEqual(failure, expected, note, file: file, line: line)
            XCTAssertFalse(failure.description.isEmpty, file: file, line: line)
        } catch {
            XCTFail("\(arguments) failed with \(error), which is not a Failure", file: file, line: line)
        }
    }
}

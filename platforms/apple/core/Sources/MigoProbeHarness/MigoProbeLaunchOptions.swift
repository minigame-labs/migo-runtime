import Foundation
import MigoProbeCore

/// What the operator declared on the command line, parsed once and strictly.
///
/// Two of gate 1's ten answers have no API behind them. Whether Lockdown Mode is
/// in force (A24) and whether the system presented a local-network alert are both
/// device state a process cannot read, so a person declares them and the record
/// says a person did.
///
/// That worked while the only way to run the gate was to tap two segmented
/// controls, and it is exactly why an unattended run could not produce evidence:
/// `no_local_network_prompt` comes back `not_probed`, the loopback origin requires
/// it, and `admit.py` then reports every loopback candidate as unmeasured -- which
/// on the page reads as though loopback had been ruled out. A gate only a finger
/// can drive is a gate whose result is a number somebody read off a screen.
///
/// So the declaration moves to the launch arguments, where automation can carry it
/// and it is still a person's answer. What must not come with it is a default. A
/// flag nobody passed is `unknown`; a flag passed wrong is an **error**, never
/// `unknown`. Collapsing those two is how a Lockdown-Mode device's answers get
/// filed as a JIT device's -- and the second one is unrecoverable once the devices
/// go home, because nothing in the record says the declaration was mistyped.
///
/// It lives in the harness rather than in the app target because an Xcode target is
/// code no lane compiles. The two-line ancestor of this parser
/// (`arguments.contains("--migo-autorun")`) lived in the view controller, which is
/// precisely the shape `platforms/apple/core` exists to move out of Xcode.
public struct MigoProbeLaunchOptions: Sendable, Equatable {

    /// Run the gate once the view is on screen, then print the records.
    public var autorun: Bool

    /// A24. `unknown` unless a person said otherwise.
    public var lockdownMode: MigoLockdownMode

    /// `nil` means nobody watched for the alert, which is a third answer and not
    /// a `false`.
    public var localNetworkPromptObserved: Bool?

    /// Names the records file, so a collector can ask for the file this run wrote
    /// instead of globbing a directory and finding the previous run's.
    public var runId: String?

    public init(
        autorun: Bool = false,
        lockdownMode: MigoLockdownMode = .unknown,
        localNetworkPromptObserved: Bool? = nil,
        runId: String? = nil
    ) {
        self.autorun = autorun
        self.lockdownMode = lockdownMode
        self.localNetworkPromptObserved = localNetworkPromptObserved
        self.runId = runId
    }

    /// Every flag this parser accepts, written **once**.
    ///
    /// `scripts/test-apple-probe-run-contract.sh` reads the `--migo-` literals out
    /// of this file and out of `scripts/run-apple-probe.sh` and requires the two
    /// sets to be equal, so it also requires each name to appear here exactly
    /// once. That is why the failures below interpolate these constants instead of
    /// spelling the flag out again: a second copy of a flag name is how a runner
    /// keeps passing an argument the app stopped reading, and nothing goes red --
    /// the record simply says `unknown` and the admission says `unmeasured`.
    public enum Flag {
        public static let autorun = "--migo-autorun"
        public static let lockdown = "--migo-lockdown"
        public static let localNetworkPrompt = "--migo-local-network-prompt"
        public static let runId = "--migo-run-id"
    }

    /// The namespace this parser claims. Everything outside it belongs to somebody
    /// else -- XCTest, `simctl`, `devicectl` and Xcode all inject arguments of
    /// their own -- so an unrecognised argument is only an error inside the prefix.
    public static let flagPrefix = "--migo-"

    public enum Failure: Error, CustomStringConvertible, Equatable {
        case flagNotRecognised(String)
        case flagRepeated(String)
        case valueMissing(flag: String, expected: [String])
        case valueNotAccepted(flag: String, value: String, expected: [String])
        case runIdNotUsable(String)

        public var description: String {
            switch self {
            case .flagNotRecognised(let flag):
                return
                    "\(flag) is not a flag this probe reads. A misspelled attestation is not a "
                    + "missing one: it runs, records `unknown`, and the admission then reports "
                    + "the arms that needed it as unmeasured. Accepted: "
                    + MigoProbeLaunchOptions.accepted.map(\.name).joined(separator: ", ")
            case .flagRepeated(let flag):
                return
                    "\(flag) was given twice. Two declarations of one device state disagree or "
                    + "they are redundant, and last-one-wins silently picks for you"
            case .valueMissing(let flag, let expected):
                return
                    "\(flag) needs a value, as \(flag)=<\(expected.joined(separator: "|"))>. "
                    + "Passing it bare is not a declaration"
            case .valueNotAccepted(let flag, let value, let expected):
                if expected.isEmpty {
                    return "\(flag) takes no value, and \(value.debugDescription) was given"
                }
                return
                    "\(flag)=\(value) is not one of \(expected.joined(separator: ", ")). It is "
                    + "refused rather than read as unknown, because unknown is what a run "
                    + "nobody attested says and this run had somebody at the bench"
            case .runIdNotUsable(let value):
                return
                    "\(MigoProbeLaunchOptions.Flag.runId)=\(value) cannot name a file. Use up to "
                    + "64 of A-Z a-z 0-9 . _ - : the run id becomes the records filename, and a "
                    + "separator or a `..` in it writes the run somewhere the collector will not "
                    + "look"
            }
        }
    }

    /// One row per flag: the name, and the values it accepts. `nil` accepted
    /// values means the flag takes none; an empty array means it takes a free-form
    /// one.
    struct Accepted {
        let name: String
        let values: [String]?
    }

    static let accepted: [Accepted] = [
        Accepted(name: Flag.autorun, values: nil),
        Accepted(name: Flag.lockdown, values: ["off", "on"]),
        Accepted(name: Flag.localNetworkPrompt, values: ["none", "shown"]),
        Accepted(name: Flag.runId, values: []),
    ]

    /// The characters a run id may use, because it becomes a filename.
    static let runIdAllowed = CharacterSet(
        charactersIn: "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789._-")
    static let runIdMaxLength = 64

    public static func parse(arguments: [String]) throws -> MigoProbeLaunchOptions {
        var options = MigoProbeLaunchOptions()
        var seen = Set<String>()

        for argument in arguments where argument.hasPrefix(flagPrefix) {
            let (name, value) = split(argument)
            guard let row = accepted.first(where: { $0.name == name }) else {
                throw Failure.flagNotRecognised(name)
            }
            guard seen.insert(name).inserted else {
                throw Failure.flagRepeated(name)
            }

            switch row.values {
            case nil:
                if let value {
                    throw Failure.valueNotAccepted(flag: name, value: value, expected: [])
                }
            case let expected?:
                guard let value else {
                    throw Failure.valueMissing(flag: name, expected: expected)
                }
                if !expected.isEmpty, !expected.contains(value) {
                    throw Failure.valueNotAccepted(
                        flag: name, value: value, expected: expected)
                }
            }

            switch name {
            case Flag.autorun:
                options.autorun = true
            case Flag.lockdown:
                options.lockdownMode = value == "on" ? .on : .off
            case Flag.localNetworkPrompt:
                options.localNetworkPromptObserved = value == "shown"
            case Flag.runId:
                guard let value, isUsableRunId(value) else {
                    throw Failure.runIdNotUsable(value ?? "")
                }
                options.runId = value
            default:
                // Unreachable: `accepted` is the only way into this switch, and a
                // row added there without a case here would silently parse to
                // nothing. Stated rather than defaulted so it is a crash on the
                // bench and not an `unknown` in a record.
                throw Failure.flagNotRecognised(name)
            }
        }

        return options
    }

    static func isUsableRunId(_ value: String) -> Bool {
        guard !value.isEmpty, value.count <= runIdMaxLength else { return false }
        return value.unicodeScalars.allSatisfy(runIdAllowed.contains)
    }

    /// `--migo-lockdown=off` splits; `--migo-autorun` has no value. Only the first
    /// `=` separates, so a free-form value may contain one.
    static func split(_ argument: String) -> (name: String, value: String?) {
        guard let separator = argument.firstIndex(of: "=") else { return (argument, nil) }
        return (
            String(argument[argument.startIndex..<separator]),
            String(argument[argument.index(after: separator)...])
        )
    }

    /// The declaration this run carries, with a fresh run id when none was named.
    public func attestation() -> MigoCapabilityGate.Attestation {
        MigoCapabilityGate.Attestation(
            lockdownMode: lockdownMode,
            localNetworkPromptObserved: localNetworkPromptObserved,
            runId: runId ?? UUID().uuidString)
    }
}

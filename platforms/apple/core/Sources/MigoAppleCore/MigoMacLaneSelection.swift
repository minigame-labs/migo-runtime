import Foundation

/// Which lane a macOS Session gets, from what the process can be observed to be.
///
/// `Sources/MigoMacV8/README.md` has promised one sentence since the lane was
/// specified: *"If the entitlement is missing or the signature does not
/// validate, the profile resolver selects a WebKit lane. It does not silently
/// fall back to a jitless V8."* Nothing checked it, because there was no
/// resolver -- `MigoMacV8` was one `Placeholder.swift`. This is the decision
/// half of that resolver.
///
/// **Why it is here and not in `MigoMacV8`.** The decision is a pure function of
/// three observations; the observations need the Security framework and a real
/// signed process. Splitting them is the same arrangement `MigoWebKitOriginRules`
/// and `MigoWebKitContentOrigin` use, for the same reason: this package is what
/// a pull request compiles and tests on every commit, and a rule that only runs
/// behind an xcframework is a rule nobody has run.
///
/// **Why jitless is not among the answers.** A V8 built jitless does not run
/// slower, it deletes WebAssembly outright (`typeof WebAssembly === "undefined"`,
/// measured on HarmonyOS NEXT and recorded in CLAUDE.md), and every Cocos and
/// Unity export ships `.wasm.br`. So it is a diagnostic profile and never a
/// default, which makes "V8 without JIT" a lane this type must never return.
public enum MigoMacLaneSelection {

    /// What the host managed to observe about its own process.
    ///
    /// Each field is tri-state rather than `Bool`, and that is the whole design.
    /// A host that could not read its own signature has not learned "unsigned";
    /// it has learned nothing, and the two want opposite answers -- an
    /// unsigned-but-readable process is a development build the maintainer is
    /// running, while an unreadable one is a question this code cannot answer
    /// and must not guess at.
    public enum Observation: Sendable, Equatable {
        case yes
        case no
        /// The question could not be asked. Distinct from `no`.
        case unknown
    }

    /// What the running process is.
    public struct ProcessSignature: Sendable, Equatable {
        /// Whether the hardened runtime flag is set on this process's signature.
        public var hardenedRuntime: Observation
        /// Whether `com.apple.security.cs.allow-jit` is in its entitlements.
        public var jitEntitlement: Observation

        public init(hardenedRuntime: Observation, jitEntitlement: Observation) {
            self.hardenedRuntime = hardenedRuntime
            self.jitEntitlement = jitEntitlement
        }
    }

    /// One decision, with the reason that made it.
    public struct Decision: Sendable, Equatable {
        public let profile: MigoRuntimeProfile
        public let reason: MigoProfileReason
        /// One sentence a host can log or show. Not a localisable string: it is
        /// for whoever is reading a crash report at two in the morning.
        public let explanation: String
    }

    /// Choose the lane.
    ///
    /// - Parameters:
    ///   - v8LaneCompiledIn: step 2 of `contracts/apple/profile-policy.json`,
    ///     "whether this binary contains the lane at all. Reported from what the
    ///     artifact actually compiled, never from what the enum can name."
    ///   - signature: what the host observed about its own process.
    public static func decide(
        v8LaneCompiledIn: Bool, signature: ProcessSignature
    ) -> Decision {
        // Step 2 first, and it is not a formality: the external-frames build of
        // this SDK contains no engine at all, and a host that asked for the V8
        // lane there would get a profile nothing can service. A missing lane is
        // not a capability failure -- nothing was probed -- so the reason says
        // the build, not the device.
        guard v8LaneCompiledIn else {
            return Decision(
                profile: .macosWebKitFull, reason: .initializationFailed,
                explanation:
                    "this build does not contain the in-process V8 lane, so there is nothing "
                    + "to select; WebKit runs the content.")
        }

        switch signature.jitEntitlement {
        case .yes:
            // Present. The hardened runtime flag does not change the answer --
            // without hardened runtime the entitlement is inert and JIT is
            // available anyway; with it, the entitlement is what grants JIT.
            // Both reach the same lane, so a host is not made to care.
            return Decision(
                profile: .macosV8Native, reason: .hostRequested,
                explanation: signature.hardenedRuntime == .yes
                    ? "hardened runtime with com.apple.security.cs.allow-jit: in-process V8 with JIT."
                    : "com.apple.security.cs.allow-jit is present: in-process V8 with JIT.")

        case .no:
            // Absent. Under a hardened runtime this process cannot map
            // executable memory, and V8 does not degrade when denied it -- it
            // dies. Without the hardened runtime flag, JIT would in fact work
            // today, and the lane is STILL refused: an app that ships
            // unhardened is an app that will be hardened before it is
            // notarised, and a runtime that worked in development and died in
            // the notarised build is the worst possible time to find out.
            return Decision(
                profile: .macosWebKitFull, reason: .capabilityProbeFailed,
                explanation:
                    "com.apple.security.cs.allow-jit is not in this process's entitlements, so "
                    + "the in-process V8 lane is not offered. Add it to the embedding app's "
                    + "entitlements and sign with --options runtime. It is NOT replaced by a "
                    + "jitless V8: that configuration removes WebAssembly entirely.")

        case .unknown:
            // Unreadable. Refusing is the only safe answer: selecting V8 on a
            // guess is a process that dies at the first compile, and the host is
            // told which question failed rather than being handed a lane that
            // may not start.
            return Decision(
                profile: .macosWebKitFull, reason: .capabilityProbeFailed,
                explanation:
                    "this process's entitlements could not be read, so whether JIT is permitted "
                    + "is unknown; WebKit runs the content rather than starting a V8 that may "
                    + "be killed on its first compile.")
        }
    }
}

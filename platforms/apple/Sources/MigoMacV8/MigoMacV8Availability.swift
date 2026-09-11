import Foundation
import MigoAppleCore

#if os(macOS)
    import Security

    /// Whether this process may run in-process V8 with JIT, and what to do if not.
    ///
    /// The measurement half of the resolver `Sources/MigoMacV8/README.md` has
    /// promised since the lane was specified. The decision half is
    /// `MigoMacLaneSelection` in the engine-free package, where a pull request
    /// runs it; this reads the two facts that decision needs off the running
    /// process.
    ///
    /// ## What it reads, and why through this API
    ///
    /// `SecCodeCopySelf` plus `SecCodeCopySigningInformation` is the public,
    /// documented way for a process to inspect its own signature.  It answers
    /// both questions from one call: `kSecCodeInfoEntitlementsDict` carries
    /// `com.apple.security.cs.allow-jit`, and `kSecCodeInfoFlags` carries
    /// `kSecCodeSignatureRuntime`, which is the hardened runtime.
    ///
    /// ## Why every failure is `unknown` rather than `false`
    ///
    /// An unsigned process and an unreadable one are different facts that want
    /// opposite answers, and collapsing them is how a resolver starts a V8 that
    /// dies at its first compile. Every early return below is `unknown`; only a
    /// signature that was read and does not carry the entitlement is `no`.
    ///
    /// ## What this is NOT
    ///
    /// It is not a security check. A process can observe its own signature; it
    /// cannot use that observation to defend itself against anything, because
    /// whatever could forge the answer already runs as the process. This exists
    /// so the host picks a lane that will start, which is a correctness question.
    public enum MigoMacV8Availability {

        /// Whether this binary contains the in-process V8 lane at all.
        ///
        /// Step 2 of `contracts/apple/profile-policy.json`: "reported from what
        /// the artifact actually compiled, never from what the enum can name."
        /// `MigoMacV8` is only built into the macOS V8 product, so the fact that
        /// this code is executing is the report -- and the `#if os(macOS)` around
        /// it is what keeps that true rather than assumed.
        public static let laneIsCompiledIn = true

        /// What this process's signature says, or `unknown` where it says
        /// nothing this code can read.
        public static func observeSelf() -> MigoMacLaneSelection.ProcessSignature {
            let unknown = MigoMacLaneSelection.ProcessSignature(
                hardenedRuntime: .unknown, jitEntitlement: .unknown)

            var code: SecCode?
            guard SecCodeCopySelf([], &code) == errSecSuccess, let code else { return unknown }

            // `SecCodeCopySigningInformation` takes a static code, not a running
            // one, and the conversion is its own call that can fail on a binary
            // whose file has been replaced underneath it.
            var staticCode: SecStaticCode?
            guard SecCodeCopyStaticCode(code, [], &staticCode) == errSecSuccess,
                let staticCode
            else { return unknown }

            var information: CFDictionary?
            let flags = SecCSFlags(rawValue: kSecCSSigningInformation | kSecCSRequirementInformation)
            guard SecCodeCopySigningInformation(staticCode, flags, &information) == errSecSuccess,
                let details = information as? [String: Any]
            else { return unknown }

            // `kSecCodeSignatureRuntime` is the hardened runtime bit. Absent
            // flags mean an unsigned or ad-hoc binary with no flags word, which
            // is readable and therefore `no` rather than `unknown`.
            let hardened: MigoMacLaneSelection.Observation
            if let codeFlags = details[kSecCodeInfoFlags as String] as? UInt32 {
                hardened = (codeFlags & UInt32(kSecCodeSignatureRuntime)) != 0 ? .yes : .no
            } else {
                hardened = .unknown
            }

            // A signed binary with no entitlements at all has an absent
            // dictionary, and that is an answer: nothing granted it JIT. An
            // unreadable dictionary is not, and the two are separated here.
            let jit: MigoMacLaneSelection.Observation
            if details[kSecCodeInfoEntitlementsDict as String] == nil {
                jit = details[kSecCodeInfoFlags as String] == nil ? .unknown : .no
            } else if let entitlements = details[kSecCodeInfoEntitlementsDict as String]
                as? [String: Any]
            {
                jit = (entitlements["com.apple.security.cs.allow-jit"] as? Bool) == true ? .yes : .no
            } else {
                jit = .unknown
            }

            return MigoMacLaneSelection.ProcessSignature(
                hardenedRuntime: hardened, jitEntitlement: jit)
        }

        /// The lane this process should run, with the reason.
        public static func resolve() -> MigoMacLaneSelection.Decision {
            MigoMacLaneSelection.decide(
                v8LaneCompiledIn: laneIsCompiledIn, signature: observeSelf())
        }
    }
#endif

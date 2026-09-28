import MigoEngine

/// What the linked engine can actually do, asked before anything is created.
///
/// WHY THE RENDERER ASKS AT ALL, instead of trusting the headers it compiled
/// against. `MIGO_C_ABI_HAS_RUNTIME` is a preprocessor macro that reports the
/// platform the *host* compiled on, not the library it linked, and the headers
/// declare every platform's descriptors on every platform -- `platform/ios.h`
/// is as compilable on Linux as it is here. This repository has already shipped
/// a package where that gap was the whole defect: windows-sdk-0.1.0 declared the
/// Win32 descriptors, pinned their layout with C assertions, exported every
/// entry point, loaded cleanly, and advertised no attachable platform kind at
/// all, because the Rust half had never been written. It could be linked and it
/// could not be used, and nothing in the package said so.
///
/// `migo_query_capabilities` is the narrow answer to the narrow question, and
/// the header says as much: it is the same fact `migo_session_attach_surface`
/// enforces rather than a second copy of it. Taking no handle, it can be asked
/// before an engine, a session or a layer exists -- which is the only useful
/// time to ask, because the alternative is discovering it from a failed attach
/// after the host has built all three.
///
/// WHAT IT ANSWERS ON APPLE TODAY is that the linked Apple platform module
/// advertises the target's `CAMetalLayer` surface kind, and `preflight` can
/// accept a matching host surface. The capability is queried from the linked
/// library rather than assumed from the headers, so the preflight remains the
/// check that the actual artifact accepts the surface this target presents.
/// The invariants below are written to hold whether a future build advertises
/// the same kind or adds another supported surface.
public struct MigoEngineCapabilities: Equatable, Sendable {
    /// The inclusive range of ABI versions the linked library accepts on its
    /// entry points.
    ///
    /// Two fields and not a `ClosedRange`, which is what this would obviously
    /// be in Swift: these numbers come from a foreign library, and `min...max`
    /// traps when `max < min`. A library that answered nonsense would take the
    /// host process down inside the call whose entire purpose is to find out
    /// whether that library can be used.
    public let acceptedABIVersionMin: UInt32
    public let acceptedABIVersionMax: UInt32

    /// Bit N is set when `MIGO_PLATFORM_*` value N can be attached by this
    /// build. Kept as the raw mask rather than a set of decoded cases, because
    /// a build may legitimately advertise a kind newer than the Swift that
    /// reads it, and decoding would have to choose between dropping that bit
    /// and inventing a case for it.
    public let attachableSurfaceKinds: UInt64

    public init(
        acceptedABIVersionMin: UInt32,
        acceptedABIVersionMax: UInt32,
        attachableSurfaceKinds: UInt64
    ) {
        self.acceptedABIVersionMin = acceptedABIVersionMin
        self.acceptedABIVersionMax = acceptedABIVersionMax
        self.attachableSurfaceKinds = attachableSurfaceKinds
    }
}

/// The library refused the capability query itself.
///
/// A distinct type from `MigoRendererPreflight` on purpose. "The library says
/// it cannot attach your layer" and "the library would not answer the question"
/// are different failures with different causes -- the first is a port that has
/// not landed, the second is a caller-side ABI mistake or a library too old to
/// have this entry point's contract -- and collapsing them would make the
/// second read as the first.
public enum MigoEngineCapabilityQueryError: Error, Equatable, Sendable {
    /// `migo_query_capabilities` returned a failure code. The header defines
    /// exactly one: `MIGO_ERROR_INVALID_ARGUMENT`, meaning the record this call
    /// passed was NULL or too small. The code is carried rather than discarded
    /// so a library that grows a second failure is not reported as the first.
    case rejected(MigoResult)
}

/// Whether this build of the engine can be driven by this renderer.
public enum MigoRendererPreflight: Equatable, Sendable {
    case ready

    /// The library does not accept the ABI version this renderer was written
    /// against. Reported before any surface-kind verdict: the meaning of the
    /// platform-kind bit numbering is defined by the ABI version, so a mask
    /// from a library that rejects our version cannot be interpreted.
    case abiVersionNotAccepted(hostRequires: UInt32, acceptsMin: UInt32, acceptsMax: UInt32)

    /// The library speaks our ABI and does not advertise the surface kind this
    /// renderer presents into. Attaching would fail with
    /// `MIGO_ERROR_UNSUPPORTED_PLATFORM`; this is the same fact, before the
    /// host has built an engine, a session and a layer to discover it.
    case surfaceKindNotAttachable(kind: UInt32)
}

extension MigoEngineCapabilities {
    /// Ask the linked library.
    ///
    /// `struct_size` is `MemoryLayout.stride` and not `.size`: the field is the
    /// C `sizeof`, which includes trailing padding, and Swift's `.size` does
    /// not. They are equal for this record, which is exactly why the wrong one
    /// would go unnoticed here and be wrong in the next record that has any.
    public static func query() throws -> MigoEngineCapabilities {
        var record = MigoCapabilities()
        record.struct_size = UInt32(MemoryLayout<MigoCapabilities>.stride)
        record.abi_version = MIGO_ABI_VERSION_CURRENT

        let result = migo_query_capabilities(&record)
        guard result == MIGO_OK else {
            throw MigoEngineCapabilityQueryError.rejected(result)
        }

        return MigoEngineCapabilities(
            acceptedABIVersionMin: record.abi_version_min,
            acceptedABIVersionMax: record.abi_version_max,
            attachableSurfaceKinds: record.platform_kinds
        )
    }

    /// The surface kind this renderer hands to the engine.
    ///
    /// The `CAMetalLayer` kind on both platforms, not the view kind: this
    /// target owns the layer -- one per Session, created and retained here --
    /// and the view descriptors exist for hosts that would rather give up that
    /// ownership. Preflighting the kind we do not pass would answer a question
    /// about a code path this target never takes.
    ///
    /// The `#else` is a compile error rather than a `0` or a fatal: a new Apple
    /// platform (tvOS, visionOS) reaching this line is a porting decision, and
    /// the two failure modes of returning something are that `0` reads as
    /// `MIGO_PLATFORM_UNKNOWN` -- a legitimate value the preflight would
    /// truthfully refuse, with a message pointing at the engine instead of at
    /// this file -- or that a trap turns it into a runtime crash on a platform
    /// nobody ever built for on purpose.
    public static var hostSurfaceKind: UInt32 {
        #if os(iOS)
            return MIGO_PLATFORM_IOS_CA_METAL_LAYER
        #elseif os(macOS)
            return MIGO_PLATFORM_MACOS_CA_METAL_LAYER
        #else
            #error("MigoAppleRenderer has no host surface kind for this platform. Add one to include/migo/platform/ and to migo-capi's platform module before adding it here -- a kind declared on only one of those sides fails in a way that misdirects.")
        #endif
    }

    /// Whether this build advertises the given `MIGO_PLATFORM_*` kind.
    ///
    /// A kind outside the mask's width is unsupported by definition rather than
    /// by arithmetic accident, which is the same rule and the same reason as
    /// `kind_is_supported` on the Rust side. Swift's shift would answer `0`
    /// here rather than trapping, so the guard buys clarity rather than safety
    /// -- but it buys the two sides agreeing, and a disagreement about kind 64
    /// is the kind of thing that is only ever found in production.
    public func canAttach(surfaceKind: UInt32) -> Bool {
        guard surfaceKind < UInt32(UInt64.bitWidth) else { return false }
        return attachableSurfaceKinds & (UInt64(1) << UInt64(surfaceKind)) != 0
    }

    /// Whether the library accepts the ABI version the caller was built for.
    public func accepts(abiVersion: UInt32) -> Bool {
        abiVersion >= acceptedABIVersionMin && abiVersion <= acceptedABIVersionMax
    }

    /// The whole precondition of `migo_session_attach_surface`, evaluated
    /// before there is a session or a layer to attach, against what this
    /// renderer itself requires.
    public func preflight() -> MigoRendererPreflight {
        preflight(hostRequires: MIGO_ABI_VERSION_CURRENT, surfaceKind: Self.hostSurfaceKind)
    }

    /// The same decision against requirements the caller states.
    ///
    /// An overload and not a pair of default arguments, which is what this
    /// obviously wants to be. A default argument is serialized into the module
    /// interface so a caller in another module can materialize it, so it is
    /// type-checked in the emit-module pass -- the one that skips
    /// non-inlinable function bodies. That is worth knowing because it decides
    /// what a failure there looks like: the first build of this file reported
    /// `cannot find 'MIGO_ABI_VERSION_CURRENT' in scope` at the default
    /// argument and nowhere else, which read as a rule about default arguments
    /// and was not one. The real cause was that the constant was spelled
    /// `UINT32_C(1)` and no Swift could name it anywhere; the emit-module pass
    /// had simply not looked at the function bodies yet. See the comment on
    /// MIGO_ABI_VERSION_1 in include/migo/types.h.
    ///
    /// The overload stays because it is the better shape regardless -- a
    /// default argument would bake this library's current ABI version into
    /// every caller at their compile time -- and because it keeps this file out
    /// of a question it does not need to answer.
    public func preflight(
        hostRequires abiVersion: UInt32,
        surfaceKind: UInt32
    ) -> MigoRendererPreflight {
        guard accepts(abiVersion: abiVersion) else {
            return .abiVersionNotAccepted(
                hostRequires: abiVersion,
                acceptsMin: acceptedABIVersionMin,
                acceptsMax: acceptedABIVersionMax
            )
        }
        guard canAttach(surfaceKind: surfaceKind) else {
            return .surfaceKindNotAttachable(kind: surfaceKind)
        }
        return .ready
    }
}

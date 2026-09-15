//! The ingress outcome record: its consistency rules, and its agreement with
//! the wire reader that produces the values.

use migo_capi_abi::{
    MIGO_ABI_VERSION_CURRENT, MIGO_ERROR_INVALID_ARGUMENT, MIGO_OK,
    external_frames::{
        MIGO_FRAME_INGRESS_ACCEPTED, MIGO_FRAME_INGRESS_DEFERRED,
        MIGO_FRAME_INGRESS_GENERATION_LOST, MIGO_FRAME_INGRESS_REJECTED,
        MIGO_FRAME_INGRESS_WOULD_BLOCK, MigoFrameIngressOutcome, write_frame_ingress_outcome,
    },
};

fn write(decision: u32, credits: u32, sequence: u64, error: u32) -> (i32, MigoFrameIngressOutcome) {
    let mut out = MigoFrameIngressOutcome {
        header: migo_capi_abi::VersionedHeader {
            struct_size: size_of::<MigoFrameIngressOutcome>() as u32,
            abi_version: MIGO_ABI_VERSION_CURRENT,
        },
        accepted_sequence: 0,
        decision: 0,
        remaining_credits: 0,
        wire_error_code: 0,
        reserved0: 0,
    };
    // SAFETY: `out` is a live, correctly sized, correctly versioned record.
    let result =
        unsafe { write_frame_ingress_outcome(&mut out, decision, credits, sequence, error) };
    (result, out)
}

#[test]
fn a_well_formed_outcome_round_trips() {
    let (result, out) = write(MIGO_FRAME_INGRESS_ACCEPTED, 1, 42, 0);
    assert_eq!(result, MIGO_OK);
    assert_eq!(out.decision, MIGO_FRAME_INGRESS_ACCEPTED);
    assert_eq!(out.accepted_sequence, 42);
    assert_eq!(out.remaining_credits, 1);
    assert_eq!(out.wire_error_code, 0);
    assert_eq!(out.reserved0, 0);
    assert_eq!(
        out.header.struct_size,
        size_of::<MigoFrameIngressOutcome>() as u32
    );
}

#[test]
fn a_sequence_number_means_the_packet_was_taken() {
    // Only ACCEPTED takes one. The natural bug is an early return that forgets
    // to clear the field, leaving a rejection that names a frame it did not
    // accept -- and the host's own bookkeeping keys on exactly that field.
    assert_eq!(
        write(MIGO_FRAME_INGRESS_ACCEPTED, 1, 0, 0).0,
        MIGO_ERROR_INVALID_ARGUMENT,
        "ACCEPTED without a sequence",
    );
    for decision in [
        MIGO_FRAME_INGRESS_WOULD_BLOCK,
        MIGO_FRAME_INGRESS_GENERATION_LOST,
        MIGO_FRAME_INGRESS_DEFERRED,
    ] {
        assert_eq!(
            write(decision, 0, 7, 0).0,
            MIGO_ERROR_INVALID_ARGUMENT,
            "decision {decision} carried a sequence number",
        );
    }
    assert_eq!(
        write(MIGO_FRAME_INGRESS_REJECTED, 1, 7, 5).0,
        MIGO_ERROR_INVALID_ARGUMENT,
        "REJECTED carried a sequence number",
    );
}

#[test]
fn an_error_code_means_the_bytes_were_refused() {
    assert_eq!(
        write(MIGO_FRAME_INGRESS_REJECTED, 1, 0, 0).0,
        MIGO_ERROR_INVALID_ARGUMENT,
        "REJECTED without a reason",
    );
    // GENERATION_LOST is not a fault and WOULD_BLOCK is not a verdict on the
    // bytes, so neither may carry one.
    assert_eq!(
        write(MIGO_FRAME_INGRESS_GENERATION_LOST, 1, 0, 14).0,
        MIGO_ERROR_INVALID_ARGUMENT,
    );
    assert_eq!(
        write(MIGO_FRAME_INGRESS_WOULD_BLOCK, 0, 0, 14).0,
        MIGO_ERROR_INVALID_ARGUMENT,
    );
    // DEFERRED is a packet held for ordering: legal bytes, nothing wrong.
    assert_eq!(
        write(MIGO_FRAME_INGRESS_DEFERRED, 1, 0, 14).0,
        MIGO_ERROR_INVALID_ARGUMENT,
    );
}

#[test]
fn would_block_is_exactly_the_no_credit_answer() {
    assert_eq!(write(MIGO_FRAME_INGRESS_WOULD_BLOCK, 0, 0, 0).0, MIGO_OK);
    // Advertising credit alongside a refusal would tell the producer to retry
    // straight back into the same refusal.
    assert_eq!(
        write(MIGO_FRAME_INGRESS_WOULD_BLOCK, 1, 0, 0).0,
        MIGO_ERROR_INVALID_ARGUMENT,
    );
}

#[test]
fn an_unrecognised_decision_is_rejected_including_zero() {
    for decision in [0u32, 6, u32::MAX] {
        assert_eq!(
            write(decision, 0, 0, 0).0,
            MIGO_ERROR_INVALID_ARGUMENT,
            "decision {decision} must not be writable",
        );
    }
}

/// The four numbers here and the four in the wire reader are one decision. This
/// is the only place that can see both, so it is the only place the drift can
/// be caught -- `frame-wire` stays out of the C ABI crate's shipping closure,
/// and the ABI crate stays out of the wire reader's.
#[test]
fn the_abi_and_the_wire_reader_agree_on_every_decision() {
    use frame_wire::IngressDecision;
    assert_eq!(
        IngressDecision::Accepted as u32,
        MIGO_FRAME_INGRESS_ACCEPTED
    );
    assert_eq!(
        IngressDecision::WouldBlock as u32,
        MIGO_FRAME_INGRESS_WOULD_BLOCK
    );
    assert_eq!(
        IngressDecision::Rejected as u32,
        MIGO_FRAME_INGRESS_REJECTED
    );
    assert_eq!(
        IngressDecision::GenerationLost as u32,
        MIGO_FRAME_INGRESS_GENERATION_LOST
    );
    assert_eq!(
        IngressDecision::Deferred as u32,
        MIGO_FRAME_INGRESS_DEFERRED
    );
}

/// Every wire rejection reason must be expressible in the field that carries
/// it, and must be distinguishable from "no error".
///
/// The lists come from `frame-wire`, which proves them complete against its own
/// source. Writing them out here instead would make this test cover every
/// reason as of whenever someone last updated it -- and a reason added later
/// would be exactly the one that could not be reported.
#[test]
fn every_wire_error_code_is_non_zero_and_distinct_from_the_ingress_range() {
    use frame_wire::{WireError, ingress::INGRESS_ERROR_BASE};

    assert!(
        WireError::ALL.len() >= 20,
        "the envelope has more rejection reasons than this: {}",
        WireError::ALL.len()
    );

    for error in WireError::ALL {
        let code = error.code();
        assert_ne!(code, 0, "{error:?} would read as 'no error'");
        assert!(
            code < INGRESS_ERROR_BASE,
            "{error:?} is {code}, inside the range reserved for identity and \
             ordering failures",
        );
        // And each one must survive the write path it is destined for.
        let (result, out) = write(MIGO_FRAME_INGRESS_REJECTED, 2, 0, code);
        assert_eq!(result, MIGO_OK, "{error:?} could not be reported");
        assert_eq!(out.wire_error_code, code);
    }

    for code in frame_wire::ingress::INGRESS_ERROR_CODES {
        assert!(
            *code >= INGRESS_ERROR_BASE,
            "identity failures start at {INGRESS_ERROR_BASE}, found {code}"
        );
        let (result, out) = write(MIGO_FRAME_INGRESS_REJECTED, 2, 0, *code);
        assert_eq!(result, MIGO_OK, "code {code} could not be reported");
        assert_eq!(out.wire_error_code, *code);
    }
}

// ---------------------------------------------------------------------------
// The header, the Rust mirror, and the protocol enums, checked against each
// other rather than each against a reviewer's memory.
// ---------------------------------------------------------------------------

/// `#define NAME <n>U` lines in the public header.
///
/// Parsed rather than restated. Three copies of these numbers exist -- the
/// header a Swift transport compiles against, the Rust constants this crate
/// exports, and the enums in `frame-wire` that produce them -- and a test that
/// listed them a fourth time would only prove the fourth copy agrees with
/// itself.
///
/// The suffixed literal is the required spelling, and the `UINT32_C` form is
/// named below so that reverting to it fails with a sentence rather than with a
/// `ParseIntError` on a string nobody expected to be parsing. The reason is in
/// the comment on `MIGO_ABI_VERSION_1` in `include/migo/types.h`: Swift's Clang
/// importer reads a macro's definition tokens, declines a function-like macro
/// invocation, and would leave every constant in these headers unnameable from
/// Swift -- which is the language of the transport that consumes these very
/// constants.
fn header_defines(prefix: &str) -> Vec<(String, u32)> {
    const HEADER: &str = include_str!("../../../../include/migo/external_frames.h");
    let mut defines = Vec::new();
    for line in HEADER.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("#define ") else {
            continue;
        };
        let Some((name, value)) = rest.split_once(char::is_whitespace) else {
            continue;
        };
        if !name.starts_with(prefix) {
            continue;
        }
        let value = value.trim();
        assert!(
            !value.contains("_C("),
            "{name} is spelled {value}. Constants in the public headers must be suffixed \
             literals: Swift's importer declines a function-like macro invocation, which \
             would make this constant unnameable from the transport that uses it."
        );
        let digits = value
            .strip_suffix('U')
            .unwrap_or_else(|| panic!("{name} is spelled {value}; expected a <n>U literal"));
        defines.push((
            name.to_string(),
            digits
                .parse()
                .unwrap_or_else(|_| panic!("{name} is spelled {value}; expected a decimal value")),
        ));
    }
    assert!(
        !defines.is_empty(),
        "the header declares no {prefix}* constants"
    );
    defines
}

/// SCREAMING_SNAKE for a CamelCase variant, so `RequestIdMismatch` lines up
/// with `MIGO_SYNC_ERROR_REQUEST_ID_MISMATCH`.
fn shouted(variant: &str) -> String {
    let mut out = String::new();
    for (index, character) in variant.char_indices() {
        if character.is_ascii_uppercase() && index > 0 {
            out.push('_');
        }
        out.push(character.to_ascii_uppercase());
    }
    out
}

#[test]
fn the_header_and_the_protocol_enums_agree_on_every_sync_constant() {
    use frame_wire::sync::{SyncError, SyncState};

    let states = header_defines("MIGO_SYNC_STATE_");
    assert_eq!(states.len(), SyncState::ALL.len());
    for state in SyncState::ALL {
        let expected = format!("MIGO_SYNC_STATE_{}", shouted(&format!("{state:?}")));
        let (_, value) = states
            .iter()
            .find(|(name, _)| *name == expected)
            .unwrap_or_else(|| panic!("the header has no {expected}"));
        assert_eq!(*value, state.code(), "{expected}");
    }

    let errors = header_defines("MIGO_SYNC_ERROR_");
    assert_eq!(errors.len(), SyncError::ALL.len());
    for error in SyncError::ALL {
        let expected = format!("MIGO_SYNC_ERROR_{}", shouted(&format!("{error:?}")));
        let (_, value) = errors
            .iter()
            .find(|(name, _)| *name == expected)
            .unwrap_or_else(|| panic!("the header has no {expected}"));
        assert_eq!(*value, error.code(), "{expected}");
    }
}

#[test]
fn the_header_and_the_protocol_enums_agree_on_every_resource_constant() {
    use frame_wire::resource::{ResourceError, ResourceState};

    let states = header_defines("MIGO_RESOURCE_STATE_");
    assert_eq!(states.len(), ResourceState::ALL.len());
    for state in ResourceState::ALL {
        let expected = format!("MIGO_RESOURCE_STATE_{}", shouted(&format!("{state:?}")));
        let (_, value) = states
            .iter()
            .find(|(name, _)| *name == expected)
            .unwrap_or_else(|| panic!("the header has no {expected}"));
        assert_eq!(*value, state.code(), "{expected}");
    }

    let errors = header_defines("MIGO_RESOURCE_ERROR_");
    assert_eq!(errors.len(), ResourceError::ALL.len());
    for error in ResourceError::ALL {
        let expected = format!("MIGO_RESOURCE_ERROR_{}", shouted(&format!("{error:?}")));
        let (_, value) = errors
            .iter()
            .find(|(name, _)| *name == expected)
            .unwrap_or_else(|| panic!("the header has no {expected}"));
        assert_eq!(*value, error.code(), "{expected}");
    }
}

/// Every state and every reason must survive the write path it is destined for.
///
/// A reason the host can reach and the ABI cannot report is a producer left
/// blocked with no explanation, which on a device is a game that stopped and
/// said nothing.
#[test]
fn every_sync_and_resource_outcome_can_actually_be_reported() {
    use frame_wire::resource::{ResourceError, ResourceState};
    use frame_wire::sync::{SyncError, SyncState};
    use migo_capi_abi::external_frames::{
        MIGO_RESOURCE_STATE_FAILED, MIGO_SYNC_STATE_FAILED, MigoResourceOutcome, MigoSyncOutcome,
        write_resource_outcome, write_sync_outcome,
    };

    for state in SyncState::ALL {
        let mut out = MigoSyncOutcome {
            header: migo_capi_abi::VersionedHeader {
                struct_size: size_of::<MigoSyncOutcome>() as u32,
                abi_version: 1,
            },
            request_id: 0,
            state: 0,
            reply_bytes: 0,
            error: 0,
        };
        let (request_id, reply_bytes, error) = match state.code() {
            MIGO_SYNC_STATE_FAILED => (7, 0, SyncError::TimedOut.code()),
            0 => (0, 0, 0),
            _ => (7, 0, 0),
        };
        let result =
            unsafe { write_sync_outcome(&mut out, request_id, state.code(), reply_bytes, error) };
        assert_eq!(result, MIGO_OK, "{state:?} could not be reported");
        assert_eq!(out.state, state.code());
    }

    for error in SyncError::ALL {
        let mut out = MigoSyncOutcome {
            header: migo_capi_abi::VersionedHeader {
                struct_size: size_of::<MigoSyncOutcome>() as u32,
                abi_version: 1,
            },
            request_id: 0,
            state: 0,
            reply_bytes: 0,
            error: 0,
        };
        let result =
            unsafe { write_sync_outcome(&mut out, 3, MIGO_SYNC_STATE_FAILED, 0, error.code()) };
        assert_eq!(result, MIGO_OK, "{error:?} could not be reported");
        assert_eq!(out.error, error.code());
    }

    for state in ResourceState::ALL {
        let mut out = MigoResourceOutcome {
            header: migo_capi_abi::VersionedHeader {
                struct_size: size_of::<MigoResourceOutcome>() as u32,
                abi_version: 1,
            },
            reservation_id: 0,
            received_bytes: 0,
            state: 0,
            error: 0,
            next_chunk: 0,
            reserved0: 0,
        };
        let error = if state.code() == MIGO_RESOURCE_STATE_FAILED {
            ResourceError::DigestMismatch.code()
        } else {
            0
        };
        let result = unsafe { write_resource_outcome(&mut out, 1, 64, state.code(), error, 1) };
        assert_eq!(result, MIGO_OK, "{state:?} could not be reported");
        assert_eq!(out.state, state.code());
    }

    for error in ResourceError::ALL {
        let mut out = MigoResourceOutcome {
            header: migo_capi_abi::VersionedHeader {
                struct_size: size_of::<MigoResourceOutcome>() as u32,
                abi_version: 1,
            },
            reservation_id: 0,
            received_bytes: 0,
            state: 0,
            error: 0,
            next_chunk: 0,
            reserved0: 0,
        };
        let result = unsafe {
            write_resource_outcome(&mut out, 1, 0, MIGO_RESOURCE_STATE_FAILED, error.code(), 0)
        };
        assert_eq!(result, MIGO_OK, "{error:?} could not be reported");
        assert_eq!(out.error, error.code());
    }
}

/// A contradiction is refused rather than written. A `READY` carrying an error
/// is a pair the producer -- which is blocked reading this -- has no way to act
/// on.
#[test]
fn contradictory_sync_and_resource_outcomes_are_refused() {
    use migo_capi_abi::external_frames::{
        MIGO_RESOURCE_STATE_READY, MIGO_SYNC_STATE_FAILED, MIGO_SYNC_STATE_READY,
        MigoResourceOutcome, MigoSyncOutcome, write_resource_outcome, write_sync_outcome,
    };

    let mut sync = MigoSyncOutcome {
        header: migo_capi_abi::VersionedHeader {
            struct_size: size_of::<MigoSyncOutcome>() as u32,
            abi_version: 1,
        },
        request_id: 0,
        state: 0,
        reply_bytes: 0,
        error: 0,
    };
    assert_ne!(
        unsafe { write_sync_outcome(&mut sync, 1, MIGO_SYNC_STATE_READY, 16, 4) },
        MIGO_OK,
        "READY with an error is a contradiction"
    );
    assert_ne!(
        unsafe { write_sync_outcome(&mut sync, 1, MIGO_SYNC_STATE_FAILED, 16, 4) },
        MIGO_OK,
        "FAILED with reply bytes is a contradiction"
    );
    assert_ne!(
        unsafe { write_sync_outcome(&mut sync, 1, MIGO_SYNC_STATE_FAILED, 0, 0) },
        MIGO_OK,
        "FAILED must say why"
    );
    assert_ne!(
        unsafe { write_sync_outcome(&mut sync, 1, 99, 0, 0) },
        MIGO_OK,
        "an unrecognised state is refused"
    );

    let mut resource = MigoResourceOutcome {
        header: migo_capi_abi::VersionedHeader {
            struct_size: size_of::<MigoResourceOutcome>() as u32,
            abi_version: 1,
        },
        reservation_id: 0,
        received_bytes: 0,
        state: 0,
        error: 0,
        next_chunk: 0,
        reserved0: 0,
    };
    assert_ne!(
        unsafe { write_resource_outcome(&mut resource, 1, 64, MIGO_RESOURCE_STATE_READY, 7, 1) },
        MIGO_OK,
        "a READY resource carrying an error is one a frame may name and a host was told not to trust"
    );
    assert_ne!(
        unsafe { write_resource_outcome(&mut resource, 1, 0, 99, 0, 0) },
        MIGO_OK,
        "an unrecognised state is refused"
    );
}

/// An uninitialised descriptor must not produce a usable session identity.
///
/// All-zero is what a `MigoExternalSessionDescriptor {0}` holds, and two hosts
/// that both forgot to fill it in would accept each other's packets. Rejecting
/// it is cheaper than every downstream check that would otherwise have to
/// notice.
#[test]
fn an_all_zero_launch_nonce_is_refused() {
    use migo_capi_abi::external_frames::{MigoExternalSessionDescriptor, launch_nonce_of};

    let mut descriptor = MigoExternalSessionDescriptor {
        struct_size: size_of::<MigoExternalSessionDescriptor>() as u32,
        abi_version: 1,
        launch_nonce: [0u8; 16],
        max_packet_bytes: 0,
        max_credits: 0,
    };
    assert_eq!(
        launch_nonce_of(&descriptor),
        None,
        "a zeroed struct has no identity"
    );

    // A single bit anywhere is enough to be a value; the strength of the source
    // is the host's business and cannot be checked from a number.
    descriptor.launch_nonce[15] = 0x80;
    assert_eq!(
        launch_nonce_of(&descriptor),
        Some(0x8000_0000_0000_0000_0000_0000_0000_0000)
    );

    // Little-endian, matching the wire header the nonce is compared against.
    descriptor.launch_nonce = [
        0x10, 0x32, 0x54, 0x76, 0x98, 0xBA, 0xDC, 0xFE, 0xEF, 0xCD, 0xAB, 0x89, 0x67, 0x45, 0x23,
        0x01,
    ];
    assert_eq!(
        launch_nonce_of(&descriptor),
        Some(0x0123_4567_89AB_CDEF_FEDC_BA98_7654_3210),
        "the descriptor and the wire header read the nonce the same way"
    );
}

/// The session configuration reads its nonce the same way the descriptor does.
///
/// They are different records filled in by different calls, and they name the
/// same session: a byte-order disagreement between them would present as a
/// session that rejects every packet as foreign, with nothing saying that the
/// two sides disagree about byte order rather than about the value.
#[test]
fn the_session_config_and_the_descriptor_read_a_nonce_identically() {
    use migo_capi_abi::config::MigoSessionConfig;
    use migo_capi_abi::external_frames::{MigoExternalSessionDescriptor, launch_nonce_of};

    let bytes = [
        0x10, 0x32, 0x54, 0x76, 0x98, 0xBA, 0xDC, 0xFE, 0xEF, 0xCD, 0xAB, 0x89, 0x67, 0x45, 0x23,
        0x01,
    ];
    let expected = Some(0x0123_4567_89AB_CDEF_FEDC_BA98_7654_3210u128);

    let descriptor = MigoExternalSessionDescriptor {
        struct_size: size_of::<MigoExternalSessionDescriptor>() as u32,
        abi_version: 1,
        launch_nonce: bytes,
        max_packet_bytes: 0,
        max_credits: 0,
    };
    assert_eq!(launch_nonce_of(&descriptor), expected);

    let config = MigoSessionConfig {
        header: migo_capi_abi::VersionedHeader {
            struct_size: size_of::<MigoSessionConfig>() as u32,
            abi_version: 1,
        },
        flags: 0,
        launch_nonce: bytes,
    };
    let validated = config.validate().expect("a well-formed config");
    assert_eq!(
        validated.launch_nonce, expected,
        "the config and the descriptor must agree byte for byte"
    );

    // And a v1 caller, whose record ends before this field, gets no identity
    // rather than a zero one that would look like a value.
    let zeroed = MigoSessionConfig {
        launch_nonce: [0u8; 16],
        ..config
    };
    assert_eq!(zeroed.validate().expect("still valid").launch_nonce, None);
}

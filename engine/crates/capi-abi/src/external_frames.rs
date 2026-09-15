//! The ABI record a host reads back after offering one externally produced
//! frame.
//!
//! This module carries no entry point, matching `include/migo/external_frames.h`.
//! The layout is frozen now because the Swift transport is written against it,
//! and the submit function lands with an implementation behind it. An exported
//! symbol that always fails is what shipped a Windows SDK that loaded, resolved
//! every entry point, and could attach nothing.

use std::mem::{offset_of, size_of};

use crate::{
    AbiStruct, MIGO_ERROR_INVALID_ARGUMENT, MIGO_OK, MigoResult, OutputVersionPolicy,
    VersionedHeader, write_versioned_output,
};

pub const MIGO_FRAME_INGRESS_ACCEPTED: u32 = 1;
pub const MIGO_FRAME_INGRESS_WOULD_BLOCK: u32 = 2;
pub const MIGO_FRAME_INGRESS_REJECTED: u32 = 3;
pub const MIGO_FRAME_INGRESS_GENERATION_LOST: u32 = 4;

/// One ingress answer.
///
/// `accepted_sequence` is placed before the 32-bit fields so the record is 32
/// bytes with no interior padding on both LP64 and ILP32 -- one layout, rather
/// than two that happen to agree today.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MigoFrameIngressOutcome {
    pub header: VersionedHeader,
    pub accepted_sequence: u64,
    pub decision: u32,
    pub remaining_credits: u32,
    pub wire_error_code: u32,
    pub reserved0: u32,
}

// SAFETY: every field has an all-zero representation and v1 requires the
// complete record.
unsafe impl AbiStruct for MigoFrameIngressOutcome {}

/// Write one ingress answer without exposing Rust layout or padding.
///
/// The consistency rules are enforced here rather than trusted from the caller,
/// because they are what the host's own logic keys on. A `REJECTED` carrying a
/// sequence number, or an `ACCEPTED` carrying an error code, would each be a
/// contradiction the host has no way to resolve -- and the natural bug is to
/// forget to clear one field on an early return.
///
/// # Safety
/// `out` must satisfy [`write_versioned_output`]'s output contract.
pub unsafe fn write_frame_ingress_outcome(
    out: *mut MigoFrameIngressOutcome,
    decision: u32,
    remaining_credits: u32,
    accepted_sequence: u64,
    wire_error_code: u32,
) -> MigoResult {
    let recognised = matches!(
        decision,
        MIGO_FRAME_INGRESS_ACCEPTED
            | MIGO_FRAME_INGRESS_WOULD_BLOCK
            | MIGO_FRAME_INGRESS_REJECTED
            | MIGO_FRAME_INGRESS_GENERATION_LOST
    );
    if !recognised {
        return MIGO_ERROR_INVALID_ARGUMENT;
    }
    // A sequence number means "this packet was taken". Only ACCEPTED took one.
    if (decision == MIGO_FRAME_INGRESS_ACCEPTED) != (accepted_sequence != 0) {
        return MIGO_ERROR_INVALID_ARGUMENT;
    }
    // And an error code means "these bytes were refused". Only REJECTED refuses:
    // GENERATION_LOST is not a fault, and WOULD_BLOCK is not a verdict on the
    // bytes at all.
    if (decision == MIGO_FRAME_INGRESS_REJECTED) != (wire_error_code != 0) {
        return MIGO_ERROR_INVALID_ARGUMENT;
    }
    // Backpressure is level-triggered: WOULD_BLOCK is the answer precisely when
    // no credit remains, so a WOULD_BLOCK advertising credit would tell the
    // producer to retry immediately into the same refusal.
    if (decision == MIGO_FRAME_INGRESS_WOULD_BLOCK) && remaining_credits != 0 {
        return MIGO_ERROR_INVALID_ARGUMENT;
    }

    let value = MigoFrameIngressOutcome {
        header: VersionedHeader {
            struct_size: size_of::<MigoFrameIngressOutcome>() as u32,
            abi_version: crate::MIGO_ABI_VERSION_CURRENT,
        },
        accepted_sequence,
        decision,
        remaining_credits,
        wire_error_code,
        reserved0: 0,
    };

    // SAFETY: forwarded from this function's contract; `value` is a distinct
    // local, fully initialized ABI record.
    let result = unsafe { write_versioned_output(out, &value, OutputVersionPolicy::CurrentAbi) };
    debug_assert!(result != MIGO_OK || !out.is_null());
    result
}

const _: () = assert!(size_of::<MigoFrameIngressOutcome>() == 32);
const _: () = assert!(offset_of!(MigoFrameIngressOutcome, header) == 0);
const _: () = assert!(offset_of!(MigoFrameIngressOutcome, accepted_sequence) == 8);
const _: () = assert!(offset_of!(MigoFrameIngressOutcome, decision) == 16);
const _: () = assert!(offset_of!(MigoFrameIngressOutcome, remaining_credits) == 20);
const _: () = assert!(offset_of!(MigoFrameIngressOutcome, wire_error_code) == 24);
const _: () = assert!(offset_of!(MigoFrameIngressOutcome, reserved0) == 28);

// ---------------------------------------------------------------------------
// The synchronous barrier
// ---------------------------------------------------------------------------

pub const MIGO_SYNC_STATE_FREE: u32 = 0;
pub const MIGO_SYNC_STATE_PENDING: u32 = 1;
pub const MIGO_SYNC_STATE_READY: u32 = 2;
pub const MIGO_SYNC_STATE_FAILED: u32 = 3;
pub const MIGO_SYNC_STATE_CANCELLED: u32 = 4;

/// Why a synchronous request failed.
///
/// Stable across releases: the producer turns these into exceptions its own
/// code catches, so a renumbering here is a renumbering of somebody's `catch`.
/// Mirrors `include/migo/external_frames.h` field for field; the C-layout gate
/// is what keeps the two from drifting.
pub const MIGO_SYNC_ERROR_ALREADY_PENDING: u32 = 1;
pub const MIGO_SYNC_ERROR_REQUEST_ID_MISMATCH: u32 = 2;
pub const MIGO_SYNC_ERROR_STALE_GENERATION: u32 = 3;
pub const MIGO_SYNC_ERROR_REPLY_TOO_LARGE: u32 = 4;
pub const MIGO_SYNC_ERROR_TIMED_OUT: u32 = 5;
pub const MIGO_SYNC_ERROR_SESSION_ENDED: u32 = 6;
pub const MIGO_SYNC_ERROR_UNSUPPORTED_OPERATION: u32 = 7;
pub const MIGO_SYNC_ERROR_LATE_REPLY: u32 = 8;
pub const MIGO_SYNC_ERROR_BAD_DEADLINE: u32 = 9;
pub const MIGO_SYNC_ERROR_BAD_REPLY_RESERVATION: u32 = 10;
/// The host implements the operation, tried it, and it failed. Distinct from
/// UNSUPPORTED_OPERATION because the two say opposite things about whether to
/// ask again.
pub const MIGO_SYNC_ERROR_OPERATION_FAILED: u32 = 11;

/// Which call the producer blocked in.
pub const MIGO_SYNC_OP_READ_PIXELS: u32 = 1;

/// Caller-written. The 64-bit members precede the 32-bit ones so the record is
/// 56 bytes with no interior padding on both LP64 and ILP32.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MigoSyncRequestDescriptor {
    pub struct_size: u32,
    pub abi_version: u32,
    pub runtime_generation: u64,
    pub surface_generation: u64,
    pub resource_epoch: u64,
    pub triggering_sequence: u64,
    pub deadline_nanos: u64,
    pub operation: u32,
    pub max_reply_bytes: u32,
}

/// One answer to a synchronous request.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MigoSyncOutcome {
    pub header: VersionedHeader,
    pub request_id: u32,
    pub state: u32,
    pub reply_bytes: u32,
    pub error: u32,
}

// SAFETY: every field has an all-zero representation and v1 requires the
// complete record.
unsafe impl AbiStruct for MigoSyncOutcome {}

/// Write one synchronous answer without exposing Rust layout or padding.
///
/// The consistency rules are enforced here rather than trusted from the caller,
/// for the same reason the ingress outcome enforces its own: a `READY` carrying
/// an error, or a `FAILED` carrying reply bytes, is a contradiction the
/// producer has no way to resolve -- and it is blocked while it reads this.
///
/// # Safety
/// `out` must satisfy [`write_versioned_output`]'s output contract.
pub unsafe fn write_sync_outcome(
    out: *mut MigoSyncOutcome,
    request_id: u32,
    state: u32,
    reply_bytes: u32,
    error: u32,
) -> MigoResult {
    let recognised = matches!(
        state,
        MIGO_SYNC_STATE_FREE
            | MIGO_SYNC_STATE_PENDING
            | MIGO_SYNC_STATE_READY
            | MIGO_SYNC_STATE_FAILED
            | MIGO_SYNC_STATE_CANCELLED
    );
    let consistent = match state {
        MIGO_SYNC_STATE_READY => error == 0,
        MIGO_SYNC_STATE_FAILED => error != 0 && reply_bytes == 0,
        MIGO_SYNC_STATE_FREE => request_id == 0 && reply_bytes == 0 && error == 0,
        _ => reply_bytes == 0,
    };
    if !recognised || !consistent {
        return MIGO_ERROR_INVALID_ARGUMENT;
    }

    let value = MigoSyncOutcome {
        header: VersionedHeader {
            struct_size: size_of::<MigoSyncOutcome>() as u32,
            abi_version: 1,
        },
        request_id,
        state,
        reply_bytes,
        error,
    };
    unsafe { write_versioned_output(out, &value, OutputVersionPolicy::CurrentAbi) }
}

// ---------------------------------------------------------------------------
// The resource lane
// ---------------------------------------------------------------------------

pub const MIGO_RESOURCE_STATE_RESERVED: u32 = 0;
pub const MIGO_RESOURCE_STATE_UPLOADING: u32 = 1;
pub const MIGO_RESOURCE_STATE_VERIFYING: u32 = 2;
pub const MIGO_RESOURCE_STATE_READY: u32 = 3;
pub const MIGO_RESOURCE_STATE_FAILED: u32 = 4;

/// Caller-written.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MigoResourceReservationDescriptor {
    pub struct_size: u32,
    pub abi_version: u32,
    pub total_bytes: u64,
    pub deadline_nanos: u64,
    pub chunk_count: u32,
    pub format: u32,
    pub sha256: [u8; 32],
}

/// One answer about a resource upload.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MigoResourceOutcome {
    pub header: VersionedHeader,
    pub reservation_id: u64,
    pub received_bytes: u64,
    pub state: u32,
    pub error: u32,
    pub next_chunk: u32,
    pub reserved0: u32,
}

// SAFETY: every field has an all-zero representation and v1 requires the
// complete record.
unsafe impl AbiStruct for MigoResourceOutcome {}

/// Write one resource answer without exposing Rust layout or padding.
///
/// # Safety
/// `out` must satisfy [`write_versioned_output`]'s output contract.
pub unsafe fn write_resource_outcome(
    out: *mut MigoResourceOutcome,
    reservation_id: u64,
    received_bytes: u64,
    state: u32,
    error: u32,
    next_chunk: u32,
) -> MigoResult {
    let recognised = matches!(
        state,
        MIGO_RESOURCE_STATE_RESERVED
            | MIGO_RESOURCE_STATE_UPLOADING
            | MIGO_RESOURCE_STATE_VERIFYING
            | MIGO_RESOURCE_STATE_READY
            | MIGO_RESOURCE_STATE_FAILED
    );
    // A READY resource that reports an error is a resource a frame may name and
    // a host has been told not to trust; there is no reading of that pair a
    // consumer can act on.
    let consistent = match state {
        MIGO_RESOURCE_STATE_FAILED => error != 0,
        _ => error == 0,
    };
    if !recognised || !consistent {
        return MIGO_ERROR_INVALID_ARGUMENT;
    }

    let value = MigoResourceOutcome {
        header: VersionedHeader {
            struct_size: size_of::<MigoResourceOutcome>() as u32,
            abi_version: 1,
        },
        reservation_id,
        received_bytes,
        state,
        error,
        next_chunk,
        reserved0: 0,
    };
    unsafe { write_versioned_output(out, &value, OutputVersionPolicy::CurrentAbi) }
}

// Layout, pinned on both widths. The comment on `MigoFrameIngressOutcome`
// explains the ordering rule these follow.
const _: () = assert!(size_of::<MigoSyncRequestDescriptor>() == 56);
const _: () = assert!(offset_of!(MigoSyncRequestDescriptor, runtime_generation) == 8);
const _: () = assert!(offset_of!(MigoSyncRequestDescriptor, surface_generation) == 16);
const _: () = assert!(offset_of!(MigoSyncRequestDescriptor, resource_epoch) == 24);
const _: () = assert!(offset_of!(MigoSyncRequestDescriptor, triggering_sequence) == 32);
const _: () = assert!(offset_of!(MigoSyncRequestDescriptor, deadline_nanos) == 40);
const _: () = assert!(offset_of!(MigoSyncRequestDescriptor, operation) == 48);
const _: () = assert!(offset_of!(MigoSyncRequestDescriptor, max_reply_bytes) == 52);

const _: () = assert!(size_of::<MigoSyncOutcome>() == 24);
const _: () = assert!(offset_of!(MigoSyncOutcome, header) == 0);
const _: () = assert!(offset_of!(MigoSyncOutcome, request_id) == 8);
const _: () = assert!(offset_of!(MigoSyncOutcome, state) == 12);
const _: () = assert!(offset_of!(MigoSyncOutcome, reply_bytes) == 16);
const _: () = assert!(offset_of!(MigoSyncOutcome, error) == 20);

const _: () = assert!(size_of::<MigoResourceReservationDescriptor>() == 64);
const _: () = assert!(offset_of!(MigoResourceReservationDescriptor, total_bytes) == 8);
const _: () = assert!(offset_of!(MigoResourceReservationDescriptor, deadline_nanos) == 16);
const _: () = assert!(offset_of!(MigoResourceReservationDescriptor, chunk_count) == 24);
const _: () = assert!(offset_of!(MigoResourceReservationDescriptor, format) == 28);
const _: () = assert!(offset_of!(MigoResourceReservationDescriptor, sha256) == 32);

const _: () = assert!(size_of::<MigoResourceOutcome>() == 40);
const _: () = assert!(offset_of!(MigoResourceOutcome, reservation_id) == 8);
const _: () = assert!(offset_of!(MigoResourceOutcome, received_bytes) == 16);
const _: () = assert!(offset_of!(MigoResourceOutcome, state) == 24);
const _: () = assert!(offset_of!(MigoResourceOutcome, error) == 28);
const _: () = assert!(offset_of!(MigoResourceOutcome, next_chunk) == 32);
const _: () = assert!(offset_of!(MigoResourceOutcome, reserved0) == 36);

// ---------------------------------------------------------------------------
// Creating an external-frame session
// ---------------------------------------------------------------------------

/// Caller-written. See the header for why the launch nonce comes from the host
/// rather than from here.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MigoExternalSessionDescriptor {
    pub struct_size: u32,
    pub abi_version: u32,
    pub launch_nonce: [u8; 16],
    pub max_packet_bytes: u32,
    pub max_credits: u32,
}

/// Sixteen caller-supplied bytes as a launch identity, or `None` for all-zero.
///
/// One function, because two records carry this field -- the external session
/// descriptor and `MigoSessionConfig` -- and they have to read it identically.
/// A second conversion would be a second chance to pick the wrong endianness,
/// and the symptom would be a session that rejects every packet as foreign with
/// no indication that the two sides disagree about byte order rather than about
/// the value.
///
/// Little-endian, matching the wire header the nonce is compared against.
///
/// Rejecting all-zero is not paranoia about a weak random source: it is what an
/// uninitialised struct holds, and a session that accepted it would accept
/// packets from any other caller who also failed to initialise theirs.
pub fn launch_nonce_from_bytes(bytes: [u8; 16]) -> Option<u128> {
    let nonce = u128::from_le_bytes(bytes);
    (nonce != 0).then_some(nonce)
}

/// The descriptor's launch identity. See [`launch_nonce_from_bytes`].
pub fn launch_nonce_of(descriptor: &MigoExternalSessionDescriptor) -> Option<u128> {
    launch_nonce_from_bytes(descriptor.launch_nonce)
}

const _: () = assert!(size_of::<MigoExternalSessionDescriptor>() == 32);
const _: () = assert!(offset_of!(MigoExternalSessionDescriptor, struct_size) == 0);
const _: () = assert!(offset_of!(MigoExternalSessionDescriptor, abi_version) == 4);
const _: () = assert!(offset_of!(MigoExternalSessionDescriptor, launch_nonce) == 8);
const _: () = assert!(offset_of!(MigoExternalSessionDescriptor, max_packet_bytes) == 24);
const _: () = assert!(offset_of!(MigoExternalSessionDescriptor, max_credits) == 28);

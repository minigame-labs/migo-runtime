#include <migo/external_frames.h>

#include <stddef.h>
#include <stdint.h>

/*
 * One layout on both pointer widths, not two that happen to agree. The record
 * holds no pointer and places its 64-bit member before the 32-bit ones, so
 * these offsets are outside the `#if UINTPTR_MAX` split every other platform
 * record needs -- and being outside it is the assertion. A field reordered into
 * the 32-bit run would still be 32 bytes on LP64 and would move on ILP32.
 */
_Static_assert(sizeof(MigoFrameIngressOutcome) == 32,
               "ingress outcome is 32 bytes on every target");
_Static_assert(offsetof(MigoFrameIngressOutcome, struct_size) == 0,
               "size prefix");
_Static_assert(offsetof(MigoFrameIngressOutcome, abi_version) == 4,
               "ABI prefix");
_Static_assert(offsetof(MigoFrameIngressOutcome, accepted_sequence) == 8,
               "the 64-bit member stays ahead of the 32-bit run");
_Static_assert(offsetof(MigoFrameIngressOutcome, decision) == 16,
               "decision offset");
_Static_assert(offsetof(MigoFrameIngressOutcome, remaining_credits) == 20,
               "remaining_credits offset");
_Static_assert(offsetof(MigoFrameIngressOutcome, wire_error_code) == 24,
               "wire_error_code offset");
_Static_assert(offsetof(MigoFrameIngressOutcome, reserved0) == 28,
               "reserved0 offset");

/*
 * Four distinct decisions. A host branches on these, and two that collide would
 * merge two different required behaviours: WOULD_BLOCK means wait and resend,
 * REJECTED means never resend these bytes.
 */
_Static_assert(MIGO_FRAME_INGRESS_ACCEPTED != MIGO_FRAME_INGRESS_WOULD_BLOCK, "1 != 2");
_Static_assert(MIGO_FRAME_INGRESS_ACCEPTED != MIGO_FRAME_INGRESS_REJECTED, "1 != 3");
_Static_assert(MIGO_FRAME_INGRESS_ACCEPTED != MIGO_FRAME_INGRESS_GENERATION_LOST, "1 != 4");
_Static_assert(MIGO_FRAME_INGRESS_WOULD_BLOCK != MIGO_FRAME_INGRESS_REJECTED, "2 != 3");
_Static_assert(MIGO_FRAME_INGRESS_WOULD_BLOCK != MIGO_FRAME_INGRESS_GENERATION_LOST, "2 != 4");
_Static_assert(MIGO_FRAME_INGRESS_REJECTED != MIGO_FRAME_INGRESS_GENERATION_LOST, "3 != 4");
/* DEFERRED, appended: a held packet must not read as any answer a host acts on. */
_Static_assert(MIGO_FRAME_INGRESS_DEFERRED == UINT32_C(5), "DEFERRED is appended, not renumbered");
_Static_assert(MIGO_FRAME_INGRESS_DEFERRED != MIGO_FRAME_INGRESS_ACCEPTED, "5 != 1");
_Static_assert(MIGO_FRAME_INGRESS_DEFERRED != MIGO_FRAME_INGRESS_WOULD_BLOCK, "5 != 2");
_Static_assert(MIGO_FRAME_INGRESS_DEFERRED != MIGO_FRAME_INGRESS_REJECTED, "5 != 3");
_Static_assert(MIGO_FRAME_INGRESS_DEFERRED != MIGO_FRAME_INGRESS_GENERATION_LOST, "5 != 4");

/* Zero is not a decision: a zeroed record must not read as a valid answer. */
_Static_assert(MIGO_FRAME_INGRESS_ACCEPTED != UINT32_C(0), "zero is not ACCEPTED");
_Static_assert(MIGO_FRAME_INGRESS_WOULD_BLOCK != UINT32_C(0), "zero is not WOULD_BLOCK");
_Static_assert(MIGO_FRAME_INGRESS_REJECTED != UINT32_C(0), "zero is not REJECTED");
_Static_assert(MIGO_FRAME_INGRESS_GENERATION_LOST != UINT32_C(0), "zero is not GENERATION_LOST");


/* ---------------------------------------------------------------------------
 * The synchronous barrier
 *
 * Same rule as above: 64-bit members ahead of the 32-bit run, so there is one
 * layout on both pointer widths and these offsets sit outside the
 * `#if UINTPTR_MAX` split. A field reordered into the 32-bit run would still
 * measure 56 bytes on LP64 and would move on ILP32.
 * ------------------------------------------------------------------------- */

_Static_assert(sizeof(MigoSyncRequestDescriptor) == 56,
               "sync request is 56 bytes on every target");
_Static_assert(offsetof(MigoSyncRequestDescriptor, runtime_generation) == 8, "");
_Static_assert(offsetof(MigoSyncRequestDescriptor, surface_generation) == 16, "");
_Static_assert(offsetof(MigoSyncRequestDescriptor, resource_epoch) == 24, "");
_Static_assert(offsetof(MigoSyncRequestDescriptor, triggering_sequence) == 32, "");
_Static_assert(offsetof(MigoSyncRequestDescriptor, deadline_nanos) == 40, "");
_Static_assert(offsetof(MigoSyncRequestDescriptor, operation) == 48, "");
_Static_assert(offsetof(MigoSyncRequestDescriptor, max_reply_bytes) == 52, "");

_Static_assert(sizeof(MigoSyncOutcome) == 24, "sync outcome is 24 bytes on every target");
_Static_assert(offsetof(MigoSyncOutcome, request_id) == 8, "");
_Static_assert(offsetof(MigoSyncOutcome, state) == 12, "");
_Static_assert(offsetof(MigoSyncOutcome, reply_bytes) == 16, "");
_Static_assert(offsetof(MigoSyncOutcome, error) == 20, "");

/*
 * FREE is zero, and that is deliberate: a zeroed mailbox reads as "no request",
 * which is the only state where reading stale fields is harmless. Every other
 * state is non-zero, so a zeroed record can never be mistaken for an answer.
 */
_Static_assert(MIGO_SYNC_STATE_FREE == UINT32_C(0), "a zeroed mailbox holds no request");
_Static_assert(MIGO_SYNC_STATE_PENDING != UINT32_C(0), "zero is not PENDING");
_Static_assert(MIGO_SYNC_STATE_READY != UINT32_C(0), "zero is not READY");
_Static_assert(MIGO_SYNC_STATE_FAILED != UINT32_C(0), "zero is not FAILED");
_Static_assert(MIGO_SYNC_STATE_CANCELLED != UINT32_C(0), "zero is not CANCELLED");

/*
 * The one-body call. The answer header is the wire format's four 32-bit words,
 * and a call body carries at least the 48-byte envelope, so a bound below that
 * would refuse every call.
 */
_Static_assert(MIGO_SYNC_ANSWER_HEADER_BYTES == 16U, "four little-endian words");
_Static_assert(MIGO_SYNC_CALL_MAX_BYTES >= 48U + MIGO_SYNC_READ_PIXELS_PARAM_BYTES,
               "the body bound admits readPixels");

/* Zero is not a failure reason: a FAILED record must say why. */
_Static_assert(MIGO_SYNC_ERROR_ALREADY_PENDING != UINT32_C(0), "");
_Static_assert(MIGO_SYNC_ERROR_BAD_REPLY_RESERVATION == UINT32_C(10),
               "the reason codes are contiguous from one; a gap means a retired code");

/* ---------------------------------------------------------------------------
 * The resource lane
 * ------------------------------------------------------------------------- */

_Static_assert(sizeof(MigoResourceReservationDescriptor) == 64,
               "resource reservation is 64 bytes on every target");
_Static_assert(offsetof(MigoResourceReservationDescriptor, total_bytes) == 8, "");
_Static_assert(offsetof(MigoResourceReservationDescriptor, deadline_nanos) == 16, "");
_Static_assert(offsetof(MigoResourceReservationDescriptor, chunk_count) == 24, "");
_Static_assert(offsetof(MigoResourceReservationDescriptor, format) == 28, "");
_Static_assert(offsetof(MigoResourceReservationDescriptor, sha256) == 32, "");
_Static_assert(sizeof(((MigoResourceReservationDescriptor *)0)->sha256) == 32,
               "a SHA-256 is thirty-two bytes; a shorter array would compare a prefix");

_Static_assert(sizeof(MigoResourceOutcome) == 40,
               "resource outcome is 40 bytes on every target");
_Static_assert(offsetof(MigoResourceOutcome, reservation_id) == 8, "");
_Static_assert(offsetof(MigoResourceOutcome, received_bytes) == 16, "");
_Static_assert(offsetof(MigoResourceOutcome, state) == 24, "");
_Static_assert(offsetof(MigoResourceOutcome, error) == 28, "");
_Static_assert(offsetof(MigoResourceOutcome, next_chunk) == 32, "");
_Static_assert(offsetof(MigoResourceOutcome, reserved0) == 36, "");

/*
 * RESERVED is zero, so a zeroed outcome reads as "declared, nothing arrived" --
 * the state in which a frame may not name the resource. READY being non-zero is
 * the load-bearing half: a zeroed record must never say a resource is usable.
 */
_Static_assert(MIGO_RESOURCE_STATE_RESERVED == UINT32_C(0), "");
_Static_assert(MIGO_RESOURCE_STATE_READY != UINT32_C(0),
               "a zeroed record must not claim a resource is ready to name");
_Static_assert(MIGO_RESOURCE_STATE_FAILED != UINT32_C(0), "");
_Static_assert(MIGO_RESOURCE_ERROR_DIGEST_MISMATCH != UINT32_C(0), "");


/* ---------------------------------------------------------------------------
 * Creating an external-frame session
 * ------------------------------------------------------------------------- */

_Static_assert(sizeof(MigoExternalSessionDescriptor) == 32,
               "external session descriptor is 32 bytes on every target");
_Static_assert(offsetof(MigoExternalSessionDescriptor, launch_nonce) == 8,
               "the nonce follows the versioned prefix");
_Static_assert(sizeof(((MigoExternalSessionDescriptor *)0)->launch_nonce) == 16,
               "128 bits: the nonce is guessed against, not collided against");
_Static_assert(offsetof(MigoExternalSessionDescriptor, max_packet_bytes) == 24, "");
_Static_assert(offsetof(MigoExternalSessionDescriptor, max_credits) == 28, "");

int migo_external_frames_c_contract(void) {
    MigoFrameIngressOutcome outcome = {0};
    outcome.struct_size = (uint32_t)sizeof outcome;
    outcome.decision = MIGO_FRAME_INGRESS_ACCEPTED;

    MigoSyncRequestDescriptor request = {0};
    request.struct_size = (uint32_t)sizeof request;
    request.max_reply_bytes = 4096u;

    MigoSyncOutcome answer = {0};
    answer.struct_size = (uint32_t)sizeof answer;
    answer.state = MIGO_SYNC_STATE_READY;

    MigoResourceReservationDescriptor reservation = {0};
    reservation.struct_size = (uint32_t)sizeof reservation;
    reservation.chunk_count = 1u;

    MigoResourceOutcome resource = {0};
    resource.struct_size = (uint32_t)sizeof resource;
    resource.state = MIGO_RESOURCE_STATE_READY;

    MigoExternalSessionDescriptor session = {0};
    session.struct_size = (uint32_t)sizeof session;
    session.launch_nonce[0] = 1u;

    return (int)(outcome.struct_size + outcome.decision + request.struct_size
                 + answer.struct_size + reservation.struct_size + resource.struct_size
                 + session.struct_size + session.launch_nonce[0]);
}

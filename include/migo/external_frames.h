#ifndef MIGO_EXTERNAL_FRAMES_H_
#define MIGO_EXTERNAL_FRAMES_H_

#include <migo/types.h>

/*
 * The record a host reads back after offering one frame produced outside this
 * process.
 *
 * On iOS the producer is a Worker inside WebKit's WebContent process: content
 * JavaScript runs there to get the system's JIT, encodes a frame's drawing work
 * into one bounded binary packet, and sends it here, where Migo renders it. The
 * Swift transport that carries those bytes never touches a Rust type and never
 * links a JavaScript engine; this record is the whole of what it sees coming
 * back.
 *
 * THE ENTRY POINTS ARE HERE AND SO IS WHAT IS BEHIND THEM. This comment used to
 * say the opposite -- "declarations only, on purpose... there is no submit
 * function here yet" -- and it was written when that was true. The rule it
 * states is still the rule and is worth keeping: an entry point lands with its
 * implementation, because an exported symbol that always fails is the shape that
 * shipped a Windows SDK which loaded, resolved every entry point, and could
 * attach nothing.
 *
 * What changed is that the implementation landed and the comment did not. A
 * public header describing an API it does not have is the same failure the rule
 * exists to prevent, pointed the other way: a reader who trusts it plans around
 * a submit function that is right there.
 */

typedef uint32_t MigoFrameIngressDecision;

/* Taken. A credit is consumed until the renderer reports completion. */
#define MIGO_FRAME_INGRESS_ACCEPTED 1U
/*
 * Legal, but no credit is available. The producer must wait. It must not drop
 * the packet: a frame may carry state or resource changes that a later frame
 * depends on, so "skip it" is not a correct answer on either side.
 */
#define MIGO_FRAME_INGRESS_WOULD_BLOCK 2U
/*
 * Malformed, or not addressed to this session. Costs no credit -- otherwise a
 * producer sending garbage would exhaust its own window and stall, which reads
 * on a device as a hang rather than as bad input. The producer must not resend
 * the same bytes.
 */
#define MIGO_FRAME_INGRESS_REJECTED 3U
/*
 * Correct bytes for a runtime generation that no longer exists -- the WebContent
 * process was replaced, or the session reloaded. Distinct from REJECTED because
 * nobody did anything wrong and no retry helps; reporting it as an error sends
 * whoever reads the telemetry looking for a bug that is not there.
 */
#define MIGO_FRAME_INGRESS_GENERATION_LOST 4U

/*
 * Library-written, so it grows append-only: a caller compiled against an
 * earlier version must keep reading the same bytes at the same offsets.
 *
 * accepted_sequence precedes the 32-bit fields deliberately. Placing the 64-bit
 * member first makes the record 32 bytes with no interior padding on both LP64
 * and ILP32, so there is one layout rather than two that happen to agree.
 */
typedef struct MigoFrameIngressOutcome {
    uint32_t struct_size;
    uint32_t abi_version;
    /* Non-zero only for ACCEPTED. */
    uint64_t accepted_sequence;
    MigoFrameIngressDecision decision;
    uint32_t remaining_credits;
    /*
     * Non-zero only for REJECTED. Stable across releases so production
     * telemetry can tell "the producer sent a short packet" apart from "the
     * producer sent another session's packet". Envelope failures are numbered
     * from 1; identity and ordering failures from 1001, so one field carries
     * either without ambiguity.
     */
    uint32_t wire_error_code;
    uint32_t reserved0;
} MigoFrameIngressOutcome;


/* ---------------------------------------------------------------------------
 * The synchronous barrier
 *
 * A handful of calls cannot be answered on the producer's side because their
 * return value *is* the answer: readPixels, getImageData, toDataURL. The
 * producer blocks; the transport carries the request here and the reply back.
 *
 * One request may be outstanding per session. A second would need a second
 * mailbox and a second waiter, and the producer is a single agent that is
 * blocked while it waits.
 *
 * The entry points are below the records, and they arrived with the session
 * that implements them rather than ahead of it -- an exported symbol that
 * always fails is the shape that shipped a Windows SDK which loaded, resolved
 * every entry point, and could attach nothing.
 * ------------------------------------------------------------------------- */

typedef uint32_t MigoSyncState;

/* No request outstanding. The only state a new one may be posted from. */
#define MIGO_SYNC_STATE_FREE      0U
/* Posted, and the producer is waiting. */
#define MIGO_SYNC_STATE_PENDING   1U
/* Answered; reply_bytes says how much of the reply buffer is the answer. */
#define MIGO_SYNC_STATE_READY     2U
/* Not answered and will not be; error says why. */
#define MIGO_SYNC_STATE_FAILED    3U
/* Withdrawn by the producer before an answer arrived. */
#define MIGO_SYNC_STATE_CANCELLED 4U

/*
 * Why a synchronous request failed. Stable across releases: the producer turns
 * these into exceptions its own code catches.
 */
typedef uint32_t MigoSyncError;
#define MIGO_SYNC_ERROR_ALREADY_PENDING        1U
#define MIGO_SYNC_ERROR_REQUEST_ID_MISMATCH    2U
#define MIGO_SYNC_ERROR_STALE_GENERATION       3U
#define MIGO_SYNC_ERROR_REPLY_TOO_LARGE        4U
#define MIGO_SYNC_ERROR_TIMED_OUT              5U
#define MIGO_SYNC_ERROR_SESSION_ENDED          6U
#define MIGO_SYNC_ERROR_UNSUPPORTED_OPERATION  7U
#define MIGO_SYNC_ERROR_LATE_REPLY             8U
#define MIGO_SYNC_ERROR_BAD_DEADLINE           9U
#define MIGO_SYNC_ERROR_BAD_REPLY_RESERVATION  10U
/*
 * The host implements the operation, tried it, and it failed.
 *
 * Distinct from MIGO_SYNC_ERROR_UNSUPPORTED_OPERATION because the two say
 * opposite things about whether to ask again: "this host does not do
 * readPixels" is permanent and a producer told that will stop asking, while
 * "the readback failed this time" is not. Mapping a driver error onto the
 * permanent one would turn one transient GL failure into a session that never
 * reads a pixel again.
 */
#define MIGO_SYNC_ERROR_OPERATION_FAILED       11U

/*
 * Caller-written. The 64-bit members precede the 32-bit ones so the record is
 * 56 bytes with no interior padding on both LP64 and ILP32 -- one layout,
 * rather than two that happen to agree today.
 *
 * deadline_nanos is a MONOTONIC clock reading, not wall time. A producer that
 * blocked across a clock adjustment would otherwise wake early or never.
 */
typedef struct MigoSyncRequestDescriptor {
    uint32_t struct_size;
    uint32_t abi_version;
    uint64_t runtime_generation;
    uint64_t surface_generation;
    uint64_t resource_epoch;
    /* The frame the producer had submitted when it blocked. */
    uint64_t triggering_sequence;
    uint64_t deadline_nanos;
    uint32_t operation;
    /* What the producer reserved; the reply is refused, never truncated. */
    uint32_t max_reply_bytes;
} MigoSyncRequestDescriptor;

/* Library-written, append-only. */
typedef struct MigoSyncOutcome {
    uint32_t struct_size;
    uint32_t abi_version;
    /* Monotonic, never zero: a cleared mailbox holds zero. */
    uint32_t request_id;
    MigoSyncState state;
    /* Non-zero only for READY. */
    uint32_t reply_bytes;
    /* Non-zero only for FAILED. */
    MigoSyncError error;
} MigoSyncOutcome;

/*
 * Which call the producer blocked in.
 *
 * Numbered rather than named, and stable: the producer writes one of these into
 * the record and the library dispatches on it.
 */
#define MIGO_SYNC_OP_READ_PIXELS 1U

/*
 * `readPixels`' arguments, which are NOT in the descriptor.
 *
 * The descriptor is a fixed rendezvous record the producer polls with atomics;
 * carrying per-operation arguments in it would size it by the largest operation
 * anyone ever adds. They travel beside the request instead, as eight
 * little-endian 32-bit words -- MIGO_SYNC_READ_PIXELS_PARAM_BYTES below is the
 * byte count, not the word count -- in this order:
 *
 *     canvas_id, x, y, width, height, format, type, reserved
 *
 * `format` must be GL_RGBA (0x1908) and `type` GL_UNSIGNED_BYTE (0x1401).
 * Anything else is refused with MIGO_SYNC_ERROR_UNSUPPORTED_OPERATION rather
 * than answered as if it were RGBA8 -- a buffer whose bytes mean something else
 * is a wrong answer that looks like a right one. The reply is
 * width * height * 4 bytes.
 */
#define MIGO_SYNC_READ_PIXELS_PARAM_BYTES 32U

/*
 * Post one synchronous request and answer it.
 *
 * The producer blocks, the transport carries the request here, this answers it,
 * and migo_session_take_sync_reply carries the bytes back.
 *
 * now_nanos is the CALLER'S monotonic clock reading, on the same clock
 * deadline_nanos is expressed on. The library reads no clock of its own for
 * this, deliberately: two clocks that agree today are a defect waiting for the
 * platform where they do not, and only the host can read the clock its producer
 * blocked against.
 *
 * params points at the operation's arguments; see the operation's own
 * definition above for the layout. It may be NULL only when param_bytes is 0.
 *
 * A request the library could not answer still returns MIGO_OK and reports
 * MIGO_SYNC_STATE_FAILED with a reason, the same way
 * migo_session_submit_external_frame reports a rejection rather than failing:
 * the call did its job and the verdict belongs where the producer reads it. A
 * request refused before it was given an id reports request_id 0.
 *
 * request IS CALLER-WRITTEN. Set its struct_size and abi_version too: the same
 * rule applies to it as to out_outcome, and a descriptor whose header is not
 * initialised is refused before the request is judged.
 *
 * One request may be outstanding per session. A second while one is pending is
 * refused with MIGO_SYNC_ERROR_ALREADY_PENDING.
 *
 * out_outcome IS CALLER-OWNED AND ITS HEADER IS AN INPUT. Set struct_size and
 * abi_version before every call: struct_size is what bounds the write into your
 * storage, so a record that arrives with a size this library does not recognise
 * is refused rather than filled in, and the call returns
 * MIGO_ERROR_INVALID_ARGUMENT. A record left holding zeros is refused too, with
 * MIGO_ERROR_UNSUPPORTED_ABI, because it claims abi_version 0 and that is
 * checked first. A producer is blocked while this is decided, so a record left
 * zeroed is a producer that waits out its whole deadline for a refusal that
 * never reached it.
 */
MIGO_API MigoResult MIGO_CALL migo_session_post_sync_request(
    MigoSession *session, const MigoSyncRequestDescriptor *request,
    const uint8_t *params, size_t param_bytes, uint64_t now_nanos,
    MigoSyncOutcome *out_outcome);

/*
 * Where the outstanding request is.
 *
 * Also what makes a passed deadline visible: nothing else runs while a request
 * is outstanding, so a request whose deadline elapsed is settled here.
 *
 * out_outcome IS CALLER-OWNED AND ITS HEADER IS AN INPUT. Set struct_size and
 * abi_version before every call: struct_size is what bounds the write into your
 * storage, so a record that arrives with a size this library does not recognise
 * is refused rather than filled in, and the call returns
 * MIGO_ERROR_INVALID_ARGUMENT. A record left holding zeros is refused too, with
 * MIGO_ERROR_UNSUPPORTED_ABI, because it claims abi_version 0 and that is
 * checked first. A producer is blocked while this is decided, so a record left
 * zeroed is a producer that waits out its whole deadline for a refusal that
 * never reached it.
 */
MIGO_API MigoResult MIGO_CALL migo_session_poll_sync(
    MigoSession *session, uint64_t now_nanos, MigoSyncOutcome *out_outcome);

/*
 * Copy a ready answer out, and free the slot.
 *
 * Refused, never truncated, when capacity is smaller than the answer -- and the
 * answer stays MIGO_SYNC_STATE_READY, so a caller may return with a large
 * enough buffer. `*out_written` receives the byte count on success and zero on
 * failure, so a caller that ignores the result cannot read a stale count as a
 * length.
 *
 * Returns MIGO_ERROR_INVALID_STATE when there is no ready answer to take.
 */
MIGO_API MigoResult MIGO_CALL migo_session_take_sync_reply(
    MigoSession *session, uint8_t *buffer, size_t capacity,
    size_t *out_written);

/*
 * The producer withdrew its request.
 *
 * Settles an outstanding request as MIGO_SYNC_STATE_CANCELLED and frees the
 * slot for the next one. A request that has already been answered stays
 * answered: cancelling is not a way to discard a reply the producer has not
 * read yet.
 *
 * out_outcome IS CALLER-OWNED AND ITS HEADER IS AN INPUT. Set struct_size and
 * abi_version before every call: struct_size is what bounds the write into your
 * storage, so a record that arrives with a size this library does not recognise
 * is refused rather than filled in, and the call returns
 * MIGO_ERROR_INVALID_ARGUMENT. A record left holding zeros is refused too, with
 * MIGO_ERROR_UNSUPPORTED_ABI, because it claims abi_version 0 and that is
 * checked first. A producer is blocked while this is decided, so a record left
 * zeroed is a producer that waits out its whole deadline for a refusal that
 * never reached it.
 */
MIGO_API MigoResult MIGO_CALL migo_session_cancel_sync(
    MigoSession *session, uint64_t now_nanos, MigoSyncOutcome *out_outcome);

/* ---------------------------------------------------------------------------
 * The resource lane
 *
 * A frame packet is small and bounded; a texture atlas is neither. Large assets
 * are reserved, uploaded in chunks, verified against a digest declared up
 * front, and become nameable from a frame only then. The frame ceiling stays
 * small because this exists.
 *
 * Verification happens BEFORE creation. Creating the GPU object as bytes arrive
 * and fixing it up if the digest turns out wrong trades a bounded failure for
 * an unbounded one: a texture whose contents are whatever arrived, already
 * bound by a frame that referenced it.
 * ------------------------------------------------------------------------- */

typedef uint32_t MigoResourceState;
#define MIGO_RESOURCE_STATE_RESERVED  0U
#define MIGO_RESOURCE_STATE_UPLOADING 1U
#define MIGO_RESOURCE_STATE_VERIFYING 2U
/* Verified. A frame may name this resource, and not before. */
#define MIGO_RESOURCE_STATE_READY     3U
#define MIGO_RESOURCE_STATE_FAILED    4U

typedef uint32_t MigoResourceError;
#define MIGO_RESOURCE_ERROR_TOO_MANY_RESERVATIONS 1U
#define MIGO_RESOURCE_ERROR_BAD_SIZE              2U
#define MIGO_RESOURCE_ERROR_BAD_CHUNK_COUNT       3U
#define MIGO_RESOURCE_ERROR_UNKNOWN_RESERVATION   4U
#define MIGO_RESOURCE_ERROR_NON_CONTIGUOUS_CHUNK  5U
#define MIGO_RESOURCE_ERROR_CHUNK_OUT_OF_BOUNDS   6U
#define MIGO_RESOURCE_ERROR_DIGEST_MISMATCH       7U
#define MIGO_RESOURCE_ERROR_TIMED_OUT             8U
#define MIGO_RESOURCE_ERROR_EPOCH_ADVANCED        9U
#define MIGO_RESOURCE_ERROR_INCOMPLETE            10U
#define MIGO_RESOURCE_ERROR_NOT_UPLOADING         11U

/*
 * Caller-written. The reservation id is assigned by the library, not chosen
 * here: an id the producer picked could collide with one already in the table,
 * and the collision would be a frame naming the wrong texture.
 */
typedef struct MigoResourceReservationDescriptor {
    uint32_t struct_size;
    uint32_t abi_version;
    uint64_t total_bytes;
    uint64_t deadline_nanos;
    uint32_t chunk_count;
    /* Producer-declared format tag; opaque to the protocol. */
    uint32_t format;
    /* The digest the uploaded bytes must hash to. */
    uint8_t  sha256[32];
} MigoResourceReservationDescriptor;

/* Library-written, append-only. */
typedef struct MigoResourceOutcome {
    uint32_t struct_size;
    uint32_t abi_version;
    /* Non-zero once a reservation exists. */
    uint64_t reservation_id;
    uint64_t received_bytes;
    MigoResourceState state;
    /* Non-zero only for FAILED. */
    MigoResourceError error;
    /* The chunk index the next upload must carry; chunks are contiguous. */
    uint32_t next_chunk;
    uint32_t reserved0;
} MigoResourceOutcome;


/* ---------------------------------------------------------------------------
 * Creating an external-frame session
 *
 * A session in this mode owns a renderer, a surface and a frame clock, and no
 * script runtime: the content's JavaScript runs in another process. Creating
 * one is therefore a different call from creating a content session, not a flag
 * on the same one -- the two do not share a lifecycle, and
 * migo_session_load_content on a session created this way returns
 * MIGO_ERROR_INVALID_STATE rather than doing something surprising.
 *
 * THE LAUNCH NONCE IS SUPPLIED BY THE HOST, not generated here, and that is the
 * important part of this record. It is the shared secret that decides whether
 * bytes arriving from another process belong to this session, so the party that
 * owns *both* ends -- the transport and the session -- has to be the one that
 * generates it. On Apple that is the Swift host, with SecRandomCopyBytes. A
 * library that invented its own would have to hand it back out for the
 * transport to use, which is one more place for it to be logged.
 *
 * Generate it with a cryptographic source. It is 128 bits because it is
 * guessed against, not collided against, and it must not appear in a URL, a
 * query string, or a log line.
 *
 * DECLARATIONS ONLY, like the records above. The entry point lands with the
 * session implementation behind it.
 * ------------------------------------------------------------------------- */

typedef struct MigoExternalSessionDescriptor {
    uint32_t struct_size;
    uint32_t abi_version;
    /*
     * 128-bit, little-endian, from a cryptographic source. All-zero is
     * rejected: it is what an uninitialised struct holds, and a session that
     * accepted it would accept packets from anyone who also sent zeros.
     */
    uint8_t  launch_nonce[16];
    /*
     * Bytes this session will accept in one packet, or 0 for the library's
     * ceiling. A value above the ceiling is clamped down, never up: a host on a
     * memory-tight device can ask for less and nothing can ask for more.
     */
    uint32_t max_packet_bytes;
    /*
     * Frames the producer may have outstanding, or 0 for the library's default.
     * Clamped into the compile-time range the same way.
     */
    uint32_t max_credits;
} MigoExternalSessionDescriptor;


/* ---------------------------------------------------------------------------
 * Entry points
 *
 * PRESENT ONLY IN THE PERFORMANCE+ PRODUCT. The other Apple products link no
 * frame transport, so they export none of these and a host that links the wrong
 * one gets an undefined symbol when it builds -- which is the loud, early
 * failure. The alternative, a symbol that resolves and always fails, is what
 * shipped a Windows SDK that loaded, resolved every entry point, and could
 * attach nothing.
 * ------------------------------------------------------------------------- */

/*
 * Offer one frame produced outside this process.
 *
 * `bytes` is borrowed for the duration of the call only: an accepted packet is
 * copied once into a buffer the library owns before this returns, so a Swift
 * transport may hand over a Data's interior pointer and reuse or free its own
 * storage immediately.
 *
 * out_outcome IS CALLER-OWNED AND ITS HEADER IS AN INPUT. Set struct_size and
 * abi_version before every call: struct_size is what bounds the write into your
 * storage, so a record that arrives holding zeros is refused rather than filled
 * in, and the call returns MIGO_ERROR_INVALID_ARGUMENT without having looked at
 * the packet. On this entry point that mistake does not degrade anything -- it
 * refuses every frame the producer ever sends, forever, while the transport
 * itself is working -- so it is worth initialising the record once and reusing
 * it rather than re-deriving it per frame.
 *
 * Returns MIGO_OK when the outcome was written -- including when the outcome
 * says the packet was rejected. A non-OK result means the call itself could not
 * be made: a bad handle, a null buffer, an outcome record whose header was not
 * filled in, or no surface attached yet.
 */
MigoResult migo_session_submit_external_frame(MigoSession *session,
                                              const uint8_t *bytes,
                                              size_t byte_count,
                                              MigoFrameIngressOutcome *out_outcome);

/*
 * Ask for one frame.
 *
 * The producer renders when told to: Migo's requestAnimationFrame is fed by
 * host vsync on every platform, and this is that signal crossing a process
 * boundary instead of a thread boundary. MIGO_ERROR_INVALID_STATE means the
 * renderer is not up yet, which is the truthful answer for a session that
 * cannot produce a frame.
 */
MigoResult migo_session_request_external_frame(MigoSession *session);

/*
 * Drain one pending WebGL error for a canvas.
 *
 * `gl.getError()` is a synchronous call the producer makes in another process;
 * the errors the decoder recorded wait here until it asks. Writes 0
 * (GL_NO_ERROR) when the queue is empty, which is what WebGL returns.
 */
MigoResult migo_session_take_external_gl_error(MigoSession *session,
                                               uint32_t canvas_id,
                                               uint32_t *out_code);

#endif /* MIGO_EXTERNAL_FRAMES_H_ */

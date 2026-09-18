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
 * Legal and addressed to this session, but one ahead of the packet that must
 * come before it. The uplink is two independent streams, so a packet can
 * overtake its predecessor in transit. It is held, costing no credit, and
 * executed in order as soon as the predecessor arrives; the producer's verdict
 * for it is sent then. Not an error, and the host must not resend or count it
 * as refused.
 */
#define MIGO_FRAME_INGRESS_DEFERRED 5U

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
 * Wait for the frame window to open, and report it.
 *
 * For a producer whose calls are synchronous and that must send a barrier
 * packet (a packet without PRESENT) while the renderer holds every credit. The
 * library answers once every packet through `triggering_sequence` is admitted
 * and a credit is free. No parameters. The reply is MIGO_SYNC_WINDOW_REPLY_BYTES:
 * remaining credits (u32), a zero u32, the accepted sequence (u64), all
 * little-endian -- the same advertisement a frame verdict or clock tick carries.
 */
#define MIGO_SYNC_OP_AWAIT_WINDOW 2U
#define MIGO_SYNC_WINDOW_REPLY_BYTES 16U

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
 * The largest body migo_session_call_sync accepts, arguments included, for
 * every operation but MIGO_SYNC_OP_SERVICE (see MIGO_SERVICE_CALL_MAX_BYTES).
 *
 * A constant of the wire format (contracts/frame-wire/wire-v1.md, "A request as
 * one body"), published here so a transport can refuse a larger body before it
 * reads it rather than after.
 */
#define MIGO_SYNC_CALL_MAX_BYTES 4096U

/*
 * A service call made synchronously -- readFileSync, getStorageSync. Its body
 * and its reply are bounded by the service stream's own limits below rather
 * than by the barrier's: a synchronous read answers with the file, and a
 * synchronous write sends one. A transport reading call bodies bounds them by
 * MIGO_SERVICE_CALL_MAX_BYTES and lets the library refuse a non-service call
 * above MIGO_SYNC_CALL_MAX_BYTES.
 */
#define MIGO_SYNC_OP_SERVICE 8U
#define MIGO_SERVICE_CALL_MAX_BYTES 67108920U

/*
 * How many bytes of answer header migo_session_call_sync writes.
 *
 * The header is the wire format's (contracts/frame-wire/wire-v1.md, "An answer
 * as one body"); the transport sends it unread, followed by the reply.
 */
#define MIGO_SYNC_ANSWER_HEADER_BYTES 16U

/* The bytes a synchronous call was answered with. Owned by the library until
 * migo_sync_reply_release. */
typedef struct MigoSyncReply MigoSyncReply;

/*
 * Answer a synchronous call carried whole in one body.
 *
 * For a producer that cannot share memory with the host: on Apple the content
 * origin is a custom scheme, where WebKit provides no SharedArrayBuffer, so a
 * Worker blocks in a synchronous request. The transport hands its body here
 * unchanged, and sends back, as one response body, the header this writes
 * followed by the reply's bytes. Both layouts are the wire format's
 * (contracts/frame-wire/wire-v1.md, "A request as one body" and "An answer as
 * one body"); the transport parses neither.
 *
 * The call carries how long the producer will wait, not a deadline: its clock
 * is not the host's. The deadline is now_nanos plus that wait, so now_nanos is
 * the caller's monotonic clock, as for migo_session_post_sync_request.
 *
 * header must have room for MIGO_SYNC_ANSWER_HEADER_BYTES; less is refused
 * with MIGO_ERROR_INVALID_ARGUMENT before the call is posted, so nothing is
 * read back for an answer that could not be delivered.
 *
 * *out_reply receives the reply when the call was answered with bytes, and
 * NULL otherwise -- including for every failure, whose verdict is in the
 * header. A reply is the renderer's own buffer, handed over rather than copied:
 * read it with migo_sync_reply_bytes, and release it with
 * migo_sync_reply_release once the bytes have been sent. A readback can be a
 * full screen at device scale, and this is the path a producer is blocked on.
 *
 * Every protocol outcome is an answer and returns MIGO_OK, including a call
 * that failed or could not be decoded: the producer is blocked on the response
 * whatever happened, and the verdict belongs in it.
 *
 * The verdict is read under the lock that settles the request, the reply is
 * that request's own readback, and the slot is freed in the same step. There is
 * no take step, and no window in which another request's answer can be taken
 * for this one.
 *
 * Blocks until the call is settled: first for the frame it names, which the
 * library waits for rather than answering early, then for the operation. Call
 * it on a thread that may block, and never while holding a lock that frame
 * submission on this session needs -- the frame it waits for arrives through
 * migo_session_submit_external_frame.
 */
MIGO_API MigoResult MIGO_CALL migo_session_call_sync(
    MigoSession *session, const uint8_t *call, size_t call_bytes, uint64_t now_nanos,
    uint8_t *header, size_t header_capacity, MigoSyncReply **out_reply);

/*
 * Where a reply's bytes are, and how many. Valid until the reply is released.
 * *out_bytes and *out_length receive NULL and zero on failure.
 */
MIGO_API MigoResult MIGO_CALL migo_sync_reply_bytes(
    const MigoSyncReply *reply, const uint8_t **out_bytes, size_t *out_length);

/*
 * Free a reply. The handle and its bytes are invalid afterwards. NULL is
 * refused with MIGO_ERROR_INVALID_ARGUMENT rather than ignored: releasing a
 * reply that was never received is a caller that lost track of which answers
 * carried bytes.
 */
MIGO_API MigoResult MIGO_CALL migo_sync_reply_release(MigoSyncReply *reply);

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
 * The socket's other message
 *
 * The producer's socket carries frame packets and one more thing: control
 * messages, which today are requests for the next frame. Migo's
 * requestAnimationFrame is fed by host vsync on every platform, and that demand
 * has to cross the process boundary for a frame-clock tick to exist.
 *
 * A transport routes each message by asking the library which it is, rather
 * than by comparing magic numbers of its own: a host that encoded the rule
 * itself would be one more implementation of the wire format to drift. The
 * contract is contracts/frame-wire/wire-v1.md, "Uplink control messages".
 * ------------------------------------------------------------------------- */

typedef uint32_t MigoUplinkMessageKind;
/*
 * Offer it to migo_session_submit_external_frame. Everything that is not a
 * control message is this -- including bytes that are not a frame either,
 * which frame ingress refuses with a verdict the producer sees. A third
 * "unknown" answer would need a third place to report it from.
 */
#define MIGO_UPLINK_MESSAGE_FRAME   1U
/* Offer it to migo_session_submit_uplink_control. */
#define MIGO_UPLINK_MESSAGE_CONTROL 2U
/* Offer it to migo_session_submit_service. */
#define MIGO_UPLINK_MESSAGE_SERVICE 3U

/*
 * Which door a message that arrived on the producer's socket goes through.
 *
 * Needs no session and reads at most the first four bytes. `bytes` may be NULL
 * only when `byte_count` is 0. Messages that arrive on the content origin
 * instead of the socket are always frames and need not be asked about.
 */
MIGO_API MigoResult MIGO_CALL migo_uplink_message_kind(
    const uint8_t *bytes, size_t byte_count, MigoUplinkMessageKind *out_kind);

/*
 * Read one control message and act on it.
 *
 * `bytes` is borrowed for the call. The whole message is validated before any
 * of it is acted on. *out_refusal_code receives 0 when the message was read --
 * including when its requests belonged to another runtime generation, which
 * are ignored rather than refused -- or a code from 3001 up naming the rule it
 * broke (see "Control refusals" in the wire contract). Report a refusal the
 * way a refused frame is reported.
 *
 * A request for a frame made before the renderer is up is held and armed when
 * it starts, not refused: the producer's first request races the host's
 * bring-up, and nothing would tell it to ask again.
 *
 * Returns MIGO_OK when *out_refusal_code was written. A non-OK result means the
 * call could not be made: a bad handle, a NULL buffer or output, or no surface
 * attached yet.
 */
MIGO_API MigoResult MIGO_CALL migo_session_submit_uplink_control(
    MigoSession *session, const uint8_t *bytes, size_t byte_count,
    uint32_t *out_refusal_code);

/* ---------------------------------------------------------------------------
 * The return path
 * -------------------------------------------------------------------------*/

/*
 * Called when the library has queued a downlink record the transport did not
 * cause -- a frame-clock tick. A verdict is queued inside a submit the
 * transport made and drains right after, so it needs no wake-up; a tick does,
 * or it waits until the producer sends something, and a producer waiting for a
 * tick sends nothing.
 *
 * Called on the session's own thread. SCHEDULE THE DRAIN, DO NOT PERFORM IT:
 * return promptly, and do not call back into the library from inside the
 * waker -- in particular not migo_session_set_downlink_waker, which waits for
 * the call in progress to return.
 */
typedef void(MIGO_CALL *MigoDownlinkWakerFn)(void *user_data);

/*
 * Install the downlink waker, or clear it with a NULL waker.
 *
 * One waker per session; installing replaces the previous one. Clearing
 * returns only after any call already in progress has returned, so once it
 * returns `user_data` may be freed. Clear it before migo_session_destroy.
 *
 * Needs an attached surface, like every other entry point on this path, and
 * returns MIGO_ERROR_INVALID_STATE without one.
 */
MIGO_API MigoResult MIGO_CALL migo_session_set_downlink_waker(
    MigoSession *session, MigoDownlinkWakerFn waker, void *user_data);

/*
 * Take the next message the host owes the producer.
 *
 * Writes at most `capacity` bytes into `buffer` and reports the length in
 * `*out_written`. Zero means there is nothing to send. That is the normal
 * answer between frames -- not an error, and not something to spin on.
 *
 * WHAT IS IN IT, AND WHY THE HOST DOES NOT BUILD IT. The bytes are a downlink
 * envelope carrying two kinds of record: the verdict on each frame the producer
 * submitted (its decision, the credits left, the accepted sequence) and the
 * frame-clock ticks that drive the producer's requestAnimationFrame. A host
 * that assembled those itself would be a third implementation of a wire format
 * that already has two, which is the drift this project keeps a gate for. A
 * transport that only copies bytes cannot drift.
 *
 * WHOLE RECORDS ONLY. A capacity too small for the envelope plus one record
 * writes nothing and leaves everything queued, so a caller that comes back with
 * a larger buffer loses nothing. 4096 bytes holds any message this queue
 * produces.
 *
 * WHEN TO CALL IT. After every submit and after every frame the host delivers,
 * and send whatever comes out. The queue is bounded and coalesces ticks, so a
 * transport that falls behind costs the producer scheduling decisions rather
 * than memory -- every record is absolute, so the next one it reads is already
 * correct.
 */
MIGO_API MigoResult MIGO_CALL migo_session_take_downlink(
    MigoSession *session, uint8_t *buffer, size_t capacity,
    size_t *out_written);

/* ---------------------------------------------------------------------------
 * The service stream
 *
 * Everything content asks the host to do that is not drawing: read a file,
 * write a save, load an image, play a sound, open a socket. Its messages are
 * sequenced, admitted strictly in order and answered on a return stream of
 * their own. The contract is contracts/frame-wire/wire-v1.md, "The service
 * stream"; a transport parses none of it.
 *
 * TRANSPORT. A message of at most 65536 bytes arrives on the producer's socket
 * (MIGO_UPLINK_MESSAGE_SERVICE); a larger one as a POST to the content origin's
 * service endpoint. Answers are drained with migo_session_take_service_message
 * and sent on the socket; an answer too large to send inline is parked, and the
 * producer fetches it from the content origin, which takes it with
 * migo_session_take_parked_reply.
 * ------------------------------------------------------------------------- */

/* The largest service message, in bytes. A transport reading a POSTed message
 * refuses a larger one before reading it. */
#define MIGO_SERVICE_MESSAGE_MAX_BYTES 67108864U

/* Bytes the library owns until migo_owned_bytes_release. Handed over rather
 * than copied: an answer can be a whole file. */
typedef struct MigoOwnedBytes MigoOwnedBytes;

/*
 * Where the bytes are, and how many. Valid until they are released.
 * *out_bytes and *out_length receive NULL and zero on failure.
 */
MIGO_API MigoResult MIGO_CALL migo_owned_bytes_view(
    const MigoOwnedBytes *owned, const uint8_t **out_bytes, size_t *out_length);

/* Free the bytes. The handle is invalid afterwards. NULL is refused. */
MIGO_API MigoResult MIGO_CALL migo_owned_bytes_release(MigoOwnedBytes *owned);

/*
 * Admit one service message, from the socket or from a POST to the content
 * origin alike.
 *
 * `bytes` is borrowed for the call. The two paths reorder, so a message that
 * arrives ahead of the one before it is held -- up to 256 messages and
 * MIGO_SERVICE_MESSAGE_MAX_BYTES -- and admitted when that one arrives; either
 * way the call returns at once and a POST can be answered straight away.
 *
 * *out_refusal_code receives 0 when the message was admitted or held, or
 * ignored as belonging to another runtime generation; otherwise a code from
 * 4001 up (see "Service refusals" in the wire contract). A refusal is also
 * reported to the producer on the return stream: the stream is broken from
 * there.
 *
 * Blocks while the session's queue of admitted work is full, which pushes back
 * on the socket; call it on a thread that may block. Returns
 * MIGO_ERROR_INVALID_STATE when the session has no attached surface yet or has
 * ended.
 */
MIGO_API MigoResult MIGO_CALL migo_session_submit_service(
    MigoSession *session, const uint8_t *bytes, size_t byte_count,
    uint32_t *out_refusal_code);

/*
 * Take the next message of answers and events the host owes the producer.
 *
 * *out_message receives NULL when nothing is queued -- the normal answer, not
 * an error. Otherwise send the bytes on the socket unread and release them.
 * Call it wherever migo_session_take_downlink is called: the downlink waker
 * fires for both.
 */
MIGO_API MigoResult MIGO_CALL migo_session_take_service_message(
    MigoSession *session, MigoOwnedBytes **out_message);

/*
 * Take a parked answer: the producer asked for it by generation and request
 * id. Taken once -- the library releases its copy as it hands this one over.
 * *out_reply receives NULL when there is no such answer, which is a producer
 * asking twice or asking for another generation's; answer that request with a
 * 404.
 */
MIGO_API MigoResult MIGO_CALL migo_session_take_parked_reply(
    MigoSession *session, uint32_t generation, uint32_t request_id, MigoOwnedBytes **out_reply);

/*
 * Where the loaded content's code is: the directory the host serves at the
 * content origin's root. Written as NUL-terminated UTF-8.
 *
 * On this execution migo_session_load_content mounts the content before it
 * returns -- the entry module is evaluated by WebKit in another process, and
 * that process's host needs this directory to serve it -- so the root is known
 * as soon as that call succeeds.
 *
 * *out_length receives the length without the NUL. Returns
 * MIGO_ERROR_INVALID_ARGUMENT when capacity is too small, with *out_length set
 * so the caller can retry; MIGO_ERROR_INVALID_STATE before content is loaded.
 */
MIGO_API MigoResult MIGO_CALL migo_session_copy_content_root(
    MigoSession *session, char *buffer, size_t capacity, size_t *out_length);

/* What migo_session_read_content_module handed over. */
#define MIGO_CONTENT_MODULE_SERVED 0U     /* the bytes are the module's source */
#define MIGO_CONTENT_MODULE_NOT_FOUND 1U  /* the bytes are why: answer 404 */
#define MIGO_CONTENT_MODULE_REFUSED 2U    /* the bytes are why: answer 403 */
#define MIGO_CONTENT_MODULE_UNREADABLE 3U /* the bytes are why: answer 500 */

/*
 * The source of the content module at `path` -- a path on the content origin,
 * "/game.js" -- as the engine evaluates it: resolved through the mounted
 * package (subpackage overlays and pack-backed packages included), contained
 * in it, UTF-8, and with the module loader's one rewrite applied, so a
 * CommonJS entry runs wrapped exactly as it does in the embedded runtime. The
 * content origin answers every script request of a game with this rather than
 * with the file.
 *
 * `path` is borrowed UTF-8 of `path_length` bytes. *out_module receives the
 * bytes -- the source, or the reason it was not served, as *out_status says --
 * which the caller releases with migo_owned_bytes_release. Reads the file on
 * the calling thread. Returns MIGO_ERROR_INVALID_STATE before content is
 * loaded.
 */
MIGO_API MigoResult MIGO_CALL migo_session_read_content_module(
    MigoSession *session, const char *path, size_t path_length, MigoOwnedBytes **out_module,
    uint32_t *out_status);

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
 * boundary instead of a thread boundary. Requests coalesce, and one made before
 * the renderer is up is held and armed when it starts. The producer's own
 * requests arrive as control messages; this is for a host that asks on its
 * behalf. MIGO_ERROR_INVALID_STATE means no surface is attached.
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

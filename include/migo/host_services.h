/*
 * Migo C ABI: the host-service channel.
 *
 * Some capabilities are never the engine's to implement. Showing an advert,
 * taking a payment, signing a player in, sharing, opening another mini-program:
 * each is the host app's own integration with a vendor SDK, a store or its own
 * backend. The channel lets content reach them through the host:
 *
 *   1. The host declares which services it supplies -- one bit per
 *      MIGO_HOST_SERVICE_* in MigoHostCallbacks.host_services -- and installs
 *      MigoHostCallbacks.on_host_service_call. A service it does not declare
 *      is never called, and content's API for it fails the way it does on a
 *      platform without it.
 *   2. Content's request arrives through the dispatcher as a
 *      MigoHostServiceCall: a service, a method within it and the request's
 *      options as a JSON object.
 *   3. A *call* (call_id != 0) is answered exactly once, from any thread, with
 *      migo_session_complete_host_service_call. A *command* (call_id == 0) is
 *      carried out and answered by nothing.
 *   4. A service with events of its own -- an advert loading, closing,
 *      resizing -- reports each with migo_session_post_host_service_event.
 *
 * Payloads are JSON because these option sets are open and vendor-shaped, and
 * are JSON on every other path into the engine; the small closed option sets
 * elsewhere in this ABI (the keyboard, vibration) stay typed. Every number here
 * is defined once, in contracts/runtime/host-services.json, append-only: the
 * request and result fields each method carries are documented there and in
 * the developer documentation.
 */
#ifndef MIGO_HOST_SERVICES_H_
#define MIGO_HOST_SERVICES_H_

#include <stddef.h> /* offsetof */
#include <migo/types.h>

MIGO_BEGIN_DECLS

/* Services. A service's number is also its bit in host_services. */
typedef uint32_t MigoHostService;
#define MIGO_HOST_SERVICE_AD 0U
#define MIGO_HOST_SERVICE_PAYMENT 1U
#define MIGO_HOST_SERVICE_AUTH 2U
#define MIGO_HOST_SERVICE_SHARE 3U
#define MIGO_HOST_SERVICE_NAVIGATE 4U

/*
 * Ads. All six are commands addressed to the advert by the adId in their
 * payload; what happens to an advert is reported as MIGO_AD_EVENT_LIFECYCLE.
 * The host is authoritative for an incentivised video's reward: content grants
 * one only when the host's close event says the video was watched to the end
 * (isEnded), and the engine never reports that on the host's behalf.
 */
#define MIGO_AD_CREATE 0U
#define MIGO_AD_LOAD 1U
#define MIGO_AD_SHOW 2U
#define MIGO_AD_HIDE 3U
#define MIGO_AD_UPDATE_STYLE 4U
#define MIGO_AD_DESTROY 5U
/* {"adId": <the advert's id>, "event": "load"|"error"|"show"|"hide"|"close"|
 * "resize", ...the event's own fields, e.g. "isEnded" on close}. */
#define MIGO_AD_EVENT_LIFECYCLE 0U

/* Payment: two calls. */
#define MIGO_PAYMENT_REQUEST_MIDAS_PAYMENT 0U
#define MIGO_PAYMENT_REQUEST_MIDAS_PAYMENT_GAME_ITEM 1U

/* Sign-in: four calls. login and getPhoneNumber succeed with {"code": "..."}. */
#define MIGO_AUTH_LOGIN 0U
#define MIGO_AUTH_CHECK_SESSION 1U
#define MIGO_AUTH_GET_USER_INFO 2U
#define MIGO_AUTH_GET_PHONE_NUMBER 3U

/* Sharing: one call. */
#define MIGO_SHARE_SHARE_APP_MESSAGE 0U

/* Navigation: one call and two commands. */
#define MIGO_NAVIGATE_NAVIGATE_TO_MINI_PROGRAM 0U
#define MIGO_NAVIGATE_NAVIGATE_BACK_MINI_PROGRAM 1U
#define MIGO_NAVIGATE_OPEN_CUSTOMER_SERVICE_CONVERSATION 2U

/*
 * The largest payload or message either direction carries, in bytes. A longer
 * one is refused with MIGO_ERROR_INVALID_ARGUMENT before any byte is read.
 */
#define MIGO_HOST_SERVICE_PAYLOAD_MAX_BYTES (1U << 20)

/*
 * A request content made of the host. Written by the library; every pointer is
 * borrowed for the duration of the callback only. call_id is opaque: the host
 * hands it back unchanged and never interprets it.
 */
typedef struct MigoHostServiceCall {
    uint32_t struct_size;
    uint32_t abi_version;
    uint64_t call_id;
    MigoHostService service;
    uint32_t method;
    const char *payload_json_utf8;
    uint32_t payload_length;
    uint32_t reserved0;
} MigoHostServiceCall;

typedef void(MIGO_CALL *MigoOnHostServiceCallFn)(void *user_data, MigoSession *session,
                                                 const MigoHostServiceCall *call);

typedef uint32_t MigoHostServiceStatus;
#define MIGO_HOST_SERVICE_STATUS_OK 0U
#define MIGO_HOST_SERVICE_STATUS_FAIL 1U

typedef uint32_t MigoHostServiceResultFlags;
#define MIGO_HOST_SERVICE_RESULT_FLAG_NONE 0U
/* error_code carries a value; without the flag a failure has no numeric code. */
#define MIGO_HOST_SERVICE_RESULT_FLAG_ERROR_CODE (1U << 0)

/*
 * A call's outcome. On MIGO_HOST_SERVICE_STATUS_OK, payload is the result as a
 * JSON object (or empty for a result with no fields) and the message is empty
 * with no flags. On MIGO_HOST_SERVICE_STATUS_FAIL, message is the reason --
 * required, and without the API's name: content sees "<api>:fail <message>",
 * composed by content because one method can serve several APIs -- error_code
 * is the API's numeric code when flagged, and payload is empty. Strings are
 * length-delimited UTF-8, read during the call and not retained.
 */
typedef struct MigoHostServiceResult {
    uint32_t struct_size;
    uint32_t abi_version;
    MigoHostServiceStatus status;
    MigoHostServiceResultFlags flags;
    int32_t error_code;
    uint32_t message_length;
    const char *message_utf8;
    const char *payload_json_utf8;
    uint32_t payload_length;
    uint32_t reserved0;
} MigoHostServiceResult;

MIGO_STATIC_ASSERT(offsetof(MigoHostServiceCall, struct_size) == 0,
                   "every versioned struct must begin with struct_size");
MIGO_STATIC_ASSERT(offsetof(MigoHostServiceCall, call_id) == 8,
                   "MigoHostServiceCall.call_id is naturally aligned");
MIGO_STATIC_ASSERT(offsetof(MigoHostServiceResult, struct_size) == 0,
                   "every versioned struct must begin with struct_size");
#if MIGO_LP64
MIGO_STATIC_ASSERT(sizeof(MigoHostServiceCall) == 40, "MigoHostServiceCall LP64 size changed");
MIGO_STATIC_ASSERT(sizeof(MigoHostServiceResult) == 48,
                   "MigoHostServiceResult LP64 size changed");
#endif

/*
 * Complete one call, exactly once, from any thread. Returns MIGO_OK when the
 * outcome was accepted -- including for a call whose content has gone, since
 * no one is left to tell and that is not the host's error -- and
 * MIGO_ERROR_INVALID_ARGUMENT for a NULL argument, a call_id that does not name
 * a call of a service this host declared, a malformed result (a failure without
 * a reason among them), or a success payload that is not a JSON object. A result must not name a field the
 * library writes itself ("requestId", "error", "errMsg").
 */
MIGO_API MigoResult MIGO_CALL migo_session_complete_host_service_call(
    MigoSession *session, uint64_t call_id, const MigoHostServiceResult *result);

/*
 * Report one of a declared service's own events, from any thread. payload is a
 * JSON object, length-delimited UTF-8, read during the call. Returns MIGO_OK
 * when the event was accepted or there is no running content to deliver it to,
 * and MIGO_ERROR_INVALID_ARGUMENT for a service the host did not declare, an
 * event that service does not define, or a payload that is not a JSON object.
 */
MIGO_API MigoResult MIGO_CALL migo_session_post_host_service_event(
    MigoSession *session, MigoHostService service, uint32_t event,
    const char *payload_json_utf8, uint32_t payload_length);

MIGO_END_DECLS

#endif /* MIGO_HOST_SERVICES_H_ */

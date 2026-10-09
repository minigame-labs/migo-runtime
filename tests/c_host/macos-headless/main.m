/*
 * The shipping macOS archive runs JavaScript, from a host, through the public C
 * ABI only.
 *
 * Everything else that checks an assembled Migo archive checks that it links.
 * `nm` says the engine's symbols are present; `xcodebuild` says a consumer
 * resolves against it; the V8 lane's own step runs `cargo test -p
 * migo-runtime-v8`, which proves the *V8* archive evaluates script and says
 * nothing about ours. A build can pass all three and still ship an engine that
 * cannot run a game -- the failure would first be seen by whoever integrated
 * it.
 *
 * So this is a host: it creates an engine, a session and a surface, loads
 * content, and waits for the content to say something only running code can
 * say. It includes nothing but the public migo headers, which is the same rule
 * tests/c_host/linux/main.c holds itself to -- if this file ever needs a
 * private engine detail, the ABI is incomplete.
 *
 * Headless on purpose. A CAMetalLayer is a rendering surface whether or not it
 * is in a window, and a window here would add an NSApplication, a run loop and
 * a reason for CI to need a session to log into. What it may not do is skip the
 * surface: `migo_session_load_content` returns MIGO_ERROR_INVALID_STATE without
 * one, because content is evaluated on the thread the render target owns.
 *
 * Build and run: scripts/test-macos-archive-runs-js.sh
 */

#import <QuartzCore/QuartzCore.h>

#include <migo/migo.h>
#include <migo/platform/macos.h>

#include <errno.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#define SURFACE_WIDTH 256
#define SURFACE_HEIGHT 256
#define SCALE_FACTOR 1.0f

/* How long the content gets. Generous rather than tuned: this measures nothing,
 * and a tight budget on a loaded CI runner turns into a flake that reads as a
 * broken archive. */
#define DEADLINE_SECONDS 60

/*
 * What the content said, and the lock that publishes it.
 *
 * The callbacks arrive on whichever thread the dispatcher chose -- here the
 * engine's own -- while main waits. A mutex and a condition variable rather
 * than a spin on an atomic: main should be asleep while the engine works, not
 * competing with it for a core on a two-core runner.
 */
typedef enum {
    OUTCOME_PENDING = 0,
    OUTCOME_EXIT_REQUESTED,
    OUTCOME_ERROR,
} Outcome;

static pthread_mutex_t g_lock = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t g_signal = PTHREAD_COND_INITIALIZER;
static Outcome g_outcome = OUTCOME_PENDING;
static char g_error_message[1024];
static atomic_int g_ready;
static atomic_int g_frames_requested;

static void publish(Outcome outcome) {
    pthread_mutex_lock(&g_lock);
    /* First answer wins. An error after the exit request is the teardown path
     * and must not overwrite the success the content already reported. */
    if (g_outcome == OUTCOME_PENDING) {
        g_outcome = outcome;
    }
    pthread_cond_signal(&g_signal);
    pthread_mutex_unlock(&g_lock);
}

static MigoResult MIGO_CALL dispatch_inline(void *dispatcher_context, MigoTaskFn task,
                                            void *task_context) {
    (void)dispatcher_context;
    task(task_context);
    return MIGO_OK;
}

static void MIGO_CALL on_ready(void *user_data, MigoSession *session) {
    (void)user_data;
    (void)session;
    atomic_store(&g_ready, 1);
    printf("[macos-headless] content is ready\n");
    fflush(stdout);
}

static void MIGO_CALL on_error(void *user_data, MigoSession *session,
                               const MigoError *error) {
    (void)user_data;
    (void)session;
    /* Length-delimited and borrowed for this call only, so it is copied before
     * the callback returns rather than kept as a pointer. */
    size_t length = (size_t)error->message_length;
    if (length >= sizeof(g_error_message)) {
        length = sizeof(g_error_message) - 1;
    }
    pthread_mutex_lock(&g_lock);
    memcpy(g_error_message, error->message_utf8, length);
    g_error_message[length] = '\0';
    pthread_mutex_unlock(&g_lock);
    fprintf(stderr, "[macos-headless] engine error %d: %s\n", (int)error->code,
            g_error_message);
    fflush(stderr);
    publish(OUTCOME_ERROR);
}

static void MIGO_CALL on_exit_requested(void *user_data, MigoSession *session) {
    (void)user_data;
    (void)session;
    printf("[macos-headless] the content asked to exit\n");
    fflush(stdout);
    publish(OUTCOME_EXIT_REQUESTED);
}

/*
 * "Schedule exactly one frame."
 *
 * Counted here and answered on the waiting thread, never from inside this
 * callback: answering inline would re-enter the engine from its own thread,
 * which the ABI does not ask for and a host should not invent. A real host
 * answers from a display link -- MigoDisplayLink is exactly that -- and this
 * one answers from its wait loop, which is the same shape with a cheaper clock.
 */
static void MIGO_CALL on_request_frame(void *user_data, MigoSession *session) {
    (void)user_data;
    (void)session;
    atomic_fetch_add(&g_frames_requested, 1);
}

/* A monotonic frame timestamp in nanoseconds, which is what the ABI asks for and
 * what AChoreographer hands an Android host. */
static int64_t now_nanos(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (int64_t)ts.tv_sec * 1000000000LL + (int64_t)ts.tv_nsec;
}

/*
 * The host-service channel, answered as a fixed fake backend.
 *
 * A real host forwards these to an ad SDK, a store, a sign-in backend. This one
 * answers every call the same way every time, so content can check the exact
 * result it receives -- scripts/fixtures/headless-host-services-probe does,
 * step by step. Content that does not use these services never calls them, so
 * declaring them costs the other fixtures nothing.
 *
 * Answered inline, from inside the callback: the header allows a callback to
 * re-enter the library, and a host whose dispatcher runs tasks on the calling
 * thread -- as this one's does -- is exactly the host that would.
 */
static const uint64_t HOST_SERVICES = (UINT64_C(1) << MIGO_HOST_SERVICE_AD)
                                      | (UINT64_C(1) << MIGO_HOST_SERVICE_PAYMENT)
                                      | (UINT64_C(1) << MIGO_HOST_SERVICE_AUTH)
                                      | (UINT64_C(1) << MIGO_HOST_SERVICE_SHARE)
                                      | (UINT64_C(1) << MIGO_HOST_SERVICE_NAVIGATE)
                                      | (UINT64_C(1) << MIGO_HOST_SERVICE_SUBPACKAGE)
                                      | (UINT64_C(1) << MIGO_HOST_SERVICE_PERMISSION)
                                      | (UINT64_C(1) << MIGO_HOST_SERVICE_SETTING)
                                      | (UINT64_C(1) << MIGO_HOST_SERVICE_INTERACTION)
                                      | (UINT64_C(1) << MIGO_HOST_SERVICE_CLIPBOARD)
                                      | (UINT64_C(1) << MIGO_HOST_SERVICE_SCAN_CODE)
                                      | (UINT64_C(1) << MIGO_HOST_SERVICE_LOCATION);

/* The clipboard this host keeps: what content last wrote. */
static char g_clipboard[256] = "";

static void probe_failure(const char *what) {
    pthread_mutex_lock(&g_lock);
    snprintf(g_error_message, sizeof(g_error_message), "host-service probe: %s", what);
    pthread_mutex_unlock(&g_lock);
    fprintf(stderr, "[macos-headless] host-service probe: %s\n", what);
    fflush(stderr);
    publish(OUTCOME_ERROR);
}

static MigoHostServiceResult host_result(MigoHostServiceStatus status) {
    MigoHostServiceResult result;
    memset(&result, 0, sizeof(result));
    result.struct_size = (uint32_t)sizeof(result);
    result.abi_version = MIGO_ABI_VERSION_CURRENT;
    result.status = status;
    return result;
}

static void complete_ok(MigoSession *session, uint64_t call_id, const char *payload_json) {
    MigoHostServiceResult result = host_result(MIGO_HOST_SERVICE_STATUS_OK);
    result.payload_json_utf8 = payload_json;
    result.payload_length = (uint32_t)strlen(payload_json);
    if (migo_session_complete_host_service_call(session, call_id, &result) != MIGO_OK) {
        probe_failure("migo_session_complete_host_service_call refused a success");
    }
}

static void complete_fail(MigoSession *session, uint64_t call_id, const char *message,
                          int has_code, int32_t code) {
    MigoHostServiceResult result = host_result(MIGO_HOST_SERVICE_STATUS_FAIL);
    result.message_utf8 = message;
    result.message_length = (uint32_t)strlen(message);
    if (has_code) {
        result.flags = MIGO_HOST_SERVICE_RESULT_FLAG_ERROR_CODE;
        result.error_code = code;
    }
    if (migo_session_complete_host_service_call(session, call_id, &result) != MIGO_OK) {
        probe_failure("migo_session_complete_host_service_call refused a failure");
    }
}

/* The advert an ad command addresses. Every ad payload carries adId; a parser
 * would be more than this probe needs to find one integer. */
static long ad_id_of(const char *payload) {
    const char *key = strstr(payload, "\"adId\":");
    return key ? strtol(key + strlen("\"adId\":"), NULL, 10) : -1;
}

static void post_ad_event(MigoSession *session, long ad_id, const char *rest) {
    char event[256];
    int length = snprintf(event, sizeof(event), "{\"adId\":%ld,%s}", ad_id, rest);
    if (migo_session_post_host_service_event(session, MIGO_HOST_SERVICE_AD,
                                             MIGO_AD_EVENT_LIFECYCLE, event,
                                             (uint32_t)length) != MIGO_OK) {
        probe_failure("migo_session_post_host_service_event refused an ad event");
    }
}

static void MIGO_CALL on_host_service_call(void *user_data, MigoSession *session,
                                           const MigoHostServiceCall *call) {
    (void)user_data;
    char payload[4096];
    size_t length = call->payload_length < sizeof(payload) - 1 ? call->payload_length
                                                               : sizeof(payload) - 1;
    memcpy(payload, call->payload_json_utf8, length);
    payload[length] = '\0';
    /* Content's own bookkeeping never reaches a host: it answers by call_id. */
    if (strstr(payload, "requestId") != NULL) {
        probe_failure("the host was handed content's requestId");
        return;
    }
    switch (call->service) {
        case MIGO_HOST_SERVICE_AUTH:
            switch (call->method) {
                case MIGO_AUTH_LOGIN:
                    complete_ok(session, call->call_id, "{\"code\":\"probe-login-code\"}");
                    return;
                case MIGO_AUTH_CHECK_SESSION:
                    complete_fail(session, call->call_id, "session expired", 1, 1);
                    return;
                case MIGO_AUTH_GET_USER_INFO:
                    complete_ok(session, call->call_id,
                                "{\"userInfo\":{\"nickName\":\"probe\",\"avatarUrl\":\"\"}}");
                    return;
                case MIGO_AUTH_GET_PHONE_NUMBER:
                    complete_fail(session, call->call_id, "no phone bound", 0, 0);
                    return;
            }
            break;
        case MIGO_HOST_SERVICE_PAYMENT:
            switch (call->method) {
                case MIGO_PAYMENT_REQUEST_MIDAS_PAYMENT:
                    complete_ok(session, call->call_id, "");
                    return;
                case MIGO_PAYMENT_REQUEST_MIDAS_PAYMENT_GAME_ITEM:
                    complete_fail(session, call->call_id, "cancel", 1, 1);
                    return;
            }
            break;
        case MIGO_HOST_SERVICE_SHARE:
            if (call->method == MIGO_SHARE_SHARE_APP_MESSAGE) {
                complete_ok(session, call->call_id, "{}");
                return;
            }
            break;
        case MIGO_HOST_SERVICE_NAVIGATE:
            switch (call->method) {
                case MIGO_NAVIGATE_NAVIGATE_TO_MINI_PROGRAM:
                    complete_ok(session, call->call_id, "");
                    return;
                case MIGO_NAVIGATE_NAVIGATE_BACK_MINI_PROGRAM:
                case MIGO_NAVIGATE_OPEN_CUSTOMER_SERVICE_CONVERSATION:
                    if (call->call_id != 0) probe_failure("a navigation command carried a call id");
                    return;
            }
            break;
        case MIGO_HOST_SERVICE_PERMISSION:
            if (call->method == MIGO_PERMISSION_REQUEST_SCOPE) {
                /* The player says yes to the camera and no to the microphone.
                 * The decision is recorded first, then the request settled --
                 * the order a real host keeps, so content that acts on success
                 * finds the scope already granted. */
                if (strstr(payload, "\"scope.camera\"") != NULL) {
                    migo_session_set_scope_state(session, MIGO_SCOPE_CAMERA,
                                                 MIGO_SCOPE_STATE_GRANTED);
                    complete_ok(session, call->call_id, "");
                } else {
                    migo_session_set_scope_state(session, MIGO_SCOPE_RECORD,
                                                 MIGO_SCOPE_STATE_DENIED);
                    complete_fail(session, call->call_id, "auth deny", 0, 0);
                }
                return;
            }
            break;
        case MIGO_HOST_SERVICE_SETTING:
            switch (call->method) {
                case MIGO_SETTING_OPEN_SETTING:
                case MIGO_SETTING_OPEN_SYSTEM_BLUETOOTH_SETTING:
                    complete_ok(session, call->call_id, "");
                    return;
                case MIGO_SETTING_OPEN_APP_AUTHORIZE_SETTING:
                    complete_fail(session, call->call_id, "no settings app", 1, -1);
                    return;
            }
            break;
        case MIGO_HOST_SERVICE_INTERACTION:
            switch (call->method) {
                case MIGO_INTERACTION_SHOW_TOAST:
                case MIGO_INTERACTION_HIDE_TOAST:
                case MIGO_INTERACTION_SHOW_LOADING:
                case MIGO_INTERACTION_HIDE_LOADING:
                    if (call->call_id != 0) probe_failure("a toast or loading command carried a call id");
                    return;
                case MIGO_INTERACTION_SHOW_MODAL:
                    /* An editable modal answers with what the player typed. */
                    complete_ok(session, call->call_id,
                                strstr(payload, "\"editable\":true") != NULL
                                    ? "{\"confirm\":true,\"cancel\":false,\"content\":\"typed\"}"
                                    : "{\"confirm\":true,\"cancel\":false}");
                    return;
                case MIGO_INTERACTION_SHOW_ACTION_SHEET:
                    complete_ok(session, call->call_id, "{\"tapIndex\":1}");
                    return;
            }
            break;
        case MIGO_HOST_SERVICE_CLIPBOARD:
            switch (call->method) {
                case MIGO_CLIPBOARD_SET_CLIPBOARD_DATA: {
                    /* {"data":"..."}: content's text, plain ASCII in this probe. */
                    const char *key = strstr(payload, "\"data\":\"");
                    const char *start = key ? key + strlen("\"data\":\"") : NULL;
                    const char *end = start ? strchr(start, '"') : NULL;
                    if (end == NULL || (size_t)(end - start) >= sizeof(g_clipboard)) {
                        complete_fail(session, call->call_id, "unreadable data", 0, 0);
                        return;
                    }
                    memcpy(g_clipboard, start, (size_t)(end - start));
                    g_clipboard[end - start] = '\0';
                    complete_ok(session, call->call_id, "");
                    return;
                }
                case MIGO_CLIPBOARD_GET_CLIPBOARD_DATA: {
                    char answer[320];
                    snprintf(answer, sizeof(answer), "{\"data\":\"%s\"}", g_clipboard);
                    complete_ok(session, call->call_id, answer);
                    return;
                }
            }
            break;
        case MIGO_HOST_SERVICE_SCAN_CODE:
            if (call->method == MIGO_SCAN_CODE_SCAN_CODE) {
                complete_ok(session, call->call_id,
                            "{\"result\":\"probe-qr\",\"scanType\":\"QR_CODE\",\"charSet\":\"utf-8\"}");
                return;
            }
            break;
        case MIGO_HOST_SERVICE_LOCATION:
            switch (call->method) {
                case MIGO_LOCATION_GET_LOCATION:
                case MIGO_LOCATION_GET_FUZZY_LOCATION:
                    complete_ok(session, call->call_id,
                                "{\"latitude\":31.2,\"longitude\":121.5,\"accuracy\":10,"
                                "\"speed\":0,\"altitude\":0,\"verticalAccuracy\":0,"
                                "\"horizontalAccuracy\":10}");
                    return;
            }
            break;
        case MIGO_HOST_SERVICE_SUBPACKAGE:
            if (call->method == MIGO_SUBPACKAGE_DOWNLOAD) {
                /* The subpackage the content's game.json declared, by name and
                 * root -- the engine read the manifest, not this host. */
                if (strstr(payload, "\"name\":\"stage1\"") == NULL
                    || strstr(payload, "\"root\":\"stage1\"") == NULL) {
                    probe_failure("the subpackage request does not name game.json's stage1");
                    return;
                }
                static const char progress[] =
                    "{\"progress\":50,\"totalBytesWritten\":512,\"totalBytesExpectedToWrite\":1024}";
                if (migo_session_update_host_service_call(session, call->call_id, progress,
                                                          (uint32_t)strlen(progress))
                    != MIGO_OK) {
                    probe_failure("migo_session_update_host_service_call refused progress");
                    return;
                }
                complete_fail(session, call->call_id, "offline", 0, 0);
                return;
            }
            break;
        case MIGO_HOST_SERVICE_AD: {
            if (call->call_id != 0) {
                probe_failure("an ad command carried a call id");
                return;
            }
            long ad_id = ad_id_of(payload);
            switch (call->method) {
                /* An advert loads as soon as it is created, as an ad SDK's does. */
                case MIGO_AD_CREATE:
                case MIGO_AD_LOAD:
                    post_ad_event(session, ad_id, "\"event\":\"load\"");
                    return;
                /* On screen, then watched to the end: the exposure settles
                 * content's show(), and the verdict is the one fact only this
                 * side can state. */
                case MIGO_AD_SHOW:
                    post_ad_event(session, ad_id, "\"event\":\"show\"");
                    post_ad_event(session, ad_id, "\"event\":\"close\",\"isEnded\":true");
                    return;
                case MIGO_AD_HIDE:
                case MIGO_AD_UPDATE_STYLE:
                case MIGO_AD_DESTROY:
                    return;
            }
            break;
        }
    }
    char what[160];
    snprintf(what, sizeof(what), "a call this host does not define: service %u method %u",
             (unsigned)call->service, (unsigned)call->method);
    probe_failure(what);
}

static int fail(const char *what, MigoResult result) {
    fprintf(stderr, "[macos-headless] %s failed: %d\n", what, (int)result);
    return 1;
}

int main(int argc, char **argv) {
    if (argc < 3) {
        fprintf(stderr, "usage: %s <files-dir> <content-id> [max-drawables]\n", argv[0]);
        return 2;
    }
    const char *files_dir = argv[1];
    const char *content_id = argv[2];
    // 0 means "leave the layer's own default alone", which is not the same as
    // asking for whatever that default happens to be: setting the property tells
    // Core Animation a pool size was chosen.
    long max_drawables = (argc > 3) ? strtol(argv[3], NULL, 10) : 0;
    if (max_drawables != 0 && (max_drawables < 2 || max_drawables > 3)) {
        fprintf(stderr, "[macos-headless] maximumDrawableCount accepts 2 or 3; got %ld\n",
                max_drawables);
        return 2;
    }

    char cache_dir[1024];
    char code_cache_dir[1024];
    snprintf(cache_dir, sizeof(cache_dir), "%s/../cache", files_dir);
    snprintf(code_cache_dir, sizeof(code_cache_dir), "%s/../code-cache", files_dir);

    /* Ask the library what it supports before building anything on it. The
     * MIGO_* macros describe the headers this file compiled against; only this
     * call describes the archive that got linked -- which is the whole subject
     * of this test. */
    MigoCapabilities caps;
    memset(&caps, 0, sizeof caps);
    caps.struct_size = (uint32_t)sizeof caps;
    caps.abi_version = MIGO_ABI_VERSION_CURRENT;
    MigoResult result = migo_query_capabilities(&caps);
    if (result != MIGO_OK) return fail("migo_query_capabilities", result);
    printf("[macos-headless] abi %u..%u, platform kinds 0x%llx\n", caps.abi_version_min,
           caps.abi_version_max, (unsigned long long)caps.platform_kinds);
    if ((caps.platform_kinds & (UINT64_C(1) << MIGO_PLATFORM_MACOS_CA_METAL_LAYER)) == 0) {
        fprintf(stderr,
                "[macos-headless] this archive reports no CAMetalLayer surface kind, so it "
                "could not run a game on macOS however well it links\n");
        return 1;
    }

    MigoEngineConfig engine_config;
    memset(&engine_config, 0, sizeof(engine_config));
    engine_config.struct_size = (uint32_t)sizeof(engine_config);
    engine_config.abi_version = MIGO_ABI_VERSION_CURRENT;
    /* The probe bundle carries no signing receipt. */
    engine_config.flags = MIGO_ENGINE_FLAG_ALLOW_UNSIGNED_CONTENT;
    engine_config.files_dir_utf8 = files_dir;
    engine_config.cache_dir_utf8 = cache_dir;
    engine_config.code_cache_dir_utf8 = code_cache_dir;

    MigoEngine *engine = NULL;
    result = migo_engine_create(&engine_config, &engine);
    if (result != MIGO_OK) return fail("migo_engine_create", result);

    MigoSessionConfig session_config;
    memset(&session_config, 0, sizeof(session_config));
    session_config.struct_size = (uint32_t)sizeof(session_config);
    session_config.abi_version = MIGO_ABI_VERSION_CURRENT;
    session_config.flags = MIGO_SESSION_FLAG_NONE;

    MigoSession *session = NULL;
    result = migo_session_create(engine, &session_config, &session);
    if (result != MIGO_OK) return fail("migo_session_create", result);

    MigoHostCallbacks host_callbacks;
    memset(&host_callbacks, 0, sizeof(host_callbacks));
    host_callbacks.struct_size = (uint32_t)sizeof(host_callbacks);
    host_callbacks.abi_version = MIGO_ABI_VERSION_CURRENT;
    host_callbacks.dispatch = dispatch_inline;
    host_callbacks.on_ready = on_ready;
    host_callbacks.on_error = on_error;
    host_callbacks.on_exit_requested = on_exit_requested;
    host_callbacks.on_request_frame = on_request_frame;
    host_callbacks.on_host_service_call = on_host_service_call;
    host_callbacks.host_services = HOST_SERVICES;

    result = migo_session_set_host_callbacks(session, &host_callbacks);
    if (result != MIGO_OK) return fail("migo_session_set_host_callbacks", result);

    /* Standing answers content reads synchronously, reported before it runs:
     * the player profile is shared, Bluetooth and Wi-Fi are on, and the OS has
     * given this app the camera and refused it the photo library. */
    result = migo_session_set_scope_state(session, MIGO_SCOPE_USER_INFO, MIGO_SCOPE_STATE_GRANTED);
    if (result != MIGO_OK) return fail("migo_session_set_scope_state", result);
    result = migo_session_set_scope_state(session, MIGO_SCOPE_USER_LOCATION, MIGO_SCOPE_STATE_GRANTED);
    if (result != MIGO_OK) return fail("migo_session_set_scope_state", result);
    result = migo_session_set_system_settings(
        session, MIGO_SYSTEM_SETTING_FLAG_BLUETOOTH_ENABLED | MIGO_SYSTEM_SETTING_FLAG_WIFI_ENABLED);
    if (result != MIGO_OK) return fail("migo_session_set_system_settings", result);
    MigoAppAuthorizeSetting authorizations;
    memset(&authorizations, 0, sizeof(authorizations));
    authorizations.struct_size = (uint32_t)sizeof(authorizations);
    authorizations.abi_version = MIGO_ABI_VERSION_CURRENT;
    authorizations.camera = MIGO_AUTHORIZATION_AUTHORIZED;
    authorizations.album = MIGO_AUTHORIZATION_DENIED;
    result = migo_session_set_app_authorize_setting(session, &authorizations);
    if (result != MIGO_OK) return fail("migo_session_set_app_authorize_setting", result);

    /* The layer is owned by this host for the length of the run. Migo retains
     * it across attach and releases its own reference during retirement; the
     * strong reference here is what keeps it alive until then. */
    CAMetalLayer *layer = [CAMetalLayer layer];
    /* Bounds and contentsScale, not only drawableSize -- and this host set only
     * the latter until 2026-09-11, which is how it spent months rendering into a
     * 1x1 surface without anything noticing.
     *
     * ANGLE's Metal backend sizes its window surface from the layer's BOUNDS
     * times contentsScale, and assigns that back to drawableSize. A layer built
     * with `[CAMetalLayer layer]` and never placed in a view hierarchy has
     * CGRectZero bounds, so ANGLE computed 0x0, called setDrawableSize(0, 0) --
     * which CAMetalLayer logs as `ignoring invalid setDrawableSize
     * width=0.000000 height=0.000000` and refuses, leaving the 256x256 below
     * intact and irrelevant -- and then reported EGL_WIDTH/EGL_HEIGHT of 0,
     * which the engine clamps to 1x1.
     *
     * Every check that ran against this host therefore ran on one pixel: the
     * frames turned, the WebGL probe read back its clear colour, and the
     * drawable-pool A/B measured throughput on a 1x1 drawable. Nothing was
     * wrong with any of them except the surface they were asking about.
     *
     * A real host does not hit this, because a layer in a view hierarchy has
     * bounds. A headless one has to say so.
     *
     * Since 2026-09-27 the engine's window surface is fixed-size: the drawable
     * is sized from the attach's width_pixels x height_pixels (and then from
     * the onscreen canvas), not from bounds, and a host no longer sets
     * drawableSize at all. The bounds stay true for Core Animation, which
     * scales the drawable to them. */
    layer.contentsScale = SCALE_FACTOR;
    layer.bounds = CGRectMake(0, 0, SURFACE_WIDTH / SCALE_FACTOR, SURFACE_HEIGHT / SCALE_FACTOR);
    layer.framebufferOnly = NO;
    // The host's property, and the reason Migo takes a layer rather than a view:
    // handed a plain CALayer, ANGLE allocates its own metal layer and the host
    // loses this setting along with contentsScale and presentsWithTransaction.
    if (max_drawables != 0) {
        layer.maximumDrawableCount = (NSUInteger)max_drawables;
    }

    MigoMacosMetalLayerDescriptor macos;
    memset(&macos, 0, sizeof(macos));
    macos.struct_size = (uint32_t)sizeof(macos);
    macos.abi_version = MIGO_ABI_VERSION_CURRENT;
    macos.platform_kind = MIGO_PLATFORM_MACOS_CA_METAL_LAYER;
    macos.flags = MIGO_PLATFORM_DESCRIPTOR_FLAG_NONE;
    macos.ca_metal_layer = (__bridge void *)layer;

    MigoSurfaceDescriptor surface;
    memset(&surface, 0, sizeof(surface));
    surface.struct_size = (uint32_t)sizeof(surface);
    surface.abi_version = MIGO_ABI_VERSION_CURRENT;
    surface.generation = 1;
    surface.platform_kind = MIGO_PLATFORM_MACOS_CA_METAL_LAYER;
    surface.flags = MIGO_SURFACE_DESCRIPTOR_FLAG_NONE;
    surface.width_pixels = SURFACE_WIDTH;
    surface.height_pixels = SURFACE_HEIGHT;
    surface.scale_factor = SCALE_FACTOR;
    surface.color_space = MIGO_COLOR_SPACE_SRGB;
    surface.alpha_mode = MIGO_ALPHA_MODE_OPAQUE;
    surface.preferred_presentation_mode = MIGO_PRESENTATION_MODE_DEFAULT;
    surface.capability_flags = MIGO_SURFACE_CAPABILITY_NONE;
    surface.platform_descriptor_size = (uint32_t)sizeof(macos);
    surface.platform_descriptor = &macos;

    MigoSurfaceAttachment *attachment = NULL;
    result = migo_session_attach_surface(session, &surface, &attachment);
    if (result != MIGO_OK) return fail("migo_session_attach_surface", result);

    MigoContentDescriptor content;
    memset(&content, 0, sizeof(content));
    content.struct_size = (uint32_t)sizeof(content);
    content.abi_version = MIGO_ABI_VERSION_CURRENT;
    content.flags = MIGO_CONTENT_FLAG_NONE;
    content.content_id_utf8 = content_id;
    content.entry_utf8 = "game.js";

    result = migo_session_load_content(session, &content);
    if (result != MIGO_OK) return fail("migo_session_load_content", result);

    int64_t run_started = now_nanos();
    struct timespec deadline;
    clock_gettime(CLOCK_REALTIME, &deadline);
    deadline.tv_sec += DEADLINE_SECONDS;

    /*
     * The frame loop. Every request the engine makes is answered exactly once,
     * from this thread, with a real timestamp -- which is what turns this from
     * "the archive evaluated a module" into "the archive turned frames": the
     * content's requestAnimationFrame callbacks only run when a vsync it asked
     * for comes back.
     *
     * The wait is short rather than a condvar sleep because there are two things
     * to wake for and only one of them signals. 4 ms is well under a display
     * period, so it never becomes the thing limiting the cadence.
     */
    int frames_answered = 0;
    pthread_mutex_lock(&g_lock);
    while (g_outcome == OUTCOME_PENDING) {
        struct timespec slice;
        clock_gettime(CLOCK_REALTIME, &slice);
        slice.tv_nsec += 4 * 1000 * 1000;
        if (slice.tv_nsec >= 1000000000L) {
            slice.tv_nsec -= 1000000000L;
            slice.tv_sec += 1;
        }
        pthread_cond_timedwait(&g_signal, &g_lock, &slice);
        if (g_outcome != OUTCOME_PENDING) {
            break;
        }
        pthread_mutex_unlock(&g_lock);

        /* Outside the lock: notify_vsync reaches the engine, and the engine is
         * entitled to call back into this host inline. */
        int requested = atomic_load(&g_frames_requested);
        while (frames_answered < requested) {
            MigoResult vsync = migo_session_notify_vsync(session, now_nanos());
            if (vsync != MIGO_OK) {
                fprintf(stderr, "[macos-headless] notify_vsync returned %d after %d frames\n",
                        (int)vsync, frames_answered);
                break;
            }
            frames_answered += 1;
        }

        struct timespec now;
        clock_gettime(CLOCK_REALTIME, &now);
        if (now.tv_sec > deadline.tv_sec
            || (now.tv_sec == deadline.tv_sec && now.tv_nsec >= deadline.tv_nsec)) {
            pthread_mutex_lock(&g_lock);
            break;
        }
        pthread_mutex_lock(&g_lock);
    }
    Outcome outcome = g_outcome;
    char message[sizeof(g_error_message)];
    memcpy(message, g_error_message, sizeof(message));
    pthread_mutex_unlock(&g_lock);

    int status;
    switch (outcome) {
        case OUTCOME_EXIT_REQUESTED: {
            double elapsed = (double)(now_nanos() - run_started) / 1e9;
            printf("[macos-headless] the shipping archive evaluated JavaScript AND turned "
                   "frames: the content summed to 500500, ran its requestAnimationFrame loop "
                   "across %d answered vsyncs, and reached migo.exitMiniProgram()\n",
                   frames_answered);
            // Throughput, not a frame rate against a display: there is no display
            // here. It is reported so a drawable-pool A/B has a number, and
            // labelled so nobody reads it as one.
            printf("[macos-headless] throughput: %d frames in %.3f s = %.1f frames/s "
                   "(max_drawables=%ld, headless, no display to pace against)\n",
                   frames_answered, elapsed,
                   elapsed > 0 ? (double)frames_answered / elapsed : 0.0, max_drawables);
            status = 0;
            break;
        }
        case OUTCOME_ERROR:
            fprintf(stderr,
                    "[macos-headless] the content raised an error rather than finishing: %s\n",
                    message);
            status = 1;
            break;
        default:
            fprintf(stderr,
                    "[macos-headless] nothing was heard from the content in %d seconds. ready=%d "
                    "frames_requested=%d frames_answered=%d -- ready=1 with frames requested and "
                    "answered says the script was evaluated and the render loop did not reach its "
                    "last frame; ready=1 with no frames requested says the surface never came up; "
                    "ready=0 says the script was never evaluated\n",
                    DEADLINE_SECONDS, atomic_load(&g_ready),
                    atomic_load(&g_frames_requested), frames_answered);
            status = 1;
            break;
    }

    /*
     * Teardown, in the order the ABI requires: the attachment is detached, the
     * release observer is polled until the driver says RELEASED, and only then
     * may the Session be destroyed -- destruction refuses while a retirement is
     * PENDING. The CAMetalLayer stays alive across all of it, because
     * MIGO_OK from begin_detach means retirement started, not that the driver
     * finished with the layer.
     *
     * A retirement that never completes is reported and does not change the
     * exit code. This test answers one question -- does the shipping archive
     * evaluate JavaScript -- and failing it for a surface-lifetime defect would
     * make it a flaky proxy for a question
     * platforms/apple/Tests/MigoAppleRendererTests owns. Silence is not an
     * option either: a stuck retirement is named here so it cannot be mistaken
     * for a clean run.
     */
    MigoSurfaceRelease *release = NULL;
    MigoResult detached = migo_surface_begin_detach(attachment, &release);
    if (detached != MIGO_OK) {
        fprintf(stderr, "[macos-headless] teardown: begin_detach returned %d\n",
                (int)detached);
    } else {
        int released = 0;
        for (int attempt = 0; attempt < 200 && !released; attempt += 1) {
            MigoSurfaceReleaseStatus release_status;
            memset(&release_status, 0, sizeof(release_status));
            release_status.struct_size = (uint32_t)sizeof(release_status);
            release_status.abi_version = MIGO_ABI_VERSION_CURRENT;
            if (migo_surface_release_query(release, &release_status) == MIGO_OK
                && release_status.state == MIGO_SURFACE_RELEASE_RELEASED) {
                released = 1;
                break;
            }
            struct timespec pause = {0, 25 * 1000 * 1000};
            nanosleep(&pause, NULL);
        }
        if (!released) {
            fprintf(stderr,
                    "[macos-headless] teardown: the retired surface was still PENDING after 5s, "
                    "so the session cannot be destroyed. The JavaScript verdict above stands; "
                    "this line is about surface retirement, which this test does not own\n");
        }
        migo_surface_release_destroy(release);
    }

    migo_session_destroy(session);
    migo_engine_destroy(engine);
    (void)layer;
    return status;
}

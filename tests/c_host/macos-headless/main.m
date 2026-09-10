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
 * "Schedule exactly one frame." Nothing here presents, so the request is
 * counted and answered on the waiting thread rather than serviced: a frame
 * driven from inside this callback would be re-entering the engine from its own
 * thread, which the ABI does not ask for and a host should not invent.
 */
static void MIGO_CALL on_request_frame(void *user_data, MigoSession *session) {
    (void)user_data;
    (void)session;
    atomic_fetch_add(&g_frames_requested, 1);
}

static int fail(const char *what, MigoResult result) {
    fprintf(stderr, "[macos-headless] %s failed: %d\n", what, (int)result);
    return 1;
}

int main(int argc, char **argv) {
    if (argc < 3) {
        fprintf(stderr, "usage: %s <files-dir> <content-id>\n", argv[0]);
        return 2;
    }
    const char *files_dir = argv[1];
    const char *content_id = argv[2];

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

    result = migo_session_set_host_callbacks(session, &host_callbacks);
    if (result != MIGO_OK) return fail("migo_session_set_host_callbacks", result);

    /* The layer is owned by this host for the length of the run. Migo retains
     * it across attach and releases its own reference during retirement; the
     * strong reference here is what keeps it alive until then. */
    CAMetalLayer *layer = [CAMetalLayer layer];
    layer.drawableSize = CGSizeMake(SURFACE_WIDTH, SURFACE_HEIGHT);
    layer.framebufferOnly = NO;

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

    struct timespec deadline;
    clock_gettime(CLOCK_REALTIME, &deadline);
    deadline.tv_sec += DEADLINE_SECONDS;

    pthread_mutex_lock(&g_lock);
    while (g_outcome == OUTCOME_PENDING) {
        if (pthread_cond_timedwait(&g_signal, &g_lock, &deadline) == ETIMEDOUT) {
            break;
        }
    }
    Outcome outcome = g_outcome;
    char message[sizeof(g_error_message)];
    memcpy(message, g_error_message, sizeof(message));
    pthread_mutex_unlock(&g_lock);

    int status;
    switch (outcome) {
        case OUTCOME_EXIT_REQUESTED:
            printf("[macos-headless] the shipping archive evaluated JavaScript: the content "
                   "summed to 500500 and reached migo.exitMiniProgram()\n");
            status = 0;
            break;
        case OUTCOME_ERROR:
            fprintf(stderr,
                    "[macos-headless] the content raised an error rather than finishing: %s\n",
                    message);
            status = 1;
            break;
        default:
            fprintf(stderr,
                    "[macos-headless] nothing was heard from the content in %d seconds. ready=%d "
                    "frames_requested=%d -- a ready callback with no exit means the script was "
                    "evaluated and did not finish; neither means it was never evaluated\n",
                    DEADLINE_SECONDS, atomic_load(&g_ready),
                    atomic_load(&g_frames_requested));
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

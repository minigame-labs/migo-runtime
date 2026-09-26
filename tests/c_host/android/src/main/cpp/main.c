/*
 * A NativeActivity host that embeds migo through the public C ABI.
 *
 * There is no Java in this application. NativeActivity loads this library and
 * calls ANativeActivity_onCreate; the NDK's glue turns the framework callbacks
 * into a command queue this file drains. Everything migo-related below uses
 * nothing but the headers under include/migo -- if this file ever needs
 * something else, the ABI is incomplete, and finding that out is why the
 * example exists.
 *
 * The Linux counterpart is tests/c_host/main.c. The three things that differ
 * are marked: the dispatcher runs on the glue's looper, touch carries every
 * pointer, and the lifecycle comes from real Android commands.
 */
#include <android/choreographer.h>
#include <android/log.h>
#include <android/native_window.h>
#include <android_native_app_glue.h>
#include <dlfcn.h>
#include <fcntl.h>
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#include <migo/migo.h>
/* migo.h aggregates the portable headers; a platform descriptor is opt-in, so
 * a host includes exactly the one platform it targets. */
#include <migo/platform/android.h>

#define TAG "migo-c-host"
#define LOGI(...) __android_log_print(ANDROID_LOG_INFO, TAG, __VA_ARGS__)
#define LOGE(...) __android_log_print(ANDROID_LOG_ERROR, TAG, __VA_ARGS__)

/* Content id is fixed here; the probe bundle and a game are both pushed to the
 * app's files dir, so switching is a matter of which id this names. */
#define MIGO_DEFAULT_CONTENT_ID "bunnymark"

/* Reads <files>/content-id if the host left one there, else the default. A real
 * embedder knows its content; this example has to be pointed at different
 * bundles during validation, and a file is the smallest thing that behaves the
 * way a host's own configuration would. */
/* Reads the first line of <files>/<name> into out; 0 when absent or empty. */
static int read_host_setting(const char *files_dir, const char *name, char *out, size_t cap) {
    char path[512];
    snprintf(path, sizeof path, "%s/%s", files_dir, name);
    FILE *f = fopen(path, "r");
    if (f == NULL) return 0;
    char buf[128] = {0};
    int found = 0;
    if (fgets(buf, sizeof buf, f) != NULL) {
        buf[strcspn(buf, "\r\n")] = 0;
        if (buf[0] != 0) {
            snprintf(out, cap, "%s", buf);
            found = 1;
        }
    }
    fclose(f);
    return found;
}

static void read_content_id(const char *files_dir, char *out, size_t cap) {
    if (!read_host_setting(files_dir, "content-id", out, cap)) {
        snprintf(out, cap, "%s", MIGO_DEFAULT_CONTENT_ID);
    }
}

/* <files>/log-level, if present, becomes MIGO_CAPI_LOG before the engine is
 * created -- the setenv the note on create_engine describes, made switchable
 * without a rebuild because a NativeActivity cannot be handed an environment.
 * migo-bench sets it to "warn": the Java SDK logs at WARN by default, so that is
 * what puts content's console on logcat on both sides of a comparison. */
static void apply_log_level(const char *files_dir) {
    char level[32];
    if (read_host_setting(files_dir, "log-level", level, sizeof level)) {
        setenv("MIGO_CAPI_LOG", level, 1);
        LOGI("MIGO_CAPI_LOG=%s", level);
    }
}

struct dispatch_msg {
    MigoTaskFn fn;
    void *ctx;
};

struct host {
    MigoEngine *engine;
    MigoSession *session;
    MigoSurfaceAttachment *attachment;
    float density;
    int content_loaded;
    char content_id[128];
    int dispatch_pipe[2]; /* [0] read, [1] write */
    /* Drives the scripted pad; see GAMEPAD_SCRIPT_FRAME. */
    unsigned frames_delivered;
    /* The generation of the live attachment, and the high-water mark the next
     * attach has to beat.
     *
     * Every attach must carry a generation strictly greater than any the Session
     * has accepted, and Android destroys and recreates the window on every trip
     * through the background -- so a host that stamps a constant attaches once
     * and is then refused with MIGO_ERROR_STALE_SURFACE for the rest of the
     * process, leaving the app with no surface after its first resume. A metrics
     * update is the opposite rule: it names the attachment it is updating, so it
     * carries exactly this value rather than the next one. */
    uint64_t surface_generation;
    /* Needed for the soft keyboard: showing and hiding the system IME is an
     * activity-level operation, and this host has no Java side to ask. */
    ANativeActivity *activity;
};

static struct host g_host;

/* ---- Callbacks, delivered through the host's own dispatcher ------------- */

/*
 * The engine must never run host code on an engine thread, so the dispatcher
 * hands the task to the looper the glue already runs: write it to a pipe the
 * looper polls, and run it on the main thread when the loop drains it.
 * Inventing a queue here would demonstrate a bespoke threading scheme rather
 * than Android integration.
 */
static MigoResult dispatch(void *context, MigoTaskFn task, void *task_context) {
    struct host *h = (struct host *)context;
    struct dispatch_msg msg = {task, task_context};
    ssize_t written = write(h->dispatch_pipe[1], &msg, sizeof msg);
    if (written != (ssize_t)sizeof msg) {
        /* Returning the rejection hands ownership back to the engine, which
         * drops the task rather than leaking it or running it on its own
         * thread. Silently succeeding here would do the latter. */
        LOGE("dispatch rejected: %s", strerror(errno));
        return MIGO_ERROR_DISPATCH_REJECTED;
    }
    return MIGO_OK;
}

static int drain_dispatch(int fd, int events, void *data) {
    (void)events;
    (void)data;
    struct dispatch_msg msg;
    while (read(fd, &msg, sizeof msg) == (ssize_t)sizeof msg) {
        msg.fn(msg.ctx);
    }
    return 1; /* keep the descriptor registered */
}

/*
 * Frame pacing.
 *
 * The engine asks for one frame at a time through on_request_frame; the host
 * answers with the display's own vsync. AChoreographer is what Android provides
 * for that, available since API 24 -- below this project's API 26 floor -- and
 * is the same signal the Java SDK uses. Letting the engine pace itself instead
 * would run the render loop off a timer that cannot align to the display.
 */
static void send_gamepad_script(struct host *h);

/*
 * The scripted pad runs on the host's own timeline, not off a keyboard
 * callback.
 *
 * It used to be sent from on_show_keyboard, which meant only content that asked
 * for the keyboard ever saw a gamepad -- so gamepad-probe, which asks for no
 * keyboard, sat on its "no pad has connected" colour forever and the one probe
 * written to exercise the gamepad round trip could not be exercised at all on
 * Android. The Linux host already drives the pad from its own clock; this
 * matches it, one second in, so both probes are fed the same way.
 */
#define GAMEPAD_SCRIPT_FRAME 60

static void deliver_vsync(struct host *h, int64_t frame_time_nanos) {
    if (h->session == NULL) return;
    MigoResult result = migo_session_notify_vsync(h->session, frame_time_nanos);
    if (result != MIGO_OK && result != MIGO_ERROR_INVALID_STATE) {
        LOGE("notify_vsync failed: %d", (int)result);
    }
    if (h->frames_delivered == GAMEPAD_SCRIPT_FRAME) send_gamepad_script(h);
    h->frames_delivered++;
}

/*
 * Two callback shapes, because the API changed under us.
 *
 * AChoreographer_postFrameCallback takes a `long`, which cannot hold a
 * nanosecond timestamp on 32-bit, so API 29 deprecated it in favour of
 * postFrameCallback64. The modern entry is resolved at runtime and the legacy
 * one kept for the API 26-28 range this project still supports -- the same
 * shape as the EGL extension entry points elsewhere in the engine, and the
 * reason is the same: a compile-time choice would either break the floor or
 * leave newer devices on a deprecated path.
 */
static void on_frame64(int64_t frame_time_nanos, void *data) {
    deliver_vsync((struct host *)data, frame_time_nanos);
}

static void on_frame_legacy(long frame_time_nanos, void *data) {
    deliver_vsync((struct host *)data, (int64_t)frame_time_nanos);
}

typedef void (*post_frame_callback64_fn)(AChoreographer *, void (*)(int64_t, void *), void *);

/* Runs on the main thread: the engine posted this through our dispatcher. */
static void on_request_frame(void *user_data, MigoSession *session) {
    (void)session;
    struct host *h = (struct host *)user_data;
    AChoreographer *grapher = AChoreographer_getInstance();
    if (grapher == NULL) {
        LOGE("no Choreographer on this thread");
        return;
    }

    static post_frame_callback64_fn post64 = NULL;
    static int resolved = 0;
    if (!resolved) {
        resolved = 1;
        post64 = (post_frame_callback64_fn)dlsym(RTLD_DEFAULT,
                                                 "AChoreographer_postFrameCallback64");
        LOGI("frame pacing: %s", post64 ? "postFrameCallback64" : "legacy postFrameCallback");
    }

    if (post64 != NULL) {
        post64(grapher, on_frame64, h);
    } else {
        AChoreographer_postFrameCallback(grapher, on_frame_legacy, h);
    }
}

static void on_ready(void *user_data, MigoSession *session) {
    (void)user_data;
    (void)session;
    LOGI("callback: content is ready");
}

static void on_exit_requested(void *user_data, MigoSession *session) {
    (void)user_data;
    (void)session;
    LOGI("callback: content requested exit");
}

/* ---- Soft keyboard ------------------------------------------------------
 *
 * The engine has no keyboard of its own, and on Android its platform accessor
 * reaches the Java SDK over JNI -- which this host, being pure native, has not
 * got. So the keyboard is a capability supplied here, through the three
 * callbacks that install together.
 *
 * Show and hide drive the real system IME through the NDK's activity API, so
 * content's migo.showKeyboard genuinely raises the keyboard on the device.
 *
 * The text is scripted rather than read back from the IME: recovering Unicode
 * text from a NativeActivity means going through KeyEvent.getUnicodeChar over
 * JNI, which is a Java dependency this example exists to avoid. The script is
 * enough to prove the inbound ABI path on device, and it makes the expected
 * pixels exact.
 */
static void send_keyboard(struct host *h, MigoKeyboardEventType type, const char *value,
                          double height_css_px) {
    if (h->session == NULL || h->attachment == NULL) return;

    MigoKeyboardEvent event;
    memset(&event, 0, sizeof event);
    event.struct_size = (uint32_t)sizeof event;
    event.abi_version = MIGO_ABI_VERSION_CURRENT;
    event.event_type = type;
    event.value_utf8 = value;
    event.value_length = value ? (uint32_t)strlen(value) : 0;
    event.height_css_px = height_css_px;

    MigoResult result = migo_session_send_keyboard_event(h->session, &event);
    if (result != MIGO_OK) {
        LOGE("keyboard event %u not delivered: %d", (unsigned)type, (int)result);
    }
}

/* ---- Composition and gamepad, scripted like the Linux example ----
 *
 * A real IME's text cannot be read from a NativeActivity without going through
 * KeyEvent.getUnicodeChar over JNI, and reading a real gamepad needs device
 * enumeration; both are host work rather than ABI. The script is what proves
 * the ABI carries them, and it makes the expected pixels exact.
 */
static void send_composition(struct host *h, MigoCompositionEventType type, const char *data) {
    if (h->session == NULL || h->attachment == NULL) return;

    MigoCompositionEvent event;
    memset(&event, 0, sizeof event);
    event.struct_size = (uint32_t)sizeof event;
    event.abi_version = MIGO_ABI_VERSION_CURRENT;
    event.event_type = type;
    event.data_utf8 = data;
    event.data_length = data ? (uint32_t)strlen(data) : 0;

    MigoResult result = migo_session_send_composition_event(h->session, &event);
    if (result != MIGO_OK) {
        LOGE("composition event %u not delivered: %d", (unsigned)type, (int)result);
    }
}

#define PROBE_AXES 4
#define PROBE_BUTTONS 17

static void send_gamepad_script(struct host *h) {
    if (h->session == NULL || h->attachment == NULL) return;

    MigoGamepadInfo info;
    memset(&info, 0, sizeof info);
    info.struct_size = (uint32_t)sizeof info;
    info.abi_version = MIGO_ABI_VERSION_CURRENT;
    info.index = 0;
    info.axis_count = PROBE_AXES;
    info.button_count = PROBE_BUTTONS;
    info.id_utf8 = "Migo Scripted Pad (Vendor: 0000 Product: 0000)";
    info.mapping_utf8 = "standard";
    LOGI("gamepad connect: %d", (int)migo_session_set_gamepad_connected(h->session, &info, 1));

    float axes[PROBE_AXES] = {0.5f, -0.5f, 0.0f, 0.0f};
    MigoGamepadButton buttons[PROBE_BUTTONS];
    memset(buttons, 0, sizeof buttons);
    buttons[0].flags = MIGO_GAMEPAD_BUTTON_FLAG_PRESSED | MIGO_GAMEPAD_BUTTON_FLAG_TOUCHED;
    buttons[0].value = 1.0f;
    /* Held at quarter travel but NOT pressed: the case that proves pressed is
     * carried rather than derived from value. */
    buttons[6].flags = MIGO_GAMEPAD_BUTTON_FLAG_TOUCHED;
    buttons[6].value = 0.25f;

    MigoGamepadStateEvent state;
    memset(&state, 0, sizeof state);
    state.struct_size = (uint32_t)sizeof state;
    state.abi_version = MIGO_ABI_VERSION_CURRENT;
    state.index = 0;
    state.axis_count = PROBE_AXES;
    state.button_count = PROBE_BUTTONS;
    state.axes = axes;
    state.buttons = buttons;
    state.timestamp_ms = 16.0;
    LOGI("gamepad sample: %d", (int)migo_session_send_gamepad_state(h->session, &state));
}

static void on_show_keyboard(void *user_data, MigoSession *session,
                             const MigoKeyboardShowOptions *options) {
    struct host *h = (struct host *)user_data;
    (void)session;
    LOGI("callback: show keyboard max_length=%u type=%u confirm=%u flags=0x%x default='%.*s'",
         options->max_length, options->keyboard_type, options->confirm_type, options->flags,
         (int)options->default_value_length, options->default_value_utf8);

    if (h->activity != NULL) {
        ANativeActivity_showSoftInput(h->activity, ANATIVEACTIVITY_SHOW_SOFT_INPUT_IMPLICIT);
    }

    /* Fed straight from here, unlike the Linux example: this callback already
     * runs on the main thread, because our dispatcher hands tasks to the
     * looper. The engine's JS thread returned from showKeyboard as soon as the
     * task was queued, so nothing is re-entered. */
    static const char *const typed[] = {"m", "mi", "mig", "migo"};
    /* The IME covering the lower part of the screen, in CSS pixels: the device
     * reports physical ones, and density is the same factor the attach
     * descriptor used. */
    send_keyboard(h, MIGO_KEYBOARD_EVENT_HEIGHT_CHANGE, NULL, 700.0 / h->density);
    for (size_t i = 0; i < sizeof typed / sizeof typed[0]; ++i) {
        send_keyboard(h, MIGO_KEYBOARD_EVENT_INPUT, typed[i], 0.0);
    }
    /* The IME half: a preedit that grows, then commits. Multi-byte on purpose --
     * preedit text being non-ASCII is the whole reason composition exists, and
     * a boundary that mangled it would look fine for ASCII. */
    send_composition(h, MIGO_COMPOSITION_EVENT_START, "");
    send_composition(h, MIGO_COMPOSITION_EVENT_UPDATE, "ni");
    send_composition(h, MIGO_COMPOSITION_EVENT_UPDATE, "nihao");
    send_composition(h, MIGO_COMPOSITION_EVENT_END, "\u4f60\u597d");
    send_keyboard(h, MIGO_KEYBOARD_EVENT_INPUT, "migo\u4f60\u597d", 0.0);

    send_keyboard(h, MIGO_KEYBOARD_EVENT_CONFIRM, "migo", 0.0);
    send_keyboard(h, MIGO_KEYBOARD_EVENT_COMPLETE, "migo", 0.0);
    send_keyboard(h, MIGO_KEYBOARD_EVENT_HEIGHT_CHANGE, NULL, 0.0);
}

static void on_hide_keyboard(void *user_data, MigoSession *session) {
    struct host *h = (struct host *)user_data;
    (void)session;
    LOGI("callback: hide keyboard");
    if (h->activity != NULL) {
        ANativeActivity_hideSoftInput(h->activity, ANATIVEACTIVITY_HIDE_SOFT_INPUT_IMPLICIT_ONLY);
    }
}

static void on_update_keyboard(void *user_data, MigoSession *session, const char *value_utf8,
                               uint32_t value_length) {
    (void)user_data;
    (void)session;
    /* Length-delimited and borrowed for this call only. */
    LOGI("callback: update keyboard '%.*s'", (int)value_length, value_utf8);
}

static void on_error(void *user_data, MigoSession *session, const MigoError *error) {
    (void)user_data;
    (void)session;
    LOGE("callback: error %d: %s", error ? (int)error->code : 0,
         (error && error->message_utf8) ? error->message_utf8 : "(none)");
}

/* ---- Touch ------------------------------------------------------------- */

/*
 * AMotionEvent_getPressure is calibrated per device, not normalized: Android's
 * own docs say 1.0 is "generally considered normal" but larger values happen,
 * and real touchscreens reach several times that. The ABI's pressure is the
 * same [0, 1] contract as the web's Touch.force, so this host has to do the
 * normalizing -- the same way it already divides x/y by density instead of
 * handing over physical pixels. Mirrors TouchInputNormalizer.pressure() in
 * the Java platform library, the one host that was already doing this.
 */
static float normalize_pressure(float value) {
    if (!(value > 0.0f)) return 0.0f; /* also catches NaN and negative zero */
    return value < 1.0f ? value : 1.0f;
}

/*
 * Every pointer is carried, which is the part the Linux example cannot test:
 * there one mouse maps to one point, so the multi-point path and the
 * per-pointer flags have never run until here.
 */
static void forward_touch(struct host *h, AInputEvent *event) {
    if (h->session == NULL || h->attachment == NULL) return;

    int32_t action = AMotionEvent_getAction(event);
    int32_t masked = action & AMOTION_EVENT_ACTION_MASK;
    size_t count = AMotionEvent_getPointerCount(event);
    if (count > MIGO_TOUCH_MAX_POINTS) count = MIGO_TOUCH_MAX_POINTS;
    if (count == 0) return;

    MigoTouchType type;
    switch (masked) {
    case AMOTION_EVENT_ACTION_DOWN:
    case AMOTION_EVENT_ACTION_POINTER_DOWN:
        type = MIGO_TOUCH_START;
        break;
    case AMOTION_EVENT_ACTION_MOVE:
        type = MIGO_TOUCH_MOVE;
        break;
    case AMOTION_EVENT_ACTION_UP:
    case AMOTION_EVENT_ACTION_POINTER_UP:
        type = MIGO_TOUCH_END;
        break;
    case AMOTION_EVENT_ACTION_CANCEL:
        type = MIGO_TOUCH_CANCEL;
        break;
    default:
        return;
    }

    /* For the pointer-specific actions exactly one pointer changed; for MOVE
     * they all did. Getting this wrong shows up as content seeing the wrong
     * finger lift -- precisely what a single-pointer host cannot catch. */
    size_t changed_index =
        (size_t)((action & AMOTION_EVENT_ACTION_POINTER_INDEX_MASK) >>
                 AMOTION_EVENT_ACTION_POINTER_INDEX_SHIFT);
    int all_changed = (masked == AMOTION_EVENT_ACTION_MOVE);

    MigoTouchPoint points[MIGO_TOUCH_MAX_POINTS];
    memset(points, 0, sizeof points);
    for (size_t i = 0; i < count; i++) {
        points[i].id = (uint32_t)AMotionEvent_getPointerId(event, i);
        /* Android reports physical pixels; the ABI takes CSS pixels. */
        points[i].x = AMotionEvent_getX(event, i) / h->density;
        points[i].y = AMotionEvent_getY(event, i) / h->density;
        points[i].pressure = normalize_pressure(AMotionEvent_getPressure(event, i));
        if (all_changed || i == changed_index) {
            points[i].flags |= MIGO_TOUCH_FLAG_CHANGED;
            if (type == MIGO_TOUCH_END || type == MIGO_TOUCH_CANCEL) {
                points[i].flags |= MIGO_TOUCH_FLAG_REMOVED;
            }
        }
    }

    MigoTouchEvent touch;
    memset(&touch, 0, sizeof touch);
    touch.struct_size = (uint32_t)sizeof touch;
    touch.abi_version = MIGO_ABI_VERSION_CURRENT;
    touch.type = type;
    touch.point_count = (uint32_t)count;
    touch.timestamp_ms = (int64_t)(AMotionEvent_getEventTime(event) / 1000000);
    touch.points = points;

    MigoResult result = migo_session_send_touch(h->session, &touch);
    if (result != MIGO_OK) {
        LOGE("touch not delivered: %d (points=%u)", (int)result, (unsigned)count);
    }
}

static int32_t on_input(struct android_app *app, AInputEvent *event) {
    struct host *h = (struct host *)app->userData;
    if (AInputEvent_getType(event) == AINPUT_EVENT_TYPE_MOTION) {
        forward_touch(h, event);
        return 1;
    }
    return 0;
}

/* ---- Engine and session ------------------------------------------------ */

/*
 * A note on diagnosing this example, because the usual channel is missing here.
 *
 * The library does not hijack the process logger; it only installs one when
 * MIGO_CAPI_LOG is set, and a NativeActivity cannot be handed an environment
 * through `am start`. So a pure-native host on Android has no engine-side log
 * unless it asks for one -- and without it, a content-side exception (a
 * TypeError in the first paint, say) looks exactly like a dead frame loop or a
 * broken surface: the host's own callbacks all report success and the screen
 * stays black. To get the engine's view, call
 *
 *     setenv("MIGO_CAPI_LOG", "info", 1);
 *
 * here, before the first engine call, and rebuild. It is left out of the
 * shipping path on purpose: a host in production receives operational failures
 * through on_error, not through logcat.
 */
static int create_engine(struct host *h, const char *files_dir) {
    char cache_dir[512];
    char code_cache_dir[512];
    snprintf(cache_dir, sizeof cache_dir, "%s/migo-cache", files_dir);
    snprintf(code_cache_dir, sizeof code_cache_dir, "%s/migo-code-cache", files_dir);

    /* No JNI reaches this process on its own -- ANativeActivity_onCreate is not
     * JNI_OnLoad. Without this, the audio backend's first output stream aborts
     * the process instead of failing an op: see migo_android_init_context's
     * doc comment in migo/platform/android.h. */
    if (h->activity != NULL) {
        migo_android_init_context(h->activity->vm, h->activity->clazz);
    }

    /* Ask the library what it supports before building anything on top of it.
     * MIGO_C_ABI_HAS_RUNTIME is a preprocessor macro and describes the headers
     * this file compiled against; only this call describes the linked library.
     * Checking the surface kind here turns an unsupported build into a clear
     * message now instead of a failed attach later. */
    MigoCapabilities caps;
    memset(&caps, 0, sizeof caps);
    caps.struct_size = (uint32_t)sizeof caps;
    caps.abi_version = MIGO_ABI_VERSION_CURRENT;
    MigoResult caps_result = migo_query_capabilities(&caps);
    if (caps_result != MIGO_OK) {
        LOGE("migo_query_capabilities failed: %d", (int)caps_result);
        return 0;
    }
    LOGI("capabilities: abi %u..%u, platform kinds 0x%llx",
         caps.abi_version_min, caps.abi_version_max,
         (unsigned long long)caps.platform_kinds);
    if ((caps.platform_kinds & (UINT64_C(1) << MIGO_PLATFORM_ANDROID_NATIVE_WINDOW)) == 0) {
        LOGE("this build cannot attach an ANativeWindow");
        return 0;
    }

    MigoEngineConfig engine_config;
    memset(&engine_config, 0, sizeof engine_config);
    engine_config.struct_size = (uint32_t)sizeof engine_config;
    engine_config.abi_version = MIGO_ABI_VERSION_CURRENT;
    /* Development example: the pushed content carries no signing receipt.
     * A production host leaves this clear and ships signed content. */
    engine_config.flags = MIGO_ENGINE_FLAG_ALLOW_UNSIGNED_CONTENT;
    engine_config.files_dir_utf8 = files_dir;
    engine_config.cache_dir_utf8 = cache_dir;
    engine_config.code_cache_dir_utf8 = code_cache_dir;

    MigoResult result = migo_engine_create(&engine_config, &h->engine);
    if (result != MIGO_OK) {
        LOGE("migo_engine_create failed: %d", (int)result);
        return 0;
    }

    MigoSessionConfig session_config;
    memset(&session_config, 0, sizeof session_config);
    session_config.struct_size = (uint32_t)sizeof session_config;
    session_config.abi_version = MIGO_ABI_VERSION_CURRENT;
    session_config.flags = MIGO_SESSION_FLAG_NONE;

    result = migo_session_create(h->engine, &session_config, &h->session);
    if (result != MIGO_OK) {
        LOGE("migo_session_create failed: %d", (int)result);
        return 0;
    }

    /* Callbacks install once, before the first attach: a queued task must never
     * see a replaced function pointer. */
    MigoHostCallbacks callbacks;
    memset(&callbacks, 0, sizeof callbacks);
    callbacks.struct_size = (uint32_t)sizeof callbacks;
    callbacks.abi_version = MIGO_ABI_VERSION_CURRENT;
    callbacks.user_data = h;
    callbacks.dispatcher_data = h;
    callbacks.dispatch = dispatch;
    callbacks.on_ready = on_ready;
    callbacks.on_exit_requested = on_exit_requested;
    callbacks.on_error = on_error;
    /* Installing this is what tells the engine the host paces frames. */
    callbacks.on_request_frame = on_request_frame;
    /* All three or none: a host that can open a keyboard but not close it would
     * strand it on screen, so a subset is refused at install time. */
    callbacks.on_show_keyboard = on_show_keyboard;
    callbacks.on_hide_keyboard = on_hide_keyboard;
    callbacks.on_update_keyboard = on_update_keyboard;

    result = migo_session_set_host_callbacks(h->session, &callbacks);
    if (result != MIGO_OK) {
        LOGE("migo_session_set_host_callbacks failed: %d", (int)result);
        return 0;
    }
    return 1;
}

static void attach_window(struct host *h, ANativeWindow *window) {
    if (h->session == NULL || window == NULL || h->attachment != NULL) return;

    int32_t width = ANativeWindow_getWidth(window);
    int32_t height = ANativeWindow_getHeight(window);

    MigoAndroidNativeWindowDescriptor native;
    memset(&native, 0, sizeof native);
    native.struct_size = (uint32_t)sizeof native;
    native.abi_version = MIGO_ABI_VERSION_CURRENT;
    native.platform_kind = MIGO_PLATFORM_ANDROID_NATIVE_WINDOW;
    native.flags = MIGO_PLATFORM_DESCRIPTOR_FLAG_NONE;
    /* The engine acquires its own reference; this one stays ours. */
    native.native_window = window;

    /* Claimed, not committed: a refused attach does not consume a generation on
     * the engine side, so the retry has to be able to offer this same one. */
    uint64_t generation = h->surface_generation + 1;

    MigoSurfaceDescriptor surface;
    memset(&surface, 0, sizeof surface);
    surface.struct_size = (uint32_t)sizeof surface;
    surface.abi_version = MIGO_ABI_VERSION_CURRENT;
    surface.generation = generation;
    surface.platform_kind = MIGO_PLATFORM_ANDROID_NATIVE_WINDOW;
    surface.flags = MIGO_SURFACE_DESCRIPTOR_FLAG_NONE;
    surface.width_pixels = (uint32_t)width;
    surface.height_pixels = (uint32_t)height;
    surface.scale_factor = h->density;
    surface.color_space = MIGO_COLOR_SPACE_SRGB;
    surface.alpha_mode = MIGO_ALPHA_MODE_OPAQUE;
    surface.preferred_presentation_mode = MIGO_PRESENTATION_MODE_DEFAULT;
    surface.capability_flags = MIGO_SURFACE_CAPABILITY_NONE;
    surface.platform_descriptor_size = (uint32_t)sizeof native;
    surface.platform_descriptor = &native;

    MigoResult result = migo_session_attach_surface(h->session, &surface, &h->attachment);
    if (result != MIGO_OK) {
        LOGE("migo_session_attach_surface failed: %d", (int)result);
        return;
    }
    h->surface_generation = generation;
    LOGI("attached %dx%d density=%.2f generation=%llu", width, height, h->density,
         (unsigned long long)generation);

    if (!h->content_loaded) {
        MigoContentDescriptor content;
        memset(&content, 0, sizeof content);
        content.struct_size = (uint32_t)sizeof content;
        content.abi_version = MIGO_ABI_VERSION_CURRENT;
        content.flags = MIGO_CONTENT_FLAG_NONE;
        content.content_id_utf8 = h->content_id;
        content.entry_utf8 = "game.js";
        result = migo_session_load_content(h->session, &content);
        if (result != MIGO_OK) {
            LOGE("migo_session_load_content failed: %d", (int)result);
            return;
        }
        h->content_loaded = 1;
        LOGI("loaded content '%s'", h->content_id);
    }
}

/* Long enough that a busy driver finishes, short enough that a stuck one shows
 * up as a logged timeout instead of an ANR with no explanation. */
#define RELEASE_TIMEOUT_MS 2000
#define RELEASE_POLL_US 2000

/*
 * Retire the attachment and block until the ANativeWindow is genuinely unused.
 *
 * This must block, and it must block here. android_native_app_glue frees its
 * reference to the window once the APP_CMD_TERM_WINDOW handler returns, so
 * returning while the release is still pending hands a live window back to the
 * framework while the GL driver may still be reading it. Blocking briefly on
 * the app thread during teardown is the cheaper of the two costs.
 *
 * The observer is level-triggered, so a release completing before the first
 * query is still observed; there is no window between begin and poll to lose.
 */
static void detach_window(struct host *h) {
    if (h->attachment == NULL) return;

    MigoSurfaceRelease *release = NULL;
    MigoResult result = migo_surface_begin_detach(h->attachment, &release);
    if (result != MIGO_OK) {
        LOGE("migo_surface_begin_detach failed: %d", (int)result);
        return;
    }
    /* Retirement is irreversible, so the attachment is gone regardless of how
     * the wait below turns out. Clearing it now keeps a failed wait from
     * leaving a dangling pointer that a later command would reuse. */
    h->attachment = NULL;

    for (long waited_us = 0; waited_us < RELEASE_TIMEOUT_MS * 1000L;
         waited_us += RELEASE_POLL_US) {
        MigoSurfaceReleaseStatus status;
        memset(&status, 0, sizeof status);
        status.struct_size = (uint32_t)sizeof status;
        status.abi_version = MIGO_ABI_VERSION_CURRENT;
        result = migo_surface_release_query(release, &status);
        if (result != MIGO_OK) {
            LOGE("migo_surface_release_query failed: %d", (int)result);
            return;
        }
        if (status.state == MIGO_SURFACE_RELEASE_RELEASED) {
            result = migo_surface_release_destroy(release);
            if (result != MIGO_OK) {
                LOGE("migo_surface_release_destroy failed: %d", (int)result);
            }
            LOGI("detached");
            return;
        }
        usleep(RELEASE_POLL_US);
    }

    /* Leaked deliberately: it is the only remaining way to learn when the
     * window becomes safe, and the framework is about to reclaim it anyway. */
    LOGE("surface release timed out; window may still be in use by the driver");
}

static void update_surface(struct host *h, ANativeWindow *window) {
    if (h->attachment == NULL || window == NULL) return;

    MigoSurfaceMetrics metrics;
    memset(&metrics, 0, sizeof metrics);
    metrics.struct_size = (uint32_t)sizeof metrics;
    metrics.abi_version = MIGO_ABI_VERSION_CURRENT;
    metrics.generation = h->surface_generation;
    metrics.width_pixels = (uint32_t)ANativeWindow_getWidth(window);
    metrics.height_pixels = (uint32_t)ANativeWindow_getHeight(window);
    metrics.scale_factor = h->density;
    metrics.color_space = MIGO_COLOR_SPACE_SRGB;
    metrics.alpha_mode = MIGO_ALPHA_MODE_OPAQUE;
    metrics.preferred_presentation_mode = MIGO_PRESENTATION_MODE_DEFAULT;
    metrics.flags = MIGO_SURFACE_DESCRIPTOR_FLAG_NONE;

    MigoResult result = migo_surface_update(h->attachment, &metrics);
    if (result != MIGO_OK) LOGE("migo_surface_update failed: %d", (int)result);
}

static void set_visibility(struct host *h, int visible) {
    if (h->session == NULL) return;
    /* The setter takes a plain boolean, not an enum. */
    MigoResult result = migo_session_set_visibility(h->session, visible ? 1 : 0);
    if (result != MIGO_OK) LOGE("migo_session_set_visibility failed: %d", (int)result);
}

/* ---- Glue command handling --------------------------------------------- */

static void on_cmd(struct android_app *app, int32_t cmd) {
    struct host *h = (struct host *)app->userData;
    switch (cmd) {
    case APP_CMD_INIT_WINDOW:
        attach_window(h, app->window);
        break;
    case APP_CMD_TERM_WINDOW:
        detach_window(h);
        break;
    case APP_CMD_WINDOW_RESIZED:
    case APP_CMD_CONFIG_CHANGED:
        update_surface(h, app->window);
        break;
    case APP_CMD_GAINED_FOCUS:
        if (h->session) migo_session_set_focus(h->session, 1);
        break;
    case APP_CMD_LOST_FOCUS:
        if (h->session) migo_session_set_focus(h->session, 0);
        break;
    case APP_CMD_RESUME:
        set_visibility(h, 1);
        break;
    case APP_CMD_PAUSE:
        set_visibility(h, 0);
        break;
    default:
        break;
    }
}

void android_main(struct android_app *app) {
    memset(&g_host, 0, sizeof g_host);
    g_host.density = (float)AConfiguration_getDensity(app->config) / 160.0f;
    if (g_host.density <= 0.0f) g_host.density = 1.0f;

    app->userData = &g_host;
    app->onAppCmd = on_cmd;
    app->onInputEvent = on_input;

    /*
     * O_NONBLOCK is required, not a nicety. drain_dispatch reads until the pipe
     * is empty, and on a blocking pipe that final read never returns: the
     * looper callback holds the thread forever, nothing else on the looper is
     * ever polled again, and any Choreographer callback posted from inside it
     * is never delivered. That presented as "the engine asks for one frame and
     * nothing ever renders".
     */
    if (pipe2(g_host.dispatch_pipe, O_NONBLOCK | O_CLOEXEC) != 0) {
        LOGE("dispatch pipe: %s", strerror(errno));
        return;
    }
    ALooper_addFd(app->looper, g_host.dispatch_pipe[0], ALOOPER_POLL_CALLBACK,
                  ALOOPER_EVENT_INPUT, drain_dispatch, NULL);

    g_host.activity = app->activity;
    LOGI("starting, internalDataPath=%s density=%.2f", app->activity->internalDataPath,
         g_host.density);
    read_content_id(app->activity->internalDataPath, g_host.content_id,
                    sizeof g_host.content_id);
    apply_log_level(app->activity->internalDataPath);
    if (!create_engine(&g_host, app->activity->internalDataPath)) return;

    while (1) {
        int events;
        struct android_poll_source *source;
        /*
         * pollOnce, not pollAll. ALooper_pollAll is deprecated because it loops
         * internally and discards ALOOPER_POLL_CALLBACK, which silently drops
         * callbacks other components registered on the looper -- AChoreographer
         * among them. With pollAll the engine asked for frames, the host armed
         * a Choreographer callback, and the callback never arrived.
         */
        while (ALooper_pollOnce(-1, NULL, &events, (void **)&source) >= 0) {
            if (source != NULL) source->process(app, source);
            if (app->destroyRequested != 0) {
                LOGI("destroy requested, tearing down");
                detach_window(&g_host);
                if (g_host.session) migo_session_destroy(g_host.session);
                if (g_host.engine) migo_engine_destroy(g_host.engine);
                close(g_host.dispatch_pipe[0]);
                close(g_host.dispatch_pipe[1]);
                return;
            }
        }
    }
}

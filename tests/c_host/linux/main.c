/*
 * Minimal third-party host: opens an X11 window and runs a Migo game through
 * the public C ABI only.
 *
 * This example is the ABI's acceptance test. It includes nothing but the public
 * migo headers, so if it ever needs a private engine detail to work, the ABI is
 * incomplete — which is the feedback we want before v1 is frozen.
 *
 * Build + run: scripts/dev-run-c-host.sh
 */

/* nanosleep is POSIX, not ISO C11, so ask for it explicitly. */
#define _POSIX_C_SOURCE 199309L

#include <migo/migo.h>
#include <migo/platform/x11.h>

#include "services.h"

#include <X11/Xlib.h>

#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

#define WINDOW_WIDTH 720
#define WINDOW_HEIGHT 1280

static void sleep_ms(long ms) {
    struct timespec ts;
    ts.tv_sec = ms / 1000;
    ts.tv_nsec = (ms % 1000) * 1000000L;
    nanosleep(&ts, NULL);
}

/* ---- Host callbacks. The engine never calls these directly: it hands a task
 * to our dispatcher, and we run it where we choose. This example runs it
 * inline, which is legal — "exactly once, inline or later". ---- */
static MigoResult MIGO_CALL dispatch_inline(void *dispatcher_context, MigoTaskFn task,
                                            void *task_context) {
    (void)dispatcher_context;
    task(task_context);
    return MIGO_OK;
}

static void MIGO_CALL on_ready(void *user_data, MigoSession *session) {
    (void)user_data;
    (void)session;
    printf("[c-host] callback: content is ready\n");
    fflush(stdout);
}

static void MIGO_CALL on_error(void *user_data, MigoSession *session,
                               const MigoError *error) {
    (void)user_data;
    (void)session;
    fprintf(stderr, "[c-host] callback: engine error %d: %.*s\n", (int)error->code,
            (int)error->message_length, error->message_utf8);
    fflush(stderr);
}

static void MIGO_CALL on_exit_requested(void *user_data, MigoSession *session) {
    (void)user_data;
    (void)session;
    printf("[c-host] callback: content asked to exit\n");
    fflush(stdout);
}

/*
 * ---- The soft keyboard, which is a capability this host supplies. ----
 *
 * X11 has no keyboard the engine can ask for, so a host here does what a real
 * one does: it answers the request itself and feeds the resulting events back.
 * This example plays a fixed script rather than opening a real IME, because the
 * point is to prove the round trip and a scripted host makes the expected
 * pixels exact.
 *
 * Install all three or none -- a host that can show a keyboard but not hide it
 * strands it on screen, so a subset is refused at install time.
 *
 * The flag is atomic because these callbacks run on whichever thread our
 * dispatcher chose (here, the engine's own), while the loop that reads it runs
 * on main.
 */
static atomic_int g_keyboard_requested;

static void MIGO_CALL on_show_keyboard(void *user_data, MigoSession *session,
                                       const MigoKeyboardShowOptions *options) {
    (void)user_data;
    (void)session;
    printf("[c-host] show keyboard: max_length=%u type=%u confirm=%u flags=0x%x default='%.*s'\n",
           options->max_length, options->keyboard_type, options->confirm_type,
           options->flags, (int)options->default_value_length,
           options->default_value_utf8);
    fflush(stdout);
    atomic_store(&g_keyboard_requested, 1);
}

static void MIGO_CALL on_hide_keyboard(void *user_data, MigoSession *session) {
    (void)user_data;
    (void)session;
    printf("[c-host] hide keyboard\n");
    fflush(stdout);
}

static void MIGO_CALL on_update_keyboard(void *user_data, MigoSession *session,
                                         const char *value_utf8, uint32_t value_length) {
    (void)user_data;
    (void)session;
    /* Length-delimited and borrowed for this call only: print it, do not keep
     * the pointer. */
    printf("[c-host] update keyboard: '%.*s'\n", (int)value_length, value_utf8);
    fflush(stdout);
}

/*
 * ---- A scripted gamepad. ----
 *
 * X11 has no gamepad this example can read without pulling in evdev or SDL, and
 * the point here is the ABI, not device enumeration. So the host plays a fixed
 * script: announce a standard pad, sweep an axis and press a button for a while,
 * then withdraw it. A scripted host makes the expected pixels exact.
 */
#define PROBE_AXES 4
#define PROBE_BUTTONS 17

static void gamepad_info(MigoGamepadInfo *info) {
    memset(info, 0, sizeof *info);
    info->struct_size = (uint32_t)sizeof *info;
    info->abi_version = MIGO_ABI_VERSION_CURRENT;
    info->index = 0;
    info->axis_count = PROBE_AXES;
    info->button_count = PROBE_BUTTONS;
    info->id_utf8 = "Migo Scripted Pad (Vendor: 0000 Product: 0000)";
    info->mapping_utf8 = "standard";
}

static void send_gamepad_sample(MigoSession *session, double phase, int elapsed_ms) {
    float axes[PROBE_AXES];
    axes[0] = (float)phase;       /* swept, so a stuck axis is visible */
    axes[1] = (float)-phase;
    axes[2] = 0.0f;
    axes[3] = 0.0f;

    MigoGamepadButton buttons[PROBE_BUTTONS];
    memset(buttons, 0, sizeof buttons);
    /* Button 0 held: a digital press with value 1.0. */
    buttons[0].flags = MIGO_GAMEPAD_BUTTON_FLAG_PRESSED | MIGO_GAMEPAD_BUTTON_FLAG_TOUCHED;
    buttons[0].value = 1.0f;
    /* Button 6 is a trigger at quarter travel and NOT pressed -- the case that
     * proves pressed is carried rather than derived from value. */
    buttons[6].flags = MIGO_GAMEPAD_BUTTON_FLAG_TOUCHED;
    buttons[6].value = 0.25f;

    MigoGamepadStateEvent event;
    memset(&event, 0, sizeof event);
    event.struct_size = (uint32_t)sizeof event;
    event.abi_version = MIGO_ABI_VERSION_CURRENT;
    event.index = 0;
    event.axis_count = PROBE_AXES;
    event.button_count = PROBE_BUTTONS;
    event.axes = axes;
    event.buttons = buttons;
    event.timestamp_ms = (double)elapsed_ms;

    MigoResult result = migo_session_send_gamepad_state(session, &event);
    if (result != MIGO_OK) {
        fprintf(stderr, "[c-host] gamepad sample not delivered: %d\n", (int)result);
    }
}

static int fail(const char *what, MigoResult result) {
    fprintf(stderr, "[c-host] %s failed: %d\n", what, (int)result);
    return 1;
}

/* A driver holding the window past detach is normal; holding it this long is
 * not, and blocking forever would turn a stall into a hang with no diagnosis. */
#define RELEASE_TIMEOUT_MS 2000
#define RELEASE_POLL_MS 4

/*
 * Retire the attachment, then wait until the native window is genuinely unused.
 *
 * migo_surface_begin_detach returning MIGO_OK means retirement started, not
 * that the driver finished with the window. XDestroyWindow at that point is a
 * use-after-free inside the GL driver -- one the engine cannot detect, because
 * the reference it would have to observe belongs to the driver.
 *
 * The observer is level-triggered, so this poll cannot miss a completion that
 * lands before the first query. Polling (rather than a callback) keeps teardown
 * on the host's own thread, which is where the window is owned.
 */
static int detach_and_await_release(MigoSurfaceAttachment *attachment) {
    MigoSurfaceRelease *release = NULL;
    MigoResult result = migo_surface_begin_detach(attachment, &release);
    if (result != MIGO_OK) return fail("migo_surface_begin_detach", result);

    for (long waited = 0; waited < RELEASE_TIMEOUT_MS; waited += RELEASE_POLL_MS) {
        MigoSurfaceReleaseStatus status;
        memset(&status, 0, sizeof status);
        /* Output records are the mirror of input ones: the caller declares the
         * shape it understands and the library writes no more than that. */
        status.struct_size = (uint32_t)sizeof status;
        status.abi_version = MIGO_ABI_VERSION_CURRENT;
        result = migo_surface_release_query(release, &status);
        if (result != MIGO_OK) return fail("migo_surface_release_query", result);
        if (status.state == MIGO_SURFACE_RELEASE_RELEASED) {
            result = migo_surface_release_destroy(release);
            if (result != MIGO_OK) return fail("migo_surface_release_destroy", result);
            return 0;
        }
        sleep_ms(RELEASE_POLL_MS);
    }

    /* Deliberately leaks the observer. It is the only thing that could still
     * report when the window becomes safe to touch, so destroying it here would
     * discard the answer; and the window must now outlive us rather than be
     * torn down while the driver may still be reading it. */
    return fail("surface release timed out", MIGO_ERROR_INTERNAL);
}

/*
 * X11 reports physical pixels; the ABI takes CSS pixels. This one constant is
 * used both for the attach descriptor and for converting input, so the two
 * cannot drift -- sending physical pixels is what makes a game render correctly
 * and respond in the wrong place.
 */
static const float SCALE_FACTOR = 1.0f;

/* Maps one X11 pointer position onto a single-point touch event (id 0), which
 * is what mini-game content listens for; there is no separate mouse event model. */
static void send_touch(MigoSession *session, MigoTouchType type, int x, int y,
                       int64_t timestamp_ms) {
    int removed = (type == MIGO_TOUCH_END || type == MIGO_TOUCH_CANCEL);

    MigoTouchPoint point;
    memset(&point, 0, sizeof point);
    point.id = 0;
    point.x = (float)x / SCALE_FACTOR;
    point.y = (float)y / SCALE_FACTOR;
    point.pressure = removed ? 0.0f : 1.0f;
    point.flags = MIGO_TOUCH_FLAG_CHANGED;
    if (removed) point.flags |= MIGO_TOUCH_FLAG_REMOVED;

    MigoTouchEvent event;
    memset(&event, 0, sizeof event);
    event.struct_size = (uint32_t)sizeof event;
    event.abi_version = MIGO_ABI_VERSION_CURRENT;
    event.type = type;
    event.point_count = 1;
    event.timestamp_ms = timestamp_ms;
    event.points = &point;

    MigoResult result = migo_session_send_touch(session, &event);
    if (result != MIGO_OK) {
        fprintf(stderr, "[c-host] touch not delivered: %d\n", (int)result);
    }
}

/*
 * Same window, new size: the FastResize path, not a destroy/recreate, so the
 * attachment's generation is unchanged. Widening the mismatch between this and
 * a real destroy/recreate is the whole reason the two paths exist -- see
 * Android's on_configure_changed, which this mirrors exactly except for the
 * source of width/height.
 */
static void send_resize(MigoSurfaceAttachment *attachment, uint64_t generation,
                        uint32_t width, uint32_t height) {
    MigoSurfaceMetrics metrics;
    memset(&metrics, 0, sizeof metrics);
    metrics.struct_size = (uint32_t)sizeof metrics;
    metrics.abi_version = MIGO_ABI_VERSION_CURRENT;
    metrics.generation = generation;
    metrics.width_pixels = width;
    metrics.height_pixels = height;
    metrics.scale_factor = SCALE_FACTOR;
    metrics.color_space = MIGO_COLOR_SPACE_SRGB;
    metrics.alpha_mode = MIGO_ALPHA_MODE_OPAQUE;
    metrics.preferred_presentation_mode = MIGO_PRESENTATION_MODE_DEFAULT;
    metrics.flags = MIGO_SURFACE_DESCRIPTOR_FLAG_NONE;

    MigoResult result = migo_surface_update(attachment, &metrics);
    if (result != MIGO_OK) {
        fprintf(stderr, "[c-host] resize not delivered: %d\n", (int)result);
    }
}

static void send_keyboard(MigoSession *session, MigoKeyboardEventType type,
                          const char *value, double height_css_px) {
    MigoKeyboardEvent event;
    memset(&event, 0, sizeof event);
    event.struct_size = (uint32_t)sizeof event;
    event.abi_version = MIGO_ABI_VERSION_CURRENT;
    event.event_type = type;
    event.value_utf8 = value;
    event.value_length = value ? (uint32_t)strlen(value) : 0;
    event.height_css_px = height_css_px;

    MigoResult result = migo_session_send_keyboard_event(session, &event);
    if (result != MIGO_OK) {
        fprintf(stderr, "[c-host] keyboard event not delivered: %d\n", (int)result);
    }
}

/*
 * What a real IME would produce, once content asked for a keyboard: the
 * keyboard taking up the lower part of the screen, the user typing, then
 * confirming. Every text event carries the WHOLE current value, not the
 * keystroke -- content reads it as the field's contents.
 *
 * The height is CSS pixels, converted from this host's own pixels by the same
 * SCALE_FACTOR the attach descriptor used.
 */
static void send_composition(MigoSession *session, MigoCompositionEventType type,
                             const char *data) {
    MigoCompositionEvent event;
    memset(&event, 0, sizeof event);
    event.struct_size = (uint32_t)sizeof event;
    event.abi_version = MIGO_ABI_VERSION_CURRENT;
    event.event_type = type;
    event.data_utf8 = data;
    event.data_length = data ? (uint32_t)strlen(data) : 0;

    MigoResult result = migo_session_send_composition_event(session, &event);
    if (result != MIGO_OK) {
        fprintf(stderr, "[c-host] composition not delivered: %d\n", (int)result);
    }
}

static void feed_scripted_keyboard(MigoSession *session) {
    static const char *const typed[] = {"m", "mi", "mig", "migo"};

    send_keyboard(session, MIGO_KEYBOARD_EVENT_HEIGHT_CHANGE, NULL,
                  260.0f / SCALE_FACTOR);
    for (size_t i = 0; i < sizeof typed / sizeof typed[0]; ++i) {
        send_keyboard(session, MIGO_KEYBOARD_EVENT_INPUT, typed[i], 0.0);
    }
    /* What an IME does before anything is committed: a preedit that grows, then
     * resolves. The committed text arrives as a keyboard input value, which is
     * why the two are sent together rather than one instead of the other. */
    send_composition(session, MIGO_COMPOSITION_EVENT_START, "");
    send_composition(session, MIGO_COMPOSITION_EVENT_UPDATE, "ni");
    send_composition(session, MIGO_COMPOSITION_EVENT_UPDATE, "nihao");
    /* Multi-byte on purpose: preedit text is the whole reason composition
     * exists, and a boundary that mangles it would look fine for ASCII. */
    send_composition(session, MIGO_COMPOSITION_EVENT_END, "\u4f60\u597d");
    send_keyboard(session, MIGO_KEYBOARD_EVENT_INPUT, "migo\u4f60\u597d", 0.0);

    send_keyboard(session, MIGO_KEYBOARD_EVENT_CONFIRM, "migo", 0.0);
    /* Complete is the keyboard being dismissed. It must arrive even though
     * content asked for the hide itself: content that never sees it goes on
     * believing the keyboard is still up, and nothing later corrects that. */
    send_keyboard(session, MIGO_KEYBOARD_EVENT_COMPLETE, "migo", 0.0);
    /* The keyboard going away is also a height change back to zero, which is
     * how content learns to lay itself back out. */
    send_keyboard(session, MIGO_KEYBOARD_EVENT_HEIGHT_CHANGE, NULL, 0.0);
}

/* Shared with wayland_host.c so both hosts report the engine identically and
 * neither grows its own copy that can drift. */
MigoResult MIGO_CALL wl_dispatch_inline(void *dispatcher_context, MigoTaskFn task,
                                        void *task_context) {
    return dispatch_inline(dispatcher_context, task, task_context);
}

void MIGO_CALL wl_on_ready(void *user_data, MigoSession *session) {
    on_ready(user_data, session);
}

void MIGO_CALL wl_on_error(void *user_data, MigoSession *session, const MigoError *error) {
    on_error(user_data, session, error);
}

int run_wayland_host(const char *files_dir, const char *content_id, int seconds);

int main(int argc, char **argv) {
    const char *files_dir = (argc > 1) ? argv[1] : "/tmp/migo-c-host/files";
    const char *content_id = (argc > 2) ? argv[2] : "c-host-demo";
    const int seconds = (argc > 3) ? atoi(argv[3]) : 10;

    /* Which window system, chosen by the host rather than guessed: a machine
     * running both (WSLg does) would otherwise get whichever this file probed
     * first, and the point of the example is that the host decides. */
    const char *backend = getenv("MIGO_C_HOST_BACKEND");
    if (backend != NULL && strcmp(backend, "wayland") == 0) {
#ifdef MIGO_C_HOST_NO_WAYLAND
        fprintf(stderr, "[c-host] this build has no Wayland host "
                        "(wayland-protocols was missing at build time)\n");
        return 1;
#else
        return run_wayland_host(files_dir, content_id, seconds);
#endif
    }

    /* ---- Host-owned window. Migo never creates one. ---- */
    /* Migo borrows this connection only during attach, then renders through
     * its own X11 connection. This event loop remains single-threaded. */
    Display *display = XOpenDisplay(NULL);
    if (!display) {
        fprintf(stderr, "[c-host] XOpenDisplay failed (DISPLAY=%s)\n",
                getenv("DISPLAY") ? getenv("DISPLAY") : "(unset)");
        return 1;
    }
    int screen = DefaultScreen(display);
    Window window = XCreateSimpleWindow(display, RootWindow(display, screen), 0, 0,
                                        WINDOW_WIDTH, WINDOW_HEIGHT, 0,
                                        BlackPixel(display, screen),
                                        BlackPixel(display, screen));
    XStoreName(display, window, "migo-c-host");
    XSelectInput(display, window,
                 StructureNotifyMask | ButtonPressMask | ButtonReleaseMask |
                     PointerMotionMask | LeaveWindowMask);
    Atom wm_delete = XInternAtom(display, "WM_DELETE_WINDOW", False);
    XSetWMProtocols(display, window, &wm_delete, 1);
    XMapWindow(display, window);
    XFlush(display);

    /* ---- Engine: the host names every storage root. ---- */
    char cache_dir[512];
    char code_cache_dir[512];
    snprintf(cache_dir, sizeof(cache_dir), "%s/../cache", files_dir);
    snprintf(code_cache_dir, sizeof(code_cache_dir), "%s/../code-cache", files_dir);

    MigoEngineConfig engine_config;
    memset(&engine_config, 0, sizeof(engine_config));
    engine_config.struct_size = (uint32_t)sizeof(engine_config);
    engine_config.abi_version = MIGO_ABI_VERSION_CURRENT;
    /* Development example: the bundled game carries no signing receipt. */
    engine_config.flags = MIGO_ENGINE_FLAG_ALLOW_UNSIGNED_CONTENT;
    engine_config.files_dir_utf8 = files_dir;
    engine_config.cache_dir_utf8 = cache_dir;
    engine_config.code_cache_dir_utf8 = code_cache_dir;

    /* Ask the library what it supports before building anything on top of it.
     * The MIGO_C_ABI_* macros describe the headers this file compiled against;
     * only this call describes the library that got linked. Checking the
     * surface kind here means an unsupported build is a clear message now
     * rather than a failed attach after an engine, a session and a window. */
    MigoCapabilities caps;
    memset(&caps, 0, sizeof caps);
    caps.struct_size = (uint32_t)sizeof caps;
    caps.abi_version = MIGO_ABI_VERSION_CURRENT;
    MigoResult result = migo_query_capabilities(&caps);
    if (result != MIGO_OK) return fail("migo_query_capabilities", result);
    fprintf(stderr, "migo: abi %u..%u, platform kinds 0x%llx\n",
            caps.abi_version_min, caps.abi_version_max,
            (unsigned long long)caps.platform_kinds);
    if ((caps.platform_kinds & (UINT64_C(1) << MIGO_PLATFORM_X11_WINDOW)) == 0) {
        fprintf(stderr, "migo: this build cannot attach an X11 window\n");
        return 1;
    }

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

    /* ---- Callbacks install once, before the first attach. ---- */
    MigoHostCallbacks host_callbacks;
    memset(&host_callbacks, 0, sizeof(host_callbacks));
    host_callbacks.struct_size = (uint32_t)sizeof(host_callbacks);
    host_callbacks.abi_version = MIGO_ABI_VERSION_CURRENT;
    host_callbacks.dispatch = dispatch_inline;
    host_callbacks.on_ready = on_ready;
    host_callbacks.on_error = on_error;
    host_callbacks.on_exit_requested = on_exit_requested;
    /* All three or none: a subset is refused with MIGO_ERROR_INVALID_ARGUMENT. */
    host_callbacks.on_show_keyboard = on_show_keyboard;
    host_callbacks.on_hide_keyboard = on_hide_keyboard;
    host_callbacks.on_update_keyboard = on_update_keyboard;
    /* The device's own services -- permission, location, the motion sensors --
     * answered from this loop (services.h), the device scripted by a feed. */
    services_init();
    host_callbacks.on_host_service_call = services_on_call;
    host_callbacks.host_services = C_HOST_SERVICES;

    result = migo_session_set_host_callbacks(session, &host_callbacks);
    if (result != MIGO_OK) return fail("migo_session_set_host_callbacks", result);

    /* ---- Say who the host is: content reads it as getAppBaseInfo().host.appId. A
     *      Linux app has no identifier the system hands out, so the host names
     *      itself, as its desktop file or D-Bus name does. Everything else is
     *      left to the engine: the system's language, no benchmark score. ---- */
    MigoHostAppInfo app_info;
    memset(&app_info, 0, sizeof(app_info));
    app_info.struct_size = (uint32_t)sizeof(app_info);
    app_info.abi_version = MIGO_ABI_VERSION_CURRENT;
    app_info.app_id_utf8 = "com.migo.example.linux";
    app_info.benchmark_level = -1;
    result = migo_session_set_host_app_info(session, &app_info);
    if (result != MIGO_OK) return fail("migo_session_set_host_app_info", result);

    /* ---- Hand the window over as a strongly typed platform descriptor. ---- */
    MigoX11WindowDescriptor x11;
    memset(&x11, 0, sizeof(x11));
    x11.struct_size = (uint32_t)sizeof(x11);
    x11.abi_version = MIGO_ABI_VERSION_CURRENT;
    x11.platform_kind = MIGO_PLATFORM_X11_WINDOW;
    x11.flags = MIGO_PLATFORM_DESCRIPTOR_FLAG_NONE;
    x11.display = display;
    x11.window = (uintptr_t)window;
    x11.screen = screen;

    MigoSurfaceDescriptor surface;
    memset(&surface, 0, sizeof(surface));
    surface.struct_size = (uint32_t)sizeof(surface);
    surface.abi_version = MIGO_ABI_VERSION_CURRENT;
    surface.generation = 1;
    surface.platform_kind = MIGO_PLATFORM_X11_WINDOW;
    surface.flags = MIGO_SURFACE_DESCRIPTOR_FLAG_NONE;
    surface.width_pixels = WINDOW_WIDTH;
    surface.height_pixels = WINDOW_HEIGHT;
    surface.scale_factor = SCALE_FACTOR;
    surface.color_space = MIGO_COLOR_SPACE_SRGB;
    surface.alpha_mode = MIGO_ALPHA_MODE_OPAQUE;
    surface.preferred_presentation_mode = MIGO_PRESENTATION_MODE_DEFAULT;
    surface.capability_flags = MIGO_SURFACE_CAPABILITY_NONE;
    surface.platform_descriptor_size = (uint32_t)sizeof(x11);
    surface.platform_descriptor = &x11;

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

    printf("[c-host] running '%s' for %ds in window 0x%lx\n", content_id, seconds,
           (unsigned long)window);
    fflush(stdout);

    /* ---- The host owns the event loop; Migo renders on its own thread. ---- */
    int pressed = 0;
    /* The generation the attach above used; this window is never destroyed and
     * recreated by this example, so every resize keeps it -- see send_resize. */
    const uint64_t surface_generation = surface.generation;
    int last_width = WINDOW_WIDTH;
    int last_height = WINDOW_HEIGHT;
    for (int elapsed = 0; elapsed < seconds * 1000; elapsed += 16) {
        while (XPending(display) > 0) {
            XEvent event;
            XNextEvent(display, &event);
            if (event.type == ClientMessage &&
                (Atom)event.xclient.data.l[0] == wm_delete) {
                printf("[c-host] window close requested\n");
                elapsed = seconds * 1000;
                break;
            }
            switch (event.type) {
            case ButtonPress:
                if (event.xbutton.button == Button1) {
                    pressed = 1;
                    send_touch(session, MIGO_TOUCH_START, event.xbutton.x,
                               event.xbutton.y, (int64_t)event.xbutton.time);
                }
                break;
            case MotionNotify:
                /* Only while a button is down: mini-game content has no hover concept,
                 * so free motion would be a stream of events no game reads. */
                if (pressed) {
                    send_touch(session, MIGO_TOUCH_MOVE, event.xmotion.x,
                               event.xmotion.y, (int64_t)event.xmotion.time);
                }
                break;
            case ButtonRelease:
                if (event.xbutton.button == Button1 && pressed) {
                    pressed = 0;
                    send_touch(session, MIGO_TOUCH_END, event.xbutton.x,
                               event.xbutton.y, (int64_t)event.xbutton.time);
                }
                break;
            case ConfigureNotify:
                /* Also fires on a plain move with the size unchanged; only a
                 * real size change is a resize the engine needs to hear about. */
                if (event.xconfigure.width != last_width ||
                    event.xconfigure.height != last_height) {
                    last_width = event.xconfigure.width;
                    last_height = event.xconfigure.height;
                    send_resize(attachment, surface_generation,
                               (uint32_t)last_width, (uint32_t)last_height);
                }
                break;
            case LeaveNotify:
                /* The pointer left with a button still down, so the release will
                 * never arrive: cancel rather than strand the touch. */
                if (pressed) {
                    pressed = 0;
                    send_touch(session, MIGO_TOUCH_CANCEL, event.xcrossing.x,
                               event.xcrossing.y, (int64_t)event.xcrossing.time);
                }
                break;
            default:
                break;
            }
        }
        /* Answer a keyboard request from our own loop, not from inside the
         * callback: the callback runs on the engine's thread, and a host that
         * fed events from there would be doing its work on a thread it never
         * agreed to own. */
        if (atomic_exchange(&g_keyboard_requested, 0)) {
            feed_scripted_keyboard(session);
        }

        /* The scripted pad: connect at 1s, sample until 6s, withdraw. */
        if (elapsed == 1008) {
            MigoGamepadInfo info;
            gamepad_info(&info);
            MigoResult r = migo_session_set_gamepad_connected(session, &info, 1);
            printf("[c-host] gamepad connect: %d\n", (int)r);
            fflush(stdout);
        } else if (elapsed > 1008 && elapsed < 6000) {
            send_gamepad_sample(session, (double)((elapsed / 16) % 100) / 100.0, elapsed);
        } else if (elapsed == 6000) {
            MigoGamepadInfo info;
            gamepad_info(&info);
            MigoResult r = migo_session_set_gamepad_connected(session, &info, 0);
            printf("[c-host] gamepad disconnect: %d\n", (int)r);
            fflush(stdout);
        }
        services_pump(session);
        sleep_ms(16);
    }

    /* ---- Teardown in the order the headers require. ---- */
    if (detach_and_await_release(attachment) != 0) return 1;
    result = migo_session_destroy(session);
    if (result != MIGO_OK) return fail("migo_session_destroy", result);
    services_shutdown();
    result = migo_engine_destroy(engine);
    if (result != MIGO_OK) return fail("migo_engine_destroy", result);

    XDestroyWindow(display, window);
    XCloseDisplay(display);
    printf("[c-host] done\n");
    return 0;
}

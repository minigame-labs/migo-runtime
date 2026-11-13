/*
 * Minimal third-party Windows host: opens a Win32 window and runs a Migo game
 * through the public C ABI only.
 *
 * The Windows counterpart of tests/c_host/linux: it includes nothing but the
 * public migo headers and the system's, and it is what migo-conformance's
 * Windows lane runs (runners/run-windows-capi.sh). A shipped Windows host is a
 * C ABI host like this one -- the engine's own services and what the host
 * reports and installs -- so this host installs what the minimal example
 * installs and reports nothing else.
 *
 *   migo-c-host FILES_DIR CONTENT_ID SECONDS
 *
 * The game is the installed content CONTENT_ID under FILES_DIR
 * (FILES_DIR\migo\games\CONTENT_ID\code\game.js), run for SECONDS or until it
 * asks to exit. Its settings come from files the environment names:
 * MIGO_C_HOST_NETWORK_POLICY, the network (none at all without it), and
 * MIGO_C_HOST_LAUNCH_OPTIONS, what the game is launched with. Content's console
 * reaches stderr through the C ABI's development log (MIGO_CAPI_LOG).
 */

#define WIN32_LEAN_AND_MEAN
#define NOMINMAX
#ifndef UNICODE
#define UNICODE
#endif
#ifndef _UNICODE
#define _UNICODE
#endif

#include <windows.h>

#include <migo/migo.h>
#include <migo/platform/win32.h>

#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <wchar.h>

/* The surface's size in pixels: a phone's portrait screen, as the Linux C
 * host's, so a bundle sees the same window on either lane. */
#define WINDOW_WIDTH 720
#define WINDOW_HEIGHT 1280

/* A driver holding the window past detach is normal; holding it this long is
 * not, and blocking forever would turn a stall into a hang with no diagnosis. */
#define RELEASE_TIMEOUT_MS 2000
#define RELEASE_POLL_MS 4
/* How long the loop waits for a message between its turns. */
#define TURN_MS 16

/* Written by the engine's thread (on_exit_requested, the keyboard), read by
 * the loop on the window's. */
static volatile LONG g_exit_requested;
static volatile LONG g_keyboard_requested;

/* What the window procedure needs of the session, set once both exist. */
static MigoSession *g_session;
static MigoSurfaceAttachment *g_attachment;
static uint64_t g_generation;
static HWND g_view;
static int g_pressed;

static int fail(const char *what, MigoResult result) {
    fprintf(stderr, "[c-host] %s failed: %d\n", what, (int)result);
    return 1;
}

/* ---- Host callbacks. The engine never calls these directly: it hands a task
 * to our dispatcher, and we run it where we choose -- here, inline, which is
 * legal: "exactly once, inline or later". ---- */
static MigoResult MIGO_CALL dispatch_inline(void *dispatcher_context, MigoTaskFn task, void *task_context) {
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

static void MIGO_CALL on_error(void *user_data, MigoSession *session, const MigoError *error) {
    (void)user_data;
    (void)session;
    fprintf(stderr, "[c-host] callback: engine error %d: %.*s\n", (int)error->code, (int)error->message_length,
            error->message_utf8);
    fflush(stderr);
}

/* Content asked to exit (exitMiniProgram), and a host closes the game: the loop
 * ends at its next turn and tears the session down. The line is the one every
 * conformance host prints for it. */
static void MIGO_CALL on_exit_requested(void *user_data, MigoSession *session) {
    (void)user_data;
    (void)session;
    printf("[host] exit requested\n");
    fflush(stdout);
    InterlockedExchange(&g_exit_requested, 1);
}

/*
 * ---- The soft keyboard, which is a capability this host supplies. ----
 *
 * As the Linux C host does: the host answers the request itself and feeds the
 * resulting events back, playing a fixed script rather than opening a real
 * IME, so the round trip is proven with exact values. All three or none.
 */
static void MIGO_CALL on_show_keyboard(void *user_data, MigoSession *session,
                                       const MigoKeyboardShowOptions *options) {
    (void)user_data;
    (void)session;
    printf("[c-host] show keyboard: max_length=%u type=%u confirm=%u flags=0x%x default='%.*s'\n", options->max_length,
           options->keyboard_type, options->confirm_type, options->flags, (int)options->default_value_length,
           options->default_value_utf8);
    fflush(stdout);
    InterlockedExchange(&g_keyboard_requested, 1);
}

static void MIGO_CALL on_hide_keyboard(void *user_data, MigoSession *session) {
    (void)user_data;
    (void)session;
    printf("[c-host] hide keyboard\n");
    fflush(stdout);
}

static void MIGO_CALL on_update_keyboard(void *user_data, MigoSession *session, const char *value_utf8,
                                         uint32_t value_length) {
    (void)user_data;
    (void)session;
    /* Length-delimited and borrowed for this call only. */
    printf("[c-host] update keyboard: '%.*s'\n", (int)value_length, value_utf8);
    fflush(stdout);
}

static void send_keyboard(MigoSession *session, MigoKeyboardEventType type, const char *value, double height_css_px) {
    MigoKeyboardEvent event;
    memset(&event, 0, sizeof event);
    event.struct_size = (uint32_t)sizeof event;
    event.abi_version = MIGO_ABI_VERSION_CURRENT;
    event.event_type = type;
    event.value_utf8 = value;
    event.value_length = value ? (uint32_t)strlen(value) : 0;
    event.height_css_px = height_css_px;
    MigoResult result = migo_session_send_keyboard_event(session, &event);
    if (result != MIGO_OK) fprintf(stderr, "[c-host] keyboard event not delivered: %d\n", (int)result);
}

static void send_composition(MigoSession *session, MigoCompositionEventType type, const char *data) {
    MigoCompositionEvent event;
    memset(&event, 0, sizeof event);
    event.struct_size = (uint32_t)sizeof event;
    event.abi_version = MIGO_ABI_VERSION_CURRENT;
    event.event_type = type;
    event.data_utf8 = data;
    event.data_length = data ? (uint32_t)strlen(data) : 0;
    MigoResult result = migo_session_send_composition_event(session, &event);
    if (result != MIGO_OK) fprintf(stderr, "[c-host] composition not delivered: %d\n", (int)result);
}

/* What a real IME would produce once content asked for a keyboard -- the
 * Linux C host's script, value for value. */
static void feed_scripted_keyboard(MigoSession *session) {
    static const char *const typed[] = {"m", "mi", "mig", "migo"};
    send_keyboard(session, MIGO_KEYBOARD_EVENT_HEIGHT_CHANGE, NULL, 260.0);
    for (size_t i = 0; i < sizeof typed / sizeof typed[0]; ++i) {
        send_keyboard(session, MIGO_KEYBOARD_EVENT_INPUT, typed[i], 0.0);
    }
    send_composition(session, MIGO_COMPOSITION_EVENT_START, "");
    send_composition(session, MIGO_COMPOSITION_EVENT_UPDATE, "ni");
    send_composition(session, MIGO_COMPOSITION_EVENT_UPDATE, "nihao");
    send_composition(session, MIGO_COMPOSITION_EVENT_END, "\xe4\xbd\xa0\xe5\xa5\xbd");
    send_keyboard(session, MIGO_KEYBOARD_EVENT_INPUT, "migo\xe4\xbd\xa0\xe5\xa5\xbd", 0.0);
    send_keyboard(session, MIGO_KEYBOARD_EVENT_CONFIRM, "migo", 0.0);
    send_keyboard(session, MIGO_KEYBOARD_EVENT_COMPLETE, "migo", 0.0);
    send_keyboard(session, MIGO_KEYBOARD_EVENT_HEIGHT_CHANGE, NULL, 0.0);
}

/* ---- Input: a mouse press is a one-point touch, what mini-game content
 * listens for. The window's pixels are CSS pixels at a scale factor of 1. ---- */
static void send_touch(MigoTouchType type, int x, int y) {
    if (g_session == NULL) return;
    const int removed = (type == MIGO_TOUCH_END || type == MIGO_TOUCH_CANCEL);
    MigoTouchPoint point;
    memset(&point, 0, sizeof point);
    point.id = 0;
    point.x = (float)x;
    point.y = (float)y;
    point.pressure = removed ? 0.0f : 1.0f;
    point.flags = MIGO_TOUCH_FLAG_CHANGED;
    if (removed) point.flags |= MIGO_TOUCH_FLAG_REMOVED;
    MigoTouchEvent event;
    memset(&event, 0, sizeof event);
    event.struct_size = (uint32_t)sizeof event;
    event.abi_version = MIGO_ABI_VERSION_CURRENT;
    event.type = type;
    event.point_count = 1;
    event.timestamp_ms = (int64_t)GetTickCount64();
    event.points = &point;
    MigoResult result = migo_session_send_touch(g_session, &event);
    if (result != MIGO_OK) fprintf(stderr, "[c-host] touch not delivered: %d\n", (int)result);
}

/* Same window, new size: the surface's metrics change, its generation does not. */
static void send_resize(uint32_t width, uint32_t height) {
    if (g_attachment == NULL || width == 0 || height == 0) return;
    MigoSurfaceMetrics metrics;
    memset(&metrics, 0, sizeof metrics);
    metrics.struct_size = (uint32_t)sizeof metrics;
    metrics.abi_version = MIGO_ABI_VERSION_CURRENT;
    metrics.generation = g_generation;
    metrics.width_pixels = width;
    metrics.height_pixels = height;
    metrics.scale_factor = 1.0f;
    metrics.color_space = MIGO_COLOR_SPACE_SRGB;
    metrics.alpha_mode = MIGO_ALPHA_MODE_OPAQUE;
    metrics.preferred_presentation_mode = MIGO_PRESENTATION_MODE_DEFAULT;
    metrics.flags = MIGO_SURFACE_DESCRIPTOR_FLAG_NONE;
    MigoResult result = migo_surface_update(g_attachment, &metrics);
    if (result != MIGO_OK) fprintf(stderr, "[c-host] resize not delivered: %d\n", (int)result);
}

/* The game's view: a child window the engine draws in, which takes the mouse. */
static LRESULT CALLBACK view_procedure(HWND window, UINT message, WPARAM wparam, LPARAM lparam) {
    const int x = (int)(short)LOWORD(lparam);
    const int y = (int)(short)HIWORD(lparam);
    switch (message) {
        case WM_LBUTTONDOWN:
            g_pressed = 1;
            SetCapture(window);
            send_touch(MIGO_TOUCH_START, x, y);
            return 0;
        case WM_MOUSEMOVE:
            /* Only while the button is down: content has no hover. */
            if (g_pressed) send_touch(MIGO_TOUCH_MOVE, x, y);
            return 0;
        case WM_LBUTTONUP:
            if (g_pressed) {
                g_pressed = 0;
                ReleaseCapture();
                send_touch(MIGO_TOUCH_END, x, y);
            }
            return 0;
        case WM_CAPTURECHANGED:
            /* The press went elsewhere: cancel rather than strand the touch. */
            if (g_pressed) {
                g_pressed = 0;
                send_touch(MIGO_TOUCH_CANCEL, 0, 0);
            }
            return 0;
        case WM_SIZE:
            send_resize(LOWORD(lparam), HIWORD(lparam));
            return 0;
        case WM_ERASEBKGND:
            /* The engine draws every pixel. */
            return 1;
        default:
            return DefWindowProcW(window, message, wparam, lparam);
    }
}

/* The frame: holds the view, which fills it, and lets the window be larger
 * than the screen it is on -- a runner's desktop is smaller than a phone. */
static LRESULT CALLBACK frame_procedure(HWND window, UINT message, WPARAM wparam, LPARAM lparam) {
    switch (message) {
        case WM_GETMINMAXINFO: {
            MINMAXINFO *limits = (MINMAXINFO *)lparam;
            limits->ptMaxTrackSize.x = 16384;
            limits->ptMaxTrackSize.y = 16384;
            return 0;
        }
        case WM_SIZE:
            if (g_view != NULL) MoveWindow(g_view, 0, 0, LOWORD(lparam), HIWORD(lparam), TRUE);
            return 0;
        case WM_CLOSE:
            printf("[c-host] window close requested\n");
            fflush(stdout);
            InterlockedExchange(&g_exit_requested, 1);
            return 0;
        default:
            return DefWindowProcW(window, message, wparam, lparam);
    }
}

/* `wide` as UTF-8, which every string the ABI takes is; NULL when it cannot be. */
static char *utf8_of(const wchar_t *wide) {
    const int size = WideCharToMultiByte(CP_UTF8, WC_ERR_INVALID_CHARS, wide, -1, NULL, 0, NULL, NULL);
    if (size <= 0) return NULL;
    char *utf8 = (char *)malloc((size_t)size);
    if (utf8 == NULL) return NULL;
    if (WideCharToMultiByte(CP_UTF8, WC_ERR_INVALID_CHARS, wide, -1, utf8, size, NULL, NULL) != size) {
        free(utf8);
        return NULL;
    }
    return utf8;
}

/* A setting's JSON, read whole from the file the environment variable `name`
 * names, or NULL when it names none. A file that cannot be read stops the
 * host: running without the setting would fail every check that needs it for a
 * reason no check names. */
static char *read_setting(const wchar_t *name) {
    wchar_t path[MAX_PATH * 4];
    const DWORD length = GetEnvironmentVariableW(name, path, (DWORD)(sizeof path / sizeof path[0]));
    if (length == 0 || length >= sizeof path / sizeof path[0]) return NULL;
    HANDLE file = CreateFileW(path, GENERIC_READ, FILE_SHARE_READ, NULL, OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, NULL);
    LARGE_INTEGER size;
    char *json = NULL;
    DWORD read = 0;
    if (file != INVALID_HANDLE_VALUE && GetFileSizeEx(file, &size) && size.QuadPart < 64 * 1024 * 1024) {
        json = (char *)malloc((size_t)size.QuadPart + 1);
        if (json != NULL && (!ReadFile(file, json, (DWORD)size.QuadPart, &read, NULL) || read != (DWORD)size.QuadPart)) {
            free(json);
            json = NULL;
        }
    }
    if (file != INVALID_HANDLE_VALUE) CloseHandle(file);
    if (json == NULL) {
        fwprintf(stderr, L"[c-host] cannot read %ls=%ls\n", name, path);
        exit(2);
    }
    json[read] = '\0';
    return json;
}

/*
 * Retire the attachment, then wait until the window is genuinely unused:
 * MIGO_OK from begin_detach means retirement started, not that the driver is
 * done with the window, and destroying it then would be a use-after-free inside
 * the driver. Polled on the window's own thread, which owns it.
 */
static int detach_and_await_release(MigoSurfaceAttachment *attachment) {
    MigoSurfaceRelease *release = NULL;
    MigoResult result = migo_surface_begin_detach(attachment, &release);
    if (result != MIGO_OK) return fail("migo_surface_begin_detach", result);
    for (long waited = 0; waited < RELEASE_TIMEOUT_MS; waited += RELEASE_POLL_MS) {
        MigoSurfaceReleaseStatus status;
        memset(&status, 0, sizeof status);
        status.struct_size = (uint32_t)sizeof status;
        status.abi_version = MIGO_ABI_VERSION_CURRENT;
        result = migo_surface_release_query(release, &status);
        if (result != MIGO_OK) return fail("migo_surface_release_query", result);
        if (status.state == MIGO_SURFACE_RELEASE_RELEASED) {
            result = migo_surface_release_destroy(release);
            if (result != MIGO_OK) return fail("migo_surface_release_destroy", result);
            return 0;
        }
        Sleep(RELEASE_POLL_MS);
    }
    /* Deliberately leaks the observer and the window: the only thing that could
     * still say when the window is safe to touch, and the window the driver may
     * still be reading. */
    return fail("surface release timed out", MIGO_ERROR_INTERNAL);
}

int wmain(int argc, wchar_t **argv) {
    char *files_dir = utf8_of(argc > 1 ? argv[1] : L"C:\\migo-c-host\\files");
    char *content_id = utf8_of(argc > 2 ? argv[2] : L"c-host-demo");
    const int seconds = (argc > 3) ? _wtoi(argv[3]) : 10;
    if (files_dir == NULL || content_id == NULL) {
        fprintf(stderr, "[c-host] the arguments are not valid UTF-16\n");
        return 2;
    }
    /* Physical pixels: the window is as large as it says, whatever the scale. */
    SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);

    /* ---- Host-owned windows. Migo never creates one. ---- */
    HINSTANCE instance = GetModuleHandleW(NULL);
    WNDCLASSEXW frame_class;
    memset(&frame_class, 0, sizeof frame_class);
    frame_class.cbSize = sizeof frame_class;
    frame_class.lpfnWndProc = frame_procedure;
    frame_class.hInstance = instance;
    frame_class.hCursor = LoadCursorW(NULL, IDC_ARROW);
    frame_class.lpszClassName = L"MigoCHostFrame";
    WNDCLASSEXW view_class = frame_class;
    view_class.lpfnWndProc = view_procedure;
    view_class.style = CS_OWNDC;
    view_class.lpszClassName = L"MigoCHostView";
    if (RegisterClassExW(&frame_class) == 0 || RegisterClassExW(&view_class) == 0) {
        fprintf(stderr, "[c-host] RegisterClassExW failed: %lu\n", GetLastError());
        return 1;
    }
    const DWORD frame_style = WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN;
    RECT outer = {0, 0, WINDOW_WIDTH, WINDOW_HEIGHT};
    AdjustWindowRectEx(&outer, frame_style, FALSE, 0);
    HWND frame = CreateWindowExW(0, frame_class.lpszClassName, L"migo-c-host", frame_style, 0, 0,
                                 outer.right - outer.left, outer.bottom - outer.top, NULL, NULL, instance, NULL);
    if (frame == NULL) {
        fprintf(stderr, "[c-host] CreateWindowExW (frame) failed: %lu\n", GetLastError());
        return 1;
    }
    g_view = CreateWindowExW(0, view_class.lpszClassName, L"", WS_CHILD | WS_VISIBLE | WS_CLIPSIBLINGS, 0, 0,
                             WINDOW_WIDTH, WINDOW_HEIGHT, frame, NULL, instance, NULL);
    if (g_view == NULL) {
        fprintf(stderr, "[c-host] CreateWindowExW (view) failed: %lu\n", GetLastError());
        return 1;
    }
    ShowWindow(frame, SW_SHOWNORMAL);
    UpdateWindow(frame);
    RECT client;
    GetClientRect(g_view, &client);

    /* ---- Engine: the host names every storage root. ---- */
    char cache_dir[1024];
    char code_cache_dir[1024];
    snprintf(cache_dir, sizeof cache_dir, "%s\\..\\cache", files_dir);
    snprintf(code_cache_dir, sizeof code_cache_dir, "%s\\..\\code-cache", files_dir);

    MigoCapabilities caps;
    memset(&caps, 0, sizeof caps);
    caps.struct_size = (uint32_t)sizeof caps;
    caps.abi_version = MIGO_ABI_VERSION_CURRENT;
    MigoResult result = migo_query_capabilities(&caps);
    if (result != MIGO_OK) return fail("migo_query_capabilities", result);
    fprintf(stderr, "migo: abi %u..%u, platform kinds 0x%llx\n", caps.abi_version_min, caps.abi_version_max,
            (unsigned long long)caps.platform_kinds);
    if ((caps.platform_kinds & (UINT64_C(1) << MIGO_PLATFORM_WIN32_HWND)) == 0) {
        fprintf(stderr, "migo: this build cannot attach a Win32 window\n");
        return 1;
    }

    MigoEngineConfig engine_config;
    memset(&engine_config, 0, sizeof engine_config);
    engine_config.struct_size = (uint32_t)sizeof engine_config;
    engine_config.abi_version = MIGO_ABI_VERSION_CURRENT;
    /* Development host: the deployed game carries no signing receipt. */
    engine_config.flags = MIGO_ENGINE_FLAG_ALLOW_UNSIGNED_CONTENT;
    engine_config.files_dir_utf8 = files_dir;
    engine_config.cache_dir_utf8 = cache_dir;
    engine_config.code_cache_dir_utf8 = code_cache_dir;
    MigoEngine *engine = NULL;
    result = migo_engine_create(&engine_config, &engine);
    if (result != MIGO_OK) return fail("migo_engine_create", result);

    MigoSessionConfig session_config;
    memset(&session_config, 0, sizeof session_config);
    session_config.struct_size = (uint32_t)sizeof session_config;
    session_config.abi_version = MIGO_ABI_VERSION_CURRENT;
    session_config.flags = MIGO_SESSION_FLAG_NONE;
    /* Borrowed for the create only. */
    char *network_policy = read_setting(L"MIGO_C_HOST_NETWORK_POLICY");
    char *launch_options = read_setting(L"MIGO_C_HOST_LAUNCH_OPTIONS");
    session_config.network_policy_json_utf8 = network_policy;
    session_config.launch_options_json_utf8 = launch_options;
    MigoSession *session = NULL;
    result = migo_session_create(engine, &session_config, &session);
    free(network_policy);
    free(launch_options);
    if (result != MIGO_OK) return fail("migo_session_create", result);

    /* ---- Callbacks install once, before the first attach. ---- */
    MigoHostCallbacks host_callbacks;
    memset(&host_callbacks, 0, sizeof host_callbacks);
    host_callbacks.struct_size = (uint32_t)sizeof host_callbacks;
    host_callbacks.abi_version = MIGO_ABI_VERSION_CURRENT;
    host_callbacks.dispatch = dispatch_inline;
    host_callbacks.on_ready = on_ready;
    host_callbacks.on_error = on_error;
    host_callbacks.on_exit_requested = on_exit_requested;
    host_callbacks.on_show_keyboard = on_show_keyboard;
    host_callbacks.on_hide_keyboard = on_hide_keyboard;
    host_callbacks.on_update_keyboard = on_update_keyboard;
    result = migo_session_set_host_callbacks(session, &host_callbacks);
    if (result != MIGO_OK) return fail("migo_session_set_host_callbacks", result);

    /* ---- Who the host is: content reads it as getAppBaseInfo().host.appId. ---- */
    MigoHostAppInfo app_info;
    memset(&app_info, 0, sizeof app_info);
    app_info.struct_size = (uint32_t)sizeof app_info;
    app_info.abi_version = MIGO_ABI_VERSION_CURRENT;
    app_info.app_id_utf8 = "com.migo.example.windows";
    app_info.benchmark_level = -1;
    result = migo_session_set_host_app_info(session, &app_info);
    if (result != MIGO_OK) return fail("migo_session_set_host_app_info", result);

    /* ---- Hand the view over as a strongly typed platform descriptor. ---- */
    MigoWin32HwndDescriptor win32;
    memset(&win32, 0, sizeof win32);
    win32.struct_size = (uint32_t)sizeof win32;
    win32.abi_version = MIGO_ABI_VERSION_CURRENT;
    win32.platform_kind = MIGO_PLATFORM_WIN32_HWND;
    win32.flags = MIGO_PLATFORM_DESCRIPTOR_FLAG_NONE;
    win32.hwnd = g_view;

    MigoSurfaceDescriptor surface;
    memset(&surface, 0, sizeof surface);
    surface.struct_size = (uint32_t)sizeof surface;
    surface.abi_version = MIGO_ABI_VERSION_CURRENT;
    surface.generation = 1;
    surface.platform_kind = MIGO_PLATFORM_WIN32_HWND;
    surface.flags = MIGO_SURFACE_DESCRIPTOR_FLAG_NONE;
    surface.width_pixels = (uint32_t)(client.right - client.left);
    surface.height_pixels = (uint32_t)(client.bottom - client.top);
    surface.scale_factor = 1.0f;
    surface.color_space = MIGO_COLOR_SPACE_SRGB;
    surface.alpha_mode = MIGO_ALPHA_MODE_OPAQUE;
    surface.preferred_presentation_mode = MIGO_PRESENTATION_MODE_DEFAULT;
    surface.capability_flags = MIGO_SURFACE_CAPABILITY_NONE;
    surface.platform_descriptor_size = (uint32_t)sizeof win32;
    surface.platform_descriptor = &win32;

    MigoSurfaceAttachment *attachment = NULL;
    result = migo_session_attach_surface(session, &surface, &attachment);
    if (result != MIGO_OK) return fail("migo_session_attach_surface", result);
    g_session = session;
    g_attachment = attachment;
    g_generation = surface.generation;

    MigoContentDescriptor content;
    memset(&content, 0, sizeof content);
    content.struct_size = (uint32_t)sizeof content;
    content.abi_version = MIGO_ABI_VERSION_CURRENT;
    content.flags = MIGO_CONTENT_FLAG_NONE;
    content.content_id_utf8 = content_id;
    content.entry_utf8 = "game.js";
    result = migo_session_load_content(session, &content);
    if (result != MIGO_OK) return fail("migo_session_load_content", result);

    printf("[c-host] running '%s' for %ds\n", content_id, seconds);
    fflush(stdout);

    /* ---- The host owns the message loop; Migo renders on its own thread. ---- */
    const ULONGLONG started = GetTickCount64();
    while (InterlockedCompareExchange(&g_exit_requested, 0, 0) == 0 &&
           GetTickCount64() - started < (ULONGLONG)seconds * 1000) {
        MSG message;
        while (PeekMessageW(&message, NULL, 0, 0, PM_REMOVE)) {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
        /* Answered from this loop, not the callback: the callback runs on the
         * engine's thread, which this host never agreed to work on. */
        if (InterlockedExchange(&g_keyboard_requested, 0) != 0) feed_scripted_keyboard(session);
        MsgWaitForMultipleObjects(0, NULL, FALSE, TURN_MS, QS_ALLINPUT);
    }

    /* ---- Teardown in the order the headers require. ---- */
    g_session = NULL;
    g_attachment = NULL;
    if (detach_and_await_release(attachment) != 0) return 1;
    result = migo_session_destroy(session);
    if (result != MIGO_OK) return fail("migo_session_destroy", result);
    result = migo_engine_destroy(engine);
    if (result != MIGO_OK) return fail("migo_engine_destroy", result);
    DestroyWindow(frame);
    free(files_dir);
    free(content_id);
    printf("[c-host] done\n");
    return 0;
}

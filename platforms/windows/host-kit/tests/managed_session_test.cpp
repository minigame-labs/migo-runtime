// The Managed session against the strict fake of the C ABI: what it creates and
// installs, engine callbacks run on the GUI thread, the content loaded once the
// view attaches, the window's state reported, the App's host services, and the
// teardown's three steps.

#include "fake_migo.hpp"

#include <migo/windows/managed_session.hpp>

#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#ifndef NOMINMAX
#define NOMINMAX
#endif
#ifndef UNICODE
#define UNICODE
#endif
#include <windows.h>

#include <algorithm>
#include <chrono>
#include <cstdio>
#include <optional>
#include <string>
#include <thread>
#include <vector>

namespace {

int g_failures = 0;

#define CHECK(condition)                                                                        \
    do {                                                                                        \
        if (!(condition)) {                                                                     \
            std::fprintf(stderr, "%s:%d: CHECK failed: %s\n", __FILE__, __LINE__, #condition); \
            ++g_failures;                                                                       \
        }                                                                                       \
    } while (false)

using migo::windows_host::hostServiceBit;
using migo::windows_host::HostServiceProvider;
using migo::windows_host::HostServiceReply;
using migo::windows_host::Json;
using migo::windows_host::SurfaceState;
using migo::windows_host::Win32ManagedSession;

template <typename Done>
bool pump_until(Done done, int ms = 3000) {
    const auto deadline = std::chrono::steady_clock::now() + std::chrono::milliseconds(ms);
    while (!done()) {
        MSG message;
        while (PeekMessageW(&message, nullptr, 0, 0, PM_REMOVE)) {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
        if (done()) return true;
        if (std::chrono::steady_clock::now() > deadline) return false;
        MsgWaitForMultipleObjects(0, nullptr, FALSE, 5, QS_ALLINPUT);
    }
    return true;
}

HWND make_frame() {
    static bool registered = false;
    if (!registered) {
        WNDCLASSEXW frame_class{};
        frame_class.cbSize = sizeof frame_class;
        frame_class.lpfnWndProc = DefWindowProcW;
        frame_class.hInstance = GetModuleHandleW(nullptr);
        frame_class.lpszClassName = L"MigoSessionTestFrame";
        RegisterClassExW(&frame_class);
        registered = true;
    }
    HWND frame = CreateWindowExW(0, L"MigoSessionTestFrame", L"session test", WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
                                 0, 0, 900, 700, nullptr, nullptr, GetModuleHandleW(nullptr), nullptr);
    ShowWindow(frame, SW_SHOWNORMAL);
    return frame;
}

std::optional<fake_migo::HostServiceAnswer> answer_to(std::uint64_t call_id) {
    for (const auto &answer : fake_migo::host_service_answers()) {
        if (answer.call_id == call_id) return answer;
    }
    return std::nullopt;
}

bool reported(const char *state) {
    const auto &reports = fake_migo::window_reports();
    return std::find(reports.begin(), reports.end(), state) != reports.end();
}

/// The App's clipboard, answering on the GUI thread.
class Clipboard final : public HostServiceProvider {
public:
    std::thread::id answered_on;
    std::uint64_t services() const noexcept override { return hostServiceBit(MIGO_HOST_SERVICE_CLIPBOARD); }
    void call(MigoHostService, std::uint32_t method, const Json &request, HostServiceReply reply) override {
        answered_on = std::this_thread::get_id();
        if (method == MIGO_CLIPBOARD_SET_CLIPBOARD_DATA) {
            data = request["data"].string();
            (void)reply.succeed();
        } else {
            (void)reply.succeed(Json::object().set("data", data));
        }
    }
    std::string data = "held";
};

/// A session in a frame, its view shown at 640 x 360; closed on the way out.
struct Running {
    HWND frame = make_frame();
    Clipboard clipboard;
    bool ready = false;
    std::thread::id ready_on;
    std::string failure;
    bool exit_asked = false;
    bool closed = false;
    std::optional<Win32ManagedSession> session;

    explicit Running(std::string app_id = {}) {
        Win32ManagedSession::Options options;
        options.appId = std::move(app_id);
        options.appServices = &clipboard;
        Win32ManagedSession::Listener listener;
        listener.contentReady = [this] {
            ready = true;
            ready_on = std::this_thread::get_id();
        };
        listener.contentFailed = [this](MigoResult, const std::string &message) { failure = message; };
        listener.exitRequested = [this] { exit_asked = true; };
        listener.closed = [this] { closed = true; };
        session.emplace(*fake_migo::engine(), frame, std::move(options), std::move(listener));
    }

    void show() {
        MoveWindow(session->view()->hwnd(), 0, 0, 640, 360, TRUE);
        ShowWindow(session->view()->hwnd(), SW_SHOW);
        CHECK(pump_until([this] { return session->view()->surfaceState() == SurfaceState::Attached; }));
    }

    ~Running() {
        if (session.has_value() && !session->isClosed()) {
            (void)session->close();
            if (session->view() != nullptr && session->view()->surfaceState() == SurfaceState::Retiring) {
                fake_migo::set_release_status(session->view()->generation(), MIGO_SURFACE_RELEASE_RELEASED);
            }
            CHECK(pump_until([this] { return closed; }));
        }
        session.reset();
        DestroyWindow(frame);
    }
};

void it_creates_the_session_and_installs_its_callbacks() {
    fake_migo::reset();
    Running running;
    CHECK(running.session->isValid());
    CHECK(fake_migo::calls().session_create == 1);
    CHECK(fake_migo::calls().set_callbacks == 1);
    const MigoHostCallbacks &installed = fake_migo::installed_callbacks();
    CHECK(installed.dispatch != nullptr && installed.on_ready != nullptr && installed.on_request_frame != nullptr);
    CHECK(installed.on_keep_screen_on != nullptr);
    // The App's services, and the Host Kit's own.
    CHECK((installed.host_services & hostServiceBit(MIGO_HOST_SERVICE_CLIPBOARD)) != 0);
    CHECK((installed.host_services & hostServiceBit(MIGO_HOST_SERVICE_WINDOW)) != 0);
    // Not named: the executable's name.
    CHECK(fake_migo::reported_host_app_id() == "migo-windows-managed-session-test");
}

void an_app_id_given_is_the_one_reported() {
    fake_migo::reset();
    Running running("com.example.game");
    CHECK(fake_migo::reported_host_app_id() == "com.example.game");
}

void content_is_loaded_once_the_view_attaches() {
    fake_migo::reset();
    Running running;
    CHECK(running.session->loadContent("game-1") == MIGO_OK);
    CHECK(fake_migo::calls().load_content == 0);
    CHECK(running.session->loadContent("game-2") == MIGO_ERROR_INVALID_STATE);
    running.show();
    CHECK(fake_migo::calls().load_content == 1);
    CHECK(fake_migo::loaded_content_id() == "game-1");
}

void engine_callbacks_run_on_the_gui_thread() {
    fake_migo::reset();
    Running running;
    running.show();
    fake_migo::deliver_ready_from_engine_thread();
    fake_migo::deliver_error_from_engine_thread(MIGO_ERROR_INTERNAL, "boom");
    CHECK(pump_until([&running] { return running.ready && !running.failure.empty(); }));
    CHECK(running.ready_on == std::this_thread::get_id());
    CHECK(running.failure == "boom");
    // A frame asked for is one frame, paced by the view.
    fake_migo::deliver_frame_request_from_engine_thread();
    CHECK(pump_until([] { return !fake_migo::vsyncs().empty(); }));
}

void the_windows_state_is_the_games() {
    fake_migo::reset();
    Running running;
    running.show();
    ShowWindow(running.frame, SW_MINIMIZE);
    CHECK(pump_until([] { return reported("paused") && reported("hidden"); }));
    ShowWindow(running.frame, SW_RESTORE);
    CHECK(pump_until([] { return reported("running") && reported("shown"); }));
}

void the_apps_services_answer_on_the_gui_thread() {
    fake_migo::reset();
    Running running;
    running.show();
    fake_migo::deliver_host_service_call_from_engine_thread(MIGO_HOST_SERVICE_CLIPBOARD,
                                                            MIGO_CLIPBOARD_SET_CLIPBOARD_DATA, 0, "{\"data\":\"copied\"}");
    fake_migo::deliver_host_service_call_from_engine_thread(MIGO_HOST_SERVICE_CLIPBOARD,
                                                            MIGO_CLIPBOARD_GET_CLIPBOARD_DATA, 7, "{}");
    CHECK(pump_until([] { return answer_to(7).has_value(); }));
    CHECK(answer_to(7).has_value() && answer_to(7)->status == MIGO_HOST_SERVICE_STATUS_OK);
    CHECK(answer_to(7).has_value() && answer_to(7)->payload == "{\"data\":\"copied\"}");
    CHECK(running.clipboard.answered_on == std::this_thread::get_id());
}

void closing_retires_the_view_then_destroys_the_session() {
    fake_migo::reset();
    Running running;
    running.show();
    CHECK(running.session->close() == MIGO_OK);
    CHECK(running.session->close() == MIGO_OK);
    CHECK(!running.closed);
    CHECK(fake_migo::session_is_alive());
    // The door is shut: a task handed over now goes back to the engine.
    const MigoHostCallbacks &installed = fake_migo::installed_callbacks();
    CHECK(installed.dispatch(installed.dispatcher_data, [](void *) {}, nullptr) == MIGO_ERROR_INVALID_STATE);
    fake_migo::set_release_status(running.session->view()->generation(), MIGO_SURFACE_RELEASE_RELEASED);
    CHECK(pump_until([&running] { return running.closed; }));
    CHECK(!fake_migo::session_is_alive());
    CHECK(running.session->isClosed());
}

void a_session_never_shown_closes_at_once() {
    fake_migo::reset();
    Running running;
    CHECK(running.session->close() == MIGO_OK);
    CHECK(running.closed);
    CHECK(!fake_migo::session_is_alive());
}

}  // namespace

int main() {
    SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    SetCursorPos(GetSystemMetrics(SM_CXVIRTUALSCREEN) - 1, GetSystemMetrics(SM_CYVIRTUALSCREEN) - 1);
    it_creates_the_session_and_installs_its_callbacks();
    an_app_id_given_is_the_one_reported();
    content_is_loaded_once_the_view_attaches();
    engine_callbacks_run_on_the_gui_thread();
    the_windows_state_is_the_games();
    the_apps_services_answer_on_the_gui_thread();
    closing_retires_the_view_then_destroys_the_session();
    a_session_never_shown_closes_at_once();
    if (g_failures != 0) {
        std::fprintf(stderr, "%d Win32ManagedSession assertion(s) failed\n", g_failures);
        return 1;
    }
    std::puts("Win32ManagedSession contract: PASS");
    return 0;
}

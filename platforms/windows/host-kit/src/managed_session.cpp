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
#include <commctrl.h>
#include <shobjidl.h>

#include <atomic>
#include <cstdio>
#include <exception>
#include <mutex>
#include <string_view>
#include <thread>
#include <utility>

namespace migo::windows_host {

namespace {

constexpr UINT kTaskMessage = WM_APP + 1;
constexpr wchar_t kTaskWindowClass[] = L"MigoWin32SessionTasks";
// The id this session's watch on the App's window goes by.
constexpr UINT_PTR kWatchId = 0x4D69676F;  // "Migo"

[[noreturn]] void fatal(const char *why) {
    std::fprintf(stderr, "migo windows host kit: %s\n", why);
    std::fflush(stderr);
    std::terminate();
}

std::string utf8_of(std::wstring_view wide) {
    if (wide.empty()) return {};
    const int bytes = WideCharToMultiByte(CP_UTF8, WC_ERR_INVALID_CHARS, wide.data(), static_cast<int>(wide.size()),
                                          nullptr, 0, nullptr, nullptr);
    if (bytes <= 0) return {};
    std::string out(static_cast<std::size_t>(bytes), '\0');
    WideCharToMultiByte(CP_UTF8, WC_ERR_INVALID_CHARS, wide.data(), static_cast<int>(wide.size()), out.data(), bytes,
                        nullptr, nullptr);
    return out;
}

/// Who the App is when it does not say: the AppUserModelID it gave its
/// process -- what the shell knows it by -- or its executable's name.
std::string default_app_id() {
    PWSTR explicit_id = nullptr;
    if (SUCCEEDED(GetCurrentProcessExplicitAppUserModelID(&explicit_id)) && explicit_id != nullptr) {
        std::string id = utf8_of(explicit_id);
        CoTaskMemFree(explicit_id);
        if (!id.empty()) return id;
    }
    wchar_t path[MAX_PATH];
    const DWORD length = GetModuleFileNameW(nullptr, path, MAX_PATH);
    if (length == 0 || length >= MAX_PATH) return {};
    std::wstring_view name(path, length);
    if (const auto slash = name.find_last_of(L"\\/"); slash != std::wstring_view::npos) name.remove_prefix(slash + 1);
    if (const auto dot = name.find_last_of(L'.'); dot != std::wstring_view::npos) name = name.substr(0, dot);
    return utf8_of(name);
}

}  // namespace

class Win32ManagedSession::Impl final {
public:
    Impl(MigoEngine &engine, HWND parent, Options options, Listener listener)
        : options_(std::move(options)), listener_(std::move(listener)), owner_(std::this_thread::get_id()) {
        static std::once_flag registered;
        std::call_once(registered, [] {
            WNDCLASSEXW tasks{};
            tasks.cbSize = sizeof tasks;
            tasks.lpfnWndProc = &Impl::taskProcedure;
            tasks.hInstance = GetModuleHandleW(nullptr);
            tasks.lpszClassName = kTaskWindowClass;
            if (RegisterClassExW(&tasks) == 0) fatal("RegisterClassExW failed for the session's tasks");
        });
        // Where engine tasks wait for the GUI thread: a window no one sees.
        task_window_ = CreateWindowExW(0, kTaskWindowClass, L"", 0, 0, 0, 0, 0, HWND_MESSAGE, nullptr,
                                       GetModuleHandleW(nullptr), this);
        if (task_window_ == nullptr) fatal("CreateWindowExW failed for the session's tasks");

        MigoSessionConfig config{};
        config.struct_size = static_cast<std::uint32_t>(sizeof config);
        config.abi_version = MIGO_ABI_VERSION_1;
        config.network_policy_json_utf8 = options_.networkPolicyJson.empty() ? nullptr : options_.networkPolicyJson.c_str();
        config.launch_options_json_utf8 = options_.launchOptionsJson.empty() ? nullptr : options_.launchOptionsJson.c_str();
        const MigoResult created = migo_session_create(&engine, &config, &session_);
        if (created != MIGO_OK || session_ == nullptr) {
            last_error_ = created != MIGO_OK ? created : MIGO_ERROR_INTERNAL;
            session_ = nullptr;
            return;
        }

        surface_host_ = std::make_unique<SurfaceHost>(session_);
        Win32SurfaceView::Listener heard;
        heard.attached = [this](std::uint64_t) { surfaceAttached(); };
        heard.released = [this](std::uint64_t) { finishClose(); };
        heard.surfaceError = [this](MigoResult error) { reportError(error); };
        heard.inputRejected = [this](MigoResult error) { reportError(error); };
        view_ = std::make_unique<Win32SurfaceView>(*surface_host_, parent, std::move(heard));
        channel_ = HostServiceChannel::open(session_);
        app_mask_ = options_.appServices != nullptr ? options_.appServices->services() : 0;

        MigoHostCallbacks callbacks{};
        callbacks.struct_size = static_cast<std::uint32_t>(sizeof callbacks);
        callbacks.abi_version = MIGO_ABI_VERSION_1;
        callbacks.user_data = this;
        callbacks.dispatcher_data = this;
        callbacks.dispatch = &Impl::dispatch;
        callbacks.on_ready = &Impl::onReady;
        callbacks.on_error = &Impl::onError;
        callbacks.on_exit_requested = &Impl::onExitRequested;
        callbacks.on_request_frame = &Impl::onRequestFrame;
        callbacks.on_keep_screen_on = &Impl::onKeepScreenOn;
        if (app_mask_ != 0) {
            callbacks.on_host_service_call = &Impl::onHostServiceCall;
            callbacks.host_services = app_mask_;
        }
        accepting_.store(true, std::memory_order_release);
        if (const MigoResult installed = migo_session_set_host_callbacks(session_, &callbacks); installed != MIGO_OK) {
            last_error_ = installed;
            stopAccepting();
            view_.reset();
            surface_host_.reset();
            channel_->close();
            (void)migo_session_destroy(session_);
            session_ = nullptr;
            return;
        }
        app_id_ = options_.appId.empty() ? default_app_id() : options_.appId;
        MigoHostAppInfo app_info{};
        app_info.struct_size = static_cast<std::uint32_t>(sizeof app_info);
        app_info.abi_version = MIGO_ABI_VERSION_1;
        app_info.app_id_utf8 = app_id_.empty() ? nullptr : app_id_.c_str();
        // The rest is the engine's: the system's language, no benchmark score.
        app_info.benchmark_level = -1;
        (void)migo_session_set_host_app_info(session_, &app_info);

        // Whether the game runs and shows is its window's: the App's top-level.
        root_ = GetAncestor(parent, GA_ROOT);
        if (root_ != nullptr && SetWindowSubclass(root_, &Impl::watchProcedure, kWatchId, reinterpret_cast<DWORD_PTR>(this))) {
            watching_ = true;
        }
    }

    ~Impl() {
        if (session_ != nullptr && !closed_) fatal("Win32ManagedSession destroyed while its Session runs");
        stopAccepting();
        unwatch();
        // Tasks the engine handed over before the door closed run exactly once.
        drainTasks();
        if (task_window_ != nullptr) DestroyWindow(task_window_);
    }

    bool isValid() const noexcept { return session_ != nullptr && !closed_; }
    MigoResult lastError() const noexcept { return last_error_; }
    Win32SurfaceView *view() const noexcept { return view_.get(); }
    bool isClosed() const noexcept { return closed_; }

    MigoResult loadContent(const std::string &content_id, const std::string &entry) {
        if (std::this_thread::get_id() != owner_) return MIGO_ERROR_WRONG_THREAD;
        if (session_ == nullptr || closing_ || closed_) return MIGO_ERROR_INVALID_STATE;
        if (content_id.empty() || entry.empty()) return MIGO_ERROR_INVALID_ARGUMENT;
        if (content_requested_) return MIGO_ERROR_INVALID_STATE;
        content_requested_ = true;
        if (!attached_) {
            // Loaded once the view attaches: until then the Session has
            // nothing to run content on.
            pending_id_ = content_id;
            pending_entry_ = entry;
            return MIGO_OK;
        }
        return loadNow(content_id, entry);
    }

    MigoResult close() {
        if (std::this_thread::get_id() != owner_) return MIGO_ERROR_WRONG_THREAD;
        if (closed_ || closing_) return MIGO_OK;
        if (session_ == nullptr) return MIGO_ERROR_INVALID_STATE;
        closing_ = true;
        // Tasks posted from now would run after the Session is gone.
        stopAccepting();
        keepAwake(false);
        unwatch();
        if (view_ != nullptr && view_->surfaceState() != SurfaceState::Detached) {
            view_->close();
            return MIGO_OK;
        }
        if (view_ != nullptr) view_->close();
        finishClose();
        return MIGO_OK;
    }

private:
    struct Task {
        MigoTaskFn function = nullptr;
        void *context = nullptr;
    };

    // ---- engine callbacks, every one run on the GUI thread through dispatch ----

    static MigoResult MIGO_CALL dispatch(void *dispatcher_context, MigoTaskFn task, void *task_context) {
        auto *impl = static_cast<Impl *>(dispatcher_context);
        if (impl == nullptr || task == nullptr) return MIGO_ERROR_INVALID_ARGUMENT;
        // Refusing hands the task back to the engine -- the only safe answer
        // once teardown has begun, since it would run after its Session.
        const std::lock_guard<std::mutex> lock(impl->door_);
        if (!impl->accepting_.load(std::memory_order_acquire)) return MIGO_ERROR_INVALID_STATE;
        auto *queued = new (std::nothrow) Task{task, task_context};
        if (queued == nullptr) return MIGO_ERROR_OUT_OF_MEMORY;
        // Queued, so the engine's thread returns at once and the task runs with
        // no Migo lock held -- which is what lets a callback re-enter the
        // lifecycle or detach.
        if (!PostMessageW(impl->task_window_, kTaskMessage, 0, reinterpret_cast<LPARAM>(queued))) {
            delete queued;
            return MIGO_ERROR_DISPATCH_REJECTED;
        }
        return MIGO_OK;
    }

    static void MIGO_CALL onReady(void *user_data, MigoSession *) {
        auto *impl = static_cast<Impl *>(user_data);
        if (impl != nullptr && impl->listener_.contentReady) impl->listener_.contentReady();
    }

    static void MIGO_CALL onError(void *user_data, MigoSession *, const MigoError *error) {
        auto *impl = static_cast<Impl *>(user_data);
        if (impl == nullptr || !impl->listener_.contentFailed) return;
        const MigoResult code = error != nullptr ? error->code : MIGO_ERROR_INTERNAL;
        std::string message;
        if (error != nullptr && error->message_utf8 != nullptr) message.assign(error->message_utf8, error->message_length);
        impl->listener_.contentFailed(code, message);
    }

    static void MIGO_CALL onExitRequested(void *user_data, MigoSession *) {
        auto *impl = static_cast<Impl *>(user_data);
        if (impl != nullptr && impl->listener_.exitRequested) impl->listener_.exitRequested();
    }

    static void MIGO_CALL onRequestFrame(void *user_data, MigoSession *) {
        auto *impl = static_cast<Impl *>(user_data);
        if (impl != nullptr && impl->view_ != nullptr) (void)impl->view_->requestFrame();
    }

    static void MIGO_CALL onKeepScreenOn(void *user_data, MigoSession *, std::uint8_t keep_on) {
        auto *impl = static_cast<Impl *>(user_data);
        if (impl == nullptr) return;
        impl->screen_wanted_ = keep_on != 0;
        impl->keepAwake(impl->screen_wanted_ && impl->running_);
    }

    static void MIGO_CALL onHostServiceCall(void *user_data, MigoSession *, const MigoHostServiceCall *call) {
        auto *impl = static_cast<Impl *>(user_data);
        if (impl == nullptr || call == nullptr || impl->channel_ == nullptr) return;
        // Borrowed for this callback only: read now.
        const std::optional<Json> parsed =
            Json::parse(std::string_view(call->payload_json_utf8, call->payload_length));
        const Json request = parsed.has_value() ? *parsed : Json::object();
        HostServiceReply reply = impl->channel_->reply(call->call_id);
        if (impl->options_.appServices != nullptr && (impl->app_mask_ & hostServiceBit(call->service)) != 0) {
            impl->options_.appServices->call(call->service, call->method, request, std::move(reply));
        } else {
            (void)reply.fail("not supported");
        }
    }

    static LRESULT CALLBACK taskProcedure(HWND window, UINT message, WPARAM wparam, LPARAM lparam) {
        if (message == kTaskMessage) {
            const auto *task = reinterpret_cast<const Task *>(lparam);
            const Task run = *task;
            delete task;
            run.function(run.context);
            return 0;
        }
        return DefWindowProcW(window, message, wparam, lparam);
    }

    /// The App's top-level window: shown and minimized, as it changes.
    static LRESULT CALLBACK watchProcedure(HWND window, UINT message, WPARAM wparam, LPARAM lparam, UINT_PTR,
                                           DWORD_PTR reference) {
        const LRESULT result = DefSubclassProc(window, message, wparam, lparam);
        switch (message) {
            case WM_SIZE:
            case WM_SHOWWINDOW:
            case WM_WINDOWPOSCHANGED:
                reinterpret_cast<Impl *>(reference)->reportWindowState();
                break;
            case WM_NCDESTROY:
                reinterpret_cast<Impl *>(reference)->watching_ = false;
                RemoveWindowSubclass(window, &Impl::watchProcedure, kWatchId);
                break;
            default:
                break;
        }
        return result;
    }

    // ---- the session's own work ---------------------------------------------------------------------

    void stopAccepting() {
        const std::lock_guard<std::mutex> lock(door_);
        accepting_.store(false, std::memory_order_release);
    }

    void drainTasks() {
        if (task_window_ == nullptr) return;
        MSG message;
        while (PeekMessageW(&message, task_window_, kTaskMessage, kTaskMessage, PM_REMOVE)) {
            DispatchMessageW(&message);
        }
    }

    void unwatch() {
        if (!watching_) return;
        RemoveWindowSubclass(root_, &Impl::watchProcedure, kWatchId);
        watching_ = false;
    }

    void surfaceAttached() {
        attached_ = true;
        reportWindowState();
        if (!pending_id_.empty()) {
            const std::string id = std::move(pending_id_);
            const std::string entry = std::move(pending_entry_);
            pending_id_.clear();
            pending_entry_.clear();
            const MigoResult loaded = loadNow(id, entry);
            if (loaded != MIGO_OK && listener_.contentFailed) {
                listener_.contentFailed(loaded, "the content could not be loaded");
            }
        }
    }

    MigoResult loadNow(const std::string &content_id, const std::string &entry) {
        MigoContentDescriptor content{};
        content.struct_size = static_cast<std::uint32_t>(sizeof content);
        content.abi_version = MIGO_ABI_VERSION_1;
        content.content_id_utf8 = content_id.c_str();
        content.entry_utf8 = entry.c_str();
        const MigoResult result = migo_session_load_content(session_, &content);
        if (result != MIGO_OK) {
            last_error_ = result;
            // Refused, the content was not loaded: it may be asked for again.
            content_requested_ = false;
        }
        return result;
    }

    /// The game runs while its window is shown and not minimized -- nobody can
    /// bring it back without acting otherwise -- and shows while it runs.
    void reportWindowState() {
        if (session_ == nullptr || closing_ || closed_ || root_ == nullptr || !attached_) return;
        const bool running = IsWindowVisible(root_) && !IsIconic(root_);
        if (running != reported_visible_) {
            reported_visible_ = running;
            (void)migo_session_set_visibility(session_, running ? 1U : 0U);
        }
        if (running != running_) {
            running_ = running;
            (void)migo_session_set_lifecycle(session_, running ? MIGO_LIFECYCLE_RUNNING : MIGO_LIFECYCLE_PAUSED);
            keepAwake(screen_wanted_ && running_);
        }
    }

    /// The display, and the machine, kept awake for content's setKeepScreenOn.
    void keepAwake(bool on) {
        if (on == awake_) return;
        awake_ = on;
        SetThreadExecutionState(on ? (ES_CONTINUOUS | ES_DISPLAY_REQUIRED | ES_SYSTEM_REQUIRED) : ES_CONTINUOUS);
    }

    void reportError(MigoResult error) {
        last_error_ = error;
        if (listener_.migoError) listener_.migoError(error);
    }

    void finishClose() {
        if (closed_) return;
        if (!closing_) {
            // Released without a close: the App retired the view itself.
            return;
        }
        // No answer reaches the Session once it is destroyed.
        if (channel_ != nullptr) channel_->close();
        keepAwake(false);
        if (session_ != nullptr) {
            // Refused while an attachment lives or a release is pending, so
            // reaching here means both are settled.
            const MigoResult destroyed = migo_session_destroy(session_);
            if (destroyed != MIGO_OK) {
                reportError(destroyed);
                return;
            }
            session_ = nullptr;
        }
        closed_ = true;
        closing_ = false;
        if (listener_.closed) listener_.closed();
    }

    const Options options_;
    const Listener listener_;
    const std::thread::id owner_;
    HWND task_window_ = nullptr;
    std::mutex door_;
    std::atomic<bool> accepting_{false};

    MigoSession *session_ = nullptr;
    MigoResult last_error_ = MIGO_OK;
    std::unique_ptr<SurfaceHost> surface_host_;
    std::unique_ptr<Win32SurfaceView> view_;
    std::shared_ptr<HostServiceChannel> channel_;
    std::uint64_t app_mask_ = 0;
    std::string app_id_;

    HWND root_ = nullptr;
    bool watching_ = false;
    bool attached_ = false;
    bool running_ = true;
    bool reported_visible_ = true;
    bool screen_wanted_ = false;
    bool awake_ = false;

    bool content_requested_ = false;
    std::string pending_id_;
    std::string pending_entry_;
    bool closing_ = false;
    bool closed_ = false;
};

Win32ManagedSession::Win32ManagedSession(MigoEngine &engine, HWND__ *parent, Options options, Listener listener)
    : impl_(std::make_unique<Impl>(engine, parent, std::move(options), std::move(listener))) {}

Win32ManagedSession::Win32ManagedSession(MigoEngine &engine, HWND__ *parent)
    : Win32ManagedSession(engine, parent, Options(), Listener()) {}

Win32ManagedSession::~Win32ManagedSession() = default;

bool Win32ManagedSession::isValid() const noexcept { return impl_->isValid(); }
MigoResult Win32ManagedSession::lastError() const noexcept { return impl_->lastError(); }
Win32SurfaceView *Win32ManagedSession::view() const noexcept { return impl_->view(); }
MigoResult Win32ManagedSession::loadContent(const std::string &content_id, const std::string &entry) {
    return impl_->loadContent(content_id, entry);
}
MigoResult Win32ManagedSession::close() { return impl_->close(); }
bool Win32ManagedSession::isClosed() const noexcept { return impl_->isClosed(); }

}  // namespace migo::windows_host

#include "videos.hpp"

#include "gui_queue.hpp"
#include "http_download.hpp"
#include "image_codec.hpp"
#include "video_controls.hpp"
#include "win32_text.hpp"

#include <commctrl.h>
#include <d2d1_1.h>
#include <d3d11.h>
#include <dwmapi.h>
#include <dxgi1_2.h>
#include <mfapi.h>
#include <mfidl.h>
#include <mfmediaengine.h>
#include <shlwapi.h>

#include <algorithm>
#include <atomic>
#include <cmath>
#include <cstdio>
#include <cstdlib>
#include <exception>
#include <map>
#include <mutex>
#include <optional>
#include <string>
#include <thread>
#include <utility>
#include <vector>

namespace migo::windows_host {

namespace {

using codec::ComPtr;

constexpr wchar_t kPlayerClass[] = L"MigoVideoPlayer";
constexpr UINT_PTR kWatchId = 0x4D69676F + 9;
constexpr UINT_PTR kTimeTimer = 1;
// Content hears where it is four times a second while it plays.
constexpr UINT kTimeUpdateMs = 250;
// A network poster is read up to 8 MiB, as the Apple SDK reads one.
constexpr std::uint64_t kMaxPosterBytes = 8ULL << 20;
constexpr char kDestroyed[] = "video destroyed";
constexpr char kCannotPlay[] = "the src cannot be played";

[[noreturn]] void fatal(const char *why) {
    std::fprintf(stderr, "migo windows host kit: %s\n", why);
    std::fflush(stderr);
    std::terminate();
}

/// A COM apartment for the calling thread, for as long as it lives.
class Apartment final {
public:
    Apartment() : joined_(SUCCEEDED(CoInitializeEx(nullptr, COINIT_MULTITHREADED))) {}
    ~Apartment() {
        if (joined_) CoUninitialize();
    }
    Apartment(const Apartment &) = delete;
    Apartment &operator=(const Apartment &) = delete;

private:
    const bool joined_;
};

/// A CSS colour as content's backgroundColor gives it -- #rgb, #rrggbb,
/// #rrggbbaa, rgb() or rgba() -- or none when it is not one of those.
std::optional<D2D1_COLOR_F> color_of(const std::string &text) {
    const auto hex = [](char digit) -> int {
        if (digit >= '0' && digit <= '9') return digit - '0';
        if (digit >= 'a' && digit <= 'f') return digit - 'a' + 10;
        if (digit >= 'A' && digit <= 'F') return digit - 'A' + 10;
        return -1;
    };
    if (!text.empty() && text[0] == '#') {
        std::vector<int> digits;
        for (std::size_t index = 1; index < text.size(); ++index) {
            const int value = hex(text[index]);
            if (value < 0) return std::nullopt;
            digits.push_back(value);
        }
        if (digits.size() == 3) {
            return D2D1::ColorF(static_cast<float>(digits[0] * 17) / 255.0F, static_cast<float>(digits[1] * 17) / 255.0F,
                                static_cast<float>(digits[2] * 17) / 255.0F, 1.0F);
        }
        if (digits.size() == 6 || digits.size() == 8) {
            const auto channel = [&digits](std::size_t at) {
                return static_cast<float>(digits[at] * 16 + digits[at + 1]) / 255.0F;
            };
            return D2D1::ColorF(channel(0), channel(2), channel(4), digits.size() == 8 ? channel(6) : 1.0F);
        }
        return std::nullopt;
    }
    // rgb(r, g, b) or rgba(r, g, b, a): the numbers between the parentheses.
    const bool alpha = text.rfind("rgba(", 0) == 0;
    if (!alpha && text.rfind("rgb(", 0) != 0) return std::nullopt;
    const std::size_t open = text.find('(');
    const std::size_t close = text.rfind(')');
    if (close == std::string::npos || close < open) return std::nullopt;
    std::vector<float> numbers;
    const char *at = text.c_str() + open + 1;
    const char *const end = text.c_str() + close;
    while (at < end) {
        char *after = nullptr;
        const float value = std::strtof(at, &after);
        if (after == at) return std::nullopt;
        numbers.push_back(value);
        at = after;
        while (at < end && (*at == ' ' || *at == ',')) ++at;
    }
    if (numbers.size() != (alpha ? 4U : 3U)) return std::nullopt;
    const auto clamp = [](float value, float top) { return std::clamp(value, 0.0F, top); };
    return D2D1::ColorF(clamp(numbers[0], 255) / 255.0F, clamp(numbers[1], 255) / 255.0F,
                        clamp(numbers[2], 255) / 255.0F, alpha ? clamp(numbers[3], 1) : 1.0F);
}

/// What a source names, as the media engine opens it: a URL as it is, a path
/// as a file URL.
std::wstring source_url(const std::string &source) {
    if (isRemote(source) || source.rfind("file:", 0) == 0) return wideOf(source);
    const std::wstring path = wideOf(source);
    wchar_t url[2084] = {};
    DWORD length = 2084;
    if (FAILED(UrlCreateFromPathW(path.c_str(), url, &length, 0))) return path;
    return std::wstring(url, length);
}

/// The media engine's news, each brought to the GUI thread.
class EngineNotify final : public IMFMediaEngineNotify {
public:
    using Handle = std::function<void(DWORD event, DWORD_PTR first, DWORD second)>;
    EngineNotify(GuiQueue::Post post, Handle handle) : post_(std::move(post)), handle_(std::move(handle)) {}

    HRESULT STDMETHODCALLTYPE QueryInterface(REFIID id, void **out) override {
        if (out == nullptr) return E_POINTER;
        if (IsEqualIID(id, __uuidof(IUnknown)) || IsEqualIID(id, __uuidof(IMFMediaEngineNotify))) {
            *out = static_cast<IMFMediaEngineNotify *>(this);
            AddRef();
            return S_OK;
        }
        *out = nullptr;
        return E_NOINTERFACE;
    }
    ULONG STDMETHODCALLTYPE AddRef() override { return references_.fetch_add(1) + 1; }
    ULONG STDMETHODCALLTYPE Release() override {
        const ULONG left = references_.fetch_sub(1) - 1;
        if (left == 0) delete this;
        return left;
    }
    HRESULT STDMETHODCALLTYPE EventNotify(DWORD event, DWORD_PTR first, DWORD second) override {
        // The engine waits on this one's event; nothing else is answered here.
        if (event == MF_MEDIA_ENGINE_EVENT_NOTIFYSTABLESTATE) {
            SetEvent(reinterpret_cast<HANDLE>(first));
            return S_OK;
        }
        post_([handle = handle_, event, first, second] { handle(event, first, second); });
        return S_OK;
    }

private:
    ~EngineNotify() = default;
    std::atomic<ULONG> references_{1};
    const GuiQueue::Post post_;
    const Handle handle_;
};

/// The device every player draws with: the GPU's, or WARP where there is
/// none, with what the media engine decodes on where it can.
struct Graphics {
    ComPtr<ID3D11Device> device;
    ComPtr<IMFDXGIDeviceManager> manager;
    UINT reset_token = 0;
    ComPtr<ID2D1Factory1> d2d;
    ComPtr<ID2D1Device> d2d_device;
    ComPtr<IDXGIFactory2> dxgi;
    ComPtr<IMFMediaEngineClassFactory> engines;
    ComPtr<IWICImagingFactory> wic;
    bool media_started = false;

    ~Graphics() {
        if (media_started) MFShutdown();
    }

    bool make() {
        const D3D_FEATURE_LEVEL levels[] = {D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_10_1,
                                            D3D_FEATURE_LEVEL_10_0};
        const auto create = [&](D3D_DRIVER_TYPE type, UINT flags) {
            return SUCCEEDED(D3D11CreateDevice(nullptr, type, nullptr, flags, levels, ARRAYSIZE(levels),
                                               D3D11_SDK_VERSION, device.ReleaseAndGetAddressOf(), nullptr, nullptr));
        };
        // Decoding on the device where it can; drawing on it in any case.
        const UINT video = D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT;
        bool decodes = create(D3D_DRIVER_TYPE_HARDWARE, video) || create(D3D_DRIVER_TYPE_WARP, video);
        if (!decodes && !create(D3D_DRIVER_TYPE_HARDWARE, D3D11_CREATE_DEVICE_BGRA_SUPPORT) &&
            !create(D3D_DRIVER_TYPE_WARP, D3D11_CREATE_DEVICE_BGRA_SUPPORT)) {
            return false;
        }
        // The media engine decodes on its threads with the device the players draw with.
        ComPtr<ID3D10Multithread> guarded;
        if (SUCCEEDED(device.As(&guarded))) guarded->SetMultithreadProtected(TRUE);
        media_started = SUCCEEDED(MFStartup(MF_VERSION, MFSTARTUP_LITE));
        if (!media_started) return false;
        if (decodes && (FAILED(MFCreateDXGIDeviceManager(&reset_token, manager.GetAddressOf())) ||
                        FAILED(manager->ResetDevice(device.Get(), reset_token)))) {
            manager.Reset();
        }
        ComPtr<IDXGIDevice> dxgi_device;
        ComPtr<IDXGIAdapter> adapter;
        if (FAILED(device.As(&dxgi_device)) || FAILED(dxgi_device->GetAdapter(adapter.GetAddressOf())) ||
            FAILED(adapter->GetParent(IID_PPV_ARGS(dxgi.GetAddressOf()))) ||
            FAILED(D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, __uuidof(ID2D1Factory1), nullptr,
                                     reinterpret_cast<void **>(d2d.GetAddressOf()))) ||
            FAILED(d2d->CreateDevice(dxgi_device.Get(), d2d_device.GetAddressOf())) ||
            FAILED(CoCreateInstance(CLSID_MFMediaEngineClassFactory, nullptr, CLSCTX_INPROC_SERVER,
                                    IID_PPV_ARGS(engines.GetAddressOf())))) {
            return false;
        }
        wic = codec::factory();
        return wic != nullptr;
    }
};

/// A file held open, read as a stream: what a player plays from when the file's
/// name may go -- the file the engine lent a preview, which it takes back once
/// the call is answered. Reads at its own position, one at a time, whichever
/// thread Media Foundation reads on.
class HandleStream final : public IStream {
public:
    explicit HandleStream(HANDLE file) : file_(file) {}

    HRESULT STDMETHODCALLTYPE QueryInterface(REFIID id, void **out) override {
        if (out == nullptr) return E_POINTER;
        if (IsEqualIID(id, __uuidof(IUnknown)) || IsEqualIID(id, __uuidof(IStream)) ||
            IsEqualIID(id, __uuidof(ISequentialStream))) {
            *out = static_cast<IStream *>(this);
            AddRef();
            return S_OK;
        }
        *out = nullptr;
        return E_NOINTERFACE;
    }
    ULONG STDMETHODCALLTYPE AddRef() override { return references_.fetch_add(1) + 1; }
    ULONG STDMETHODCALLTYPE Release() override {
        const ULONG left = references_.fetch_sub(1) - 1;
        if (left == 0) delete this;
        return left;
    }

    HRESULT STDMETHODCALLTYPE Read(void *into, ULONG wanted, ULONG *read) override {
        const std::lock_guard<std::mutex> lock(mutex_);
        OVERLAPPED at{};
        at.Offset = static_cast<DWORD>(position_ & 0xFFFFFFFFULL);
        at.OffsetHigh = static_cast<DWORD>(position_ >> 32);
        DWORD got = 0;
        if (!ReadFile(file_, into, wanted, &got, &at) && GetLastError() != ERROR_HANDLE_EOF) {
            if (read != nullptr) *read = 0;
            return HRESULT_FROM_WIN32(GetLastError());
        }
        position_ += got;
        if (read != nullptr) *read = got;
        return got < wanted ? S_FALSE : S_OK;
    }
    HRESULT STDMETHODCALLTYPE Write(const void *, ULONG, ULONG *) override { return STG_E_ACCESSDENIED; }
    HRESULT STDMETHODCALLTYPE Seek(LARGE_INTEGER move, DWORD origin, ULARGE_INTEGER *now) override {
        const std::lock_guard<std::mutex> lock(mutex_);
        LARGE_INTEGER size{};
        if (!GetFileSizeEx(file_, &size)) return HRESULT_FROM_WIN32(GetLastError());
        long long base = 0;
        if (origin == STREAM_SEEK_CUR) base = static_cast<long long>(position_);
        if (origin == STREAM_SEEK_END) base = size.QuadPart;
        const long long target = base + move.QuadPart;
        if (target < 0) return STG_E_INVALIDFUNCTION;
        position_ = static_cast<unsigned long long>(target);
        if (now != nullptr) now->QuadPart = position_;
        return S_OK;
    }
    HRESULT STDMETHODCALLTYPE SetSize(ULARGE_INTEGER) override { return STG_E_ACCESSDENIED; }
    HRESULT STDMETHODCALLTYPE CopyTo(IStream *, ULARGE_INTEGER, ULARGE_INTEGER *, ULARGE_INTEGER *) override {
        return E_NOTIMPL;
    }
    HRESULT STDMETHODCALLTYPE Commit(DWORD) override { return S_OK; }
    HRESULT STDMETHODCALLTYPE Revert() override { return S_OK; }
    HRESULT STDMETHODCALLTYPE LockRegion(ULARGE_INTEGER, ULARGE_INTEGER, DWORD) override {
        return STG_E_INVALIDFUNCTION;
    }
    HRESULT STDMETHODCALLTYPE UnlockRegion(ULARGE_INTEGER, ULARGE_INTEGER, DWORD) override {
        return STG_E_INVALIDFUNCTION;
    }
    HRESULT STDMETHODCALLTYPE Stat(STATSTG *stat, DWORD) override {
        if (stat == nullptr) return STG_E_INVALIDPOINTER;
        *stat = STATSTG{};
        stat->type = STGTY_STREAM;
        LARGE_INTEGER size{};
        if (!GetFileSizeEx(file_, &size)) return HRESULT_FROM_WIN32(GetLastError());
        stat->cbSize.QuadPart = static_cast<ULONGLONG>(size.QuadPart);
        stat->grfMode = STGM_READ;
        return S_OK;
    }
    HRESULT STDMETHODCALLTYPE Clone(IStream **) override { return E_NOTIMPL; }

private:
    ~HandleStream() = default;
    std::atomic<ULONG> references_{1};
    std::mutex mutex_;
    const HANDLE file_;
    unsigned long long position_ = 0;
};

/// `file` as Media Foundation reads a source; the player keeps the handle.
HRESULT byte_stream_on(HANDLE file, ComPtr<IMFByteStream> &bytes) {
    auto *stream = new HandleStream(file);
    const HRESULT result = MFCreateMFByteStreamOnStream(stream, bytes.GetAddressOf());
    stream->Release();
    return result;
}

}  // namespace

/// Where a player stands: the window that owns its windows, the window whose
/// client area its place is in -- the game's view, or a viewer -- that
/// window's pixels to a logical one, and who hears what it does (nobody, for a
/// player that is not content's).
struct VideoStage {
    HWND owner = nullptr;
    HWND reference = nullptr;
    std::function<float()> scale;
    std::function<void(std::uint32_t event, Json payload)> tell;
    /// A file held open that it plays, its URL `src` naming only its kind; or
    /// none, when `src` is what it plays.
    HANDLE file = INVALID_HANDLE_VALUE;
};

class Videos::Impl final {
public:
    class Player;
    class Embedded;

    Impl(Win32SurfaceView &view, std::shared_ptr<HostServiceChannel> channel)
        : view_(view), root_(GetAncestor(view.hwnd(), GA_ROOT)), channel_(std::move(channel)),
          post_(queue_.poster()) {
        static std::once_flag registered;
        std::call_once(registered, [] {
            WNDCLASSEXW player{};
            player.cbSize = sizeof player;
            player.lpfnWndProc = &Impl::playerProcedure;
            player.hInstance = GetModuleHandleW(nullptr);
            player.hCursor = LoadCursorW(nullptr, IDC_ARROW);
            player.style = CS_DBLCLKS;
            player.lpszClassName = kPlayerClass;
            if (RegisterClassExW(&player) == 0) fatal("RegisterClassExW failed for the video players");
        });
        watching_view_ = SetWindowSubclass(view_.hwnd(), &Impl::watch, kWatchId, reinterpret_cast<DWORD_PTR>(this));
        if (root_ != nullptr && root_ != view_.hwnd()) {
            watching_root_ = SetWindowSubclass(root_, &Impl::watch, kWatchId, reinterpret_cast<DWORD_PTR>(this));
        }
    }

    ~Impl();

    void call(std::uint32_t method, const Json &request, HostServiceReply reply);
    void setRunning(bool running);
    std::unique_ptr<EmbeddedVideo> embed(HWND window, HANDLE file, const std::string &url, const std::string &poster);
    void unembed(Player *player);

    // ---- what the players share ---------------------------------------------------------------------

    Graphics *graphics() {
        if (graphics_ == nullptr && !graphics_failed_) {
            auto made = std::make_unique<Graphics>();
            if (made->make()) {
                graphics_ = std::move(made);
            } else {
                graphics_failed_ = true;
            }
        }
        return graphics_.get();
    }

    [[nodiscard]] Win32SurfaceView &view() const noexcept { return view_; }
    [[nodiscard]] HWND root() const noexcept { return root_; }
    [[nodiscard]] const GuiQueue::Post &post() const noexcept { return post_; }

    void tell(std::int64_t id, std::uint32_t event, Json payload) {
        payload.set("videoId", static_cast<double>(id));
        // A reading the next supersedes is dropped when content falls behind.
        (void)channel_->post(MIGO_HOST_SERVICE_VIDEO, event, payload);
    }

    /// The compositor's beat, for as long as a player is there to draw.
    void clock();

private:
    static LRESULT CALLBACK playerProcedure(HWND window, UINT message, WPARAM wparam, LPARAM lparam);
    static LRESULT CALLBACK watch(HWND window, UINT message, WPARAM wparam, LPARAM lparam, UINT_PTR, DWORD_PTR reference);

    void placeAll();
    void startClock();
    void stopClock();
    void beat();

    Win32SurfaceView &view_;
    const HWND root_;
    const std::shared_ptr<HostServiceChannel> channel_;
    GuiQueue queue_;
    const GuiQueue::Post post_;
    bool watching_view_ = false;
    bool watching_root_ = false;
    bool running_ = true;
    std::unique_ptr<Graphics> graphics_;
    bool graphics_failed_ = false;
    std::map<std::int64_t, std::unique_ptr<Player>> players_;
    /// The players shown in the Host Kit's own windows: no content's.
    std::map<Player *, std::unique_ptr<Player>> embedded_;

    std::thread clock_;
    std::atomic<bool> clocking_{false};
    std::atomic<bool> beat_pending_{false};
};

// ---- one player ------------------------------------------------------------------------------------

class Videos::Impl::Player final : public VideoControlled {
public:
    Player(Impl &owner, VideoStage stage, Json options)
        : owner_(owner), stage_(std::move(stage)), options_(std::move(options)) {
        window_ = CreateWindowExW(WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW, kPlayerClass, L"", WS_POPUP, 0, 0, 1, 1,
                                  stage_.owner, nullptr, GetModuleHandleW(nullptr), this);
        if (window_ == nullptr) fatal("CreateWindowExW failed for a video player");
        // Made after the player's window, so above it.
        controls_ = std::make_unique<VideoControls>(stage_.owner, *this);
        controls_->configure(options_);
        startAt_ = number("initialTime", 0);
        playWanted_ = flag("autoplay", false);
        place();
        loadPoster();
        openSource();
    }

    ~Player() {
        release(kDestroyed);
        SetWindowLongPtrW(window_, GWLP_USERDATA, 0);
        DestroyWindow(window_);
        if (stage_.file != INVALID_HANDLE_VALUE) CloseHandle(stage_.file);
    }

    Player(const Player &) = delete;
    Player &operator=(const Player &) = delete;

    // ---- calls --------------------------------------------------------------------------------------

    void play(HostServiceReply reply) {
        pendingPlays_.push_back(std::move(reply));
        if (engine_ == nullptr || !broken_.empty()) {
            answer(pendingPlays_, string("src").empty() ? std::string("no src")
                                  : broken_.empty()     ? std::string(kCannotPlay)
                                                        : broken_);
            return;
        }
        playWanted_ = true;
        if (playing_) {
            answer(pendingPlays_, std::nullopt);
        } else if (prepared_ && !suspended_) {
            start();
        }
    }

    void pause(HostServiceReply reply) {
        // A play waiting for the player never plays: it is overtaken.
        playWanted_ = false;
        answer(pendingPlays_, std::string("paused before it played"));
        pauseNow();
        (void)reply.succeed();
    }

    void stop(HostServiceReply reply) {
        playWanted_ = false;
        answer(pendingPlays_, std::string("stopped before it played"));
        halt();
        resumeAfterSuspend_ = false;
        if (prepared_ && broken_.empty()) {
            seekPlayback(0);
        } else {
            startAt_ = 0;
        }
        (void)reply.succeed();
    }

    void seek(double seconds, HostServiceReply reply) {
        pendingSeeks_.push_back(std::move(reply));
        if (engine_ == nullptr || !broken_.empty()) {
            answer(pendingSeeks_, string("src").empty() ? std::string("no src") : std::string(kCannotPlay));
        } else if (prepared_) {
            seekPlayback(seconds);
        } else {
            startAt_ = seconds;
        }
    }

    void fullScreen(bool on, HostServiceReply reply) {
        fullScreen_ = on;
        place();
        // Laid out: the window moved as it was asked to.
        (void)reply.succeed();
    }

    /// What content changed.
    void set(const Json &changed) {
        if (released_) return;
        for (const auto &[key, value] : changed.members()) {
            if (key != "videoId") options_.set(key, value);
        }
        const auto has = [&changed](const char *key) { return changed.contains(key); };
        if (has("x") || has("y") || has("width") || has("height") || has("underGameView")) place();
        if (has("objectFit") || has("backgroundColor")) render();
        if (has("muted") && engine_ != nullptr) engine_->SetMuted(flag("muted", false) ? TRUE : FALSE);
        if (has("loop") && engine_ != nullptr) engine_->SetLoop(flag("loop", false) ? TRUE : FALSE);
        if (has("playbackRate") && playing_ && engine_ != nullptr) engine_->SetPlaybackRate(rate());
        for (const char *option : {"controls", "showProgress", "showProgressInControlMode", "showCenterPlayBtn",
                                   "enablePlayGesture", "enableProgressGesture"}) {
            if (has(option)) {
                controls_->configure(options_);
                break;
            }
        }
        if (has("poster")) loadPoster();
        if (has("src")) {
            playWanted_ = false;
            failPending("src changed");
            startAt_ = number("initialTime", 0);
            openSource();
        }
    }

    // ---- the game -----------------------------------------------------------------------------------

    void suspend() {
        suspended_ = true;
        if (!playing_) return;
        halt();
        resumeAfterSuspend_ = true;
    }

    void resume() {
        suspended_ = false;
        if (released_ || !(resumeAfterSuspend_ || playWanted_)) return;
        resumeAfterSuspend_ = false;
        if (prepared_ && !playing_ && broken_.empty()) start();
    }

    /// Gone: its calls still unanswered fail with `reason`.
    void release(const std::string &reason) {
        if (released_) return;
        failPending(reason);
        released_ = true;
        ++posterGeneration_;
        stopPoster();
        KillTimer(window_, kTimeTimer);
        closeEngine();
        ShowWindow(window_, SW_HIDE);
        controls_->place(window_, 0, 0, 0, 0, 1, false);
    }

    /// Where content put it -- or the whole of the game's view when full
    /// screen -- cut to the view; hidden while the view is, and under the game.
    void place() {
        if (released_) return;
        HWND view = stage_.reference;
        RECT client{};
        GetClientRect(view, &client);
        RECT wanted = client;
        if (!fullScreen_) {
            const double scale = stage_.scale();
            const double x = number("x", 0);
            const double y = number("y", 0);
            wanted = RECT{static_cast<LONG>(std::lround(x * scale)), static_cast<LONG>(std::lround(y * scale)),
                          static_cast<LONG>(std::lround((x + number("width", 300)) * scale)),
                          static_cast<LONG>(std::lround((y + number("height", 150)) * scale))};
        }
        RECT shown{};
        if (!IsWindowVisible(view) || (flag("underGameView", false) && !fullScreen_) ||
            !IntersectRect(&shown, &wanted, &client)) {
            ShowWindow(window_, SW_HIDE);
            controls_->place(window_, 0, 0, 0, 0, 1, false);
            return;
        }
        content_ = SIZE{wanted.right - wanted.left, wanted.bottom - wanted.top};
        offset_ = POINT{wanted.left - shown.left, wanted.top - shown.top};
        POINT origin{shown.left, shown.top};
        ClientToScreen(view, &origin);
        const SIZE size{shown.right - shown.left, shown.bottom - shown.top};
        SetWindowPos(window_, nullptr, origin.x, origin.y, size.cx, size.cy,
                     SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOOWNERZORDER | SWP_SHOWWINDOW);
        controls_->place(window_, origin.x, origin.y, size.cx, size.cy, stage_.scale(), true);
        resize(size);
        render();
    }

    /// At `left`, `top`, `width` x `height` pixels of its window's client area.
    void placeAt(int left, int top, int width, int height) {
        const double scale = stage_.scale();
        options_.set("x", left / scale).set("y", top / scale).set("width", width / scale).set("height", height / scale);
        place();
    }

    // ---- what the controls drive ------------------------------------------------------------------

    [[nodiscard]] bool isPlaying() const override { return playing_; }
    [[nodiscard]] bool isFullScreen() const override { return fullScreen_; }

    void togglePlay() override {
        if (released_ || engine_ == nullptr || !broken_.empty()) return;
        if (playing_) {
            playWanted_ = false;
            pauseNow();
        } else {
            playWanted_ = true;
            if (prepared_ && !suspended_) start();
        }
    }

    void seekTo(double seconds) override {
        if (released_ || !prepared_ || engine_ == nullptr || !broken_.empty()) return;
        seekPlayback(seconds);
        controls_->refresh();
    }

    void toggleFullScreen() override {
        if (released_) return;
        fullScreen_ = !fullScreen_;
        place();
    }

    void inputChanged() override {}

    [[nodiscard]] bool takesInput() const { return !released_ && controls_->takesInput(); }
    bool mouse(UINT message, WPARAM wparam, LPARAM lparam) { return controls_->mouse(message, wparam, lparam); }

    /// One beat of the compositor: a new frame drawn, where the engine has one.
    void beat() {
        if (released_ || engine_ == nullptr || !prepared_ || frame_ == nullptr) return;
        LONGLONG presented = 0;
        if (engine_->OnVideoStreamTick(&presented) != S_OK) return;
        if (!transfer()) return;
        haveFrame_ = true;
        render();
    }

    /// Four times a second while it plays: where it is, and what it holds.
    void tick() {
        if (released_ || !playing_) return;
        controls_->refresh();
        tell(MIGO_VIDEO_EVENT_TIME_UPDATE,
                    Json::object().set("position", position()).set("duration", duration()));
        reportBuffered();
    }

    void engineEvent(DWORD event, DWORD_PTR first, DWORD) {
        if (released_ || engine_ == nullptr) return;
        switch (event) {
            case MF_MEDIA_ENGINE_EVENT_LOADEDMETADATA:
                prepareFrames();
                break;
            case MF_MEDIA_ENGINE_EVENT_CANPLAY:
                settled();
                break;
            case MF_MEDIA_ENGINE_EVENT_SEEKED:
                // A seek a later one overtakes arrives where the later one asked:
                // the later one's answer is the answer to both.
                answer(pendingSeeks_, std::nullopt);
                controls_->refresh();
                break;
            case MF_MEDIA_ENGINE_EVENT_DURATIONCHANGE:
                controls_->refresh();
                break;
            case MF_MEDIA_ENGINE_EVENT_PLAYING:
                waiting_ = false;
                break;
            case MF_MEDIA_ENGINE_EVENT_WAITING:
                if (playing_ && !waiting_) {
                    waiting_ = true;
                    tell(MIGO_VIDEO_EVENT_WAITING, Json::object());
                }
                break;
            case MF_MEDIA_ENGINE_EVENT_PROGRESS:
                reportBuffered();
                break;
            case MF_MEDIA_ENGINE_EVENT_ENDED:
                ended();
                break;
            case MF_MEDIA_ENGINE_EVENT_ERROR:
                report(first == MF_MEDIA_ENGINE_ERR_NETWORK ? std::string("network error") : std::string(kCannotPlay));
                break;
            default:
                break;
        }
    }

    [[nodiscard]] HWND window() const noexcept { return window_; }

private:
    friend class Videos::Impl;

    // ---- the engine -----------------------------------------------------------------------------------

    void openSource() {
        closeEngine();
        played_ = false;
        const std::string src = string("src");
        if (src.empty()) return;
        Graphics *graphics = owner_.graphics();
        if (graphics == nullptr) {
            report("Media Foundation is not available");
            return;
        }
        // News of an engine a later source replaced is not this one's.
        const std::uint64_t generation = ++engineGeneration_;
        auto alive = alive_;
        auto *notify = new EngineNotify(owner_.post(), [this, alive, generation](DWORD event, DWORD_PTR first,
                                                                                 DWORD second) {
            if (*alive && generation == engineGeneration_) engineEvent(event, first, second);
        });
        ComPtr<IMFAttributes> attributes;
        HRESULT result = MFCreateAttributes(attributes.GetAddressOf(), 3);
        if (SUCCEEDED(result)) result = attributes->SetUnknown(MF_MEDIA_ENGINE_CALLBACK, notify);
        if (SUCCEEDED(result) && graphics->manager != nullptr) {
            result = attributes->SetUnknown(MF_MEDIA_ENGINE_DXGI_MANAGER, graphics->manager.Get());
        }
        if (SUCCEEDED(result)) result = attributes->SetUINT32(MF_MEDIA_ENGINE_VIDEO_OUTPUT_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM);
        if (SUCCEEDED(result)) result = graphics->engines->CreateInstance(0, attributes.Get(), engine_.GetAddressOf());
        notify->Release();
        if (FAILED(result)) {
            engine_.Reset();
            report("Media Foundation is not available");
            return;
        }
        engine_->SetAutoPlay(FALSE);
        engine_->SetMuted(flag("muted", false) ? TRUE : FALSE);
        engine_->SetLoop(flag("loop", false) ? TRUE : FALSE);
        engine_->SetDefaultPlaybackRate(rate());
        const std::wstring url = source_url(src);
        BSTR source = SysAllocString(url.c_str());
        if (stage_.file != INVALID_HANDLE_VALUE) {
            // The file held open, not its name: what was lent may be taken back.
            ComPtr<IMFMediaEngineEx> extended;
            ComPtr<IMFByteStream> bytes;
            result = engine_.As(&extended);
            if (SUCCEEDED(result)) result = byte_stream_on(stage_.file, bytes);
            if (SUCCEEDED(result)) result = extended->SetSourceFromByteStream(bytes.Get(), source);
        } else {
            result = engine_->SetSource(source);
        }
        SysFreeString(source);
        if (FAILED(result)) report(kCannotPlay);
    }

    void closeEngine() {
        KillTimer(window_, kTimeTimer);
        ++engineGeneration_;
        if (engine_ != nullptr) engine_->Shutdown();
        engine_.Reset();
        frame_.Reset();
        texture_.Reset();
        softFrame_.Reset();
        haveFrame_ = false;
        prepared_ = false;
        playing_ = false;
        ended_ = false;
        waiting_ = false;
        lastBuffered_ = -1;
        broken_.clear();
        render();
    }

    /// Where its frames are drawn into, once its size is known.
    void prepareFrames() {
        Graphics *graphics = owner_.graphics();
        DWORD width = 0;
        DWORD height = 0;
        if (graphics == nullptr || dc_ == nullptr || FAILED(engine_->GetNativeVideoSize(&width, &height)) ||
            width == 0 || height == 0) {
            return;
        }
        frame_.Reset();
        texture_.Reset();
        softFrame_.Reset();
        videoSize_ = SIZE{static_cast<LONG>(width), static_cast<LONG>(height)};
        const D2D1_BITMAP_PROPERTIES1 drawn = D2D1::BitmapProperties1(
            D2D1_BITMAP_OPTIONS_NONE, D2D1::PixelFormat(DXGI_FORMAT_B8G8R8A8_UNORM, D2D1_ALPHA_MODE_IGNORE));
        if (graphics->manager != nullptr) {
            // On the device: the engine writes the frame, Direct2D reads it.
            D3D11_TEXTURE2D_DESC description{};
            description.Width = width;
            description.Height = height;
            description.MipLevels = 1;
            description.ArraySize = 1;
            description.Format = DXGI_FORMAT_B8G8R8A8_UNORM;
            description.SampleDesc.Count = 1;
            description.Usage = D3D11_USAGE_DEFAULT;
            description.BindFlags = D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE;
            ComPtr<IDXGISurface> surface;
            if (SUCCEEDED(graphics->device->CreateTexture2D(&description, nullptr, texture_.GetAddressOf())) &&
                SUCCEEDED(texture_.As(&surface)) &&
                SUCCEEDED(dc_->CreateBitmapFromDxgiSurface(surface.Get(), &drawn, frame_.GetAddressOf()))) {
                return;
            }
            texture_.Reset();
        }
        // Decoded in software: the frame goes through memory.
        if (SUCCEEDED(graphics->wic->CreateBitmap(width, height, GUID_WICPixelFormat32bppPBGRA, WICBitmapCacheOnLoad,
                                                  softFrame_.GetAddressOf()))) {
            const D2D1_BITMAP_PROPERTIES1 copied = D2D1::BitmapProperties1(
                D2D1_BITMAP_OPTIONS_NONE, D2D1::PixelFormat(DXGI_FORMAT_B8G8R8A8_UNORM, D2D1_ALPHA_MODE_PREMULTIPLIED));
            dc_->CreateBitmap(D2D1::SizeU(width, height), nullptr, 0, &copied, frame_.GetAddressOf());
        }
    }

    bool transfer() {
        const RECT whole{0, 0, videoSize_.cx, videoSize_.cy};
        const MFARGB black{0, 0, 0, 255};
        if (texture_ != nullptr) {
            return SUCCEEDED(engine_->TransferVideoFrame(texture_.Get(), nullptr, &whole, &black));
        }
        if (softFrame_ == nullptr || FAILED(engine_->TransferVideoFrame(softFrame_.Get(), nullptr, &whole, &black))) {
            return false;
        }
        ComPtr<IWICBitmapLock> lock;
        const WICRect all{0, 0, videoSize_.cx, videoSize_.cy};
        UINT stride = 0;
        UINT size = 0;
        BYTE *pixels = nullptr;
        if (FAILED(softFrame_->Lock(&all, WICBitmapLockRead, lock.GetAddressOf())) || FAILED(lock->GetStride(&stride)) ||
            FAILED(lock->GetDataPointer(&size, &pixels))) {
            return false;
        }
        return SUCCEEDED(frame_->CopyFromMemory(nullptr, pixels, stride));
    }

    /// At its first frame: it can play and seek.
    void settled() {
        if (released_ || !broken_.empty() || prepared_) return;
        prepared_ = true;
        if (startAt_ > 0) {
            seekPlayback(startAt_);
            startAt_ = 0;
        } else {
            answer(pendingSeeks_, std::nullopt);
        }
        if (playWanted_ && !suspended_) start();
        reportBuffered();
        controls_->refresh();
    }

    void start() {
        if (engine_ == nullptr) return;
        // Played again from the start once it has ended.
        if (ended_) seekPlayback(0);
        engine_->SetPlaybackRate(rate());
        engine_->Play();
        playing_ = true;
        played_ = true;
        waiting_ = false;
        answer(pendingPlays_, std::nullopt);
        tell(MIGO_VIDEO_EVENT_PLAY, Json::object());
        SetTimer(window_, kTimeTimer, kTimeUpdateMs, nullptr);
        owner_.clock();
        render();
        controls_->refresh();
    }

    /// Paused, as content's call does.
    void pauseNow() {
        if (playing_) {
            halt();
            tell(MIGO_VIDEO_EVENT_PAUSE, Json::object());
        }
        resumeAfterSuspend_ = false;
        controls_->refresh();
    }

    /// Not playing, without telling content.
    void halt() {
        if (playing_ && engine_ != nullptr) engine_->Pause();
        playing_ = false;
        waiting_ = false;
        KillTimer(window_, kTimeTimer);
    }

    void seekPlayback(double seconds) {
        ended_ = false;
        engine_->SetCurrentTime(std::max(0.0, seconds));
    }

    void ended() {
        if (flag("loop", false)) return;
        halt();
        playWanted_ = false;
        ended_ = true;
        tell(MIGO_VIDEO_EVENT_ENDED, Json::object());
        controls_->refresh();
    }

    /// It cannot play this source: content is told once, and its calls fail.
    void report(const std::string &reason) {
        if (released_ || !broken_.empty()) return;
        broken_ = reason;
        failPending(reason);
        halt();
        playWanted_ = false;
        tell(MIGO_VIDEO_EVENT_ERROR, Json::object().set("errMsg", "video:fail " + reason));
        controls_->refresh();
    }

    [[nodiscard]] double position() const override {
        if (!prepared_ || engine_ == nullptr) return 0;
        if (ended_) return duration();
        const double at = engine_->GetCurrentTime();
        return std::isfinite(at) && at > 0 ? at : 0;
    }

    [[nodiscard]] double duration() const override {
        if (!prepared_ || engine_ == nullptr) return 0;
        const double length = engine_->GetDuration();
        return std::isfinite(length) && length > 0 ? length : 0;
    }

    void reportBuffered() {
        if (!prepared_ || engine_ == nullptr) return;
        const double total = duration();
        ComPtr<IMFMediaTimeRange> ranges;
        if (total <= 0 || FAILED(engine_->GetBuffered(ranges.GetAddressOf()))) return;
        const DWORD count = ranges->GetLength();
        double end = 0;
        if (count == 0 || FAILED(ranges->GetEnd(count - 1, &end))) return;
        const int buffered = static_cast<int>(std::clamp(std::lround(end / total * 100), 0L, 100L));
        if (buffered == lastBuffered_) return;
        lastBuffered_ = buffered;
        tell(MIGO_VIDEO_EVENT_PROGRESS, Json::object().set("buffered", buffered).set("duration", total));
    }

    // ---- drawing --------------------------------------------------------------------------------------

    /// The swap chain at `size`, and Direct2D drawing into it.
    void resize(SIZE size) {
        Graphics *graphics = owner_.graphics();
        if (graphics == nullptr || size.cx <= 0 || size.cy <= 0) return;
        if (swapChain_ != nullptr && size.cx == chainSize_.cx && size.cy == chainSize_.cy) return;
        if (dc_ == nullptr &&
            FAILED(graphics->d2d_device->CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE, dc_.GetAddressOf()))) {
            return;
        }
        dc_->SetTarget(nullptr);
        target_.Reset();
        HRESULT result = S_OK;
        if (swapChain_ == nullptr) {
            DXGI_SWAP_CHAIN_DESC1 description{};
            description.Width = static_cast<UINT>(size.cx);
            description.Height = static_cast<UINT>(size.cy);
            description.Format = DXGI_FORMAT_B8G8R8A8_UNORM;
            description.SampleDesc.Count = 1;
            description.BufferUsage = DXGI_USAGE_RENDER_TARGET_OUTPUT;
            description.BufferCount = 2;
            description.Scaling = DXGI_SCALING_STRETCH;
            description.SwapEffect = DXGI_SWAP_EFFECT_FLIP_DISCARD;
            description.AlphaMode = DXGI_ALPHA_MODE_IGNORE;
            result = graphics->dxgi->CreateSwapChainForHwnd(graphics->device.Get(), window_, &description, nullptr,
                                                            nullptr, swapChain_.GetAddressOf());
            if (SUCCEEDED(result)) graphics->dxgi->MakeWindowAssociation(window_, DXGI_MWA_NO_ALT_ENTER);
        } else {
            result = swapChain_->ResizeBuffers(0, static_cast<UINT>(size.cx), static_cast<UINT>(size.cy),
                                               DXGI_FORMAT_UNKNOWN, 0);
        }
        ComPtr<IDXGISurface> back;
        const D2D1_BITMAP_PROPERTIES1 properties =
            D2D1::BitmapProperties1(D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
                                    D2D1::PixelFormat(DXGI_FORMAT_B8G8R8A8_UNORM, D2D1_ALPHA_MODE_IGNORE));
        if (FAILED(result) || FAILED(swapChain_->GetBuffer(0, IID_PPV_ARGS(back.GetAddressOf()))) ||
            FAILED(dc_->CreateBitmapFromDxgiSurface(back.Get(), &properties, target_.GetAddressOf()))) {
            swapChain_.Reset();
            target_.Reset();
            return;
        }
        chainSize_ = size;
        // Its frames, and its poster, are the device context's.
        if (engine_ != nullptr && frame_ == nullptr && prepared_) prepareFrames();
    }

    /// `picture` (of `size`) fitted into `area` as objectFit says.
    D2D1_RECT_F fitted(D2D1_SIZE_F picture, D2D1_RECT_F area, const std::string &fit) const {
        if (fit == "fill" || picture.width <= 0 || picture.height <= 0) return area;
        const float width = area.right - area.left;
        const float height = area.bottom - area.top;
        const float scale = fit == "cover" ? std::max(width / picture.width, height / picture.height)
                                           : std::min(width / picture.width, height / picture.height);
        const float shown_width = picture.width * scale;
        const float shown_height = picture.height * scale;
        const float left = area.left + (width - shown_width) / 2;
        const float top = area.top + (height - shown_height) / 2;
        return D2D1::RectF(left, top, left + shown_width, top + shown_height);
    }

    void render() {
        if (dc_ == nullptr || target_ == nullptr || swapChain_ == nullptr) return;
        dc_->SetTarget(target_.Get());
        dc_->SetDpi(96.0F, 96.0F);
        dc_->BeginDraw();
        // Opaque: a translucent background is drawn over black, as nothing
        // shows through the window.
        D2D1_COLOR_F background = color_of(string("backgroundColor")).value_or(D2D1::ColorF(0, 0, 0, 1));
        background = D2D1::ColorF(background.r * background.a, background.g * background.a, background.b * background.a, 1);
        dc_->Clear(background);
        dc_->SetTransform(D2D1::Matrix3x2F::Translation(static_cast<float>(offset_.x), static_cast<float>(offset_.y)));
        const D2D1_RECT_F area = D2D1::RectF(0, 0, static_cast<float>(content_.cx), static_cast<float>(content_.cy));
        // Its poster stands until it plays.
        if (poster_ != nullptr && !played_) {
            dc_->DrawBitmap(poster_.Get(), fitted(poster_->GetSize(), area, "contain"), 1.0F,
                            D2D1_INTERPOLATION_MODE_LINEAR);
        } else if (haveFrame_ && frame_ != nullptr) {
            dc_->DrawBitmap(frame_.Get(),
                            fitted(D2D1::SizeF(static_cast<float>(videoSize_.cx), static_cast<float>(videoSize_.cy)),
                                   area, string("objectFit")),
                            1.0F, D2D1_INTERPOLATION_MODE_LINEAR);
        }
        dc_->SetTransform(D2D1::Matrix3x2F::Identity());
        const HRESULT drawn = dc_->EndDraw();
        dc_->SetTarget(nullptr);
        if (SUCCEEDED(drawn)) swapChain_->Present(0, 0);
    }

    // ---- the poster -----------------------------------------------------------------------------------

    void stopPoster() {
        if (posterDownloads_ != nullptr) posterDownloads_->stop();
        if (posterLoad_.joinable()) posterLoad_.join();
        posterDownloads_.reset();
    }

    void loadPoster() {
        const int generation = ++posterGeneration_;
        stopPoster();
        poster_.Reset();
        render();
        const std::string poster = string("poster");
        if (poster.empty()) return;
        auto downloads = std::make_shared<Downloads>();
        posterDownloads_ = downloads;
        auto alive = alive_;
        posterLoad_ = std::thread([this, alive, generation, poster, downloads, post = owner_.post()] {
            const Apartment apartment;
            ComPtr<IWICImagingFactory> wic = codec::factory();
            if (wic == nullptr) return;
            std::string error;
            std::optional<codec::Opened> image;
            HANDLE held = INVALID_HANDLE_VALUE;
            if (isRemote(poster)) {
                // Downloaded into a file removed as it closes.
                wchar_t directory[MAX_PATH + 1] = {};
                wchar_t name[MAX_PATH + 1] = {};
                if (GetTempPathW(MAX_PATH + 1, directory) == 0 ||
                    GetTempFileNameW(directory, L"mgv", 0, name) == 0) {
                    return;
                }
                held = CreateFileW(name, GENERIC_READ | GENERIC_WRITE, FILE_SHARE_READ | FILE_SHARE_DELETE, nullptr,
                                   CREATE_ALWAYS, FILE_ATTRIBUTE_TEMPORARY | FILE_FLAG_DELETE_ON_CLOSE, nullptr);
                if (held == INVALID_HANDLE_VALUE) return;
                if (download(*downloads, wideOf(poster), held, kMaxPosterBytes).empty()) {
                    image = codec::open(*wic.Get(), held, error);
                }
            } else {
                image = codec::open(*wic.Get(), wideOf(poster.rfind("file://", 0) == 0 ? poster.substr(7) : poster), error);
            }
            ComPtr<IWICBitmap> decoded;
            if (image.has_value()) {
                ComPtr<IWICBitmapSource> upright =
                    codec::upright(*wic.Get(), *image, image->uprightWidth(), image->uprightHeight());
                ComPtr<IWICFormatConverter> converted;
                if (upright != nullptr && SUCCEEDED(wic->CreateFormatConverter(converted.GetAddressOf())) &&
                    SUCCEEDED(converted->Initialize(upright.Get(), GUID_WICPixelFormat32bppPBGRA, WICBitmapDitherTypeNone,
                                                    nullptr, 0.0, WICBitmapPaletteTypeCustom))) {
                    wic->CreateBitmapFromSource(converted.Get(), WICBitmapCacheOnLoad, decoded.GetAddressOf());
                }
            }
            if (held != INVALID_HANDLE_VALUE) CloseHandle(held);
            if (decoded == nullptr) return;
            post([this, alive, generation, decoded] {
                if (!*alive || released_ || generation != posterGeneration_ || dc_ == nullptr) return;
                poster_.Reset();
                dc_->CreateBitmapFromWicBitmap(decoded.Get(), nullptr, poster_.GetAddressOf());
                render();
            });
        });
    }

    // ---- answers --------------------------------------------------------------------------------------

    /// Done when `failure` is empty, failed with it otherwise.
    static void answer(std::vector<HostServiceReply> &replies, const std::optional<std::string> &failure) {
        std::vector<HostServiceReply> taken = std::move(replies);
        replies.clear();
        for (HostServiceReply &reply : taken) {
            if (failure.has_value()) {
                (void)reply.fail(*failure);
            } else {
                (void)reply.succeed();
            }
        }
    }

    void failPending(const std::string &reason) {
        answer(pendingPlays_, reason);
        answer(pendingSeeks_, reason);
    }

    // ---- options --------------------------------------------------------------------------------------

    [[nodiscard]] double rate() const {
        const double value = number("playbackRate", 1);
        return value > 0 ? value : 1;
    }
    [[nodiscard]] std::string string(const char *key) const { return options_[key].string(); }
    [[nodiscard]] double number(const char *key, double fallback) const {
        const double value = options_[key].numberOr(fallback);
        return std::isfinite(value) ? value : fallback;
    }
    [[nodiscard]] bool flag(const char *key, bool fallback) const { return options_[key].boolOr(fallback); }

    /// What it did, to whoever hears it.
    void tell(std::uint32_t event, Json payload) {
        if (stage_.tell) stage_.tell(event, std::move(payload));
    }

    Impl &owner_;
    VideoStage stage_;
    Json options_;
    const std::shared_ptr<bool> alive_ = std::make_shared<bool>(true);
    HWND window_ = nullptr;
    std::unique_ptr<VideoControls> controls_;

    ComPtr<IMFMediaEngine> engine_;
    std::uint64_t engineGeneration_ = 0;
    ComPtr<IDXGISwapChain1> swapChain_;
    SIZE chainSize_{0, 0};
    ComPtr<ID2D1DeviceContext> dc_;
    ComPtr<ID2D1Bitmap1> target_;
    ComPtr<ID3D11Texture2D> texture_;
    ComPtr<IWICBitmap> softFrame_;
    ComPtr<ID2D1Bitmap1> frame_;
    SIZE videoSize_{0, 0};
    bool haveFrame_ = false;
    ComPtr<ID2D1Bitmap> poster_;
    int posterGeneration_ = 0;
    std::shared_ptr<Downloads> posterDownloads_;
    std::thread posterLoad_;
    SIZE content_{0, 0};
    POINT offset_{0, 0};

    /// Calls waiting to be answered: plays until it plays, seeks until it is there.
    std::vector<HostServiceReply> pendingPlays_;
    std::vector<HostServiceReply> pendingSeeks_;
    bool prepared_ = false;
    bool playing_ = false;
    bool played_ = false;
    bool playWanted_ = false;
    double startAt_ = 0;
    bool ended_ = false;
    bool suspended_ = false;
    bool resumeAfterSuspend_ = false;
    bool waiting_ = false;
    int lastBuffered_ = -1;
    bool fullScreen_ = false;
    bool released_ = false;
    /// Why this source cannot play, once it has failed.
    std::string broken_;

public:
    // The destructor runs `release`, which reads `alive_`; it goes last.
    void forget() { *alive_ = false; }
};

// ---- the players -----------------------------------------------------------------------------------

Videos::Impl::~Impl() {
    stopClock();
    // The game ended: every player goes, and its calls still unanswered fail.
    for (auto &[id, player] : players_) player->forget();
    players_.clear();
    for (auto &[raw, player] : embedded_) player->forget();
    embedded_.clear();
    if (watching_view_ && view_.hwnd() != nullptr) RemoveWindowSubclass(view_.hwnd(), &Impl::watch, kWatchId);
    if (watching_root_) RemoveWindowSubclass(root_, &Impl::watch, kWatchId);
}

void Videos::Impl::call(std::uint32_t method, const Json &request, HostServiceReply reply) {
    const auto id = static_cast<std::int64_t>(request["videoId"].numberOr(0));
    const auto found = players_.find(id);
    Player *player = found == players_.end() ? nullptr : found->second.get();
    switch (method) {
        case MIGO_VIDEO_CREATE: {
            if (player != nullptr) return;
            VideoStage stage;
            stage.owner = root_;
            stage.reference = view_.hwnd();
            stage.scale = [this] { return view_.scale(); };
            stage.tell = [this, id](std::uint32_t event, Json payload) { tell(id, event, std::move(payload)); };
            auto made = std::make_unique<Player>(*this, std::move(stage), request);
            if (!running_) made->suspend();
            players_.emplace(id, std::move(made));
            startClock();
            return;
        }
        case MIGO_VIDEO_DESTROY:
            if (player != nullptr) {
                player->forget();
                players_.erase(found);
            }
            if (players_.empty() && embedded_.empty()) stopClock();
            return;
        case MIGO_VIDEO_SET:
            if (player != nullptr) player->set(request);
            return;
        default:
            break;
    }
    if (player == nullptr) {
        (void)reply.fail(kDestroyed);
        return;
    }
    switch (method) {
        case MIGO_VIDEO_PLAY:
            player->play(std::move(reply));
            return;
        case MIGO_VIDEO_PAUSE:
            player->pause(std::move(reply));
            return;
        case MIGO_VIDEO_STOP:
            player->stop(std::move(reply));
            return;
        case MIGO_VIDEO_SEEK:
            player->seek(request["time"].numberOr(0), std::move(reply));
            return;
        case MIGO_VIDEO_REQUEST_FULL_SCREEN:
            player->fullScreen(true, std::move(reply));
            return;
        case MIGO_VIDEO_EXIT_FULL_SCREEN:
            player->fullScreen(false, std::move(reply));
            return;
        default:
            (void)reply.fail("not supported");
            return;
    }
}

void Videos::Impl::setRunning(bool running) {
    if (running == running_) return;
    running_ = running;
    for (auto &[id, player] : players_) {
        if (running) {
            player->resume();
        } else {
            player->suspend();
        }
    }
}

void Videos::Impl::clock() { startClock(); }

void Videos::Impl::startClock() {
    if (clocking_.exchange(true)) return;
    if (clock_.joinable()) clock_.join();
    clock_ = std::thread([this] {
        while (clocking_.load()) {
            // The compositor's beat; where there is none, a display's.
            if (FAILED(DwmFlush())) Sleep(16);
            if (!beat_pending_.exchange(true)) {
                post_([this] {
                    beat_pending_.store(false);
                    beat();
                });
            }
        }
    });
}

void Videos::Impl::stopClock() {
    clocking_.store(false);
    if (clock_.joinable()) clock_.join();
}

void Videos::Impl::beat() {
    for (auto &[id, player] : players_) player->beat();
    for (auto &[raw, player] : embedded_) player->beat();
}

/// A video the Host Kit shows in a window of its own: a player there, its
/// place what the window says.
class Videos::Impl::Embedded final : public EmbeddedVideo {
public:
    Embedded(Impl &videos, Player &player) : videos_(videos), player_(player) {}
    ~Embedded() override { videos_.unembed(&player_); }
    void place(int left, int top, int width, int height) override {
        player_.placeAt(left, top, width, height);
    }

private:
    Impl &videos_;
    Player &player_;
};

std::unique_ptr<EmbeddedVideo> Videos::Impl::embed(HWND window, HANDLE file, const std::string &url,
                                                   const std::string &poster) {
    if (graphics() == nullptr) {
        if (file != INVALID_HANDLE_VALUE) CloseHandle(file);
        return nullptr;
    }
    VideoStage stage;
    stage.owner = window;
    stage.reference = window;
    stage.scale = [window] {
        return static_cast<float>(std::max<UINT>(GetDpiForWindow(window), USER_DEFAULT_SCREEN_DPI / 4)) /
               static_cast<float>(USER_DEFAULT_SCREEN_DPI);
    };
    stage.file = file;
    // Placed by its window as it lays out; not until then.
    Json options = Json::object().set("src", url).set("poster", poster).set("width", 0).set("height", 0);
    auto made = std::make_unique<Player>(*this, std::move(stage), std::move(options));
    Player *raw = made.get();
    embedded_.emplace(raw, std::move(made));
    startClock();
    return std::make_unique<Embedded>(*this, *raw);
}

void Videos::Impl::unembed(Player *player) {
    const auto found = embedded_.find(player);
    if (found == embedded_.end()) return;
    found->second->forget();
    embedded_.erase(found);
    if (players_.empty() && embedded_.empty()) stopClock();
}


void Videos::Impl::placeAll() {
    for (auto &[id, player] : players_) player->place();
}

LRESULT CALLBACK Videos::Impl::playerProcedure(HWND window, UINT message, WPARAM wparam, LPARAM lparam) {
    if (message == WM_NCCREATE) {
        const auto *create = reinterpret_cast<const CREATESTRUCTW *>(lparam);
        SetWindowLongPtrW(window, GWLP_USERDATA, reinterpret_cast<LONG_PTR>(create->lpCreateParams));
    }
    auto *player = reinterpret_cast<Player *>(GetWindowLongPtrW(window, GWLP_USERDATA));
    switch (message) {
        case WM_TIMER:
            if (player != nullptr && wparam == kTimeTimer) player->tick();
            return 0;
        case WM_NCHITTEST:
            // With none of its controls asked for, a click on the video is the game's.
            return player != nullptr && player->takesInput() ? HTCLIENT : HTTRANSPARENT;
        case WM_LBUTTONDOWN:
        case WM_LBUTTONUP:
        case WM_LBUTTONDBLCLK:
        case WM_MOUSEMOVE: {
            // A press is followed wherever the mouse goes until its release.
            if (message == WM_LBUTTONDOWN) SetCapture(window);
            if (message == WM_LBUTTONUP && GetCapture() == window) ReleaseCapture();
            if (player != nullptr && player->mouse(message, wparam, lparam)) return 0;
            return DefWindowProcW(window, message, wparam, lparam);
        }
        case WM_MOUSEACTIVATE:
            return MA_NOACTIVATE;
        case WM_ERASEBKGND:
            return 1;
        default:
            return DefWindowProcW(window, message, wparam, lparam);
    }
}

LRESULT CALLBACK Videos::Impl::watch(HWND window, UINT message, WPARAM wparam, LPARAM lparam, UINT_PTR,
                                     DWORD_PTR reference) {
    const LRESULT result = DefSubclassProc(window, message, wparam, lparam);
    auto *impl = reinterpret_cast<Impl *>(reference);
    switch (message) {
        case WM_WINDOWPOSCHANGED:
        case WM_SHOWWINDOW:
            impl->placeAll();
            break;
        case WM_NCDESTROY:
            if (window == impl->root_) {
                impl->watching_root_ = false;
            } else {
                impl->watching_view_ = false;
            }
            RemoveWindowSubclass(window, &Impl::watch, kWatchId);
            break;
        default:
            break;
    }
    return result;
}

Videos::Videos(Win32SurfaceView &view, std::shared_ptr<HostServiceChannel> channel)
    : impl_(std::make_unique<Impl>(view, std::move(channel))) {}
Videos::~Videos() = default;
void Videos::call(std::uint32_t method, const Json &request, HostServiceReply reply) {
    impl_->call(method, request, std::move(reply));
}
void Videos::setRunning(bool running) { impl_->setRunning(running); }
std::unique_ptr<EmbeddedVideo> Videos::embed(HWND__ *window, void *file, const std::string &url,
                                             const std::string &poster) {
    return impl_->embed(window, static_cast<HANDLE>(file), url, poster);
}

}  // namespace migo::windows_host

// A Windows host built on the Windows Host Kit's Managed shape: one window, one game.
//
// It is what migo-conformance's Windows Host Kit lane runs, and it is written as
// an App would write it -- Win32ManagedSession owns the Session, the view and the
// built-in host services -- so that what the lane verifies is the platform's own
// host layer, not a host made for the test. The C host beside it
// (tests/c_host/windows) is the other shape: the public C ABI with nothing on top.
//
//   migo-win32-host FILES_DIR CONTENT_ID SECONDS
//
// The game is the installed content CONTENT_ID under FILES_DIR, run for SECONDS
// or until it asks to exit. Its settings come as the C host's do, from files
// named by the environment: MIGO_WIN32_HOST_NETWORK_POLICY, the network (none at
// all without it), and MIGO_WIN32_HOST_LAUNCH_OPTIONS, what the game is launched
// with.
//
// The player is scripted, as the Linux Qt host's is: MIGO_WIN32_HOST_FEED names
// a file that whoever runs the host appends lines to while it runs, and
//
//   TAP-TEXT <text>    the player clicks the button that says <text>
//   TYPE-TEXT <text>   the player types <text> into the field that holds the focus
//   PRESS-ENTER        the player presses Enter there
//   PICK-NEWEST        the player picks the newest of their files -- those in their
//                      Pictures folder, which MIGO_WIN32_HOST_PICTURES names for a
//                      run (the Host Kit's photo album too) -- of the kind the file
//                      dialog is showing, and opens it
//   CANCEL-PICK        the player cancels that dialog
//   SAVE-FILE          the player saves, under the name the dialog offers, in the
//                      folder MIGO_WIN32_HOST_SAVE_DIR names
//   CLOSE-PREVIEW      the player closes the Host Kit's viewer, with Esc
//   POINTER-OVER-GAME  the player is at the game: its window in front, the game
//                      holding the keyboard, the cursor in its middle
//   MOVE-MOUSE <dx> <dy>  the player moves the mouse by that many pixels
//   PRESS-ESCAPE       the player presses Esc
//
// are done in that order, each once it can be -- a dialog may not be up yet when
// its line arrives, nor the keyboard's field have taken the focus. Clicks are the
// buttons' own (BM_CLICK), keys the messages a keyboard sends the focused field,
// and the mouse and Esc the system's input (SendInput): a pointer lock goes by
// the mouse's raw motion, which no message sent to a window carries.

#include <migo/migo.h>
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
#include <shlobj.h>
#include <shlwapi.h>

#include <algorithm>
#include <cstdio>
#include <cstdlib>
#include <deque>
#include <filesystem>
#include <fstream>
#include <optional>
#include <string>
#include <vector>

using migo::windows_host::Win32ManagedSession;

namespace {

namespace fs = std::filesystem;

constexpr wchar_t kFrameClass[] = L"MigoWin32Host";
// A phone's shape, as the Linux Qt host's 720 x 1280, fitted to the screen.
constexpr int kAspectWidth = 9;
constexpr int kAspectHeight = 16;
constexpr UINT_PTR kPlayerTimer = 1;
constexpr UINT_PTR kEndTimer = 2;
// The file dialog's own controls: its file name box and its file type list.
constexpr int kFileNameControl = 1148;
constexpr int kFileTypeControl = 1136;

std::string utf8_of(std::wstring_view wide) {
    if (wide.empty()) return {};
    const int bytes = WideCharToMultiByte(CP_UTF8, 0, wide.data(), static_cast<int>(wide.size()), nullptr, 0, nullptr,
                                          nullptr);
    std::string out(static_cast<std::size_t>(std::max(bytes, 0)), '\0');
    WideCharToMultiByte(CP_UTF8, 0, wide.data(), static_cast<int>(wide.size()), out.data(), bytes, nullptr, nullptr);
    return out;
}

std::wstring wide_of(std::string_view utf8) {
    if (utf8.empty()) return {};
    const int units = MultiByteToWideChar(CP_UTF8, 0, utf8.data(), static_cast<int>(utf8.size()), nullptr, 0);
    std::wstring out(static_cast<std::size_t>(std::max(units, 0)), L'\0');
    MultiByteToWideChar(CP_UTF8, 0, utf8.data(), static_cast<int>(utf8.size()), out.data(), units);
    return out;
}

/// The environment variable `name`, or empty when it is not set.
std::wstring env(const wchar_t *name) {
    const DWORD needed = GetEnvironmentVariableW(name, nullptr, 0);
    if (needed == 0) return {};
    std::wstring value(needed, L'\0');
    const DWORD length = GetEnvironmentVariableW(name, value.data(), needed);
    value.resize(length < needed ? length : 0);
    return value;
}

/// A setting's JSON, read whole from the file the environment variable `name`
/// names, or empty when it names none. A file that cannot be read stops the
/// host: running without the setting would fail every check that needs it for a
/// reason no check names.
std::string read_setting(const wchar_t *name) {
    const std::wstring path = env(name);
    if (path.empty()) return {};
    std::ifstream file(fs::path(path), std::ios::binary);
    if (!file) {
        std::fprintf(stderr, "[win32-host] cannot read %ls=%ls\n", name, path.c_str());
        std::exit(2);
    }
    return std::string(std::istreambuf_iterator<char>(file), std::istreambuf_iterator<char>());
}

std::wstring text_of(HWND window) {
    wchar_t text[1024] = {};
    GetWindowTextW(window, text, 1024);
    return text;
}

std::wstring class_of(HWND window) {
    wchar_t name[128] = {};
    GetClassNameW(window, name, 128);
    return name;
}

/// The visible top-level windows of this process.
std::vector<HWND> our_windows() {
    std::vector<HWND> windows;
    EnumWindows(
        [](HWND window, LPARAM found) -> BOOL {
            DWORD process = 0;
            GetWindowThreadProcessId(window, &process);
            if (process == GetCurrentProcessId() && IsWindowVisible(window)) {
                reinterpret_cast<std::vector<HWND> *>(found)->push_back(window);
            }
            return TRUE;
        },
        reinterpret_cast<LPARAM>(&windows));
    return windows;
}

/// Why the request at hand cannot be done yet, for the line the player prints
/// when one has waited long.
std::string g_waiting_for;

/// Clicks the button that says `text`, if any window of this App shows one.
bool tap(const std::wstring &text) {
    for (HWND window : our_windows()) {
        struct Search {
            const std::wstring *text;
            HWND found;
        } search{&text, nullptr};
        EnumChildWindows(
            window,
            [](HWND child, LPARAM context) -> BOOL {
                auto *search = reinterpret_cast<Search *>(context);
                if (_wcsicmp(class_of(child).c_str(), L"Button") == 0 && IsWindowVisible(child) && IsWindowEnabled(child) &&
                    text_of(child) == *search->text) {
                    search->found = child;
                    return FALSE;
                }
                return TRUE;
            },
            reinterpret_cast<LPARAM>(&search));
        if (search.found != nullptr) {
            SetForegroundWindow(window);
            SendMessageW(search.found, BM_CLICK, 0, 0);
            return true;
        }
    }
    g_waiting_for = "no button says " + utf8_of(text);
    return false;
}

/// The field the keyboard's keys go to, if one holds the focus.
HWND focused_field() {
    HWND focus = GetFocus();
    if (focus == nullptr || !IsWindowVisible(focus) || _wcsicmp(class_of(focus).c_str(), L"Edit") != 0) {
        g_waiting_for = "no field holds the focus";
        return nullptr;
    }
    return focus;
}

bool type(const std::wstring &text) {
    HWND field = focused_field();
    if (field == nullptr) return false;
    for (wchar_t unit : text) SendMessageW(field, WM_CHAR, unit, 1);
    return true;
}

/// Enter, posted as a keyboard posts it: a dialog's navigation sees it on its way.
bool enter() {
    HWND field = focused_field();
    if (field == nullptr) return false;
    PostMessageW(field, WM_KEYDOWN, VK_RETURN, 1);
    PostMessageW(field, WM_KEYUP, VK_RETURN, 0xC0000001);
    return true;
}

/// The system's file dialog the Host Kit shows -- on a thread of its own, so
/// not this one's -- if one is up.
HWND file_dialog(HWND frame) {
    for (HWND window : our_windows()) {
        if (class_of(window) == L"#32770" && GetWindow(window, GW_OWNER) == frame &&
            GetWindowThreadProcessId(window, nullptr) != GetCurrentThreadId()) {
            return window;
        }
    }
    g_waiting_for = "no file dialog is open";
    return nullptr;
}

/// The dialog's file name box: the Edit inside its control 1148 where it has
/// one -- an open dialog does -- else the field that holds the dialog's
/// keyboard as it opens, which a save dialog gives its name, else the first
/// field it shows.
HWND file_name_box(HWND dialog) {
    if (HWND box = GetDlgItem(dialog, kFileNameControl); box != nullptr) {
        if (HWND edit = FindWindowExW(box, nullptr, L"Edit", nullptr); edit != nullptr) return edit;
        HWND combo = FindWindowExW(box, nullptr, L"ComboBox", nullptr);
        return combo != nullptr ? FindWindowExW(combo, nullptr, L"Edit", nullptr) : box;
    }
    GUITHREADINFO focus{};
    focus.cbSize = sizeof focus;
    if (GetGUIThreadInfo(GetWindowThreadProcessId(dialog, nullptr), &focus) && focus.hwndFocus != nullptr &&
        IsChild(dialog, focus.hwndFocus) && _wcsicmp(class_of(focus.hwndFocus).c_str(), L"Edit") == 0) {
        return focus.hwndFocus;
    }
    HWND found = nullptr;
    EnumChildWindows(
        dialog,
        [](HWND child, LPARAM out) -> BOOL {
            if (_wcsicmp(class_of(child).c_str(), L"Edit") == 0 && IsWindowVisible(child)) {
                *reinterpret_cast<HWND *>(out) = child;
                return FALSE;
            }
            return TRUE;
        },
        reinterpret_cast<LPARAM>(&found));
    return found;
}

std::wstring perceived_kind(const fs::path &file) {
    PERCEIVED perceived = PERCEIVED_TYPE_UNSPECIFIED;
    PERCEIVEDFLAG flags = 0;
    if (FAILED(AssocGetPerceivedType(file.extension().c_str(), &perceived, &flags, nullptr))) return L"file";
    return perceived == PERCEIVED_TYPE_IMAGE ? L"image" : perceived == PERCEIVED_TYPE_VIDEO ? L"video" : L"file";
}

/// Whether `file` is of the kind the dialog's file type shows: its label --
/// "Images", "Videos", "Images and videos" -- or the patterns it names, as
/// "Files (*.txt;*.pdf)" does; anything, where it shows no type.
bool shown_by(const std::wstring &type, const fs::path &file) {
    const std::wstring kind = perceived_kind(file);
    if (type.rfind(L"Images and videos", 0) == 0) return kind == L"image" || kind == L"video";
    if (type.rfind(L"Images", 0) == 0) return kind == L"image";
    if (type.rfind(L"Videos", 0) == 0) return kind == L"video";
    const std::size_t from = type.find(L'(');
    const std::size_t to = type.rfind(L')');
    if (from == std::wstring::npos || to == std::wstring::npos || to < from) return true;
    const std::wstring patterns = type.substr(from + 1, to - from - 1);
    std::size_t start = 0;
    while (start <= patterns.size()) {
        std::size_t end = patterns.find(L';', start);
        if (end == std::wstring::npos) end = patterns.size();
        const std::wstring pattern = patterns.substr(start, end - start);
        if (!pattern.empty() && PathMatchSpecW(file.filename().c_str(), pattern.c_str())) return true;
        start = end + 1;
    }
    return false;
}

/// The player's Pictures folder: the run's own where it names one.
fs::path pictures() {
    if (const std::wstring named = env(L"MIGO_WIN32_HOST_PICTURES"); !named.empty()) return fs::path(named);
    PWSTR path = nullptr;
    fs::path found;
    if (SUCCEEDED(SHGetKnownFolderPath(FOLDERID_Pictures, 0, nullptr, &path)) && path != nullptr) found = path;
    CoTaskMemFree(path);
    return found;
}

bool pick_newest(HWND frame) {
    HWND dialog = file_dialog(frame);
    if (dialog == nullptr) return false;
    HWND name = file_name_box(dialog);
    if (name == nullptr) {
        g_waiting_for = "the file dialog shows no file name box: it is not an open dialog";
        return false;
    }
    HWND type = GetDlgItem(dialog, kFileTypeControl);
    const std::wstring shown = type != nullptr ? text_of(type) : std::wstring();
    std::optional<fs::path> newest;
    fs::file_time_type newest_time{};
    std::error_code error;
    for (const auto &entry : fs::directory_iterator(pictures(), error)) {
        if (!entry.is_regular_file(error) || !shown_by(shown, entry.path())) continue;
        const fs::file_time_type written = entry.last_write_time(error);
        if (!newest.has_value() || written > newest_time) {
            newest = entry.path();
            newest_time = written;
        }
    }
    if (!newest.has_value()) {
        g_waiting_for = utf8_of(pictures().wstring()) + " holds no file the dialog's type \"" + utf8_of(shown) + "\" shows";
        return false;
    }
    SetWindowTextW(name, newest->c_str());
    PostMessageW(dialog, WM_COMMAND, MAKEWPARAM(IDOK, BN_CLICKED), 0);
    return true;
}

bool cancel_pick(HWND frame) {
    HWND dialog = file_dialog(frame);
    if (dialog == nullptr) return false;
    PostMessageW(dialog, WM_COMMAND, MAKEWPARAM(IDCANCEL, BN_CLICKED), 0);
    return true;
}

/// Saves under the name the dialog offers, in the folder the environment names:
/// the player's choice of where, which a run makes anew each time.
bool save_file(HWND frame) {
    HWND dialog = file_dialog(frame);
    if (dialog == nullptr) return false;
    HWND name = file_name_box(dialog);
    const std::wstring folder = env(L"MIGO_WIN32_HOST_SAVE_DIR");
    const std::wstring offered = name != nullptr ? text_of(name) : std::wstring();
    if (name == nullptr || folder.empty() || offered.empty()) {
        g_waiting_for = "the save dialog offers no name, or MIGO_WIN32_HOST_SAVE_DIR names no folder";
        return false;
    }
    SetWindowTextW(name, (fs::path(folder) / fs::path(offered).filename()).c_str());
    PostMessageW(dialog, WM_COMMAND, MAKEWPARAM(IDOK, BN_CLICKED), 0);
    return true;
}

bool close_preview() {
    for (HWND window : our_windows()) {
        if (class_of(window) != L"MigoMediaViewer") continue;
        PostMessageW(window, WM_KEYDOWN, VK_ESCAPE, 1);
        return true;
    }
    g_waiting_for = "no viewer is up";
    return false;
}

void send_key(WORD key) {
    INPUT presses[2] = {};
    presses[0].type = INPUT_KEYBOARD;
    presses[0].ki.wVk = key;
    presses[1] = presses[0];
    presses[1].ki.dwFlags = KEYEVENTF_KEYUP;
    SendInput(2, presses, sizeof(INPUT));
}

/// The scripted player: reads the feed as it grows, and keeps each request, in
/// order, until it can be done.
class Player final {
public:
    Player(const std::wstring &path, HWND frame, Win32ManagedSession &game) : frame_(frame), game_(game) {
        if (!path.empty()) path_ = path;
    }

    /// One step, every 50 ms.
    void step() {
        if (path_.empty()) return;
        read();
        while (!requests_.empty() && perform(requests_.front())) {
            requests_.pop_front();
            waited_ = 0;
            g_waiting_for.clear();
        }
        // A request that has waited five seconds says so, once: a bundle that then
        // times out says only that nothing happened.
        if (!requests_.empty() && ++waited_ == 100) {
            std::fprintf(stderr, "[win32-host] the player's request \"%s\" still waits: %s\n",
                         utf8_of(requests_.front()).c_str(),
                         g_waiting_for.empty() ? "what it acts on is not up" : g_waiting_for.c_str());
        }
    }

private:
    void read() {
        std::ifstream feed(path_, std::ios::binary);
        if (!feed) return;
        feed.seekg(static_cast<std::streamoff>(offset_));
        std::string arrived((std::istreambuf_iterator<char>(feed)), std::istreambuf_iterator<char>());
        offset_ += arrived.size();
        buffered_ += arrived;
        // A line is read only once it is whole: its writer may be part way through it.
        for (std::size_t end; (end = buffered_.find('\n')) != std::string::npos;) {
            std::string line = buffered_.substr(0, end);
            buffered_.erase(0, end + 1);
            if (!line.empty() && line.back() == '\r') line.pop_back();
            if (!line.empty()) requests_.push_back(wide_of(line));
        }
    }

    bool perform(const std::wstring &line) {
        const auto starts = [&line](const wchar_t *prefix) { return line.rfind(prefix, 0) == 0; };
        if (starts(L"TAP-TEXT ")) return tap(line.substr(9));
        if (starts(L"TYPE-TEXT ")) return type(line.substr(10));
        if (line == L"PRESS-ENTER") return enter();
        if (line == L"PICK-NEWEST") return pick_newest(frame_);
        if (line == L"CANCEL-PICK") return cancel_pick(frame_);
        if (line == L"SAVE-FILE") return save_file(frame_);
        if (line == L"CLOSE-PREVIEW") return close_preview();
        if (line == L"POINTER-OVER-GAME") return pointerOverGame();
        if (starts(L"MOVE-MOUSE ")) {
            wchar_t *after_dx = nullptr;
            wchar_t *after_dy = nullptr;
            const long dx = std::wcstol(line.c_str() + 11, &after_dx, 10);
            const long dy = std::wcstol(after_dx, &after_dy, 10);
            if (after_dx != line.c_str() + 11 && after_dy != after_dx) {
                INPUT move = {};
                move.type = INPUT_MOUSE;
                move.mi.dx = dx;
                move.mi.dy = dy;
                move.mi.dwFlags = MOUSEEVENTF_MOVE;
                SendInput(1, &move, sizeof move);
            }
            return true;
        }
        if (line == L"PRESS-ESCAPE") {
            send_key(VK_ESCAPE);
            return true;
        }
        std::fprintf(stderr, "[win32-host] the player has no request \"%s\"\n", utf8_of(line).c_str());
        return true;
    }

    /// The player at the game, which is where a pointer lock may be taken;
    /// asked again until the window is in front and the game holds the keyboard.
    bool pointerOverGame() {
        HWND view = game_.view() != nullptr ? game_.view()->hwnd() : nullptr;
        if (view == nullptr || !IsWindowVisible(view)) return false;
        SetForegroundWindow(frame_);
        SetFocus(view);
        RECT client{};
        GetClientRect(view, &client);
        POINT middle{client.right / 2, client.bottom / 2};
        ClientToScreen(view, &middle);
        SetCursorPos(middle.x, middle.y);
        if (GetForegroundWindow() != frame_ || GetFocus() != view) {
            g_waiting_for = "the game's window is not in front, or the game does not hold the keyboard";
            return false;
        }
        return true;
    }

    const HWND frame_;
    Win32ManagedSession &game_;
    fs::path path_;
    std::uintmax_t offset_ = 0;
    std::string buffered_;
    std::deque<std::wstring> requests_;
    int waited_ = 0;
};

/// What the frame's procedure reaches.
struct Host {
    Win32ManagedSession *game = nullptr;
    Player *player = nullptr;
};
Host g_host;

LRESULT CALLBACK frame_procedure(HWND window, UINT message, WPARAM wparam, LPARAM lparam) {
    switch (message) {
        case WM_SIZE:
            // The game fills the window.
            if (g_host.game != nullptr && g_host.game->view() != nullptr) {
                MoveWindow(g_host.game->view()->hwnd(), 0, 0, LOWORD(lparam), HIWORD(lparam), TRUE);
            }
            return 0;
        case WM_TIMER:
            if (wparam == kPlayerTimer && g_host.player != nullptr) g_host.player->step();
            if (wparam == kEndTimer && g_host.game != nullptr) {
                KillTimer(window, kEndTimer);
                (void)g_host.game->close();
            }
            return 0;
        case WM_CLOSE:
            // Closing the window ends the game the way every other ending does:
            // through close(), and the host quits once the Session is gone.
            if (g_host.game != nullptr) (void)g_host.game->close();
            return 0;
        default:
            return DefWindowProcW(window, message, wparam, lparam);
    }
}

}  // namespace

int wmain(int argc, wchar_t **argv) {
    SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    const std::string files_dir = utf8_of(argc > 1 ? argv[1] : L"C:\\migo-win32-host\\files");
    const std::string content_id = utf8_of(argc > 2 ? argv[2] : L"win32-host-demo");
    const int seconds = argc > 3 ? _wtoi(argv[3]) : 10;

    // ---- the engine: the host names every storage root ----
    const std::string cache_dir = files_dir + "\\..\\cache";
    const std::string code_cache_dir = files_dir + "\\..\\code-cache";
    MigoEngineConfig engine_config{};
    engine_config.struct_size = static_cast<std::uint32_t>(sizeof engine_config);
    engine_config.abi_version = MIGO_ABI_VERSION_CURRENT;
    // Development host: the deployed game carries no signing receipt.
    engine_config.flags = MIGO_ENGINE_FLAG_ALLOW_UNSIGNED_CONTENT;
    engine_config.files_dir_utf8 = files_dir.c_str();
    engine_config.cache_dir_utf8 = cache_dir.c_str();
    engine_config.code_cache_dir_utf8 = code_cache_dir.c_str();
    MigoEngine *engine = nullptr;
    if (const MigoResult created = migo_engine_create(&engine_config, &engine); created != MIGO_OK) {
        std::fprintf(stderr, "[win32-host] migo_engine_create failed: %d\n", static_cast<int>(created));
        return 1;
    }

    // ---- the window: a phone's shape, as tall as the screen's work area lets it be ----
    WNDCLASSEXW frame_class{};
    frame_class.cbSize = sizeof frame_class;
    frame_class.lpfnWndProc = frame_procedure;
    frame_class.hInstance = GetModuleHandleW(nullptr);
    frame_class.hCursor = LoadCursorW(nullptr, IDC_ARROW);
    frame_class.lpszClassName = kFrameClass;
    RegisterClassExW(&frame_class);
    const DWORD style = WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN;
    HWND frame = CreateWindowExW(0, kFrameClass, L"migo-win32-host", style, CW_USEDEFAULT, CW_USEDEFAULT, 400, 700,
                                 nullptr, nullptr, GetModuleHandleW(nullptr), nullptr);
    RECT work{};
    SystemParametersInfoW(SPI_GETWORKAREA, 0, &work, 0);
    RECT outer{0, 0, 0, 0};
    AdjustWindowRectExForDpi(&outer, style, FALSE, 0, GetDpiForWindow(frame));
    const LONG height = (work.bottom - work.top) - (outer.bottom - outer.top);
    const LONG width = height * kAspectWidth / kAspectHeight;
    SetWindowPos(frame, nullptr, work.left, work.top, width + (outer.right - outer.left),
                 height + (outer.bottom - outer.top), SWP_NOZORDER);

    // ---- the game ----
    Win32ManagedSession::Options options;
    options.networkPolicyJson = read_setting(L"MIGO_WIN32_HOST_NETWORK_POLICY");
    options.launchOptionsJson = read_setting(L"MIGO_WIN32_HOST_LAUNCH_OPTIONS");
    // Who the host is, as content reads it (getAppBaseInfo().host.appId).
    options.appId = "com.migo.example.windows";
    // The player's Pictures folder is the photo album the Host Kit saves to and picks from.
    options.files.album = pictures();
    Win32ManagedSession::Listener listener;
    listener.contentReady = [] {
        std::printf("[win32-host] content is ready\n");
        std::fflush(stdout);
    };
    listener.contentFailed = [](MigoResult code, const std::string &message) {
        std::fprintf(stderr, "[win32-host] content failed %d: %s\n", static_cast<int>(code), message.c_str());
    };
    listener.migoError = [](MigoResult code) {
        std::fprintf(stderr, "[win32-host] refused: %d\n", static_cast<int>(code));
    };
    // The line every conformance host prints when content asks to exit; then the game goes.
    listener.exitRequested = [] {
        std::printf("[host] exit requested\n");
        std::fflush(stdout);
        if (g_host.game != nullptr) (void)g_host.game->close();
    };
    listener.closed = [] {
        std::printf("[win32-host] done\n");
        std::fflush(stdout);
        PostQuitMessage(0);
    };
    // The game goes before its window, which its view is a child of.
    int status = 0;
    {
        Win32ManagedSession game(*engine, frame, std::move(options), std::move(listener));
        if (!game.isValid()) {
            std::fprintf(stderr, "[win32-host] the session was not created: %d\n", static_cast<int>(game.lastError()));
            return 1;
        }
        g_host.game = &game;
        Player player(env(L"MIGO_WIN32_HOST_FEED"), frame, game);
        g_host.player = &player;

        RECT client{};
        GetClientRect(frame, &client);
        MoveWindow(game.view()->hwnd(), 0, 0, client.right, client.bottom, FALSE);
        ShowWindow(game.view()->hwnd(), SW_SHOW);
        ShowWindow(frame, SW_SHOWNORMAL);
        SetForegroundWindow(frame);
        SetTimer(frame, kPlayerTimer, 50, nullptr);
        SetTimer(frame, kEndTimer, static_cast<UINT>(std::max(1, seconds)) * 1000U, nullptr);
        if (const MigoResult loaded = game.loadContent(content_id, "game.js"); loaded != MIGO_OK) {
            std::fprintf(stderr, "[win32-host] loadContent failed: %d\n", static_cast<int>(loaded));
            (void)game.close();
        }
        std::printf("[win32-host] running '%s' for %ds\n", content_id.c_str(), seconds);
        std::fflush(stdout);

        MSG message;
        while (GetMessageW(&message, nullptr, 0, 0) > 0) {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
        status = game.isClosed() ? 0 : 1;
        g_host.player = nullptr;
        g_host.game = nullptr;
        KillTimer(frame, kPlayerTimer);
    }
    DestroyWindow(frame);
    (void)migo_engine_destroy(engine);
    return status;
}

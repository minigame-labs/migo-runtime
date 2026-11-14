#include "images.hpp"

#include "gui_queue.hpp"
#include "image_codec.hpp"
#include "media_viewer.hpp"
#include "staging.hpp"
#include "win32_text.hpp"

#include <mfapi.h>
#include <mfidl.h>
#include <mfreadwrite.h>
#include <shlobj.h>
#include <shlwapi.h>
#include <shobjidl.h>
#include <winhttp.h>

#include <algorithm>
#include <atomic>
#include <chrono>
#include <cmath>
#include <cwctype>
#include <mutex>
#include <optional>
#include <set>
#include <string_view>
#include <thread>
#include <vector>

namespace migo::windows_host {

namespace {

using codec::ComPtr;

constexpr char kNotSupported[] = "not supported";
constexpr char kCancel[] = "cancel";
constexpr char kNowhere[] = "there is nowhere to put the file";
/// How long a preview's download may go without a byte: the network calls' own default.
constexpr DWORD kTransferTimeoutMs = 60'000;
// How often a dialog asked to close is looked at again.
constexpr DWORD kDialogCloseRetryMs = 50;

/// A COM apartment for the calling thread, for as long as it lives.
class Apartment final {
public:
    explicit Apartment(DWORD model) : joined_(SUCCEEDED(CoInitializeEx(nullptr, model | COINIT_DISABLE_OLE1DDE))) {}
    ~Apartment() {
        if (joined_) CoUninitialize();
    }
    Apartment(const Apartment &) = delete;
    Apartment &operator=(const Apartment &) = delete;

private:
    const bool joined_;
};

/// Runs `work` on the system's thread pool, in the multithreaded apartment.
void off_the_gui_thread(std::function<void()> work) {
    struct Job {
        static void NTAPI run(PTP_CALLBACK_INSTANCE, void *context) {
            const std::unique_ptr<std::function<void()>> job(static_cast<std::function<void()> *>(context));
            const Apartment apartment(COINIT_MULTITHREADED);
            (*job)();
        }
    };
    auto *job = new std::function<void()>(std::move(work));
    if (!TrySubmitThreadpoolCallback(&Job::run, job, nullptr)) {
        // The pool is out of room: a thread of the job's own.
        std::thread([job] { Job::run(nullptr, job); }).detach();
    }
}

/// Waits for `event`, letting through the messages other threads send this
/// one -- a dialog owned by the App's window sends it some -- for up to `ms`.
void wait_letting_sent_messages_through(HANDLE event, DWORD ms) {
    if (MsgWaitForMultipleObjectsEx(1, &event, ms, QS_SENDMESSAGE, 0) == WAIT_OBJECT_0 + 1) {
        MSG message;
        PeekMessageW(&message, nullptr, 0, 0, PM_NOREMOVE | PM_QS_SENDMESSAGE);
    }
}

std::string reason_of_error(DWORD error) {
    switch (error) {
        case ERROR_FILE_NOT_FOUND:
        case ERROR_PATH_NOT_FOUND:
            return "file not found";
        case ERROR_ACCESS_DENIED:
            return "access denied";
        case ERROR_DISK_FULL:
        case ERROR_HANDLE_DISK_FULL:
            return "the disk is full";
        default:
            return "the file could not be copied";
    }
}

/// Copies `from` to `to`, replacing what is there only once the copy is whole.
bool copy_whole(const std::wstring &from, const std::wstring &to, std::string &error) {
    const std::wstring partial = to + L".migo-partial";
    if (!CopyFileExW(from.c_str(), partial.c_str(), nullptr, nullptr, nullptr, 0)) {
        error = reason_of_error(GetLastError());
        DeleteFileW(partial.c_str());
        return false;
    }
    if (!MoveFileExW(partial.c_str(), to.c_str(), MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH)) {
        error = reason_of_error(GetLastError());
        DeleteFileW(partial.c_str());
        return false;
    }
    return true;
}

std::optional<WIN32_FILE_ATTRIBUTE_DATA> attributes_of(const std::wstring &path) {
    WIN32_FILE_ATTRIBUTE_DATA data{};
    if (!GetFileAttributesExW(path.c_str(), GetFileExInfoStandard, &data)) return std::nullopt;
    return data;
}

double size_of(const std::wstring &path) {
    const auto data = attributes_of(path);
    if (!data.has_value()) return 0;
    return static_cast<double>((static_cast<std::uint64_t>(data->nFileSizeHigh) << 32) | data->nFileSizeLow);
}

/// Seconds since the Unix epoch of a file time.
double seconds_of(const FILETIME &time) {
    const std::uint64_t ticks = (static_cast<std::uint64_t>(time.dwHighDateTime) << 32) | time.dwLowDateTime;
    constexpr std::uint64_t kEpoch = 116444736000000000ULL;  // 1970-01-01 in 100 ns ticks since 1601
    return ticks > kEpoch ? std::floor(static_cast<double>(ticks - kEpoch) / 1e7) : 0;
}

std::wstring suffix_of(const std::wstring &path) {
    const wchar_t *extension = PathFindExtensionW(path.c_str());
    std::wstring suffix = extension != nullptr && *extension == L'.' ? std::wstring(extension + 1) : std::wstring();
    for (wchar_t &letter : suffix) letter = static_cast<wchar_t>(std::towlower(letter));
    return suffix;
}

std::wstring name_of(const std::wstring &path) {
    const wchar_t *name = PathFindFileNameW(path.c_str());
    return name != nullptr ? std::wstring(name) : path;
}

/// "image", "video" or "file", as chooseMessageFile words it: by what the
/// file is, as the system perceives its kind.
std::string kind_of(const std::wstring &path) {
    const std::wstring suffix = L"." + suffix_of(path);
    PERCEIVED perceived = PERCEIVED_TYPE_UNSPECIFIED;
    PERCEIVEDFLAG flags = 0;
    if (suffix.size() > 1 && SUCCEEDED(AssocGetPerceivedType(suffix.c_str(), &perceived, &flags, nullptr))) {
        if (perceived == PERCEIVED_TYPE_IMAGE) return "image";
        if (perceived == PERCEIVED_TYPE_VIDEO) return "video";
    }
    return "file";
}

/// Every suffix a video Media Foundation opens is stored under: its byte-stream
/// handlers' registrations, the ones the system perceives as video.
std::wstring video_extensions() {
    std::set<std::wstring> suffixes;
    HKEY handlers = nullptr;
    if (RegOpenKeyExW(HKEY_LOCAL_MACHINE, L"SOFTWARE\\Microsoft\\Windows Media Foundation\\ByteStreamHandlers", 0,
                      KEY_ENUMERATE_SUB_KEYS, &handlers) == ERROR_SUCCESS) {
        wchar_t name[256];
        for (DWORD index = 0;; ++index) {
            DWORD length = 256;
            if (RegEnumKeyExW(handlers, index, name, &length, nullptr, nullptr, nullptr, nullptr) != ERROR_SUCCESS) {
                break;
            }
            if (length < 2 || name[0] != L'.') continue;
            if (kind_of(std::wstring(L"x") + name) == "video") suffixes.insert(suffix_of(std::wstring(L"x") + name));
        }
        RegCloseKey(handlers);
    }
    std::wstring joined;
    for (const std::wstring &suffix : suffixes) {
        if (!joined.empty()) joined += L';';
        joined += suffix;
    }
    return joined;
}

/// "jpg;png" as a dialog's pattern: "*.jpg;*.png".
std::wstring patterns_of(const std::wstring &suffixes) {
    std::wstring patterns;
    std::size_t start = 0;
    while (start <= suffixes.size()) {
        std::size_t end = suffixes.find(L';', start);
        if (end == std::wstring::npos) end = suffixes.size();
        if (end > start) {
            if (!patterns.empty()) patterns += L';';
            patterns += L"*." + suffixes.substr(start, end - start);
        }
        start = end + 1;
    }
    return patterns;
}

std::wstring known_folder(const KNOWNFOLDERID &folder) {
    PWSTR path = nullptr;
    std::wstring found;
    if (SUCCEEDED(SHGetKnownFolderPath(folder, KF_FLAG_CREATE, nullptr, &path)) && path != nullptr) found = path;
    CoTaskMemFree(path);
    return found;
}

/// The most the player may pick: `value`, or `fallback`, from 1 to 100.
std::size_t count_of(const Json &value, double fallback) {
    return static_cast<std::size_t>(std::clamp(value.numberOr(fallback), 1.0, 100.0));
}

/// A width or height asked for in pixels, or 0 when none is.
std::uint32_t dimension_of(const Json &value) {
    const double asked = value.numberOr(0);
    return std::isfinite(asked) && asked >= 1 ? static_cast<std::uint32_t>(std::min(asked, 1e7)) : 0;
}

/// The camera is not offered: a call that allows only it is refused, one that
/// allows the album too is shown the dialog.
bool album_allowed(const Json &sources) {
    if (!sources.isArray() || sources.items().empty()) return true;
    for (const Json &source : sources.items()) {
        if (source.string() == "album") return true;
    }
    return false;
}

/// Answers `reply` with `result`, which names `files` of the Host Kit's own.
/// The Session takes each as the call completes; what it did not take -- the
/// game that asked has gone -- is removed.
void hand_over(HostServiceReply &reply, const Json &result, const std::vector<std::wstring> &files) {
    (void)reply.succeed(result);
    for (const std::wstring &file : files) DeleteFileW(file.c_str());
}

void give_up(HostServiceReply &reply, const std::string &reason, const std::vector<std::wstring> &staged) {
    for (const std::wstring &file : staged) DeleteFileW(file.c_str());
    (void)reply.fail(reason);
}

/// The image at `path` decoded upright at the size `width` x `height` asks of
/// it, refusing one larger than `Images::kMaxPixels`.
ComPtr<IWICBitmapSource> decode(IWICImagingFactory &wic, const codec::Opened &image, std::uint32_t width,
                                std::uint32_t height, std::string &error) {
    const auto [target_width, target_height] =
        Images::targetSize(image.uprightWidth(), image.uprightHeight(), width, height);
    if (static_cast<std::int64_t>(target_width) * target_height > Images::kMaxPixels) {
        error = "image too large";
        return nullptr;
    }
    ComPtr<IWICBitmapSource> source = codec::upright(wic, image, target_width, target_height);
    if (source == nullptr) error = "decode failed";
    return source;
}

/// A staged copy of the picked image at `file` that content can draw: a JPEG,
/// PNG, GIF, BMP or WebP as it is; anything else WIC reads -- a TIFF, a HEIF
/// where its codec is installed -- written upright, as a PNG when it has
/// transparency and as a JPEG when it has none. Empty, with `error`, when
/// there is no image to give.
std::wstring stage_image(IWICImagingFactory &wic, const Staging &staging, const std::wstring &file,
                         std::string &error) {
    const std::string name = utf8Of(name_of(file));
    std::string reason;
    const std::optional<codec::Opened> image = codec::open(wic, file, reason);
    if (!image.has_value()) {
        error = name + ": " + reason;
        return {};
    }
    if (const wchar_t *drawable = codec::drawableSuffix(image->container); drawable != nullptr) {
        const std::wstring copy = staging.path(drawable);
        if (copy.empty()) {
            error = kNowhere;
            return {};
        }
        if (!CopyFileW(file.c_str(), copy.c_str(), TRUE)) {
            error = name + " could not be copied";
            return {};
        }
        return copy;
    }
    ComPtr<IWICBitmapSource> source = decode(wic, *image, 0, 0, reason);
    if (source == nullptr) {
        error = name + ": " + reason;
        return {};
    }
    const bool transparent = codec::hasAlpha(wic, *source.Get());
    const std::wstring copy = staging.path(transparent ? L"png" : L"jpg");
    if (copy.empty()) {
        error = kNowhere;
        return {};
    }
    if (!codec::write(wic, *source.Get(), copy, transparent ? GUID_ContainerFormatPng : GUID_ContainerFormatJpeg, 92,
                      reason)) {
        error = name + ": " + reason;
        return {};
    }
    return copy;
}

/// A video as chooseMedia describes it: its length in seconds and its size as
/// shown, read by Media Foundation.
struct VideoFacts {
    double seconds = 0;
    std::uint32_t width = 0;
    std::uint32_t height = 0;
};

std::optional<VideoFacts> describe_video(const std::wstring &path) {
    if (FAILED(MFStartup(MF_VERSION, MFSTARTUP_LITE))) return std::nullopt;
    std::optional<VideoFacts> facts;
    {
        ComPtr<IMFSourceReader> reader;
        if (SUCCEEDED(MFCreateSourceReaderFromURL(path.c_str(), nullptr, reader.GetAddressOf()))) {
            VideoFacts found;
            PROPVARIANT duration;
            PropVariantInit(&duration);
            if (SUCCEEDED(reader->GetPresentationAttribute(static_cast<DWORD>(MF_SOURCE_READER_MEDIASOURCE),
                                                           MF_PD_DURATION, &duration)) &&
                duration.vt == VT_UI8) {
                found.seconds = static_cast<double>(duration.uhVal.QuadPart) / 1e7;
            }
            PropVariantClear(&duration);
            ComPtr<IMFMediaType> type;
            if (SUCCEEDED(reader->GetCurrentMediaType(static_cast<DWORD>(MF_SOURCE_READER_FIRST_VIDEO_STREAM),
                                                      type.GetAddressOf()))) {
                UINT32 width = 0;
                UINT32 height = 0;
                if (SUCCEEDED(MFGetAttributeSize(type.Get(), MF_MT_FRAME_SIZE, &width, &height))) {
                    // A quarter turn exchanges the sides it is shown at.
                    const UINT32 rotation = MFGetAttributeUINT32(type.Get(), MF_MT_VIDEO_ROTATION, 0);
                    const bool turned = rotation == 90 || rotation == 270;
                    found.width = turned ? height : width;
                    found.height = turned ? width : height;
                }
            }
            facts = found;
        }
    }
    MFShutdown();
    return facts;
}

/// The system's thumbnail of `path`, written as the other platforms write a
/// cover: a JPEG at 80. Empty when the system has none.
std::wstring stage_cover(IWICImagingFactory &wic, const Staging &staging, const std::wstring &path) {
    ComPtr<IShellItemImageFactory> item;
    if (FAILED(SHCreateItemFromParsingName(path.c_str(), nullptr, IID_PPV_ARGS(item.GetAddressOf())))) return {};
    HBITMAP thumbnail = nullptr;
    if (FAILED(item->GetImage(SIZE{512, 512}, SIIGBF_THUMBNAILONLY | SIIGBF_BIGGERSIZEOK, &thumbnail)) ||
        thumbnail == nullptr) {
        return {};
    }
    std::wstring cover;
    ComPtr<IWICBitmap> bitmap;
    if (SUCCEEDED(wic.CreateBitmapFromHBITMAP(thumbnail, nullptr, WICBitmapIgnoreAlpha, bitmap.GetAddressOf()))) {
        const std::wstring staged = staging.path(L"jpg");
        std::string unwritten;
        if (!staged.empty() && codec::write(wic, *bitmap.Get(), staged, GUID_ContainerFormatJpeg, 80, unwritten)) {
            cover = staged;
        }
    }
    DeleteObject(thumbnail);
    return cover;
}

/// One open dialog's question: what may be picked, how many, from where.
struct OpenQuestion {
    HWND owner = nullptr;
    std::size_t count = 1;
    /// Each filter's label and its "*.a;*.b" patterns; computed on the dialog's
    /// thread when `images` or `videos` ask for them.
    std::vector<std::pair<std::wstring, std::wstring>> filters;
    bool image_filter = false;
    bool video_filter = false;
    std::wstring folder;
};

/// A preview's downloads, stoppable from the GUI thread: closing the request in
/// flight makes the call waiting on it return.
class Downloads final {
public:
    void stop() {
        const std::lock_guard<std::mutex> lock(mutex_);
        stopped_ = true;
        if (request_ != nullptr) WinHttpCloseHandle(request_);
        request_ = nullptr;
    }

    [[nodiscard]] bool stopped() {
        const std::lock_guard<std::mutex> lock(mutex_);
        return stopped_;
    }

    /// `request` is the one in flight; false when the downloads were stopped.
    bool begin(HINTERNET request) {
        const std::lock_guard<std::mutex> lock(mutex_);
        if (stopped_) return false;
        request_ = request;
        return true;
    }

    /// Closes `request` unless stopping did.
    void end(HINTERNET request) {
        const std::lock_guard<std::mutex> lock(mutex_);
        if (request_ == request) WinHttpCloseHandle(request);
        request_ = nullptr;
    }

private:
    std::mutex mutex_;
    bool stopped_ = false;
    HINTERNET request_ = nullptr;
};

/// Downloads `url` into `file`; empty on success, else why not.
std::string download(Downloads &downloads, const std::wstring &url, HANDLE file) {
    URL_COMPONENTS parts{};
    parts.dwStructSize = sizeof parts;
    parts.dwHostNameLength = static_cast<DWORD>(-1);
    parts.dwUrlPathLength = static_cast<DWORD>(-1);
    parts.dwExtraInfoLength = static_cast<DWORD>(-1);
    parts.dwSchemeLength = static_cast<DWORD>(-1);
    if (!WinHttpCrackUrl(url.c_str(), 0, 0, &parts) || parts.dwHostNameLength == 0) return "download failed: bad URL";
    const std::wstring host(parts.lpszHostName, parts.dwHostNameLength);
    std::wstring path(parts.lpszUrlPath, parts.dwUrlPathLength);
    if (parts.lpszExtraInfo != nullptr) path.append(parts.lpszExtraInfo, parts.dwExtraInfoLength);
    if (path.empty()) path = L"/";

    // The system's proxy, as the player set it.
    HINTERNET session = WinHttpOpen(L"Migo", WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_NO_PROXY_NAME,
                                    WINHTTP_NO_PROXY_BYPASS, 0);
    if (session == nullptr) return "download failed: the network is not available";
    const int timeout = static_cast<int>(kTransferTimeoutMs);
    WinHttpSetTimeouts(session, timeout, timeout, timeout, timeout);
    std::string error;
    HINTERNET connection = WinHttpConnect(session, host.c_str(), parts.nPort, 0);
    HINTERNET request = connection == nullptr
                            ? nullptr
                            : WinHttpOpenRequest(connection, L"GET", path.c_str(), nullptr, WINHTTP_NO_REFERER,
                                                 WINHTTP_DEFAULT_ACCEPT_TYPES,
                                                 parts.nScheme == INTERNET_SCHEME_HTTPS ? WINHTTP_FLAG_SECURE : 0);
    if (request == nullptr || !downloads.begin(request)) {
        if (request != nullptr) WinHttpCloseHandle(request);
        error = request == nullptr ? "download failed: the server cannot be reached" : "download failed: stopped";
    } else {
        DWORD status = 0;
        DWORD status_size = sizeof status;
        if (!WinHttpSendRequest(request, WINHTTP_NO_ADDITIONAL_HEADERS, 0, WINHTTP_NO_REQUEST_DATA, 0, 0, 0) ||
            !WinHttpReceiveResponse(request, nullptr)) {
            error = downloads.stopped() ? "download failed: stopped" : "download failed: the server cannot be reached";
        } else if (!WinHttpQueryHeaders(request, WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                                        WINHTTP_HEADER_NAME_BY_INDEX, &status, &status_size, WINHTTP_NO_HEADER_INDEX) ||
                   status < 200 || status >= 300) {
            error = "download failed: HTTP " + std::to_string(status);
        } else {
            std::vector<char> buffer(64 * 1024);
            for (;;) {
                DWORD received = 0;
                if (!WinHttpReadData(request, buffer.data(), static_cast<DWORD>(buffer.size()), &received)) {
                    error = downloads.stopped() ? "download failed: stopped" : "download failed: the transfer broke off";
                    break;
                }
                if (received == 0) break;
                DWORD written = 0;
                if (!WriteFile(file, buffer.data(), received, &written, nullptr) || written != received) {
                    error = "download failed: " + reason_of_error(GetLastError());
                    break;
                }
            }
        }
        downloads.end(request);
    }
    if (connection != nullptr) WinHttpCloseHandle(connection);
    WinHttpCloseHandle(session);
    return error;
}

bool remote(const std::string &source) {
    return source.rfind("http://", 0) == 0 || source.rfind("https://", 0) == 0;
}

}  // namespace

// ---- the service ------------------------------------------------------------------------------

Images::Places Images::Places::system() { return {Staging::systemRoot(), known_folder(FOLDERID_Pictures)}; }

std::pair<std::uint32_t, std::uint32_t> Images::targetSize(std::uint32_t source_width, std::uint32_t source_height,
                                                           std::uint32_t width, std::uint32_t height) noexcept {
    // `side` scaled as `asked` is to `other`: at least a pixel.
    const auto scaled = [](std::uint32_t side, std::uint32_t asked, std::uint32_t other) {
        const double value = std::round(static_cast<double>(side) * asked / std::max<std::uint32_t>(other, 1));
        return static_cast<std::uint32_t>(std::clamp(value, 1.0, 4294967295.0));
    };
    if (width > 0 && height > 0) return {width, height};
    if (width > 0) return {width, scaled(source_height, width, source_width)};
    if (height > 0) return {scaled(source_width, height, source_height), height};
    return {source_width, source_height};
}

class Images::Impl final {
public:
    Impl(HWND owner, Places places)
        : owner_(owner), post_(queue_.poster()), places_(std::move(places)),
          dialog_done_(CreateEventW(nullptr, TRUE, TRUE, nullptr)) {}

    ~Impl() {
        *alive_ = false;
        // A dialog still up is dismissed with the service that showed it; the
        // answer it gives reaches nobody once the channel is closed. A preview
        // shown goes too, and the downloads still running stop.
        closeDialog();
        viewer_.reset();
        for (Loader &loader : loaders_) loader.downloads->stop();
        for (Loader &loader : loaders_) loader.thread.join();
        if (dialog_done_ != nullptr) CloseHandle(dialog_done_);
    }

    void call(std::uint32_t method, const Json &request, HostServiceReply reply) {
        switch (method) {
            case MIGO_IMAGE_COMPRESS_IMAGE:
                compressImage(request, std::move(reply));
                return;
            case MIGO_IMAGE_CHOOSE_IMAGE:
                chooseImage(request, std::move(reply));
                return;
            case MIGO_IMAGE_CHOOSE_MEDIA:
                chooseMedia(request, std::move(reply));
                return;
            case MIGO_IMAGE_CHOOSE_MESSAGE_FILE:
                chooseMessageFile(request, std::move(reply));
                return;
            case MIGO_IMAGE_SAVE_FILE_TO_DISK:
                saveFileToDisk(request, std::move(reply));
                return;
            case MIGO_IMAGE_SAVE_IMAGE_TO_PHOTOS_ALBUM:
                saveImageToPhotosAlbum(request, std::move(reply));
                return;
            case MIGO_IMAGE_PREVIEW_IMAGE: {
                std::vector<std::string> urls;
                for (const Json &url : request["urls"].items()) urls.push_back(url.string());
                // `current` is one of them, made a real path as they were.
                const auto at = std::find(urls.begin(), urls.end(), request["current"].string());
                preview(std::move(urls), at == urls.end() ? 0 : static_cast<int>(at - urls.begin()), std::move(reply));
                return;
            }
            case MIGO_IMAGE_PREVIEW_MEDIA: {
                std::vector<std::string> urls;
                for (const Json &source : request["sources"].items()) {
                    // Videos once the Host Kit plays them.
                    if (source["type"].string() == "video") {
                        (void)reply.fail(kNotSupported);
                        return;
                    }
                    urls.push_back(source["url"].string());
                }
                preview(std::move(urls), static_cast<int>(request["current"].numberOr(0)), std::move(reply));
                return;
            }
            default:
                (void)reply.fail(kNotSupported);
                return;
        }
    }

private:
    std::shared_ptr<Staging> staging() {
        if (staging_ == nullptr) staging_ = std::make_shared<Staging>(places_.staging);
        return staging_;
    }

    // ---- the dialogs' thread ----------------------------------------------------------------------

    /// Runs `work` on a thread of its own, in an apartment of its own, where it
    /// shows a dialog modal to the App's window: the GUI thread goes on. One at
    /// a time.
    bool startDialog(HostServiceReply &reply, std::function<void(const std::atomic<bool> &cancelled)> work) {
        if (dialog_busy_->load()) {
            (void)reply.fail("another file dialog is showing");
            return false;
        }
        if (dialog_thread_.joinable()) dialog_thread_.join();
        dialog_busy_->store(true);
        dialog_cancelled_->store(false);
        ResetEvent(dialog_done_);
        dialog_thread_id_->store(0);
        dialog_thread_ = std::thread([work = std::move(work), busy = dialog_busy_, cancelled = dialog_cancelled_,
                                      thread_id = dialog_thread_id_, done = dialog_done_] {
            thread_id->store(GetCurrentThreadId());
            {
                const Apartment apartment(COINIT_APARTMENTTHREADED);
                work(*cancelled);
            }
            busy->store(false);
            SetEvent(done);
        });
        return true;
    }

    void closeDialog() {
        if (!dialog_thread_.joinable()) return;
        dialog_cancelled_->store(true);
        while (WaitForSingleObject(dialog_done_, 0) == WAIT_TIMEOUT) {
            // The dialog's own windows are told to close -- the player's
            // dismissal -- as soon as they exist.
            if (const DWORD thread = dialog_thread_id_->load(); thread != 0) {
                EnumThreadWindows(
                    thread,
                    [](HWND window, LPARAM) -> BOOL {
                        if (IsWindowVisible(window)) PostMessageW(window, WM_CLOSE, 0, 0);
                        return TRUE;
                    },
                    0);
            }
            wait_letting_sent_messages_through(dialog_done_, kDialogCloseRetryMs);
        }
        dialog_thread_.join();
    }

    /// The paths the player picked, at most `question.count`; none when they
    /// dismissed the dialog, or with `error` when it could not be shown. On the
    /// dialog's thread.
    static std::optional<std::vector<std::wstring>> askOpen(const OpenQuestion &question,
                                                            const std::atomic<bool> &cancelled, std::string &error) {
        ComPtr<IFileOpenDialog> dialog;
        if (FAILED(CoCreateInstance(CLSID_FileOpenDialog, nullptr, CLSCTX_INPROC_SERVER,
                                    IID_PPV_ARGS(dialog.GetAddressOf())))) {
            error = "the file dialog could not be shown";
            return std::nullopt;
        }
        FILEOPENDIALOGOPTIONS options = 0;
        dialog->GetOptions(&options);
        options |= FOS_FORCEFILESYSTEM | FOS_FILEMUSTEXIST | FOS_PATHMUSTEXIST;
        if (question.count > 1) options |= FOS_ALLOWMULTISELECT;
        dialog->SetOptions(options);
        std::vector<std::pair<std::wstring, std::wstring>> filters = question.filters;
        if (question.image_filter || question.video_filter) {
            ComPtr<IWICImagingFactory> wic = codec::factory();
            const std::wstring images =
                question.image_filter && wic != nullptr ? patterns_of(codec::decoderExtensions(*wic.Get())) : L"";
            const std::wstring videos = question.video_filter ? patterns_of(video_extensions()) : L"";
            if (!images.empty() && !videos.empty()) filters.emplace_back(L"Images and videos", images + L";" + videos);
            if (!images.empty()) filters.emplace_back(L"Images", images);
            if (!videos.empty()) filters.emplace_back(L"Videos", videos);
        }
        std::vector<COMDLG_FILTERSPEC> specs;
        for (const auto &[label, patterns] : filters) specs.push_back({label.c_str(), patterns.c_str()});
        if (!specs.empty()) dialog->SetFileTypes(static_cast<UINT>(specs.size()), specs.data());
        if (!question.folder.empty()) {
            ComPtr<IShellItem> folder;
            if (SUCCEEDED(SHCreateItemFromParsingName(question.folder.c_str(), nullptr,
                                                      IID_PPV_ARGS(folder.GetAddressOf())))) {
                dialog->SetDefaultFolder(folder.Get());
            }
        }
        if (cancelled.load()) return std::nullopt;
        const HRESULT shown = dialog->Show(question.owner);
        if (shown == HRESULT_FROM_WIN32(ERROR_CANCELLED)) return std::nullopt;
        if (FAILED(shown)) {
            error = "the file dialog could not be shown";
            return std::nullopt;
        }
        ComPtr<IShellItemArray> results;
        DWORD picked = 0;
        if (FAILED(dialog->GetResults(results.GetAddressOf())) || FAILED(results->GetCount(&picked))) {
            error = "the file dialog gave no files";
            return std::nullopt;
        }
        // A dialog does not count what the player selects.
        std::vector<std::wstring> paths;
        for (DWORD index = 0; index < picked && paths.size() < question.count; ++index) {
            ComPtr<IShellItem> item;
            PWSTR path = nullptr;
            if (SUCCEEDED(results->GetItemAt(index, item.GetAddressOf())) &&
                SUCCEEDED(item->GetDisplayName(SIGDN_FILESYSPATH, &path)) && path != nullptr) {
                paths.emplace_back(path);
            }
            CoTaskMemFree(path);
        }
        return paths;
    }

    using Picked = std::function<void(std::vector<std::wstring> files, HostServiceReply &reply)>;

    void pick(OpenQuestion question, HostServiceReply reply, Picked picked) {
        question.owner = owner_;
        auto answer = std::make_shared<HostServiceReply>(std::move(reply));
        (void)startDialog(*answer, [question = std::move(question), answer, picked = std::move(picked)](
                                       const std::atomic<bool> &cancelled) {
            std::string error;
            std::optional<std::vector<std::wstring>> files = askOpen(question, cancelled, error);
            if (!files.has_value()) {
                (void)answer->fail(error.empty() ? std::string(kCancel) : error);
                return;
            }
            picked(std::move(*files), *answer);
        });
    }

    // ---- compressImage --------------------------------------------------------------------------

    void compressImage(const Json &request, HostServiceReply reply) {
        const std::wstring source = wideOf(request["src"].string());
        if (source.empty()) {
            (void)reply.fail("src is required");
            return;
        }
        const int quality = static_cast<int>(std::lround(std::clamp(request["quality"].numberOr(80), 0.0, 100.0)));
        const std::uint32_t width = dimension_of(request["compressedWidth"]);
        const std::uint32_t height = dimension_of(request["compressedHeight"]);
        off_the_gui_thread([staging = staging(), reply = std::move(reply), source, quality, width, height]() mutable {
            ComPtr<IWICImagingFactory> wic = codec::factory();
            std::string error;
            if (wic == nullptr) {
                (void)reply.fail("decode failed");
                return;
            }
            const std::optional<codec::Opened> image = codec::open(*wic.Get(), source, error);
            ComPtr<IWICBitmapSource> decoded = image.has_value() ? decode(*wic.Get(), *image, width, height, error) : nullptr;
            if (decoded == nullptr) {
                (void)reply.fail(error);
                return;
            }
            // A PNG stays a PNG; anything else becomes a JPEG, at the quality asked.
            const bool png = IsEqualGUID(image->container, GUID_ContainerFormatPng) != FALSE;
            const std::wstring output = staging->path(png ? L"png" : L"jpg");
            if (output.empty()) {
                (void)reply.fail(kNowhere);
                return;
            }
            if (!codec::write(*wic.Get(), *decoded.Get(), output, png ? GUID_ContainerFormatPng : GUID_ContainerFormatJpeg,
                              quality, error)) {
                (void)reply.fail(error);
                return;
            }
            hand_over(reply, Json::object().set("tempFilePath", utf8Of(output)), {output});
        });
    }

    // ---- the pickers ----------------------------------------------------------------------------

    void chooseImage(const Json &request, HostServiceReply reply) {
        if (!album_allowed(request["sourceType"])) {
            (void)reply.fail(kNotSupported);
            return;
        }
        OpenQuestion question;
        question.count = count_of(request["count"], 9);
        question.image_filter = true;
        question.folder = places_.album;
        // The originals, whichever size is asked for: the player picked them.
        pick(std::move(question), std::move(reply),
             [staging = staging()](std::vector<std::wstring> files, HostServiceReply &reply) {
                 ComPtr<IWICImagingFactory> wic = codec::factory();
                 std::vector<std::wstring> staged;
                 Json paths = Json(Json::Array{});
                 Json described = Json(Json::Array{});
                 for (const std::wstring &file : files) {
                     std::string error = "decode failed";
                     const std::wstring copy = wic != nullptr ? stage_image(*wic.Get(), *staging, file, error) : L"";
                     if (copy.empty()) {
                         give_up(reply, error, staged);
                         return;
                     }
                     staged.push_back(copy);
                     paths.push(utf8Of(copy));
                     described.push(Json::object().set("path", utf8Of(copy)).set("size", size_of(copy)));
                 }
                 hand_over(reply, Json::object().set("tempFilePaths", std::move(paths)).set("tempFiles", std::move(described)),
                           staged);
             });
    }

    void chooseMedia(const Json &request, HostServiceReply reply) {
        bool images = false;
        bool videos = false;
        const Json::Array &types = request["mediaType"].items();
        for (const Json &type : types) {
            images = images || type.string() == "image" || type.string() == "mix";
            videos = videos || type.string() == "video" || type.string() == "mix";
        }
        // None named is both.
        if (types.empty()) images = videos = true;
        if (!(images || videos) || !album_allowed(request["sourceType"])) {
            (void)reply.fail(kNotSupported);
            return;
        }
        OpenQuestion question;
        question.count = count_of(request["count"], 9);
        question.image_filter = images;
        question.video_filter = videos;
        question.folder = places_.album;
        pick(std::move(question), std::move(reply),
             [staging = staging(), videos](std::vector<std::wstring> files, HostServiceReply &reply) {
                 ComPtr<IWICImagingFactory> wic = codec::factory();
                 if (wic == nullptr) {
                     (void)reply.fail("decode failed");
                     return;
                 }
                 std::vector<std::wstring> staged;
                 Json items = Json(Json::Array{});
                 bool saw_image = false;
                 bool saw_video = false;
                 for (const std::wstring &file : files) {
                     std::string error;
                     if (videos && kind_of(file) == "video") {
                         const std::wstring copy = staging->path(suffix_of(file));
                         if (copy.empty() || !CopyFileW(file.c_str(), copy.c_str(), TRUE)) {
                             give_up(reply, copy.empty() ? std::string(kNowhere) : utf8Of(name_of(file)) + " could not be copied",
                                     staged);
                             return;
                         }
                         staged.push_back(copy);
                         // Its length, its size and its cover where they can be
                         // read; zeros and no cover where they cannot, as the
                         // other platforms answer.
                         const VideoFacts facts = describe_video(copy).value_or(VideoFacts{});
                         Json item = Json::object()
                                         .set("tempFilePath", utf8Of(copy))
                                         .set("size", size_of(copy))
                                         .set("duration", facts.seconds)
                                         .set("width", facts.width)
                                         .set("height", facts.height)
                                         .set("fileType", "video");
                         if (const std::wstring cover = stage_cover(*wic.Get(), *staging, copy); !cover.empty()) {
                             staged.push_back(cover);
                             item.set("thumbTempFilePath", utf8Of(cover));
                         }
                         items.push(std::move(item));
                         saw_video = true;
                         continue;
                     }
                     const std::wstring copy = stage_image(*wic.Get(), *staging, file, error);
                     if (copy.empty()) {
                         give_up(reply, error, staged);
                         return;
                     }
                     staged.push_back(copy);
                     std::string unread;
                     const std::optional<codec::Opened> image = codec::open(*wic.Get(), copy, unread);
                     items.push(Json::object()
                                    .set("tempFilePath", utf8Of(copy))
                                    .set("size", size_of(copy))
                                    .set("width", image.has_value() ? image->uprightWidth() : 0U)
                                    .set("height", image.has_value() ? image->uprightHeight() : 0U)
                                    .set("fileType", "image"));
                     saw_image = true;
                 }
                 const char *type = saw_image && saw_video ? "mix" : saw_video ? "video" : "image";
                 hand_over(reply, Json::object().set("type", type).set("tempFiles", std::move(items)), staged);
             });
    }

    void chooseMessageFile(const Json &request, HostServiceReply reply) {
        // "file" is anything but an image or a video; "all", or a type not known, anything.
        std::string type = request["type"].stringOr("all");
        if (type != "image" && type != "video" && type != "file") type = "all";
        // The extensions narrow a pick of files, and only of files.
        std::set<std::wstring> extensions;
        if (type == "file") {
            for (const Json &value : request["extension"].items()) {
                std::wstring extension = wideOf(value.string());
                if (!extension.empty() && extension.front() == L'.') extension.erase(0, 1);
                for (wchar_t &letter : extension) letter = static_cast<wchar_t>(std::towlower(letter));
                if (!extension.empty()) extensions.insert(extension);
            }
        }
        OpenQuestion question;
        question.count = count_of(request["count"], 1);
        question.image_filter = type == "image";
        question.video_filter = type == "video";
        if (!extensions.empty()) {
            std::wstring patterns;
            for (const std::wstring &extension : extensions) patterns += (patterns.empty() ? L"*." : L";*.") + extension;
            // Named with its patterns, as a Windows file type is.
            question.filters.emplace_back(L"Files (" + patterns + L")", patterns);
        }
        pick(std::move(question), std::move(reply),
             [staging = staging(), type, extensions](std::vector<std::wstring> files, HostServiceReply &reply) {
                 std::vector<std::wstring> staged;
                 Json described = Json(Json::Array{});
                 for (const std::wstring &file : files) {
                     // The dialog's filter is the player's to change: what it let
                     // through that the call did not ask for is left out.
                     const std::string kind = kind_of(file);
                     if (type != "all" && kind != type) continue;
                     if (!extensions.empty() && extensions.count(suffix_of(file)) == 0) continue;
                     const std::wstring copy = staging->path(suffix_of(file));
                     if (copy.empty()) {
                         give_up(reply, kNowhere, staged);
                         return;
                     }
                     if (!CopyFileW(file.c_str(), copy.c_str(), TRUE)) {
                         give_up(reply, utf8Of(name_of(file)) + " could not be copied", staged);
                         return;
                     }
                     staged.push_back(copy);
                     const auto original = attributes_of(file);
                     described.push(Json::object()
                                        .set("path", utf8Of(copy))
                                        .set("size", size_of(copy))
                                        .set("name", utf8Of(name_of(file)))
                                        .set("type", kind)
                                        .set("time", original.has_value() ? seconds_of(original->ftLastWriteTime) : 0.0));
                 }
                 hand_over(reply, Json::object().set("tempFiles", std::move(described)), staged);
             });
    }

    // ---- saving ---------------------------------------------------------------------------------

    void saveFileToDisk(const Json &request, HostServiceReply reply) {
        const std::wstring source = wideOf(request["filePath"].string());
        if (source.empty()) {
            (void)reply.fail("filePath is required");
            return;
        }
        const auto file = attributes_of(source);
        if (!file.has_value() || (file->dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY) != 0) {
            (void)reply.fail("file not found");
            return;
        }
        auto answer = std::make_shared<HostServiceReply>(std::move(reply));
        (void)startDialog(*answer, [owner = owner_, source, answer](const std::atomic<bool> &cancelled) {
            ComPtr<IFileSaveDialog> dialog;
            if (FAILED(CoCreateInstance(CLSID_FileSaveDialog, nullptr, CLSCTX_INPROC_SERVER,
                                        IID_PPV_ARGS(dialog.GetAddressOf())))) {
                (void)answer->fail("the file dialog could not be shown");
                return;
            }
            // The dialog asks before replacing a file; the copy replaces it only once whole.
            FILEOPENDIALOGOPTIONS options = 0;
            dialog->GetOptions(&options);
            dialog->SetOptions(options | FOS_FORCEFILESYSTEM | FOS_OVERWRITEPROMPT | FOS_PATHMUSTEXIST);
            dialog->SetFileName(name_of(source).c_str());
            if (const std::wstring suffix = suffix_of(source); !suffix.empty()) dialog->SetDefaultExtension(suffix.c_str());
            if (cancelled.load()) {
                (void)answer->fail(kCancel);
                return;
            }
            const HRESULT shown = dialog->Show(owner);
            ComPtr<IShellItem> chosen;
            PWSTR destination = nullptr;
            if (shown == HRESULT_FROM_WIN32(ERROR_CANCELLED)) {
                (void)answer->fail(kCancel);
            } else if (FAILED(shown) || FAILED(dialog->GetResult(chosen.GetAddressOf())) ||
                       FAILED(chosen->GetDisplayName(SIGDN_FILESYSPATH, &destination)) || destination == nullptr) {
                (void)answer->fail("the file dialog could not be shown");
            } else {
                std::string error;
                (void)(copy_whole(source, destination, error) ? answer->succeed() : answer->fail(error));
            }
            CoTaskMemFree(destination);
        });
    }

    void saveImageToPhotosAlbum(const Json &request, HostServiceReply reply) {
        const std::wstring source = wideOf(request["filePath"].string());
        if (source.empty()) {
            (void)reply.fail("filePath is required");
            return;
        }
        if (places_.album.empty()) {
            (void)reply.fail("there is no photo album");
            return;
        }
        off_the_gui_thread([album = places_.album, source, reply = std::move(reply)]() mutable {
            ComPtr<IWICImagingFactory> wic = codec::factory();
            std::string error;
            const std::optional<codec::Opened> image =
                wic != nullptr ? codec::open(*wic.Get(), source, error) : std::nullopt;
            if (!image.has_value()) {
                (void)reply.fail(error == "file not found" ? error : std::string("not an image"));
                return;
            }
            if (SHCreateDirectoryExW(nullptr, album.c_str(), nullptr) != ERROR_SUCCESS && GetLastError() != ERROR_ALREADY_EXISTS &&
                !attributes_of(album).has_value()) {
                (void)reply.fail("the photo album cannot be written to");
                return;
            }
            // Named as a camera names a photo, with the suffix of what it holds.
            const wchar_t *drawable = codec::drawableSuffix(image->container);
            std::wstring suffix = drawable != nullptr ? std::wstring(drawable) : suffix_of(source);
            if (suffix.empty()) suffix = L"img";
            const auto now = std::chrono::duration_cast<std::chrono::milliseconds>(
                                 std::chrono::system_clock::now().time_since_epoch())
                                 .count();
            const std::wstring stem = album + L"\\IMG_" + std::to_wstring(now);
            std::wstring destination = stem + L"." + suffix;
            for (int n = 1; attributes_of(destination).has_value(); ++n) {
                destination = stem + L"_" + std::to_wstring(n) + L"." + suffix;
            }
            (void)(copy_whole(source, destination, error) ? reply.succeed() : reply.fail(error));
        });
    }

    // ---- the previews -----------------------------------------------------------------------------

    void preview(std::vector<std::string> sources, int current, HostServiceReply reply) {
        if (sources.empty() ||
            std::any_of(sources.begin(), sources.end(), [](const std::string &source) { return source.empty(); })) {
            (void)reply.fail("nothing to preview");
            return;
        }
        current = std::clamp(current, 0, static_cast<int>(sources.size()) - 1);
        // The files, open now: the engine may take back what it lent once the
        // call is answered. What is downloaded is a file removed as it closes.
        auto files = std::make_shared<std::vector<HANDLE>>(sources.size(), INVALID_HANDLE_VALUE);
        const auto release = [](std::vector<HANDLE> &opened) {
            for (HANDLE &file : opened) {
                if (file != INVALID_HANDLE_VALUE) CloseHandle(file);
                file = INVALID_HANDLE_VALUE;
            }
        };
        std::vector<std::pair<std::size_t, std::wstring>> remotes;
        for (std::size_t index = 0; index < sources.size(); ++index) {
            const std::wstring source = wideOf(sources[index]);
            if (remote(sources[index])) {
                const std::wstring path = staging()->path(L"");
                HANDLE file = path.empty() ? INVALID_HANDLE_VALUE
                                           : CreateFileW(path.c_str(), GENERIC_READ | GENERIC_WRITE,
                                                         FILE_SHARE_READ | FILE_SHARE_DELETE, nullptr, CREATE_NEW,
                                                         FILE_ATTRIBUTE_TEMPORARY | FILE_FLAG_DELETE_ON_CLOSE, nullptr);
                if (file == INVALID_HANDLE_VALUE) {
                    release(*files);
                    (void)reply.fail(kNowhere);
                    return;
                }
                (*files)[index] = file;
                remotes.emplace_back(index, source);
                continue;
            }
            HANDLE file = CreateFileW(source.c_str(), GENERIC_READ, FILE_SHARE_READ | FILE_SHARE_DELETE, nullptr,
                                      OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, nullptr);
            if (file == INVALID_HANDLE_VALUE) {
                const DWORD error = GetLastError();
                release(*files);
                (void)reply.fail(error == ERROR_FILE_NOT_FOUND || error == ERROR_PATH_NOT_FOUND
                                     ? std::string("file not found")
                                     : utf8Of(name_of(source)) + " cannot be read");
                return;
            }
            (*files)[index] = file;
        }
        if (remotes.empty()) {
            showPreview(std::move(*files), current, std::move(reply));
            return;
        }
        // The downloads on a thread of their own; the viewer shown, on the GUI
        // thread, once all are in. Those that finished before are reclaimed.
        loaders_.erase(std::remove_if(loaders_.begin(), loaders_.end(),
                                      [](Loader &loader) {
                                          if (!loader.finished->load()) return false;
                                          loader.thread.join();
                                          return true;
                                      }),
                       loaders_.end());
        auto downloads = std::make_shared<Downloads>();
        auto finished = std::make_shared<std::atomic<bool>>(false);
        auto answer = std::make_shared<HostServiceReply>(std::move(reply));
        std::thread thread([this, alive = alive_, post = post_, downloads, finished, files, remotes = std::move(remotes),
                            current, answer, release] {
            std::string error;
            for (const auto &[index, url] : remotes) {
                error = download(*downloads, url, (*files)[index]);
                if (!error.empty()) break;
            }
            finished->store(true);
            post([this, alive, downloads, files, current, answer, error, release] {
                if (!*alive || !error.empty() || downloads->stopped()) {
                    release(*files);
                    (void)answer->fail(error.empty() ? std::string("download failed: stopped") : error);
                    return;
                }
                showPreview(std::move(*files), current, std::move(*answer));
            });
        });
        loaders_.push_back(Loader{std::move(thread), std::move(downloads), std::move(finished)});
    }

    void showPreview(std::vector<HANDLE> files, int current, HostServiceReply reply) {
        // Another preview replaces the one shown.
        viewer_.reset();
        std::vector<void *> held(files.begin(), files.end());
        std::shared_ptr<bool> alive = alive_;
        const std::uint64_t shown = ++viewers_shown_;
        viewer_ = std::make_unique<MediaViewer>(owner_, std::move(held), current, [this, alive, shown] {
            // Gone once the message that closed it is handled -- unless another
            // preview has replaced it by then.
            post_([this, alive, shown] {
                if (*alive && viewers_shown_ == shown) viewer_.reset();
            });
        });
        (void)reply.succeed();
    }

    const HWND owner_;
    // Where the downloads' and the viewer's news reach the GUI thread; what
    // still waits when the service goes is dropped with it.
    GuiQueue queue_;
    const GuiQueue::Post post_;
    const Places places_;
    std::shared_ptr<bool> alive_ = std::make_shared<bool>(true);
    std::shared_ptr<Staging> staging_;

    std::thread dialog_thread_;
    const HANDLE dialog_done_;
    std::shared_ptr<std::atomic<bool>> dialog_busy_ = std::make_shared<std::atomic<bool>>(false);
    std::shared_ptr<std::atomic<bool>> dialog_cancelled_ = std::make_shared<std::atomic<bool>>(false);
    std::shared_ptr<std::atomic<DWORD>> dialog_thread_id_ = std::make_shared<std::atomic<DWORD>>(0);

    std::unique_ptr<MediaViewer> viewer_;
    std::uint64_t viewers_shown_ = 0;

    /// A preview's downloads, on a thread of their own.
    struct Loader {
        std::thread thread;
        std::shared_ptr<Downloads> downloads;
        std::shared_ptr<std::atomic<bool>> finished;
    };
    std::vector<Loader> loaders_;
};

Images::Images(HWND__ *owner, Places places) : impl_(std::make_unique<Impl>(owner, std::move(places))) {}

Images::~Images() = default;

void Images::call(std::uint32_t method, const Json &request, HostServiceReply reply) {
    impl_->call(method, request, std::move(reply));
}

}  // namespace migo::windows_host

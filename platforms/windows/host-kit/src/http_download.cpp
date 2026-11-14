#include "http_download.hpp"

#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#ifndef NOMINMAX
#define NOMINMAX
#endif
#include <windows.h>
#include <winhttp.h>

#include <vector>

namespace migo::windows_host {

namespace {

/// How long a download may go without a byte: the network calls' own default.
constexpr int kTransferTimeoutMs = 60'000;

}  // namespace

void Downloads::stop() {
    const std::lock_guard<std::mutex> lock(mutex_);
    stopped_ = true;
    if (request_ != nullptr) WinHttpCloseHandle(request_);
    request_ = nullptr;
}

bool Downloads::stopped() {
    const std::lock_guard<std::mutex> lock(mutex_);
    return stopped_;
}

bool Downloads::begin(void *request) {
    const std::lock_guard<std::mutex> lock(mutex_);
    if (stopped_) return false;
    request_ = request;
    return true;
}

void Downloads::end(void *request) {
    const std::lock_guard<std::mutex> lock(mutex_);
    if (request_ == request) WinHttpCloseHandle(request);
    request_ = nullptr;
}

std::string download(Downloads &downloads, const std::wstring &url, void *file, std::uint64_t limit) {
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
    WinHttpSetTimeouts(session, kTransferTimeoutMs, kTransferTimeoutMs, kTransferTimeoutMs, kTransferTimeoutMs);
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
            std::uint64_t total = 0;
            for (;;) {
                DWORD received = 0;
                if (!WinHttpReadData(request, buffer.data(), static_cast<DWORD>(buffer.size()), &received)) {
                    error = downloads.stopped() ? "download failed: stopped" : "download failed: the transfer broke off";
                    break;
                }
                if (received == 0) break;
                total += received;
                if (limit != 0 && total > limit) {
                    error = "download failed: larger than " + std::to_string(limit) + " bytes";
                    break;
                }
                DWORD written = 0;
                if (!WriteFile(static_cast<HANDLE>(file), buffer.data(), received, &written, nullptr) ||
                    written != received) {
                    const DWORD why = GetLastError();
                    error = why == ERROR_DISK_FULL || why == ERROR_HANDLE_DISK_FULL ? "download failed: the disk is full"
                                                                                     : "download failed: it cannot be kept";
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

bool isRemote(std::string_view source) noexcept {
    return source.rfind("http://", 0) == 0 || source.rfind("https://", 0) == 0;
}

}  // namespace migo::windows_host

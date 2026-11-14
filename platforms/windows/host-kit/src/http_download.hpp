#ifndef MIGO_WINDOWS_HOST_KIT_HTTP_DOWNLOAD_HPP_
#define MIGO_WINDOWS_HOST_KIT_HTTP_DOWNLOAD_HPP_

// Internal to the Host Kit, not installed: what the services download -- a
// preview's image, a video's poster -- through WinHTTP.

#include <cstdint>
#include <mutex>
#include <string>
#include <string_view>

namespace migo::windows_host {

/// Downloads that can be stopped from another thread: closing the request in
/// flight makes the call waiting on it return. Any thread.
class Downloads final {
public:
    void stop();
    [[nodiscard]] bool stopped();

private:
    friend std::string download(Downloads &downloads, const std::wstring &url, void *file, std::uint64_t limit);
    /// `request` is the one in flight; false when the downloads were stopped.
    bool begin(void *request);
    /// Closes `request` unless stopping did.
    void end(void *request);

    std::mutex mutex_;
    bool stopped_ = false;
    void *request_ = nullptr;
};

/// Downloads `url` (http or https) into `file`, an open file HANDLE, through
/// the system's proxy, no referrer sent: the host has none to send. At most
/// `limit` bytes when it is not 0. Empty on success, else why not -- "download
/// failed: ...". Blocks: called off the GUI thread.
std::string download(Downloads &downloads, const std::wstring &url, void *file, std::uint64_t limit = 0);

/// Whether `source` is an http(s) URL rather than a path.
[[nodiscard]] bool isRemote(std::string_view source) noexcept;

}  // namespace migo::windows_host

#endif  // MIGO_WINDOWS_HOST_KIT_HTTP_DOWNLOAD_HPP_

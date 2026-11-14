#ifndef MIGO_WINDOWS_HOST_KIT_STAGING_HPP_
#define MIGO_WINDOWS_HOST_KIT_STAGING_HPP_

// Internal to the Host Kit, not installed: where the files the services hand
// the Session wait until it takes them.

#include <string>
#include <string_view>

namespace migo::windows_host {

/// A directory of the Host Kit's own for the files it hands the Session -- a
/// picked image's copy, a compressed image -- which the Session takes as the
/// call is answered; what it does not take is removed then, and whatever is
/// left goes with the directory. Made in `root` -- the user's temporary
/// directory by default, under their local application data, the volume the
/// engine's cache is on more often than not, so taking a file is a rename
/// rather than a copy. The Linux Host Kit's, for Win32. Any thread once made.
class Staging final {
public:
    explicit Staging(const std::wstring &root);
    ~Staging();

    Staging(const Staging &) = delete;
    Staging &operator=(const Staging &) = delete;

    /// The user's temporary directory.
    [[nodiscard]] static std::wstring systemRoot();

    /// A new file's path, with `suffix` (no dot) when it has one; empty when
    /// there is nowhere to put one.
    [[nodiscard]] std::wstring path(std::wstring_view suffix) const;
    /// The directory, or empty when there is none.
    [[nodiscard]] const std::wstring &directory() const noexcept { return directory_; }

private:
    std::wstring directory_;
};

}  // namespace migo::windows_host

#endif  // MIGO_WINDOWS_HOST_KIT_STAGING_HPP_

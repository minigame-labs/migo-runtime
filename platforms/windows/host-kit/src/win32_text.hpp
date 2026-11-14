#ifndef MIGO_WINDOWS_HOST_KIT_WIN32_TEXT_HPP_
#define MIGO_WINDOWS_HOST_KIT_WIN32_TEXT_HPP_

// Internal to the Host Kit, not installed: the C ABI's text is UTF-8,
// Windows' UTF-16.

#include <string>
#include <string_view>

namespace migo::windows_host {

/// `utf8` as UTF-16; an ill-formed sequence becomes U+FFFD, as the system converts it.
[[nodiscard]] std::wstring wideOf(std::string_view utf8);
/// `wide` as UTF-8; an unpaired surrogate becomes U+FFFD.
[[nodiscard]] std::string utf8Of(std::wstring_view wide);

}  // namespace migo::windows_host

#endif  // MIGO_WINDOWS_HOST_KIT_WIN32_TEXT_HPP_

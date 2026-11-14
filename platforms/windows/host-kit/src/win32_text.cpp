#include "win32_text.hpp"

#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#ifndef NOMINMAX
#define NOMINMAX
#endif
#include <windows.h>

#include <climits>

namespace migo::windows_host {

std::wstring wideOf(std::string_view utf8) {
    if (utf8.empty() || utf8.size() > static_cast<std::size_t>(INT_MAX)) return {};
    const int length = static_cast<int>(utf8.size());
    const int units = MultiByteToWideChar(CP_UTF8, 0, utf8.data(), length, nullptr, 0);
    if (units <= 0) return {};
    std::wstring out(static_cast<std::size_t>(units), L'\0');
    MultiByteToWideChar(CP_UTF8, 0, utf8.data(), length, out.data(), units);
    return out;
}

std::string utf8Of(std::wstring_view wide) {
    if (wide.empty() || wide.size() > static_cast<std::size_t>(INT_MAX)) return {};
    const int length = static_cast<int>(wide.size());
    const int bytes = WideCharToMultiByte(CP_UTF8, 0, wide.data(), length, nullptr, 0, nullptr, nullptr);
    if (bytes <= 0) return {};
    std::string out(static_cast<std::size_t>(bytes), '\0');
    WideCharToMultiByte(CP_UTF8, 0, wide.data(), length, out.data(), bytes, nullptr, nullptr);
    return out;
}

}  // namespace migo::windows_host

#include "staging.hpp"

#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#ifndef NOMINMAX
#define NOMINMAX
#endif
#include <windows.h>
#include <objbase.h>

#include <cstdio>
#include <filesystem>
#include <system_error>

namespace migo::windows_host {

namespace {

/// A name no other file in the directory has: a fresh GUID's.
std::wstring unique_name() {
    GUID guid{};
    if (FAILED(CoCreateGuid(&guid))) return {};
    wchar_t text[40] = {};
    if (StringFromGUID2(guid, text, 40) == 0) return {};
    // Without its braces.
    std::wstring name(text + 1);
    if (!name.empty() && name.back() == L'}') name.pop_back();
    return name;
}

}  // namespace

std::wstring Staging::systemRoot() {
    wchar_t buffer[MAX_PATH + 1] = {};
    const DWORD length = GetTempPathW(MAX_PATH + 1, buffer);
    if (length == 0 || length > MAX_PATH) return {};
    return std::wstring(buffer, length);
}

Staging::Staging(const std::wstring &root) {
    const std::wstring base = root.empty() ? systemRoot() : root;
    if (base.empty()) {
        std::fprintf(stderr, "migo windows host kit: no staging directory for files: no temporary directory\n");
        return;
    }
    std::error_code error;
    std::filesystem::create_directories(base, error);
    const std::wstring name = unique_name();
    if (name.empty()) return;
    const std::filesystem::path directory = std::filesystem::path(base) / (L"migo-host-files-" + name);
    if (!std::filesystem::create_directory(directory, error)) {
        std::fprintf(stderr, "migo windows host kit: no staging directory for files: %s\n", error.message().c_str());
        return;
    }
    directory_ = directory.wstring();
}

Staging::~Staging() {
    if (directory_.empty()) return;
    std::error_code error;
    std::filesystem::remove_all(directory_, error);
}

std::wstring Staging::path(std::wstring_view suffix) const {
    if (directory_.empty()) return {};
    std::wstring name = unique_name();
    if (name.empty()) return {};
    if (!suffix.empty()) {
        name += L'.';
        name += suffix;
    }
    return (std::filesystem::path(directory_) / name).wstring();
}

}  // namespace migo::windows_host

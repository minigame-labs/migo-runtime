#include "dom_keys.hpp"

#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#ifndef NOMINMAX
#define NOMINMAX
#endif
#include <windows.h>

#include <array>
#include <cstring>

namespace migo::windows_host::detail {

namespace {

struct Named {
    std::uint32_t scan;
    const char *name;
};

// Scan codes without the E0 prefix: Chromium's keycode_converter table for
// Windows, the keys a desktop keyboard has.
constexpr Named kPlain[] = {
    {0x01, "Escape"},         {0x02, "Digit1"},       {0x03, "Digit2"},        {0x04, "Digit3"},
    {0x05, "Digit4"},         {0x06, "Digit5"},       {0x07, "Digit6"},        {0x08, "Digit7"},
    {0x09, "Digit8"},         {0x0A, "Digit9"},       {0x0B, "Digit0"},        {0x0C, "Minus"},
    {0x0D, "Equal"},          {0x0E, "Backspace"},    {0x0F, "Tab"},           {0x10, "KeyQ"},
    {0x11, "KeyW"},           {0x12, "KeyE"},         {0x13, "KeyR"},          {0x14, "KeyT"},
    {0x15, "KeyY"},           {0x16, "KeyU"},         {0x17, "KeyI"},          {0x18, "KeyO"},
    {0x19, "KeyP"},           {0x1A, "BracketLeft"},  {0x1B, "BracketRight"},  {0x1C, "Enter"},
    {0x1D, "ControlLeft"},    {0x1E, "KeyA"},         {0x1F, "KeyS"},          {0x20, "KeyD"},
    {0x21, "KeyF"},           {0x22, "KeyG"},         {0x23, "KeyH"},          {0x24, "KeyJ"},
    {0x25, "KeyK"},           {0x26, "KeyL"},         {0x27, "Semicolon"},     {0x28, "Quote"},
    {0x29, "Backquote"},      {0x2A, "ShiftLeft"},    {0x2B, "Backslash"},     {0x2C, "KeyZ"},
    {0x2D, "KeyX"},           {0x2E, "KeyC"},         {0x2F, "KeyV"},          {0x30, "KeyB"},
    {0x31, "KeyN"},           {0x32, "KeyM"},         {0x33, "Comma"},         {0x34, "Period"},
    {0x35, "Slash"},          {0x36, "ShiftRight"},   {0x37, "NumpadMultiply"}, {0x38, "AltLeft"},
    {0x39, "Space"},          {0x3A, "CapsLock"},     {0x3B, "F1"},            {0x3C, "F2"},
    {0x3D, "F3"},             {0x3E, "F4"},           {0x3F, "F5"},            {0x40, "F6"},
    {0x41, "F7"},             {0x42, "F8"},           {0x43, "F9"},            {0x44, "F10"},
    {0x45, "Pause"},          {0x46, "ScrollLock"},   {0x47, "Numpad7"},       {0x48, "Numpad8"},
    {0x49, "Numpad9"},        {0x4A, "NumpadSubtract"}, {0x4B, "Numpad4"},     {0x4C, "Numpad5"},
    {0x4D, "Numpad6"},        {0x4E, "NumpadAdd"},    {0x4F, "Numpad1"},       {0x50, "Numpad2"},
    {0x51, "Numpad3"},        {0x52, "Numpad0"},      {0x53, "NumpadDecimal"}, {0x56, "IntlBackslash"},
    {0x57, "F11"},            {0x58, "F12"},          {0x59, "NumpadEqual"},   {0x64, "F13"},
    {0x65, "F14"},            {0x66, "F15"},          {0x67, "F16"},           {0x68, "F17"},
    {0x69, "F18"},            {0x6A, "F19"},          {0x6B, "F20"},           {0x6C, "F21"},
    {0x6D, "F22"},            {0x6E, "F23"},          {0x70, "KanaMode"},      {0x73, "IntlRo"},
    {0x76, "F24"},            {0x79, "Convert"},      {0x7B, "NonConvert"},    {0x7D, "IntlYen"},
    {0x7E, "NumpadComma"},
};

// Scan codes with the E0 prefix.
constexpr Named kExtended[] = {
    {0x10, "MediaTrackPrevious"}, {0x19, "MediaTrackNext"}, {0x1C, "NumpadEnter"},   {0x1D, "ControlRight"},
    {0x20, "AudioVolumeMute"},    {0x21, "LaunchApp2"},     {0x22, "MediaPlayPause"}, {0x24, "MediaStop"},
    {0x2E, "AudioVolumeDown"},    {0x30, "AudioVolumeUp"},  {0x32, "BrowserHome"},   {0x35, "NumpadDivide"},
    {0x37, "PrintScreen"},        {0x38, "AltRight"},       {0x45, "NumLock"},       {0x47, "Home"},
    {0x48, "ArrowUp"},            {0x49, "PageUp"},         {0x4B, "ArrowLeft"},     {0x4D, "ArrowRight"},
    {0x4F, "End"},                {0x50, "ArrowDown"},      {0x51, "PageDown"},      {0x52, "Insert"},
    {0x53, "Delete"},             {0x5B, "MetaLeft"},       {0x5C, "MetaRight"},     {0x5D, "ContextMenu"},
    {0x5E, "Power"},              {0x65, "BrowserSearch"},  {0x66, "BrowserFavorites"}, {0x67, "BrowserRefresh"},
    {0x68, "BrowserStop"},        {0x69, "BrowserForward"}, {0x6A, "BrowserBack"},   {0x6B, "LaunchApp1"},
    {0x6C, "LaunchMail"},         {0x6D, "MediaSelect"},
};

struct NamedKey {
    std::uint32_t virtual_key;
    const char *name;
};

// The keys whose `key` is a name, not what they type.
constexpr NamedKey kNamedKeys[] = {
    {VK_BACK, "Backspace"},
    {VK_TAB, "Tab"},
    {VK_CLEAR, "Clear"},
    {VK_RETURN, "Enter"},
    {VK_SHIFT, "Shift"},
    {VK_LSHIFT, "Shift"},
    {VK_RSHIFT, "Shift"},
    {VK_CONTROL, "Control"},
    {VK_LCONTROL, "Control"},
    {VK_RCONTROL, "Control"},
    {VK_MENU, "Alt"},
    {VK_LMENU, "Alt"},
    {VK_RMENU, "Alt"},
    {VK_PAUSE, "Pause"},
    {VK_CAPITAL, "CapsLock"},
    {VK_KANA, "KanaMode"},
    {VK_JUNJA, "JunjaMode"},
    {VK_FINAL, "FinalMode"},
    {VK_KANJI, "KanjiMode"},
    {VK_ESCAPE, "Escape"},
    {VK_CONVERT, "Convert"},
    {VK_NONCONVERT, "NonConvert"},
    {VK_ACCEPT, "Accept"},
    {VK_MODECHANGE, "ModeChange"},
    {VK_PRIOR, "PageUp"},
    {VK_NEXT, "PageDown"},
    {VK_END, "End"},
    {VK_HOME, "Home"},
    {VK_LEFT, "ArrowLeft"},
    {VK_UP, "ArrowUp"},
    {VK_RIGHT, "ArrowRight"},
    {VK_DOWN, "ArrowDown"},
    {VK_SELECT, "Select"},
    {VK_PRINT, "Print"},
    {VK_EXECUTE, "Execute"},
    {VK_SNAPSHOT, "PrintScreen"},
    {VK_INSERT, "Insert"},
    {VK_DELETE, "Delete"},
    {VK_HELP, "Help"},
    {VK_LWIN, "Meta"},
    {VK_RWIN, "Meta"},
    {VK_APPS, "ContextMenu"},
    {VK_SLEEP, "Standby"},
    {VK_NUMLOCK, "NumLock"},
    {VK_SCROLL, "ScrollLock"},
    {VK_BROWSER_BACK, "BrowserBack"},
    {VK_BROWSER_FORWARD, "BrowserForward"},
    {VK_BROWSER_REFRESH, "BrowserRefresh"},
    {VK_BROWSER_STOP, "BrowserStop"},
    {VK_BROWSER_SEARCH, "BrowserSearch"},
    {VK_BROWSER_FAVORITES, "BrowserFavorites"},
    {VK_BROWSER_HOME, "BrowserHome"},
    {VK_VOLUME_MUTE, "AudioVolumeMute"},
    {VK_VOLUME_DOWN, "AudioVolumeDown"},
    {VK_VOLUME_UP, "AudioVolumeUp"},
    {VK_MEDIA_NEXT_TRACK, "MediaTrackNext"},
    {VK_MEDIA_PREV_TRACK, "MediaTrackPrevious"},
    {VK_MEDIA_STOP, "MediaStop"},
    {VK_MEDIA_PLAY_PAUSE, "MediaPlayPause"},
    {VK_LAUNCH_MAIL, "LaunchMail"},
    {VK_LAUNCH_MEDIA_SELECT, "LaunchMediaPlayer"},
    {VK_LAUNCH_APP1, "LaunchApplication1"},
    {VK_LAUNCH_APP2, "LaunchApplication2"},
    {VK_PROCESSKEY, "Process"},
    {VK_ATTN, "Attn"},
    {VK_CRSEL, "CrSel"},
    {VK_EXSEL, "ExSel"},
    {VK_EREOF, "EraseEof"},
    {VK_PLAY, "Play"},
    {VK_ZOOM, "ZoomToggle"},
};

std::size_t emit(const char *name, char *buffer, std::size_t capacity) noexcept {
    const std::size_t length = std::strlen(name);
    if (length > capacity) return 0;
    std::memcpy(buffer, name, length);
    return length;
}

constexpr bool down(const std::uint8_t *state, int virtual_key) noexcept { return (state[virtual_key] & 0x80) != 0; }

}  // namespace

const char *dom_code(std::uint32_t scan_code, bool extended) noexcept {
    const std::uint32_t scan = scan_code & 0xFF;
    if (extended) {
        for (const Named &entry : kExtended) {
            if (entry.scan == scan) return entry.name;
        }
    } else {
        for (const Named &entry : kPlain) {
            if (entry.scan == scan) return entry.name;
        }
    }
    return "Unidentified";
}

std::size_t dom_key(std::uint32_t virtual_key, std::uint32_t scan_code, bool extended, const std::uint8_t *state,
                    void *layout, char *buffer, std::size_t capacity) noexcept {
    for (const NamedKey &entry : kNamedKeys) {
        if (entry.virtual_key == virtual_key) return emit(entry.name, buffer, capacity);
    }
    if (virtual_key >= VK_F1 && virtual_key <= VK_F24) {
        char name[4] = {'F', 0, 0, 0};
        const std::uint32_t number = virtual_key - VK_F1 + 1;
        if (number < 10) {
            name[1] = static_cast<char>('0' + number);
        } else {
            name[1] = static_cast<char>('0' + number / 10);
            name[2] = static_cast<char>('0' + number % 10);
        }
        return emit(name, buffer, capacity);
    }
    // What the key types. Control alone is not part of it -- Ctrl+A's key is
    // "a" -- but Control with Alt is AltGr, which picks the character.
    std::array<std::uint8_t, 256> keys{};
    std::memcpy(keys.data(), state, keys.size());
    if (down(state, VK_CONTROL) && !down(state, VK_MENU)) {
        keys[VK_CONTROL] = keys[VK_LCONTROL] = keys[VK_RCONTROL] = 0;
    }
    wchar_t typed[8] = {};
    const UINT scan = (scan_code & 0xFF) | (extended ? 0xE000U : 0U);
    // Flag 4: the keyboard's state is not changed, so a dead key pending in it
    // still combines with the next key.
    const int produced = ToUnicodeEx(virtual_key, scan, keys.data(), typed, 8, 4, static_cast<HKL>(layout));
    if (produced < 0) return emit("Dead", buffer, capacity);
    if (produced == 0 || typed[0] < 0x20 || typed[0] == 0x7F) return emit("Unidentified", buffer, capacity);
    const int bytes = WideCharToMultiByte(CP_UTF8, WC_ERR_INVALID_CHARS, typed, produced, nullptr, 0, nullptr, nullptr);
    if (bytes <= 0 || static_cast<std::size_t>(bytes) > capacity) return 0;
    return static_cast<std::size_t>(
        WideCharToMultiByte(CP_UTF8, WC_ERR_INVALID_CHARS, typed, produced, buffer, bytes, nullptr, nullptr));
}

}  // namespace migo::windows_host::detail

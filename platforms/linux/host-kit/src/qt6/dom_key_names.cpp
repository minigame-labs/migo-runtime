#include "dom_key_names.hpp"

#include <QChar>
#include <QString>

#include <cstring>

namespace migo::linux_host::qt6::detail {

namespace {

// X11 hardware keycodes are evdev codes plus this offset on every server that
// uses the evdev/libinput driver, which is every modern Linux desktop.
constexpr std::uint32_t kX11EvdevOffset = 8;

// Indexed by evdev code (linux/input-event-codes.h). A nullptr entry is a code
// this table does not name; the lookup reports "Unidentified" for it rather
// than guessing, because a wrong `code` is worse than an unknown one -- content
// would act on a key the user did not press.
constexpr const char *kEvdevToDomCode[] = {
    /*   0 */ nullptr,      "Escape",       "Digit1",       "Digit2",
    /*   4 */ "Digit3",     "Digit4",       "Digit5",       "Digit6",
    /*   8 */ "Digit7",     "Digit8",       "Digit9",       "Digit0",
    /*  12 */ "Minus",      "Equal",        "Backspace",    "Tab",
    /*  16 */ "KeyQ",       "KeyW",         "KeyE",         "KeyR",
    /*  20 */ "KeyT",       "KeyY",         "KeyU",         "KeyI",
    /*  24 */ "KeyO",       "KeyP",         "BracketLeft",  "BracketRight",
    /*  28 */ "Enter",      "ControlLeft",  "KeyA",         "KeyS",
    /*  32 */ "KeyD",       "KeyF",         "KeyG",         "KeyH",
    /*  36 */ "KeyJ",       "KeyK",         "KeyL",         "Semicolon",
    /*  40 */ "Quote",      "Backquote",    "ShiftLeft",    "Backslash",
    /*  44 */ "KeyZ",       "KeyX",         "KeyC",         "KeyV",
    /*  48 */ "KeyB",       "KeyN",         "KeyM",         "Comma",
    /*  52 */ "Period",     "Slash",        "ShiftRight",   "NumpadMultiply",
    /*  56 */ "AltLeft",    "Space",        "CapsLock",     "F1",
    /*  60 */ "F2",         "F3",           "F4",           "F5",
    /*  64 */ "F6",         "F7",           "F8",           "F9",
    /*  68 */ "F10",        "NumLock",      "ScrollLock",   "Numpad7",
    /*  72 */ "Numpad8",    "Numpad9",      "NumpadSubtract", "Numpad4",
    /*  76 */ "Numpad5",    "Numpad6",      "NumpadAdd",    "Numpad1",
    /*  80 */ "Numpad2",    "Numpad3",      "Numpad0",      "NumpadDecimal",
    /*  84 */ nullptr,      nullptr,        "IntlBackslash", "F11",
    /*  88 */ "F12",        "IntlRo",       nullptr,        nullptr,
    /*  92 */ nullptr,      nullptr,        nullptr,        "KanaMode",
    /*  96 */ "NumpadEnter", "ControlRight", "NumpadDivide", "PrintScreen",
    /* 100 */ "AltRight",   nullptr,        "Home",         "ArrowUp",
    /* 104 */ "PageUp",     "ArrowLeft",    "ArrowRight",   "End",
    /* 108 */ "ArrowDown",  "PageDown",     "Insert",       "Delete",
    /* 112 */ nullptr,      "AudioVolumeMute", "AudioVolumeDown", "AudioVolumeUp",
    /* 116 */ "Power",      "NumpadEqual",  nullptr,        "Pause",
    /* 120 */ nullptr,      "NumpadComma",  "Lang1",        "Lang2",
    /* 124 */ "IntlYen",    "MetaLeft",     "MetaRight",    "ContextMenu",
};

constexpr std::size_t kEvdevTableSize = sizeof(kEvdevToDomCode) / sizeof(kEvdevToDomCode[0]);

struct NamedKey {
    int qt_key;
    const char *dom_key;
};

// DOM key names for the keys whose meaning is not their text. Qt reports a
// control character for several of these (Enter is "\r", Escape is "\x1b"), so
// checking the name first is what keeps them out of content's text handling.
constexpr NamedKey kNamedKeys[] = {
    {Qt::Key_Escape, "Escape"},        {Qt::Key_Tab, "Tab"},
    {Qt::Key_Backtab, "Tab"},          {Qt::Key_Backspace, "Backspace"},
    {Qt::Key_Return, "Enter"},         {Qt::Key_Enter, "Enter"},
    {Qt::Key_Insert, "Insert"},        {Qt::Key_Delete, "Delete"},
    {Qt::Key_Pause, "Pause"},          {Qt::Key_Print, "PrintScreen"},
    {Qt::Key_Home, "Home"},            {Qt::Key_End, "End"},
    {Qt::Key_Left, "ArrowLeft"},       {Qt::Key_Up, "ArrowUp"},
    {Qt::Key_Right, "ArrowRight"},     {Qt::Key_Down, "ArrowDown"},
    {Qt::Key_PageUp, "PageUp"},        {Qt::Key_PageDown, "PageDown"},
    {Qt::Key_Shift, "Shift"},          {Qt::Key_Control, "Control"},
    {Qt::Key_Meta, "Meta"},            {Qt::Key_Alt, "Alt"},
    {Qt::Key_AltGr, "AltGraph"},       {Qt::Key_CapsLock, "CapsLock"},
    {Qt::Key_NumLock, "NumLock"},      {Qt::Key_ScrollLock, "ScrollLock"},
    {Qt::Key_Menu, "ContextMenu"},     {Qt::Key_Help, "Help"},
    {Qt::Key_Clear, "Clear"},          {Qt::Key_Space, " "},
};

}  // namespace

const char *dom_code_from_x11_keycode(std::uint32_t native_scan_code) noexcept {
    if (native_scan_code < kX11EvdevOffset) return "Unidentified";
    const std::uint32_t evdev = native_scan_code - kX11EvdevOffset;
    if (evdev >= kEvdevTableSize) return "Unidentified";
    const char *code = kEvdevToDomCode[evdev];
    return code != nullptr ? code : "Unidentified";
}

namespace {

/// Encode one Unicode code point as UTF-8, without allocating.
std::size_t encode_utf8(char32_t code_point, char *out) {
    if (code_point < 0x80) {
        out[0] = static_cast<char>(code_point);
        return 1;
    }
    if (code_point < 0x800) {
        out[0] = static_cast<char>(0xC0 | (code_point >> 6));
        out[1] = static_cast<char>(0x80 | (code_point & 0x3F));
        return 2;
    }
    if (code_point < 0x10000) {
        // A lone surrogate is not a character; reporting it would put invalid
        // UTF-8 across the boundary, which the C ABI rejects outright.
        if (code_point >= 0xD800 && code_point <= 0xDFFF) return 0;
        out[0] = static_cast<char>(0xE0 | (code_point >> 12));
        out[1] = static_cast<char>(0x80 | ((code_point >> 6) & 0x3F));
        out[2] = static_cast<char>(0x80 | (code_point & 0x3F));
        return 3;
    }
    if (code_point <= 0x10FFFF) {
        out[0] = static_cast<char>(0xF0 | (code_point >> 18));
        out[1] = static_cast<char>(0x80 | ((code_point >> 12) & 0x3F));
        out[2] = static_cast<char>(0x80 | ((code_point >> 6) & 0x3F));
        out[3] = static_cast<char>(0x80 | (code_point & 0x3F));
        return 4;
    }
    return 0;
}

/// Copy a name into the caller's buffer, or report that it does not fit.
std::size_t emit_name(const char *name, char *buffer, std::size_t capacity) {
    const std::size_t length = std::strlen(name);
    if (length > capacity) return 0;
    std::memcpy(buffer, name, length);
    return length;
}

}  // namespace

std::size_t dom_key_from_qt_event(const QKeyEvent &event, char *buffer, std::size_t capacity) {
    if (buffer == nullptr) return 0;
    const int key = event.key();

    for (const NamedKey &named : kNamedKeys) {
        if (named.qt_key == key) return emit_name(named.dom_key, buffer, capacity);
    }

    if (key >= Qt::Key_F1 && key <= Qt::Key_F35) {
        char name[8] = {};
        const int number = key - Qt::Key_F1 + 1;
        name[0] = 'F';
        if (number < 10) {
            name[1] = static_cast<char>('0' + number);
        } else {
            name[1] = static_cast<char>('0' + number / 10);
            name[2] = static_cast<char>('0' + number % 10);
        }
        return emit_name(name, buffer, capacity);
    }

    // The text Qt produced, unless it is a control character. Ctrl+A reports
    // "\x01" there; DOM reports "a", which the layout-aware branch below
    // reconstructs.
    //
    // Everything here works on QChar code units rather than QString/QByteArray:
    // neither has a small-string optimisation in Qt 6, so building either one
    // would allocate on every press. QKeyEvent::text() itself is implicitly
    // shared, so reading it does not.
    const QString text = event.text();
    char16_t units[2] = {};
    int unit_count = 0;
    if (!text.isEmpty() && text.at(0).unicode() >= 0x20 && text.at(0).unicode() != 0x7F) {
        units[0] = text.at(0).unicode();
        unit_count = 1;
        if (text.size() > 1 && text.at(0).isHighSurrogate() && text.at(1).isLowSurrogate()) {
            units[1] = text.at(1).unicode();
            unit_count = 2;
        }
    } else if (key > 0 && key <= 0xFF) {
        // A printable key whose text a modifier suppressed. Qt::Key values in
        // the Latin-1 range are the uppercase character, so the shift state
        // decides the case the same way the DOM does.
        const QChar character(static_cast<char16_t>(key));
        const bool shifted = (event.modifiers() & Qt::ShiftModifier) != 0;
        units[0] = (shifted ? character.toUpper() : character.toLower()).unicode();
        unit_count = 1;
    } else if (key == Qt::Key_Dead_Grave || key == Qt::Key_Dead_Acute ||
               key == Qt::Key_Dead_Circumflex || key == Qt::Key_Dead_Tilde ||
               key == Qt::Key_Dead_Diaeresis) {
        // A dead key produces no text of its own; the DOM names that state
        // rather than leaving it blank.
        return emit_name("Dead", buffer, capacity);
    } else {
        return emit_name("Unidentified", buffer, capacity);
    }

    char32_t code_point = units[0];
    if (unit_count == 2) {
        code_point = 0x10000 + ((static_cast<char32_t>(units[0]) - 0xD800) << 10) +
                     (static_cast<char32_t>(units[1]) - 0xDC00);
    }

    char encoded[5] = {};
    const std::size_t encoded_length = encode_utf8(code_point, encoded);
    if (encoded_length == 0) return emit_name("Unidentified", buffer, capacity);
    if (encoded_length > capacity) return 0;
    std::memcpy(buffer, encoded, encoded_length);
    return encoded_length;
}

}  // namespace migo::linux_host::qt6::detail

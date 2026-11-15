#ifndef MIGO_LINUX_HOST_KIT_QT6_DOM_KEY_NAMES_HPP_
#define MIGO_LINUX_HOST_KIT_QT6_DOM_KEY_NAMES_HPP_

#include <QKeyEvent>

#include <cstddef>
#include <cstdint>

namespace migo::linux_host::qt6::detail {

/// DOM `KeyboardEvent.code` for an X11 hardware keycode.
///
/// `code` names the physical key and must not depend on the layout: on a French
/// keyboard the key that produces "a" is still `KeyQ`. Deriving it from
/// `QKeyEvent::key()` -- which is the layout's answer -- is the classic mistake,
/// and it makes WASD movement break for every non-QWERTY user while looking
/// correct to the developer.
///
/// X11 hardware keycodes are evdev codes offset by 8 on every modern server, so
/// the table below is indexed by evdev code and the offset is removed here.
///
/// Returns "Unidentified" for a key this table does not name, which is the DOM's
/// own answer and is what the C ABI requires: it rejects an empty `code`,
/// because a code always identifies something.
[[nodiscard]] const char *dom_code_from_x11_keycode(std::uint32_t native_scan_code) noexcept;

/// DOM `KeyboardEvent.key` for a Qt key event.
///
/// `key` is what the press produces given the current layout and modifiers, so
/// unlike `code` it comes from Qt's interpretation. Named keys ("ArrowLeft",
/// "Enter", "Shift") take priority over the event's text, because Qt reports a
/// control character for several of them.
///
/// The result is UTF-8 and may legitimately be empty: a dead key produces no
/// text, and the C ABI accepts an empty `key` for exactly that reason.
///
/// Written into a caller-owned buffer rather than returned as a container so
/// the key path allocates nothing. `QByteArray` has no small-string
/// optimisation in Qt 6, so returning one would put a heap allocation on every
/// press. Returns the number of bytes written, or 0 when the name does not fit,
/// which the caller reports as an unidentified key rather than a truncated one:
/// half of a UTF-8 sequence is not a shorter name, it is invalid text.
[[nodiscard]] std::size_t dom_key_from_qt_event(const QKeyEvent &event, char *buffer,
                                                std::size_t capacity);

}  // namespace migo::linux_host::qt6::detail

#endif  // MIGO_LINUX_HOST_KIT_QT6_DOM_KEY_NAMES_HPP_

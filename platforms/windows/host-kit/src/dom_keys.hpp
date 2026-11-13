#ifndef MIGO_WINDOWS_HOST_KIT_DOM_KEYS_HPP_
#define MIGO_WINDOWS_HOST_KIT_DOM_KEYS_HPP_

// Internal to the Windows Host Kit, not installed: the DOM's names for a key
// Windows reports.

#include <cstddef>
#include <cstdint>

namespace migo::windows_host::detail {

/// DOM `KeyboardEvent.code` for a key's scan code, as WM_KEYDOWN's lParam
/// carries it: bits 16-23, and bit 24 for the extended (E0) keys.
///
/// `code` names the physical key and must not depend on the layout -- the key
/// that types "a" on a French keyboard is still `KeyQ` -- so it comes from the
/// scan code, never from the virtual key, which is the layout's answer. The
/// table is Chromium's for Windows, its quirks included: Windows reports NumLock
/// as E0 45 and Pause as 45.
///
/// "Unidentified" for a key the table does not name: the DOM's own answer, and
/// what the C ABI needs -- it refuses an empty `code`.
[[nodiscard]] const char *dom_code(std::uint32_t scan_code, bool extended) noexcept;

/// DOM `KeyboardEvent.key` for a key press: its name when it has one
/// ("ArrowLeft", "Enter", "Shift"), otherwise what it types with the keyboard
/// state `state` (256 bytes, GetKeyboardState's) in the layout `layout` --
/// "Dead" for a dead key, "Unidentified" for a key that types nothing.
///
/// Read without changing the keyboard's state (ToUnicodeEx's flag 4), so a
/// dead key still combines with the key that follows it.
///
/// Written as UTF-8 into the caller's buffer, so the key path allocates
/// nothing; returns the bytes written, or 0 when the name does not fit.
[[nodiscard]] std::size_t dom_key(std::uint32_t virtual_key, std::uint32_t scan_code, bool extended,
                                  const std::uint8_t *state, void *layout, char *buffer,
                                  std::size_t capacity) noexcept;

}  // namespace migo::windows_host::detail

#endif  // MIGO_WINDOWS_HOST_KIT_DOM_KEYS_HPP_

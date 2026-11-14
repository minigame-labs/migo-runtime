#ifndef MIGO_WINDOWS_HOST_KIT_BRIGHTNESS_HPP_
#define MIGO_WINDOWS_HOST_KIT_BRIGHTNESS_HPP_

// Internal to the Host Kit, not installed: the brightness of the screen the
// game is on.

#include <optional>
#include <string>

struct HWND__;

namespace migo::windows_host {

/// The brightness of the screen `window` is on, 0 to 1, as Windows controls it:
/// an external monitor through DDC/CI (the monitor configuration API), a
/// laptop's own panel through WMI (WmiMonitorBrightness). Both can take tens
/// of milliseconds -- a monitor answers over its cable -- so they are called
/// off the GUI thread, in a multithreaded apartment. None, or a reason, where
/// the screen has no brightness the system controls.
[[nodiscard]] std::optional<double> screenBrightness(HWND__ *window);
/// Sets it; empty on success, else why not.
[[nodiscard]] std::string setScreenBrightness(HWND__ *window, double value);

/// What both say when the screen has none.
inline constexpr char kNoBrightness[] = "the screen has no brightness the system controls";

}  // namespace migo::windows_host

#endif  // MIGO_WINDOWS_HOST_KIT_BRIGHTNESS_HPP_

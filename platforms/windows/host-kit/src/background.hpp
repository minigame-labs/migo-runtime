#ifndef MIGO_WINDOWS_HOST_KIT_BACKGROUND_HPP_
#define MIGO_WINDOWS_HOST_KIT_BACKGROUND_HPP_

// Internal to the Host Kit, not installed: work that must not hold the GUI thread.

#include <functional>

namespace migo::windows_host {

/// Runs `work` on the system's thread pool, in the multithreaded COM
/// apartment -- a file copied, an image encoded, a monitor asked over its
/// cable. What it captures outlives the call; it answers through the
/// Session's channel, which any thread may.
void runInBackground(std::function<void()> work);

}  // namespace migo::windows_host

#endif  // MIGO_WINDOWS_HOST_KIT_BACKGROUND_HPP_

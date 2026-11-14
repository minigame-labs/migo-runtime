#ifndef MIGO_WINDOWS_HOST_KIT_GAMEPADS_HPP_
#define MIGO_WINDOWS_HOST_KIT_GAMEPADS_HPP_

// Internal to the Host Kit, not installed: the gamepads Windows sees.

#include <migo/migo.h>

#include <functional>
#include <memory>

namespace migo::windows_host {

/// The gamepads Windows.Gaming.Input sees -- the Linux Host Kit's `Gamepads`,
/// for Win32 -- found as they come and go. An Xbox-class pad is mapped onto
/// the W3C standard layout (17 buttons, 4 axes, the triggers analog) and has
/// dual and trigger rumble; any other game controller is passed on as it is,
/// its buttons and axes in the order the driver gives them, mapping "", no
/// rumble -- unless it has more of either than the engine holds, when it is
/// left out rather than cut short.
///
/// Read while the game runs (`setActive`), sixty times a second, a sample sent
/// for each new reading; the system gives a desktop App's pads readings while
/// it is in the foreground. Timestamps are the readings' own, in milliseconds.
/// GUI thread only; the system's news of pads coming and going is brought to
/// it.
class Gamepads final {
public:
    /// What the Session is told: a pad connected or withdrawn, and its samples.
    struct Sink {
        std::function<void(const MigoGamepadInfo &info, bool connected)> connection;
        std::function<void(const MigoGamepadStateEvent &sample)> sample;
    };

    explicit Gamepads(Sink sink);
    /// Stops what rumbles.
    ~Gamepads();

    Gamepads(const Gamepads &) = delete;
    Gamepads &operator=(const Gamepads &) = delete;

    /// Every pad announced again: the Session had nothing to announce to before.
    void announceAll();
    /// Whether the pads are read: while the game runs with a surface.
    void setActive(bool active);
    /// Content's playEffect, from on_gamepad_rumble.
    void rumble(const MigoGamepadRumble &rumble);

private:
    class Impl;
    std::unique_ptr<Impl> impl_;
};

}  // namespace migo::windows_host

#endif  // MIGO_WINDOWS_HOST_KIT_GAMEPADS_HPP_

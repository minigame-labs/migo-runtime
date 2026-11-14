#ifndef MIGO_WINDOWS_HOST_KIT_GAMEPAD_MAPPING_HPP_
#define MIGO_WINDOWS_HOST_KIT_GAMEPAD_MAPPING_HPP_

// Internal to the Host Kit, not installed: an Xbox-class pad's reading on the
// W3C standard gamepad layout.

#include <migo/migo.h>

#include <array>
#include <cstdint>

namespace migo::windows_host {

/// What Windows.Gaming.Input reads from an Xbox-class pad (GamepadReading),
/// as plain values: its buttons as GamepadButtons flags, its triggers 0..1,
/// its sticks -1..1 with up positive.
struct XboxReading {
    std::uint32_t buttons = 0;
    double left_trigger = 0;
    double right_trigger = 0;
    double left_x = 0;
    double left_y = 0;
    double right_x = 0;
    double right_y = 0;
};

/// The standard layout's 17 buttons and 4 axes.
struct StandardSample {
    static constexpr std::uint32_t kButtons = 17;
    static constexpr std::uint32_t kAxes = 4;
    std::array<MigoGamepadButton, kButtons> buttons{};
    std::array<float, kAxes> axes{};
};

/// GamepadButtons' flags, as Windows.Gaming.Input numbers them.
namespace xbox {
constexpr std::uint32_t kMenu = 0x1;
constexpr std::uint32_t kView = 0x2;
constexpr std::uint32_t kA = 0x4;
constexpr std::uint32_t kB = 0x8;
constexpr std::uint32_t kX = 0x10;
constexpr std::uint32_t kY = 0x20;
constexpr std::uint32_t kDPadUp = 0x40;
constexpr std::uint32_t kDPadDown = 0x80;
constexpr std::uint32_t kDPadLeft = 0x100;
constexpr std::uint32_t kDPadRight = 0x200;
constexpr std::uint32_t kLeftShoulder = 0x400;
constexpr std::uint32_t kRightShoulder = 0x800;
constexpr std::uint32_t kLeftThumbstick = 0x1000;
constexpr std::uint32_t kRightThumbstick = 0x2000;
}  // namespace xbox

/// An analog trigger counts as pressed past XInput's own threshold, 30 of 255.
constexpr double kTriggerPressed = 30.0 / 255.0;

/// The reading on the standard layout: A B X Y, the shoulders, the triggers as
/// analog buttons, View and Menu, the stick clicks, the D-pad, and the centre
/// button -- which Windows.Gaming.Input keeps for the system, never pressed.
/// The sticks' up is the layout's -1.
[[nodiscard]] inline StandardSample standardSample(const XboxReading &reading) noexcept {
    StandardSample sample;
    // A digital button is touched while it is pressed.
    const auto press = [&sample](std::uint32_t index, bool pressed) {
        sample.buttons[index].flags =
            pressed ? (MIGO_GAMEPAD_BUTTON_FLAG_PRESSED | MIGO_GAMEPAD_BUTTON_FLAG_TOUCHED) : MIGO_GAMEPAD_BUTTON_FLAG_NONE;
        sample.buttons[index].value = pressed ? 1.0F : 0.0F;
    };
    const auto held = [&reading](std::uint32_t flag) { return (reading.buttons & flag) != 0; };
    press(0, held(xbox::kA));
    press(1, held(xbox::kB));
    press(2, held(xbox::kX));
    press(3, held(xbox::kY));
    press(4, held(xbox::kLeftShoulder));
    press(5, held(xbox::kRightShoulder));
    // A trigger is touched as soon as it moves, pressed past the threshold.
    const auto trigger = [&sample](std::uint32_t index, double value) {
        const double clamped = value < 0 ? 0 : (value > 1 ? 1 : value);
        sample.buttons[index].value = static_cast<float>(clamped);
        sample.buttons[index].flags = (clamped > kTriggerPressed ? MIGO_GAMEPAD_BUTTON_FLAG_PRESSED : 0U) |
                                      (clamped > 0 ? MIGO_GAMEPAD_BUTTON_FLAG_TOUCHED : 0U);
    };
    trigger(6, reading.left_trigger);
    trigger(7, reading.right_trigger);
    press(8, held(xbox::kView));
    press(9, held(xbox::kMenu));
    press(10, held(xbox::kLeftThumbstick));
    press(11, held(xbox::kRightThumbstick));
    press(12, held(xbox::kDPadUp));
    press(13, held(xbox::kDPadDown));
    press(14, held(xbox::kDPadLeft));
    press(15, held(xbox::kDPadRight));
    press(16, false);
    const auto axis = [](double value) {
        return static_cast<float>(value < -1 ? -1 : (value > 1 ? 1 : value));
    };
    sample.axes[0] = axis(reading.left_x);
    sample.axes[1] = axis(-reading.left_y);
    sample.axes[2] = axis(reading.right_x);
    sample.axes[3] = axis(-reading.right_y);
    return sample;
}

}  // namespace migo::windows_host

#endif  // MIGO_WINDOWS_HOST_KIT_GAMEPAD_MAPPING_HPP_

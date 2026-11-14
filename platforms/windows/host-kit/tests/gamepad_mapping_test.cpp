// An Xbox-class pad's reading on the W3C standard gamepad layout.

#include "../src/gamepad_mapping.hpp"

#include <cmath>
#include <cstdio>

namespace {

int g_failures = 0;

#define CHECK(condition)                                                                        \
    do {                                                                                        \
        if (!(condition)) {                                                                     \
            std::fprintf(stderr, "%s:%d: CHECK failed: %s\n", __FILE__, __LINE__, #condition); \
            ++g_failures;                                                                       \
        }                                                                                       \
    } while (false)

using migo::windows_host::standardSample;
using migo::windows_host::StandardSample;
using migo::windows_host::XboxReading;
namespace xbox = migo::windows_host::xbox;

constexpr MigoGamepadButtonFlags kHeld = MIGO_GAMEPAD_BUTTON_FLAG_PRESSED | MIGO_GAMEPAD_BUTTON_FLAG_TOUCHED;

bool held(const StandardSample &sample, unsigned index) {
    return sample.buttons[index].flags == kHeld && sample.buttons[index].value == 1.0F;
}

bool released(const StandardSample &sample, unsigned index) {
    return sample.buttons[index].flags == MIGO_GAMEPAD_BUTTON_FLAG_NONE && sample.buttons[index].value == 0.0F;
}

void each_button_has_its_place() {
    const struct {
        std::uint32_t flag;
        unsigned index;
    } kPlaces[] = {
        {xbox::kA, 0},           {xbox::kB, 1},           {xbox::kX, 2},
        {xbox::kY, 3},           {xbox::kLeftShoulder, 4}, {xbox::kRightShoulder, 5},
        {xbox::kView, 8},        {xbox::kMenu, 9},        {xbox::kLeftThumbstick, 10},
        {xbox::kRightThumbstick, 11}, {xbox::kDPadUp, 12}, {xbox::kDPadDown, 13},
        {xbox::kDPadLeft, 14},   {xbox::kDPadRight, 15},
    };
    for (const auto &place : kPlaces) {
        XboxReading reading;
        reading.buttons = place.flag;
        const StandardSample sample = standardSample(reading);
        for (unsigned index = 0; index < StandardSample::kButtons; ++index) {
            if (index == place.index) {
                CHECK(held(sample, index));
            } else {
                CHECK(released(sample, index));
            }
        }
    }
    // The centre button is the system's: never pressed.
    XboxReading everything;
    everything.buttons = 0x3FFF;
    CHECK(released(standardSample(everything), 16));
}

void triggers_are_analog() {
    XboxReading reading;
    reading.left_trigger = 0.05;
    reading.right_trigger = 0.5;
    const StandardSample sample = standardSample(reading);
    // Touched as soon as it moves; pressed past XInput's threshold.
    CHECK(sample.buttons[6].flags == MIGO_GAMEPAD_BUTTON_FLAG_TOUCHED);
    CHECK(std::fabs(sample.buttons[6].value - 0.05F) < 1e-6F);
    CHECK(sample.buttons[7].flags == kHeld && sample.buttons[7].value == 0.5F);
    reading.right_trigger = 2;
    CHECK(standardSample(reading).buttons[7].value == 1.0F);
}

void up_is_minus_one() {
    XboxReading reading;
    reading.left_x = 0.25;
    reading.left_y = 1;
    reading.right_x = -1;
    reading.right_y = -0.5;
    const StandardSample sample = standardSample(reading);
    CHECK(sample.axes[0] == 0.25F && sample.axes[1] == -1.0F);
    CHECK(sample.axes[2] == -1.0F && sample.axes[3] == 0.5F);
}

}  // namespace

int main() {
    each_button_has_its_place();
    triggers_are_analog();
    up_is_minus_one();
    if (g_failures != 0) {
        std::fprintf(stderr, "%d gamepad mapping assertion(s) failed\n", g_failures);
        return 1;
    }
    std::puts("Gamepad mapping contract: PASS");
    return 0;
}

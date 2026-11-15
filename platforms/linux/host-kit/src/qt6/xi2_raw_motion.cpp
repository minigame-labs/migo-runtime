#include "xi2_raw_motion.hpp"

#include <xcb/xcb.h>
#include <xcb/xinput.h>

namespace migo::linux_host::qt6 {

namespace {

double fromFixed(const xcb_input_fp3232_t &value) noexcept {
    return static_cast<double>(value.integral) + static_cast<double>(value.frac) / 4294967296.0;
}

}  // namespace

std::optional<RawMotion> rawMotion(const void *event, std::uint8_t xi_opcode) noexcept {
    const auto *generic = static_cast<const xcb_ge_generic_event_t *>(event);
    if ((generic->response_type & 0x7F) != XCB_GE_GENERIC || generic->extension != xi_opcode ||
        generic->event_type != XCB_INPUT_RAW_MOTION) {
        return std::nullopt;
    }
    // A raw motion event is laid out as a raw button press is: a valuator mask,
    // then the values of the valuators it sets, in order -- the processed ones
    // first, the device's own after them. Qt hands a filter the event as xcb
    // read it, with xcb's full_sequence before the mask, which is the layout
    // xcb's accessors read.
    const auto *raw = static_cast<const xcb_input_raw_button_press_event_t *>(event);
    const std::uint32_t *mask = xcb_input_raw_button_press_valuator_mask(raw);
    const xcb_input_fp3232_t *values = xcb_input_raw_button_press_axisvalues(raw);
    const int bits = raw->valuators_len * 32;
    RawMotion motion;
    motion.time = raw->time;
    bool moved = false;
    int index = 0;
    for (int bit = 0; bit < bits; ++bit) {
        if ((mask[bit / 32] & (1U << (bit % 32))) == 0) continue;
        const double value = fromFixed(values[index++]);
        // A pointer's valuators 0 and 1 are its x and y.
        if (bit == 0) {
            motion.delta.setX(value);
            moved = moved || value != 0.0;
        } else if (bit == 1) {
            motion.delta.setY(value);
            moved = moved || value != 0.0;
        }
        if (bit >= 1) break;
    }
    return moved ? std::optional<RawMotion>(motion) : std::nullopt;
}

}  // namespace migo::linux_host::qt6

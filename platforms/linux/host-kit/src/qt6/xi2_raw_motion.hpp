#ifndef MIGO_LINUX_QT6_XI2_RAW_MOTION_HPP_
#define MIGO_LINUX_QT6_XI2_RAW_MOTION_HPP_

// Internal to the Host Kit, not installed: how the view reads the mouse's motion
// while a pointer lock holds the cursor.

#include <QPointF>

#include <cstdint>
#include <optional>

namespace migo::linux_host::qt6 {

/// One XInput 2 raw motion event's motion.
struct RawMotion {
    /// In the screen's pixels, after the pointer's acceleration: what would have
    /// moved the cursor, which is what a player's hand is used to.
    QPointF delta;
    /// The X server's time of the event, in milliseconds, as Qt's input events carry it.
    std::uint32_t time = 0;
};

/// The motion `event` carries when it is an XInput 2 raw motion event of the
/// extension with major opcode `xi_opcode` -- `event` is an xcb event as Qt's
/// native event filters receive it -- and moves either axis; none otherwise.
[[nodiscard]] std::optional<RawMotion> rawMotion(const void *event, std::uint8_t xi_opcode) noexcept;

}  // namespace migo::linux_host::qt6

#endif  // MIGO_LINUX_QT6_XI2_RAW_MOTION_HPP_

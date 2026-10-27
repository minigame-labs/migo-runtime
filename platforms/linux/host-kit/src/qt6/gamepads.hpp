#ifndef MIGO_LINUX_QT6_GAMEPADS_HPP_
#define MIGO_LINUX_QT6_GAMEPADS_HPP_

// Internal to the Host Kit, not installed: the gamepads the kernel shows.

#include <migo/migo.h>

#include <QByteArray>
#include <QHash>
#include <QObject>
#include <QString>

#include <cstdint>
#include <functional>
#include <map>
#include <memory>
#include <utility>
#include <vector>

class QSocketNotifier;
struct udev;
struct udev_monitor;

namespace migo::linux_host::qt6 {

/// The gamepads the kernel shows: evdev devices udev tags as joysticks, found
/// as they come and go. A pad that follows the kernel's gamepad layout
/// (Documentation/input/gamepad.rst: BTN_SOUTH and the sticks) is mapped onto
/// the W3C standard layout -- 17 buttons, 4 axes, the analog triggers and the
/// D-pad wherever the driver reports them -- and one that does not is passed on
/// as it is, its buttons and axes in code order, mapping "". A pad whose driver
/// plays FF_RUMBLE has dual rumble.
///
/// Events are read as they come, on the GUI thread, and a sample goes to the
/// Session at each report the device makes. Timestamps are CLOCK_MONOTONIC, the
/// clock the X server stamps the view's input with.
class Gamepads final : public QObject {
public:
    /// What the Session is told.
    struct Sink {
        std::function<void(const MigoGamepadInfo &info, bool connected)> connection;
        std::function<void(const MigoGamepadStateEvent &sample)> sample;
    };

    /// What a device says it has, as evdev answers.
    struct Capabilities {
        QByteArray name;
        /// Its EV_KEY codes.
        std::vector<int> keys;
        /// Its EV_ABS codes, each with its range.
        std::map<int, std::pair<int, int>> axes;
    };

    /// What drives a device's motors -- the strong and weak magnitudes 0..1,
    /// for so long -- or empty when it has none.
    using Rumble = std::function<void(float strong, float weak, std::uint32_t duration_ms)>;

    explicit Gamepads(Sink sink, QObject *parent = nullptr);
    ~Gamepads() override;

    Gamepads(const Gamepads &) = delete;
    Gamepads &operator=(const Gamepads &) = delete;

    /// Finds the joysticks udev knows, and watches for those that come and go.
    void watchSystem();

    /// A device at `node`, its events read from `fd` -- which it now owns -- until
    /// it goes. Takes the lowest free index.
    void add(const QString &node, int fd, const Capabilities &capabilities, Rumble rumble);
    void remove(const QString &node);

    /// Every pad announced again: the Session had nothing to announce to before.
    void announce();
    void rumble(const MigoGamepadRumble &request);

private:
    struct Pad;

    void openDevice(const QString &node);
    void readEvents(Pad &pad);
    void deliver(Pad &pad, double timestamp_ms);
    void received();
    [[nodiscard]] MigoGamepadInfo infoOf(const Pad &pad) const;

    const Sink sink_;
    std::map<QString, std::unique_ptr<Pad>> pads_;
    udev *udev_ = nullptr;
    udev_monitor *monitor_ = nullptr;
    QSocketNotifier *monitor_notifier_ = nullptr;
};

}  // namespace migo::linux_host::qt6

#endif  // MIGO_LINUX_QT6_GAMEPADS_HPP_

#ifndef MIGO_LINUX_QT6_BACKLIGHT_HPP_
#define MIGO_LINUX_QT6_BACKLIGHT_HPP_

// Internal to the Host Kit, not installed: the screen's brightness.

#include <QDBusConnection>
#include <QObject>
#include <QString>

#include <cstdint>
#include <functional>
#include <optional>

namespace migo::linux_host::qt6 {

/// The screen's backlight as a Linux desktop controls it: read where the kernel
/// shows each backlight (`/sys/class/backlight`), and set through logind, which
/// lets the user of the seat's active session set its backlight without
/// privileges. A desktop whose screens have no backlight -- an external
/// monitor, a virtual machine -- has none to read or set.
class Backlight final : public QObject {
public:
    /// `sysfs` is where the backlights are, `bus` where logind answers: the
    /// system's, `/sys/class/backlight` and the system bus.
    Backlight(QString sysfs, QDBusConnection bus, QObject *parent = nullptr);

    /// The backlight's brightness, 0 to 1; none without a backlight.
    [[nodiscard]] std::optional<double> brightness() const;

    /// Set the backlight to `value`, 0 to 1 -- 0 its darkest, still lit: content
    /// asking for the darkest screen does not ask for none -- and then `done`
    /// with why it was not, empty when it was.
    void setBrightness(double value, std::function<void(const QString &error)> done);

private:
    struct Device {
        QString name;
        std::uint32_t max = 0;
        std::uint32_t brightness = 0;
    };
    /// The backlight the desktop's own tools pick: the firmware's over the
    /// platform's over a raw one, as systemd and GNOME pick; by name among equals.
    [[nodiscard]] std::optional<Device> device() const;

    const QString sysfs_;
    QDBusConnection bus_;
};

}  // namespace migo::linux_host::qt6

#endif  // MIGO_LINUX_QT6_BACKLIGHT_HPP_

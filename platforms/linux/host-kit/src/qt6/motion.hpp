#ifndef MIGO_LINUX_QT6_MOTION_HPP_
#define MIGO_LINUX_QT6_MOTION_HPP_

// Internal to the Host Kit, not installed: the device's motion sensors.

#include <migo/migo.h>

#include <QHash>
#include <QString>

#include <array>
#include <chrono>
#include <condition_variable>
#include <functional>
#include <memory>
#include <mutex>
#include <optional>
#include <thread>
#include <vector>

namespace migo::linux_host::qt6 {

/// The motion sensors the kernel shows through IIO -- an accelerometer, a
/// gyroscope, a magnetometer -- read where the kernel shows them, in the
/// device's frame: x right, y up the screen, z out of it. Each sensor's mount
/// matrix turns its axes into that frame: udev's (`ACCEL_MOUNT_MATRIX`, the
/// hardware database's correction for the laptop it is in) where it has one,
/// the kernel's (`mount_matrix`) otherwise. With an accelerometer in the base
/// and one in the display (a convertible), the display's is the device's.
///
/// The compass and the device's orientation are worked out from gravity and
/// the magnetic field as Android's SensorManager does -- the rotation matrix of
/// `getRotationMatrix`, the heading of `getOrientation`, the W3C angles the
/// Android SDK reports -- so both platforms answer the same position alike. A
/// sensor the device lacks is refused with what it lacks: nothing is made up.
///
/// Sensors are read on a thread of their own, at the interval each was started
/// with, and every reading goes to `report` from that thread.
class MotionSensors final {
public:
    /// One IIO device as udev knows it: its sysfs directory and its properties.
    struct Device {
        QString path;
        QHash<QString, QString> properties;
    };

    /// The IIO devices udev knows.
    [[nodiscard]] static std::vector<Device> enumerate();

    explicit MotionSensors(const std::vector<Device> &devices, std::function<void(const MigoSensorSample &)> report);
    ~MotionSensors();

    MotionSensors(const MotionSensors &) = delete;
    MotionSensors &operator=(const MotionSensors &) = delete;

    /// Why `kind` cannot be read here -- the sensor it needs the device lacks --
    /// or empty when it can.
    [[nodiscard]] QString missing(MigoSensorKind kind) const;

    /// Reads `kind` every `interval` from now on, or at the new interval when it
    /// runs already. One `missing` names is not started.
    void start(MigoSensorKind kind, std::chrono::milliseconds interval);
    void stop(MigoSensorKind kind);

    /// A 3x3 matrix, row by row.
    using Matrix = std::array<double, 9>;

    /// The rotation from the device's frame to the world's -- east, north, up --
    /// for `gravity` (as an accelerometer reads it) and `field` (as a
    /// magnetometer does): none in free fall or beside a magnet.
    [[nodiscard]] static std::optional<Matrix> rotation(const std::array<double, 3> &gravity,
                                                        const std::array<double, 3> &field);
    /// The heading of the top of the screen from magnetic north, in degrees, 0 to 360.
    [[nodiscard]] static double heading(const Matrix &rotation);
    /// The W3C DeviceOrientation angles -- alpha 0 to 360, beta -180 to 180,
    /// gamma -90 to 90, in degrees -- as the Android SDK decomposes them.
    [[nodiscard]] static std::array<double, 3> orientation(const Matrix &rotation);

private:
    class Sensor;

    void run();
    /// One reading of `kind`, when its sensors answer.
    std::optional<MigoSensorSample> read(MigoSensorKind kind) const;

    std::unique_ptr<Sensor> accelerometer_;
    std::unique_ptr<Sensor> gyroscope_;
    std::unique_ptr<Sensor> magnetometer_;
    const std::function<void(const MigoSensorSample &)> report_;

    struct Stream {
        bool running = false;
        std::chrono::milliseconds interval{0};
        std::chrono::steady_clock::time_point due;
    };
    std::mutex mutex_;
    std::condition_variable changed_;
    std::array<Stream, 4> streams_;
    bool stopping_ = false;
    std::thread worker_;
};

}  // namespace migo::linux_host::qt6

#endif  // MIGO_LINUX_QT6_MOTION_HPP_

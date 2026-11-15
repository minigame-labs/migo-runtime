#include "motion.hpp"

#include <QByteArray>
#include <QFile>
#include <QStringList>

#include <libudev.h>

#include <fcntl.h>
#include <unistd.h>

#include <algorithm>
#include <cmath>
#include <cstdlib>

namespace migo::linux_host::qt6 {

namespace {

/// Standard gravity: what an accelerometer reads at rest, in m/s^2.
constexpr double kStandardGravity = 9.80665;
/// Gauss in microtesla: a magnetometer's unit in IIO, and the one Android's
/// free-fall threshold is in.
constexpr double kMicroteslaPerGauss = 100.0;
constexpr double kPi = 3.14159265358979323846;
constexpr double kDegreesPerRadian = 180.0 / kPi;
/// The compass's interval: its start names none.
constexpr std::chrono::milliseconds kCompassInterval{200};

constexpr MotionSensors::Matrix kIdentity{1, 0, 0, 0, 1, 0, 0, 0, 1};

/// A mount matrix as udev and the kernel write one, "x1, y1, z1; x2, y2, z2;
/// x3, y3, z3"; none when it is not nine numbers.
std::optional<MotionSensors::Matrix> parseMatrix(const QString &text) {
    const QStringList rows = text.split(QLatin1Char(';'));
    if (rows.size() != 3) return std::nullopt;
    MotionSensors::Matrix matrix{};
    std::size_t at = 0;
    for (const QString &row : rows) {
        const QStringList cells = row.split(QLatin1Char(','));
        if (cells.size() != 3) return std::nullopt;
        for (const QString &cell : cells) {
            bool number = false;
            matrix[at++] = cell.trimmed().toDouble(&number);
            if (!number) return std::nullopt;
        }
    }
    return matrix;
}

/// The first of `names` the device shows, read whole and trimmed; empty when none is.
QString attribute(const QString &device, std::initializer_list<QString> names) {
    for (const QString &name : names) {
        QFile file(device + QLatin1Char('/') + name);
        if (file.open(QIODevice::ReadOnly)) return QString::fromLatin1(file.readAll()).trimmed();
    }
    return {};
}

}  // namespace

// ---- one sensor --------------------------------------------------------------------------------

/// One IIO sensor's three channels, held open, and what turns a reading into
/// the device's frame.
class MotionSensors::Sensor final {
public:
    /// The `channel` sensor ("accel", "anglvel", "magn") of `device`, or none
    /// when it shows no such channels or they cannot be opened. `udev_matrix`
    /// names udev's correction for this kind of sensor, where there is one.
    static std::unique_ptr<Sensor> open(const Device &device, const QString &channel, const QString &udev_matrix) {
        auto sensor = std::unique_ptr<Sensor>(new Sensor);
        const QString prefix = QStringLiteral("in_") + channel;
        const QString axes[3] = {QStringLiteral("x"), QStringLiteral("y"), QStringLiteral("z")};
        const QString shared_scale = attribute(device.path, {prefix + QStringLiteral("_scale")});
        const QString shared_offset = attribute(device.path, {prefix + QStringLiteral("_offset")});
        for (int axis = 0; axis < 3; ++axis) {
            const QString name = prefix + QLatin1Char('_') + axes[axis];
            const QByteArray raw = QFile::encodeName(device.path + QLatin1Char('/') + name + QStringLiteral("_raw"));
            sensor->files_[axis] = ::open(raw.constData(), O_RDONLY | O_CLOEXEC);
            if (sensor->files_[axis] < 0) return nullptr;
            const QString scale = attribute(device.path, {name + QStringLiteral("_scale")});
            const QString offset = attribute(device.path, {name + QStringLiteral("_offset")});
            sensor->scale_[axis] = (scale.isEmpty() ? shared_scale : scale).toDouble();
            if (sensor->scale_[axis] == 0) sensor->scale_[axis] = 1;
            sensor->offset_[axis] = (offset.isEmpty() ? shared_offset : offset).toDouble();
        }
        std::optional<Matrix> mount;
        if (!udev_matrix.isEmpty()) mount = parseMatrix(device.properties.value(udev_matrix));
        if (!mount.has_value()) {
            mount = parseMatrix(attribute(device.path, {prefix + QStringLiteral("_mount_matrix"), QStringLiteral("mount_matrix")}));
        }
        sensor->mount_ = mount.value_or(kIdentity);
        return sensor;
    }

    ~Sensor() {
        for (const int file : files_) {
            if (file >= 0) ::close(file);
        }
    }

    /// One reading in the device's frame, in the channel's unit (m/s^2, rad/s,
    /// Gauss); none when the kernel does not answer.
    [[nodiscard]] std::optional<std::array<double, 3>> read() const {
        std::array<double, 3> sensor{};
        for (int axis = 0; axis < 3; ++axis) {
            char text[32];
            const ssize_t length = ::pread(files_[axis], text, sizeof(text) - 1, 0);
            if (length <= 0) return std::nullopt;
            text[length] = '\0';
            char *end = nullptr;
            const double raw = std::strtod(text, &end);
            if (end == text) return std::nullopt;
            sensor[axis] = (raw + offset_[axis]) * scale_[axis];
        }
        return std::array<double, 3>{
            mount_[0] * sensor[0] + mount_[1] * sensor[1] + mount_[2] * sensor[2],
            mount_[3] * sensor[0] + mount_[4] * sensor[1] + mount_[5] * sensor[2],
            mount_[6] * sensor[0] + mount_[7] * sensor[1] + mount_[8] * sensor[2],
        };
    }

private:
    Sensor() = default;

    std::array<int, 3> files_{-1, -1, -1};
    std::array<double, 3> scale_{1, 1, 1};
    std::array<double, 3> offset_{0, 0, 0};
    Matrix mount_ = kIdentity;
};

// ---- finding the sensors ------------------------------------------------------------------------

std::vector<MotionSensors::Device> MotionSensors::enumerate() {
    std::vector<Device> found;
    udev *context = udev_new();
    if (context == nullptr) return found;
    udev_enumerate *scan = udev_enumerate_new(context);
    if (scan != nullptr && udev_enumerate_add_match_subsystem(scan, "iio") >= 0 &&
        udev_enumerate_scan_devices(scan) >= 0) {
        udev_list_entry *entry = nullptr;
        udev_list_entry_foreach(entry, udev_enumerate_get_list_entry(scan)) {
            const char *path = udev_list_entry_get_name(entry);
            udev_device *device = udev_device_new_from_syspath(context, path);
            if (device == nullptr) continue;
            Device known{QFile::decodeName(path), {}};
            udev_list_entry *property = nullptr;
            udev_list_entry_foreach(property, udev_device_get_properties_list_entry(device)) {
                known.properties.insert(QString::fromUtf8(udev_list_entry_get_name(property)),
                                        QString::fromUtf8(udev_list_entry_get_value(property)));
            }
            udev_device_unref(device);
            found.push_back(std::move(known));
        }
    }
    if (scan != nullptr) udev_enumerate_unref(scan);
    udev_unref(context);
    return found;
}

MotionSensors::MotionSensors(const std::vector<Device> &devices, std::function<void(const MigoSensorSample &)> report)
    : report_(std::move(report)) {
    // A convertible's base has an accelerometer of its own: the display's is
    // the one that turns with the screen.
    std::vector<const Device *> accelerometers;
    for (const Device &device : devices) {
        if (QFile::exists(device.path + QStringLiteral("/in_accel_x_raw"))) accelerometers.push_back(&device);
    }
    std::stable_partition(accelerometers.begin(), accelerometers.end(), [](const Device *device) {
        return device->properties.value(QStringLiteral("ACCEL_LOCATION")) != QLatin1String("base");
    });
    for (const Device *device : accelerometers) {
        if ((accelerometer_ = Sensor::open(*device, QStringLiteral("accel"), QStringLiteral("ACCEL_MOUNT_MATRIX")))) break;
    }
    for (const Device &device : devices) {
        if (gyroscope_ == nullptr) gyroscope_ = Sensor::open(device, QStringLiteral("anglvel"), QString());
        if (magnetometer_ == nullptr) magnetometer_ = Sensor::open(device, QStringLiteral("magn"), QString());
    }
}

MotionSensors::~MotionSensors() {
    {
        const std::lock_guard<std::mutex> lock(mutex_);
        stopping_ = true;
    }
    changed_.notify_all();
    if (worker_.joinable()) worker_.join();
}

QString MotionSensors::missing(MigoSensorKind kind) const {
    switch (kind) {
        case MIGO_SENSOR_ACCELEROMETER:
            return accelerometer_ == nullptr ? QStringLiteral("the device has no accelerometer") : QString();
        case MIGO_SENSOR_GYROSCOPE:
            return gyroscope_ == nullptr ? QStringLiteral("the device has no gyroscope") : QString();
        case MIGO_SENSOR_COMPASS:
        case MIGO_SENSOR_DEVICE_MOTION:
            // Which way is north needs the field; which way is down, gravity.
            if (magnetometer_ == nullptr) return QStringLiteral("the device has no magnetometer");
            return accelerometer_ == nullptr ? QStringLiteral("the device has no accelerometer") : QString();
        default:
            return QStringLiteral("not supported");
    }
}

void MotionSensors::start(MigoSensorKind kind, std::chrono::milliseconds interval) {
    if (kind >= streams_.size() || !missing(kind).isEmpty()) return;
    {
        const std::lock_guard<std::mutex> lock(mutex_);
        Stream &stream = streams_[kind];
        stream.running = true;
        stream.interval = kind == MIGO_SENSOR_COMPASS ? kCompassInterval : std::max(interval, std::chrono::milliseconds{1});
        // The first reading at once.
        stream.due = std::chrono::steady_clock::now();
        if (!worker_.joinable()) worker_ = std::thread(&MotionSensors::run, this);
    }
    changed_.notify_all();
}

void MotionSensors::stop(MigoSensorKind kind) {
    if (kind >= streams_.size()) return;
    {
        const std::lock_guard<std::mutex> lock(mutex_);
        streams_[kind].running = false;
    }
    changed_.notify_all();
}

void MotionSensors::run() {
    std::unique_lock<std::mutex> lock(mutex_);
    while (!stopping_) {
        auto next = std::chrono::steady_clock::time_point::max();
        for (const Stream &stream : streams_) {
            if (stream.running) next = std::min(next, stream.due);
        }
        if (next == std::chrono::steady_clock::time_point::max()) {
            changed_.wait(lock);
            continue;
        }
        const auto now = std::chrono::steady_clock::now();
        if (now < next) {
            changed_.wait_until(lock, next);
            continue;
        }
        std::array<bool, 4> due{};
        for (std::size_t kind = 0; kind < streams_.size(); ++kind) {
            Stream &stream = streams_[kind];
            if (!stream.running || stream.due > now) continue;
            due[kind] = true;
            // On schedule, or -- after a stall -- from now: never a burst to catch up.
            stream.due += stream.interval;
            if (stream.due <= now) stream.due = now + stream.interval;
        }
        // Read outside the lock: the kernel may take its time, and a start or
        // stop meanwhile must not wait for it.
        lock.unlock();
        for (std::size_t kind = 0; kind < due.size(); ++kind) {
            if (!due[kind]) continue;
            if (const std::optional<MigoSensorSample> sample = read(static_cast<MigoSensorKind>(kind))) report_(*sample);
        }
        lock.lock();
    }
}

std::optional<MigoSensorSample> MotionSensors::read(MigoSensorKind kind) const {
    MigoSensorSample sample{};
    sample.struct_size = static_cast<std::uint32_t>(sizeof(sample));
    sample.abi_version = MIGO_ABI_VERSION_1;
    sample.kind = kind;
    switch (kind) {
        case MIGO_SENSOR_ACCELEROMETER: {
            const auto gravity = accelerometer_->read();
            if (!gravity.has_value()) return std::nullopt;
            for (int axis = 0; axis < 3; ++axis) sample.values[axis] = (*gravity)[axis] / kStandardGravity;
            return sample;
        }
        case MIGO_SENSOR_GYROSCOPE: {
            const auto turning = gyroscope_->read();
            if (!turning.has_value()) return std::nullopt;
            for (int axis = 0; axis < 3; ++axis) sample.values[axis] = (*turning)[axis];
            return sample;
        }
        case MIGO_SENSOR_COMPASS:
        case MIGO_SENSOR_DEVICE_MOTION: {
            const auto gravity = accelerometer_->read();
            const auto field = magnetometer_->read();
            if (!gravity.has_value() || !field.has_value()) return std::nullopt;
            const std::array<double, 3> microtesla{(*field)[0] * kMicroteslaPerGauss, (*field)[1] * kMicroteslaPerGauss,
                                                   (*field)[2] * kMicroteslaPerGauss};
            const std::optional<Matrix> turned = rotation(*gravity, microtesla);
            if (!turned.has_value()) return std::nullopt;
            if (kind == MIGO_SENSOR_COMPASS) {
                // IIO says nothing of a magnetometer's calibration.
                sample.compass_accuracy = MIGO_COMPASS_ACCURACY_UNKNOWN;
                sample.values[0] = heading(*turned);
            } else {
                const std::array<double, 3> angles = orientation(*turned);
                for (int axis = 0; axis < 3; ++axis) sample.values[axis] = angles[axis];
            }
            return sample;
        }
        default:
            return std::nullopt;
    }
}

// ---- the arithmetic -----------------------------------------------------------------------------

std::optional<MotionSensors::Matrix> MotionSensors::rotation(const std::array<double, 3> &gravity,
                                                             const std::array<double, 3> &field) {
    // Android's SensorManager.getRotationMatrix: east is the field across
    // gravity, north is gravity across east, up is gravity.
    const double ax = gravity[0], ay = gravity[1], az = gravity[2];
    const double ex = field[0], ey = field[1], ez = field[2];
    double hx = ey * az - ez * ay;
    double hy = ez * ax - ex * az;
    double hz = ex * ay - ey * ax;
    const double east = std::sqrt(hx * hx + hy * hy + hz * hz);
    // In free fall, or with the field along gravity -- near a pole, beside a
    // magnet -- there is no east. The threshold is Android's, in its units:
    // m/s^2 times microtesla; a device at rest reads in the hundreds.
    if (east < 0.1) return std::nullopt;
    hx /= east;
    hy /= east;
    hz /= east;
    const double up = std::sqrt(ax * ax + ay * ay + az * az);
    const double ux = ax / up, uy = ay / up, uz = az / up;
    const double nx = uy * hz - uz * hy;
    const double ny = uz * hx - ux * hz;
    const double nz = ux * hy - uy * hx;
    return Matrix{hx, hy, hz, nx, ny, nz, ux, uy, uz};
}

double MotionSensors::heading(const Matrix &r) {
    // Android's SensorManager.getOrientation: the azimuth.
    double degrees = std::atan2(r[1], r[4]) * kDegreesPerRadian;
    if (degrees < 0) degrees += 360;
    return std::fmod(degrees, 360.0);
}

std::array<double, 3> MotionSensors::orientation(const Matrix &r) {
    // R = Rz(alpha) Rx(beta) Ry(gamma), decomposed as the specification does
    // and as the Android SDK reports it (DeviceSensorManager.deviceOrientationAngles).
    double alpha = 0;
    double beta = 0;
    double gamma = 0;
    if (r[8] > 0) {
        alpha = std::atan2(-r[1], r[4]);
        beta = std::asin(r[7]);
        gamma = std::atan2(-r[6], r[8]);
    } else if (r[8] < 0) {
        alpha = std::atan2(r[1], -r[4]);
        beta = -std::asin(r[7]);
        beta += beta >= 0 ? -kPi : kPi;
        gamma = std::atan2(r[6], -r[8]);
    } else if (r[6] > 0) {
        alpha = std::atan2(-r[1], r[4]);
        beta = std::asin(r[7]);
        gamma = -kPi / 2;
    } else if (r[6] < 0) {
        alpha = std::atan2(r[1], -r[4]);
        beta = -std::asin(r[7]);
        beta += beta >= 0 ? -kPi : kPi;
        gamma = -kPi / 2;
    } else {
        // Gimbal lock: alpha and gamma turn about one axis; the whole turn is alpha.
        alpha = std::atan2(r[3], r[0]);
        beta = r[7] > 0 ? kPi / 2 : -kPi / 2;
        gamma = 0;
    }
    if (alpha < 0) alpha += 2 * kPi;
    return {std::fmod(alpha * kDegreesPerRadian, 360.0), beta * kDegreesPerRadian, gamma * kDegreesPerRadian};
}

}  // namespace migo::linux_host::qt6

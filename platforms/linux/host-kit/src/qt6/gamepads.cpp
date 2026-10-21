#include "gamepads.hpp"

#include <QFile>
#include <QSocketNotifier>

#include <libudev.h>
#include <linux/input.h>

#include <fcntl.h>
#include <sys/ioctl.h>
#include <unistd.h>

#include <algorithm>
#include <array>
#include <cerrno>
#include <cmath>
#include <ctime>
#include <optional>

namespace migo::linux_host::qt6 {

namespace {

/// What a W3C standard-layout button is pressed by, digitally.
constexpr std::array<int, 17> kStandardButtons{
    BTN_SOUTH, BTN_EAST,   BTN_WEST,   BTN_NORTH,   BTN_TL,         BTN_TR,         BTN_TL2,         BTN_TR2, BTN_SELECT,
    BTN_START, BTN_THUMBL, BTN_THUMBR, BTN_DPAD_UP, BTN_DPAD_DOWN, BTN_DPAD_LEFT, BTN_DPAD_RIGHT, BTN_MODE,
};
constexpr int kLeftTrigger = 6;
constexpr int kRightTrigger = 7;
constexpr int kDpadUp = 12;
constexpr int kDpadDown = 13;
constexpr int kDpadLeft = 14;
constexpr int kDpadRight = 15;
/// An analog trigger counts as pressed past this: Chromium's threshold.
constexpr float kTriggerPressed = 30.0F / 255.0F;

/// Where an axis's value goes.
struct AxisTarget {
    enum class Kind { Axis, Trigger, HatX, HatY } kind;
    int index;
};

bool hasCode(const std::vector<int> &codes, int code) { return std::find(codes.begin(), codes.end(), code) != codes.end(); }

/// Plays (1) or stops (0) the effect `id`: false when the device is gone, whose
/// removal follows from its events.
bool play(int fd, int id, int value) {
    const input_event event{{}, EV_FF, static_cast<__u16>(id), value};
    return ::write(fd, &event, sizeof(event)) == static_cast<ssize_t>(sizeof(event));
}

/// The value of `raw` in `range`, from -1 to 1 -- or 0 to 1 for a trigger.
float scaled(int raw, std::pair<int, int> range, bool trigger) {
    const double span = static_cast<double>(range.second) - range.first;
    if (span <= 0) return 0;
    const double unit = (static_cast<double>(raw) - range.first) / span;
    return static_cast<float>(trigger ? std::clamp(unit, 0.0, 1.0) : std::clamp(unit * 2 - 1, -1.0, 1.0));
}

}  // namespace

/// One pad: its device, its index, the layout it maps onto, its state.
struct Gamepads::Pad {
    QString node;
    int fd = -1;
    std::uint32_t index = 0;
    QByteArray name;
    bool standard = false;
    Rumble rumble;
    std::vector<float> axes;
    std::vector<MigoGamepadButton> buttons;
    QHash<int, int> key_buttons;
    QHash<int, AxisTarget> axis_targets;
    std::map<int, std::pair<int, int>> ranges;
    QSocketNotifier *notifier = nullptr;
    /// Events since the kernel dropped some are not a state until it reports again.
    bool dropped = false;

    ~Pad() {
        // A pad may go from its own notifier's signal: the notifier goes after it.
        if (notifier != nullptr) {
            notifier->setEnabled(false);
            notifier->deleteLater();
        }
        if (fd >= 0) ::close(fd);
    }
};

Gamepads::Gamepads(Sink sink, QObject *parent) : QObject(parent), sink_(std::move(sink)) {}

Gamepads::~Gamepads() {
    // Gone with the Session: nobody is told.
    pads_.clear();
    delete monitor_notifier_;
    if (monitor_ != nullptr) udev_monitor_unref(monitor_);
    if (udev_ != nullptr) udev_unref(udev_);
}

void Gamepads::watchSystem() {
    udev_ = udev_new();
    if (udev_ == nullptr) return;
    // Watched before listing, so a pad plugged in between is not missed; one
    // listed and announced too is added once.
    monitor_ = udev_monitor_new_from_netlink(udev_, "udev");
    if (monitor_ != nullptr && udev_monitor_filter_add_match_subsystem_devtype(monitor_, "input", nullptr) >= 0 &&
        udev_monitor_enable_receiving(monitor_) >= 0) {
        monitor_notifier_ = new QSocketNotifier(udev_monitor_get_fd(monitor_), QSocketNotifier::Read, this);
        connect(monitor_notifier_, &QSocketNotifier::activated, this, [this] { received(); });
    }
    udev_enumerate *scan = udev_enumerate_new(udev_);
    if (scan == nullptr) return;
    udev_enumerate_add_match_subsystem(scan, "input");
    udev_enumerate_add_match_property(scan, "ID_INPUT_JOYSTICK", "1");
    udev_enumerate_scan_devices(scan);
    udev_list_entry *entry = nullptr;
    udev_list_entry_foreach(entry, udev_enumerate_get_list_entry(scan)) {
        udev_device *device = udev_device_new_from_syspath(udev_, udev_list_entry_get_name(entry));
        if (device == nullptr) continue;
        if (const char *node = udev_device_get_devnode(device); node != nullptr) {
            const QString path = QFile::decodeName(node);
            // The event device: the legacy js node reports the same pad.
            if (path.startsWith(QLatin1String("/dev/input/event"))) openDevice(path);
        }
        udev_device_unref(device);
    }
    udev_enumerate_unref(scan);
}

void Gamepads::received() {
    udev_device *device = udev_monitor_receive_device(monitor_);
    if (device == nullptr) return;
    const char *node = udev_device_get_devnode(device);
    const char *action = udev_device_get_action(device);
    const char *joystick = udev_device_get_property_value(device, "ID_INPUT_JOYSTICK");
    if (node != nullptr && action != nullptr) {
        const QString path = QFile::decodeName(node);
        if (qstrcmp(action, "remove") == 0) {
            remove(path);
        } else if (qstrcmp(action, "add") == 0 && joystick != nullptr && qstrcmp(joystick, "1") == 0 &&
                   path.startsWith(QLatin1String("/dev/input/event"))) {
            openDevice(path);
        }
    }
    udev_device_unref(device);
}

void Gamepads::openDevice(const QString &node) {
    if (pads_.count(node) != 0) return;
    const QByteArray path = QFile::encodeName(node);
    // Written to for the motors; read-only when it may not be.
    int fd = ::open(path.constData(), O_RDWR | O_NONBLOCK | O_CLOEXEC);
    const bool writable = fd >= 0;
    if (!writable) fd = ::open(path.constData(), O_RDONLY | O_NONBLOCK | O_CLOEXEC);
    if (fd < 0) return;
    Capabilities capabilities;
    char name[256] = {};
    if (::ioctl(fd, EVIOCGNAME(sizeof(name) - 1), name) >= 0) capabilities.name = QByteArray(name);
    const auto bits = [fd](int type, auto &into) {
        return ::ioctl(fd, EVIOCGBIT(type, sizeof(into)), into.data()) >= 0;
    };
    const auto set = [](const auto &array, int bit) {
        constexpr int kBits = sizeof(array[0]) * 8;
        return (array[static_cast<std::size_t>(bit / kBits)] >> (bit % kBits)) & 1;
    };
    std::array<unsigned long, KEY_MAX / (sizeof(unsigned long) * 8) + 1> keys{};
    if (bits(EV_KEY, keys)) {
        for (int code = BTN_MISC; code <= KEY_MAX; ++code) {
            if (set(keys, code)) capabilities.keys.push_back(code);
        }
    }
    std::array<unsigned long, ABS_MAX / (sizeof(unsigned long) * 8) + 1> absolute{};
    if (bits(EV_ABS, absolute)) {
        for (int code = 0; code < ABS_MISC; ++code) {
            input_absinfo info{};
            if (set(absolute, code) && ::ioctl(fd, EVIOCGABS(code), &info) >= 0) {
                capabilities.axes.emplace(code, std::make_pair(info.minimum, info.maximum));
            }
        }
    }
    std::array<unsigned long, FF_MAX / (sizeof(unsigned long) * 8) + 1> effects{};
    Rumble rumble;
    if (writable && bits(EV_FF, effects) && set(effects, FF_RUMBLE)) {
        // One effect per pad, uploaded again for each change: the kernel
        // replaces an effect that keeps its id.
        auto effect = std::make_shared<int>(-1);
        rumble = [fd, effect](float strong, float weak, std::uint32_t duration_ms) {
            if (strong <= 0 && weak <= 0) {
                if (*effect >= 0 && !play(fd, *effect, 0)) *effect = -1;
                return;
            }
            ff_effect upload{};
            upload.type = FF_RUMBLE;
            upload.id = static_cast<__s16>(*effect);
            upload.u.rumble.strong_magnitude = static_cast<__u16>(std::lround(std::clamp(strong, 0.0F, 1.0F) * 0xffff));
            upload.u.rumble.weak_magnitude = static_cast<__u16>(std::lround(std::clamp(weak, 0.0F, 1.0F) * 0xffff));
            upload.replay.length = static_cast<__u16>(std::min<std::uint32_t>(duration_ms, 0xffff));
            if (::ioctl(fd, EVIOCSFF, &upload) < 0) return;
            *effect = upload.id;
            if (!play(fd, upload.id, 1)) *effect = -1;
        };
    }
    // The X server's clock, which the view's input is stamped with.
    int clock = CLOCK_MONOTONIC;
    (void)::ioctl(fd, EVIOCSCLOCKID, &clock);
    if (capabilities.keys.empty() && capabilities.axes.empty()) {
        ::close(fd);
        return;
    }
    add(node, fd, capabilities, std::move(rumble));
}

void Gamepads::add(const QString &node, int fd, const Capabilities &capabilities, Rumble rumble) {
    if (pads_.count(node) != 0) {
        ::close(fd);
        return;
    }
    auto pad = std::make_unique<Pad>();
    pad->node = node;
    pad->fd = fd;
    pad->name = capabilities.name;
    pad->rumble = std::move(rumble);
    pad->ranges = capabilities.axes;
    // The lowest index no pad holds: content keeps an index for as long as its pad stays.
    std::uint32_t index = 0;
    for (bool taken = true; taken; ++index) {
        taken = std::any_of(pads_.begin(), pads_.end(), [index](const auto &held) { return held.second->index == index; });
        if (!taken) break;
    }
    pad->index = index;

    const auto has_axis = [&capabilities](int code) { return capabilities.axes.count(code) != 0; };
    pad->standard = hasCode(capabilities.keys, BTN_SOUTH) && has_axis(ABS_X) && has_axis(ABS_Y);
    if (pad->standard) {
        pad->axes.assign(4, 0.0F);
        pad->buttons.assign(kStandardButtons.size(), MigoGamepadButton{});
        for (std::size_t button = 0; button < kStandardButtons.size(); ++button) {
            if (hasCode(capabilities.keys, kStandardButtons[button])) {
                pad->key_buttons.insert(kStandardButtons[button], static_cast<int>(button));
            }
        }
        pad->axis_targets.insert(ABS_X, {AxisTarget::Kind::Axis, 0});
        pad->axis_targets.insert(ABS_Y, {AxisTarget::Kind::Axis, 1});
        // The right stick on RX/RY, the triggers on Z/RZ, as the gamepad layout
        // has them; an older layout put the right stick on Z/RZ.
        const bool right_on_r = has_axis(ABS_RX) && has_axis(ABS_RY);
        pad->axis_targets.insert(right_on_r ? ABS_RX : ABS_Z, {AxisTarget::Kind::Axis, 2});
        pad->axis_targets.insert(right_on_r ? ABS_RY : ABS_RZ, {AxisTarget::Kind::Axis, 3});
        // The analog triggers where the driver reports them: Z and RZ when the
        // right stick is not there, else the brake and gas, else the second hat.
        const std::array<int, 3> left{right_on_r ? ABS_Z : -1, ABS_BRAKE, ABS_HAT2Y};
        const std::array<int, 3> right{right_on_r ? ABS_RZ : -1, ABS_GAS, ABS_HAT2X};
        for (const auto &[candidates, button] : {std::make_pair(left, kLeftTrigger), std::make_pair(right, kRightTrigger)}) {
            for (const int code : candidates) {
                if (code < 0 || !has_axis(code)) continue;
                pad->axis_targets.insert(code, {AxisTarget::Kind::Trigger, button});
                break;
            }
        }
        if (has_axis(ABS_HAT0X)) pad->axis_targets.insert(ABS_HAT0X, {AxisTarget::Kind::HatX, 0});
        if (has_axis(ABS_HAT0Y)) pad->axis_targets.insert(ABS_HAT0Y, {AxisTarget::Kind::HatY, 0});
    } else {
        // As it is: its buttons and axes in code order, as many as the Session takes.
        for (const int code : capabilities.keys) {
            if (code < BTN_JOYSTICK || pad->key_buttons.size() >= static_cast<qsizetype>(MIGO_GAMEPAD_MAX_BUTTONS)) continue;
            pad->key_buttons.insert(code, static_cast<int>(pad->key_buttons.size()));
        }
        int axis = 0;
        for (const auto &[code, range] : capabilities.axes) {
            if (axis >= MIGO_GAMEPAD_MAX_AXES) break;
            pad->axis_targets.insert(code, {AxisTarget::Kind::Axis, axis++});
        }
        pad->axes.assign(static_cast<std::size_t>(axis), 0.0F);
        pad->buttons.assign(static_cast<std::size_t>(pad->key_buttons.size()), MigoGamepadButton{});
    }
    // At rest until the device says otherwise: centred axes where the range has a centre.
    for (auto target = pad->axis_targets.cbegin(); target != pad->axis_targets.cend(); ++target) {
        if (target.value().kind == AxisTarget::Kind::Axis) {
            const auto range = pad->ranges.at(target.key());
            pad->axes[static_cast<std::size_t>(target.value().index)] = scaled((range.first + range.second) / 2, range, false);
        }
    }

    Pad &held = *pad;
    held.notifier = new QSocketNotifier(fd, QSocketNotifier::Read);
    connect(held.notifier, &QSocketNotifier::activated, this, [this, node] {
        const auto found = pads_.find(node);
        if (found != pads_.end()) readEvents(*found->second);
    });
    pads_.emplace(node, std::move(pad));
    sink_.connection(infoOf(held), true);
}

void Gamepads::remove(const QString &node) {
    const auto found = pads_.find(node);
    if (found == pads_.end()) return;
    const MigoGamepadInfo info = infoOf(*found->second);
    sink_.connection(info, false);
    pads_.erase(found);
}

void Gamepads::announce() {
    for (const auto &held : pads_) sink_.connection(infoOf(*held.second), true);
}

MigoGamepadInfo Gamepads::infoOf(const Pad &pad) const {
    MigoGamepadInfo info{};
    info.struct_size = static_cast<std::uint32_t>(sizeof(info));
    info.abi_version = MIGO_ABI_VERSION_1;
    info.index = pad.index;
    info.axis_count = static_cast<std::uint32_t>(pad.axes.size());
    info.button_count = static_cast<std::uint32_t>(pad.buttons.size());
    info.haptics = pad.rumble ? MIGO_GAMEPAD_HAPTICS_DUAL_RUMBLE : MIGO_GAMEPAD_HAPTICS_NONE;
    info.id_utf8 = pad.name.constData();
    info.mapping_utf8 = pad.standard ? "standard" : "";
    return info;
}

void Gamepads::rumble(const MigoGamepadRumble &request) {
    for (const auto &held : pads_) {
        if (held.second->index != request.index || !held.second->rumble) continue;
        held.second->rumble(request.strong_magnitude, request.weak_magnitude, request.duration_ms);
        return;
    }
}

void Gamepads::readEvents(Pad &pad) {
    std::array<input_event, 64> events{};
    for (;;) {
        const ssize_t length = ::read(pad.fd, events.data(), sizeof(events));
        if (length < 0 && errno == EINTR) continue;
        if (length < 0 && errno == EAGAIN) return;
        if (length <= 0) {
            // Unplugged: udev says so too, but the device is gone now.
            remove(pad.node);
            return;
        }
        for (std::size_t at = 0; at < static_cast<std::size_t>(length) / sizeof(input_event); ++at) {
            const input_event &event = events[at];
            if (event.type == EV_SYN) {
                if (event.code == SYN_DROPPED) {
                    pad.dropped = true;
                } else if (event.code == SYN_REPORT) {
                    if (pad.dropped) {
                        // What was missed is not known: the next report is whole again.
                        pad.dropped = false;
                        continue;
                    }
                    deliver(pad, static_cast<double>(event.input_event_sec) * 1000.0 + event.input_event_usec / 1000.0);
                }
                continue;
            }
            if (pad.dropped) continue;
            if (event.type == EV_KEY) {
                const auto button = pad.key_buttons.constFind(event.code);
                if (button == pad.key_buttons.cend()) continue;
                MigoGamepadButton &state = pad.buttons[static_cast<std::size_t>(button.value())];
                const bool down = event.value != 0;
                state.value = down ? 1.0F : 0.0F;
                state.flags = down ? MIGO_GAMEPAD_BUTTON_FLAG_PRESSED | MIGO_GAMEPAD_BUTTON_FLAG_TOUCHED
                                   : MIGO_GAMEPAD_BUTTON_FLAG_NONE;
            } else if (event.type == EV_ABS) {
                const auto target = pad.axis_targets.constFind(event.code);
                if (target == pad.axis_targets.cend()) continue;
                const std::pair<int, int> range = pad.ranges.at(event.code);
                switch (target.value().kind) {
                    case AxisTarget::Kind::Axis:
                        pad.axes[static_cast<std::size_t>(target.value().index)] = scaled(event.value, range, false);
                        break;
                    case AxisTarget::Kind::Trigger: {
                        MigoGamepadButton &state = pad.buttons[static_cast<std::size_t>(target.value().index)];
                        state.value = scaled(event.value, range, true);
                        state.flags = state.value > kTriggerPressed
                                          ? MIGO_GAMEPAD_BUTTON_FLAG_PRESSED | MIGO_GAMEPAD_BUTTON_FLAG_TOUCHED
                                          : (state.value > 0 ? MIGO_GAMEPAD_BUTTON_FLAG_TOUCHED : MIGO_GAMEPAD_BUTTON_FLAG_NONE);
                        break;
                    }
                    case AxisTarget::Kind::HatX:
                    case AxisTarget::Kind::HatY: {
                        const bool horizontal = target.value().kind == AxisTarget::Kind::HatX;
                        const int negative = horizontal ? kDpadLeft : kDpadUp;
                        const int positive = horizontal ? kDpadRight : kDpadDown;
                        for (const auto &[button, on] : {std::make_pair(negative, event.value < 0),
                                                         std::make_pair(positive, event.value > 0)}) {
                            MigoGamepadButton &state = pad.buttons[static_cast<std::size_t>(button)];
                            state.value = on ? 1.0F : 0.0F;
                            state.flags = on ? MIGO_GAMEPAD_BUTTON_FLAG_PRESSED | MIGO_GAMEPAD_BUTTON_FLAG_TOUCHED
                                             : MIGO_GAMEPAD_BUTTON_FLAG_NONE;
                        }
                        break;
                    }
                }
            }
        }
    }
}

void Gamepads::deliver(Pad &pad, double timestamp_ms) {
    MigoGamepadStateEvent sample{};
    sample.struct_size = static_cast<std::uint32_t>(sizeof(sample));
    sample.abi_version = MIGO_ABI_VERSION_1;
    sample.index = pad.index;
    sample.axis_count = static_cast<std::uint32_t>(pad.axes.size());
    sample.button_count = static_cast<std::uint32_t>(pad.buttons.size());
    sample.axes = pad.axes.data();
    sample.buttons = pad.buttons.data();
    sample.timestamp_ms = timestamp_ms;
    sink_.sample(sample);
}

}  // namespace migo::linux_host::qt6

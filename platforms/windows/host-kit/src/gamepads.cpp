#include "gamepads.hpp"

#include "gamepad_mapping.hpp"
#include "gui_queue.hpp"
#include "win32_text.hpp"

#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#ifndef NOMINMAX
#define NOMINMAX
#endif
#ifndef UNICODE
#define UNICODE
#endif
#include <windows.h>

#include <winrt/Windows.Foundation.Collections.h>
#include <winrt/Windows.Foundation.h>
#include <winrt/Windows.Gaming.Input.h>

#include <algorithm>
#include <cstdio>
#include <exception>
#include <mutex>
#include <optional>
#include <string>
#include <vector>

namespace migo::windows_host {

namespace {

namespace input = winrt::Windows::Gaming::Input;
using winrt::Windows::Foundation::IInspectable;

constexpr wchar_t kTimerClass[] = L"MigoGamepads";
constexpr UINT_PTR kPollTimer = 1;
// Each pad's rumble stops on a timer of its own: this one's id plus its index.
constexpr UINT_PTR kRumbleTimer = 100;
constexpr UINT kPollMs = 16;

[[noreturn]] void fatal(const char *why) {
    std::fprintf(stderr, "migo windows host kit: %s\n", why);
    std::fflush(stderr);
    std::terminate();
}

std::string hex4(std::uint16_t value) {
    char text[8];
    std::snprintf(text, sizeof text, "%04x", value);
    return text;
}

}  // namespace

class Gamepads::Impl final {
public:
    explicit Impl(Sink sink) : sink_(std::move(sink)), post_(queue_.poster()) {
        static std::once_flag registered;
        std::call_once(registered, [] {
            WNDCLASSEXW timers{};
            timers.cbSize = sizeof timers;
            timers.lpfnWndProc = &Impl::procedure;
            timers.hInstance = GetModuleHandleW(nullptr);
            timers.lpszClassName = kTimerClass;
            if (RegisterClassExW(&timers) == 0) fatal("RegisterClassExW failed for the gamepads");
        });
        window_ = CreateWindowExW(0, kTimerClass, L"", 0, 0, 0, 0, 0, HWND_MESSAGE, nullptr, GetModuleHandleW(nullptr),
                                  this);
        if (window_ == nullptr) fatal("CreateWindowExW failed for the gamepads");
        try {
            // The system's news comes on its threads; the pads are this thread's.
            const GuiQueue::Post post = post_;
            gamepad_added_ = input::Gamepad::GamepadAdded([this, post](IInspectable const &, input::Gamepad const &pad) {
                post([this, pad] { addGamepad(pad); });
            });
            gamepad_removed_ = input::Gamepad::GamepadRemoved([this, post](IInspectable const &, input::Gamepad const &pad) {
                post([this, pad] { remove(pad, nullptr); });
            });
            raw_added_ = input::RawGameController::RawGameControllerAdded(
                [this, post](IInspectable const &, input::RawGameController const &controller) {
                    post([this, controller] { addRaw(controller); });
                });
            raw_removed_ = input::RawGameController::RawGameControllerRemoved(
                [this, post](IInspectable const &, input::RawGameController const &controller) {
                    post([this, controller] { remove(nullptr, controller); });
                });
            for (const input::Gamepad &pad : input::Gamepad::Gamepads()) addGamepad(pad);
            for (const input::RawGameController &controller : input::RawGameController::RawGameControllers()) {
                addRaw(controller);
            }
            watching_ = true;
        } catch (const winrt::hresult_error &) {
            // No game controller support on this system: no pads.
        }
    }

    ~Impl() {
        if (watching_) {
            try {
                input::Gamepad::GamepadAdded(gamepad_added_);
                input::Gamepad::GamepadRemoved(gamepad_removed_);
                input::RawGameController::RawGameControllerAdded(raw_added_);
                input::RawGameController::RawGameControllerRemoved(raw_removed_);
            } catch (const winrt::hresult_error &) {
            }
        }
        for (Pad &pad : pads_) stopRumble(pad);
        SetWindowLongPtrW(window_, GWLP_USERDATA, 0);
        DestroyWindow(window_);
    }

    void announceAll() {
        for (const Pad &pad : pads_) announce(pad, true);
    }

    void setActive(bool active) {
        active_ = active;
        schedule();
    }

    void rumble(const MigoGamepadRumble &rumble) {
        Pad *pad = find(rumble.index);
        if (pad == nullptr || !pad->gamepad) return;
        if (rumble.duration_ms == 0) {
            stopRumble(*pad);
            return;
        }
        const auto clamp = [](float value) { return static_cast<double>(std::clamp(value, 0.0F, 1.0F)); };
        try {
            pad->gamepad.Vibration(input::GamepadVibration{clamp(rumble.strong_magnitude), clamp(rumble.weak_magnitude),
                                                           clamp(rumble.left_trigger), clamp(rumble.right_trigger)});
        } catch (const winrt::hresult_error &) {
            return;
        }
        // Each replaces what the pad was playing; the motors stop on time.
        SetTimer(window_, kRumbleTimer + pad->index, rumble.duration_ms, nullptr);
    }

private:
    struct Pad {
        std::uint32_t index = 0;
        input::Gamepad gamepad{nullptr};
        input::RawGameController raw{nullptr};
        std::string id;
        std::uint32_t axes = 0;
        std::uint32_t buttons = 0;
        bool standard = false;
        std::uint64_t last_reading = 0;
    };

    static LRESULT CALLBACK procedure(HWND window, UINT message, WPARAM wparam, LPARAM lparam) {
        if (message == WM_NCCREATE) {
            const auto *create = reinterpret_cast<const CREATESTRUCTW *>(lparam);
            SetWindowLongPtrW(window, GWLP_USERDATA, reinterpret_cast<LONG_PTR>(create->lpCreateParams));
        }
        auto *impl = reinterpret_cast<Impl *>(GetWindowLongPtrW(window, GWLP_USERDATA));
        if (message == WM_TIMER && impl != nullptr) {
            if (wparam == kPollTimer) {
                impl->poll();
            } else if (wparam >= kRumbleTimer) {
                KillTimer(window, wparam);
                if (Pad *pad = impl->find(static_cast<std::uint32_t>(wparam - kRumbleTimer)); pad != nullptr) {
                    impl->stopRumble(*pad);
                }
            }
            return 0;
        }
        return DefWindowProcW(window, message, wparam, lparam);
    }

    Pad *find(std::uint32_t index) {
        const auto found = std::find_if(pads_.begin(), pads_.end(), [index](const Pad &pad) { return pad.index == index; });
        return found == pads_.end() ? nullptr : &*found;
    }

    /// The lowest index no pad holds: an emptied slot stays empty only until
    /// the next pad, as content holds on to an index while its pad stays.
    std::uint32_t freeIndex() const {
        for (std::uint32_t index = 0;; ++index) {
            if (std::none_of(pads_.begin(), pads_.end(), [index](const Pad &pad) { return pad.index == index; })) {
                return index;
            }
        }
    }

    static std::string nameOf(const input::RawGameController &controller, const char *kind) {
        std::string name = utf8Of(controller.DisplayName());
        if (name.empty()) name = "Game controller";
        return name + " (" + kind + "Vendor: " + hex4(controller.HardwareVendorId()) +
               " Product: " + hex4(controller.HardwareProductId()) + ")";
    }

    void addGamepad(const input::Gamepad &gamepad) {
        if (std::any_of(pads_.begin(), pads_.end(), [&gamepad](const Pad &pad) { return pad.gamepad == gamepad; })) {
            return;
        }
        Pad pad;
        pad.index = freeIndex();
        pad.gamepad = gamepad;
        pad.standard = true;
        pad.axes = StandardSample::kAxes;
        pad.buttons = StandardSample::kButtons;
        const input::RawGameController controller = input::RawGameController::FromGameController(gamepad);
        pad.id = controller ? nameOf(controller, "STANDARD GAMEPAD ") : std::string("Xbox Controller (STANDARD GAMEPAD)");
        pads_.push_back(pad);
        announce(pads_.back(), true);
        schedule();
    }

    void addRaw(const input::RawGameController &controller) {
        // An Xbox-class pad is its Gamepad's, on the standard layout.
        if (input::Gamepad::FromGameController(controller)) return;
        if (std::any_of(pads_.begin(), pads_.end(), [&controller](const Pad &pad) { return pad.raw == controller; })) {
            return;
        }
        const std::int32_t axes = controller.AxisCount();
        const std::int32_t buttons = controller.ButtonCount();
        // What the engine cannot hold is left out whole: a dropped button is one
        // content is watching.
        if (axes < 0 || buttons < 0 || axes > static_cast<std::int32_t>(MIGO_GAMEPAD_MAX_AXES) ||
            buttons > static_cast<std::int32_t>(MIGO_GAMEPAD_MAX_BUTTONS) || axes + buttons == 0) {
            return;
        }
        Pad pad;
        pad.index = freeIndex();
        pad.raw = controller;
        pad.axes = static_cast<std::uint32_t>(axes);
        pad.buttons = static_cast<std::uint32_t>(buttons);
        pad.id = nameOf(controller, "");
        pads_.push_back(pad);
        announce(pads_.back(), true);
        schedule();
    }

    void remove(const input::Gamepad &gamepad, const input::RawGameController &controller) {
        const auto gone = std::find_if(pads_.begin(), pads_.end(), [&](const Pad &pad) {
            return (gamepad && pad.gamepad == gamepad) || (controller && pad.raw == controller);
        });
        if (gone == pads_.end()) return;
        stopRumble(*gone);
        announce(*gone, false);
        pads_.erase(gone);
        schedule();
    }

    void announce(const Pad &pad, bool connected) {
        if (!sink_.connection) return;
        MigoGamepadInfo info{};
        info.struct_size = static_cast<std::uint32_t>(sizeof info);
        info.abi_version = MIGO_ABI_VERSION_1;
        info.index = pad.index;
        info.axis_count = pad.axes;
        info.button_count = pad.buttons;
        info.haptics = pad.gamepad ? (MIGO_GAMEPAD_HAPTICS_DUAL_RUMBLE | MIGO_GAMEPAD_HAPTICS_TRIGGER_RUMBLE)
                                   : MIGO_GAMEPAD_HAPTICS_NONE;
        info.id_utf8 = pad.id.c_str();
        info.mapping_utf8 = pad.standard ? "standard" : "";
        sink_.connection(info, connected);
    }

    void stopRumble(Pad &pad) {
        KillTimer(window_, kRumbleTimer + pad.index);
        if (!pad.gamepad) return;
        try {
            pad.gamepad.Vibration(input::GamepadVibration{});
        } catch (const winrt::hresult_error &) {
        }
    }

    /// Reading while the game runs and there is a pad to read.
    void schedule() {
        const bool polling = active_ && !pads_.empty();
        if (polling == polling_) return;
        polling_ = polling;
        if (polling) {
            SetTimer(window_, kPollTimer, kPollMs, nullptr);
        } else {
            KillTimer(window_, kPollTimer);
        }
    }

    void poll() {
        for (Pad &pad : pads_) {
            try {
                if (pad.gamepad) {
                    const input::GamepadReading reading = pad.gamepad.GetCurrentReading();
                    if (reading.Timestamp == pad.last_reading) continue;
                    pad.last_reading = reading.Timestamp;
                    XboxReading plain;
                    plain.buttons = static_cast<std::uint32_t>(reading.Buttons);
                    plain.left_trigger = reading.LeftTrigger;
                    plain.right_trigger = reading.RightTrigger;
                    plain.left_x = reading.LeftThumbstickX;
                    plain.left_y = reading.LeftThumbstickY;
                    plain.right_x = reading.RightThumbstickX;
                    plain.right_y = reading.RightThumbstickY;
                    const StandardSample sample = standardSample(plain);
                    send(pad, sample.axes.data(), sample.buttons.data(), reading.Timestamp);
                } else if (pad.raw) {
                    std::vector<input::GameControllerSwitchPosition> switches(
                        static_cast<std::size_t>(std::max(0, pad.raw.SwitchCount())));
                    std::vector<double> positions(pad.axes);
                    // vector<bool> has no data(): the reading goes through an array of its own.
                    std::unique_ptr<bool[]> held(new bool[std::max<std::size_t>(pad.buttons, 1)]());
                    const std::uint64_t timestamp = pad.raw.GetCurrentReading(
                        winrt::array_view<bool>(held.get(), held.get() + pad.buttons), switches, positions);
                    if (timestamp == pad.last_reading) continue;
                    pad.last_reading = timestamp;
                    std::vector<float> axes(pad.axes);
                    // The driver's 0..1, the layout's -1..1.
                    for (std::size_t index = 0; index < axes.size(); ++index) {
                        axes[index] = static_cast<float>(std::clamp(positions[index] * 2.0 - 1.0, -1.0, 1.0));
                    }
                    std::vector<MigoGamepadButton> buttons(pad.buttons);
                    for (std::size_t index = 0; index < buttons.size(); ++index) {
                        buttons[index].flags = held[index] ? (MIGO_GAMEPAD_BUTTON_FLAG_PRESSED | MIGO_GAMEPAD_BUTTON_FLAG_TOUCHED)
                                                           : MIGO_GAMEPAD_BUTTON_FLAG_NONE;
                        buttons[index].value = held[index] ? 1.0F : 0.0F;
                    }
                    send(pad, axes.data(), buttons.data(), timestamp);
                }
            } catch (const winrt::hresult_error &) {
                // A pad that went as it was read: its removal follows.
            }
        }
    }

    void send(const Pad &pad, const float *axes, const MigoGamepadButton *buttons, std::uint64_t timestamp_us) {
        if (!sink_.sample) return;
        MigoGamepadStateEvent event{};
        event.struct_size = static_cast<std::uint32_t>(sizeof event);
        event.abi_version = MIGO_ABI_VERSION_1;
        event.index = pad.index;
        event.axis_count = pad.axes;
        event.button_count = pad.buttons;
        event.axes = axes;
        event.buttons = buttons;
        event.timestamp_ms = static_cast<double>(timestamp_us) / 1000.0;
        sink_.sample(event);
    }

    const Sink sink_;
    GuiQueue queue_;
    const GuiQueue::Post post_;
    HWND window_ = nullptr;
    bool watching_ = false;
    bool active_ = false;
    bool polling_ = false;
    std::vector<Pad> pads_;
    winrt::event_token gamepad_added_{};
    winrt::event_token gamepad_removed_{};
    winrt::event_token raw_added_{};
    winrt::event_token raw_removed_{};
};

Gamepads::Gamepads(Sink sink) : impl_(std::make_unique<Impl>(std::move(sink))) {}
Gamepads::~Gamepads() = default;
void Gamepads::announceAll() { impl_->announceAll(); }
void Gamepads::setActive(bool active) { impl_->setActive(active); }
void Gamepads::rumble(const MigoGamepadRumble &rumble) { impl_->rumble(rumble); }

}  // namespace migo::windows_host

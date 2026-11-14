#include "brightness.hpp"

#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#ifndef NOMINMAX
#define NOMINMAX
#endif
#include <windows.h>
#include <oleauto.h>
#include <highlevelmonitorconfigurationapi.h>
#include <physicalmonitorenumerationapi.h>
#include <wbemidl.h>
#include <wrl/client.h>

#include <algorithm>
#include <cmath>
#include <vector>

namespace migo::windows_host {

namespace {

using Microsoft::WRL::ComPtr;

/// The physical monitors behind the screen `window` is on, released as it goes.
class PhysicalMonitors final {
public:
    explicit PhysicalMonitors(HWND window) {
        const HMONITOR screen = MonitorFromWindow(window, MONITOR_DEFAULTTONEAREST);
        DWORD count = 0;
        if (screen == nullptr || !GetNumberOfPhysicalMonitorsFromHMONITOR(screen, &count) || count == 0) return;
        monitors_.resize(count);
        if (!GetPhysicalMonitorsFromHMONITOR(screen, count, monitors_.data())) monitors_.clear();
    }
    ~PhysicalMonitors() {
        if (!monitors_.empty()) DestroyPhysicalMonitors(static_cast<DWORD>(monitors_.size()), monitors_.data());
    }
    PhysicalMonitors(const PhysicalMonitors &) = delete;
    PhysicalMonitors &operator=(const PhysicalMonitors &) = delete;

    [[nodiscard]] const std::vector<PHYSICAL_MONITOR> &all() const noexcept { return monitors_; }

private:
    std::vector<PHYSICAL_MONITOR> monitors_;
};

/// WMI's root\wmi namespace, where a laptop panel's brightness is.
ComPtr<IWbemServices> wmi() {
    ComPtr<IWbemLocator> locator;
    ComPtr<IWbemServices> services;
    BSTR space = SysAllocString(L"ROOT\\WMI");
    const bool connected =
        SUCCEEDED(CoCreateInstance(CLSID_WbemLocator, nullptr, CLSCTX_INPROC_SERVER, IID_PPV_ARGS(locator.GetAddressOf()))) &&
        SUCCEEDED(locator->ConnectServer(space, nullptr, nullptr, nullptr, 0, nullptr, nullptr, services.GetAddressOf())) &&
        SUCCEEDED(CoSetProxyBlanket(services.Get(), RPC_C_AUTHN_WINNT, RPC_C_AUTHZ_NONE, nullptr, RPC_C_AUTHN_LEVEL_CALL,
                                    RPC_C_IMP_LEVEL_IMPERSONATE, nullptr, EOAC_NONE));
    SysFreeString(space);
    return connected ? services : nullptr;
}

/// The first instance of `wmi_class` WMI has, or none.
ComPtr<IWbemClassObject> first_instance(IWbemServices &services, const wchar_t *query) {
    ComPtr<IEnumWbemClassObject> found;
    BSTR language = SysAllocString(L"WQL");
    BSTR text = SysAllocString(query);
    const HRESULT result = services.ExecQuery(language, text, WBEM_FLAG_FORWARD_ONLY | WBEM_FLAG_RETURN_IMMEDIATELY,
                                              nullptr, found.GetAddressOf());
    SysFreeString(language);
    SysFreeString(text);
    if (FAILED(result)) return nullptr;
    ComPtr<IWbemClassObject> instance;
    ULONG returned = 0;
    if (found->Next(static_cast<LONG>(WBEM_INFINITE), 1, instance.GetAddressOf(), &returned) != WBEM_S_NO_ERROR || returned == 0) {
        return nullptr;
    }
    return instance;
}

std::optional<double> panel_brightness() {
    ComPtr<IWbemServices> services = wmi();
    if (services == nullptr) return std::nullopt;
    ComPtr<IWbemClassObject> panel = first_instance(*services.Get(), L"SELECT CurrentBrightness FROM WmiMonitorBrightness");
    if (panel == nullptr) return std::nullopt;
    VARIANT value;
    VariantInit(&value);
    std::optional<double> brightness;
    if (SUCCEEDED(panel->Get(L"CurrentBrightness", 0, &value, nullptr, nullptr)) && (value.vt == VT_UI1 || value.vt == VT_I4)) {
        brightness = static_cast<double>(value.vt == VT_UI1 ? value.bVal : value.lVal) / 100.0;
    }
    VariantClear(&value);
    return brightness;
}

bool set_panel_brightness(double value) {
    ComPtr<IWbemServices> services = wmi();
    if (services == nullptr) return false;
    ComPtr<IWbemClassObject> methods =
        first_instance(*services.Get(), L"SELECT * FROM WmiMonitorBrightnessMethods");
    if (methods == nullptr) return false;
    // WmiSetBrightness(Timeout, Brightness) on that instance.
    VARIANT path;
    VariantInit(&path);
    ComPtr<IWbemClassObject> klass;
    ComPtr<IWbemClassObject> signature;
    ComPtr<IWbemClassObject> arguments;
    BSTR class_name = SysAllocString(L"WmiMonitorBrightnessMethods");
    BSTR method = SysAllocString(L"WmiSetBrightness");
    bool done = SUCCEEDED(methods->Get(L"__PATH", 0, &path, nullptr, nullptr)) && path.vt == VT_BSTR &&
                SUCCEEDED(services->GetObject(class_name, 0, nullptr, klass.GetAddressOf(), nullptr)) &&
                SUCCEEDED(klass->GetMethod(method, 0, signature.GetAddressOf(), nullptr)) &&
                SUCCEEDED(signature->SpawnInstance(0, arguments.GetAddressOf()));
    if (done) {
        VARIANT timeout;
        VariantInit(&timeout);
        timeout.vt = VT_I4;
        timeout.lVal = 0;
        VARIANT level;
        VariantInit(&level);
        level.vt = VT_UI1;
        level.bVal = static_cast<BYTE>(std::lround(std::clamp(value, 0.0, 1.0) * 100));
        done = SUCCEEDED(arguments->Put(L"Timeout", 0, &timeout, 0)) && SUCCEEDED(arguments->Put(L"Brightness", 0, &level, 0)) &&
               SUCCEEDED(services->ExecMethod(path.bstrVal, method, 0, nullptr, arguments.Get(), nullptr, nullptr));
    }
    SysFreeString(class_name);
    SysFreeString(method);
    VariantClear(&path);
    return done;
}

}  // namespace

std::optional<double> screenBrightness(HWND__ *window) {
    // An external monitor answers over DDC/CI.
    const PhysicalMonitors monitors(window);
    for (const PHYSICAL_MONITOR &monitor : monitors.all()) {
        DWORD lowest = 0;
        DWORD current = 0;
        DWORD highest = 0;
        if (GetMonitorBrightness(monitor.hPhysicalMonitor, &lowest, &current, &highest) && highest > lowest) {
            return static_cast<double>(std::clamp(current, lowest, highest) - lowest) / static_cast<double>(highest - lowest);
        }
    }
    // A laptop's own panel, through WMI.
    return panel_brightness();
}

std::string setScreenBrightness(HWND__ *window, double value) {
    const double clamped = std::clamp(value, 0.0, 1.0);
    const PhysicalMonitors monitors(window);
    for (const PHYSICAL_MONITOR &monitor : monitors.all()) {
        DWORD lowest = 0;
        DWORD current = 0;
        DWORD highest = 0;
        if (GetMonitorBrightness(monitor.hPhysicalMonitor, &lowest, &current, &highest) && highest > lowest) {
            const auto level = static_cast<DWORD>(std::lround(lowest + clamped * (highest - lowest)));
            return SetMonitorBrightness(monitor.hPhysicalMonitor, level) ? std::string() : "the monitor refused the brightness";
        }
    }
    return set_panel_brightness(clamped) ? std::string() : std::string(kNoBrightness);
}

}  // namespace migo::windows_host

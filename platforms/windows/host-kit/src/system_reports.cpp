#include "system_reports.hpp"

#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#ifndef NOMINMAX
#define NOMINMAX
#endif
#ifndef UNICODE
#define UNICODE
#endif
#include <winsock2.h>
#include <windows.h>
#include <commctrl.h>
#include <iphlpapi.h>
#include <netlistmgr.h>
#include <ocidl.h>
#include <wrl/client.h>

#include <atomic>
#include <cstring>
#include <mutex>
#include <optional>
#include <thread>
#include <utility>
#include <vector>

namespace migo::windows_host {

namespace {

using Microsoft::WRL::ComPtr;

constexpr UINT_PTR kPowerWatchId = 0x4D69676F + 7;
// How often a raised low-memory notification is looked at again, to tell when
// it has been lowered and the next one is a new warning.
constexpr DWORD kMemoryRecheckMs = 5000;
// Mobile broadband, which ipifcons.h names IF_TYPE_WWANPP and IF_TYPE_WWANPP2.
constexpr ULONG kMobileBroadband = 243;
constexpr ULONG kMobileBroadband2 = 244;

/// Where a watcher's thread hands its news to the GUI thread: closed before
/// the reports go, after which it hands nothing.
class Relay final {
public:
    explicit Relay(SystemReports::Post post) : post_(std::move(post)) {}

    void send(std::function<void()> task) {
        const std::lock_guard<std::mutex> lock(mutex_);
        if (open_) post_(std::move(task));
    }

    void close() {
        const std::lock_guard<std::mutex> lock(mutex_);
        open_ = false;
    }

private:
    std::mutex mutex_;
    SystemReports::Post post_;
    bool open_ = true;
};

/// The Network List Manager's events, each one a reason to read the network again.
class NetworkEvents final : public INetworkListManagerEvents, public INetworkConnectionEvents {
public:
    explicit NetworkEvents(std::function<void()> changed) : changed_(std::move(changed)) {}

    HRESULT STDMETHODCALLTYPE QueryInterface(REFIID id, void **out) override {
        if (out == nullptr) return E_POINTER;
        if (IsEqualIID(id, __uuidof(IUnknown)) || IsEqualIID(id, __uuidof(INetworkListManagerEvents))) {
            *out = static_cast<INetworkListManagerEvents *>(this);
        } else if (IsEqualIID(id, __uuidof(INetworkConnectionEvents))) {
            *out = static_cast<INetworkConnectionEvents *>(this);
        } else {
            *out = nullptr;
            return E_NOINTERFACE;
        }
        AddRef();
        return S_OK;
    }
    ULONG STDMETHODCALLTYPE AddRef() override { return references_.fetch_add(1, std::memory_order_relaxed) + 1; }
    ULONG STDMETHODCALLTYPE Release() override {
        const ULONG left = references_.fetch_sub(1, std::memory_order_acq_rel) - 1;
        if (left == 0) delete this;
        return left;
    }

    HRESULT STDMETHODCALLTYPE ConnectivityChanged(NLM_CONNECTIVITY) override {
        changed_();
        return S_OK;
    }
    HRESULT STDMETHODCALLTYPE NetworkConnectionConnectivityChanged(GUID, NLM_CONNECTIVITY) override {
        changed_();
        return S_OK;
    }
    HRESULT STDMETHODCALLTYPE NetworkConnectionPropertyChanged(GUID, NLM_CONNECTION_PROPERTY_CHANGE) override {
        changed_();
        return S_OK;
    }

private:
    ~NetworkEvents() = default;

    std::atomic<ULONG> references_{1};
    const std::function<void()> changed_;
};

/// The kind of network an adapter is, in the API's words: a wire is Wi-Fi, the
/// nearest it has; a mobile broadband modem a cellular network of no known
/// generation.
MigoNetworkType type_of(ULONG interface_type) noexcept {
    switch (interface_type) {
        case IF_TYPE_IEEE80211:
        case IF_TYPE_ETHERNET_CSMACD:
            return MIGO_NETWORK_WIFI;
        case kMobileBroadband:
        case kMobileBroadband2:
        default:
            return MIGO_NETWORK_UNKNOWN;
    }
}

/// The kind of the adapter `adapter` names, from the system's adapter table.
MigoNetworkType adapter_type(const GUID &adapter) {
    wchar_t wide[40] = {};
    if (StringFromGUID2(adapter, wide, 40) == 0) return MIGO_NETWORK_UNKNOWN;
    char name[40] = {};
    for (std::size_t index = 0; index + 1 < sizeof name && wide[index] != L'\0'; ++index) {
        name[index] = static_cast<char>(wide[index]);
    }
    ULONG size = 16 * 1024;
    std::vector<unsigned char> table;
    ULONG result = ERROR_BUFFER_OVERFLOW;
    // The table can grow between the measure and the read.
    for (int attempt = 0; attempt < 3 && result == ERROR_BUFFER_OVERFLOW; ++attempt) {
        table.resize(size);
        result = GetAdaptersAddresses(AF_UNSPEC,
                                      GAA_FLAG_SKIP_UNICAST | GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST |
                                          GAA_FLAG_SKIP_DNS_SERVER,
                                      nullptr, reinterpret_cast<IP_ADAPTER_ADDRESSES *>(table.data()), &size);
    }
    if (result != NO_ERROR) return MIGO_NETWORK_UNKNOWN;
    for (const auto *entry = reinterpret_cast<const IP_ADAPTER_ADDRESSES *>(table.data()); entry != nullptr;
         entry = entry->Next) {
        if (entry->AdapterName != nullptr && _stricmp(entry->AdapterName, name) == 0) return type_of(entry->IfType);
    }
    return MIGO_NETWORK_UNKNOWN;
}

}  // namespace

class SystemReports::Impl final {
public:
    Impl(MigoSession *session, HWND root, Post post)
        : session_(session), root_(root), relay_(std::make_shared<Relay>(std::move(post))) {
        watchPower();
        reportBattery();
        watchNetwork();
        watchMemory();
    }

    ~Impl() {
        // Nothing a watcher says reaches the Session from here.
        relay_->close();
        *alive_ = false;
        if (memory_thread_.joinable()) {
            SetEvent(memory_stop_);
            memory_thread_.join();
        }
        if (memory_stop_ != nullptr) CloseHandle(memory_stop_);
        if (low_memory_ != nullptr) CloseHandle(low_memory_);
        if (list_point_ != nullptr) list_point_->Unadvise(list_cookie_);
        if (connection_point_ != nullptr) connection_point_->Unadvise(connection_cookie_);
        for (HPOWERNOTIFY notification : power_notifications_) UnregisterPowerSettingNotification(notification);
        if (watching_root_) RemoveWindowSubclass(root_, &Impl::powerWatch, kPowerWatchId);
    }

private:
    /// A task for the GUI thread that does nothing once the reports are gone.
    std::function<void()> guarded(void (Impl::*work)()) {
        std::shared_ptr<bool> alive = alive_;
        return [this, alive, work] {
            if (*alive) (this->*work)();
        };
    }

    // ---- the battery ----------------------------------------------------------------------------------

    void watchPower() {
        if (root_ == nullptr) return;
        watching_root_ = SetWindowSubclass(root_, &Impl::powerWatch, kPowerWatchId, reinterpret_cast<DWORD_PTR>(this));
        // Sent to the App's window: its charge, its supply, the energy saver.
        for (const GUID *setting : {&GUID_BATTERY_PERCENTAGE_REMAINING, &GUID_ACDC_POWER_SOURCE, &GUID_POWER_SAVING_STATUS}) {
            if (HPOWERNOTIFY notification = RegisterPowerSettingNotification(root_, setting, DEVICE_NOTIFY_WINDOW_HANDLE);
                notification != nullptr) {
                power_notifications_.push_back(notification);
            }
        }
    }

    static LRESULT CALLBACK powerWatch(HWND window, UINT message, WPARAM wparam, LPARAM lparam, UINT_PTR,
                                       DWORD_PTR reference) {
        auto *impl = reinterpret_cast<Impl *>(reference);
        if (message == WM_POWERBROADCAST && (wparam == PBT_POWERSETTINGCHANGE || wparam == PBT_APMPOWERSTATUSCHANGE)) {
            impl->reportBattery();
        } else if (message == WM_NCDESTROY) {
            impl->watching_root_ = false;
            RemoveWindowSubclass(window, &Impl::powerWatch, kPowerWatchId);
        }
        return DefSubclassProc(window, message, wparam, lparam);
    }

    void reportBattery() {
        SYSTEM_POWER_STATUS status{};
        if (!GetSystemPowerStatus(&status)) return;
        // No battery, or none the system can read: nothing to report.
        if ((status.BatteryFlag & BATTERY_FLAG_NO_BATTERY) != 0 || status.BatteryFlag == BATTERY_FLAG_UNKNOWN ||
            status.BatteryLifePercent > 100) {
            return;
        }
        MigoBatteryFlags flags = MIGO_BATTERY_FLAG_NONE;
        if ((status.BatteryFlag & BATTERY_FLAG_CHARGING) != 0) flags |= MIGO_BATTERY_FLAG_CHARGING;
        // The energy saver is Windows' low-power mode.
        if (status.SystemStatusFlag == 1) flags |= MIGO_BATTERY_FLAG_LOW_POWER_MODE;
        const std::uint32_t level = status.BatteryLifePercent;
        if (battery_.has_value() && battery_->first == level && battery_->second == flags) return;
        battery_ = std::make_pair(level, flags);
        (void)migo_session_set_battery_status(session_, level, flags);
    }

    // ---- the network ----------------------------------------------------------------------------------

    void watchNetwork() {
        if (FAILED(CoCreateInstance(__uuidof(NetworkListManager), nullptr, CLSCTX_ALL,
                                    IID_PPV_ARGS(network_.GetAddressOf())))) {
            return;
        }
        reportNetwork();
        ComPtr<IConnectionPointContainer> points;
        if (FAILED(network_.As(&points))) return;
        // The events arrive on whatever thread COM calls from; the reading is the GUI thread's.
        std::shared_ptr<Relay> relay = relay_;
        std::function<void()> read_again = guarded(&Impl::reportNetwork);
        auto *events = new NetworkEvents([relay, read_again] { relay->send(read_again); });
        if (SUCCEEDED(points->FindConnectionPoint(__uuidof(INetworkListManagerEvents), list_point_.GetAddressOf()))) {
            if (FAILED(list_point_->Advise(static_cast<INetworkListManagerEvents *>(events), &list_cookie_))) {
                list_point_.Reset();
            }
        }
        if (SUCCEEDED(points->FindConnectionPoint(__uuidof(INetworkConnectionEvents),
                                                  connection_point_.GetAddressOf()))) {
            if (FAILED(connection_point_->Advise(static_cast<INetworkListManagerEvents *>(events),
                                                 &connection_cookie_))) {
                connection_point_.Reset();
            }
        }
        events->Release();
    }

    void reportNetwork() {
        if (network_ == nullptr) return;
        NLM_CONNECTIVITY overall = NLM_CONNECTIVITY_DISCONNECTED;
        if (FAILED(network_->GetConnectivity(&overall))) return;
        constexpr int kInternet = NLM_CONNECTIVITY_IPV4_INTERNET | NLM_CONNECTIVITY_IPV6_INTERNET;
        MigoNetworkType type = MIGO_NETWORK_NONE;
        if (overall != NLM_CONNECTIVITY_DISCONNECTED) {
            // The connection that reaches the internet; failing that, the first connected one.
            type = MIGO_NETWORK_UNKNOWN;
            bool found = false;
            bool found_internet = false;
            ComPtr<IEnumNetworkConnections> connections;
            if (SUCCEEDED(network_->GetNetworkConnections(connections.GetAddressOf()))) {
                ComPtr<INetworkConnection> connection;
                ULONG fetched = 0;
                while (connections->Next(1, connection.ReleaseAndGetAddressOf(), &fetched) == S_OK && fetched == 1) {
                    VARIANT_BOOL connected = VARIANT_FALSE;
                    if (FAILED(connection->get_IsConnected(&connected)) || connected != VARIANT_TRUE) continue;
                    NLM_CONNECTIVITY reach = NLM_CONNECTIVITY_DISCONNECTED;
                    (void)connection->GetConnectivity(&reach);
                    const bool internet = (reach & kInternet) != 0;
                    if (found && (found_internet || !internet)) continue;
                    GUID adapter{};
                    type = SUCCEEDED(connection->GetAdapterId(&adapter)) ? adapter_type(adapter) : MIGO_NETWORK_UNKNOWN;
                    found = true;
                    found_internet = internet;
                }
            }
        }
        // Connected is reaching the internet; a network that does not -- a
        // LAN alone -- is still the network the device is on.
        const bool connected = type != MIGO_NETWORK_NONE && (overall & kInternet) != 0;
        if (network_reported_ && type == network_type_ && connected == network_connected_) return;
        network_reported_ = true;
        network_type_ = type;
        network_connected_ = connected;
        (void)migo_session_set_network_status(session_, type, connected ? 1U : 0U);
    }

    // ---- memory -----------------------------------------------------------------------------------

    void watchMemory() {
        low_memory_ = CreateMemoryResourceNotification(LowMemoryResourceNotification);
        memory_stop_ = CreateEventW(nullptr, TRUE, FALSE, nullptr);
        if (low_memory_ == nullptr || memory_stop_ == nullptr) return;
        std::shared_ptr<Relay> relay = relay_;
        std::function<void()> warn = guarded(&Impl::warnMemory);
        memory_thread_ = std::thread([low = low_memory_, stop = memory_stop_, relay, warn] {
            for (;;) {
                const HANDLE waits[2] = {stop, low};
                if (WaitForMultipleObjects(2, waits, FALSE, INFINITE) != WAIT_OBJECT_0 + 1) return;
                relay->send(warn);
                // Raised until memory comes back: one warning for it, not one a moment.
                for (;;) {
                    if (WaitForSingleObject(stop, kMemoryRecheckMs) != WAIT_TIMEOUT) return;
                    BOOL raised = FALSE;
                    if (!QueryMemoryResourceNotification(low, &raised)) return;
                    if (!raised) break;
                }
            }
        });
    }

    void warnMemory() { (void)migo_session_post_memory_warning(session_, MIGO_MEMORY_WARNING_CRITICAL); }

    MigoSession *const session_;
    const HWND root_;
    const std::shared_ptr<Relay> relay_;
    std::shared_ptr<bool> alive_ = std::make_shared<bool>(true);

    bool watching_root_ = false;
    std::vector<HPOWERNOTIFY> power_notifications_;
    std::optional<std::pair<std::uint32_t, MigoBatteryFlags>> battery_;

    ComPtr<INetworkListManager> network_;
    ComPtr<IConnectionPoint> list_point_;
    ComPtr<IConnectionPoint> connection_point_;
    DWORD list_cookie_ = 0;
    DWORD connection_cookie_ = 0;
    bool network_reported_ = false;
    MigoNetworkType network_type_ = MIGO_NETWORK_NONE;
    bool network_connected_ = false;

    HANDLE low_memory_ = nullptr;
    HANDLE memory_stop_ = nullptr;
    std::thread memory_thread_;
};

SystemReports::SystemReports(MigoSession *session, HWND__ *root, Post post)
    : impl_(std::make_unique<Impl>(session, root, std::move(post))) {}

SystemReports::~SystemReports() = default;

}  // namespace migo::windows_host

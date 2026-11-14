#ifndef MIGO_WINDOWS_HOST_KIT_SYSTEM_REPORTS_HPP_
#define MIGO_WINDOWS_HOST_KIT_SYSTEM_REPORTS_HPP_

// Internal to the Host Kit, not installed: what Windows says about the
// machine, told to the Session.

#include <migo/migo.h>

#include <functional>
#include <memory>

struct HWND__;

namespace migo::windows_host {

/// The battery, the network and memory pressure, as Windows says they are and
/// each time they change, reported to the Session, which keeps what it was
/// told -- the Linux Host Kit's reports, for Win32:
///
/// - The battery (getBatteryInfo): its charge, whether it is charging, and the
///   energy saver as the low-power mode, read from the system's power status
///   at each power-setting notification the App's window is sent. A PC with
///   no battery reports none, and content's call fails "not supported", as the
///   header says it should.
/// - The network (getNetworkType, onNetworkStatusChange): the Network List
///   Manager's account -- connected when it reaches the internet -- and the
///   kind of adapter the connection runs over: Wi-Fi, a wire (Wi-Fi, the
///   nearest the API has), a mobile broadband modem (a cellular network of no
///   known generation). Read again at each of its events.
/// - Memory (onMemoryWarning): the system's low-memory notification, posted as
///   CRITICAL -- Windows raises it only when physical memory is nearly gone --
///   once each time it is raised.
///
/// COM: the GUI thread is the apartment the network's events arrive in, or, in
/// a multithreaded one, they are brought to it. `post` runs a task on the GUI
/// thread; what reaches it after this object is gone does nothing. GUI thread
/// only; it goes before the Session does.
class SystemReports final {
public:
    using Post = std::function<void(std::function<void()>)>;

    SystemReports(MigoSession *session, HWND__ *root, Post post);
    ~SystemReports();

    SystemReports(const SystemReports &) = delete;
    SystemReports &operator=(const SystemReports &) = delete;

private:
    class Impl;
    std::unique_ptr<Impl> impl_;
};

}  // namespace migo::windows_host

#endif  // MIGO_WINDOWS_HOST_KIT_SYSTEM_REPORTS_HPP_

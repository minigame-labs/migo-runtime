#ifndef MIGO_LINUX_QT6_MEMORY_MONITOR_HPP_
#define MIGO_LINUX_QT6_MEMORY_MONITOR_HPP_

// Internal to the Host Kit, not installed: the desktop warning that memory runs low.

#include <migo/session.h>

#include <QDBusConnection>
#include <QObject>

#include <functional>

namespace migo::linux_host::qt6 {

/// The desktop's warnings that memory runs low, heard where GLib's memory
/// monitor hears them: low-memory-monitor's on the system bus, or, inside a
/// sandbox, the desktop portal's on the session bus, which relays them. A
/// desktop that runs neither warns of nothing.
class MemoryMonitor final : public QObject {
    Q_OBJECT

public:
    enum class Source { LowMemoryMonitor, Portal };

    /// `bus` is where `source` answers: the system's for low-memory-monitor,
    /// the session's for the portal. `warn` is called with each warning's level.
    MemoryMonitor(QDBusConnection bus, Source source, std::function<void(MigoMemoryWarningLevel)> warn,
                  QObject *parent = nullptr);

private Q_SLOTS:
    /// low-memory-monitor's levels: 50 low, caches should go; 100 medium; 255
    /// critical, the system is about to end processes.
    void lowMemoryWarning(uchar level);

private:
    std::function<void(MigoMemoryWarningLevel)> warn_;
};

}  // namespace migo::linux_host::qt6

#endif  // MIGO_LINUX_QT6_MEMORY_MONITOR_HPP_

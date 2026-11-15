#include "memory_monitor.hpp"

#include <utility>

namespace migo::linux_host::qt6 {

MemoryMonitor::MemoryMonitor(QDBusConnection bus, Source source, std::function<void(MigoMemoryWarningLevel)> warn,
                             QObject *parent)
    : QObject(parent), warn_(std::move(warn)) {
    const bool portal = source == Source::Portal;
    bus.connect(portal ? QStringLiteral("org.freedesktop.portal.Desktop") : QStringLiteral("org.freedesktop.LowMemoryMonitor"),
                portal ? QStringLiteral("/org/freedesktop/portal/desktop") : QStringLiteral("/org/freedesktop/LowMemoryMonitor"),
                portal ? QStringLiteral("org.freedesktop.portal.MemoryMonitor") : QStringLiteral("org.freedesktop.LowMemoryMonitor"),
                QStringLiteral("LowMemoryWarning"), this, SLOT(lowMemoryWarning(uchar)));
}

void MemoryMonitor::lowMemoryWarning(uchar level) {
    constexpr uchar kLow = 50;
    constexpr uchar kMedium = 100;
    constexpr uchar kCritical = 255;
    if (level >= kCritical) {
        warn_(MIGO_MEMORY_WARNING_CRITICAL);
    } else if (level >= kMedium) {
        warn_(MIGO_MEMORY_WARNING_LOW);
    } else if (level >= kLow) {
        warn_(MIGO_MEMORY_WARNING_MODERATE);
    }
}

}  // namespace migo::linux_host::qt6

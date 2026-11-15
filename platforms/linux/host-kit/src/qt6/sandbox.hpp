#ifndef MIGO_LINUX_QT6_SANDBOX_HPP_
#define MIGO_LINUX_QT6_SANDBOX_HPP_

// Internal to the Host Kit, not installed.

#include <QFile>
#include <QString>
#include <QtGlobal>

namespace migo::linux_host::qt6 {

/// Whether the App runs inside a sandbox (Flatpak, Snap), where the desktop's
/// portals stand between it and the system.
inline bool insideSandbox() {
    return QFile::exists(QStringLiteral("/.flatpak-info")) || qEnvironmentVariableIsSet("SNAP");
}

}  // namespace migo::linux_host::qt6

#endif  // MIGO_LINUX_QT6_SANDBOX_HPP_

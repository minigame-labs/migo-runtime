#ifndef MIGO_LINUX_QT6_STAGING_HPP_
#define MIGO_LINUX_QT6_STAGING_HPP_

// Internal to the Host Kit, not installed: where the files the services hand
// the Session wait until it takes them.

#include <QDir>
#include <QString>
#include <QTemporaryDir>
#include <QUuid>

#include <memory>

namespace migo::linux_host::qt6 {

/// A directory of the Host Kit's own for the files it hands the Session -- a
/// picked image's copy, a recording -- which the Session takes as the call is
/// answered or the event posted; what it does not take is removed then, and
/// whatever is left goes with the directory. Made in `root` -- the App's cache,
/// usually on the volume of the engine's `cache_dir`, so taking a file is a
/// rename rather than a copy -- or the system's temporary directory when there
/// is no `root`. Any thread once made.
class Staging final {
public:
    explicit Staging(const QString &root) : dir_(make(root)) {}

    /// A new file's path, with `suffix` when it has one; empty when there is
    /// nowhere to put one.
    [[nodiscard]] QString path(const QString &suffix) const {
        if (dir_ == nullptr) return {};
        QString name = QUuid::createUuid().toString(QUuid::WithoutBraces);
        if (!suffix.isEmpty()) name += QLatin1Char('.') + suffix;
        return dir_->filePath(name);
    }

    /// The directory, or empty when there is none.
    [[nodiscard]] QString directory() const { return dir_ == nullptr ? QString() : dir_->path(); }

private:
    static std::unique_ptr<QTemporaryDir> make(const QString &root) {
        const QString pattern = QStringLiteral("/migo-host-files-XXXXXX");
        if (!root.isEmpty() && QDir().mkpath(root)) {
            auto dir = std::make_unique<QTemporaryDir>(root + pattern);
            if (dir->isValid()) return dir;
        }
        // The system's temporary directory: another volume more often than not,
        // where taking a file costs the Session a copy.
        auto dir = std::make_unique<QTemporaryDir>(QDir::tempPath() + pattern);
        if (dir->isValid()) return dir;
        qWarning("migo host kit: no staging directory for files: %s", qPrintable(dir->errorString()));
        return nullptr;
    }

    std::unique_ptr<QTemporaryDir> dir_;
};

}  // namespace migo::linux_host::qt6

#endif  // MIGO_LINUX_QT6_STAGING_HPP_

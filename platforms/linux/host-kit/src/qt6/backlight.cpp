#include "backlight.hpp"

#include <QDBusMessage>
#include <QDBusPendingCallWatcher>
#include <QDBusPendingReply>
#include <QDir>
#include <QFile>

#include <cmath>
#include <utility>

namespace migo::linux_host::qt6 {

namespace {

/// A sysfs attribute: one line, a number or a word.
QByteArray attribute(const QString &path) {
    QFile file(path);
    if (!file.open(QIODevice::ReadOnly)) return {};
    return file.readLine().trimmed();
}

std::optional<std::uint32_t> number(const QString &path) {
    bool ok = false;
    const std::uint32_t value = attribute(path).toUInt(&ok);
    return ok ? std::optional<std::uint32_t>(value) : std::nullopt;
}

/// How the desktop's tools rank a backlight's type; lower first, none for one they do not use.
std::optional<int> rank(const QByteArray &type) {
    if (type == "firmware") return 0;
    if (type == "platform") return 1;
    if (type == "raw") return 2;
    return std::nullopt;
}

}  // namespace

Backlight::Backlight(QString sysfs, QDBusConnection bus, QObject *parent)
    : QObject(parent), sysfs_(std::move(sysfs)), bus_(std::move(bus)) {}

std::optional<Backlight::Device> Backlight::device() const {
    std::optional<Device> picked;
    int picked_rank = 0;
    // Sorted by name, so equals are picked the same way every time.
    const QStringList names = QDir(sysfs_).entryList(QDir::Dirs | QDir::NoDotAndDotDot, QDir::Name);
    for (const QString &name : names) {
        const QString path = sysfs_ + QLatin1Char('/') + name;
        const std::optional<int> type = rank(attribute(path + QStringLiteral("/type")));
        const std::optional<std::uint32_t> max = number(path + QStringLiteral("/max_brightness"));
        const std::optional<std::uint32_t> brightness = number(path + QStringLiteral("/brightness"));
        if (!type.has_value() || !max.has_value() || *max == 0 || !brightness.has_value()) continue;
        if (picked.has_value() && *type >= picked_rank) continue;
        picked = Device{name, *max, *brightness};
        picked_rank = *type;
    }
    return picked;
}

std::optional<double> Backlight::brightness() const {
    const std::optional<Device> backlight = device();
    if (!backlight.has_value()) return std::nullopt;
    return qBound(0.0, static_cast<double>(backlight->brightness) / backlight->max, 1.0);
}

void Backlight::setBrightness(double value, std::function<void(const QString &error)> done) {
    const std::optional<Device> backlight = device();
    if (!backlight.has_value()) {
        done(QStringLiteral("the screen has no backlight the system controls"));
        return;
    }
    const auto level = static_cast<std::uint32_t>(std::lround(qBound(0.0, value, 1.0) * backlight->max));
    QDBusMessage call = QDBusMessage::createMethodCall(
        QStringLiteral("org.freedesktop.login1"), QStringLiteral("/org/freedesktop/login1/session/auto"),
        QStringLiteral("org.freedesktop.login1.Session"), QStringLiteral("SetBrightness"));
    call << QStringLiteral("backlight") << backlight->name << qMax<std::uint32_t>(level, 1U);
    auto *watcher = new QDBusPendingCallWatcher(bus_.asyncCall(call), this);
    connect(watcher, &QDBusPendingCallWatcher::finished, this, [watcher, done = std::move(done)] {
        const QDBusPendingReply<> reply = *watcher;
        watcher->deleteLater();
        done(reply.isError() ? reply.error().message() : QString());
    });
}

}  // namespace migo::linux_host::qt6

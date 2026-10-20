#include "screen_awake.hpp"

#include <QDBusMessage>
#include <QDBusObjectPath>
#include <QDBusPendingCallWatcher>
#include <QDBusPendingReply>
#include <QGuiApplication>
#include <QVariantMap>

#include <utility>

namespace migo::linux_host::qt6 {

namespace {

const QString kReason = QStringLiteral("A game asked to keep the display on");

const QString kPortal = QStringLiteral("org.freedesktop.portal.Desktop");
const QString kPortalPath = QStringLiteral("/org/freedesktop/portal/desktop");
/// The portal's Inhibit flag for the session's idle.
constexpr std::uint32_t kInhibitIdle = 8;

const QString kScreenSaver = QStringLiteral("org.freedesktop.ScreenSaver");
const QString kScreenSaverPath = QStringLiteral("/org/freedesktop/ScreenSaver");

}  // namespace

ScreenAwake::ScreenAwake(QDBusConnection bus, QObject *parent) : QObject(parent), bus_(std::move(bus)) {}

ScreenAwake::~ScreenAwake() { release(); }

void ScreenAwake::keepAwake(bool on, std::uint64_t window) {
    if (on == wanted_) return;
    wanted_ = on;
    if (asking_) return;  // The answer on its way sees what the game wants by then.
    if (on) {
        inhibit(window);
    } else {
        release();
    }
}

void ScreenAwake::inhibit(std::uint64_t window) {
    asking_ = true;
    QDBusMessage call = QDBusMessage::createMethodCall(kPortal, kPortalPath, QStringLiteral("org.freedesktop.portal.Inhibit"),
                                                       QStringLiteral("Inhibit"));
    // The portal names an X11 window "x11:" and its id in hexadecimal.
    call << QStringLiteral("x11:%1").arg(window, 0, 16) << kInhibitIdle
         << QVariantMap{{QStringLiteral("reason"), kReason}};
    auto *watcher = new QDBusPendingCallWatcher(bus_.asyncCall(call), this);
    connect(watcher, &QDBusPendingCallWatcher::finished, this, [this, watcher] {
        const QDBusPendingReply<QDBusObjectPath> reply = *watcher;
        watcher->deleteLater();
        if (reply.isError()) {
            inhibitThroughScreenSaver();
            return;
        }
        granted(Held::Portal, reply.value().path(), 0);
    });
}

void ScreenAwake::inhibitThroughScreenSaver() {
    QDBusMessage call = QDBusMessage::createMethodCall(kScreenSaver, kScreenSaverPath, kScreenSaver,
                                                       QStringLiteral("Inhibit"));
    const QString name = QGuiApplication::applicationDisplayName();
    call << (name.isEmpty() ? QStringLiteral("Migo") : name) << kReason;
    auto *watcher = new QDBusPendingCallWatcher(bus_.asyncCall(call), this);
    connect(watcher, &QDBusPendingCallWatcher::finished, this, [this, watcher] {
        const QDBusPendingReply<std::uint32_t> reply = *watcher;
        watcher->deleteLater();
        if (reply.isError()) {
            asking_ = false;
            // Nothing on this desktop holds its idle off: the display sleeps as it would.
            qWarning("migo host kit: setKeepScreenOn: neither the desktop portal nor the screen saver answers (%s)",
                     qPrintable(reply.error().message()));
            return;
        }
        granted(Held::ScreenSaver, QString(), reply.value());
    });
}

void ScreenAwake::granted(Held how, const QString &portal_request, std::uint32_t cookie) {
    asking_ = false;
    held_ = how;
    portal_request_ = portal_request;
    cookie_ = cookie;
    // The game stopped asking while the desktop answered.
    if (!wanted_) release();
}

void ScreenAwake::release() {
    // Neither answer is waited for: there is nothing to do if letting go failed.
    switch (held_) {
        case Held::None:
            return;
        case Held::Portal:
            bus_.send(QDBusMessage::createMethodCall(kPortal, portal_request_,
                                                     QStringLiteral("org.freedesktop.portal.Request"),
                                                     QStringLiteral("Close")));
            break;
        case Held::ScreenSaver: {
            QDBusMessage call = QDBusMessage::createMethodCall(kScreenSaver, kScreenSaverPath, kScreenSaver,
                                                               QStringLiteral("UnInhibit"));
            call << cookie_;
            bus_.send(call);
            break;
        }
    }
    held_ = Held::None;
    portal_request_.clear();
    cookie_ = 0;
}

}  // namespace migo::linux_host::qt6

#include "fake_migo.hpp"
#include "screen_awake.hpp"
#include "system_reports.hpp"

#include <migo/linux/qt6/managed_session.hpp>

#include <QApplication>
#include <QDBusConnection>
#include <QDBusMessage>
#include <QDeadlineTimer>
#include <QDBusObjectPath>
#include <QVariantMap>
#include <QVBoxLayout>
#include <QSignalSpy>
#include <QThread>
#include <QtTest>

#include <memory>
#include <string>
#include <vector>

using migo::linux_host::SurfaceState;
using migo::linux_host::qt6::MigoManagedSession;
using migo::linux_host::qt6::BatteryReport;
using migo::linux_host::qt6::NetworkReport;
using migo::linux_host::qt6::ScreenAwake;

namespace {

const QString kRequestPath = QStringLiteral("/org/freedesktop/portal/desktop/request/1_1/migo");

/// What closing the portal's request ends: the inhibition.
class FakeRequest final : public QObject {
    Q_OBJECT
    Q_CLASSINFO("D-Bus Interface", "org.freedesktop.portal.Request")

public:
    int closed = 0;

public Q_SLOTS:
    void Close() { ++closed; }
};

/// The desktop portal's Inhibit, on the session bus.
class FakePortal final : public QObject {
    Q_OBJECT
    Q_CLASSINFO("D-Bus Interface", "org.freedesktop.portal.Inhibit")

public:
    QStringList asked;
    FakeRequest request;

public Q_SLOTS:
    QDBusObjectPath Inhibit(const QString &window, uint flags, const QVariantMap &options) {
        asked << QStringLiteral("%1 %2 %3").arg(window).arg(flags).arg(options.value(QStringLiteral("reason")).toString());
        QDBusConnection::sessionBus().registerObject(kRequestPath, &request, QDBusConnection::ExportAllSlots);
        return QDBusObjectPath(kRequestPath);
    }
};

/// The freedesktop screen saver, on the session bus.
class FakeScreenSaver final : public QObject {
    Q_OBJECT
    Q_CLASSINFO("D-Bus Interface", "org.freedesktop.ScreenSaver")

public:
    QStringList asked;
    QList<uint> released;

public Q_SLOTS:
    uint Inhibit(const QString &application, const QString &reason) {
        asked << application + QStringLiteral(": ") + reason;
        return 7;
    }
    void UnInhibit(uint cookie) { released << cookie; }
};

/// UPower's display device: every battery the machine has, as one.
class FakeBattery final : public QObject {
    Q_OBJECT
    Q_CLASSINFO("D-Bus Interface", "org.freedesktop.UPower.Device")
    Q_PROPERTY(uint Type MEMBER type)
    Q_PROPERTY(bool IsPresent MEMBER present)
    Q_PROPERTY(double Percentage MEMBER percentage)
    Q_PROPERTY(uint State MEMBER state)

public:
    uint type = 2;  // a battery
    bool present = true;
    double percentage = 76.4;
    uint state = 2;  // discharging
};

/// power-profiles-daemon, as UPower's.
class FakePowerProfiles final : public QObject {
    Q_OBJECT
    Q_CLASSINFO("D-Bus Interface", "org.freedesktop.UPower.PowerProfiles")
    Q_PROPERTY(QString ActiveProfile MEMBER profile)

public:
    QString profile = QStringLiteral("power-saver");
};

/// NetworkManager.
class FakeNetworkManager final : public QObject {
    Q_OBJECT
    Q_CLASSINFO("D-Bus Interface", "org.freedesktop.NetworkManager")
    Q_PROPERTY(uint State MEMBER state)
    Q_PROPERTY(uint Connectivity MEMBER connectivity)
    Q_PROPERTY(QString PrimaryConnectionType MEMBER primary)
    Q_PROPERTY(bool WirelessEnabled MEMBER wireless)
    Q_PROPERTY(bool WirelessHardwareEnabled MEMBER radio)

public:
    uint state = 70;         // connected, globally
    uint connectivity = 4;   // full
    QString primary = QStringLiteral("802-11-wireless");
    bool wireless = true;
    bool radio = true;
};

/// Say, as a D-Bus object does, that its properties of `interface` changed.
void announce(const QString &path, const QString &interface) {
    QDBusMessage signal =
        QDBusMessage::createSignal(path, QStringLiteral("org.freedesktop.DBus.Properties"), QStringLiteral("PropertiesChanged"));
    signal << interface << QVariantMap() << QStringList();
    QDBusConnection::sessionBus().send(signal);
}

/// A fake standing on the session bus as `service` at `path` while it lives.
class OnTheBus {
public:
    OnTheBus(QObject &fake, const QString &service, const QString &path) : service_(service), path_(path) {
        QDBusConnection bus = QDBusConnection::sessionBus();
        registered_ = bus.registerService(service) &&
                      bus.registerObject(path, &fake, QDBusConnection::ExportAllSlots | QDBusConnection::ExportAllProperties);
    }
    ~OnTheBus() {
        QDBusConnection bus = QDBusConnection::sessionBus();
        bus.unregisterObject(path_);
        bus.unregisterObject(kRequestPath);
        bus.unregisterService(service_);
    }
    [[nodiscard]] bool registered() const { return registered_; }

private:
    QString service_;
    QString path_;
    bool registered_ = false;
};

/// Retires an attached Managed session on the way out, however the test left:
/// destroyed with its view attached, it would terminate the test run.
struct ReleasedOnExit {
    MigoManagedSession &managed;
    ~ReleasedOnExit() {
        if (managed.isClosed()) return;
        (void)managed.close();
        if (managed.view() != nullptr) {
            fake_migo::set_release_status(managed.view()->generation(), MIGO_SURFACE_RELEASE_RELEASED);
        }
        QDeadlineTimer deadline(5000);
        while (!managed.isClosed() && !deadline.hasExpired()) QCoreApplication::processEvents(QEventLoop::AllEvents, 20);
    }
};

bool require_xcb(const char *why, const char *file, int line) {
    if (QGuiApplication::platformName() == QStringLiteral("xcb")) return true;
    QTest::qSkip(why, file, line);
    return false;
}

}  // namespace

class QtManagedSessionTest final : public QObject {
    Q_OBJECT

private slots:
    void init() { fake_migo::reset(); }

    /// The wrapper owns a Session and a view, and installs the table once.
    void constructing_creates_a_session_and_installs_its_callbacks() {
        QWidget container;
        MigoManagedSession managed(*fake_migo::engine(), container);

        QVERIFY(managed.isValid());
        QCOMPARE(managed.lastError(), MIGO_OK);
        QVERIFY(managed.view() != nullptr);
        QCOMPARE(fake_migo::calls().session_create, 1);
        QCOMPARE(fake_migo::calls().set_callbacks, 1);
        QVERIFY(fake_migo::session_is_alive());

        QCOMPARE(managed.close(), MIGO_OK);
        QVERIFY(managed.isClosed());
    }

    /// The soft keyboard is not claimed, and not claimed in part.
    ///
    /// The three callbacks install together or not at all. This wrapper cannot
    /// honour them -- the common mini-game platform's model needs the host to own a text field and report
    /// its whole current value, and a desktop host has a physical keyboard
    /// whose input already reaches content as key and composition events. So
    /// content's `migo.showKeyboard` correctly reports failure rather than
    /// opening a keyboard that reports nothing back.
    void the_soft_keyboard_capability_is_declined_whole() {
        QWidget container;
        MigoManagedSession managed(*fake_migo::engine(), container);
        QVERIFY(managed.isValid());

        const MigoHostCallbacks &installed = fake_migo::installed_callbacks();
        // Compared as booleans: QCOMPARE cannot take a function pointer, and
        // the assertion is about presence, not identity.
        QVERIFY(installed.on_show_keyboard == nullptr);
        QVERIFY(installed.on_hide_keyboard == nullptr);
        QVERIFY(installed.on_update_keyboard == nullptr);
        // But the capabilities it does claim are there.
        QVERIFY(installed.dispatch != nullptr);
        QVERIFY(installed.on_ready != nullptr);
        QVERIFY(installed.on_request_frame != nullptr);

        QCOMPARE(managed.close(), MIGO_OK);
    }

    /// The display is kept awake through the desktop portal's Inhibit, for the
    /// game's window, and let go when the game stops asking.
    void the_display_is_kept_awake_through_the_portal() {
        QVERIFY2(QDBusConnection::sessionBus().isConnected(),
                 "no session bus: run this test under dbus-run-session, as scripts/test-linux-qt-host-kit.sh does");
        FakePortal portal;
        OnTheBus standing(portal, QStringLiteral("org.freedesktop.portal.Desktop"),
                          QStringLiteral("/org/freedesktop/portal/desktop"));
        QVERIFY(standing.registered());
        ScreenAwake awake(QDBusConnection::sessionBus());

        awake.keepAwake(true, 0x2a00001);
        QTRY_VERIFY(awake.isHeld());
        QCOMPARE(portal.asked, QStringList{QStringLiteral("x11:2a00001 8 A game asked to keep the display on")});
        awake.keepAwake(true, 0x2a00001);
        QCOMPARE(portal.asked.size(), 1);
        awake.keepAwake(false, 0x2a00001);
        QVERIFY(!awake.isHeld());
        QTRY_COMPARE(portal.request.closed, 1);

        // Let go while the desktop was still answering: let go as it answers.
        awake.keepAwake(true, 0x2a00001);
        awake.keepAwake(false, 0x2a00001);
        QTRY_COMPARE(portal.request.closed, 2);
        QVERIFY(!awake.isHeld());
    }

    /// Where no portal answers, the freedesktop screen saver does; what is
    /// held goes with the holder.
    void without_a_portal_the_screen_saver_keeps_the_display_awake() {
        QVERIFY2(QDBusConnection::sessionBus().isConnected(),
                 "no session bus: run this test under dbus-run-session, as scripts/test-linux-qt-host-kit.sh does");
        FakeScreenSaver saver;
        OnTheBus standing(saver, QStringLiteral("org.freedesktop.ScreenSaver"),
                          QStringLiteral("/org/freedesktop/ScreenSaver"));
        QVERIFY(standing.registered());
        {
            ScreenAwake awake(QDBusConnection::sessionBus());
            awake.keepAwake(true, 1);
            QTRY_VERIFY(awake.isHeld());
            QCOMPARE(saver.asked.size(), 1);
            QVERIFY(saver.asked[0].endsWith(QStringLiteral(": A game asked to keep the display on")));
        }
        QTRY_COMPARE(saver.released, QList<uint>{7});
    }

    /// Where neither answers, nothing holds the display, and the host says why.
    void without_either_nothing_holds_the_display() {
        QVERIFY2(QDBusConnection::sessionBus().isConnected(),
                 "no session bus: run this test under dbus-run-session, as scripts/test-linux-qt-host-kit.sh does");
        QTest::ignoreMessage(QtWarningMsg,
                             QRegularExpression(QStringLiteral("neither the desktop portal nor the screen saver answers")));
        ScreenAwake awake(QDBusConnection::sessionBus());
        awake.keepAwake(true, 1);
        QTest::qWait(300);
        QVERIFY(!awake.isHeld());
    }

    /// The Managed session holds content's setKeepScreenOn for it, and lets go
    /// when the Session ends, whatever content last asked.
    void the_session_keeps_the_display_awake_while_content_asks() {
        QVERIFY2(QDBusConnection::sessionBus().isConnected(),
                 "no session bus: run this test under dbus-run-session, as scripts/test-linux-qt-host-kit.sh does");
        FakePortal portal;
        OnTheBus standing(portal, QStringLiteral("org.freedesktop.portal.Desktop"),
                          QStringLiteral("/org/freedesktop/portal/desktop"));
        QVERIFY(standing.registered());
        QWidget container;
        MigoManagedSession managed(*fake_migo::engine(), container);
        QVERIFY(fake_migo::installed_callbacks().on_keep_screen_on != nullptr);

        fake_migo::deliver_keep_screen_on_from_engine_thread(true);
        QTRY_COMPARE(portal.asked.size(), 1);
        QVERIFY(portal.asked[0].startsWith(QStringLiteral("x11:%1 ").arg(container.winId(), 0, 16)));
        QTest::qWait(100);
        QCOMPARE(portal.request.closed, 0);
        QCOMPARE(managed.close(), MIGO_OK);
        QTRY_COMPARE(portal.request.closed, 1);
    }

    /// The battery is UPower's display device -- its level, whether it charges
    /// -- with the power profile's saver as the low-power mode, reported each
    /// time what is reported changes.
    void the_battery_is_reported_as_upower_shows_it() {
        QVERIFY2(QDBusConnection::sessionBus().isConnected(),
                 "no session bus: run this test under dbus-run-session, as scripts/test-linux-qt-host-kit.sh does");
        const QString device = QStringLiteral("/org/freedesktop/UPower/devices/DisplayDevice");
        FakeBattery battery;
        OnTheBus upower(battery, QStringLiteral("org.freedesktop.UPower"), device);
        QVERIFY(upower.registered());
        std::vector<BatteryReport::State> reports;
        BatteryReport report(QDBusConnection::sessionBus(),
                             [&reports](const BatteryReport::State &state) { reports.push_back(state); });

        QTRY_COMPARE(reports.size(), std::size_t{1});
        QCOMPARE(reports[0].level_percent, 76U);
        QCOMPARE(reports[0].flags, MigoBatteryFlags{MIGO_BATTERY_FLAG_NONE});
        battery.state = 1;  // charging
        announce(device, QStringLiteral("org.freedesktop.UPower.Device"));
        QTRY_COMPARE(reports.size(), std::size_t{2});
        QCOMPARE(reports[1].flags, MigoBatteryFlags{MIGO_BATTERY_FLAG_CHARGING});

        FakePowerProfiles profiles;
        OnTheBus saver(profiles, QStringLiteral("org.freedesktop.UPower.PowerProfiles"),
                       QStringLiteral("/org/freedesktop/UPower/PowerProfiles"));
        QVERIFY(saver.registered());
        QTRY_COMPARE(reports.size(), std::size_t{3});
        QCOMPARE(reports[2].flags, MigoBatteryFlags{MIGO_BATTERY_FLAG_CHARGING | MIGO_BATTERY_FLAG_LOW_POWER_MODE});

        // A change below what the level is reported in is not reported again.
        battery.percentage = 76.2;
        announce(device, QStringLiteral("org.freedesktop.UPower.Device"));
        QTest::qWait(200);
        QCOMPARE(reports.size(), std::size_t{3});
    }

    /// A machine with no battery reports none: content is told its call is
    /// not supported, the answer for such a machine.
    void a_machine_without_a_battery_reports_none() {
        QVERIFY2(QDBusConnection::sessionBus().isConnected(),
                 "no session bus: run this test under dbus-run-session, as scripts/test-linux-qt-host-kit.sh does");
        FakeBattery none;
        none.type = 0;
        none.present = false;
        OnTheBus upower(none, QStringLiteral("org.freedesktop.UPower"),
                        QStringLiteral("/org/freedesktop/UPower/devices/DisplayDevice"));
        QVERIFY(upower.registered());
        std::vector<BatteryReport::State> reports;
        BatteryReport report(QDBusConnection::sessionBus(),
                             [&reports](const BatteryReport::State &state) { reports.push_back(state); });
        QTest::qWait(300);
        QVERIFY(reports.empty());
    }

    /// The network is NetworkManager's primary connection: Wi-Fi, and wired as
    /// Wi-Fi; a modem's generation unknown; whether it reaches the internet by
    /// NetworkManager's check, or by its state where the check is off; none
    /// while disconnected.
    void the_network_is_reported_as_networkmanager_shows_it() {
        QVERIFY2(QDBusConnection::sessionBus().isConnected(),
                 "no session bus: run this test under dbus-run-session, as scripts/test-linux-qt-host-kit.sh does");
        const QString path = QStringLiteral("/org/freedesktop/NetworkManager");
        const QString interface = QStringLiteral("org.freedesktop.NetworkManager");
        FakeNetworkManager manager;
        OnTheBus networkmanager(manager, interface, path);
        QVERIFY(networkmanager.registered());
        std::vector<NetworkReport::State> reports;
        NetworkReport report(QDBusConnection::sessionBus(),
                             [&reports](const NetworkReport::State &state) { reports.push_back(state); });
        const auto last = [&reports] { return reports.back(); };

        QTRY_COMPARE(reports.size(), std::size_t{1});
        QCOMPARE(last().type, MigoNetworkType{MIGO_NETWORK_WIFI});
        QVERIFY(last().connected);
        QVERIFY(last().wifi_enabled);
        manager.primary = QStringLiteral("802-3-ethernet");
        manager.radio = false;  // the radio's own switch: Wi-Fi is off
        manager.connectivity = 3;  // limited: no internet
        announce(path, interface);
        QTRY_COMPARE(reports.size(), std::size_t{2});
        QCOMPARE(last().type, MigoNetworkType{MIGO_NETWORK_WIFI});
        QVERIFY(!last().connected);
        QVERIFY(!last().wifi_enabled);
        manager.connectivity = 0;  // not checked: the state says
        announce(path, interface);
        QTRY_COMPARE(reports.size(), std::size_t{3});
        QVERIFY(last().connected);
        manager.primary = QStringLiteral("gsm");
        announce(path, interface);
        QTRY_COMPARE(reports.size(), std::size_t{4});
        QCOMPARE(last().type, MigoNetworkType{MIGO_NETWORK_UNKNOWN});
        manager.state = 20;  // disconnected
        manager.primary.clear();
        announce(path, interface);
        QTRY_COMPARE(reports.size(), std::size_t{5});
        QCOMPARE(last().type, MigoNetworkType{MIGO_NETWORK_NONE});
        QVERIFY(!last().connected);
    }

    /// Content runs while its window is shown and not minimized, and is told
    /// so when that changes: it starts shown and running, so nothing is said
    /// until then.
    void content_runs_and_shows_with_its_window() {
        if (!require_xcb("xcb-only: the view attaches only on X11", __FILE__, __LINE__)) return;
        QWidget container;
        MigoManagedSession managed(*fake_migo::engine(), container);
        const ReleasedOnExit released{managed};
        auto *layout = new QVBoxLayout(&container);
        layout->setContentsMargins(0, 0, 0, 0);
        layout->addWidget(managed.view());
        container.resize(320, 200);
        container.show();
        QTRY_COMPARE(fake_migo::calls().attach, 1);
        QTest::qWait(100);
        using Said = std::vector<std::string>;
        QCOMPARE(fake_migo::window_reports(), Said{});

        container.showMinimized();
        QTRY_COMPARE(fake_migo::window_reports(), (Said{"hidden", "paused"}));
        container.showNormal();
        QTRY_COMPARE(fake_migo::window_reports(), (Said{"hidden", "paused", "shown", "running"}));
    }

    /// An App outside a sandbox is granted what the system could gate, and the
    /// Session is told so; location is GeoClue's agent's, and not determined.
    void the_app_is_granted_what_the_system_could_gate() {
        QWidget container;
        MigoManagedSession managed(*fake_migo::engine(), container);
        const MigoAppAuthorizeSetting &granted = fake_migo::reported_app_authorization();
        QCOMPARE(granted.struct_size, std::uint32_t{sizeof(MigoAppAuthorizeSetting)});
        for (const std::uint8_t field : {granted.album, granted.bluetooth, granted.camera, granted.microphone,
                                         granted.notification, granted.phone_calendar}) {
            QCOMPARE(field, std::uint8_t{MIGO_AUTHORIZATION_AUTHORIZED});
        }
        QCOMPARE(granted.location, std::uint8_t{MIGO_AUTHORIZATION_NOT_DETERMINED});
        QCOMPARE(managed.close(), MIGO_OK);
    }

    /// A Session created but left without callbacks must not be stranded.
    void a_refused_callback_install_destroys_the_session_it_created() {
        fake_migo::set_set_callbacks_result(MIGO_ERROR_INVALID_STATE);
        QWidget container;
        MigoManagedSession managed(*fake_migo::engine(), container);

        QVERIFY(!managed.isValid());
        QCOMPARE(managed.lastError(), MIGO_ERROR_INVALID_STATE);
        QCOMPARE(fake_migo::calls().session_create, 1);
        // The Session it made is gone: leaving it alive would leak a handle no
        // caller can reach, because the wrapper reports itself invalid.
        QCOMPARE(fake_migo::calls().session_destroy, 1);
        QVERIFY(!fake_migo::session_is_alive());
    }

    void a_refused_session_create_yields_an_invalid_wrapper() {
        fake_migo::set_session_create_result(MIGO_ERROR_INVALID_ARGUMENT);
        QWidget container;
        MigoManagedSession managed(*fake_migo::engine(), container);

        QVERIFY(!managed.isValid());
        QCOMPARE(managed.lastError(), MIGO_ERROR_INVALID_ARGUMENT);
        QCOMPARE(fake_migo::calls().set_callbacks, 0);
        QCOMPARE(fake_migo::calls().session_destroy, 0);
    }

    /// An engine callback must arrive on the GUI thread, not on the engine's.
    ///
    /// The dispatcher is the whole reason a host can touch Qt from a callback.
    /// A test that invoked the callback directly would prove nothing about it,
    /// so the fake hands it to the dispatcher from a real other thread.
    void engine_callbacks_are_marshalled_onto_the_gui_thread() {
        QWidget container;
        MigoManagedSession managed(*fake_migo::engine(), container);
        QVERIFY(managed.isValid());

        QThread *observed = nullptr;
        connect(&managed, &MigoManagedSession::contentReady, this,
                [&observed] { observed = QThread::currentThread(); });

        fake_migo::deliver_ready_from_engine_thread();
        QTRY_VERIFY(observed != nullptr);
        QCOMPARE(observed, QThread::currentThread());

        QCOMPARE(managed.close(), MIGO_OK);
    }

    /// An error raised after content starts carries its code and message.
    void a_runtime_error_arrives_with_its_message() {
        QWidget container;
        MigoManagedSession managed(*fake_migo::engine(), container);
        QVERIFY(managed.isValid());
        QSignalSpy failures(&managed, &MigoManagedSession::contentFailed);

        fake_migo::deliver_error_from_engine_thread(MIGO_ERROR_INTERNAL, "content blew up");
        QTRY_COMPARE(failures.count(), 1);
        QCOMPARE(failures.at(0).at(0).value<MigoResult>(), MIGO_ERROR_INTERNAL);
        QCOMPARE(failures.at(0).at(1).toString(), QStringLiteral("content blew up"));

        QCOMPARE(managed.close(), MIGO_OK);
    }

    /// Loading reports argument failures synchronously, not through the signal.
    /// Content waits for the view: the Session has nothing to run it on until
    /// the view first attaches, which is after Qt has put the window on screen.
    /// Asked for before then, it is loaded then; asked for twice, it is refused.
    void content_waits_for_the_view_to_attach() {
        QWidget container;
        MigoManagedSession managed(*fake_migo::engine(), container);
        QVERIFY(managed.isValid());

        QCOMPARE(managed.loadContent(QStringLiteral("game"), QStringLiteral("game.js")), MIGO_OK);
        QCOMPARE(fake_migo::calls().load_content, 0);
        QCOMPARE(managed.loadContent(QStringLiteral("game"), QStringLiteral("game.js")),
                 MIGO_ERROR_INVALID_STATE);

        if (require_xcb("xcb-only: the view attaches only on X11", __FILE__, __LINE__)) {
            container.show();
            QTRY_COMPARE(fake_migo::calls().attach, 1);
            QCOMPARE(fake_migo::calls().load_content, 1);
            QCOMPARE(fake_migo::loaded_content_id(), std::string("game"));
            QCOMPARE(managed.close(), MIGO_OK);
            fake_migo::set_release_status(managed.view()->generation(), MIGO_SURFACE_RELEASE_RELEASED);
            QTRY_VERIFY(managed.isClosed());
        } else {
            QCOMPARE(managed.close(), MIGO_OK);
        }
    }

    /// Once the view has attached, a load is the library's answer, now; one the
    /// library refuses when the view attaches arrives as `contentFailed`.
    void a_load_is_answered_now_once_attached_and_later_before() {
        if (!require_xcb("xcb-only test", __FILE__, __LINE__)) return;
        {
            QWidget container;
            MigoManagedSession managed(*fake_migo::engine(), container);
            QSignalSpy failures(&managed, &MigoManagedSession::contentFailed);
            container.show();
            QTRY_COMPARE(fake_migo::calls().attach, 1);

            fake_migo::set_load_content_result(MIGO_ERROR_INVALID_ARGUMENT);
            QCOMPARE(managed.loadContent(QStringLiteral("game"), QStringLiteral("game.js")),
                     MIGO_ERROR_INVALID_ARGUMENT);
            QCOMPARE(failures.count(), 0);
            // Refused, it was not loaded: it may be asked for again.
            fake_migo::set_load_content_result(MIGO_OK);
            QCOMPARE(managed.loadContent(QStringLiteral("game"), QStringLiteral("game.js")), MIGO_OK);
            QCOMPARE(fake_migo::loaded_content_id(), std::string("game"));

            QCOMPARE(managed.close(), MIGO_OK);
            fake_migo::set_release_status(managed.view()->generation(), MIGO_SURFACE_RELEASE_RELEASED);
            QTRY_VERIFY(managed.isClosed());
        }
        fake_migo::reset();
        {
            QWidget container;
            MigoManagedSession managed(*fake_migo::engine(), container);
            QSignalSpy failures(&managed, &MigoManagedSession::contentFailed);
            fake_migo::set_load_content_result(MIGO_ERROR_INVALID_ARGUMENT);
            QCOMPARE(managed.loadContent(QStringLiteral("game"), QStringLiteral("game.js")), MIGO_OK);
            container.show();
            QTRY_COMPARE(failures.count(), 1);
            QCOMPARE(failures.at(0).at(0).value<MigoResult>(), MIGO_ERROR_INVALID_ARGUMENT);

            QCOMPARE(managed.close(), MIGO_OK);
            fake_migo::set_release_status(managed.view()->generation(), MIGO_SURFACE_RELEASE_RELEASED);
            QTRY_VERIFY(managed.isClosed());
        }
    }

    /// Once teardown starts the dispatcher must refuse, not queue.
    ///
    /// Accepting a task here would run it against a Session that is on its way
    /// out. Refusing hands ownership back to Migo, which is the ABI's own rule
    /// for a rejection -- dropping it silently would instead leak whatever the
    /// engine attached to the task.
    void the_dispatcher_refuses_tasks_once_close_has_started() {
        QWidget container;
        MigoManagedSession managed(*fake_migo::engine(), container);
        QVERIFY(managed.isValid());
        QSignalSpy ready(&managed, &MigoManagedSession::contentReady);

        QCOMPARE(managed.close(), MIGO_OK);
        QVERIFY(managed.isClosed());

        fake_migo::deliver_ready_from_engine_thread();
        QTest::qWait(30);
        QCOMPARE(ready.count(), 0);
    }

    void close_is_idempotent_and_reports_completion_once() {
        QWidget container;
        MigoManagedSession managed(*fake_migo::engine(), container);
        QVERIFY(managed.isValid());
        QSignalSpy closed(&managed, &MigoManagedSession::sessionClosed);

        QCOMPARE(managed.close(), MIGO_OK);
        QCOMPARE(managed.close(), MIGO_OK);
        QCOMPARE(managed.close(), MIGO_OK);
        QCOMPARE(closed.count(), 1);
        QCOMPARE(fake_migo::calls().session_destroy, 1);
    }

    /// A frame the engine asks for must reach Qt's clock and come back.
    void a_frame_request_from_the_engine_reaches_the_toolkit_clock() {
        if (!require_xcb("xcb-only test", __FILE__, __LINE__)) return;
        QWidget container;
        MigoManagedSession managed(*fake_migo::engine(), container);
        QVERIFY(managed.isValid());
        managed.view()->resize(320, 180);
        container.show();
        managed.view()->show();
        QTRY_COMPARE(fake_migo::calls().attach, 1);

        fake_migo::deliver_frame_request_from_engine_thread();
        QTRY_COMPARE(fake_migo::calls().vsync, 1);

        fake_migo::set_release_status(managed.view()->generation(),
                                      MIGO_SURFACE_RELEASE_RELEASED);
        QCOMPARE(managed.close(), MIGO_OK);
        QTRY_VERIFY(managed.isClosed());
    }

    /// The Session must outlive the Surface it is attached to.
    ///
    /// `migo_session_destroy` refuses while an attachment is live or a release
    /// is pending, so destroying early is not merely impolite -- it fails, and
    /// a wrapper that ignored the failure would leak the Session while telling
    /// the App it had closed.
    void closing_destroys_the_session_only_after_the_surface_is_released() {
        if (!require_xcb("xcb-only test", __FILE__, __LINE__)) return;
        QWidget container;
        MigoManagedSession managed(*fake_migo::engine(), container);
        QVERIFY(managed.isValid());
        managed.view()->resize(320, 180);
        container.show();
        managed.view()->show();
        QTRY_COMPARE(fake_migo::calls().attach, 1);

        // Hold the release pending: the Session must not be destroyed yet.
        fake_migo::set_release_status(managed.view()->generation(), MIGO_SURFACE_RELEASE_PENDING);
        QCOMPARE(managed.close(), MIGO_OK);
        QTest::qWait(40);
        QCOMPARE(fake_migo::calls().session_destroy, 0);
        QVERIFY(fake_migo::session_is_alive());
        QVERIFY(!managed.isClosed());

        fake_migo::set_release_status(managed.view()->generation(),
                                      MIGO_SURFACE_RELEASE_RELEASED);
        QTRY_VERIFY(managed.isClosed());
        QCOMPARE(fake_migo::calls().session_destroy, 1);
        QVERIFY(!fake_migo::session_is_alive());
    }
};

QTEST_MAIN(QtManagedSessionTest)
#include "qt_managed_session_test.moc"

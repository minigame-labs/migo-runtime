#include "fake_migo.hpp"
#include "gamepads.hpp"
#include "memory_monitor.hpp"
#include "screen_awake.hpp"
#include "system_reports.hpp"

#include <migo/linux/qt6/managed_session.hpp>

#include <QApplication>
#include <QDBusConnection>
#include <QDBusContext>
#include <QDBusMessage>
#include <QDeadlineTimer>
#include <QDBusObjectPath>
#include <QJsonArray>
#include <QJsonDocument>
#include <QJsonObject>
#include <QLineEdit>
#include <QPlainTextEdit>
#include <QVariantMap>
#include <QVBoxLayout>
#include <QSignalSpy>
#include <QTemporaryDir>
#include <QThread>
#include <QtTest>

#include <linux/input.h>
#include <linux/rfkill.h>

#include <fcntl.h>
#include <sys/stat.h>
#include <unistd.h>

#include <memory>
#include <optional>
#include <string>
#include <utility>
#include <vector>

using migo::linux_host::SurfaceState;
using migo::linux_host::qt6::MigoManagedSession;
using migo::linux_host::qt6::BatteryReport;
using migo::linux_host::qt6::Gamepads;
using migo::linux_host::qt6::MemoryMonitor;
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

/// systemd-networkd's manager: its state, and its account of its links.
class FakeNetworkd final : public QObject, protected QDBusContext {
    Q_OBJECT
    Q_CLASSINFO("D-Bus Interface", "org.freedesktop.network1.Manager")
    Q_PROPERTY(QString OperationalState MEMBER operational)

public:
    QString operational = QStringLiteral("routable");
    /// Describe's answer; none is a networkd before 248, which has no Describe.
    std::optional<QString> described;

public slots:
    QString Describe() {
        if (!described.has_value()) {
            sendErrorReply(QDBusError::UnknownMethod, QStringLiteral("no Describe before systemd 248"));
            return {};
        }
        return *described;
    }
};

/// What networkd's Describe says of links that each have a default route in
/// the main table, as (Type, metric); a negative metric is a link without one.
QString networkdLinks(const std::vector<std::pair<QString, int>> &links) {
    QJsonArray interfaces;
    int index = 1;
    for (const auto &[type, metric] : links) {
        QJsonArray routes;
        if (metric >= 0) {
            routes.append(QJsonObject{{QStringLiteral("Family"), 2},
                                      {QStringLiteral("Destination"), QJsonArray{0, 0, 0, 0}},
                                      {QStringLiteral("DestinationPrefixLength"), 0},
                                      {QStringLiteral("Priority"), metric},
                                      {QStringLiteral("Table"), 254}});
        }
        // A route of the same link to its own subnet is not the way out.
        routes.append(QJsonObject{{QStringLiteral("Family"), 2},
                                  {QStringLiteral("Destination"), QJsonArray{10, 0, index, 0}},
                                  {QStringLiteral("DestinationPrefixLength"), 24},
                                  {QStringLiteral("Priority"), 0},
                                  {QStringLiteral("Table"), 254}});
        interfaces.append(QJsonObject{{QStringLiteral("Index"), index},
                                      {QStringLiteral("Name"), QStringLiteral("link%1").arg(index)},
                                      {QStringLiteral("Type"), type},
                                      {QStringLiteral("Routes"), routes}});
        ++index;
    }
    return QString::fromUtf8(QJsonDocument(QJsonObject{{QStringLiteral("Interfaces"), interfaces}}).toJson());
}

/// The kernel's rfkill device, played through a FIFO: what it says of a radio
/// as `rfkill_event`s, in the size a reader that asks for them gets.
class FakeRfkill {
public:
    FakeRfkill() : path_(dir_.filePath(QStringLiteral("rfkill"))) {
        valid_ = ::mkfifo(path_.toLocal8Bit().constData(), 0600) == 0;
        // Held open for writing, so the reader never sees the end of it.
        if (valid_) writer_ = ::open(path_.toLocal8Bit().constData(), O_RDWR | O_CLOEXEC);
        valid_ = valid_ && writer_ >= 0;
    }
    ~FakeRfkill() {
        if (writer_ >= 0) ::close(writer_);
    }
    [[nodiscard]] bool valid() const { return valid_; }
    [[nodiscard]] QString path() const { return path_; }
    void say(std::uint32_t radio, std::uint8_t op, std::uint8_t type, bool soft, bool hard) const {
        rfkill_event event{};
        event.idx = radio;
        event.op = op;
        event.type = type;
        event.soft = soft ? 1 : 0;
        event.hard = hard ? 1 : 0;
        QCOMPARE(::write(writer_, &event, sizeof(event)), static_cast<ssize_t>(sizeof(event)));
    }

private:
    QTemporaryDir dir_;
    QString path_;
    int writer_ = -1;
    bool valid_ = false;
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

/// What content asked for when it opened the keyboard.
MigoKeyboardShowOptions keyboardOptions(MigoKeyboardFlags flags = MIGO_KEYBOARD_FLAG_NONE, std::uint32_t max_length = 0,
                                        MigoKeyboardType type = MIGO_KEYBOARD_TYPE_TEXT,
                                        MigoKeyboardConfirmType confirm = MIGO_KEYBOARD_CONFIRM_DONE) {
    MigoKeyboardShowOptions options{};
    options.struct_size = static_cast<std::uint32_t>(sizeof(options));
    options.abi_version = MIGO_ABI_VERSION_1;
    options.flags = flags;
    options.max_length = max_length;
    options.confirm_type = confirm;
    options.keyboard_type = type;
    return options;
}

/// The keyboard's field, over the game's window: null once it is put away.
QWidget *keyboardField(QWidget &container) {
    return container.findChild<QWidget *>(QStringLiteral("migoKeyboardField"), Qt::FindDirectChildrenOnly);
}

/// What the Session was told of the keyboard, from the `from`th event: each event and its text.
std::vector<std::pair<MigoKeyboardEventType, std::string>> said(std::size_t from = 0) {
    std::vector<std::pair<MigoKeyboardEventType, std::string>> told;
    const std::vector<fake_migo::KeyboardRecord> &events = fake_migo::keyboard_events();
    for (std::size_t index = from; index < events.size(); ++index) {
        told.emplace_back(events[index].event_type, events[index].value);
    }
    return told;
}

/// The game on screen, its view attached.
void showGame(QWidget &container, MigoManagedSession &managed) {
    auto *layout = new QVBoxLayout(&container);
    layout->setContentsMargins(0, 0, 0, 0);
    layout->addWidget(managed.view());
    container.resize(320, 200);
    container.show();
    QTRY_COMPARE(fake_migo::calls().attach, 1);
}

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

    /// The soft keyboard is claimed whole: the three callbacks install together.
    void the_soft_keyboard_is_claimed_whole() {
        QWidget container;
        MigoManagedSession managed(*fake_migo::engine(), container);
        QVERIFY(managed.isValid());

        const MigoHostCallbacks &installed = fake_migo::installed_callbacks();
        // Compared as booleans: QCOMPARE cannot take a function pointer, and
        // the assertion is about presence, not identity.
        QVERIFY(installed.on_show_keyboard != nullptr);
        QVERIFY(installed.on_hide_keyboard != nullptr);
        QVERIFY(installed.on_update_keyboard != nullptr);

        QCOMPARE(managed.close(), MIGO_OK);
    }

    /// Content's showKeyboard opens a field along the bottom of the game, the
    /// height content is told; each edit reports the whole text, content's own
    /// correction is not reported back, and Enter confirms and closes it,
    /// telling content the height 0 and the text it was left with.
    void content_types_into_a_field_along_the_bottom_of_the_game() {
        if (!require_xcb("xcb-only: the field takes the keyboard's focus", __FILE__, __LINE__)) return;
        QWidget container;
        MigoManagedSession managed(*fake_migo::engine(), container);
        const ReleasedOnExit released{managed};
        showGame(container, managed);

        fake_migo::deliver_show_keyboard_from_engine_thread(keyboardOptions(), "ab");
        QWidget *field = keyboardField(container);
        QVERIFY(field != nullptr);
        QTRY_VERIFY(field->isVisible());
        auto *line = field->findChild<QLineEdit *>();
        QVERIFY(line != nullptr && line->isVisible());
        QCOMPARE(line->text(), QStringLiteral("ab"));
        QTRY_VERIFY(line->hasFocus());
        const QRect game(managed.view()->mapToGlobal(QPoint(0, 0)), managed.view()->size());
        QCOMPARE(field->geometry().left(), game.left());
        QCOMPARE(field->geometry().width(), game.width());
        QCOMPARE(field->geometry().bottom(), game.bottom());
        using Said = std::vector<std::pair<MigoKeyboardEventType, std::string>>;
        QCOMPARE(said(), (Said{{MIGO_KEYBOARD_EVENT_HEIGHT_CHANGE, ""}}));
        QVERIFY(field->height() > 0);
        QCOMPARE(fake_migo::keyboard_events()[0].height_css_px, static_cast<double>(field->height()));

        QTest::keyClicks(line, QStringLiteral("cd"));
        fake_migo::deliver_update_keyboard_from_engine_thread("xyz");
        QTRY_COMPARE(line->text(), QStringLiteral("xyz"));
        QTest::keyClick(line, Qt::Key_Return);
        QCOMPARE(said(), (Said{{MIGO_KEYBOARD_EVENT_HEIGHT_CHANGE, ""},
                               {MIGO_KEYBOARD_EVENT_INPUT, "abc"},
                               {MIGO_KEYBOARD_EVENT_INPUT, "abcd"},
                               {MIGO_KEYBOARD_EVENT_CONFIRM, "xyz"},
                               {MIGO_KEYBOARD_EVENT_HEIGHT_CHANGE, ""},
                               {MIGO_KEYBOARD_EVENT_COMPLETE, "xyz"}}));
        QCOMPARE(fake_migo::keyboard_events()[4].height_css_px, 0.0);
        QVERIFY(!field->isVisible());
    }

    /// A confirm content asked to hold leaves the field up; content's
    /// hideKeyboard closes it, as the player's going would.
    void a_held_confirm_keeps_the_field_until_content_hides_it() {
        if (!require_xcb("xcb-only: the field takes the keyboard's focus", __FILE__, __LINE__)) return;
        QWidget container;
        MigoManagedSession managed(*fake_migo::engine(), container);
        const ReleasedOnExit released{managed};
        showGame(container, managed);

        fake_migo::deliver_show_keyboard_from_engine_thread(keyboardOptions(MIGO_KEYBOARD_FLAG_CONFIRM_HOLD), "go");
        QWidget *field = keyboardField(container);
        QTRY_VERIFY(field->isVisible());
        auto *line = field->findChild<QLineEdit *>();
        QTest::keyClick(line, Qt::Key_Return);
        QVERIFY(field->isVisible());
        fake_migo::deliver_hide_keyboard_from_engine_thread();
        QTRY_VERIFY(!field->isVisible());
        using Said = std::vector<std::pair<MigoKeyboardEventType, std::string>>;
        QCOMPARE(said(), (Said{{MIGO_KEYBOARD_EVENT_HEIGHT_CHANGE, ""},
                               {MIGO_KEYBOARD_EVENT_CONFIRM, "go"},
                               {MIGO_KEYBOARD_EVENT_HEIGHT_CHANGE, ""},
                               {MIGO_KEYBOARD_EVENT_COMPLETE, "go"}}));
        // A keyboard that is not up is not hidden again.
        fake_migo::deliver_hide_keyboard_from_engine_thread();
        QTest::qWait(50);
        QCOMPARE(fake_migo::keyboard_events().size(), std::size_t{4});
    }

    /// What content asked of the keyboard: a length the text keeps to, digits
    /// for a number keyboard -- and an input method told so -- the confirm key
    /// it named, and several lines, in which Enter is a new line.
    void the_field_is_the_keyboard_content_asked_for() {
        if (!require_xcb("xcb-only: the field takes the keyboard's focus", __FILE__, __LINE__)) return;
        QWidget container;
        MigoManagedSession managed(*fake_migo::engine(), container);
        const ReleasedOnExit released{managed};
        showGame(container, managed);
        QWidget *field = nullptr;

        fake_migo::deliver_show_keyboard_from_engine_thread(keyboardOptions(MIGO_KEYBOARD_FLAG_NONE, 3), "");
        field = keyboardField(container);
        QTRY_VERIFY(field->isVisible());
        auto *line = field->findChild<QLineEdit *>();
        QTest::keyClicks(line, QStringLiteral("abcdef"));
        QCOMPARE(line->text(), QStringLiteral("abc"));
        fake_migo::deliver_hide_keyboard_from_engine_thread();
        QTRY_VERIFY(!field->isVisible());

        fake_migo::deliver_show_keyboard_from_engine_thread(
            keyboardOptions(MIGO_KEYBOARD_FLAG_NONE, 0, MIGO_KEYBOARD_TYPE_NUMBER, MIGO_KEYBOARD_CONFIRM_SEND), "");
        QTRY_VERIFY(field->isVisible());
        QTest::keyClicks(line, QStringLiteral("1a2"));
        QCOMPARE(line->text(), QStringLiteral("12"));
        QVERIFY(line->inputMethodHints().testFlag(Qt::ImhDigitsOnly));
        QCOMPARE(line->inputMethodQuery(Qt::ImEnterKeyType).toInt(), static_cast<int>(Qt::EnterKeySend));
        fake_migo::deliver_hide_keyboard_from_engine_thread();
        QTRY_VERIFY(!field->isVisible());

        // Text again: the digits and the confirm key were that request's.
        fake_migo::deliver_show_keyboard_from_engine_thread(keyboardOptions(), "");
        QTRY_VERIFY(field->isVisible());
        QTest::keyClicks(line, QStringLiteral("x1"));
        QCOMPARE(line->text(), QStringLiteral("x1"));
        QCOMPARE(line->inputMethodQuery(Qt::ImEnterKeyType).toInt(), static_cast<int>(Qt::EnterKeyDone));
        fake_migo::deliver_hide_keyboard_from_engine_thread();
        QTRY_VERIFY(!field->isVisible());
        const int one_line = field->height();

        const std::size_t before = fake_migo::keyboard_events().size();
        fake_migo::deliver_show_keyboard_from_engine_thread(keyboardOptions(MIGO_KEYBOARD_FLAG_MULTIPLE, 4), "");
        QTRY_VERIFY(field->isVisible());
        auto *lines = field->findChild<QPlainTextEdit *>();
        QVERIFY(lines != nullptr && lines->isVisible() && !line->isVisible());
        QVERIFY(field->height() > one_line);
        QTest::keyClicks(lines, QStringLiteral("a"));
        QTest::keyClick(lines, Qt::Key_Return);
        QTest::keyClicks(lines, QStringLiteral("bcd"));
        QCOMPARE(lines->toPlainText(), QStringLiteral("a\nbc"));
        using Said = std::vector<std::pair<MigoKeyboardEventType, std::string>>;
        QCOMPARE(said(before), (Said{{MIGO_KEYBOARD_EVENT_HEIGHT_CHANGE, ""},
                               {MIGO_KEYBOARD_EVENT_INPUT, "a"},
                               {MIGO_KEYBOARD_EVENT_INPUT, "a\n"},
                               {MIGO_KEYBOARD_EVENT_INPUT, "a\nb"},
                               {MIGO_KEYBOARD_EVENT_INPUT, "a\nbc"},
                               {MIGO_KEYBOARD_EVENT_INPUT, "a\nbc"}}));
    }

    /// The player closes the field with Esc, or by turning to the game:
    /// content is told the keyboard went, with the text it was left with.
    void escape_or_turning_away_closes_the_field() {
        if (!require_xcb("xcb-only: the field takes the keyboard's focus", __FILE__, __LINE__)) return;
        QWidget container;
        MigoManagedSession managed(*fake_migo::engine(), container);
        const ReleasedOnExit released{managed};
        showGame(container, managed);

        fake_migo::deliver_show_keyboard_from_engine_thread(keyboardOptions(), "esc");
        QWidget *field = keyboardField(container);
        QTRY_VERIFY(field->isVisible());
        auto *line = field->findChild<QLineEdit *>();
        QTRY_VERIFY(line->hasFocus());
        QTest::keyClick(line, Qt::Key_Escape);
        QVERIFY(!field->isVisible());
        // The keys are the game's again.
        QTRY_VERIFY(managed.view()->hasFocus());

        fake_migo::deliver_show_keyboard_from_engine_thread(keyboardOptions(), "away");
        QTRY_VERIFY(field->isVisible());
        QTRY_VERIFY(line->hasFocus());
        container.activateWindow();
        QTRY_VERIFY(!field->isVisible());
        using Said = std::vector<std::pair<MigoKeyboardEventType, std::string>>;
        QCOMPARE(said(), (Said{{MIGO_KEYBOARD_EVENT_HEIGHT_CHANGE, ""},
                               {MIGO_KEYBOARD_EVENT_HEIGHT_CHANGE, ""},
                               {MIGO_KEYBOARD_EVENT_COMPLETE, "esc"},
                               {MIGO_KEYBOARD_EVENT_HEIGHT_CHANGE, ""},
                               {MIGO_KEYBOARD_EVENT_HEIGHT_CHANGE, ""},
                               {MIGO_KEYBOARD_EVENT_COMPLETE, "away"}}));
    }

    /// A Session that closes takes the field with it, telling content nothing:
    /// it is going.
    void closing_puts_the_field_away_unreported() {
        if (!require_xcb("xcb-only: the field takes the keyboard's focus", __FILE__, __LINE__)) return;
        QWidget container;
        MigoManagedSession managed(*fake_migo::engine(), container);
        showGame(container, managed);
        fake_migo::deliver_show_keyboard_from_engine_thread(keyboardOptions(), "left");
        QTRY_VERIFY(keyboardField(container) != nullptr && keyboardField(container)->isVisible());

        QCOMPARE(managed.close(), MIGO_OK);
        QVERIFY(keyboardField(container) == nullptr);
        fake_migo::set_release_status(managed.view()->generation(), MIGO_SURFACE_RELEASE_RELEASED);
        QTRY_VERIFY(managed.isClosed());
        QCOMPARE(fake_migo::keyboard_events().size(), std::size_t{1});
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
        NetworkReport report(
            QDBusConnection::sessionBus(),
            [&reports](const NetworkReport::State &state) { reports.push_back(state); }, nullptr,
            QStringLiteral("/nonexistent/rfkill"));
        const auto last = [&reports] { return reports.back(); };

        QTRY_COMPARE(reports.size(), std::size_t{1});
        QCOMPARE(last().type, MigoNetworkType{MIGO_NETWORK_WIFI});
        QVERIFY(last().connected);
        QVERIFY(last().wifi_enabled == std::optional<bool>(true));
        manager.primary = QStringLiteral("802-3-ethernet");
        manager.radio = false;  // the radio's own switch: Wi-Fi is off
        manager.connectivity = 3;  // limited: no internet
        announce(path, interface);
        QTRY_COMPARE(reports.size(), std::size_t{2});
        QCOMPARE(last().type, MigoNetworkType{MIGO_NETWORK_WIFI});
        QVERIFY(!last().connected);
        QVERIFY(last().wifi_enabled == std::optional<bool>(false));
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

    /// Without NetworkManager the network is systemd-networkd's: the kind of
    /// the link with the preferred default route, wired as Wi-Fi and a modem
    /// unknown; connected while networkd says the machine is routable; none
    /// once no link routes anywhere. Wi-Fi is on by the kernel's rfkill switch.
    void the_network_is_networkd_s_where_networkmanager_does_not_run() {
        QVERIFY2(QDBusConnection::sessionBus().isConnected(),
                 "no session bus: run this test under dbus-run-session, as scripts/test-linux-qt-host-kit.sh does");
        const QString path = QStringLiteral("/org/freedesktop/network1");
        const QString interface = QStringLiteral("org.freedesktop.network1.Manager");
        FakeNetworkd networkd;
        // Ethernet's metric is lower, so it is the way out over the modem.
        networkd.described = networkdLinks({{QStringLiteral("wwan"), 700}, {QStringLiteral("ether"), 100}});
        OnTheBus on_the_bus(networkd, QStringLiteral("org.freedesktop.network1"), path);
        QVERIFY(on_the_bus.registered());
        FakeRfkill rfkill;
        QVERIFY(rfkill.valid());
        rfkill.say(0, RFKILL_OP_ADD, RFKILL_TYPE_BLUETOOTH, false, false);
        rfkill.say(1, RFKILL_OP_ADD, RFKILL_TYPE_WLAN, false, false);
        std::vector<NetworkReport::State> reports;
        NetworkReport report(
            QDBusConnection::sessionBus(), [&reports](const NetworkReport::State &state) { reports.push_back(state); },
            nullptr, rfkill.path());
        const auto last = [&reports] { return reports.back(); };

        QTRY_VERIFY(!reports.empty() && last().type == MIGO_NETWORK_WIFI);
        QVERIFY(last().connected);
        QVERIFY(last().wifi_enabled == std::optional<bool>(true));

        const std::size_t before_switch = reports.size();
        rfkill.say(1, RFKILL_OP_CHANGE, RFKILL_TYPE_WLAN, true, false);  // switched off
        QTRY_COMPARE(reports.size(), before_switch + 1);
        QVERIFY(last().wifi_enabled == std::optional<bool>(false));
        rfkill.say(1, RFKILL_OP_CHANGE, RFKILL_TYPE_WLAN, false, true);  // the radio's own switch
        QTest::qWait(100);
        QCOMPARE(reports.size(), before_switch + 1);  // still off: nothing new to say
        rfkill.say(1, RFKILL_OP_CHANGE, RFKILL_TYPE_WLAN, false, false);
        QTRY_COMPARE(reports.size(), before_switch + 2);
        QVERIFY(last().wifi_enabled == std::optional<bool>(true));

        // The modem now has the lower metric: the internet goes out through it.
        networkd.described = networkdLinks({{QStringLiteral("wwan"), 50}, {QStringLiteral("ether"), 100}});
        announce(path, interface);
        QTRY_COMPARE(last().type, MigoNetworkType{MIGO_NETWORK_UNKNOWN});
        QVERIFY(last().connected);

        // An address on the link but none the internet answers: not connected.
        networkd.operational = QStringLiteral("degraded");
        announce(path, interface);
        QTRY_VERIFY(!last().connected);
        QCOMPARE(last().type, MigoNetworkType{MIGO_NETWORK_UNKNOWN});

        // No link routes anywhere: no network.
        networkd.operational = QStringLiteral("carrier");
        networkd.described = networkdLinks({{QStringLiteral("wlan"), -1}, {QStringLiteral("ether"), -1}});
        announce(path, interface);
        QTRY_COMPARE(last().type, MigoNetworkType{MIGO_NETWORK_NONE});
        QVERIFY(!last().connected);

        // A networkd before Describe: routable, through a link it cannot name.
        networkd.operational = QStringLiteral("routable");
        networkd.described.reset();
        announce(path, interface);
        QTRY_COMPARE(last().type, MigoNetworkType{MIGO_NETWORK_UNKNOWN});
        QVERIFY(last().connected);
    }

    /// Where NetworkManager runs it is the one that says, beside networkd; and
    /// without an rfkill device to read nothing is said of the Wi-Fi switch,
    /// rather than that it is off.
    void networkmanager_comes_first_and_an_unknown_switch_is_not_off() {
        const QString networkd_path = QStringLiteral("/org/freedesktop/network1");
        FakeNetworkd networkd;
        networkd.described = networkdLinks({{QStringLiteral("wwan"), 100}});
        OnTheBus networkd_on_the_bus(networkd, QStringLiteral("org.freedesktop.network1"), networkd_path);
        QVERIFY(networkd_on_the_bus.registered());
        std::vector<NetworkReport::State> reports;
        NetworkReport report(
            QDBusConnection::sessionBus(), [&reports](const NetworkReport::State &state) { reports.push_back(state); },
            nullptr, QStringLiteral("/nonexistent/rfkill"));
        QTRY_VERIFY(!reports.empty() && reports.back().type == MIGO_NETWORK_UNKNOWN);
        QVERIFY(!reports.back().wifi_enabled.has_value());

        FakeNetworkManager manager;  // Wi-Fi, connected, switched on
        OnTheBus networkmanager(manager, QStringLiteral("org.freedesktop.NetworkManager"),
                                QStringLiteral("/org/freedesktop/NetworkManager"));
        QVERIFY(networkmanager.registered());
        QTRY_COMPARE(reports.back().type, MigoNetworkType{MIGO_NETWORK_WIFI});
        QVERIFY(reports.back().wifi_enabled == std::optional<bool>(true));
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

    /// A desktop's memory warnings are heard from low-memory-monitor, or inside
    /// a sandbox from the portal that relays them, each at its level: low,
    /// medium and critical; a level below low is no warning.
    void the_desktops_memory_warnings_are_heard_at_their_levels() {
        QVERIFY2(QDBusConnection::sessionBus().isConnected(),
                 "no session bus: run this test under dbus-run-session, as scripts/test-linux-qt-host-kit.sh does");
        struct Case {
            MemoryMonitor::Source source;
            const char *service;
            const char *path;
            const char *interface;
        };
        for (const Case &heard : {Case{MemoryMonitor::Source::LowMemoryMonitor, "org.freedesktop.LowMemoryMonitor",
                                       "/org/freedesktop/LowMemoryMonitor", "org.freedesktop.LowMemoryMonitor"},
                                  Case{MemoryMonitor::Source::Portal, "org.freedesktop.portal.Desktop",
                                       "/org/freedesktop/portal/desktop", "org.freedesktop.portal.MemoryMonitor"}}) {
            QObject sender;
            OnTheBus standing(sender, QLatin1String(heard.service), QLatin1String(heard.path));
            QVERIFY(standing.registered());
            std::vector<MigoMemoryWarningLevel> warned;
            MemoryMonitor monitor(QDBusConnection::sessionBus(), heard.source,
                                  [&warned](MigoMemoryWarningLevel level) { warned.push_back(level); });
            for (const uchar level : {uchar{20}, uchar{50}, uchar{100}, uchar{255}}) {
                QDBusMessage signal = QDBusMessage::createSignal(QLatin1String(heard.path), QLatin1String(heard.interface),
                                                                 QStringLiteral("LowMemoryWarning"));
                // A byte, as the desktop sends it: a bare uchar would go as an int.
                signal << QVariant::fromValue(level);
                QVERIFY(QDBusConnection::sessionBus().send(signal));
            }
            QTRY_COMPARE(warned.size(), std::size_t{3});
            QCOMPARE(warned, (std::vector<MigoMemoryWarningLevel>{MIGO_MEMORY_WARNING_MODERATE, MIGO_MEMORY_WARNING_LOW,
                                                                  MIGO_MEMORY_WARNING_CRITICAL}));
        }
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

    /// The gamepads the kernel shows: a pad following the kernel's gamepad
    /// layout mapped onto the standard one -- buttons, sticks, analog
    /// triggers, the D-pad's hat -- with its rumble; one that does not passed on
    /// as it is; the lowest free index; a report delivered whole, none after
    /// the kernel dropped events until it reports again; an unplugged pad
    /// withdrawn.
    void gamepads_are_the_kernels() {
        struct Heard {
            std::vector<std::pair<MigoGamepadInfo, bool>> connections;
            std::vector<std::string> ids;
            std::vector<std::string> mappings;
            std::vector<std::vector<float>> axes;
            std::vector<std::vector<MigoGamepadButton>> buttons;
            std::vector<double> times;
        } heard;
        Gamepads pads(Gamepads::Sink{
            [&heard](const MigoGamepadInfo &info, bool connected) {
                heard.connections.emplace_back(info, connected);
                heard.ids.emplace_back(info.id_utf8);
                heard.mappings.emplace_back(info.mapping_utf8);
            },
            [&heard](const MigoGamepadStateEvent &sample) {
                heard.axes.emplace_back(sample.axes, sample.axes + sample.axis_count);
                heard.buttons.emplace_back(sample.buttons, sample.buttons + sample.button_count);
                heard.times.push_back(sample.timestamp_ms);
            }});
        // A pipe for each device: what the kernel would write, written here.
        const auto device = [] {
            std::array<int, 2> ends{};
            if (::pipe2(ends.data(), O_NONBLOCK | O_CLOEXEC) != 0) qFatal("pipe2");
            return ends;
        };
        const auto send = [](int fd, std::vector<input_event> events) {
            for (input_event &event : events) {
                event.input_event_sec = 12;
                event.input_event_usec = 500000;
            }
            const auto bytes = static_cast<ssize_t>(events.size() * sizeof(input_event));
            QCOMPARE(::write(fd, events.data(), static_cast<size_t>(bytes)), bytes);
        };
        const auto event = [](int type, int code, int value) {
            input_event made{};
            made.type = static_cast<__u16>(type);
            made.code = static_cast<__u16>(code);
            made.value = value;
            return made;
        };

        Gamepads::Capabilities standard;
        standard.name = "Pad";
        standard.keys = {BTN_SOUTH, BTN_EAST, BTN_NORTH, BTN_WEST, BTN_TL, BTN_TR, BTN_SELECT, BTN_START, BTN_MODE,
                         BTN_THUMBL, BTN_THUMBR};
        for (const int stick : {ABS_X, ABS_Y, ABS_RX, ABS_RY}) standard.axes[stick] = {-32768, 32767};
        standard.axes[ABS_Z] = {0, 1023};
        standard.axes[ABS_RZ] = {0, 1023};
        standard.axes[ABS_HAT0X] = {-1, 1};
        standard.axes[ABS_HAT0Y] = {-1, 1};
        std::vector<std::tuple<float, float, std::uint32_t>> rumbled;
        const auto first = device();
        pads.add(QStringLiteral("/dev/input/event7"), first[0], standard,
                 [&rumbled](float strong, float weak, std::uint32_t ms) { rumbled.emplace_back(strong, weak, ms); });
        QCOMPARE(heard.connections.size(), std::size_t{1});
        const MigoGamepadInfo announced = heard.connections[0].first;
        QVERIFY(heard.connections[0].second);
        QCOMPARE(announced.index, 0U);
        QCOMPARE(announced.axis_count, 4U);
        QCOMPARE(announced.button_count, 17U);
        QCOMPARE(announced.haptics, MIGO_GAMEPAD_HAPTICS_DUAL_RUMBLE);
        QCOMPARE(heard.ids[0], std::string("Pad"));
        QCOMPARE(heard.mappings[0], std::string("standard"));

        // A, the left stick right, the left trigger full, the D-pad up; the left
        // action button is BTN_WEST, the top one BTN_NORTH.
        send(first[1], {event(EV_KEY, BTN_SOUTH, 1), event(EV_KEY, BTN_WEST, 1), event(EV_ABS, ABS_X, 32767),
                        event(EV_ABS, ABS_Z, 1023), event(EV_ABS, ABS_HAT0Y, -1), event(EV_SYN, SYN_REPORT, 0)});
        QTRY_COMPARE(heard.buttons.size(), std::size_t{1});
        const auto &pressed = heard.buttons[0];
        QVERIFY(pressed[0].flags & MIGO_GAMEPAD_BUTTON_FLAG_PRESSED);
        QCOMPARE(pressed[0].value, 1.0F);
        QVERIFY(pressed[2].flags & MIGO_GAMEPAD_BUTTON_FLAG_PRESSED);
        QCOMPARE(pressed[3].value, 0.0F);
        QCOMPARE(pressed[6].value, 1.0F);
        QVERIFY(pressed[6].flags & MIGO_GAMEPAD_BUTTON_FLAG_PRESSED);
        QVERIFY(pressed[12].flags & MIGO_GAMEPAD_BUTTON_FLAG_PRESSED);
        QCOMPARE(pressed[13].value, 0.0F);
        QCOMPARE(heard.axes[0][0], 1.0F);
        QVERIFY(std::abs(heard.axes[0][1]) < 1e-4F);
        QCOMPARE(heard.times[0], 12500.0);

        // Events after the kernel dropped some make no sample until it reports again.
        send(first[1], {event(EV_SYN, SYN_DROPPED, 0), event(EV_KEY, BTN_EAST, 1), event(EV_SYN, SYN_REPORT, 0)});
        QTest::qWait(50);
        QCOMPARE(heard.buttons.size(), std::size_t{1});
        send(first[1], {event(EV_ABS, ABS_Z, 0), event(EV_SYN, SYN_REPORT, 0)});
        QTRY_COMPARE(heard.buttons.size(), std::size_t{2});
        QCOMPARE(heard.buttons[1][6].value, 0.0F);

        // A joystick of its own layout: as it is.
        Gamepads::Capabilities stick;
        stick.name = "Stick";
        stick.keys = {BTN_TRIGGER, BTN_THUMB, BTN_THUMB2};
        stick.axes[ABS_X] = {0, 255};
        stick.axes[ABS_Y] = {0, 255};
        stick.axes[ABS_THROTTLE] = {0, 255};
        const auto second = device();
        pads.add(QStringLiteral("/dev/input/event9"), second[0], stick, {});
        QCOMPARE(heard.connections.back().first.index, 1U);
        QCOMPARE(heard.connections.back().first.button_count, 3U);
        QCOMPARE(heard.connections.back().first.axis_count, 3U);
        QCOMPARE(heard.connections.back().first.haptics, MIGO_GAMEPAD_HAPTICS_NONE);
        QCOMPARE(heard.mappings.back(), std::string());

        MigoGamepadRumble rumble{};
        rumble.struct_size = sizeof(rumble);
        rumble.index = 0;
        rumble.duration_ms = 200;
        rumble.strong_magnitude = 0.75F;
        rumble.weak_magnitude = 0.25F;
        pads.rumble(rumble);
        QCOMPARE(rumbled.size(), std::size_t{1});
        QCOMPARE(std::get<0>(rumbled[0]), 0.75F);
        QCOMPARE(std::get<2>(rumbled[0]), 200U);

        // Unplugged: withdrawn, and its index free for the next pad.
        ::close(first[1]);
        QTRY_COMPARE(heard.connections.size(), std::size_t{3});
        QVERIFY(!heard.connections.back().second);
        QCOMPARE(heard.connections.back().first.index, 0U);
        const auto third = device();
        pads.add(QStringLiteral("/dev/input/event11"), third[0], standard, {});
        QCOMPARE(heard.connections.back().first.index, 0U);
        ::close(second[1]);
        ::close(third[1]);
    }

    /// A Managed Session plays the pads' effects, so their rumble is announced.
    void a_managed_session_plays_the_gamepads_rumble() {
        QWidget parent;
        MigoManagedSession managed(*fake_migo::engine(), parent);
        QVERIFY(managed.isValid());
        QVERIFY(fake_migo::installed_callbacks().on_gamepad_rumble != nullptr);
        QCOMPARE(managed.close(), MIGO_OK);
    }
};

QTEST_MAIN(QtManagedSessionTest)
#include "qt_managed_session_test.moc"

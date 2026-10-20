#include "backlight.hpp"
#include "fake_migo.hpp"
#include "x11_player.hpp"

#include <migo/linux/qt6/host_services.hpp>
#include <migo/linux/qt6/managed_session.hpp>

#include <QAction>
#include <QApplication>
#include <QClipboard>
#include <QCursor>
#include <QDBusConnection>
#include <QDBusContext>
#include <QDBusError>
#include <QDeadlineTimer>
#include <QDir>
#include <QFile>
#include <QDialog>
#include <QJsonArray>
#include <QJsonDocument>
#include <QJsonObject>
#include <QLabel>
#include <QLineEdit>
#include <QMenu>
#include <QPixmap>
#include <QPlainTextEdit>
#include <QPushButton>
#include <QScreen>
#include <QTemporaryDir>
#include <QVBoxLayout>
#include <QtTest>

#include <memory>
#include <optional>

using migo::linux_host::SurfaceHost;
using migo::linux_host::SurfaceState;
using migo::linux_host::qt6::hostServiceBit;
using migo::linux_host::qt6::MigoManagedSession;
using migo::linux_host::qt6::MigoQtBuiltInHostServices;
using migo::linux_host::qt6::MigoQtHostServiceChannel;
using migo::linux_host::qt6::MigoQtHostServiceProvider;
using migo::linux_host::qt6::MigoQtHostServiceReply;
using migo::linux_host::qt6::MigoQtX11SurfaceView;

namespace {

bool require_xcb(const char *why, const char *file, int line) {
    if (QGuiApplication::platformName() == QStringLiteral("xcb")) return true;
    QTest::qSkip(why, file, line);
    return false;
}

std::string json(const QJsonObject &object) {
    return QJsonDocument(object).toJson(QJsonDocument::Compact).toStdString();
}

QJsonObject parsed(const std::string &payload) {
    return QJsonDocument::fromJson(QByteArray::fromStdString(payload)).object();
}

/// The answer the host gave the call `call_id`, or null.
const fake_migo::HostServiceAnswer *answerTo(std::uint64_t call_id) {
    for (const auto &answer : fake_migo::host_service_answers()) {
        if (answer.call_id == call_id) return &answer;
    }
    return nullptr;
}

/// The indicator standing over the game, or null.
QWidget *indicator() {
    for (QWidget *widget : QApplication::topLevelWidgets()) {
        if (widget->objectName() == QStringLiteral("migoIndicator") && widget->isVisible()) return widget;
    }
    return nullptr;
}

QString indicatorText() {
    QWidget *shown = indicator();
    if (shown == nullptr) return {};
    const auto labels = shown->findChildren<QLabel *>();
    for (QLabel *label : labels) {
        if (!label->text().isEmpty()) return label->text();
    }
    return {};
}

template <typename Window>
Window *shownWindow() {
    for (QWidget *widget : QApplication::topLevelWidgets()) {
        if (auto *window = qobject_cast<Window *>(widget); window != nullptr && window->isVisible()) return window;
    }
    return nullptr;
}

QPushButton *buttonNamed(QWidget &window, const QString &text) {
    const auto buttons = window.findChildren<QPushButton *>();
    for (QPushButton *button : buttons) {
        if (button->text() == text) return button;
    }
    return nullptr;
}

/// A Managed Session in a window of its own, closed on the way out so a failed
/// assertion reports itself rather than the wrapper's destroyed-while-open abort.
struct Managed {
    QWidget container;
    MigoManagedSession session;

    explicit Managed(const MigoManagedSession::Options &options = {})
        : session(*fake_migo::engine(), container, options) {}
    ~Managed() {
        if (!session.isClosed()) (void)session.close();
        // A view that attached (xcb) retires first: the library reports RELEASED,
        // and only then is the Session closed.
        if (!session.isClosed() && session.view() != nullptr) {
            fake_migo::set_release_status(session.view()->generation(), MIGO_SURFACE_RELEASE_RELEASED);
            QDeadlineTimer deadline(5000);
            while (!session.isClosed() && !deadline.hasExpired()) {
                QCoreApplication::processEvents(QEventLoop::AllEvents, 20);
            }
        }
    }
};

/// logind's part in a backlight, on the session bus: what it is asked to set,
/// written where the kernel shows the backlight, as logind has the kernel do.
class FakeLogind final : public QObject, protected QDBusContext {
    Q_OBJECT
    Q_CLASSINFO("D-Bus Interface", "org.freedesktop.login1.Session")

public:
    explicit FakeLogind(QString sysfs) : sysfs_(std::move(sysfs)) {}
    QStringList asked;
    bool refuse = false;

public Q_SLOTS:
    void SetBrightness(const QString &subsystem, const QString &name, uint brightness) {
        asked << QStringLiteral("%1 %2 %3").arg(subsystem, name).arg(brightness);
        if (refuse) {
            sendErrorReply(QDBusError::AccessDenied, QStringLiteral("not the active session"));
            return;
        }
        QFile file(sysfs_ + QLatin1Char('/') + name + QStringLiteral("/brightness"));
        if (file.open(QIODevice::WriteOnly)) file.write(QByteArray::number(brightness) + '\n');
    }

private:
    QString sysfs_;
};

/// A backlight where the kernel would show it.
void makeBacklight(const QString &sysfs, const QString &name, const char *type, int max, int brightness) {
    QDir().mkpath(sysfs + QLatin1Char('/') + name);
    const auto write = [&](const char *attribute, const QByteArray &value) {
        QFile file(sysfs + QLatin1Char('/') + name + QLatin1Char('/') + QLatin1String(attribute));
        QVERIFY(file.open(QIODevice::WriteOnly));
        file.write(value + '\n');
    };
    write("type", type);
    write("max_brightness", QByteArray::number(max));
    write("brightness", QByteArray::number(brightness));
}

/// An App's own services: the advert, and the clipboard it chose to answer itself.
class AppServices final : public MigoQtHostServiceProvider {
public:
    std::uint64_t services() const noexcept override {
        return hostServiceBit(MIGO_HOST_SERVICE_AD) | hostServiceBit(MIGO_HOST_SERVICE_CLIPBOARD);
    }
    void call(MigoHostService service, std::uint32_t method, const QJsonObject &request,
              MigoQtHostServiceReply reply) override {
        calls.push_back({service, method, request});
        (void)reply.succeed({{QStringLiteral("data"), QStringLiteral("the app's")}});
    }
    struct Call {
        MigoHostService service;
        std::uint32_t method;
        QJsonObject request;
    };
    std::vector<Call> calls;
};

/// A shown view, attached, with the built-in services over it; retired on the way out.
struct AttachedServices {
    SurfaceHost host{fake_migo::session()};
    QWidget container;
    MigoQtX11SurfaceView view{host, container};
    std::shared_ptr<MigoQtHostServiceChannel> channel = MigoQtHostServiceChannel::open(fake_migo::session());
    MigoQtBuiltInHostServices services{view, channel};

    AttachedServices() {
        view.resize(320, 180);
        container.show();
        view.show();
    }

    ~AttachedServices() {
        channel->close();
        if (view.surfaceState() == SurfaceState::Attached) (void)view.beginDetach();
        if (view.surfaceState() == SurfaceState::Retiring) {
            fake_migo::set_release_status(view.generation(), MIGO_SURFACE_RELEASE_RELEASED);
            bool released = false;
            (void)view.pollDetach(&released);
        }
    }

    void command(MigoHostService service, std::uint32_t method, const QJsonObject &request) {
        services.call(service, method, request, channel->reply(0));
    }
};

/// Whether the game was pressed at `at`.
bool pressAt(QPointF at) {
    for (const fake_migo::PointerRecord &pointer : fake_migo::pointers()) {
        if (pointer.event_type == MIGO_POINTER_EVENT_DOWN && pointer.x == static_cast<float>(at.x()) &&
            pointer.y == static_cast<float>(at.y())) {
            return true;
        }
    }
    return false;
}

void press(QWidget &view) {
    const QPointF at(10.0, 10.0);
    QMouseEvent event(QEvent::MouseButtonPress, at, at, view.mapToGlobal(at), Qt::LeftButton, Qt::LeftButton,
                      Qt::NoModifier);
    QCoreApplication::sendEvent(&view, &event);
}

}  // namespace

class QtHostServicesTest final : public QObject {
    Q_OBJECT

private slots:
    void init() { fake_migo::reset(); }

    /// A Managed Session declares what Qt answers itself, and creates the
    /// Session with no network and the default launch unless told otherwise.
    void a_managed_session_declares_the_built_in_services() {
        Managed fixture;
        MigoManagedSession &managed = fixture.session;
        QVERIFY(managed.isValid());

        const MigoHostCallbacks &installed = fake_migo::installed_callbacks();
        QVERIFY(installed.on_host_service_call != nullptr);
        QCOMPARE(installed.host_services, hostServiceBit(MIGO_HOST_SERVICE_INTERACTION) |
                                              hostServiceBit(MIGO_HOST_SERVICE_CLIPBOARD) |
                                              hostServiceBit(MIGO_HOST_SERVICE_WINDOW) |
                                              hostServiceBit(MIGO_HOST_SERVICE_SCREEN));
        QCOMPARE(fake_migo::created_network_policy(), std::string());
        QCOMPARE(fake_migo::created_launch_options(), std::string());

        QCOMPARE(managed.close(), MIGO_OK);
    }

    /// The App's network policy and launch options reach the Session it is created with.
    void the_options_reach_the_session() {
        MigoManagedSession::Options options;
        options.networkPolicyJson = R"({"request":["example.com"]})";
        options.launchOptionsJson = R"({"scene":1007})";
        Managed fixture(options);
        MigoManagedSession &managed = fixture.session;
        QVERIFY(managed.isValid());

        QCOMPARE(fake_migo::created_network_policy(), std::string(R"({"request":["example.com"]})"));
        QCOMPARE(fake_migo::created_launch_options(), std::string(R"({"scene":1007})"));
        QCOMPARE(managed.close(), MIGO_OK);
    }

    /// The App says who it is: the identifier it gives, or else its desktop
    /// file name; with neither, the Session is told nothing.
    void the_app_says_who_it_is() {
        {
            Managed fixture;
            QCOMPARE(fake_migo::reported_host_app_id(), std::string());
        }
        fake_migo::reset();
        QGuiApplication::setDesktopFileName(QStringLiteral("com.example.desktop"));
        {
            Managed fixture;
            QCOMPARE(fake_migo::reported_host_app_id(), std::string("com.example.desktop"));
        }
        fake_migo::reset();
        {
            MigoManagedSession::Options options;
            options.appId = QStringLiteral("com.example.given");
            Managed fixture(options);
            QCOMPARE(fake_migo::reported_host_app_id(), std::string("com.example.given"));
        }
        QGuiApplication::setDesktopFileName(QString());
    }

    /// What the App names is the App's -- a service it adds, and one the Host
    /// Kit would otherwise answer -- and the rest is the Host Kit's.
    void the_app_answers_what_it_names() {
        AppServices app;
        MigoManagedSession::Options options;
        options.appServices = &app;
        Managed fixture(options);
        MigoManagedSession &managed = fixture.session;
        QVERIFY(managed.isValid());
        QCOMPARE(fake_migo::installed_callbacks().host_services,
                 hostServiceBit(MIGO_HOST_SERVICE_AD) | hostServiceBit(MIGO_HOST_SERVICE_CLIPBOARD) |
                     hostServiceBit(MIGO_HOST_SERVICE_INTERACTION) | hostServiceBit(MIGO_HOST_SERVICE_WINDOW) |
                     hostServiceBit(MIGO_HOST_SERVICE_SCREEN));

        fake_migo::deliver_host_service_call_from_engine_thread(MIGO_HOST_SERVICE_CLIPBOARD,
                                                                MIGO_CLIPBOARD_GET_CLIPBOARD_DATA, 3, "{}");
        QTRY_VERIFY(answerTo(3) != nullptr);
        QCOMPARE(parsed(answerTo(3)->payload).value(QStringLiteral("data")).toString(), QStringLiteral("the app's"));
        QCOMPARE(app.calls.size(), std::size_t{1});

        fake_migo::deliver_host_service_call_from_engine_thread(
            MIGO_HOST_SERVICE_INTERACTION, MIGO_INTERACTION_SHOW_ACTION_SHEET, 4, json({{"itemList", QJsonArray()}}));
        QTRY_VERIFY(answerTo(4) != nullptr);
        QCOMPARE(answerTo(4)->message, std::string("itemList is empty"));
        QCOMPARE(app.calls.size(), std::size_t{1});

        QCOMPARE(managed.close(), MIGO_OK);
    }

    /// The clipboard is Qt's: what content writes, it reads back.
    void the_clipboard_round_trips() {
        Managed fixture;
        MigoManagedSession &managed = fixture.session;
        fake_migo::deliver_host_service_call_from_engine_thread(
            MIGO_HOST_SERVICE_CLIPBOARD, MIGO_CLIPBOARD_SET_CLIPBOARD_DATA, 1, json({{"data", "migo \u00e9"}}));
        QTRY_VERIFY(answerTo(1) != nullptr);
        QCOMPARE(answerTo(1)->status, MIGO_HOST_SERVICE_STATUS_OK);
        QCOMPARE(QGuiApplication::clipboard()->text(), QStringLiteral("migo \u00e9"));

        fake_migo::deliver_host_service_call_from_engine_thread(MIGO_HOST_SERVICE_CLIPBOARD,
                                                                MIGO_CLIPBOARD_GET_CLIPBOARD_DATA, 2, "{}");
        QTRY_VERIFY(answerTo(2) != nullptr);
        QCOMPARE(parsed(answerTo(2)->payload).value(QStringLiteral("data")).toString(), QStringLiteral("migo \u00e9"));
        QCOMPARE(managed.close(), MIGO_OK);
    }

    /// A modal answers which button the player chose, and an editable one what they typed.
    void a_modal_answers_the_players_choice() {
        Managed fixture;
        MigoManagedSession &managed = fixture.session;

        fake_migo::deliver_host_service_call_from_engine_thread(
            MIGO_HOST_SERVICE_INTERACTION, MIGO_INTERACTION_SHOW_MODAL, 10,
            json({{"title", "Quit?"}, {"content", "Progress is saved."}, {"confirmText", "Quit"}}));
        QTRY_VERIFY(shownWindow<QDialog>() != nullptr);
        QDialog *dialog = shownWindow<QDialog>();
        QVERIFY(buttonNamed(*dialog, QStringLiteral("\u53d6\u6d88")) != nullptr);
        buttonNamed(*dialog, QStringLiteral("Quit"))->click();
        QTRY_VERIFY(answerTo(10) != nullptr);
        QCOMPARE(parsed(answerTo(10)->payload),
                 QJsonObject({{QStringLiteral("confirm"), true}, {QStringLiteral("cancel"), false}}));

        fake_migo::deliver_host_service_call_from_engine_thread(
            MIGO_HOST_SERVICE_INTERACTION, MIGO_INTERACTION_SHOW_MODAL, 11,
            json({{"title", "Name"}, {"content", "Ada"}, {"editable", true}, {"placeholderText", "your name"}}));
        QTRY_VERIFY(shownWindow<QDialog>() != nullptr);
        dialog = shownWindow<QDialog>();
        QLineEdit *field = dialog->findChild<QLineEdit *>();
        QVERIFY(field != nullptr);
        QCOMPARE(field->text(), QStringLiteral("Ada"));
        QCOMPARE(field->placeholderText(), QStringLiteral("your name"));
        field->setText(QStringLiteral("Grace"));
        buttonNamed(*dialog, QStringLiteral("\u786e\u5b9a"))->click();
        QTRY_VERIFY(answerTo(11) != nullptr);
        QCOMPARE(parsed(answerTo(11)->payload).value(QStringLiteral("content")).toString(), QStringLiteral("Grace"));

        // Dismissed any other way, it is declined.
        fake_migo::deliver_host_service_call_from_engine_thread(
            MIGO_HOST_SERVICE_INTERACTION, MIGO_INTERACTION_SHOW_MODAL, 12, json({{"content", "Sure?"}}));
        QTRY_VERIFY(shownWindow<QDialog>() != nullptr);
        shownWindow<QDialog>()->reject();
        QTRY_VERIFY(answerTo(12) != nullptr);
        QCOMPARE(parsed(answerTo(12)->payload),
                 QJsonObject({{QStringLiteral("confirm"), false}, {QStringLiteral("cancel"), true}}));
        QCOMPARE(managed.close(), MIGO_OK);
    }

    /// An action sheet answers the item tapped, or fails as cancelled when dismissed.
    void an_action_sheet_answers_the_tap_or_the_dismissal() {
        Managed fixture;
        MigoManagedSession &managed = fixture.session;

        fake_migo::deliver_host_service_call_from_engine_thread(
            MIGO_HOST_SERVICE_INTERACTION, MIGO_INTERACTION_SHOW_ACTION_SHEET, 20,
            json({{"alertText", "Share"}, {"itemList", QJsonArray({"Friends", "Group"})}}));
        QTRY_VERIFY(shownWindow<QMenu>() != nullptr);
        QMenu *menu = shownWindow<QMenu>();
        QAction *group = nullptr;
        for (QAction *action : menu->actions()) {
            if (action->text() == QStringLiteral("Group")) group = action;
        }
        QVERIFY(group != nullptr);
        // As a click does it: the menu hides, then reports the item.
        menu->hide();
        group->trigger();
        QTRY_VERIFY(answerTo(20) != nullptr);
        QCOMPARE(parsed(answerTo(20)->payload).value(QStringLiteral("tapIndex")).toInt(-1), 1);

        fake_migo::deliver_host_service_call_from_engine_thread(
            MIGO_HOST_SERVICE_INTERACTION, MIGO_INTERACTION_SHOW_ACTION_SHEET, 21,
            json({{"itemList", QJsonArray({"Friends"})}}));
        QTRY_VERIFY(shownWindow<QMenu>() != nullptr);
        shownWindow<QMenu>()->close();
        QTRY_VERIFY(answerTo(21) != nullptr);
        QCOMPARE(answerTo(21)->status, MIGO_HOST_SERVICE_STATUS_FAIL);
        QCOMPARE(answerTo(21)->message, std::string("cancel"));
        QCOMPARE(managed.close(), MIGO_OK);
    }

    /// A toast hides itself; a loading indicator replaces it and stays; each
    /// hide hides only its own kind; a command is answered by nothing.
    void toasts_and_loading_are_one_at_a_time() {
        Managed fixture;
        MigoManagedSession &managed = fixture.session;

        fake_migo::deliver_host_service_call_from_engine_thread(
            MIGO_HOST_SERVICE_INTERACTION, MIGO_INTERACTION_SHOW_TOAST, 0,
            json({{"title", "Saved"}, {"duration", 400}}));
        QTRY_COMPARE(indicatorText(), QStringLiteral("Saved"));
        QTRY_VERIFY(indicator() == nullptr);

        fake_migo::deliver_host_service_call_from_engine_thread(
            MIGO_HOST_SERVICE_INTERACTION, MIGO_INTERACTION_SHOW_TOAST, 0,
            json({{"title", "Saved"}, {"duration", 60000}}));
        fake_migo::deliver_host_service_call_from_engine_thread(MIGO_HOST_SERVICE_INTERACTION,
                                                                MIGO_INTERACTION_HIDE_LOADING, 0, "{}");
        QTRY_COMPARE(indicatorText(), QStringLiteral("Saved"));
        fake_migo::deliver_host_service_call_from_engine_thread(
            MIGO_HOST_SERVICE_INTERACTION, MIGO_INTERACTION_SHOW_LOADING, 0, json({{"title", "Loading"}}));
        QTRY_COMPARE(indicatorText(), QStringLiteral("Loading"));
        fake_migo::deliver_host_service_call_from_engine_thread(MIGO_HOST_SERVICE_INTERACTION,
                                                                MIGO_INTERACTION_HIDE_TOAST, 0, "{}");
        QTRY_COMPARE(indicatorText(), QStringLiteral("Loading"));
        fake_migo::deliver_host_service_call_from_engine_thread(MIGO_HOST_SERVICE_INTERACTION,
                                                                MIGO_INTERACTION_HIDE_LOADING, 0, "{}");
        QTRY_VERIFY(indicator() == nullptr);

        QVERIFY(fake_migo::host_service_answers().empty());
        QCOMPARE(managed.close(), MIGO_OK);
    }

    /// While an indicator with a mask is up, the game hears none of the
    /// player's input; once it is gone, it does again.
    void a_mask_keeps_input_from_the_game() {
        if (!require_xcb("xcb-only test", __FILE__, __LINE__)) return;
        AttachedServices fixture;
        QTRY_COMPARE(fake_migo::calls().attach, 1);

        fixture.command(MIGO_HOST_SERVICE_INTERACTION, MIGO_INTERACTION_SHOW_LOADING,
                        {{QStringLiteral("title"), QStringLiteral("Wait")}, {QStringLiteral("mask"), true}});
        press(fixture.view);
        QCOMPARE(fake_migo::pointers().size(), std::size_t{0});

        fixture.command(MIGO_HOST_SERVICE_INTERACTION, MIGO_INTERACTION_HIDE_LOADING, {});
        press(fixture.view);
        QCOMPARE(fake_migo::pointers().size(), std::size_t{1});
    }

    /// An indicator stands over the game and takes none of the player's
    /// input: a click on it is the game's.
    void an_indicator_takes_none_of_the_players_input() {
        if (!require_xcb("xcb-only test", __FILE__, __LINE__)) return;
        AttachedServices fixture;
        QTRY_COMPARE(fake_migo::calls().attach, 1);
        fixture.command(MIGO_HOST_SERVICE_INTERACTION, MIGO_INTERACTION_SHOW_TOAST,
                        {{QStringLiteral("title"), QStringLiteral("Saved")}, {QStringLiteral("duration"), 10000}});
        QCOMPARE(fixture.services.indicatorTitle(), QStringLiteral("Saved"));
        x11_player::Hands hands;
        const QPoint centre = fixture.view.rect().center();
        QCursor::setPos(fixture.view.mapToGlobal(centre));
        hands.click(1);
        QTRY_VERIFY(pressAt(QPointF(centre)));
    }

    /// The cursor over the game is the one content asks for: a CSS keyword, the
    /// system's for "default", or an image with its hotspot clamped into it.
    void the_cursor_is_the_one_content_asks_for() {
        Managed fixture;
        MigoQtX11SurfaceView &view = *fixture.session.view();
        const auto cursor = [](const QJsonObject &request) {
            fake_migo::deliver_host_service_call_from_engine_thread(MIGO_HOST_SERVICE_WINDOW, MIGO_WINDOW_SET_CURSOR,
                                                                    0, json(request));
        };
        cursor({{"keyword", "pointer"}});
        QTRY_COMPARE(view.cursor().shape(), Qt::PointingHandCursor);
        cursor({{"keyword", "none"}});
        QTRY_COMPARE(view.cursor().shape(), Qt::BlankCursor);
        cursor({{"keyword", "default"}});
        QTRY_VERIFY(!view.testAttribute(Qt::WA_SetCursor));

        QTemporaryDir directory;
        QPixmap image(8, 8);
        image.fill(Qt::red);
        const QString path = directory.filePath(QStringLiteral("cursor.png"));
        QVERIFY(image.save(path));
        cursor({{"path", path}, {"x", 3}, {"y", 99}});
        QTRY_COMPARE(view.cursor().shape(), Qt::BitmapCursor);
        QCOMPARE(view.cursor().hotSpot(), QPoint(3, 7));
        // An image Qt cannot read leaves the cursor as it was.
        cursor({{"path", directory.filePath(QStringLiteral("none.png"))}});
        QCoreApplication::processEvents();
        QCOMPARE(view.cursor().shape(), Qt::BitmapCursor);
        QCOMPARE(fixture.session.close(), MIGO_OK);
    }

    /// The size asked for is the game's: the window grows or shrinks by what the
    /// view must, and a full-screen window is not sized.
    void the_window_is_sized_for_the_game() {
        Managed fixture;
        auto *layout = new QVBoxLayout(&fixture.container);
        layout->setContentsMargins(0, 0, 0, 0);
        layout->addWidget(fixture.session.view());
        fixture.container.resize(320, 200);
        fixture.container.show();
        QTRY_COMPARE(fixture.session.view()->size(), QSize(320, 200));

        const qreal ratio = fixture.session.view()->devicePixelRatioF();
        fake_migo::deliver_host_service_call_from_engine_thread(
            MIGO_HOST_SERVICE_WINDOW, MIGO_WINDOW_SET_WINDOW_SIZE, 50,
            json({{"width", qRound(400 * ratio)}, {"height", qRound(300 * ratio)}}));
        QTRY_VERIFY(answerTo(50) != nullptr);
        QCOMPARE(answerTo(50)->status, MIGO_HOST_SERVICE_STATUS_OK);
        QTRY_COMPARE(fixture.session.view()->size(), QSize(400, 300));

        fake_migo::deliver_host_service_call_from_engine_thread(MIGO_HOST_SERVICE_WINDOW,
                                                                MIGO_WINDOW_SET_WINDOW_SIZE, 51,
                                                                json({{"width", 0}, {"height", 300}}));
        QTRY_VERIFY(answerTo(51) != nullptr);
        QCOMPARE(answerTo(51)->message, std::string("width and height must be positive numbers"));

        fixture.container.showFullScreen();
        fake_migo::deliver_host_service_call_from_engine_thread(MIGO_HOST_SERVICE_WINDOW,
                                                                MIGO_WINDOW_SET_WINDOW_SIZE, 52,
                                                                json({{"width", 200}, {"height", 100}}));
        QTRY_VERIFY(answerTo(52) != nullptr);
        QCOMPARE(answerTo(52)->message, std::string("the window is full screen"));
        fixture.container.hide();
        QCOMPARE(fixture.session.close(), MIGO_OK);
    }

    /// Each change of the window's state is reported once, in the platform's
    /// words; a pointer lock asked for with the player elsewhere is refused,
    /// and said to be.
    void the_window_reports_its_state_and_a_refused_pointer_lock() {
        Managed fixture;
        const auto events = [] {
            std::vector<std::string> said;
            for (const auto &event : fake_migo::host_service_events()) {
                if (event.service == MIGO_HOST_SERVICE_WINDOW) said.push_back(event.payload);
            }
            return said;
        };
        // One change at a time, each settled: the platform confirms a state
        // change after the event loop runs, and a request made before that is
        // confirmed in its turn too.
        const auto settle = [](QWidget &window, Qt::WindowStates state) {
            window.setWindowState(state);
            QTest::qWait(100);
        };
        settle(fixture.container, Qt::WindowMinimized);
        settle(fixture.container, Qt::WindowMinimized);
        settle(fixture.container, Qt::WindowFullScreen);
        settle(fixture.container, Qt::WindowNoState);
        QCOMPARE(events(), (std::vector<std::string>{R"({"state":"minimize"})", R"({"state":"maximize"})",
                                                     R"({"state":"normalize"})"}));

        // The view does not hold the keyboard: Esc, the way out, would not reach it.
        fixture.session.view()->clearFocus();
        fake_migo::deliver_host_service_call_from_engine_thread(MIGO_HOST_SERVICE_WINDOW,
                                                                MIGO_WINDOW_REQUEST_POINTER_LOCK, 0, "{}");
        QTRY_COMPARE(events().size(), std::size_t{4});
        QCOMPARE(events().back(), std::string(R"({"locked":false})"));
        QCOMPARE(fixture.session.close(), MIGO_OK);
    }

    /// A lock content asks for with the player at the game is taken and said
    /// to be, as its release is.
    void a_pointer_lock_is_reported_as_it_is_taken_and_released() {
        if (!require_xcb("xcb-only test", __FILE__, __LINE__)) return;
        Managed fixture;
        MigoQtX11SurfaceView &view = *fixture.session.view();
        auto *layout = new QVBoxLayout(&fixture.container);
        layout->setContentsMargins(0, 0, 0, 0);
        layout->addWidget(&view);
        fixture.container.resize(320, 200);
        fixture.container.show();
        QTRY_COMPARE(fake_migo::calls().attach, 1);
        fixture.container.activateWindow();
        QVERIFY(QTest::qWaitForWindowActive(&fixture.container));
        view.setFocus();
        QCursor::setPos(view.mapToGlobal(QPoint(10, 10)));
        const auto locks = [] {
            std::vector<std::string> said;
            for (const auto &event : fake_migo::host_service_events()) {
                if (event.service == MIGO_HOST_SERVICE_WINDOW &&
                    event.event == MIGO_WINDOW_EVENT_POINTER_LOCK_CHANGE) {
                    said.push_back(event.payload);
                }
            }
            return said;
        };
        const auto command = [](std::uint32_t method) {
            fake_migo::deliver_host_service_call_from_engine_thread(MIGO_HOST_SERVICE_WINDOW, method, 0, "{}");
        };

        command(MIGO_WINDOW_REQUEST_POINTER_LOCK);
        QTRY_VERIFY(view.isPointerLocked());
        command(MIGO_WINDOW_REQUEST_POINTER_LOCK);
        command(MIGO_WINDOW_EXIT_POINTER_LOCK);
        QTRY_VERIFY(!view.isPointerLocked());
        QCOMPARE(locks(), (std::vector<std::string>{R"({"locked":true})", R"({"locked":false})"}));
        QCOMPARE(fixture.session.close(), MIGO_OK);
    }

    /// The screen's orientation is the game's shape: each crossing of the view
    /// from one shape to the other is reported once, and setDeviceOrientation
    /// turns the window -- refused for a value that is neither shape, while the
    /// window is full screen, and when the screen cannot hold it turned.
    void the_game_turns_with_its_window() {
        Managed fixture;
        MigoQtX11SurfaceView &view = *fixture.session.view();
        auto *layout = new QVBoxLayout(&fixture.container);
        layout->setContentsMargins(0, 0, 0, 0);
        layout->addWidget(&view);
        fixture.container.resize(320, 200);
        fixture.container.show();
        QTRY_COMPARE(view.size(), QSize(320, 200));
        const auto turns = [] {
            std::vector<std::string> said;
            for (const auto &event : fake_migo::host_service_events()) {
                if (event.service == MIGO_HOST_SERVICE_SCREEN &&
                    event.event == MIGO_SCREEN_EVENT_DEVICE_ORIENTATION_CHANGE) {
                    said.push_back(event.payload);
                }
            }
            return said;
        };
        const auto turn = [](std::uint64_t call_id, const char *value) {
            fake_migo::deliver_host_service_call_from_engine_thread(
                MIGO_HOST_SERVICE_SCREEN, MIGO_SCREEN_SET_DEVICE_ORIENTATION, call_id, json({{"value", value}}));
            QTRY_VERIFY(answerTo(call_id) != nullptr);
        };
        const std::size_t before = turns().size();

        turn(70, "portrait");
        QCOMPARE(answerTo(70)->status, MIGO_HOST_SERVICE_STATUS_OK);
        QTRY_COMPARE(view.size(), QSize(200, 320));
        QTRY_COMPARE(turns().size(), before + 1);
        QCOMPARE(turns().back(), std::string(R"({"value":"portrait"})"));
        turn(71, "portrait");
        QCOMPARE(answerTo(71)->status, MIGO_HOST_SERVICE_STATUS_OK);
        QCOMPARE(view.size(), QSize(200, 320));
        turn(72, "upside-down");
        QCOMPARE(answerTo(72)->message, std::string("value must be portrait or landscape"));

        // Wider than the screen is tall: turned, it would not fit.
        const QSize room = fixture.container.screen()->availableGeometry().size();
        fixture.container.resize(room.height() + 50, 100);
        QTRY_COMPARE(view.size(), QSize(room.height() + 50, 100));
        QTRY_COMPARE(turns().size(), before + 2);
        QCOMPARE(turns().back(), std::string(R"({"value":"landscape"})"));
        turn(73, "portrait");
        QCOMPARE(answerTo(73)->message, std::string("the screen cannot hold the window turned"));

        fixture.container.showFullScreen();
        turn(74, "portrait");
        QCOMPARE(answerTo(74)->message, std::string("the window is full screen"));
        fixture.container.hide();
        QCOMPARE(fixture.session.close(), MIGO_OK);
    }

    /// setEnableDebug turns the debugging aids: the Session is told -- with who
    /// the App is, as before -- and a panel over the top of the game shows what
    /// content writes to its console, taking none of the player's input.
    void the_debugging_aids_show_the_console_and_tell_the_session() {
        MigoManagedSession::Options options;
        options.appId = QStringLiteral("com.example.debugging");
        Managed fixture(options);
        MigoQtX11SurfaceView &view = *fixture.session.view();
        auto *layout = new QVBoxLayout(&fixture.container);
        layout->setContentsMargins(0, 0, 0, 0);
        layout->addWidget(&view);
        fixture.container.resize(320, 200);
        fixture.container.show();
        QTRY_COMPARE(view.size(), QSize(320, 200));
        QSignalSpy changes(&fixture.session, &MigoManagedSession::debugAidsChanged);
        const auto panel = [] {
            for (QWidget *window : QApplication::topLevelWidgets()) {
                if (window->objectName() == QLatin1String("migoDebugPanel")) return window;
            }
            return static_cast<QWidget *>(nullptr);
        };
        const auto debugging = [](bool on) {
            fake_migo::deliver_host_service_call_from_engine_thread(
                MIGO_HOST_SERVICE_SCREEN, MIGO_SCREEN_SET_ENABLE_DEBUG, 0, json({{"enableDebug", on}}));
        };
        QCOMPARE(panel(), nullptr);
        debugging(true);
        QTRY_VERIFY(fixture.session.debugAids());
        QCOMPARE(changes.size(), 1);
        QCOMPARE(fake_migo::reported_host_app_flags(), std::uint32_t{MIGO_HOST_APP_INFO_FLAG_DEBUG});
        QCOMPARE(fake_migo::reported_host_app_id(), std::string("com.example.debugging"));
        QVERIFY(panel() != nullptr);
        QVERIFY(panel()->isVisible());
        QCOMPARE(panel()->geometry().topLeft(), view.mapToGlobal(QPoint(0, 0)));
        QCOMPARE(panel()->width(), view.width());
        fake_migo::write_console(1, "hello");
        fake_migo::write_console(3, "it broke");
        auto *lines = panel()->findChild<QPlainTextEdit *>();
        QVERIFY(lines != nullptr);
        QTRY_VERIFY(lines->toPlainText().contains(QStringLiteral("E it broke")));
        QVERIFY(lines->toPlainText().contains(QStringLiteral("I hello")));

        if (QGuiApplication::platformName() == QStringLiteral("xcb")) {
            // A click on the panel is the game's.
            QTRY_COMPARE(fake_migo::calls().attach, 1);
            x11_player::Hands hands;
            QCursor::setPos(view.mapToGlobal(QPoint(10, 10)));
            hands.click(1);
            QTRY_VERIFY(pressAt(QPointF(10.0, 10.0)));
        }

        debugging(false);
        QTRY_VERIFY(!fixture.session.debugAids());
        QCOMPARE(changes.size(), 2);
        QCOMPARE(fake_migo::reported_host_app_flags(), std::uint32_t{0});
        QCOMPARE(fake_migo::reported_host_app_id(), std::string("com.example.debugging"));
        QCOMPARE(panel(), nullptr);
        QCOMPARE(fixture.session.close(), MIGO_OK);
    }

    /// The brightness is the backlight the desktop's own tools pick -- the
    /// firmware's over a raw one -- read where the kernel shows it and set
    /// through logind, never to unlit; a screen with none, or logind refusing,
    /// is said to be.
    void the_backlight_is_read_from_the_kernel_and_set_through_logind() {
        QDBusConnection bus = QDBusConnection::sessionBus();
        QVERIFY2(bus.isConnected(), "no session bus: run this test under dbus-run-session, as scripts/test-linux-qt-host-kit.sh does");
        QTemporaryDir sysfs;
        FakeLogind logind(sysfs.path());
        QVERIFY(bus.registerService(QStringLiteral("org.freedesktop.login1")));
        QVERIFY(bus.registerObject(QStringLiteral("/org/freedesktop/login1/session/auto"), &logind,
                                   QDBusConnection::ExportAllSlots));
        migo::linux_host::qt6::Backlight backlight(sysfs.path(), bus);
        std::optional<QString> said;
        const auto set = [&](double value) {
            said.reset();
            backlight.setBrightness(value, [&said](const QString &error) { said = error; });
            return QTest::qWaitFor([&said] { return said.has_value(); });
        };

        QVERIFY(!backlight.brightness().has_value());
        QVERIFY(set(0.5));
        QCOMPARE(*said, QStringLiteral("the screen has no backlight the system controls"));

        makeBacklight(sysfs.path(), QStringLiteral("intel_backlight"), "raw", 1000, 500);
        makeBacklight(sysfs.path(), QStringLiteral("acpi_video0"), "firmware", 100, 30);
        QCOMPARE(backlight.brightness(), std::optional<double>(0.3));
        QVERIFY(set(0.75));
        QCOMPARE(*said, QString());
        QCOMPARE(logind.asked.back(), QStringLiteral("backlight acpi_video0 75"));
        QCOMPARE(backlight.brightness(), std::optional<double>(0.75));
        QVERIFY(set(0.0));
        QCOMPARE(logind.asked.back(), QStringLiteral("backlight acpi_video0 1"));
        QVERIFY(set(4.0));
        QCOMPARE(logind.asked.back(), QStringLiteral("backlight acpi_video0 100"));

        logind.refuse = true;
        QVERIFY(set(0.5));
        QCOMPARE(*said, QStringLiteral("not the active session"));
        bus.unregisterObject(QStringLiteral("/org/freedesktop/login1/session/auto"));
        QVERIFY(bus.unregisterService(QStringLiteral("org.freedesktop.login1")));
    }

    /// A call is answered once; a failure needs a reason, and one without is
    /// refused without spending the answer.
    void a_call_is_answered_once_and_a_failure_names_why() {
        AppServices app;
        MigoManagedSession::Options options;
        options.appServices = &app;
        Managed fixture(options);
        MigoManagedSession &managed = fixture.session;
        std::shared_ptr<MigoQtHostServiceChannel> channel = MigoQtHostServiceChannel::open(fake_migo::session());

        fake_migo::deliver_host_service_call_from_engine_thread(MIGO_HOST_SERVICE_AD, 0, 30, "{}");
        QTRY_VERIFY(answerTo(30) != nullptr);
        // The App answered it; a second answer through a copy of the reply is refused here, not by the library.
        MigoQtHostServiceReply reply = channel->reply(31);
        QCOMPARE(reply.fail(QString()), MIGO_ERROR_INVALID_ARGUMENT);
        fake_migo::deliver_host_service_call_from_engine_thread(MIGO_HOST_SERVICE_AD, 0, 31, "{}");
        QTRY_VERIFY(answerTo(31) != nullptr);
        QCOMPARE(reply.fail(QStringLiteral("too late")), MIGO_ERROR_INVALID_ARGUMENT);
        MigoQtHostServiceReply copy = reply;
        QCOMPARE(copy.succeed(), MIGO_ERROR_INVALID_STATE);
        QCOMPARE(fake_migo::host_service_answers().size(), std::size_t{2});
        channel->close();
        QCOMPARE(managed.close(), MIGO_OK);
    }

    /// A dialog still up when the game goes is closed with it, and its answer
    /// reaches nobody: the Session it would have reached is destroyed.
    void what_is_shown_goes_with_the_game() {
        Managed fixture;
        MigoManagedSession &managed = fixture.session;
        fake_migo::deliver_host_service_call_from_engine_thread(
            MIGO_HOST_SERVICE_INTERACTION, MIGO_INTERACTION_SHOW_MODAL, 40, json({{"content", "Still there?"}}));
        QTRY_VERIFY(shownWindow<QDialog>() != nullptr);

        QCOMPARE(managed.close(), MIGO_OK);
        QVERIFY(managed.isClosed());
        QTRY_VERIFY(shownWindow<QDialog>() == nullptr);
        QVERIFY(answerTo(40) == nullptr);
        QVERIFY(!fake_migo::session_is_alive());
    }
};

QTEST_MAIN(QtHostServicesTest)
#include "qt_host_services_test.moc"

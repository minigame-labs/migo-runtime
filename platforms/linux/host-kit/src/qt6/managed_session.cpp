#include <migo/linux/qt6/managed_session.hpp>

#include "gamepads.hpp"
#include "memory_monitor.hpp"
#include "overlay.hpp"
#include "sandbox.hpp"
#include "screen_awake.hpp"
#include "system_reports.hpp"

#include <QDBusConnection>
#include <QElapsedTimer>
#include <QWindow>
#include <QEvent>
#include <QFontDatabase>
#include <QGuiApplication>
#include <QJsonArray>
#include <QJsonDocument>
#include <QJsonObject>
#include <QLabel>
#include <QMetaObject>
#include <QPlainTextEdit>
#include <QPointer>
#include <QThread>
#include <QTimer>
#include <QVBoxLayout>
#include <QWidget>

#include <atomic>
#include <cmath>
#include <optional>
#include <utility>

namespace migo::linux_host::qt6 {

namespace {

/// The debugging panel, shown while the debugging aids are on: the frames a
/// second the view delivered and the latest lines content wrote to its console,
/// read from the Session twice a second however fast content writes, over the
/// top of the game (`showOverTheGame`), taking no input, so the game stays
/// playable under it; it follows the newest line.
class DebugPanel final : public QWidget {
public:
    /// The panel keeps the latest lines, as the Session's own buffer keeps its latest.
    static constexpr int kLineLimit = 200;

    DebugPanel(MigoSession *session, MigoQtX11SurfaceView &view)
        : QWidget(view.window(), Qt::ToolTip | Qt::FramelessWindowHint | Qt::WindowDoesNotAcceptFocus),
          session_(session),
          view_(&view),
          header_(new QLabel(this)),
          lines_(new QPlainTextEdit(this)) {
        setObjectName(QStringLiteral("migoDebugPanel"));
        setStyleSheet(QStringLiteral(
            "#migoDebugPanel { background: rgba(20, 20, 20, 200); }"
            " QLabel { color: white; font-weight: 600; }"
            " QPlainTextEdit { background: transparent; border: none; color: #e0e0e0; }"));
        const QFont fixed = QFontDatabase::systemFont(QFontDatabase::FixedFont);
        header_->setFont(fixed);
        lines_->setFont(fixed);
        lines_->setReadOnly(true);
        lines_->setMaximumBlockCount(kLineLimit);
        lines_->setVerticalScrollBarPolicy(Qt::ScrollBarAlwaysOff);
        lines_->setHorizontalScrollBarPolicy(Qt::ScrollBarAlwaysOff);
        auto *layout = new QVBoxLayout(this);
        layout->setContentsMargins(6, 4, 6, 4);
        layout->setSpacing(2);
        layout->addWidget(header_);
        layout->addWidget(lines_, 1);
        view.installEventFilter(this);
        view.window()->installEventFilter(this);
        clock_.start();
        connect(&timer_, &QTimer::timeout, this, [this] { refresh(); });
        timer_.start(500);
        refresh();
        place();
        showOverTheGame(*this);
    }

protected:
    bool eventFilter(QObject *watched, QEvent *event) override {
        if (event->type() == QEvent::Move || event->type() == QEvent::Resize) place();
        return QWidget::eventFilter(watched, event);
    }

private:
    /// Over the top 40% of the game, its full width.
    void place() {
        if (view_.isNull()) return;
        setGeometry(QRect(view_->mapToGlobal(QPoint(0, 0)),
                          QSize(view_->width(), qMax(1, qRound(view_->height() * 0.4)))));
    }

    void refresh() {
        readConsole();
        if (view_.isNull()) return;
        // The rate is the change in frames delivered since the last sample.
        const std::uint64_t delivered = view_->framesDelivered();
        const qint64 now = clock_.nsecsElapsed();
        if (sampled_at_ >= 0 && now > sampled_at_ && delivered >= sampled_frames_) {
            frames_per_second_ = static_cast<double>(delivered - sampled_frames_) * 1e9 /
                                 static_cast<double>(now - sampled_at_);
        }
        sampled_frames_ = delivered;
        sampled_at_ = now;
        header_->setText(frames_per_second_.has_value()
                             ? QString::asprintf("%.1f frames/s", *frames_per_second_)
                             : QStringLiteral("-- frames/s"));
    }

    /// What content wrote since the last read. New lines can arrive between
    /// the measure and the copy, so a copy refused as too small is measured again.
    void readConsole() {
        std::size_t length = 0;
        MigoResult result = migo_session_copy_console_logs(session_, cursor_, nullptr, 0, &length);
        for (int attempt = 0; attempt < 4 && result == MIGO_ERROR_INVALID_ARGUMENT; ++attempt) {
            buffer_.resize(static_cast<qsizetype>(length + 1));
            result = migo_session_copy_console_logs(session_, cursor_, buffer_.data(),
                                                    static_cast<std::size_t>(buffer_.size()), &length);
        }
        if (result != MIGO_OK) return;
        const QJsonObject read =
            QJsonDocument::fromJson(QByteArray::fromRawData(buffer_.constData(), static_cast<qsizetype>(length)))
                .object();
        const QJsonArray logs = read.value(QStringLiteral("logs")).toArray();
        for (const QJsonValue &value : logs) {
            const QJsonObject log = value.toObject();
            const int level = log.value(QStringLiteral("l")).toInt(1);
            QString line = QStringLiteral("%1 %2").arg(mark(level), log.value(QStringLiteral("m")).toString());
            line = line.toHtmlEscaped().replace(QLatin1Char('\n'), QStringLiteral("<br>"));
            lines_->appendHtml(QStringLiteral("<span style=\"color:%1\">%2</span>").arg(colour(level), line));
        }
        if (!logs.isEmpty()) lines_->ensureCursorVisible();
        cursor_ = static_cast<std::uint64_t>(
            read.value(QStringLiteral("cursor")).toInteger(static_cast<qint64>(cursor_)));
    }

    static QString mark(int level) {
        switch (level) {
            case 3: return QStringLiteral("E");
            case 2: return QStringLiteral("W");
            case 0: return QStringLiteral("D");
            default: return QStringLiteral("I");
        }
    }

    static QString colour(int level) {
        switch (level) {
            case 3: return QStringLiteral("#f0544f");
            case 2: return QStringLiteral("#ffb84d");
            case 0: return QStringLiteral("#9e9e9e");
            default: return QStringLiteral("#e0e0e0");
        }
    }

    MigoSession *const session_;
    QPointer<MigoQtX11SurfaceView> view_;
    QLabel *const header_;
    QPlainTextEdit *const lines_;
    QTimer timer_;
    QElapsedTimer clock_;
    QByteArray buffer_;
    std::uint64_t cursor_ = 0;
    std::uint64_t sampled_frames_ = 0;
    qint64 sampled_at_ = -1;
    std::optional<double> frames_per_second_;
};

}  // namespace

/// Everything the wrapper owns, kept out of the header so the public type
/// exposes no engine handles a caller could retire behind its back.
class MigoManagedSession::Impl final : public QObject {
public:
    explicit Impl(MigoManagedSession &owner) : owner_(owner) {}

    MigoManagedSession &owner_;
    QThread *gui_thread = QThread::currentThread();
    MigoSession *session = nullptr;
    std::unique_ptr<SurfaceHost> surface_host;
    QPointer<MigoQtX11SurfaceView> view;
    MigoResult last_error = MIGO_OK;
    /// Where host-service answers go; closed before the Session is destroyed.
    std::shared_ptr<MigoQtHostServiceChannel> channel;
    /// The App's own services and the services it declared, read once.
    MigoQtHostServiceProvider *app_services = nullptr;
    std::uint64_t app_mask = 0;
    std::unique_ptr<MigoQtBuiltInHostServices> built_in;
    /// Who the App is, as it was reported; reported again with the debugging aids.
    QByteArray app_id;
    bool debug_aids = false;
    QPointer<DebugPanel> debug_panel;
    /// Content's setKeepScreenOn, held for it while it asks and the Session runs.
    std::unique_ptr<ScreenAwake> screen_awake;
    /// What the system says of the battery and the network, reported to the Session as it changes.
    std::unique_ptr<BatteryReport> battery;
    std::unique_ptr<NetworkReport> network;
    /// The desktop's warnings that memory runs low, passed on to the Session.
    std::unique_ptr<MemoryMonitor> memory;
    /// The gamepads the kernel shows, announced to the Session while it has a Surface.
    std::unique_ptr<Gamepads> gamepads;
    /// The system switches reported so far (getSystemSetting).
    MigoSystemSettingFlags system_settings = MIGO_SYSTEM_SETTING_FLAG_NONE;
    /// What content was last told of its window: it starts shown and running.
    bool reported_running = true;
    bool reported_visible = true;
    /// Whether the window has been exposed yet: until it first is, being
    /// unexposed is its coming up, not its being covered.
    bool exposed_once = false;
    /// The view's window, and its native window, watched for whether content
    /// runs and shows -- from the first attach, when both are on screen.
    QPointer<QWidget> watched_widget;
    QPointer<QWindow> watched_window;
    bool closing = false;
    bool closed = false;
    /// Whether the view has attached: the Session has its host from then on.
    bool attached = false;
    /// Content asked for and not yet handed to the Session, which it is once the view attaches.
    QByteArray pending_content_id;
    QByteArray pending_entry;
    bool content_pending = false;
    bool content_requested = false;
    /// Read from engine threads inside the dispatcher, so it cannot be a plain
    /// bool: the dispatcher must refuse once teardown starts, and it is the
    /// engine that asks.
    std::atomic<bool> accepting_tasks{false};

    /// One dispatched engine task. Allocated by the dispatcher on an engine
    /// thread and consumed exactly once on the GUI thread.
    struct Task {
        MigoTaskFn function = nullptr;
        void *context = nullptr;
    };

    static MigoResult MIGO_CALL dispatch(void *dispatcher_context, MigoTaskFn task,
                                         void *task_context);

    // The C callbacks are static members rather than free functions because
    // they must name Impl, which is private to the owning class.
    static void MIGO_CALL onReady(void *user_data, MigoSession *session);
    static void MIGO_CALL onError(void *user_data, MigoSession *session, const MigoError *error);
    static void MIGO_CALL onExitRequested(void *user_data, MigoSession *session);
    static void MIGO_CALL onRequestFrame(void *user_data, MigoSession *session);
    static void MIGO_CALL onKeepScreenOn(void *user_data, MigoSession *session, std::uint8_t keep_on);
    static void MIGO_CALL onGamepadRumble(void *user_data, MigoSession *session, const MigoGamepadRumble *rumble);
    static void MIGO_CALL onHostServiceCall(void *user_data, MigoSession *session,
                                            const MigoHostServiceCall *call);

    void runTask(Task task) {
        if (task.function != nullptr) task.function(task.context);
    }

    // Signals are protected, and only a member of the owning class may emit
    // them. Impl is a nested class and has that access; the C callbacks below
    // are free functions and do not, so they go through these.
    void emitReady() { Q_EMIT owner_.contentReady(); }
    void emitFailed(MigoResult code, const QString &message) {
        Q_EMIT owner_.contentFailed(code, message);
    }
    void emitExitRequested() { Q_EMIT owner_.exitRequested(); }
    void emitClosed() { Q_EMIT owner_.sessionClosed(); }
    void emitError(MigoResult code) { Q_EMIT owner_.migoError(code); }

    void finishClose();
    MigoResult loadNow(const QByteArray &content_id, const QByteArray &entry);

    /// Content runs while its window is shown and not minimized -- nobody can
    /// bring it back without acting otherwise -- and is visible while it runs
    /// and its window is exposed: a covered window keeps its game running, as
    /// a background browser tab keeps its script running.
    void reportWindowState() {
        if (session == nullptr || closing || view.isNull()) return;
        QWidget *window = view->window();
        const bool running = window->isVisible() && !window->isMinimized();
        const QWindow *handle = window->windowHandle();
        const bool exposed = handle != nullptr && handle->isExposed();
        exposed_once = exposed_once || exposed;
        const bool visible = running && (exposed || !exposed_once);
        if (visible != reported_visible) {
            reported_visible = visible;
            (void)migo_session_set_visibility(session, visible ? 1U : 0U);
        }
        if (running != reported_running) {
            reported_running = running;
            (void)migo_session_set_lifecycle(session, running ? MIGO_LIFECYCLE_RUNNING : MIGO_LIFECYCLE_PAUSED);
        }
    }

    void watch(QWidget *window) {
        if (window != watched_widget) {
            if (!watched_widget.isNull()) watched_widget->removeEventFilter(this);
            watched_widget = window;
            window->installEventFilter(this);
        }
        QWindow *handle = window->windowHandle();
        if (handle != watched_window) {
            if (!watched_window.isNull()) watched_window->removeEventFilter(this);
            watched_window = handle;
            if (handle != nullptr) handle->installEventFilter(this);
        }
    }

    void setSystemSetting(MigoSystemSettingFlags flag, bool on) {
        const MigoSystemSettingFlags settings = on ? (system_settings | flag) : (system_settings & ~flag);
        if (settings == system_settings || session == nullptr || closing) return;
        system_settings = settings;
        (void)migo_session_set_system_settings(session, settings);
    }

    bool eventFilter(QObject *watched, QEvent *event) override {
        switch (event->type()) {
            case QEvent::WindowStateChange:
            case QEvent::Show:
            case QEvent::Hide:
            case QEvent::Expose:
                reportWindowState();
                break;
            default:
                break;
        }
        return QObject::eventFilter(watched, event);
    }

    /// What the App says about itself: who it is, and whether its debugging aids are on.
    MigoResult reportAppInfo() {
        MigoHostAppInfo app_info{};
        app_info.struct_size = static_cast<std::uint32_t>(sizeof(app_info));
        app_info.abi_version = MIGO_ABI_VERSION_1;
        app_info.app_id_utf8 = app_id.isEmpty() ? nullptr : app_id.constData();
        // The rest is the engine's: the system's language, no benchmark score.
        app_info.benchmark_level = -1;
        app_info.flags = debug_aids ? MIGO_HOST_APP_INFO_FLAG_DEBUG : 0U;
        return migo_session_set_host_app_info(session, &app_info);
    }

    /// The panel over the top of the game, from the moment the aids are turned
    /// on: what content writes from then is what it shows.
    void showDebugPanel(bool on) {
        if (on && debug_panel.isNull() && !view.isNull()) {
            debug_panel = new DebugPanel(session, *view);
        } else if (!on && !debug_panel.isNull()) {
            delete debug_panel.data();
        }
    }
};

MigoResult MigoManagedSession::Impl::loadNow(const QByteArray &content_id, const QByteArray &entry) {
    MigoContentDescriptor content{};
    content.struct_size = static_cast<std::uint32_t>(sizeof(content));
    content.abi_version = MIGO_ABI_VERSION_1;
    content.content_id_utf8 = content_id.constData();
    content.entry_utf8 = entry.constData();
    const MigoResult result = migo_session_load_content(session, &content);
    if (result != MIGO_OK) {
        last_error = result;
        // Refused, the content was not loaded: it may be asked for again.
        content_requested = false;
    }
    return result;
}

MigoResult MIGO_CALL MigoManagedSession::Impl::dispatch(void *dispatcher_context, MigoTaskFn task,
                                                        void *task_context) {
    auto *impl = static_cast<Impl *>(dispatcher_context);
    if (impl == nullptr || task == nullptr) return MIGO_ERROR_INVALID_ARGUMENT;
    // Refusing hands ownership of the task back to Migo, which is the only safe
    // answer once teardown has started: a task posted now would run after the
    // Session it belongs to is gone. Dropping it silently would instead leak
    // whatever the engine attached to it.
    if (!impl->accepting_tasks.load(std::memory_order_acquire)) {
        return MIGO_ERROR_INVALID_STATE;
    }
    const Task queued{task, task_context};
    // Queued, so the engine thread returns promptly and the task runs on the
    // GUI thread with no Migo lock held -- which is what makes it legal for a
    // callback to re-enter lifecycle or detach.
    QMetaObject::invokeMethod(
        impl, [impl, queued] { impl->runTask(queued); }, Qt::QueuedConnection);
    return MIGO_OK;
}

void MIGO_CALL MigoManagedSession::Impl::onReady(void *user_data, MigoSession *) {
    if (auto *impl = static_cast<Impl *>(user_data)) impl->emitReady();
}

void MIGO_CALL MigoManagedSession::Impl::onError(void *user_data, MigoSession *,
                                                 const MigoError *error) {
    auto *impl = static_cast<Impl *>(user_data);
    if (impl == nullptr) return;
    const MigoResult code = error != nullptr ? error->code : MIGO_ERROR_INTERNAL;
    QString message;
    if (error != nullptr && error->message_utf8 != nullptr) {
        message = QString::fromUtf8(error->message_utf8, static_cast<qsizetype>(
                                                              error->message_length));
    }
    impl->emitFailed(code, message);
}

void MIGO_CALL MigoManagedSession::Impl::onExitRequested(void *user_data, MigoSession *) {
    if (auto *impl = static_cast<Impl *>(user_data)) impl->emitExitRequested();
}

void MIGO_CALL MigoManagedSession::Impl::onRequestFrame(void *user_data, MigoSession *) {
    auto *impl = static_cast<Impl *>(user_data);
    if (impl == nullptr || impl->view.isNull()) return;
    // The Managed shape owns the callback table, so it is this wrapper that
    // installs the frame request and forwards it. In the Bound shape the App
    // does exactly this itself -- the difference between the two shapes is who
    // owns the table, not how frames are paced.
    (void)impl->view->requestFrame();
}

void MIGO_CALL MigoManagedSession::Impl::onKeepScreenOn(void *user_data, MigoSession *, std::uint8_t keep_on) {
    auto *impl = static_cast<Impl *>(user_data);
    if (impl == nullptr || impl->screen_awake == nullptr || impl->view.isNull()) return;
    impl->screen_awake->keepAwake(keep_on != 0, static_cast<std::uint64_t>(impl->view->window()->winId()));
}

void MIGO_CALL MigoManagedSession::Impl::onGamepadRumble(void *user_data, MigoSession *,
                                                         const MigoGamepadRumble *rumble) {
    auto *impl = static_cast<Impl *>(user_data);
    if (impl == nullptr || rumble == nullptr || impl->gamepads == nullptr) return;
    impl->gamepads->rumble(*rumble);
}

void MIGO_CALL MigoManagedSession::Impl::onHostServiceCall(void *user_data, MigoSession *,
                                                           const MigoHostServiceCall *call) {
    auto *impl = static_cast<Impl *>(user_data);
    if (impl == nullptr || call == nullptr || impl->channel == nullptr) return;
    // The payload is borrowed for this callback only: read now.
    const QJsonObject request =
        QJsonDocument::fromJson(QByteArray(call->payload_json_utf8, static_cast<qsizetype>(call->payload_length)))
            .object();
    MigoQtHostServiceReply reply = impl->channel->reply(call->call_id);
    const std::uint64_t bit = hostServiceBit(call->service);
    if (impl->app_services != nullptr && (impl->app_mask & bit) != 0) {
        impl->app_services->call(call->service, call->method, request, std::move(reply));
    } else if (impl->built_in != nullptr && (impl->built_in->services() & bit) != 0) {
        impl->built_in->call(call->service, call->method, request, std::move(reply));
    } else {
        // Declared at install, gone since: the built-in services go with the view.
        (void)reply.fail(QStringLiteral("not supported"));
    }
}

void MigoManagedSession::Impl::finishClose() {
    // No answer may reach the Session once it is destroyed, and what the
    // built-in services show goes with the game, as does the panel reading it.
    if (channel != nullptr) channel->close();
    built_in.reset();
    showDebugPanel(false);
    // The Session's request ends with it, whatever it last said, and nothing
    // more is reported to it.
    screen_awake.reset();
    battery.reset();
    network.reset();
    memory.reset();
    gamepads.reset();
    if (session != nullptr) {
        // Refuses while an attachment is live, a transition is running, or any
        // release is pending, so reaching here means all three are settled.
        const MigoResult result = migo_session_destroy(session);
        if (result != MIGO_OK) {
            last_error = result;
            emitError(result);
            return;
        }
        session = nullptr;
    }
    closed = true;
    closing = false;
    emitClosed();
}

MigoManagedSession::MigoManagedSession(MigoEngine &engine, QWidget &parent, QObject *object_parent)
    : MigoManagedSession(engine, parent, Options{}, object_parent) {}

MigoManagedSession::MigoManagedSession(MigoEngine &engine, QWidget &parent, const Options &options,
                                       QObject *object_parent)
    : QObject(object_parent), impl_(std::make_unique<Impl>(*this)) {
    MigoSessionConfig config{};
    config.struct_size = static_cast<std::uint32_t>(sizeof(config));
    config.abi_version = MIGO_ABI_VERSION_1;
    // NUL-terminated, as QByteArray always is; empty is the C ABI's NULL.
    config.network_policy_json_utf8 =
        options.networkPolicyJson.isEmpty() ? nullptr : options.networkPolicyJson.constData();
    config.launch_options_json_utf8 =
        options.launchOptionsJson.isEmpty() ? nullptr : options.launchOptionsJson.constData();

    const MigoResult created = migo_session_create(&engine, &config, &impl_->session);
    if (created != MIGO_OK || impl_->session == nullptr) {
        impl_->last_error = created != MIGO_OK ? created : MIGO_ERROR_INTERNAL;
        impl_->session = nullptr;
        return;
    }

    MigoHostCallbacks callbacks{};
    callbacks.struct_size = static_cast<std::uint32_t>(sizeof(callbacks));
    callbacks.abi_version = MIGO_ABI_VERSION_1;
    callbacks.user_data = impl_.get();
    callbacks.dispatcher_data = impl_.get();
    callbacks.dispatch = &Impl::dispatch;
    callbacks.on_ready = &Impl::onReady;
    callbacks.on_error = &Impl::onError;
    callbacks.on_exit_requested = &Impl::onExitRequested;
    callbacks.on_request_frame = &Impl::onRequestFrame;
    callbacks.on_keep_screen_on = &Impl::onKeepScreenOn;
    callbacks.on_gamepad_rumble = &Impl::onGamepadRumble;
    impl_->screen_awake = std::make_unique<ScreenAwake>(QDBusConnection::sessionBus());
    // The three soft-keyboard callbacks are deliberately absent, and absent
    // together: installing a subset is refused, and installing all three would
    // claim a capability this wrapper cannot honour. The common mini-game platform's soft keyboard
    // requires the host to own a text field and report its whole current value;
    // a desktop host has a physical keyboard, and content already receives key
    // events and IME composition directly through the view. Claiming the
    // capability and then not maintaining a field would be worse than not
    // having it -- content's migo.showKeyboard correctly reports failure instead.

    // The view is made before the table is installed because the built-in
    // services stand over it; nothing reaches either until content runs.
    impl_->surface_host = std::make_unique<SurfaceHost>(impl_->session);
    impl_->view = new MigoQtX11SurfaceView(*impl_->surface_host, parent);
    impl_->channel = MigoQtHostServiceChannel::open(impl_->session);
    impl_->built_in = std::make_unique<MigoQtBuiltInHostServices>(*impl_->view, impl_->channel, options.videos);
    impl_->app_services = options.appServices;
    impl_->app_mask = options.appServices != nullptr ? options.appServices->services() : 0;
    const std::uint64_t declared = impl_->app_mask | impl_->built_in->services();
    if (declared != 0) {
        callbacks.on_host_service_call = &Impl::onHostServiceCall;
        callbacks.host_services = declared;
    }

    MigoResult installed = migo_session_set_host_callbacks(impl_->session, &callbacks);
    // Who the host is, as content reads it. A Linux App has no identifier the
    // system hands out; it names itself, and its desktop file name is that name.
    impl_->app_id = (options.appId.isEmpty() ? QGuiApplication::desktopFileName() : options.appId).toUtf8();
    if (installed == MIGO_OK && !impl_->app_id.isEmpty()) installed = impl_->reportAppInfo();
    if (installed != MIGO_OK) {
        impl_->last_error = installed;
        impl_->channel->close();
        impl_->built_in.reset();
        delete impl_->view.data();
        impl_->surface_host.reset();
        (void)migo_session_destroy(impl_->session);
        impl_->session = nullptr;
        return;
    }
    impl_->accepting_tasks.store(true, std::memory_order_release);

    // The battery and the network, as the system says they are and each time
    // they change; the Session keeps what it was told.
    Impl *impl = impl_.get();
    impl_->battery = std::make_unique<BatteryReport>(
        QDBusConnection::systemBus(), [impl](const BatteryReport::State &state) {
            if (impl->session == nullptr || impl->closing) return;
            (void)migo_session_set_battery_status(impl->session, state.level_percent, state.flags);
        });
    impl_->network = std::make_unique<NetworkReport>(
        QDBusConnection::systemBus(), [impl](const NetworkReport::State &state) {
            if (impl->session == nullptr || impl->closing) return;
            (void)migo_session_set_network_status(impl->session, state.type, state.connected ? 1U : 0U);
            impl->setSystemSetting(MIGO_SYSTEM_SETTING_FLAG_WIFI_ENABLED, state.wifi_enabled);
        });

    // What the system grants the App. An App outside a sandbox is granted all
    // the system could gate -- its files, its devices, its notifications --
    // for the user who runs it; inside one (Flatpak, Snap) the portals decide
    // as content asks, which is not determined until then. Location is
    // GeoClue's agent's to decide, asked when content asks for
    // scope.userLocation (MigoQtBuiltInHostServices), and not reported here.
    MigoAppAuthorizeSetting granted{};
    granted.struct_size = static_cast<std::uint32_t>(sizeof(granted));
    granted.abi_version = MIGO_ABI_VERSION_1;
    const bool sandboxed = insideSandbox();
    if (!sandboxed) {
        for (std::uint8_t *field : {&granted.album, &granted.bluetooth, &granted.camera, &granted.microphone,
                                    &granted.notification, &granted.notification_alert, &granted.notification_badge,
                                    &granted.notification_sound, &granted.phone_calendar}) {
            *field = MIGO_AUTHORIZATION_AUTHORIZED;
        }
    }
    (void)migo_session_set_app_authorize_setting(impl_->session, &granted);

    // Memory warnings where GLib's memory monitor hears them: inside a sandbox
    // the portal relays them, outside it low-memory-monitor sends them itself.
    impl_->memory = std::make_unique<MemoryMonitor>(
        sandboxed ? QDBusConnection::sessionBus() : QDBusConnection::systemBus(),
        sandboxed ? MemoryMonitor::Source::Portal : MemoryMonitor::Source::LowMemoryMonitor,
        [impl](MigoMemoryWarningLevel level) {
            if (impl->session == nullptr || impl->closing) return;
            (void)migo_session_post_memory_warning(impl->session, level);
        });

    // The gamepads, told to the Session while it has a Surface to deliver to:
    // before its first attach, and between a detach and the next, there is
    // nobody to tell, and the pads are announced again as it attaches.
    impl_->gamepads = std::make_unique<Gamepads>(Gamepads::Sink{
        [impl](const MigoGamepadInfo &info, bool connected) {
            if (impl->session == nullptr || impl->closing || !impl->attached) return;
            (void)migo_session_set_gamepad_connected(impl->session, &info, connected ? 1U : 0U);
        },
        [impl](const MigoGamepadStateEvent &sample) {
            if (impl->session == nullptr || impl->closing || !impl->attached) return;
            (void)migo_session_send_gamepad_state(impl->session, &sample);
        }});
    impl_->gamepads->watchSystem();

    connect(impl_->built_in.get(), &MigoQtBuiltInHostServices::debugAidsRequested, this,
            [this](bool on) { (void)setDebugAids(on); });
    connect(impl_->view.data(), &MigoQtX11SurfaceView::surfaceError, this,
            &MigoManagedSession::migoError);
    connect(impl_->view.data(), &MigoQtX11SurfaceView::inputRejected, this,
            &MigoManagedSession::migoError);
    // The view already polls its own release observer and reports the result.
    // A second poller here would race it: whichever asked first would consume
    // the transition and the other would see MIGO_ERROR_INVALID_STATE, so the
    // wrapper waits for the view's answer instead of asking again.
    connect(impl_->view.data(), &MigoQtX11SurfaceView::surfaceReleased, this,
            [this](quint64) { impl_->finishClose(); });
    // The Session has its host once the view first attaches; content asked for
    // before then is loaded now.
    connect(impl_->view.data(), &MigoQtX11SurfaceView::surfaceAttached, this, [this](quint64) {
        impl_->attached = true;
        // A pad the Session has already heard of answers that it has; one that
        // came while nothing was attached is heard of now.
        if (impl_->gamepads != nullptr) impl_->gamepads->announce();
        // The window is on screen by now, with its native window to watch for
        // exposure; a view reparented since its last attach has another.
        impl_->watch(impl_->view->window());
        impl_->reportWindowState();
        if (!impl_->content_pending || impl_->closing) return;
        impl_->content_pending = false;
        const QByteArray content_id = std::exchange(impl_->pending_content_id, QByteArray());
        const QByteArray entry = std::exchange(impl_->pending_entry, QByteArray());
        const MigoResult result = impl_->loadNow(content_id, entry);
        if (result != MIGO_OK) {
            impl_->emitFailed(result, QStringLiteral("the content was not loaded: migo_session_load_content refused it"));
        }
    });
}

MigoManagedSession::~MigoManagedSession() {
    impl_->accepting_tasks.store(false, std::memory_order_release);
    if (impl_->session != nullptr) {
        qFatal(
            "MigoManagedSession destroyed before its Session was closed; call close() and wait "
            "for sessionClosed()");
    }
}

bool MigoManagedSession::isValid() const noexcept { return impl_->session != nullptr; }

MigoResult MigoManagedSession::lastError() const noexcept { return impl_->last_error; }

MigoQtX11SurfaceView *MigoManagedSession::view() const noexcept { return impl_->view.data(); }

bool MigoManagedSession::isClosed() const noexcept { return impl_->closed; }

bool MigoManagedSession::debugAids() const noexcept { return impl_->debug_aids; }

MigoResult MigoManagedSession::setDebugAids(bool on) {
    if (QThread::currentThread() != impl_->gui_thread) return MIGO_ERROR_WRONG_THREAD;
    if (impl_->session == nullptr || impl_->closing) return MIGO_ERROR_INVALID_STATE;
    if (on == impl_->debug_aids) return MIGO_OK;
    impl_->debug_aids = on;
    const MigoResult reported = impl_->reportAppInfo();
    if (reported != MIGO_OK) impl_->last_error = reported;
    impl_->showDebugPanel(on);
    Q_EMIT debugAidsChanged(on);
    return reported;
}

MigoResult MigoManagedSession::loadContent(const QString &content_id, const QString &entry) {
    if (QThread::currentThread() != impl_->gui_thread) return MIGO_ERROR_WRONG_THREAD;
    if (impl_->session == nullptr || impl_->closing) return MIGO_ERROR_INVALID_STATE;
    if (impl_->content_requested) return MIGO_ERROR_INVALID_STATE;
    impl_->content_requested = true;
    if (impl_->attached) return impl_->loadNow(content_id.toUtf8(), entry.toUtf8());
    impl_->pending_content_id = content_id.toUtf8();
    impl_->pending_entry = entry.toUtf8();
    impl_->content_pending = true;
    return MIGO_OK;
}

MigoResult MigoManagedSession::close() {
    if (QThread::currentThread() != impl_->gui_thread) return MIGO_ERROR_WRONG_THREAD;
    if (impl_->closed) return MIGO_OK;
    if (impl_->closing) return MIGO_OK;
    impl_->closing = true;
    // Stop accepting engine tasks before anything is torn down: a task accepted
    // now could otherwise run against a Session that is already going away.
    impl_->accepting_tasks.store(false, std::memory_order_release);

    if (impl_->view.isNull() || impl_->view->surfaceState() == SurfaceState::Detached) {
        impl_->finishClose();
        return MIGO_OK;
    }
    if (impl_->view->surfaceState() == SurfaceState::Attached) {
        const MigoResult began = impl_->view->beginDetach();
        if (began != MIGO_OK) {
            impl_->closing = false;
            impl_->accepting_tasks.store(true, std::memory_order_release);
            impl_->last_error = began;
            return began;
        }
    }
    // From here the view's own release polling drives the rest; `finishClose`
    // runs from its `surfaceReleased` signal.
    return MIGO_OK;
}

}  // namespace migo::linux_host::qt6

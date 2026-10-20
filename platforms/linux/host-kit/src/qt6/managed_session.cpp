#include <migo/linux/qt6/managed_session.hpp>

#include <QJsonDocument>
#include <QJsonObject>
#include <QMetaObject>
#include <QPointer>
#include <QThread>

#include <atomic>
#include <utility>

namespace migo::linux_host::qt6 {

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
    bool closing = false;
    bool closed = false;
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
};

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
    // built-in services show goes with the game.
    if (channel != nullptr) channel->close();
    built_in.reset();
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
    impl_->built_in = std::make_unique<MigoQtBuiltInHostServices>(*impl_->view);
    impl_->app_services = options.appServices;
    impl_->app_mask = options.appServices != nullptr ? options.appServices->services() : 0;
    const std::uint64_t declared = impl_->app_mask | impl_->built_in->services();
    if (declared != 0) {
        callbacks.on_host_service_call = &Impl::onHostServiceCall;
        callbacks.host_services = declared;
    }

    const MigoResult installed = migo_session_set_host_callbacks(impl_->session, &callbacks);
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

MigoResult MigoManagedSession::loadContent(const QString &content_id, const QString &entry) {
    if (QThread::currentThread() != impl_->gui_thread) return MIGO_ERROR_WRONG_THREAD;
    if (impl_->session == nullptr || impl_->closing) return MIGO_ERROR_INVALID_STATE;

    const QByteArray id_utf8 = content_id.toUtf8();
    const QByteArray entry_utf8 = entry.toUtf8();
    MigoContentDescriptor content{};
    content.struct_size = static_cast<std::uint32_t>(sizeof(content));
    content.abi_version = MIGO_ABI_VERSION_1;
    content.content_id_utf8 = id_utf8.constData();
    content.entry_utf8 = entry_utf8.constData();

    const MigoResult result = migo_session_load_content(impl_->session, &content);
    if (result != MIGO_OK) impl_->last_error = result;
    return result;
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

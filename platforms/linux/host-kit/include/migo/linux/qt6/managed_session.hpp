#ifndef MIGO_LINUX_QT6_MANAGED_SESSION_HPP_
#define MIGO_LINUX_QT6_MANAGED_SESSION_HPP_

#include <migo/linux/qt6/host_services.hpp>
#include <migo/linux/qt6/x11_surface_view.hpp>
#include <migo/linux/surface_host.hpp>

#include <QByteArray>
#include <QObject>
#include <QString>

#include <memory>

namespace migo::linux_host::qt6 {

/// A Qt 6 Widgets container that owns one Migo Session.
///
/// This is the Managed half of the Host Kit's two ownership shapes. The Bound
/// half is `MigoQtX11SurfaceView` driven by an App-owned `SurfaceHost`: there
/// the App owns the Session, keeps it alive across views, and installs its own
/// callback table. Here the Host Kit owns the Session, its callbacks and its
/// view, and retires all three on request. The two are separate types rather
/// than one type with a flag, because the difference is who is responsible for
/// an asynchronous teardown -- a question a boolean cannot answer.
///
/// What this does NOT own is the `MigoEngine`: one App may run several Sessions
/// on one engine (preloading, multiple windows), and a wrapper that created its
/// own would make that impossible. The engine, the parent widget and the
/// QApplication must each outlive this object.
///
/// Teardown is the C ABI's three steps and cannot be shortened: begin the
/// detach, wait for the release observer to report RELEASED, then destroy the
/// Session. `close()` starts that and returns immediately; `sessionClosed`
/// arrives once the Session is really gone. Destroying this object while it is
/// still running is a programming error and terminates, for the same reason the
/// view does: silently dropping the only release handle leaves the host unable
/// to destroy its own window safely, and no later call can recover it.
///
/// The whole public API is confined to the GUI thread, which is the Session's
/// owner thread. Engine callbacks arrive on engine threads and are marshalled
/// here before any of them runs.
class MigoManagedSession final : public QObject {
    Q_OBJECT

public:
    /// What the Session is created with. Everything here is fixed for the
    /// Session's life: the C ABI takes it at creation and when the callback
    /// table is installed, both of which happen in the constructor.
    struct Options {
        /// The network content may reach, as `MigoSessionConfig`'s
        /// `network_policy_json_utf8` takes it. Empty grants none: the engine
        /// gives content no network a host did not.
        QByteArray networkPolicyJson;
        /// What the game is launched with, as `MigoSessionConfig`'s
        /// `launch_options_json_utf8` takes it. Empty launches it with the
        /// engine's defaults.
        QByteArray launchOptionsJson;
        /// The host services the App answers itself -- its adverts, payments,
        /// sign-in -- or null. The services it names are its, including any
        /// the Host Kit would answer; the Host Kit answers the rest it can
        /// with Qt's own (`MigoQtBuiltInHostServices`). It must outlive the
        /// Session, and is called on the GUI thread.
        MigoQtHostServiceProvider *appServices = nullptr;
    };

    /// Constructs the Session and its view, and installs the callback table.
    ///
    /// The callback table can be installed only once per Session and only
    /// before the first attach, so it happens here rather than being exposed:
    /// a later install would be refused, and a host that could try would have a
    /// window in which queued callbacks observed replaced function pointers.
    MigoManagedSession(MigoEngine &engine, QWidget &parent, const Options &options,
                       QObject *object_parent = nullptr);
    /// The same, with no network, the default launch and no services of the
    /// App's own.
    MigoManagedSession(MigoEngine &engine, QWidget &parent, QObject *object_parent = nullptr);
    ~MigoManagedSession() override;

    MigoManagedSession(const MigoManagedSession &) = delete;
    MigoManagedSession &operator=(const MigoManagedSession &) = delete;

    /// Whether construction produced a usable Session.
    [[nodiscard]] bool isValid() const noexcept;

    /// The result of the failing construction step, or MIGO_OK.
    [[nodiscard]] MigoResult lastError() const noexcept;

    /// The view to place in the App's layout. Never null while this object is
    /// alive; owned by the parent widget passed to the constructor.
    [[nodiscard]] MigoQtX11SurfaceView *view() const noexcept;

    /// Start evaluating the named content.
    ///
    /// Content needs the view's Surface: the Session has nothing to run it on
    /// until the view first attaches, which happens once Qt has put its window
    /// on screen. Called before that, the content is loaded then, and this
    /// returns MIGO_OK; called after, it is loaded now. Argument and state
    /// problems found now are reported through the return value; a load the
    /// library refuses when the view attaches, and a failure raised once the
    /// content is running, arrive through `contentFailed`, because by then this
    /// call's stack is gone. A Session loads content once: a second call while
    /// one is waiting or loaded returns MIGO_ERROR_INVALID_STATE.
    [[nodiscard]] MigoResult loadContent(const QString &content_id, const QString &entry);

    /// Begin the asynchronous teardown. Safe to call more than once.
    ///
    /// Returns MIGO_OK when teardown has started or already finished. Does not
    /// block: `sessionClosed` reports completion.
    [[nodiscard]] MigoResult close();

    /// Whether the Session has been destroyed.
    [[nodiscard]] bool isClosed() const noexcept;

Q_SIGNALS:
    /// Content finished loading and is running.
    void contentReady();
    /// Content raised an error after it started running.
    void contentFailed(MigoResult code, const QString &message);
    /// Content asked to exit.
    void exitRequested();
    /// The Session has been destroyed and this object can be deleted.
    void sessionClosed();
    /// A Surface or input call was refused. Forwarded from the view so an App
    /// using the Managed shape does not have to connect to it separately.
    void migoError(MigoResult code);

private:
    class Impl;
    const std::unique_ptr<Impl> impl_;
};

}  // namespace migo::linux_host::qt6

#endif  // MIGO_LINUX_QT6_MANAGED_SESSION_HPP_

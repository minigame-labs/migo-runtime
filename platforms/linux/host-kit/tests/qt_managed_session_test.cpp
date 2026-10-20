#include "fake_migo.hpp"

#include <migo/linux/qt6/managed_session.hpp>

#include <QApplication>
#include <QSignalSpy>
#include <QThread>
#include <QtTest>

#include <memory>

using migo::linux_host::SurfaceState;
using migo::linux_host::qt6::MigoManagedSession;

namespace {

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

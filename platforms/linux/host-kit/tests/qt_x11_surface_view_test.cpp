#include "fake_migo.hpp"

#include <migo/linux/qt6/x11_surface_view.hpp>

#include <QApplication>
#include <QScreen>
#include <QSignalSpy>
#include <QVBoxLayout>
#include <QWindow>
#include <QtGui/qguiapplication_platform.h>
#include <QtTest>

#include <X11/Xlib.h>

#include <thread>

using migo::linux_host::SurfaceState;
using migo::linux_host::SurfaceHost;
using migo::linux_host::qt6::MigoQtX11SurfaceView;

class QtX11SurfaceViewTest final : public QObject {
    Q_OBJECT

private slots:
    void init() { fake_migo::reset(); }

    void native_child_does_not_force_its_host_ancestors_native() {
        SurfaceHost surface_host(fake_migo::session());
        QWidget top_level;
        QWidget host_container(&top_level);

        MigoQtX11SurfaceView view(surface_host, host_container);

        QCOMPARE(view.testAttribute(Qt::WA_NativeWindow), true);
        QCOMPARE(view.testAttribute(Qt::WA_DontCreateNativeAncestors), true);
        QCOMPARE(host_container.testAttribute(Qt::WA_NativeWindow), false);
    }

    /// The engine binds the window as the X server has it, so the view attaches
    /// once its window is mapped at the widget's size. Qt sends a widget its show
    /// event before it maps the native window and applies its geometry, and a
    /// view a layout places -- every Managed Session's -- was bound while its
    /// window was still unmapped at a child's default 100x30: the game drew into
    /// a 100x30 canvas behind a full-size view.
    void attaches_once_its_window_is_mapped_at_its_size() {
        if (!requirePlatform(QGuiApplication::platformName() == QStringLiteral("xcb"),
                             "xcb-only test", __FILE__, __LINE__)) {
            return;
        }

        SurfaceHost surface_host(fake_migo::session());
        QWidget container;
        auto *layout = new QVBoxLayout(&container);
        layout->setContentsMargins(0, 0, 0, 0);
        MigoQtX11SurfaceView view(surface_host, container);
        layout->addWidget(&view);
        container.resize(320, 180);
        // Retired on the way out, so a failed assertion reports itself rather
        // than the view's destroyed-before-RELEASED abort.
        struct Retire {
            MigoQtX11SurfaceView &view;
            ~Retire() {
                if (view.surfaceState() == SurfaceState::Attached) (void)view.beginDetach();
                if (view.surfaceState() == SurfaceState::Retiring) {
                    fake_migo::set_release_status(view.generation(), MIGO_SURFACE_RELEASE_RELEASED);
                    bool released = false;
                    (void)view.pollDetach(&released);
                }
            }
        } retire{view};

        XWindowAttributes at_attach{};
        connect(&view, &MigoQtX11SurfaceView::surfaceAttached, &view, [&](quint64) {
            auto *x11 = qGuiApp->nativeInterface<QNativeInterface::QX11Application>();
            XGetWindowAttributes(x11->display(), static_cast<::Window>(view.winId()), &at_attach);
        });
        container.show();
        QTRY_COMPARE(fake_migo::calls().attach, 1);

        const qreal ratio = view.devicePixelRatioF();
        QCOMPARE(at_attach.width, qRound(view.width() * ratio));
        QCOMPARE(at_attach.height, qRound(view.height() * ratio));
        QVERIFY(at_attach.map_state != IsUnmapped);
    }

    void public_control_methods_reject_foreign_threads_without_touching_qt_state() {
        SurfaceHost surface_host(fake_migo::session());
        QWidget container;
        MigoQtX11SurfaceView view(surface_host, container);
        QSignalSpy errors(&view, &MigoQtX11SurfaceView::surfaceError);

        MigoResult attach_result = MIGO_OK;
        std::thread foreign_attach([&] { attach_result = view.attachSurface(); });
        foreign_attach.join();
        QCOMPARE(attach_result, MIGO_ERROR_WRONG_THREAD);
        QCOMPARE(view.lastError(), MIGO_OK);
        QCOMPARE(errors.count(), 0);
        QCOMPARE(fake_migo::calls().attach, 0);
    }

    void owned_view_control_methods_preserve_thread_and_release_ownership() {
        if (!requirePlatform(QGuiApplication::platformName() == QStringLiteral("xcb"),
                             "xcb-only test", __FILE__, __LINE__)) {
            return;
        }

        SurfaceHost surface_host(fake_migo::session());
        QWidget container;
        MigoQtX11SurfaceView view(surface_host, container);
        view.resize(320, 180);
        container.show();
        view.show();
        QTRY_COMPARE(fake_migo::calls().attach, 1);
        QCOMPARE(view.surfaceState(), SurfaceState::Attached);
        QSignalSpy errors(&view, &MigoQtX11SurfaceView::surfaceError);

        MigoResult begin_result = MIGO_OK;
        std::thread foreign_begin([&] { begin_result = view.beginDetach(); });
        foreign_begin.join();
        QCOMPARE(begin_result, MIGO_ERROR_WRONG_THREAD);
        QCOMPARE(view.lastError(), MIGO_OK);
        QCOMPARE(errors.count(), 0);
        QCOMPARE(surface_host.state(), SurfaceState::Attached);
        QCOMPARE(fake_migo::calls().begin_detach, 0);

        QCOMPARE(view.beginDetach(), MIGO_OK);
        QCOMPARE(view.isReleasePolling(), true);
        bool released = true;
        MigoResult poll_result = MIGO_OK;
        std::thread foreign_poll([&] { poll_result = view.pollDetach(&released); });
        foreign_poll.join();
        QCOMPARE(poll_result, MIGO_ERROR_WRONG_THREAD);
        QCOMPARE(released, true);
        QCOMPARE(view.isReleasePolling(), true);
        QCOMPARE(fake_migo::calls().query, 0);

        QCOMPARE(view.pollDetach(nullptr), MIGO_ERROR_INVALID_ARGUMENT);
        QCOMPARE(view.isReleasePolling(), true);
        QCOMPARE(surface_host.state(), SurfaceState::Retiring);
        QCOMPARE(fake_migo::calls().query, 0);

        fake_migo::set_query_result(MIGO_ERROR_INTERNAL);
        released = true;
        QCOMPARE(view.pollDetach(&released), MIGO_ERROR_INTERNAL);
        QCOMPARE(released, false);
        QCOMPARE(view.isReleasePolling(), false);
        QCOMPARE(surface_host.state(), SurfaceState::Retiring);

        fake_migo::set_query_result(MIGO_OK);
        fake_migo::set_release_status(view.generation(), MIGO_SURFACE_RELEASE_PENDING);
        QCOMPARE(view.pollDetach(&released), MIGO_OK);
        QCOMPARE(released, false);
        QCOMPARE(view.isReleasePolling(), true);

        fake_migo::set_release_status(view.generation(), MIGO_SURFACE_RELEASE_RELEASED);
        QCOMPARE(view.pollDetach(&released), MIGO_OK);
        QCOMPARE(released, true);
        QCOMPARE(surface_host.state(), SurfaceState::Detached);
        QCOMPARE(view.surfaceState(), SurfaceState::Detached);
    }

    void passive_view_does_not_claim_another_views_controller_attachment() {
        SurfaceHost surface_host(fake_migo::session());
        migo::linux_host::SurfaceMetrics metrics;
        metrics.width_pixels = 320;
        metrics.height_pixels = 180;
        QCOMPARE(surface_host.attach(
                     migo::linux_host::X11Target{reinterpret_cast<void *>(1), 2, 0}, metrics),
                 MIGO_OK);

        QWidget container;
        {
            MigoQtX11SurfaceView passive(surface_host, container);
            QCOMPARE(passive.surfaceState(), SurfaceState::Detached);
            QCOMPARE(passive.generation(), quint64{0});
            container.show();
            passive.show();
            passive.resize(640, 360);
            QCoreApplication::processEvents();
            QCOMPARE(fake_migo::calls().update, 0);
            passive.close();
        }

        QCOMPARE(surface_host.begin_detach(), MIGO_OK);
        fake_migo::set_release_status(surface_host.generation(),
                                      MIGO_SURFACE_RELEASE_RELEASED);
        bool released = false;
        QCOMPARE(surface_host.poll_release(&released), MIGO_OK);
        QCOMPARE(released, true);
    }

    void xcb_view_attaches_the_widget_native_window() {
        if (!requirePlatform(QGuiApplication::platformName() == QStringLiteral("xcb"),
                             "xcb-only test", __FILE__, __LINE__)) {
            return;
        }

        SurfaceHost surface_host(fake_migo::session());
        QWidget container;
        MigoQtX11SurfaceView view(surface_host, container);
        view.resize(320, 180);
        QSignalSpy attached(&view, &MigoQtX11SurfaceView::surfaceAttached);
        container.show();
        view.show();

        QTRY_COMPARE(fake_migo::calls().attach, 1);
        QCOMPARE(attached.count(), 1);
        QCOMPARE(view.isWindow(), false);
        QCOMPARE(view.parentWidget(), &container);
        QCOMPARE(view.surfaceState(), SurfaceState::Attached);
        QCOMPARE(fake_migo::last_surface().platform_kind, MIGO_PLATFORM_X11_WINDOW);
        QCOMPARE(fake_migo::last_x11().display != nullptr, true);
        QCOMPARE(fake_migo::last_x11().window != uintptr_t{0}, true);
        QCOMPARE(fake_migo::last_surface().width_pixels,
                 static_cast<uint32_t>(qRound(view.width() * view.devicePixelRatioF())));
        QCOMPARE(fake_migo::last_surface().height_pixels,
                 static_cast<uint32_t>(qRound(view.height() * view.devicePixelRatioF())));

        retire(view);
    }

    void resize_updates_physical_metrics_without_reattaching() {
        if (!requirePlatform(QGuiApplication::platformName() == QStringLiteral("xcb"),
                             "xcb-only test", __FILE__, __LINE__)) {
            return;
        }

        SurfaceHost surface_host(fake_migo::session());
        QWidget container;
        MigoQtX11SurfaceView view(surface_host, container);
        view.resize(200, 100);
        container.show();
        view.show();
        QTRY_COMPARE(fake_migo::calls().attach, 1);

        const int updates_before_resize = fake_migo::calls().update;
        view.resize(320, 180);
        view.resize(480, 270);
        view.resize(640, 360);
        QTRY_COMPARE(fake_migo::calls().update, updates_before_resize + 1);
        QTest::qWait(20);
        QCOMPARE(fake_migo::calls().update, updates_before_resize + 1);
        QCOMPARE(fake_migo::calls().attach, 1);
        QCOMPARE(fake_migo::last_metrics().generation, view.generation());
        QCOMPARE(fake_migo::last_metrics().width_pixels,
                 static_cast<uint32_t>(qRound(view.width() * view.devicePixelRatioF())));
        QCOMPARE(fake_migo::last_metrics().height_pixels,
                 static_cast<uint32_t>(qRound(view.height() * view.devicePixelRatioF())));

        retire(view);
    }

    void reattach_restores_screen_change_metrics_delivery() {
        if (!requirePlatform(QGuiApplication::platformName() == QStringLiteral("xcb"),
                             "xcb-only test", __FILE__, __LINE__)) {
            return;
        }

        SurfaceHost surface_host(fake_migo::session());
        QWidget container;
        MigoQtX11SurfaceView view(surface_host, container);
        view.resize(320, 180);
        container.show();
        view.show();
        QTRY_COMPARE(fake_migo::calls().attach, 1);

        QWindow *const first_window = view.windowHandle();
        QVERIFY(first_window != nullptr);
        // Model the disconnection Qt performs when reparenting recreates a
        // platform window. A boolean "connected once" flag cannot distinguish
        // this state from a live connection on the replacement QWindow.
        QVERIFY(QObject::disconnect(first_window, SIGNAL(screenChanged(QScreen *)),
                                    &view, nullptr));
        retire(view);

        QCOMPARE(view.attachSurface(), MIGO_OK);
        QCOMPARE(view.generation(), quint64{2});
        const int updates_before_screen_change = fake_migo::calls().update;
        QWindow *const current_window = view.windowHandle();
        QVERIFY(current_window != nullptr);
        QVERIFY(QMetaObject::invokeMethod(current_window, "screenChanged",
                                          Qt::DirectConnection,
                                          Q_ARG(QScreen *, view.screen())));
        QTRY_COMPARE(fake_migo::calls().update, updates_before_screen_change + 1);

        retire(view);
    }

    void close_waits_for_release_before_destroying_the_native_surface() {
        if (!requirePlatform(QGuiApplication::platformName() == QStringLiteral("xcb"),
                             "xcb-only test", __FILE__, __LINE__)) {
            return;
        }

        SurfaceHost surface_host(fake_migo::session());
        QWidget container;
        MigoQtX11SurfaceView view(surface_host, container);
        view.resize(320, 180);
        container.show();
        view.show();
        QTRY_COMPARE(fake_migo::calls().attach, 1);
        QSignalSpy released(&view, &MigoQtX11SurfaceView::surfaceReleased);

        fake_migo::set_release_status(view.generation(), MIGO_SURFACE_RELEASE_PENDING);
        view.close();
        QTRY_COMPARE(fake_migo::calls().begin_detach, 1);
        QCOMPARE(view.surfaceState(), SurfaceState::Retiring);
        QCOMPARE(view.isVisible(), true);
        QCOMPARE(released.count(), 0);

        fake_migo::set_release_status(view.generation(), MIGO_SURFACE_RELEASE_RELEASED);
        QTRY_COMPARE(released.count(), 1);
        QTRY_COMPARE(view.surfaceState(), SurfaceState::Detached);
        QTRY_COMPARE(view.isVisible(), false);
        QCOMPARE(fake_migo::calls().destroy_release, 1);

        view.show();
        QTRY_COMPARE(fake_migo::calls().attach, 2);
        QCOMPARE(view.generation(), quint64{2});
        retire(view);
    }

    void stalled_release_backs_off_without_discarding_the_observer() {
        if (!requirePlatform(QGuiApplication::platformName() == QStringLiteral("xcb"),
                             "xcb-only test", __FILE__, __LINE__)) {
            return;
        }

        SurfaceHost surface_host(fake_migo::session());
        QWidget container;
        MigoQtX11SurfaceView view(surface_host, container);
        view.resize(320, 180);
        container.show();
        view.show();
        QTRY_COMPARE(fake_migo::calls().attach, 1);

        QSignalSpy stalled(&view, &MigoQtX11SurfaceView::surfaceReleaseStalled);
        fake_migo::set_release_status(view.generation(), MIGO_SURFACE_RELEASE_PENDING);
        QCOMPARE(view.beginDetach(), MIGO_OK);
        QTRY_COMPARE_WITH_TIMEOUT(stalled.count(), 1, 5000);
        const int queries_at_stall = fake_migo::calls().query;
        QTest::qWait(600);
        const int stalled_queries = fake_migo::calls().query - queries_at_stall;
        QCOMPARE(stalled.count(), 1);
        QVERIFY(stalled_queries >= 1);
        QVERIFY(stalled_queries <= 4);
        QCOMPARE(view.surfaceState(), SurfaceState::Retiring);
        QCOMPARE(view.isReleasePolling(), true);
        QCOMPARE(fake_migo::calls().destroy_release, 0);

        fake_migo::set_release_status(view.generation(), MIGO_SURFACE_RELEASE_RELEASED);
        QTRY_COMPARE(view.surfaceState(), SurfaceState::Detached);
        QCOMPARE(fake_migo::calls().destroy_release, 1);
    }

    void offscreen_platform_fails_closed_without_entering_migo() {
        if (!requirePlatform(QGuiApplication::platformName() != QStringLiteral("xcb"),
                             "offscreen-only test", __FILE__, __LINE__)) {
            return;
        }

        SurfaceHost surface_host(fake_migo::session());
        QWidget container;
        MigoQtX11SurfaceView view(surface_host, container);
        view.resize(320, 180);
        QSignalSpy errors(&view, &MigoQtX11SurfaceView::surfaceError);
        container.show();
        view.show();
        QTRY_COMPARE(view.lastError(), MIGO_ERROR_UNSUPPORTED_PLATFORM);
        QCOMPARE(fake_migo::calls().attach, 0);
        QCOMPARE(view.surfaceState(), SurfaceState::Detached);
        QCOMPARE(errors.count(), 1);
    }

    void sequential_views_share_the_session_generation_source() {
        if (!requirePlatform(QGuiApplication::platformName() == QStringLiteral("xcb"),
                             "xcb-only test", __FILE__, __LINE__)) {
            return;
        }

        SurfaceHost surface_host(fake_migo::session());
        QWidget container;
        container.show();
        {
            MigoQtX11SurfaceView first(surface_host, container);
            first.resize(320, 180);
            first.show();
            QTRY_COMPARE(fake_migo::calls().attach, 1);
            QCOMPARE(first.generation(), quint64{1});
            retire(first);
        }
        {
            MigoQtX11SurfaceView second(surface_host, container);
            second.resize(320, 180);
            second.show();
            QTRY_COMPARE(fake_migo::calls().attach, 2);
            QCOMPARE(second.generation(), quint64{2});
            retire(second);
        }
    }

private:
    static bool requirePlatform(bool available, const char *reason,
                                const char *file, int line) {
        if (available) return true;
        QTest::qSkip(reason, file, line);
        return false;
    }

    static void retire(MigoQtX11SurfaceView &view) {
        const uint64_t generation = view.generation();
        QCOMPARE(view.beginDetach(), MIGO_OK);
        fake_migo::set_release_status(generation, MIGO_SURFACE_RELEASE_RELEASED);
        bool released = false;
        QCOMPARE(view.pollDetach(&released), MIGO_OK);
        QCOMPARE(released, true);
        QCOMPARE(view.surfaceState(), SurfaceState::Detached);
    }
};

QTEST_MAIN(QtX11SurfaceViewTest)

#include "qt_x11_surface_view_test.moc"

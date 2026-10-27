#include "fake_migo.hpp"
#include "x11_player.hpp"
#include "xi2_raw_motion.hpp"

#include <migo/linux/qt6/x11_surface_view.hpp>

#include <QApplication>
#include <QCursor>
#include <QInputMethodEvent>
#include <QKeyEvent>
#include <QMouseEvent>
#include <QPointingDevice>
#include <QWheelEvent>
#include <QtTest>

#include <atomic>
#include <thread>
#include <cstddef>
#include <cstdlib>
#include <memory>
#include <dlfcn.h>
#include <initializer_list>
#include <new>

#include <xcb/xcb.h>
#include <xcb/xinput.h>

using migo::linux_host::SurfaceHost;
using migo::linux_host::SurfaceState;
using migo::linux_host::qt6::MigoQtX11SurfaceView;
using migo::linux_host::qt6::RawMotion;
using migo::linux_host::qt6::rawMotion;

namespace {

/// Counts every heap allocation, so the input path can be proven to make none.
///
/// A per-event allocation is invisible in a functional test and shows up on a
/// device as jitter under a finger that never stops moving, which is the one
/// input symptom nobody can reproduce on demand.
///
/// `malloc` is interposed rather than `operator new`, because Qt's containers
/// do not use `operator new`: `QArrayData::allocate` calls `::malloc` directly,
/// so a counter that hooked only `operator new` would report zero for exactly
/// the allocations this test exists to catch -- a QString or QByteArray built
/// per event. That was the first version of this test, and it passed against a
/// deliberately injected `QByteArray` per key press.
std::atomic<std::size_t> g_allocations{0};

}  // namespace

// AddressSanitizer replaces the allocator wholesale and enters interceptors from
// inside its own startup -- `AsanInitInternal` resolves the real functions via
// `dlsym`, which allocates, before `main` and before ASan installs its SIGSEGV
// handler. A raw `malloc` interposer is therefore called during ASan init and
// faults the process before any test runs (the crash lands with no ASan report,
// because ASan's handler is not up yet). So whenever ASan is on, the counting
// MUST go through ASan's own hook, never the interposer below.
//
// The hook is declared in `<sanitizer/allocator_interface.h>`, which clang's
// compiler-rt ships but gcc's libasan does not -- yet gcc's libasan still exports
// the `__sanitizer_install_malloc_and_free_hooks` symbol. So key the decision on
// "is ASan on" alone, never on the header: an earlier version added the header to
// this condition, which sent a gcc sanitize build down the interposer path and
// into exactly the startup crash described above. When ASan is on but the header
// is absent, declare the stable hook prototype ourselves instead.
#if defined(__SANITIZE_ADDRESS__) || (defined(__has_feature) && __has_feature(address_sanitizer))
#define MIGO_COUNT_ALLOCATIONS_WITH_SANITIZER 1
#else
#define MIGO_COUNT_ALLOCATIONS_WITH_SANITIZER 0
#endif

#if MIGO_COUNT_ALLOCATIONS_WITH_SANITIZER

#if defined(__has_include) && __has_include(<sanitizer/allocator_interface.h>)
#include <sanitizer/allocator_interface.h>
#else
extern "C" int __sanitizer_install_malloc_and_free_hooks(
    void (*)(const volatile void *, std::size_t), void (*)(const volatile void *));
#endif

namespace {

void sanitizer_malloc_hook(const volatile void *, std::size_t) {
    g_allocations.fetch_add(1, std::memory_order_relaxed);
}

void sanitizer_free_hook(const volatile void *) {}

void install_allocation_counter() {
    __sanitizer_install_malloc_and_free_hooks(sanitizer_malloc_hook, sanitizer_free_hook);
}

}  // namespace

#else

extern "C" {

using MallocFn = void *(*)(std::size_t);

namespace {

/// Storage for the allocations `dlsym` itself makes while resolving `malloc`.
///
/// Resolving the real `malloc` can allocate, and that allocation would re-enter
/// this interposer before it has a function to forward to. A bump buffer
/// answers those few requests; they are never freed, which is correct for a
/// handful of bytes that live as long as the process.
alignas(std::max_align_t) unsigned char g_bootstrap[4096];
std::size_t g_bootstrap_used = 0;
MallocFn g_real_malloc = nullptr;
bool g_resolving = false;

bool from_bootstrap(void *pointer) {
    auto *bytes = static_cast<unsigned char *>(pointer);
    return bytes >= g_bootstrap && bytes < g_bootstrap + sizeof(g_bootstrap);
}

}  // namespace

void *malloc(std::size_t size) {
    if (g_real_malloc == nullptr) {
        if (g_resolving) {
            const std::size_t aligned = (size + alignof(std::max_align_t) - 1) &
                                        ~(alignof(std::max_align_t) - 1);
            if (g_bootstrap_used + aligned > sizeof(g_bootstrap)) return nullptr;
            void *block = g_bootstrap + g_bootstrap_used;
            g_bootstrap_used += aligned;
            return block;
        }
        g_resolving = true;
        g_real_malloc = reinterpret_cast<MallocFn>(dlsym(RTLD_NEXT, "malloc"));
        g_resolving = false;
        if (g_real_malloc == nullptr) return nullptr;
    }
    g_allocations.fetch_add(1, std::memory_order_relaxed);
    return g_real_malloc(size);
}

void free(void *pointer) {
    if (pointer == nullptr || from_bootstrap(pointer)) return;
    static void (*real_free)(void *) = nullptr;
    if (real_free == nullptr) {
        real_free = reinterpret_cast<void (*)(void *)>(dlsym(RTLD_NEXT, "free"));
    }
    if (real_free != nullptr) real_free(pointer);
}

}  // extern "C"

namespace {

void install_allocation_counter() {}

}  // namespace

#endif

namespace {

bool require_xcb(const char *why, const char *file, int line) {
    if (QGuiApplication::platformName() == QStringLiteral("xcb")) return true;
    QTest::qSkip(why, file, line);
    return false;
}

/// X11 hardware keycodes are evdev codes plus 8.
constexpr quint32 x11_keycode(quint32 evdev) { return evdev + 8; }
constexpr quint32 kEvdevKeyA = 30;
constexpr quint32 kEvdevArrowLeft = 105;

/// A shown view with a live attachment, retired on the way out.
///
/// Retirement belongs in a destructor rather than at the end of each test: the
/// view fail-fast destroys itself if its Surface never reached RELEASED, so a
/// test that failed an assertion half way through would abort there and report
/// the abort instead of the assertion that actually broke.
struct AttachedFixture {
    SurfaceHost host{fake_migo::session()};
    QWidget container;
    MigoQtX11SurfaceView view{host, container};

    AttachedFixture() {
        view.resize(320, 180);
        container.show();
        view.show();
    }

    ~AttachedFixture() {
        // Input failures are injected by some tests; releasing must not inherit
        // them, or the fixture would strand the very handle it exists to return.
        fake_migo::set_input_result(MIGO_OK);
        if (view.surfaceState() == SurfaceState::Attached) {
            (void)view.beginDetach();
        }
        if (view.surfaceState() == SurfaceState::Retiring) {
            fake_migo::set_release_status(view.generation(), MIGO_SURFACE_RELEASE_RELEASED);
            bool released = false;
            (void)view.pollDetach(&released);
        }
    }
};

}  // namespace

class QtX11InputTest final : public QObject {
    Q_OBJECT

private:
    static void sendMouse(QWidget &view, QEvent::Type type, Qt::MouseButton button,
                          Qt::MouseButtons buttons, QPointF position, quint64 timestamp) {
        QMouseEvent event(type, position, position, view.mapToGlobal(position), button, buttons,
                          Qt::NoModifier);
        event.setTimestamp(timestamp);
        QCoreApplication::sendEvent(&view, &event);
    }

    /// The player at the game: its window active, the view holding the
    /// keyboard, the cursor at `at` in the view.
    static bool bringPlayerTo(AttachedFixture &fixture, QPoint at) {
        fixture.container.activateWindow();
        if (!QTest::qWaitForWindowActive(&fixture.container)) return false;
        fixture.view.setFocus();
        QCursor::setPos(fixture.view.mapToGlobal(at));
        return fixture.view.hasFocus();
    }

private slots:
    void initTestCase() { install_allocation_counter(); }

    void init() { fake_migo::reset(); }

    /// A press, a drag and a release must reach content on both streams, with
    /// coordinates in CSS pixels.
    ///
    /// Qt's logical position already is physical pixels over the same device
    /// pixel ratio the view reported as `scale_factor`, so the two agree
    /// exactly. Multiplying by the ratio here -- the obvious-looking
    /// "conversion" -- is what puts every tap in the wrong place on a HiDPI
    /// screen, and this is the assertion that catches it.
    void a_press_drag_and_release_reach_both_streams_in_css_pixels() {
        if (!require_xcb("xcb-only test", __FILE__, __LINE__)) return;
        AttachedFixture fixture;
        MigoQtX11SurfaceView &view = fixture.view;
        QTRY_COMPARE(fake_migo::calls().attach, 1);
        QCOMPARE(view.surfaceState(), SurfaceState::Attached);

        sendMouse(view, QEvent::MouseButtonPress, Qt::LeftButton, Qt::LeftButton, {10.0, 20.0},
                  1000);
        sendMouse(view, QEvent::MouseMove, Qt::NoButton, Qt::LeftButton, {30.0, 40.0}, 1016);
        sendMouse(view, QEvent::MouseButtonRelease, Qt::LeftButton, Qt::NoButton, {30.0, 40.0},
                  1032);

        const auto &pointers = fake_migo::pointers();
        QCOMPARE(pointers.size(), std::size_t{3});
        QCOMPARE(pointers[0].event_type, MIGO_POINTER_EVENT_DOWN);
        QCOMPARE(pointers[0].x, 10.0F);
        QCOMPARE(pointers[0].y, 20.0F);
        QCOMPARE(pointers[0].button, 0U);
        QCOMPARE(pointers[0].timestamp_ms, 1000.0);
        QCOMPARE(pointers[1].event_type, MIGO_POINTER_EVENT_MOVE);
        QCOMPARE(pointers[1].x, 30.0F);
        QCOMPARE(pointers[2].event_type, MIGO_POINTER_EVENT_UP);

        const auto &touches = fake_migo::touches();
        QCOMPARE(touches.size(), std::size_t{3});
        QCOMPARE(touches[0].type, MIGO_TOUCH_START);
        QCOMPARE(touches[0].points.size(), std::size_t{1});
        QCOMPARE(touches[0].points[0].id, 0U);
        QCOMPARE(touches[0].points[0].x, 10.0F);
        QCOMPARE(touches[1].type, MIGO_TOUCH_MOVE);
        QCOMPARE(touches[2].type, MIGO_TOUCH_END);
        // The finger that left must say so, or content keeps it in `touches`.
        QVERIFY((touches[2].points[0].flags & MIGO_TOUCH_FLAG_REMOVED) != 0);
    }

    /// Hover must not be delivered: mini-game content has no hover concept, and a free
    /// motion stream is events no game reads.
    void motion_without_a_button_reaches_the_mouse_stream_but_not_touch() {
        if (!require_xcb("xcb-only test", __FILE__, __LINE__)) return;
        AttachedFixture fixture;
        MigoQtX11SurfaceView &view = fixture.view;
        QTRY_COMPARE(fake_migo::calls().attach, 1);
        QCOMPARE(view.surfaceState(), SurfaceState::Attached);

        sendMouse(view, QEvent::MouseMove, Qt::NoButton, Qt::NoButton, {5.0, 6.0}, 10);

        QCOMPARE(fake_migo::pointers().size(), std::size_t{1});
        QCOMPARE(fake_migo::touches().size(), std::size_t{0});
    }

    /// A move after a release must not name the button that was released.
    ///
    /// `include/migo/input.h` says a move "names the button being held", and the
    /// view used to answer from an ordinal written on press and on release and
    /// never cleared. So every hover after a right-click reported the secondary
    /// button as held, and content saw a drag that was not happening.
    ///
    /// The secondary button specifically. The primary's ordinal is 0, which is
    /// also what a hover must report, so a test that presses the primary passes
    /// whether the view reads the event or a stale field.
    void a_move_after_a_release_names_no_held_button() {
        if (!require_xcb("xcb-only test", __FILE__, __LINE__)) return;
        AttachedFixture fixture;
        MigoQtX11SurfaceView &view = fixture.view;
        QTRY_COMPARE(fake_migo::calls().attach, 1);

        sendMouse(view, QEvent::MouseButtonPress, Qt::RightButton, Qt::RightButton, {1.0, 2.0}, 1);
        sendMouse(view, QEvent::MouseMove, Qt::NoButton, Qt::RightButton, {3.0, 4.0}, 2);
        sendMouse(view, QEvent::MouseButtonRelease, Qt::RightButton, Qt::NoButton, {3.0, 4.0}, 3);
        sendMouse(view, QEvent::MouseMove, Qt::NoButton, Qt::NoButton, {5.0, 6.0}, 4);

        const auto &pointers = fake_migo::pointers();
        QCOMPARE(pointers.size(), std::size_t{4});
        QCOMPARE(pointers[0].event_type, MIGO_POINTER_EVENT_DOWN);
        QCOMPARE(pointers[0].button, 2U);
        // Held, so the drag names it.
        QCOMPARE(pointers[1].event_type, MIGO_POINTER_EVENT_MOVE);
        QCOMPARE(pointers[1].button, 2U);
        // The button that changed state.
        QCOMPARE(pointers[2].event_type, MIGO_POINTER_EVENT_UP);
        QCOMPARE(pointers[2].button, 2U);
        // Nothing is held any more.
        QCOMPARE(pointers[3].event_type, MIGO_POINTER_EVENT_MOVE);
        QCOMPARE(pointers[3].button, 0U);
    }

    /// With two buttons down, releasing one leaves the other named.
    ///
    /// The same stale-ordinal defect in its second form: remembering the button
    /// last *touched* reports the released one, so a chord that ends with the
    /// primary still down told content the secondary was held.
    void releasing_one_of_two_buttons_leaves_the_other_named() {
        if (!require_xcb("xcb-only test", __FILE__, __LINE__)) return;
        AttachedFixture fixture;
        MigoQtX11SurfaceView &view = fixture.view;
        QTRY_COMPARE(fake_migo::calls().attach, 1);

        sendMouse(view, QEvent::MouseButtonPress, Qt::LeftButton, Qt::LeftButton, {1.0, 2.0}, 1);
        sendMouse(view, QEvent::MouseButtonPress, Qt::RightButton,
                  Qt::LeftButton | Qt::RightButton, {1.0, 2.0}, 2);
        sendMouse(view, QEvent::MouseButtonRelease, Qt::RightButton, Qt::LeftButton, {1.0, 2.0}, 3);
        sendMouse(view, QEvent::MouseMove, Qt::NoButton, Qt::LeftButton, {3.0, 4.0}, 4);

        const auto &pointers = fake_migo::pointers();
        QCOMPARE(pointers.size(), std::size_t{4});
        QCOMPARE(pointers[2].event_type, MIGO_POINTER_EVENT_UP);
        QCOMPARE(pointers[2].button, 2U);
        QCOMPARE(pointers[3].event_type, MIGO_POINTER_EVENT_MOVE);
        QCOMPARE(pointers[3].button, 0U);
    }

    /// Content that listens for both streams must be able to receive one.
    void pointer_delivery_can_be_narrowed_to_a_single_stream() {
        if (!require_xcb("xcb-only test", __FILE__, __LINE__)) return;
        AttachedFixture fixture;
        MigoQtX11SurfaceView &view = fixture.view;
        view.setPointerDelivery(MigoQtX11SurfaceView::PointerDelivery::TouchOnly);
        QTRY_COMPARE(fake_migo::calls().attach, 1);

        sendMouse(view, QEvent::MouseButtonPress, Qt::LeftButton, Qt::LeftButton, {1.0, 2.0}, 1);
        QCOMPARE(fake_migo::pointers().size(), std::size_t{0});
        QCOMPARE(fake_migo::touches().size(), std::size_t{1});

        view.setPointerDelivery(MigoQtX11SurfaceView::PointerDelivery::MouseOnly);
        sendMouse(view, QEvent::MouseButtonRelease, Qt::LeftButton, Qt::NoButton, {1.0, 2.0}, 2);
        QCOMPARE(fake_migo::pointers().size(), std::size_t{1});
        QCOMPARE(fake_migo::touches().size(), std::size_t{1});
    }

    /// The wheel's unit must survive, because only content can convert it.
    ///
    /// Qt reports a trackpad in pixels and a wheel in eighths of a degree.
    /// Collapsing both into pixels needs a line height the engine does not
    /// have, so `delta_mode` says which one arrived.
    void the_wheel_reports_the_unit_it_was_given() {
        if (!require_xcb("xcb-only test", __FILE__, __LINE__)) return;
        AttachedFixture fixture;
        MigoQtX11SurfaceView &view = fixture.view;
        QTRY_COMPARE(fake_migo::calls().attach, 1);
        QCOMPARE(view.surfaceState(), SurfaceState::Attached);

        QWheelEvent pixel_scroll({5.0, 5.0}, view.mapToGlobal(QPointF{5.0, 5.0}), QPoint(0, -12),
                                 QPoint(0, 0), Qt::NoButton, Qt::NoModifier,
                                 Qt::NoScrollPhase, false);
        QCoreApplication::sendEvent(&view, &pixel_scroll);

        QCOMPARE(fake_migo::wheels().size(), std::size_t{1});
        QCOMPARE(fake_migo::wheels()[0].delta_mode, MIGO_WHEEL_DELTA_MODE_PIXEL);
        // Qt's sign is the opposite of the DOM's: scrolling the content down is
        // a positive deltaY in a browser.
        QCOMPARE(fake_migo::wheels()[0].delta_y, 12.0);

        QWheelEvent notch({5.0, 5.0}, view.mapToGlobal(QPointF{5.0, 5.0}), QPoint(0, 0),
                          QPoint(0, -120), Qt::NoButton, Qt::NoModifier, Qt::NoScrollPhase,
                          false);
        QCoreApplication::sendEvent(&view, &notch);

        QCOMPARE(fake_migo::wheels().size(), std::size_t{2});
        QCOMPARE(fake_migo::wheels()[1].delta_mode, MIGO_WHEEL_DELTA_MODE_LINE);
        QCOMPARE(fake_migo::wheels()[1].delta_y, 3.0);
    }

    /// `code` must name the physical key and `key` what it produced.
    ///
    /// Sending `code` in both -- the likely mistake -- gives content "KeyA" as
    /// typed text. The scan code here is the evdev code for the key labelled A
    /// on a US layout, and the assertion holds on any layout precisely because
    /// `code` does not consult one.
    void a_key_press_carries_code_key_modifiers_and_repeat() {
        if (!require_xcb("xcb-only test", __FILE__, __LINE__)) return;
        AttachedFixture fixture;
        MigoQtX11SurfaceView &view = fixture.view;
        QTRY_COMPARE(fake_migo::calls().attach, 1);
        QCOMPARE(view.surfaceState(), SurfaceState::Attached);

        QKeyEvent press(QEvent::KeyPress, Qt::Key_A, Qt::ControlModifier, x11_keycode(kEvdevKeyA),
                        0, 0, QStringLiteral("\x01"), /*autorep=*/true);
        press.setTimestamp(77);
        QCoreApplication::sendEvent(&view, &press);

        QCOMPARE(fake_migo::keys().size(), std::size_t{1});
        const auto &key = fake_migo::keys()[0];
        QCOMPARE(key.event_type, MIGO_KEY_EVENT_DOWN);
        QCOMPARE(key.code, std::string("KeyA"));
        // Ctrl+A produces a control character in Qt and "a" in the DOM.
        QCOMPARE(key.key, std::string("a"));
        QCOMPARE(key.modifiers, MIGO_KEY_MODIFIER_CONTROL);
        QCOMPARE(key.flags, MIGO_KEY_EVENT_FLAG_REPEAT);
        QCOMPARE(key.timestamp_ms, 77.0);
    }

    /// A named key must arrive under its DOM name, not as its control byte.
    void a_named_key_arrives_as_its_dom_name() {
        if (!require_xcb("xcb-only test", __FILE__, __LINE__)) return;
        AttachedFixture fixture;
        MigoQtX11SurfaceView &view = fixture.view;
        QTRY_COMPARE(fake_migo::calls().attach, 1);
        QCOMPARE(view.surfaceState(), SurfaceState::Attached);

        QKeyEvent press(QEvent::KeyPress, Qt::Key_Left, Qt::NoModifier,
                        x11_keycode(kEvdevArrowLeft), 0, 0, QString(), false);
        QCoreApplication::sendEvent(&view, &press);

        QCOMPARE(fake_migo::keys().size(), std::size_t{1});
        QCOMPARE(fake_migo::keys()[0].code, std::string("ArrowLeft"));
        QCOMPARE(fake_migo::keys()[0].key, std::string("ArrowLeft"));
    }

    /// A key this build cannot name must still be identified.
    ///
    /// The C ABI rejects an empty `code`, so a bridge that had nothing to say
    /// would drop the press entirely rather than report an unknown one.
    void an_unknown_scan_code_is_reported_as_unidentified_rather_than_dropped() {
        if (!require_xcb("xcb-only test", __FILE__, __LINE__)) return;
        AttachedFixture fixture;
        MigoQtX11SurfaceView &view = fixture.view;
        QTRY_COMPARE(fake_migo::calls().attach, 1);
        QCOMPARE(view.surfaceState(), SurfaceState::Attached);

        QKeyEvent press(QEvent::KeyPress, Qt::Key_Any, Qt::NoModifier, 60000, 0, 0, QString(),
                        false);
        QCoreApplication::sendEvent(&view, &press);

        QCOMPARE(fake_migo::keys().size(), std::size_t{1});
        QCOMPARE(fake_migo::keys()[0].code, std::string("Unidentified"));
    }

    /// A multi-byte preedit must cross the boundary intact.
    ///
    /// Qt is UTF-16 and the ABI is length-delimited UTF-8; a length that splits
    /// a character is rejected rather than delivered mangled, and a pinyin
    /// preedit is made of exactly those characters. An ASCII probe would not
    /// notice a broken conversion.
    void a_composition_carries_multibyte_preedit_and_commit() {
        if (!require_xcb("xcb-only test", __FILE__, __LINE__)) return;
        AttachedFixture fixture;
        MigoQtX11SurfaceView &view = fixture.view;
        QTRY_COMPARE(fake_migo::calls().attach, 1);
        QCOMPARE(view.surfaceState(), SurfaceState::Attached);

        const QString preedit = QString::fromUtf8("\xe4\xbd\xa0");         // U+4F60
        const QString committed = QString::fromUtf8("\xe4\xbd\xa0\xe5\xa5\xbd");  // U+4F60 U+597D

        QInputMethodEvent typing(preedit, {});
        QCoreApplication::sendEvent(&view, &typing);

        QCOMPARE(fake_migo::compositions().size(), std::size_t{2});
        QCOMPARE(fake_migo::compositions()[0].event_type, MIGO_COMPOSITION_EVENT_START);
        QCOMPARE(fake_migo::compositions()[1].event_type, MIGO_COMPOSITION_EVENT_UPDATE);
        QCOMPARE(fake_migo::compositions()[1].data, preedit.toUtf8().toStdString());

        QInputMethodEvent accept;
        accept.setCommitString(committed);
        QCoreApplication::sendEvent(&view, &accept);

        QCOMPARE(fake_migo::compositions().size(), std::size_t{3});
        QCOMPARE(fake_migo::compositions()[2].event_type, MIGO_COMPOSITION_EVENT_END);
        QCOMPARE(fake_migo::compositions()[2].data, committed.toUtf8().toStdString());
    }

    /// A real multi-finger gesture must arrive as several points, with the
    /// ones that changed marked as such.
    ///
    /// Content reads `touches` for the whole hand and `changedTouches` for what
    /// moved; a bridge that marked every point as changed would make a
    /// stationary finger look like it were being dragged.
    void a_multi_finger_gesture_arrives_as_several_points() {
        if (!require_xcb("xcb-only test", __FILE__, __LINE__)) return;
        AttachedFixture fixture;
        MigoQtX11SurfaceView &view = fixture.view;
        QTRY_COMPARE(fake_migo::calls().attach, 1);

        // Owned here: QTest::createTouchDevice hands back a device the caller
        // must delete, and leaving it to the process exit turns the sanitizer
        // lane red for a test-harness reason rather than a real one.
        const std::unique_ptr<QPointingDevice> owned_device(QTest::createTouchDevice());
        QPointingDevice *device = owned_device.get();
        QTest::touchEvent(&view, device).press(0, QPoint(10, 10), &view).press(1,
                                                                              QPoint(50, 60),
                                                                              &view);
        QCOMPARE(fake_migo::touches().size(), std::size_t{1});
        const auto &begin = fake_migo::touches()[0];
        QCOMPARE(begin.type, MIGO_TOUCH_START);
        QCOMPARE(begin.points.size(), std::size_t{2});
        QCOMPARE(begin.points[0].id, 0U);
        QCOMPARE(begin.points[1].id, 1U);
        QCOMPARE(begin.points[1].x, 50.0F);
        QVERIFY((begin.points[0].flags & MIGO_TOUCH_FLAG_CHANGED) != 0);

        // Move one finger and leave the other where it is.
        QTest::touchEvent(&view, device).move(0, QPoint(20, 20), &view).stationary(1);
        QCOMPARE(fake_migo::touches().size(), std::size_t{2});
        const auto &moved = fake_migo::touches()[1];
        QCOMPARE(moved.type, MIGO_TOUCH_MOVE);
        QCOMPARE(moved.points.size(), std::size_t{2});
        QVERIFY((moved.points[0].flags & MIGO_TOUCH_FLAG_CHANGED) != 0);
        QVERIFY((moved.points[1].flags & MIGO_TOUCH_FLAG_CHANGED) == 0);

        QTest::touchEvent(&view, device).release(0, QPoint(20, 20), &view).release(1,
                                                                                  QPoint(50, 60),
                                                                                  &view);
        const auto &ended = fake_migo::touches().back();
        QCOMPARE(ended.type, MIGO_TOUCH_END);
        QVERIFY((ended.points[0].flags & MIGO_TOUCH_FLAG_REMOVED) != 0);
    }

    /// Losing focus must retract what content is still waiting on.
    ///
    /// A press whose release never arrives leaves a finger down forever, and a
    /// preedit whose end never arrives leaves text on screen that nothing will
    /// clear. Neither has a later event that corrects it, which is the same
    /// reason the ABI reports a full queue instead of dropping an END.
    void losing_focus_retracts_the_press_and_the_preedit() {
        if (!require_xcb("xcb-only test", __FILE__, __LINE__)) return;
        AttachedFixture fixture;
        MigoQtX11SurfaceView &view = fixture.view;
        QTRY_COMPARE(fake_migo::calls().attach, 1);
        QCOMPARE(view.surfaceState(), SurfaceState::Attached);

        sendMouse(view, QEvent::MouseButtonPress, Qt::LeftButton, Qt::LeftButton, {7.0, 8.0}, 1);
        QInputMethodEvent typing(QString::fromUtf8("\xe4\xbd\xa0"), {});
        QCoreApplication::sendEvent(&view, &typing);
        const std::size_t touches_before = fake_migo::touches().size();
        const std::size_t compositions_before = fake_migo::compositions().size();

        QFocusEvent focus_out(QEvent::FocusOut, Qt::OtherFocusReason);
        QCoreApplication::sendEvent(&view, &focus_out);

        QCOMPARE(fake_migo::touches().size(), touches_before + 1);
        QCOMPARE(fake_migo::touches().back().type, MIGO_TOUCH_CANCEL);
        QCOMPARE(fake_migo::compositions().size(), compositions_before + 1);
        QCOMPARE(fake_migo::compositions().back().event_type, MIGO_COMPOSITION_EVENT_END);
        QCOMPARE(fake_migo::compositions().back().data, std::string());
        QCOMPARE(fake_migo::focus_changes().size(), std::size_t{1});
        QCOMPARE(fake_migo::focus_changes()[0], std::uint8_t{0});

        // Retraction must happen once: a second focus-out has nothing left to
        // withdraw, and sending another cancel would tell content a finger it
        // does not have was lifted.
        QCoreApplication::sendEvent(&view, &focus_out);
        QCOMPARE(fake_migo::touches().size(), touches_before + 1);
    }

    /// An XInput 2 raw motion event's motion is read from the valuators it
    /// sets, a pointer's x and y being its first two, whichever of them it
    /// carries; an event that moves neither, or is not raw motion of the
    /// extension asked about, is none.
    void raw_motion_is_read_from_the_valuators_it_sets() {
        constexpr std::uint8_t kOpcode = 131;
        // As xcb reads one: the fixed part with full_sequence, a mask word, the values.
        struct Event {
            xcb_input_raw_button_press_event_t head;
            std::uint32_t mask;
            xcb_input_fp3232_t values[4];
        };
        const auto event = [](std::uint32_t mask, std::initializer_list<xcb_input_fp3232_t> values) {
            Event raw{};
            raw.head.response_type = XCB_GE_GENERIC;
            raw.head.extension = kOpcode;
            raw.head.event_type = XCB_INPUT_RAW_MOTION;
            raw.head.time = 4242;
            raw.head.valuators_len = 1;
            raw.mask = mask;
            int index = 0;
            for (const xcb_input_fp3232_t value : values) raw.values[index++] = value;
            return raw;
        };
        // 7.5 and -2.25: a whole part, then the fraction over 2^32 added to it.
        Event both = event(0b11, {{7, 0x80000000U}, {-3, 0xC0000000U}});
        std::optional<RawMotion> motion = rawMotion(&both, kOpcode);
        QVERIFY(motion.has_value());
        QCOMPARE(motion->delta, QPointF(7.5, -2.25));
        QCOMPARE(motion->time, 4242U);

        Event y_only = event(0b10, {{5, 0}});
        motion = rawMotion(&y_only, kOpcode);
        QVERIFY(motion.has_value());
        QCOMPARE(motion->delta, QPointF(0.0, 5.0));

        // A third valuator -- a wheel's, say -- is not the pointer's motion.
        Event x_and_wheel = event(0b101, {{4, 0}, {9, 0}});
        motion = rawMotion(&x_and_wheel, kOpcode);
        QVERIFY(motion.has_value());
        QCOMPARE(motion->delta, QPointF(4.0, 0.0));
        Event wheel_only = event(0b100, {{9, 0}});
        QVERIFY(!rawMotion(&wheel_only, kOpcode).has_value());

        Event still = event(0b11, {{0, 0}, {0, 0}});
        QVERIFY(!rawMotion(&still, kOpcode).has_value());
        QVERIFY(!rawMotion(&both, kOpcode + 1).has_value());
        Event press = both;
        press.head.event_type = XCB_INPUT_RAW_BUTTON_PRESS;
        QVERIFY(!rawMotion(&press, kOpcode).has_value());
        Event core = both;
        core.head.response_type = XCB_MOTION_NOTIFY;
        QVERIFY(!rawMotion(&core, kOpcode).has_value());
    }

    /// A pointer lock is taken only with the player at the game -- the view
    /// holding the keyboard, the cursor over it -- and hides the cursor. The
    /// mouse's raw motion then moves content's pointer from where the cursor
    /// stood, past the view's edge too, the hidden cursor's own motion moving
    /// nothing, and a click lands where content's pointer is. Esc releases it,
    /// neither that Esc nor its release reaching content, and the cursor stands
    /// where it stood.
    void a_pointer_lock_moves_the_pointer_by_the_mouse_until_esc() {
        if (!require_xcb("xcb-only test", __FILE__, __LINE__)) return;
        AttachedFixture fixture;
        MigoQtX11SurfaceView &view = fixture.view;
        QTRY_COMPARE(fake_migo::calls().attach, 1);
        x11_player::Hands hands;
        QSignalSpy changes(&view, &MigoQtX11SurfaceView::pointerLockChanged);

        view.clearFocus();
        QCursor::setPos(view.mapToGlobal(QPoint(100, 50)));
        QVERIFY(!view.lockPointer());
        QVERIFY(bringPlayerTo(fixture, QPoint(view.width() + 10, 50)));
        QVERIFY(!view.lockPointer());
        QCOMPARE(changes.size(), 0);

        const QPoint origin = view.mapToGlobal(QPoint(100, 50));
        QCursor::setPos(origin);
        QVERIFY(view.lockPointer());
        QVERIFY(view.isPointerLocked());
        QCOMPARE(changes.size(), 1);
        QCOMPARE(changes.at(0).at(0).toBool(), true);
        QVERIFY(QGuiApplication::overrideCursor() != nullptr);
        QCOMPARE(QGuiApplication::overrideCursor()->shape(), Qt::BlankCursor);
        // A lock already held is no change.
        QVERIFY(view.lockPointer());
        QCOMPARE(changes.size(), 1);

        const std::size_t before = fake_migo::pointers().size();
        hands.move(30, -70);
        QTRY_COMPARE(fake_migo::pointers().size(), before + 1);
        QTest::qWait(100);
        QCOMPARE(fake_migo::pointers().size(), before + 1);
        QCOMPARE(fake_migo::pointers().back().event_type, MIGO_POINTER_EVENT_MOVE);
        QCOMPARE(fake_migo::pointers().back().x, 130.0F);
        QCOMPARE(fake_migo::pointers().back().y, -20.0F);

        hands.click(1);
        QTRY_COMPARE(fake_migo::pointers().size(), before + 3);
        QCOMPARE(fake_migo::pointers()[before + 1].event_type, MIGO_POINTER_EVENT_DOWN);
        QCOMPARE(fake_migo::pointers()[before + 1].x, 130.0F);
        QCOMPARE(fake_migo::pointers()[before + 1].y, -20.0F);
        QCOMPARE(fake_migo::pointers()[before + 2].event_type, MIGO_POINTER_EVENT_UP);

        const std::size_t keys = fake_migo::keys().size();
        hands.escape();
        QTRY_VERIFY(!view.isPointerLocked());
        QCOMPARE(changes.size(), 2);
        QCOMPARE(changes.at(1).at(0).toBool(), false);
        QCOMPARE(QGuiApplication::overrideCursor(), nullptr);
        QCOMPARE(QCursor::pos(), origin);
        QTest::qWait(100);
        QCOMPARE(fake_migo::keys().size(), keys);
        // The next Esc is the game's.
        hands.escape();
        QTRY_COMPARE(fake_migo::keys().size(), keys + 2);
    }

    /// A lock goes when the view loses the keyboard -- Esc would no longer
    /// reach it -- and when the game goes.
    void a_pointer_lock_goes_with_the_keyboard_and_with_the_game() {
        if (!require_xcb("xcb-only test", __FILE__, __LINE__)) return;
        AttachedFixture fixture;
        MigoQtX11SurfaceView &view = fixture.view;
        QTRY_COMPARE(fake_migo::calls().attach, 1);
        QSignalSpy changes(&view, &MigoQtX11SurfaceView::pointerLockChanged);

        QVERIFY(bringPlayerTo(fixture, QPoint(10, 10)));
        QVERIFY(view.lockPointer());
        view.clearFocus();
        QVERIFY(!view.isPointerLocked());
        QCOMPARE(changes.size(), 2);

        QVERIFY(bringPlayerTo(fixture, QPoint(10, 10)));
        QVERIFY(view.lockPointer());
        QCOMPARE(view.beginDetach(), MIGO_OK);
        QVERIFY(!view.isPointerLocked());
        QCOMPARE(changes.size(), 4);
        QCOMPARE(changes.at(3).at(0).toBool(), false);
        QCOMPARE(QGuiApplication::overrideCursor(), nullptr);
    }

    /// Without an attachment there is nothing to deliver to.
    ///
    /// Runs on every platform, including offscreen, because it is the one input
    /// assertion that does not need a real X11 window -- and it is the state a
    /// host is in for the whole time before it shows the view.
    void input_before_attach_reaches_nothing() {
        SurfaceHost surface_host(fake_migo::session());
        QWidget container;
        MigoQtX11SurfaceView view(surface_host, container);
        view.resize(100, 100);

        sendMouse(view, QEvent::MouseButtonPress, Qt::LeftButton, Qt::LeftButton, {1.0, 1.0}, 1);
        QKeyEvent press(QEvent::KeyPress, Qt::Key_A, Qt::NoModifier, x11_keycode(kEvdevKeyA), 0,
                        0, QStringLiteral("a"), false);
        QCoreApplication::sendEvent(&view, &press);
        QFocusEvent focus_in(QEvent::FocusIn, Qt::OtherFocusReason);
        QCoreApplication::sendEvent(&view, &focus_in);

        QCOMPARE(fake_migo::calls().pointer, 0);
        QCOMPARE(fake_migo::calls().touch, 0);
        QCOMPARE(fake_migo::calls().key, 0);
        QCOMPARE(fake_migo::calls().focus, 0);
    }

    /// A refused event must be reported, not swallowed.
    void a_refused_event_is_reported_to_the_host() {
        if (!require_xcb("xcb-only test", __FILE__, __LINE__)) return;
        AttachedFixture fixture;
        MigoQtX11SurfaceView &view = fixture.view;
        QTRY_COMPARE(fake_migo::calls().attach, 1);
        QCOMPARE(view.surfaceState(), SurfaceState::Attached);
        QSignalSpy rejected(&view, &MigoQtX11SurfaceView::inputRejected);
        QSignalSpy surface_errors(&view, &MigoQtX11SurfaceView::surfaceError);

        fake_migo::set_input_result(MIGO_ERROR_WOULD_BLOCK);
        sendMouse(view, QEvent::MouseButtonPress, Qt::LeftButton, Qt::LeftButton, {1.0, 1.0}, 1);

        QVERIFY(rejected.count() > 0);
        QCOMPARE(rejected.at(0).at(0).value<MigoResult>(), MIGO_ERROR_WOULD_BLOCK);
        // Backpressure is not a Surface fault: a host that tore the Surface
        // down here would turn a full queue into a black screen.
        QCOMPARE(surface_errors.count(), 0);
        QCOMPARE(view.lastError(), MIGO_OK);
    }

    /// One request must produce exactly one frame boundary.
    ///
    /// The engine asks for a frame at a time, so a bridge that reported two
    /// boundaries for one request would advance the content's clock at double
    /// rate -- animation twice as fast, physics twice as far per step.
    void one_frame_request_produces_exactly_one_boundary() {
        if (!require_xcb("xcb-only test", __FILE__, __LINE__)) return;
        AttachedFixture fixture;
        MigoQtX11SurfaceView &view = fixture.view;
        QTRY_COMPARE(fake_migo::calls().attach, 1);

        QCOMPARE(view.requestFrame(), MIGO_OK);
        QVERIFY(view.isFramePending());
        QTRY_COMPARE(fake_migo::calls().vsync, 1);
        QCOMPARE(view.isFramePending(), false);
        QCOMPARE(fake_migo::vsyncs().size(), std::size_t{1});
        QVERIFY(fake_migo::vsyncs()[0] > 0);

        // Nothing further arrives on its own: the engine has not asked again.
        QTest::qWait(80);
        QCOMPARE(fake_migo::calls().vsync, 1);
    }

    /// Asking twice before the frame arrives must still produce one boundary.
    ///
    /// `QWindow::requestUpdate()` already ignores repeat calls before delivery,
    /// so this asserts the adapter does not undo that -- reporting a boundary
    /// per call rather than per delivered update would be the way to break it.
    /// What the adapter's own pending flag prevents is the opposite case, an
    /// unrequested repaint, covered separately below.
    void a_second_request_before_the_frame_coalesces() {
        if (!require_xcb("xcb-only test", __FILE__, __LINE__)) return;
        AttachedFixture fixture;
        MigoQtX11SurfaceView &view = fixture.view;
        QTRY_COMPARE(fake_migo::calls().attach, 1);

        QCOMPARE(view.requestFrame(), MIGO_OK);
        QCOMPARE(view.requestFrame(), MIGO_OK);
        QCOMPARE(view.requestFrame(), MIGO_OK);
        QTRY_COMPARE(fake_migo::calls().vsync, 1);
        QTest::qWait(80);
        QCOMPARE(fake_migo::calls().vsync, 1);
    }

    /// A repaint nobody asked for must not be reported as a frame.
    ///
    /// Qt repaints for its own reasons -- a resize, an expose, a window
    /// manager event -- and reporting those as frame boundaries would advance
    /// the content's clock on window activity rather than on the frames the
    /// engine asked for. This is what the adapter's pending flag is for; Qt's
    /// own coalescing does not cover it.
    void an_unrequested_repaint_does_not_report_a_frame() {
        if (!require_xcb("xcb-only test", __FILE__, __LINE__)) return;
        AttachedFixture fixture;
        MigoQtX11SurfaceView &view = fixture.view;
        QTRY_COMPARE(fake_migo::calls().attach, 1);
        QCOMPARE(view.isFramePending(), false);

        // Exactly what a window manager or a layout change produces.
        view.update();
        view.repaint();
        QTest::qWait(60);
        QCOMPARE(fake_migo::calls().vsync, 0);

        // And the path still works afterwards, so the guard is not simply
        // disabling frames.
        QCOMPARE(view.requestFrame(), MIGO_OK);
        QTRY_COMPARE(fake_migo::calls().vsync, 1);
    }

    /// Frames must not be driven by a clock of this adapter's own.
    ///
    /// A fixed-interval timer would keep reporting boundaries the engine never
    /// asked for, and would compete with the compositor rather than follow it.
    /// Waiting several display intervals without asking is what distinguishes
    /// the two.
    void no_frame_arrives_without_the_engine_asking() {
        if (!require_xcb("xcb-only test", __FILE__, __LINE__)) return;
        AttachedFixture fixture;
        QTRY_COMPARE(fake_migo::calls().attach, 1);

        QTest::qWait(150);
        QCOMPARE(fake_migo::calls().vsync, 0);
    }

    /// A frame request is a Session call and follows the same rules as the rest.
    void a_frame_request_is_refused_when_detached_or_from_another_thread() {
        SurfaceHost surface_host(fake_migo::session());
        QWidget container;
        MigoQtX11SurfaceView view(surface_host, container);
        view.resize(100, 100);

        QCOMPARE(view.requestFrame(), MIGO_ERROR_INVALID_STATE);

        MigoResult foreign = MIGO_OK;
        std::thread other([&] { foreign = view.requestFrame(); });
        other.join();
        QCOMPARE(foreign, MIGO_ERROR_WRONG_THREAD);
        QCOMPARE(fake_migo::calls().vsync, 0);
    }

    /// The hot path must not allocate.
    ///
    /// Measured as the difference between the same burst delivered and not
    /// delivered, rather than between two burst lengths: constructing and
    /// dispatching a Qt event allocates inside Qt itself, so a raw count would
    /// measure the toolkit rather than this adapter. Retiring the Surface makes
    /// the handlers return before touching anything, which leaves exactly this
    /// code's own allocations in the difference.
    void the_hot_path_allocates_nothing_per_event() {
        if (!require_xcb("xcb-only test", __FILE__, __LINE__)) return;
        AttachedFixture fixture;
        MigoQtX11SurfaceView &view = fixture.view;
        QTRY_COMPARE(fake_migo::calls().attach, 1);

        constexpr int kBurst = 40;
        const auto burst = [&] {
            const std::size_t before = g_allocations.load(std::memory_order_relaxed);
            for (int i = 0; i < kBurst; ++i) {
                sendMouse(view, QEvent::MouseMove, Qt::NoButton, Qt::LeftButton,
                          {2.0 + i, 3.0}, static_cast<quint64>(100 + i));
                QKeyEvent press(QEvent::KeyPress, Qt::Key_A, Qt::NoModifier,
                                x11_keycode(kEvdevKeyA), 0, 0, QStringLiteral("a"), false);
                press.setTimestamp(static_cast<quint64>(200 + i));
                QCoreApplication::sendEvent(&view, &press);
                QWheelEvent scroll({5.0, 5.0}, view.mapToGlobal(QPointF{5.0, 5.0}),
                                   QPoint(0, -12), QPoint(0, 0), Qt::NoButton, Qt::NoModifier,
                                   Qt::NoScrollPhase, false);
                QCoreApplication::sendEvent(&view, &scroll);
            }
            return g_allocations.load(std::memory_order_relaxed) - before;
        };

        // Warm up whatever Qt initialises lazily, so it lands in neither sample.
        (void)burst();
        const std::size_t delivering = burst();
        QVERIFY(fake_migo::calls().pointer > 0);

        // Retire the Surface: the handlers now return before reaching the ABI.
        QCOMPARE(view.beginDetach(), MIGO_OK);
        fake_migo::set_release_status(view.generation(), MIGO_SURFACE_RELEASE_RELEASED);
        bool released = false;
        QCOMPARE(view.pollDetach(&released), MIGO_OK);
        QVERIFY(released);
        const int pointer_calls = fake_migo::calls().pointer;

        const std::size_t not_delivering = burst();
        QCOMPARE(fake_migo::calls().pointer, pointer_calls);

        // The counter must be live. Dispatching a Qt event allocates inside Qt
        // whether or not this adapter delivers it, so a zero here means the
        // hook was never installed and the comparison below would hold no
        // matter what this code did -- the same vacuous pass the operator-new
        // version of this counter produced against Qt's containers.
        QVERIFY(not_delivering > 0);
        QCOMPARE(delivering, not_delivering);
    }
};

QTEST_MAIN(QtX11InputTest)
#include "qt_x11_input_test.moc"

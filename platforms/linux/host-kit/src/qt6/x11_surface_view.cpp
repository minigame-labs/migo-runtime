#include <migo/linux/qt6/x11_surface_view.hpp>

#include "dom_key_names.hpp"
#include "xi2_raw_motion.hpp"

#include <QAbstractNativeEventFilter>
#include <QCloseEvent>
#include <QCursor>
#include <QFocusEvent>
#include <QInputMethodEvent>
#include <QKeyEvent>
#include <QMouseEvent>
#include <QTouchEvent>
#include <QWheelEvent>
#include <QGuiApplication>
#include <QPaintEngine>
#include <QPlatformSurfaceEvent>
#include <QPointer>
#include <QResizeEvent>
#include <QScreen>
#include <QShowEvent>
#include <QThread>
#include <QWindow>
#include <QtGui/qguiapplication_platform.h>

#if !QT_CONFIG(xcb)
#error "MigoQtX11SurfaceView requires a Qt build with the xcb platform feature"
#endif

#include <X11/Xlib.h>
#include <xcb/xcb.h>
#include <xcb/xinput.h>

#include <algorithm>
#include <chrono>
#include <cmath>
#include <cstdlib>
#include <cstring>
#include <limits>
#include <optional>
#include <vector>

namespace migo::linux_host::qt6 {

namespace {

constexpr int kFastReleasePollIntervalMs = 4;
constexpr int kNormalReleasePollIntervalMs = 16;
constexpr int kStalledReleasePollIntervalMs = 250;
constexpr qint64 kFastReleaseWindowMs = 100;
constexpr qint64 kReleaseStallThresholdMs = 2000;

struct XcbFree {
    void operator()(void *reply) const noexcept { std::free(reply); }
};
template <typename T>
using XcbReply = std::unique_ptr<T, XcbFree>;

}  // namespace

// ---------------------------------------------------------------------------
// Pointer lock
//
// While a lock holds, the mouse is grabbed and the cursor hidden -- Qt's grab
// with a cursor is an application's override cursor, so a cursor content sets
// meanwhile shows once the lock is gone, as in a browser -- and what moves
// content's pointer is XInput 2's raw motion. The root window reports it
// whatever the cursor does, at the screen's edge too, where a cursor stops, and
// to every client that selected it there, grab or none. A selection is this
// client's, one per window and device, so the lock's is made over what this
// client had selected on the root for every master device, and that is put back.
// ---------------------------------------------------------------------------

class MigoQtX11SurfaceView::PointerLock final : public QAbstractNativeEventFilter {
public:
    /// Reads `view`'s raw motion from now on; null when the X server will not report it.
    static std::unique_ptr<PointerLock> engage(MigoQtX11SurfaceView &view) {
        auto *native = qGuiApp->nativeInterface<QNativeInterface::QX11Application>();
        xcb_connection_t *connection = native != nullptr ? native->connection() : nullptr;
        if (connection == nullptr) return nullptr;
        const xcb_query_extension_reply_t *xi = xcb_get_extension_data(connection, &xcb_input_id);
        if (xi == nullptr || xi->present == 0) return nullptr;
        // Raw events are reported on the root of the view's screen alone.
        const XcbReply<xcb_get_geometry_reply_t> geometry(xcb_get_geometry_reply(
            connection, xcb_get_geometry(connection, static_cast<xcb_drawable_t>(view.winId())),
            nullptr));
        if (geometry == nullptr) return nullptr;
        const XcbReply<xcb_input_xi_get_selected_events_reply_t> selected(
            xcb_input_xi_get_selected_events_reply(
                connection, xcb_input_xi_get_selected_events(connection, geometry->root), nullptr));
        if (selected == nullptr) return nullptr;
        std::vector<std::uint32_t> previous{0};
        for (auto masks = xcb_input_xi_get_selected_events_masks_iterator(selected.get());
             masks.rem > 0; xcb_input_event_mask_next(&masks)) {
            if (masks.data->deviceid != XCB_INPUT_DEVICE_ALL_MASTER || masks.data->mask_len == 0) {
                continue;
            }
            const std::uint32_t *words = xcb_input_event_mask_mask(masks.data);
            previous.assign(words, words + masks.data->mask_len);
        }

        std::unique_ptr<PointerLock> lock(
            new PointerLock(view, connection, geometry->root, xi->major_opcode, std::move(previous)));
        std::vector<std::uint32_t> wanted = lock->previous_;
        wanted[0] |= XCB_INPUT_XI_EVENT_MASK_RAW_MOTION;
        const std::vector<std::uint32_t> request = lock->selection(wanted);
        if (XcbReply<xcb_generic_error_t> error(xcb_request_check(
                connection, xcb_input_xi_select_events_checked(
                                connection, lock->root_, 1,
                                reinterpret_cast<const xcb_input_event_mask_t *>(request.data()))));
            error != nullptr) {
            return nullptr;
        }
        qApp->installNativeEventFilter(lock.get());
        return lock;
    }

    ~PointerLock() override {
        qApp->removeNativeEventFilter(this);
        const std::vector<std::uint32_t> request = selection(previous_);
        xcb_input_xi_select_events(connection_, root_, 1,
                                   reinterpret_cast<const xcb_input_event_mask_t *>(request.data()));
        xcb_flush(connection_);
    }

    PointerLock(const PointerLock &) = delete;
    PointerLock &operator=(const PointerLock &) = delete;

    bool nativeEventFilter(const QByteArray &type, void *message, qintptr *result) override {
        Q_UNUSED(result);
        if (type != "xcb_generic_event_t") return false;
        if (const std::optional<RawMotion> motion = rawMotion(message, opcode_)) {
            // The screen's pixels are the view's times its device pixel ratio.
            view_.moveLockedPointer(motion->delta / view_.devicePixelRatioF(),
                                    static_cast<double>(motion->time));
        }
        // Not taken from Qt: another filter may read the same motion.
        return false;
    }

private:
    PointerLock(MigoQtX11SurfaceView &view, xcb_connection_t *connection, xcb_window_t root,
                std::uint8_t opcode, std::vector<std::uint32_t> previous)
        : view_(view), connection_(connection), root_(root), opcode_(opcode),
          previous_(std::move(previous)) {}

    /// XISelectEvents' one mask for every master device: its head, then `mask`.
    static std::vector<std::uint32_t> selection(const std::vector<std::uint32_t> &mask) {
        std::vector<std::uint32_t> request(1 + mask.size());
        const xcb_input_event_mask_t head{XCB_INPUT_DEVICE_ALL_MASTER,
                                          static_cast<std::uint16_t>(mask.size())};
        static_assert(sizeof(head) == sizeof(std::uint32_t));
        std::memcpy(request.data(), &head, sizeof(head));
        std::copy(mask.begin(), mask.end(), request.begin() + 1);
        return request;
    }

    MigoQtX11SurfaceView &view_;
    xcb_connection_t *const connection_;
    const xcb_window_t root_;
    const std::uint8_t opcode_;
    /// What this client had selected on the root for every master device.
    const std::vector<std::uint32_t> previous_;
};

MigoQtX11SurfaceView::MigoQtX11SurfaceView(SurfaceHost &surface_host, QWidget &parent)
    : QWidget(&parent),
      surface_host_(surface_host),
      owner_thread_(QThread::currentThread()) {
    // Parenting happens here rather than in the initialiser list: it sends a
    // ChildAdded event synchronously, and this class overrides `event()`, so
    // doing it earlier would run that override against members that do not
    // exist yet.
    release_timer_.setParent(this);
    metrics_update_timer_.setParent(this);
    attach_timer_.setParent(this);
    setAttribute(Qt::WA_DontCreateNativeAncestors);
    setAttribute(Qt::WA_NativeWindow);
    setAttribute(Qt::WA_PaintOnScreen);
    setAttribute(Qt::WA_OpaquePaintEvent);
    setAttribute(Qt::WA_NoSystemBackground);
    // Input is delivered to this widget only if it can hold focus and is
    // offered touch and IME events; none of these installs a filter or takes
    // anything away from the App's own widgets.
    setAttribute(Qt::WA_AcceptTouchEvents);
    setAttribute(Qt::WA_InputMethodEnabled);
    setFocusPolicy(Qt::StrongFocus);
    // Hover reaches the mouse stream, which is the point of having one: PC mini-game
    // content listens for `onMouseMove` without a button held. The touch stream
    // still only sees motion while pressed, because mini-game content on a phone has
    // no hover concept and a free motion stream would be events no game reads.
    setMouseTracking(true);
    release_timer_.setInterval(kFastReleasePollIntervalMs);
    release_timer_.setTimerType(Qt::PreciseTimer);
    connect(&release_timer_, &QTimer::timeout, this, [this] {
        bool released = false;
        (void)pollDetach(&released);
    });

    metrics_update_timer_.setSingleShot(true);
    connect(&metrics_update_timer_, &QTimer::timeout, this, [this] {
        if (!owns_surface_ || surface_host_.state() != SurfaceState::Attached ||
            surface_host_.generation() != owned_generation_) {
            return;
        }
        SurfaceMetrics metrics;
        const MigoResult metrics_result = currentMetrics(&metrics);
        if (metrics_result != MIGO_OK) {
            recordError(metrics_result);
            return;
        }
        recordError(surface_host_.update(metrics));
    });

    attach_timer_.setSingleShot(true);
    connect(&attach_timer_, &QTimer::timeout, this, [this] {
        if (isVisible() && !owns_surface_ && surface_host_.state() == SurfaceState::Detached &&
            !close_pending_) {
            (void)attachSurface();
        }
    });
}

MigoQtX11SurfaceView::~MigoQtX11SurfaceView() {
    release_timer_.stop();
    metrics_update_timer_.stop();
    // The player's mouse is never left grabbed, whatever happened; there is no
    // one left to tell.
    if (pointer_lock_ != nullptr) {
        pointer_lock_.reset();
        releaseMouse();
    }
    if (owns_surface_) {
        qFatal("MigoQtX11SurfaceView destroyed before its native Surface reached RELEASED");
    }
}

QPaintEngine *MigoQtX11SurfaceView::paintEngine() const { return nullptr; }

MigoResult MigoQtX11SurfaceView::currentMetrics(SurfaceMetrics *metrics) const noexcept {
    if (metrics == nullptr) return MIGO_ERROR_INVALID_ARGUMENT;
    const double scale = devicePixelRatioF();
    const double physical_width = static_cast<double>(width()) * scale;
    const double physical_height = static_cast<double>(height()) * scale;
    constexpr double kMaxDimension =
        static_cast<double>(std::numeric_limits<std::uint32_t>::max());
    if (!std::isfinite(scale) || scale <= 0.0 || physical_width < 1.0 ||
        physical_height < 1.0 || physical_width > kMaxDimension ||
        physical_height > kMaxDimension) {
        return MIGO_ERROR_INVALID_ARGUMENT;
    }

    metrics->width_pixels = static_cast<std::uint32_t>(std::llround(physical_width));
    metrics->height_pixels = static_cast<std::uint32_t>(std::llround(physical_height));
    metrics->scale_factor = static_cast<float>(scale);
    metrics->color_space = MIGO_COLOR_SPACE_SRGB;
    metrics->alpha_mode = MIGO_ALPHA_MODE_OPAQUE;
    metrics->presentation_mode = MIGO_PRESENTATION_MODE_DEFAULT;
    metrics->required_capabilities = MIGO_SURFACE_CAPABILITY_NONE;
    return MIGO_OK;
}

void MigoQtX11SurfaceView::recordError(MigoResult error) {
    last_error_ = error;
    if (error != MIGO_OK) Q_EMIT surfaceError(error);
}

MigoResult MigoQtX11SurfaceView::attachSurface() {
    if (QThread::currentThread() != owner_thread_) return MIGO_ERROR_WRONG_THREAD;
    if (owns_surface_ || surface_host_.state() != SurfaceState::Detached || close_pending_) {
        recordError(MIGO_ERROR_INVALID_STATE);
        return MIGO_ERROR_INVALID_STATE;
    }
    if (QGuiApplication::platformName() != QStringLiteral("xcb")) {
        recordError(MIGO_ERROR_UNSUPPORTED_PLATFORM);
        return MIGO_ERROR_UNSUPPORTED_PLATFORM;
    }

    auto *native = qGuiApp->nativeInterface<QNativeInterface::QX11Application>();
    if (native == nullptr || native->display() == nullptr) {
        recordError(MIGO_ERROR_UNSUPPORTED_PLATFORM);
        return MIGO_ERROR_UNSUPPORTED_PLATFORM;
    }

    const WId id = winId();
    if (id == 0) {
        recordError(MIGO_ERROR_INTERNAL);
        return MIGO_ERROR_INTERNAL;
    }

    // A detached QWidget may be reparented, which can replace its QWindow and
    // silently invalidate the old sender connection. Rebuild this cheap
    // control-path connection on every attach instead of caching a boolean.
    QObject::disconnect(screen_changed_connection_);
    screen_changed_connection_ = {};
    if (QWindow *native_window = windowHandle(); native_window != nullptr) {
        screen_changed_connection_ =
            connect(native_window, &QWindow::screenChanged, this,
                    [this](QScreen *) { scheduleMetricsUpdate(); });
    }

    XWindowAttributes attributes{};
    const ::Window x11_window = static_cast<::Window>(id);
    if (XGetWindowAttributes(native->display(), x11_window, &attributes) == 0 ||
        attributes.screen == nullptr) {
        recordError(MIGO_ERROR_INTERNAL);
        return MIGO_ERROR_INTERNAL;
    }

    SurfaceMetrics metrics;
    const MigoResult metrics_result = currentMetrics(&metrics);
    if (metrics_result != MIGO_OK) {
        recordError(metrics_result);
        return metrics_result;
    }

    const X11Target target{native->display(), static_cast<std::uintptr_t>(id),
                           XScreenNumberOfScreen(attributes.screen)};
    const MigoResult result = surface_host_.attach(target, metrics);
    if (result == MIGO_OK || surface_host_.state() == SurfaceState::Faulted) {
        owns_surface_ = true;
        owned_generation_ = surface_host_.generation();
    }
    recordError(result);
    if (result == MIGO_OK) Q_EMIT surfaceAttached(owned_generation_);
    return result;
}

MigoResult MigoQtX11SurfaceView::beginDetach() {
    if (QThread::currentThread() != owner_thread_) return MIGO_ERROR_WRONG_THREAD;
    if (!owns_surface_ || surface_host_.state() != SurfaceState::Attached ||
        surface_host_.generation() != owned_generation_) {
        recordError(MIGO_ERROR_INVALID_STATE);
        return MIGO_ERROR_INVALID_STATE;
    }
    // The game is going: a lock it holds goes first.
    unlockPointer();
    const MigoResult result = surface_host_.begin_detach();
    recordError(result);
    if (result == MIGO_OK) {
        metrics_update_timer_.stop();
        release_stall_reported_ = false;
        release_elapsed_.start();
        release_timer_.setInterval(kFastReleasePollIntervalMs);
        release_timer_.start();
    }
    return result;
}

MigoResult MigoQtX11SurfaceView::pollDetach(bool *released) {
    if (QThread::currentThread() != owner_thread_) return MIGO_ERROR_WRONG_THREAD;
    if (released == nullptr) {
        recordError(MIGO_ERROR_INVALID_ARGUMENT);
        return MIGO_ERROR_INVALID_ARGUMENT;
    }
    if (!owns_surface_ || surface_host_.state() != SurfaceState::Retiring ||
        surface_host_.generation() != owned_generation_) {
        recordError(MIGO_ERROR_INVALID_STATE);
        return MIGO_ERROR_INVALID_STATE;
    }
    const MigoResult result = surface_host_.poll_release(released);
    if (result != MIGO_OK) {
        release_timer_.stop();
        recordError(result);
        return result;
    }
    recordError(result);
    if (!*released) {
        updateReleasePollCadence();
        return result;
    }

    release_timer_.stop();
    release_elapsed_.invalidate();
    const std::uint64_t released_generation = owned_generation_;
    owned_generation_ = 0;
    owns_surface_ = false;
    const bool should_close = close_pending_;
    QPointer<MigoQtX11SurfaceView> guard(this);
    Q_EMIT surfaceReleased(released_generation);
    if (guard != nullptr) {
        guard->close_pending_ = false;
        if (should_close) guard->close();
    }
    return MIGO_OK;
}

void MigoQtX11SurfaceView::updateReleasePollCadence() {
    if (!release_elapsed_.isValid()) return;
    const qint64 elapsed_ms = release_elapsed_.elapsed();
    int interval_ms = kFastReleasePollIntervalMs;
    if (elapsed_ms >= kReleaseStallThresholdMs) {
        interval_ms = kStalledReleasePollIntervalMs;
    } else if (elapsed_ms >= kFastReleaseWindowMs) {
        interval_ms = kNormalReleasePollIntervalMs;
    }
    if (release_timer_.interval() != interval_ms) release_timer_.setInterval(interval_ms);
    if (!release_timer_.isActive()) release_timer_.start();

    if (elapsed_ms >= kReleaseStallThresholdMs && !release_stall_reported_) {
        release_stall_reported_ = true;
        Q_EMIT surfaceReleaseStalled(surface_host_.generation(), elapsed_ms);
    }
}

void MigoQtX11SurfaceView::scheduleMetricsUpdate() {
    if (owns_surface_ && surface_host_.state() == SurfaceState::Attached &&
        surface_host_.generation() == owned_generation_ &&
        !metrics_update_timer_.isActive()) {
        metrics_update_timer_.start(0);
    }
}

void MigoQtX11SurfaceView::showEvent(QShowEvent *event) {
    QWidget::showEvent(event);
    // Not now: Qt sends a widget its show event before it maps the native window
    // and applies its geometry, and the engine binds the window as the X server
    // has it. Attached here, a view a layout had sized was bound while its window
    // was still unmapped at the size it was created with -- a child's 100x30 --
    // and the game drew into that. A zero timeout runs once this show is done.
    if (!owns_surface_ && surface_host_.state() == SurfaceState::Detached && !close_pending_) {
        attach_timer_.start(0);
    }
}

void MigoQtX11SurfaceView::resizeEvent(QResizeEvent *event) {
    QWidget::resizeEvent(event);
    scheduleMetricsUpdate();
}

void MigoQtX11SurfaceView::closeEvent(QCloseEvent *event) {
    if (!owns_surface_) {
        close_pending_ = false;
        QWidget::closeEvent(event);
        return;
    }
    switch (surface_host_.state()) {
        case SurfaceState::Detached:
            qFatal("MigoQtX11SurfaceView attachment was released outside its owner view");
        case SurfaceState::Attached:
            event->ignore();
            close_pending_ = true;
            {
                QPointer<MigoQtX11SurfaceView> guard(this);
                const MigoResult result = beginDetach();
                if (guard != nullptr && result != MIGO_OK) guard->close_pending_ = false;
            }
            return;
        case SurfaceState::Retiring:
            close_pending_ = true;
            event->ignore();
            return;
        case SurfaceState::Faulted:
            qFatal("MigoQtX11SurfaceView cannot close after losing its release observer");
    }
}

// ---------------------------------------------------------------------------
// Input
//
// Every handler below runs in the Qt event loop on the GUI thread, which is the
// Session owner thread this view was constructed on, so nothing here hops
// threads, filters events globally, or polls. Coordinates need no conversion:
// Qt's logical position is physical pixels divided by the same device pixel
// ratio this view reports as `scale_factor` at attach, and CSS pixels are
// defined the same way. That equality is why this code looks like it forgot to
// convert -- writing `position() * devicePixelRatio()` here is the mistake, and
// it lands every touch in the wrong place on a HiDPI screen.
// ---------------------------------------------------------------------------

bool MigoQtX11SurfaceView::inputIsDeliverable() const noexcept {
    return owns_surface_ && surface_host_.state() == SurfaceState::Attached &&
           surface_host_.generation() == owned_generation_ &&
           surface_host_.session() != nullptr;
}

void MigoQtX11SurfaceView::recordInputResult(MigoResult result) {
    if (result != MIGO_OK) Q_EMIT inputRejected(result);
}

namespace {

MigoKeyModifiers dom_modifiers(Qt::KeyboardModifiers modifiers) noexcept {
    MigoKeyModifiers result = MIGO_KEY_MODIFIER_NONE;
    if (modifiers & Qt::ControlModifier) result |= MIGO_KEY_MODIFIER_CONTROL;
    if (modifiers & Qt::ShiftModifier) result |= MIGO_KEY_MODIFIER_SHIFT;
    if (modifiers & Qt::AltModifier) result |= MIGO_KEY_MODIFIER_ALT;
    if (modifiers & Qt::MetaModifier) result |= MIGO_KEY_MODIFIER_META;
    return result;
}

// DOM MouseEvent.button ordinals. Qt's enum is a bitmask, so it cannot be cast.
std::uint32_t dom_button(Qt::MouseButton button) noexcept {
    switch (button) {
        case Qt::LeftButton:
            return 0;
        case Qt::MiddleButton:
            return 1;
        case Qt::RightButton:
            return 2;
        case Qt::BackButton:
            return 3;
        case Qt::ForwardButton:
            return 4;
        default:
            return 0;
    }
}

// The button a move names, from the set still held after the event.
//
// `include/migo/input.h` says a move "names the button being held", so this has
// to be asked of the event rather than remembered: a remembered ordinal survives
// the release that ends the drag, and every later hover then reports a button
// nobody is holding. Lowest ordinal first, because the ABI's field is one button
// while Qt's mask is several, and primary is the one content acts on.
std::uint32_t dom_button_held(Qt::MouseButtons buttons) noexcept {
    for (const Qt::MouseButton candidate : {Qt::LeftButton, Qt::MiddleButton, Qt::RightButton,
                                            Qt::BackButton, Qt::ForwardButton}) {
        if (buttons & candidate) return dom_button(candidate);
    }
    return 0;
}

}  // namespace

void MigoQtX11SurfaceView::deliverPointer(std::uint32_t kind, std::uint32_t button,
                                          double timestamp_ms) {
    MigoPointerEvent out{};
    out.struct_size = static_cast<std::uint32_t>(sizeof(out));
    out.abi_version = MIGO_ABI_VERSION_1;
    out.event_type = kind;
    out.button = button;
    out.x = last_pointer_x_;
    out.y = last_pointer_y_;
    out.timestamp_ms = timestamp_ms;
    recordInputResult(migo_session_send_pointer_event(surface_host_.session(), &out));
}

void MigoQtX11SurfaceView::deliverMouseAsTouch(MigoTouchType kind, double timestamp_ms) {
    // mini-game content has no hover concept, so a mouse maps to one finger with id 0
    // and only while a button is held. A free-motion stream would be events no
    // game reads.
    MigoTouchPoint &point = touch_points_[0];
    point.id = 0;
    point.x = last_pointer_x_;
    point.y = last_pointer_y_;
    point.pressure = kind == MIGO_TOUCH_END || kind == MIGO_TOUCH_CANCEL ? 0.0F : 1.0F;
    point.flags = MIGO_TOUCH_FLAG_CHANGED;
    if (kind == MIGO_TOUCH_END || kind == MIGO_TOUCH_CANCEL) {
        point.flags |= MIGO_TOUCH_FLAG_REMOVED;
    }

    MigoTouchEvent out{};
    out.struct_size = static_cast<std::uint32_t>(sizeof(out));
    out.abi_version = MIGO_ABI_VERSION_1;
    out.type = kind;
    out.point_count = 1;
    out.timestamp_ms = static_cast<std::int64_t>(timestamp_ms);
    out.points = touch_points_.data();
    recordInputResult(migo_session_send_touch(surface_host_.session(), &out));
}

// A press or a release names the button that changed state; a move names what is
// still down. Both come from the event, so there is no remembered ordinal to go
// stale between them. While a lock holds the pointer, a press or a release is
// where content's pointer is, not where the hidden cursor went.

void MigoQtX11SurfaceView::mousePressEvent(QMouseEvent *event) {
    QWidget::mousePressEvent(event);
    if (!inputIsDeliverable()) return;
    if (pointer_lock_ == nullptr) {
        last_pointer_x_ = static_cast<float>(event->position().x());
        last_pointer_y_ = static_cast<float>(event->position().y());
    }
    const auto timestamp = static_cast<double>(event->timestamp());
    const bool first_press = !mouse_pressed_;
    mouse_pressed_ = true;
    if (pointer_delivery_ != PointerDelivery::TouchOnly) {
        deliverPointer(MIGO_POINTER_EVENT_DOWN, dom_button(event->button()), timestamp);
    }
    if (pointer_delivery_ != PointerDelivery::MouseOnly && first_press) {
        deliverMouseAsTouch(MIGO_TOUCH_START, timestamp);
    }
}

void MigoQtX11SurfaceView::mouseMoveEvent(QMouseEvent *event) {
    QWidget::mouseMoveEvent(event);
    // Locked, the raw motion moves content's pointer (moveLockedPointer); this is
    // the hidden cursor's.
    if (!inputIsDeliverable() || pointer_lock_ != nullptr) return;
    last_pointer_x_ = static_cast<float>(event->position().x());
    last_pointer_y_ = static_cast<float>(event->position().y());
    const auto timestamp = static_cast<double>(event->timestamp());
    if (pointer_delivery_ != PointerDelivery::TouchOnly) {
        deliverPointer(MIGO_POINTER_EVENT_MOVE, dom_button_held(event->buttons()), timestamp);
    }
    if (pointer_delivery_ != PointerDelivery::MouseOnly && mouse_pressed_) {
        deliverMouseAsTouch(MIGO_TOUCH_MOVE, timestamp);
    }
}

void MigoQtX11SurfaceView::mouseReleaseEvent(QMouseEvent *event) {
    QWidget::mouseReleaseEvent(event);
    if (!inputIsDeliverable()) return;
    if (pointer_lock_ == nullptr) {
        last_pointer_x_ = static_cast<float>(event->position().x());
        last_pointer_y_ = static_cast<float>(event->position().y());
    }
    const auto timestamp = static_cast<double>(event->timestamp());
    const bool was_pressed = mouse_pressed_;
    mouse_pressed_ = false;
    if (pointer_delivery_ != PointerDelivery::TouchOnly) {
        deliverPointer(MIGO_POINTER_EVENT_UP, dom_button(event->button()), timestamp);
    }
    if (pointer_delivery_ != PointerDelivery::MouseOnly && was_pressed) {
        deliverMouseAsTouch(MIGO_TOUCH_END, timestamp);
    }
}

// The pointer lock; what reads the mouse while it holds is PointerLock, above.

bool MigoQtX11SurfaceView::lockPointer() {
    if (pointer_lock_ != nullptr) return true;
    // With the player at the game: the view holds the keyboard -- so Esc, the way
    // out, reaches it -- and the cursor is over the part of it that shows.
    if (!inputIsDeliverable() || !hasFocus()) return false;
    const QPoint origin = QCursor::pos(screen());
    const QPointF position = mapFromGlobal(QPointF(origin));
    if (!visibleRegion().contains(position.toPoint())) return false;
    pointer_lock_ = PointerLock::engage(*this);
    if (pointer_lock_ == nullptr) return false;
    lock_origin_ = origin;
    locked_position_ = position;
    last_pointer_x_ = static_cast<float>(position.x());
    last_pointer_y_ = static_cast<float>(position.y());
    grabMouse(QCursor(Qt::BlankCursor));
    Q_EMIT pointerLockChanged(true);
    return true;
}

void MigoQtX11SurfaceView::unlockPointer() {
    if (pointer_lock_ == nullptr) return;
    pointer_lock_.reset();
    releaseMouse();
    // The motion that moved content's pointer moved the hidden cursor too: it
    // stands where it stood when the lock was taken.
    QCursor::setPos(screen(), lock_origin_);
    Q_EMIT pointerLockChanged(false);
}

void MigoQtX11SurfaceView::moveLockedPointer(QPointF delta, double timestamp_ms) {
    if (!inputIsDeliverable()) return;
    locked_position_ += delta;
    last_pointer_x_ = static_cast<float>(locked_position_.x());
    last_pointer_y_ = static_cast<float>(locked_position_.y());
    if (pointer_delivery_ != PointerDelivery::TouchOnly) {
        deliverPointer(MIGO_POINTER_EVENT_MOVE, dom_button_held(QGuiApplication::mouseButtons()),
                       timestamp_ms);
    }
    if (pointer_delivery_ != PointerDelivery::MouseOnly && mouse_pressed_) {
        deliverMouseAsTouch(MIGO_TOUCH_MOVE, timestamp_ms);
    }
}

void MigoQtX11SurfaceView::wheelEvent(QWheelEvent *event) {
    QWidget::wheelEvent(event);
    if (!inputIsDeliverable()) return;

    MigoWheelEvent out{};
    out.struct_size = static_cast<std::uint32_t>(sizeof(out));
    out.abi_version = MIGO_ABI_VERSION_1;
    out.timestamp_ms = static_cast<double>(event->timestamp());

    // A trackpad reports pixel deltas; a wheel reports eighths of a degree.
    // Neither is converted into the other here: the ABI carries the unit
    // precisely so the host does not have to guess a line height it does not
    // have. The DOM sign convention is the opposite of Qt's -- scrolling the
    // content down is a positive deltaY -- so both axes are negated.
    const QPoint pixels = event->pixelDelta();
    if (!pixels.isNull()) {
        out.delta_mode = MIGO_WHEEL_DELTA_MODE_PIXEL;
        out.delta_x = -static_cast<double>(pixels.x());
        out.delta_y = -static_cast<double>(pixels.y());
    } else {
        const QPoint angle = event->angleDelta();
        if (angle.isNull()) return;
        // One notch is 120 eighths of a degree and scrolls three lines, which
        // is the step every toolkit and browser settled on.
        constexpr double kEighthsPerNotch = 120.0;
        constexpr double kLinesPerNotch = 3.0;
        out.delta_mode = MIGO_WHEEL_DELTA_MODE_LINE;
        out.delta_x = -static_cast<double>(angle.x()) / kEighthsPerNotch * kLinesPerNotch;
        out.delta_y = -static_cast<double>(angle.y()) / kEighthsPerNotch * kLinesPerNotch;
    }
    recordInputResult(migo_session_send_wheel_event(surface_host_.session(), &out));
}

void MigoQtX11SurfaceView::deliverKey(const QKeyEvent &event, std::uint32_t kind) {
    char key_buffer[32];
    const std::size_t key_length =
        detail::dom_key_from_qt_event(event, key_buffer, sizeof(key_buffer));
    const char *code = detail::dom_code_from_x11_keycode(event.nativeScanCode());

    MigoKeyEvent out{};
    out.struct_size = static_cast<std::uint32_t>(sizeof(out));
    out.abi_version = MIGO_ABI_VERSION_1;
    out.event_type = kind;
    out.key_utf8 = key_buffer;
    out.key_length = static_cast<std::uint32_t>(key_length);
    out.code_utf8 = code;
    out.code_length = static_cast<std::uint32_t>(std::strlen(code));
    out.timestamp_ms = static_cast<double>(event.timestamp());
    out.modifiers = dom_modifiers(event.modifiers());
    out.flags = event.isAutoRepeat() ? MIGO_KEY_EVENT_FLAG_REPEAT : MIGO_KEY_EVENT_FLAG_NONE;
    recordInputResult(migo_session_send_key_event(surface_host_.session(), &out));
}

void MigoQtX11SurfaceView::keyPressEvent(QKeyEvent *event) {
    // Esc is the player's way out of a pointer lock, as in a browser: that Esc,
    // its repeats and its release are not the game's.
    if (event->key() == Qt::Key_Escape && (pointer_lock_ != nullptr || escape_released_lock_)) {
        if (pointer_lock_ != nullptr) {
            escape_released_lock_ = true;
            unlockPointer();
        }
        event->accept();
        return;
    }
    if (!inputIsDeliverable()) {
        QWidget::keyPressEvent(event);
        return;
    }
    deliverKey(*event, MIGO_KEY_EVENT_DOWN);
    event->accept();
}

void MigoQtX11SurfaceView::keyReleaseEvent(QKeyEvent *event) {
    if (escape_released_lock_ && event->key() == Qt::Key_Escape) {
        // X11 repeats a held key as a release and a press each time.
        if (!event->isAutoRepeat()) escape_released_lock_ = false;
        event->accept();
        return;
    }
    if (!inputIsDeliverable()) {
        QWidget::keyReleaseEvent(event);
        return;
    }
    deliverKey(*event, MIGO_KEY_EVENT_UP);
    event->accept();
}

void MigoQtX11SurfaceView::deliverTouch(const QTouchEvent &event) {
    const auto &points = event.points();
    if (points.empty()) return;
    // A batch larger than the ABI accepts is refused rather than truncated: the
    // point that would be dropped is one content is tracking.
    const std::size_t count =
        std::min<std::size_t>(points.size(), touch_points_.size());

    MigoTouchType kind = MIGO_TOUCH_MOVE;
    switch (event.type()) {
        case QEvent::TouchBegin:
            kind = MIGO_TOUCH_START;
            break;
        case QEvent::TouchEnd:
            kind = MIGO_TOUCH_END;
            break;
        case QEvent::TouchCancel:
            kind = MIGO_TOUCH_CANCEL;
            break;
        default:
            kind = MIGO_TOUCH_MOVE;
            break;
    }

    for (std::size_t index = 0; index < count; ++index) {
        const QEventPoint &source = points[static_cast<int>(index)];
        MigoTouchPoint &point = touch_points_[index];
        point.id = static_cast<std::uint32_t>(source.id());
        point.x = static_cast<float>(source.position().x());
        point.y = static_cast<float>(source.position().y());
        const float pressure = static_cast<float>(source.pressure());
        // Qt reports -1 for a device without pressure, which the ABI's 0..1
        // range refuses; the touch itself is still real.
        point.pressure = pressure >= 0.0F && pressure <= 1.0F ? pressure : 1.0F;
        point.flags = MIGO_TOUCH_FLAG_NONE;
        if (source.state() != QEventPoint::Stationary) point.flags |= MIGO_TOUCH_FLAG_CHANGED;
        if (source.state() == QEventPoint::Released) {
            point.flags |= MIGO_TOUCH_FLAG_REMOVED;
            point.pressure = 0.0F;
        }
    }

    MigoTouchEvent out{};
    out.struct_size = static_cast<std::uint32_t>(sizeof(out));
    out.abi_version = MIGO_ABI_VERSION_1;
    out.type = kind;
    out.point_count = static_cast<std::uint32_t>(count);
    out.timestamp_ms = static_cast<std::int64_t>(event.timestamp());
    out.points = touch_points_.data();
    recordInputResult(migo_session_send_touch(surface_host_.session(), &out));
}

void MigoQtX11SurfaceView::deliverComposition(std::uint32_t kind, const char *data,
                                              std::uint32_t length) {
    MigoCompositionEvent out{};
    out.struct_size = static_cast<std::uint32_t>(sizeof(out));
    out.abi_version = MIGO_ABI_VERSION_1;
    out.event_type = kind;
    out.data_utf8 = data;
    out.data_length = length;
    recordInputResult(migo_session_send_composition_event(surface_host_.session(), &out));
}

void MigoQtX11SurfaceView::inputMethodEvent(QInputMethodEvent *event) {
    if (!inputIsDeliverable()) {
        QWidget::inputMethodEvent(event);
        return;
    }

    // The IME's two halves are different events, not one: the preedit is what
    // is still being typed and the commit is what was accepted. Content drawing
    // its own field needs both, so neither is inferred from the other.
    //
    // Encoding matters here more than anywhere else on this path: Qt is UTF-16
    // and the ABI is length-delimited UTF-8, and a length that splits a
    // multi-byte character is rejected rather than delivered mangled -- which is
    // exactly what a pinyin preedit is made of.
    const QByteArray commit = event->commitString().toUtf8();
    const QByteArray preedit = event->preeditString().toUtf8();

    if (!preedit.isEmpty()) {
        if (!composing_) {
            composing_ = true;
            deliverComposition(MIGO_COMPOSITION_EVENT_START, preedit.constData(),
                               static_cast<std::uint32_t>(preedit.size()));
        }
        deliverComposition(MIGO_COMPOSITION_EVENT_UPDATE, preedit.constData(),
                           static_cast<std::uint32_t>(preedit.size()));
    }

    if (!commit.isEmpty() || (composing_ && preedit.isEmpty())) {
        // An empty commit while composing is a cancellation, which content must
        // still see so it can clear the preedit it has been drawing.
        if (!composing_) {
            composing_ = true;
            deliverComposition(MIGO_COMPOSITION_EVENT_START, nullptr, 0);
        }
        composing_ = false;
        deliverComposition(MIGO_COMPOSITION_EVENT_END, commit.constData(),
                           static_cast<std::uint32_t>(commit.size()));
    }

    event->accept();
}

QVariant MigoQtX11SurfaceView::inputMethodQuery(Qt::InputMethodQuery query) const {
    switch (query) {
        case Qt::ImEnabled:
            return QVariant(true);
        case Qt::ImCursorRectangle:
            // The engine has no text-field geometry to report, so the candidate
            // window is anchored to the view itself rather than to a caret that
            // does not exist here. Content draws its own field; a wrong caret
            // rectangle would put the candidate list somewhere arbitrary.
            return QVariant(QRect(0, height(), 1, 1));
        case Qt::ImHints:
            return QVariant(static_cast<int>(Qt::ImhNone));
        default:
            return QWidget::inputMethodQuery(query);
    }
}

void MigoQtX11SurfaceView::retractPendingInput() {
    if (!inputIsDeliverable()) {
        mouse_pressed_ = false;
        composing_ = false;
        return;
    }
    if (mouse_pressed_) {
        mouse_pressed_ = false;
        MigoTouchPoint &point = touch_points_[0];
        point.id = 0;
        point.x = last_pointer_x_;
        point.y = last_pointer_y_;
        point.pressure = 0.0F;
        point.flags = MIGO_TOUCH_FLAG_CHANGED | MIGO_TOUCH_FLAG_REMOVED;

        MigoTouchEvent out{};
        out.struct_size = static_cast<std::uint32_t>(sizeof(out));
        out.abi_version = MIGO_ABI_VERSION_1;
        out.type = MIGO_TOUCH_CANCEL;
        out.point_count = 1;
        out.timestamp_ms = 0;
        out.points = touch_points_.data();
        recordInputResult(migo_session_send_touch(surface_host_.session(), &out));
    }
    if (composing_) {
        composing_ = false;
        // An empty end is the DOM's cancellation, which is what a focus loss
        // mid-composition is.
        deliverComposition(MIGO_COMPOSITION_EVENT_END, nullptr, 0);
    }
}

// ---------------------------------------------------------------------------
// Frame clock
// ---------------------------------------------------------------------------

MigoResult MigoQtX11SurfaceView::requestFrame() {
    if (QThread::currentThread() != owner_thread_) return MIGO_ERROR_WRONG_THREAD;
    if (!inputIsDeliverable()) return MIGO_ERROR_INVALID_STATE;
    // Already asked. Qt coalesces its own update requests, and the engine asks
    // again when it wants another frame, so arming twice would report two
    // boundaries for one and run the content's clock at double rate.
    if (frame_requested_) return MIGO_OK;

    QWindow *native_window = windowHandle();
    if (native_window == nullptr) return MIGO_ERROR_INVALID_STATE;
    frame_requested_ = true;
    native_window->requestUpdate();
    return MIGO_OK;
}

void MigoQtX11SurfaceView::notifyFrameBoundary() {
    if (!frame_requested_) return;
    frame_requested_ = false;
    if (!inputIsDeliverable()) return;
    // Qt does not hand the platform's frame timestamp to the update it
    // delivers, so this reports when the boundary was observed. It is the same
    // clock `QElapsedTimer` uses and the closest honest answer available; the
    // engine treats it as a monotonic frame time, not as a display timestamp.
    const auto now = std::chrono::steady_clock::now().time_since_epoch();
    const auto nanos = std::chrono::duration_cast<std::chrono::nanoseconds>(now).count();
    recordInputResult(
        migo_session_notify_vsync(surface_host_.session(), static_cast<std::int64_t>(nanos)));
}

void MigoQtX11SurfaceView::paintEvent(QPaintEvent *event) {
    // The view paints nothing itself -- Migo renders into the native window on
    // its own thread -- so this exists only as the frame boundary Qt delivers
    // after `requestUpdate()`. Not calling the base implementation is
    // deliberate: `WA_PaintOnScreen` with a null paint engine means Qt must not
    // try to back this widget with a surface of its own.
    Q_UNUSED(event);
    notifyFrameBoundary();
}

void MigoQtX11SurfaceView::focusInEvent(QFocusEvent *event) {
    QWidget::focusInEvent(event);
    if (!inputIsDeliverable()) return;
    recordInputResult(migo_session_set_focus(surface_host_.session(), 1));
}

void MigoQtX11SurfaceView::focusOutEvent(QFocusEvent *event) {
    QWidget::focusOutEvent(event);
    // Esc no longer reaches the view: a lock it holds is released.
    unlockPointer();
    // Order matters: the press and the preedit are retracted while content
    // still believes it has focus, so a listener that ignores input while
    // unfocused still sees them end. Held keys are deliberately not synthesized
    // as released -- a browser does not either, and inventing an up for a key
    // the user may still be holding is its own wrong answer.
    retractPendingInput();
    if (!inputIsDeliverable()) return;
    recordInputResult(migo_session_set_focus(surface_host_.session(), 0));
}

bool MigoQtX11SurfaceView::event(QEvent *event) {
    if (owns_surface_ && event->type() == QEvent::PlatformSurface) {
        const auto *surface_event = static_cast<QPlatformSurfaceEvent *>(event);
        if (surface_event->surfaceEventType() ==
                QPlatformSurfaceEvent::SurfaceAboutToBeDestroyed &&
            surface_host_.state() != SurfaceState::Detached) {
            qFatal("Qt attempted to destroy an X11 Surface before Migo reported RELEASED");
        }
    }
    switch (event->type()) {
        case QEvent::UpdateRequest:
            // Qt delivers the answer to `requestUpdate()` here on some paths and
            // as a paint event on others, depending on whether the platform
            // window backs the widget. Both are the same boundary, and
            // `notifyFrameBoundary` reports it once because the pending flag is
            // cleared by whichever arrives first.
            notifyFrameBoundary();
            break;
        case QEvent::TouchBegin:
        case QEvent::TouchUpdate:
        case QEvent::TouchEnd:
        case QEvent::TouchCancel:
            if (inputIsDeliverable()) {
                deliverTouch(*static_cast<QTouchEvent *>(event));
                event->accept();
                return true;
            }
            break;
        default:
            break;
    }
    return QWidget::event(event);
}

}  // namespace migo::linux_host::qt6

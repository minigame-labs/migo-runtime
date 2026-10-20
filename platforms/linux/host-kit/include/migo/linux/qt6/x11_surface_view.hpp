#ifndef MIGO_LINUX_QT6_X11_SURFACE_VIEW_HPP_
#define MIGO_LINUX_QT6_X11_SURFACE_VIEW_HPP_

#include <migo/input.h>
#include <migo/linux/surface_host.hpp>

#include <QElapsedTimer>
#include <QMetaObject>
#include <QPoint>
#include <QPointF>
#include <QTimer>
#include <QWidget>

#include <array>
#include <memory>

class QCloseEvent;
class QFocusEvent;
class QInputMethodEvent;
class QKeyEvent;
class QMouseEvent;
class QPaintEngine;
class QPaintEvent;
class QResizeEvent;
class QShowEvent;
class QThread;
class QTouchEvent;
class QWheelEvent;

namespace migo::linux_host::qt6 {

/// A surface-only Qt 6 Widgets adapter for an App-owned SurfaceHost.
///
/// The widget is a native X11 child window placed by the host's layout. It does
/// not create/configure the Session, install its callback table, load content,
/// or translate input. Those belong to the Managed Host Kit and to the host that
/// selected Bound ownership. Call `close()` (or `beginDetach()` and
/// `pollDetach()`) and wait for `surfaceReleased` before destroying a parent
/// that owns this widget's native window. SurfaceHost and QApplication must
/// each outlive this view. The controller remains at a stable address; keeping
/// it at Session scope preserves
/// the C ABI's strictly increasing Surface generation across replacement views.
/// Only one view may be Attached or Retiring through that controller at once.
/// State/generation accessors are local to this view: a passive replacement
/// never updates, retires, or fail-fast destroys another view's attachment.
/// Once this view attaches, drive that generation through the view methods;
/// bypassing them through SurfaceHost would desynchronize native-window guards.
/// Reparenting can recreate the XID and is allowed only while Detached.
/// A release-query error pauses automatic polling without discarding the
/// observer; call pollDetach() explicitly to retry after handling the error.
/// The complete public API is GUI-thread confined. Foreign control calls return
/// MIGO_ERROR_WRONG_THREAD without touching Qt state or entering Migo.
class MigoQtX11SurfaceView final : public QWidget {
    Q_OBJECT

public:
    /// Which pointer streams a mouse produces.
    ///
    /// The C ABI carries both because a host, not the engine, knows what its
    /// content listens for: mini-game content written for a phone listens for touch,
    /// content written for a PC mini-game platform listens for the mouse, and neither is
    /// synthesized from the other. The default sends both, because a desktop
    /// host most often runs phone-first content that would otherwise receive
    /// nothing at all. Content that listens for both -- rare on mini-game platforms, common in
    /// HTML5 -- would act on one press twice, so it narrows this explicitly.
    enum class PointerDelivery : std::uint8_t { TouchAndMouse, TouchOnly, MouseOnly };

    MigoQtX11SurfaceView(SurfaceHost &surface_host, QWidget &parent);
    ~MigoQtX11SurfaceView() override;

    MigoQtX11SurfaceView(const MigoQtX11SurfaceView &) = delete;
    MigoQtX11SurfaceView &operator=(const MigoQtX11SurfaceView &) = delete;

    [[nodiscard]] MigoResult attachSurface();
    [[nodiscard]] MigoResult beginDetach();
    [[nodiscard]] MigoResult pollDetach(bool *released);

    void setPointerDelivery(PointerDelivery delivery) noexcept { pointer_delivery_ = delivery; }
    [[nodiscard]] PointerDelivery pointerDelivery() const noexcept { return pointer_delivery_; }

    /// Hold the pointer for the game: the C ABI's window service
    /// (REQUEST_POINTER_LOCK). The cursor is hidden and the mouse grabbed, and
    /// the mouse's motion -- XInput 2's raw motion, after the pointer's
    /// acceleration -- moves content's pointer from where the cursor stood,
    /// past the view's edges if it goes there, so movementX/movementY keep
    /// moving while the cursor shows nothing.
    ///
    /// Taken only while the view is attached, its window active and the cursor
    /// over the view: a lock taken with the cursor elsewhere would send the
    /// player's clicks wherever it stands. Released by Esc -- the player's way
    /// out, as in a browser; that Esc is not the game's -- by the view losing
    /// focus, by a detach, and by `unlockPointer()`; the cursor then stands where
    /// it stood. Returns whether a lock is held now; `pointerLockChanged` says
    /// when that changes.
    [[nodiscard]] bool lockPointer();
    void unlockPointer();
    [[nodiscard]] bool isPointerLocked() const noexcept { return pointer_lock_ != nullptr; }

    /// Ask the toolkit for one frame, in response to the engine asking for one.
    ///
    /// The App calls this from its own `on_request_frame` callback. The view
    /// does not install that callback itself: the callback table is the App's,
    /// installable once per Session, and a Host Kit that claimed it would
    /// decide the App's frame policy for it.
    ///
    /// One request produces at most one `migo_session_notify_vsync`. A second
    /// call before the frame arrives coalesces into the first rather than
    /// queueing another, because the engine asks again when it wants another
    /// frame and two notifications for one request would run the content's
    /// clock at double speed.
    ///
    /// Paced by `QWindow::requestUpdate()`, which is Qt's own frame clock.
    /// A fixed-interval timer would be a second, unsynchronised clock competing
    /// with the compositor's.
    [[nodiscard]] MigoResult requestFrame();

    /// Whether a requested frame has not yet been reported to the engine.
    [[nodiscard]] bool isFramePending() const noexcept { return frame_requested_; }

    /// The frames reported to the engine so far: what a debugging panel's
    /// frames a second are counted from.
    [[nodiscard]] std::uint64_t framesDelivered() const noexcept { return frames_delivered_; }

    [[nodiscard]] SurfaceState surfaceState() const noexcept {
        return owns_surface_ ? surface_host_.state() : SurfaceState::Detached;
    }
    [[nodiscard]] std::uint64_t generation() const noexcept { return owned_generation_; }
    [[nodiscard]] MigoResult lastError() const noexcept { return last_error_; }
    [[nodiscard]] bool isReleasePolling() const noexcept { return release_timer_.isActive(); }

Q_SIGNALS:
    void surfaceAttached(quint64 generation);
    void surfaceReleased(quint64 generation);
    void surfaceReleaseStalled(quint64 generation, qint64 elapsed_ms);
    void surfaceError(MigoResult error);
    /// One input event was refused. Reported separately from `surfaceError`
    /// because a full queue is backpressure, not a Surface fault, and a host
    /// that treats the two alike would tear down a working Surface.
    void inputRejected(MigoResult error);
    /// A pointer lock was taken or released.
    void pointerLockChanged(bool locked);

protected:
    [[nodiscard]] QPaintEngine *paintEngine() const override;
    void showEvent(QShowEvent *event) override;
    void resizeEvent(QResizeEvent *event) override;
    void closeEvent(QCloseEvent *event) override;
    void paintEvent(QPaintEvent *event) override;
    void mousePressEvent(QMouseEvent *event) override;
    void mouseMoveEvent(QMouseEvent *event) override;
    void mouseReleaseEvent(QMouseEvent *event) override;
    void wheelEvent(QWheelEvent *event) override;
    void keyPressEvent(QKeyEvent *event) override;
    void keyReleaseEvent(QKeyEvent *event) override;
    void focusInEvent(QFocusEvent *event) override;
    void focusOutEvent(QFocusEvent *event) override;
    void inputMethodEvent(QInputMethodEvent *event) override;
    [[nodiscard]] QVariant inputMethodQuery(Qt::InputMethodQuery query) const override;
    bool event(QEvent *event) override;

private:
    [[nodiscard]] MigoResult currentMetrics(SurfaceMetrics *metrics) const noexcept;
    void scheduleMetricsUpdate();
    void updateReleasePollCadence();
    void recordError(MigoResult error);

    /// True while this view owns a live attachment, which is the only state in
    /// which the ABI accepts input at all.
    [[nodiscard]] bool inputIsDeliverable() const noexcept;
    void recordInputResult(MigoResult result);
    void deliverPointer(std::uint32_t kind, std::uint32_t button, double timestamp_ms);
    void deliverMouseAsTouch(MigoTouchType kind, double timestamp_ms);
    /// The mouse moved while a lock holds the pointer: `delta` in the screen's
    /// pixels moves content's pointer.
    void moveLockedPointer(QPointF delta, double timestamp_ms);
    void deliverKey(const QKeyEvent &event, std::uint32_t kind);
    void deliverTouch(const QTouchEvent &event);
    void deliverComposition(std::uint32_t kind, const char *data, std::uint32_t length);
    /// Report the frame boundary the toolkit just delivered.
    void notifyFrameBoundary();
    /// Retract whatever the content still believes is in progress.
    ///
    /// A press or a preedit that never ends leaves content waiting forever, and
    /// no later event corrects it -- the same reason the ABI reports a full
    /// queue instead of dropping an END.
    void retractPendingInput();

    SurfaceHost &surface_host_;
    QThread *const owner_thread_;
    // Parented in the constructor body, not here. `QTimer(this)` calls
    // QObject::setParent, which sends a ChildAdded event synchronously -- and
    // because `event()` is overridden, that reaches this class before the
    // members declared after these two have been initialised. Reading one there
    // is undefined behaviour; on the heap it reads the allocator's fill byte.
    QTimer release_timer_;
    QTimer metrics_update_timer_;
    /// The attach a show schedules: it runs once Qt has mapped the native
    /// window and applied its geometry, which it does after the show event.
    QTimer attach_timer_;
    QElapsedTimer release_elapsed_;
    MigoResult last_error_ = MIGO_OK;
    std::uint64_t owned_generation_ = 0;
    bool owns_surface_ = false;
    bool close_pending_ = false;
    bool release_stall_reported_ = false;
    QMetaObject::Connection screen_changed_connection_;

    PointerDelivery pointer_delivery_ = PointerDelivery::TouchAndMouse;
    /// Fixed storage for one touch batch. The ABI's maximum is small and known,
    /// so the delivery path never allocates however many fingers arrive.
    std::array<MigoTouchPoint, MIGO_TOUCH_MAX_POINTS> touch_points_{};
    /// True while a mouse button is down, so a focus loss can retract the press
    /// the content is still waiting to see released. Which button is held is not
    /// tracked: the ABI asks a move to name the button being held, and the event
    /// already carries that, whereas a remembered ordinal outlives its release.
    bool mouse_pressed_ = false;
    float last_pointer_x_ = 0.0F;
    float last_pointer_y_ = 0.0F;
    /// True between compositionstart and compositionend.
    bool composing_ = false;
    /// True between a frame request and the frame boundary it produced.
    bool frame_requested_ = false;
    std::uint64_t frames_delivered_ = 0;

    /// What a held pointer lock reads the mouse with; null while none is held.
    class PointerLock;
    std::unique_ptr<PointerLock> pointer_lock_;
    /// Where content's pointer is while a lock holds it, in the view's (CSS) pixels.
    QPointF locked_position_;
    /// Where the cursor stood when the lock was taken, on the screen: where it stands again.
    QPoint lock_origin_;
    /// The Esc that released a lock: its release is not the game's either.
    bool escape_released_lock_ = false;
};

}  // namespace migo::linux_host::qt6

#endif  // MIGO_LINUX_QT6_X11_SURFACE_VIEW_HPP_

#include <migo/linux/qt6/host_services.hpp>

#include "backlight.hpp"
#include "bluetooth.hpp"
#include "geolocation.hpp"
#include "images.hpp"
#include "motion.hpp"
#include "overlay.hpp"
#include "sandbox.hpp"

#include <QAction>
#include <QClipboard>
#include <QCursor>
#include <QDialog>
#include <QDialogButtonBox>
#include <QEvent>
#include <QGuiApplication>
#include <QHash>
#include <QHBoxLayout>
#include <QJsonArray>
#include <QJsonDocument>
#include <QJsonValue>
#include <QLabel>
#include <QLineEdit>
#include <QList>
#include <QMenu>
#include <QPixmap>
#include <QProgressBar>
#include <QPushButton>
#include <QScreen>
#include <QStyle>
#include <QTimer>
#include <QVBoxLayout>
#include <QWidget>

#include <chrono>
#include <cmath>
#include <optional>
#include <utility>

namespace migo::linux_host::qt6 {

// ---- the channel and its answers -------------------------------------------------------------

std::shared_ptr<MigoQtHostServiceChannel> MigoQtHostServiceChannel::open(MigoSession *session) {
    return std::shared_ptr<MigoQtHostServiceChannel>(new MigoQtHostServiceChannel(session));
}

void MigoQtHostServiceChannel::close() noexcept {
    const std::lock_guard<std::mutex> lock(mutex_);
    session_ = nullptr;
}

MigoQtHostServiceReply MigoQtHostServiceChannel::reply(std::uint64_t call_id) {
    return MigoQtHostServiceReply(shared_from_this(), call_id);
}

MigoResult MigoQtHostServiceChannel::post(MigoHostService service, std::uint32_t event,
                                          const QJsonObject &payload) {
    const QByteArray json = QJsonDocument(payload).toJson(QJsonDocument::Compact);
    const std::lock_guard<std::mutex> lock(mutex_);
    if (session_ == nullptr) return MIGO_OK;
    return migo_session_post_host_service_event(session_, service, event, json.constData(),
                                                static_cast<std::uint32_t>(json.size()));
}

MigoResult MigoQtHostServiceChannel::setScopeState(MigoScope scope, MigoScopeState state) {
    const std::lock_guard<std::mutex> lock(mutex_);
    if (session_ == nullptr) return MIGO_OK;
    return migo_session_set_scope_state(session_, scope, state);
}

MigoResult MigoQtHostServiceChannel::postSensorSample(const MigoSensorSample &sample) {
    const std::lock_guard<std::mutex> lock(mutex_);
    if (session_ == nullptr) return MIGO_OK;
    return migo_session_post_sensor_sample(session_, &sample);
}

MigoResult MigoQtHostServiceChannel::postBleCharacteristicValue(const QString &device, const QString &service,
                                                               const QString &characteristic, const QByteArray &value) {
    const QByteArray device_id = device.toUtf8();
    const QByteArray service_id = service.toUtf8();
    const QByteArray characteristic_id = characteristic.toUtf8();
    MigoBleCharacteristicValue posted{};
    posted.struct_size = static_cast<std::uint32_t>(sizeof(posted));
    posted.abi_version = MIGO_ABI_VERSION_1;
    posted.device_id_utf8 = device_id.constData();
    posted.device_id_length = static_cast<std::uint32_t>(device_id.size());
    posted.service_id_utf8 = service_id.constData();
    posted.service_id_length = static_cast<std::uint32_t>(service_id.size());
    posted.characteristic_id_utf8 = characteristic_id.constData();
    posted.characteristic_id_length = static_cast<std::uint32_t>(characteristic_id.size());
    posted.value = reinterpret_cast<const std::uint8_t *>(value.constData());
    posted.value_length = static_cast<std::uint32_t>(value.size());
    const std::lock_guard<std::mutex> lock(mutex_);
    if (session_ == nullptr) return MIGO_OK;
    return migo_session_post_ble_characteristic_value(session_, &posted);
}

MigoResult MigoQtHostServiceChannel::complete(std::uint64_t call_id, const MigoHostServiceResult &result) {
    // Held across the call so close() cannot return -- and the Session be
    // destroyed -- while an answer is still on its way in.
    const std::lock_guard<std::mutex> lock(mutex_);
    if (session_ == nullptr) return MIGO_OK;
    return migo_session_complete_host_service_call(session_, call_id, &result);
}

MigoQtHostServiceReply::MigoQtHostServiceReply(std::shared_ptr<MigoQtHostServiceChannel> channel,
                                               std::uint64_t call_id)
    : channel_(std::move(channel)), call_id_(call_id), answered_(std::make_shared<std::atomic<bool>>(false)) {}

MigoResult MigoQtHostServiceReply::answer(const MigoHostServiceResult &result) {
    if (call_id_ == 0) return MIGO_OK;
    if (answered_->exchange(true, std::memory_order_acq_rel)) return MIGO_ERROR_INVALID_STATE;
    return channel_->complete(call_id_, result);
}

MigoResult MigoQtHostServiceReply::succeed(const QJsonObject &result) {
    const QByteArray json = QJsonDocument(result).toJson(QJsonDocument::Compact);
    MigoHostServiceResult outcome{};
    outcome.struct_size = static_cast<std::uint32_t>(sizeof(outcome));
    outcome.abi_version = MIGO_ABI_VERSION_1;
    outcome.status = MIGO_HOST_SERVICE_STATUS_OK;
    outcome.payload_json_utf8 = json.constData();
    outcome.payload_length = static_cast<std::uint32_t>(json.size());
    return answer(outcome);
}

MigoResult MigoQtHostServiceReply::fail(const QString &reason, std::optional<std::int32_t> error_code) {
    // Refused before the answer is spent: the library refuses a failure with no
    // reason, and spending the one answer on that would leave content waiting.
    if (reason.isEmpty()) return MIGO_ERROR_INVALID_ARGUMENT;
    const QByteArray message = reason.toUtf8();
    MigoHostServiceResult outcome{};
    outcome.struct_size = static_cast<std::uint32_t>(sizeof(outcome));
    outcome.abi_version = MIGO_ABI_VERSION_1;
    outcome.status = MIGO_HOST_SERVICE_STATUS_FAIL;
    outcome.message_utf8 = message.constData();
    outcome.message_length = static_cast<std::uint32_t>(message.size());
    if (error_code.has_value()) {
        outcome.flags = MIGO_HOST_SERVICE_RESULT_FLAG_ERROR_CODE;
        outcome.error_code = *error_code;
    }
    return answer(outcome);
}

// ---- the interaction service ------------------------------------------------------------------

namespace {

/// The platform's own button texts, which content's call carries anyway.
const QString kCancel = QStringLiteral("\u53D6\u6D88");
const QString kConfirm = QStringLiteral("\u786E\u5B9A");
const QString kNoWindow = QStringLiteral("there is no window to show it in");

bool isInput(QEvent::Type type) noexcept {
    switch (type) {
        case QEvent::MouseButtonPress:
        case QEvent::MouseButtonRelease:
        case QEvent::MouseButtonDblClick:
        case QEvent::MouseMove:
        case QEvent::Wheel:
        case QEvent::KeyPress:
        case QEvent::KeyRelease:
        case QEvent::TouchBegin:
        case QEvent::TouchUpdate:
        case QEvent::TouchEnd:
        case QEvent::TouchCancel:
        case QEvent::InputMethod:
            return true;
        default:
            return false;
    }
}

}  // namespace

class MigoQtBuiltInHostServices::Interaction final : public QObject {
public:
    explicit Interaction(QWidget &surface) : surface_(&surface) {
        expiry_.setSingleShot(true);
        connect(&expiry_, &QTimer::timeout, this, [this] { hideIndicator(); });
        surface.installEventFilter(this);
        if (QWidget *window = surface.window(); window != &surface) window->installEventFilter(this);
    }

    ~Interaction() override {
        hideIndicator();
        // A dialog or menu still up is closed with the service that showed it;
        // the answer it gives on closing reaches nobody once the channel is.
        for (const QPointer<QWidget> &open : std::as_const(open_)) {
            if (!open.isNull()) open->close();
        }
    }

    void call(std::uint32_t method, const QJsonObject &request, MigoQtHostServiceReply reply) {
        switch (method) {
            case MIGO_INTERACTION_SHOW_TOAST:
                showIndicator(Shown::Toast, request.value(QStringLiteral("title")).toString(),
                              request.value(QStringLiteral("icon")).toString(QStringLiteral("success")),
                              request.value(QStringLiteral("mask")).toBool(false),
                              request.value(QStringLiteral("duration")).toDouble(1500));
                return;
            case MIGO_INTERACTION_HIDE_TOAST:
                if (shown_ == Shown::Toast) hideIndicator();
                return;
            case MIGO_INTERACTION_SHOW_LOADING:
                showIndicator(Shown::Loading, request.value(QStringLiteral("title")).toString(),
                              QStringLiteral("loading"), request.value(QStringLiteral("mask")).toBool(false),
                              std::nullopt);
                return;
            case MIGO_INTERACTION_HIDE_LOADING:
                if (shown_ == Shown::Loading) hideIndicator();
                return;
            case MIGO_INTERACTION_SHOW_MODAL:
                showModal(request, std::move(reply));
                return;
            case MIGO_INTERACTION_SHOW_ACTION_SHEET:
                showActionSheet(request, std::move(reply));
                return;
            default:
                (void)reply.fail(QStringLiteral("not supported"));
                return;
        }
    }

    [[nodiscard]] QString indicatorTitle() const { return indicator_.isNull() ? QString() : title_; }

protected:
    bool eventFilter(QObject *watched, QEvent *event) override {
        if (watched == surface_.data() && masked_ && isInput(event->type())) return true;
        if (!indicator_.isNull() && (event->type() == QEvent::Move || event->type() == QEvent::Resize)) {
            placeIndicator();
        }
        return QObject::eventFilter(watched, event);
    }

private:
    enum class Shown { None, Toast, Loading };

    void showIndicator(Shown kind, const QString &title, const QString &icon, bool mask,
                       std::optional<double> duration_ms) {
        hideIndicator();
        if (surface_.isNull()) return;
        // Over the game, taking none of its input: a mask keeps that from the
        // game, not the indicator.
        auto *panel = new QWidget(surface_->window(),
                                  Qt::ToolTip | Qt::FramelessWindowHint | Qt::WindowDoesNotAcceptFocus);
        panel->setObjectName(QStringLiteral("migoIndicator"));
        panel->setStyleSheet(QStringLiteral(
            "#migoIndicator { background: rgba(32, 32, 32, 230); border-radius: 8px; }"
            " QLabel { color: white; }"));
        auto *layout = new QVBoxLayout(panel);
        layout->setContentsMargins(20, 16, 20, 16);
        layout->setSpacing(10);
        if (icon == QLatin1String("loading")) {
            auto *busy = new QProgressBar(panel);
            busy->setRange(0, 0);
            busy->setTextVisible(false);
            busy->setFixedWidth(96);
            layout->addWidget(busy, 0, Qt::AlignHCenter);
        } else if (icon == QLatin1String("success") || icon == QLatin1String("error")) {
            auto *glyph = new QLabel(panel);
            const QStyle::StandardPixmap pixmap = icon == QLatin1String("success")
                                                      ? QStyle::SP_DialogApplyButton
                                                      : QStyle::SP_MessageBoxCritical;
            glyph->setPixmap(panel->style()->standardIcon(pixmap).pixmap(40, 40));
            layout->addWidget(glyph, 0, Qt::AlignHCenter);
        }
        if (!title.isEmpty()) {
            auto *text = new QLabel(title, panel);
            text->setWordWrap(true);
            text->setAlignment(Qt::AlignCenter);
            text->setMaximumWidth(280);
            layout->addWidget(text);
        }
        indicator_ = panel;
        title_ = title;
        shown_ = kind;
        masked_ = mask;
        panel->adjustSize();
        placeIndicator();
        showOverTheGame(*panel);
        if (duration_ms.has_value()) expiry_.start(static_cast<int>(qMax(0.0, *duration_ms)));
    }

    void hideIndicator() {
        expiry_.stop();
        shown_ = Shown::None;
        masked_ = false;
        title_.clear();
        if (!indicator_.isNull()) delete indicator_.data();
    }

    void placeIndicator() {
        if (indicator_.isNull() || surface_.isNull()) return;
        const QPoint centre = surface_->mapToGlobal(surface_->rect().center());
        indicator_->move(centre - QPoint(indicator_->width() / 2, indicator_->height() / 2));
    }

    void showModal(const QJsonObject &request, MigoQtHostServiceReply reply) {
        if (surface_.isNull()) {
            (void)reply.fail(kNoWindow);
            return;
        }
        const QString title = request.value(QStringLiteral("title")).toString();
        const QString content = request.value(QStringLiteral("content")).toString();
        const bool editable = request.value(QStringLiteral("editable")).toBool(false);

        auto *dialog = new QDialog(surface_->window());
        dialog->setAttribute(Qt::WA_DeleteOnClose);
        dialog->setWindowModality(Qt::WindowModal);
        dialog->setWindowTitle(title.isEmpty() ? QGuiApplication::applicationDisplayName() : title);
        auto *layout = new QVBoxLayout(dialog);
        if (!title.isEmpty()) {
            auto *heading = new QLabel(title, dialog);
            QFont font = heading->font();
            font.setBold(true);
            heading->setFont(font);
            heading->setWordWrap(true);
            layout->addWidget(heading);
        }
        // Editable, the content is what the field starts with; otherwise the message.
        QPointer<QLineEdit> field;
        if (editable) {
            field = new QLineEdit(content, dialog);
            field->setPlaceholderText(request.value(QStringLiteral("placeholderText")).toString());
            layout->addWidget(field);
        } else if (!content.isEmpty()) {
            auto *message = new QLabel(content, dialog);
            message->setWordWrap(true);
            layout->addWidget(message);
        }
        auto *buttons = new QDialogButtonBox(dialog);
        if (request.value(QStringLiteral("showCancel")).toBool(true)) {
            buttons->addButton(request.value(QStringLiteral("cancelText")).toString(kCancel),
                               QDialogButtonBox::RejectRole);
        }
        QPushButton *confirm = buttons->addButton(
            request.value(QStringLiteral("confirmText")).toString(kConfirm), QDialogButtonBox::AcceptRole);
        confirm->setDefault(true);
        layout->addWidget(buttons);
        connect(buttons, &QDialogButtonBox::accepted, dialog, &QDialog::accept);
        connect(buttons, &QDialogButtonBox::rejected, dialog, &QDialog::reject);
        // Closing the dialog any other way is the player declining it.
        connect(dialog, &QDialog::finished, dialog, [reply, field, editable](int code) mutable {
            const bool confirmed = code == QDialog::Accepted;
            QJsonObject answer{{QStringLiteral("confirm"), confirmed}, {QStringLiteral("cancel"), !confirmed}};
            if (editable) answer.insert(QStringLiteral("content"), field.isNull() ? QString() : field->text());
            (void)reply.succeed(answer);
        });
        open_.append(dialog);
        dialog->open();
    }

    void showActionSheet(const QJsonObject &request, MigoQtHostServiceReply reply) {
        const QJsonArray items = request.value(QStringLiteral("itemList")).toArray();
        if (items.isEmpty()) {
            (void)reply.fail(QStringLiteral("itemList is empty"));
            return;
        }
        if (surface_.isNull()) {
            (void)reply.fail(kNoWindow);
            return;
        }
        auto *menu = new QMenu(surface_->window());
        menu->setAttribute(Qt::WA_DeleteOnClose);
        const QString alert = request.value(QStringLiteral("alertText")).toString();
        if (!alert.isEmpty()) menu->addSection(alert);
        for (int index = 0; index < items.size(); ++index) {
            menu->addAction(items.at(index).toString())->setData(index);
        }
        // A menu hides before it reports the item chosen, in the same turn; the
        // answer waits for that turn to end, and no item by then is a dismissal.
        auto chosen = std::make_shared<int>(-1);
        connect(menu, &QMenu::triggered, menu, [chosen](QAction *action) { *chosen = action->data().toInt(); });
        connect(menu, &QMenu::aboutToHide, menu, [chosen, reply]() mutable {
            QTimer::singleShot(0, [chosen, reply]() mutable {
                if (*chosen >= 0) {
                    (void)reply.succeed({{QStringLiteral("tapIndex"), *chosen}});
                } else {
                    (void)reply.fail(QStringLiteral("cancel"));
                }
            });
        });
        open_.append(menu);
        const QSize size = menu->sizeHint();
        menu->popup(surface_->mapToGlobal(surface_->rect().center()) - QPoint(size.width() / 2, size.height() / 2));
    }

    QPointer<QWidget> surface_;
    Shown shown_ = Shown::None;
    bool masked_ = false;
    QString title_;
    QPointer<QWidget> indicator_;
    QTimer expiry_;
    QList<QPointer<QWidget>> open_;
};

// ---- the window service -----------------------------------------------------------------------

namespace {

/// The screen's orientation on a desktop: the game's shape. Square is
/// landscape, as a phone's screen is never square.
QString orientationOf(QSize game) {
    return game.height() > game.width() ? QStringLiteral("portrait") : QStringLiteral("landscape");
}

/// A window's state in the platform's words: full screen is maximized too.
QString stateOf(const QWidget &window) {
    if (window.isMinimized()) return QStringLiteral("minimize");
    if (window.isMaximized() || window.isFullScreen()) return QStringLiteral("maximize");
    return QStringLiteral("normalize");
}

/// A CSS cursor keyword as Qt's cursor, or none for the system's own. What Qt
/// has no cursor for -- context-menu, cell, the zooms -- is the arrow.
std::optional<Qt::CursorShape> cursorShape(const QString &keyword) {
    static const QHash<QString, Qt::CursorShape> shapes = {
        {QStringLiteral("none"), Qt::BlankCursor},
        {QStringLiteral("pointer"), Qt::PointingHandCursor},
        {QStringLiteral("text"), Qt::IBeamCursor},
        {QStringLiteral("vertical-text"), Qt::IBeamCursor},
        {QStringLiteral("crosshair"), Qt::CrossCursor},
        {QStringLiteral("wait"), Qt::WaitCursor},
        {QStringLiteral("progress"), Qt::BusyCursor},
        {QStringLiteral("help"), Qt::WhatsThisCursor},
        {QStringLiteral("move"), Qt::SizeAllCursor},
        {QStringLiteral("all-scroll"), Qt::SizeAllCursor},
        {QStringLiteral("not-allowed"), Qt::ForbiddenCursor},
        {QStringLiteral("no-drop"), Qt::ForbiddenCursor},
        {QStringLiteral("grab"), Qt::OpenHandCursor},
        {QStringLiteral("grabbing"), Qt::ClosedHandCursor},
        {QStringLiteral("copy"), Qt::DragCopyCursor},
        {QStringLiteral("alias"), Qt::DragLinkCursor},
        {QStringLiteral("n-resize"), Qt::SizeVerCursor},
        {QStringLiteral("s-resize"), Qt::SizeVerCursor},
        {QStringLiteral("ns-resize"), Qt::SizeVerCursor},
        {QStringLiteral("e-resize"), Qt::SizeHorCursor},
        {QStringLiteral("w-resize"), Qt::SizeHorCursor},
        {QStringLiteral("ew-resize"), Qt::SizeHorCursor},
        {QStringLiteral("row-resize"), Qt::SplitVCursor},
        {QStringLiteral("col-resize"), Qt::SplitHCursor},
        {QStringLiteral("ne-resize"), Qt::SizeBDiagCursor},
        {QStringLiteral("sw-resize"), Qt::SizeBDiagCursor},
        {QStringLiteral("nesw-resize"), Qt::SizeBDiagCursor},
        {QStringLiteral("nw-resize"), Qt::SizeFDiagCursor},
        {QStringLiteral("se-resize"), Qt::SizeFDiagCursor},
        {QStringLiteral("nwse-resize"), Qt::SizeFDiagCursor},
    };
    if (keyword == QLatin1String("default") || keyword == QLatin1String("auto")) return std::nullopt;
    return shapes.value(keyword, Qt::ArrowCursor);
}

}  // namespace

class MigoQtBuiltInHostServices::WindowService final : public QObject {
public:
    WindowService(MigoQtX11SurfaceView &view, std::shared_ptr<MigoQtHostServiceChannel> channel)
        : view_(&view),
          channel_(std::move(channel)),
          reported_(stateOf(*view.window())),
          reported_orientation_(orientationOf(view.size())) {
        view.window()->installEventFilter(this);
        // The game turns when its view does, whatever turned it: the window
        // dragged to the other shape, the App's layout, or setDeviceOrientation.
        view.installEventFilter(this);
        // A lock taken, or released -- by content, by the player's Esc, by the
        // view losing the keyboard -- is reported as it happens.
        connect(&view, &MigoQtX11SurfaceView::pointerLockChanged, this, [this](bool locked) {
            (void)channel_->post(MIGO_HOST_SERVICE_WINDOW, MIGO_WINDOW_EVENT_POINTER_LOCK_CHANGE,
                                 {{QStringLiteral("locked"), locked}});
        });
    }

    ~WindowService() override {
        // The game's lock goes with the game.
        if (!view_.isNull()) view_->unlockPointer();
    }

    void call(std::uint32_t method, const QJsonObject &request, MigoQtHostServiceReply reply) {
        switch (method) {
            case MIGO_WINDOW_SET_WINDOW_SIZE:
                setWindowSize(request, std::move(reply));
                return;
            case MIGO_WINDOW_SET_CURSOR:
                setCursor(request);
                return;
            case MIGO_WINDOW_REQUEST_POINTER_LOCK:
                // A lock taken is reported by the view; a refusal -- the player is
                // not at the game -- here. A lock already held is no change.
                if (view_.isNull() || !view_->lockPointer()) {
                    (void)channel_->post(MIGO_HOST_SERVICE_WINDOW, MIGO_WINDOW_EVENT_POINTER_LOCK_CHANGE,
                                         {{QStringLiteral("locked"), false}});
                }
                return;
            case MIGO_WINDOW_EXIT_POINTER_LOCK:
                if (!view_.isNull()) view_->unlockPointer();
                return;
            default:
                (void)reply.fail(QStringLiteral("not supported"));
                return;
        }
    }

    /// setDeviceOrientation: the game's window turned to the shape asked for,
    /// the view's width and height exchanged when it is the other shape.
    /// Content hears the turn as the view turns.
    void setOrientation(const QJsonObject &request, MigoQtHostServiceReply reply) {
        const QString value = request.value(QStringLiteral("value")).toString();
        if (value != QLatin1String("portrait") && value != QLatin1String("landscape")) {
            (void)reply.fail(QStringLiteral("value must be portrait or landscape"));
            return;
        }
        if (view_.isNull()) {
            (void)reply.fail(QStringLiteral("there is no window to turn"));
            return;
        }
        QWidget *window = view_->window();
        if (window->isFullScreen()) {
            (void)reply.fail(QStringLiteral("the window is full screen"));
            return;
        }
        const QSize game = view_->size();
        if (orientationOf(game) == value) {
            (void)reply.succeed();
            return;
        }
        const QSize turned = window->size() - game + game.transposed();
        // The window's frame, which the window manager draws, comes with it.
        const QSize frame = window->frameGeometry().size() - window->size();
        if (const QScreen *screen = window->screen(); screen != nullptr) {
            const QSize room = screen->availableGeometry().size();
            if (turned.width() + frame.width() > room.width() || turned.height() + frame.height() > room.height()) {
                (void)reply.fail(QStringLiteral("the screen cannot hold the window turned"));
                return;
            }
        }
        window->resize(turned);
        (void)reply.succeed();
    }

protected:
    bool eventFilter(QObject *watched, QEvent *event) override {
        if (event->type() == QEvent::Resize && !view_.isNull() && watched == view_.data()) {
            reportOrientation();
        }
        if (event->type() == QEvent::WindowStateChange && !view_.isNull() && watched == view_->window()) {
            const QString state = stateOf(*view_->window());
            if (state != reported_) {
                reported_ = state;
                (void)channel_->post(MIGO_HOST_SERVICE_WINDOW, MIGO_WINDOW_EVENT_WINDOW_STATE_CHANGE,
                                     {{QStringLiteral("state"), state}});
            }
        }
        return QObject::eventFilter(watched, event);
    }

private:
    /// The size asked for is the game's, in pixels: the window grows or shrinks
    /// by what the game's view must.
    void setWindowSize(const QJsonObject &request, MigoQtHostServiceReply reply) {
        const double width = request.value(QStringLiteral("width")).toDouble(-1);
        const double height = request.value(QStringLiteral("height")).toDouble(-1);
        if (!std::isfinite(width) || !std::isfinite(height) || width <= 0 || height <= 0) {
            (void)reply.fail(QStringLiteral("width and height must be positive numbers"));
            return;
        }
        if (view_.isNull()) {
            (void)reply.fail(QStringLiteral("there is no window to size"));
            return;
        }
        QWidget *window = view_->window();
        if (window->isFullScreen()) {
            (void)reply.fail(QStringLiteral("the window is full screen"));
            return;
        }
        const qreal ratio = view_->devicePixelRatioF();
        const QSize game(qMax(1, qRound(width / ratio)), qMax(1, qRound(height / ratio)));
        window->resize(window->size() - view_->size() + game);
        (void)reply.succeed();
    }

    void setCursor(const QJsonObject &request) {
        if (view_.isNull()) return;
        if (request.contains(QStringLiteral("keyword"))) {
            const std::optional<Qt::CursorShape> shape = cursorShape(request.value(QStringLiteral("keyword")).toString());
            if (shape.has_value()) {
                view_->setCursor(*shape);
            } else {
                view_->unsetCursor();
            }
            return;
        }
        const QString path = request.value(QStringLiteral("path")).toString();
        const QPixmap image(path);
        if (image.isNull()) {
            qWarning("migo host kit: setCursor: %s is not an image Qt reads; the cursor is unchanged", qPrintable(path));
            return;
        }
        // The hotspot is in the image's pixels, clamped into it.
        const int x = qBound(0, request.value(QStringLiteral("x")).toInt(0), image.width() - 1);
        const int y = qBound(0, request.value(QStringLiteral("y")).toInt(0), image.height() - 1);
        view_->setCursor(QCursor(image, x, y));
    }

    /// Content hears the game turn when its view crosses from one shape to
    /// the other, as a turn is heard on a phone -- not on every resize.
    void reportOrientation() {
        const QString orientation = orientationOf(view_->size());
        if (orientation == reported_orientation_) return;
        reported_orientation_ = orientation;
        (void)channel_->post(MIGO_HOST_SERVICE_SCREEN, MIGO_SCREEN_EVENT_DEVICE_ORIENTATION_CHANGE,
                             {{QStringLiteral("value"), orientation}});
    }

    QPointer<MigoQtX11SurfaceView> view_;
    std::shared_ptr<MigoQtHostServiceChannel> channel_;
    /// The window's state last reported, or the one the game started in.
    QString reported_;
    /// The screen's orientation last reported -- the game's shape -- or the one
    /// the game started in.
    QString reported_orientation_;
};

// ---- the built-in services --------------------------------------------------------------------

MigoQtBuiltInHostServices::MigoQtBuiltInHostServices(MigoQtX11SurfaceView &view,
                                                     std::shared_ptr<MigoQtHostServiceChannel> channel,
                                                     QObject *parent)
    : QObject(parent),
      surface_(&view),
      channel_(channel),
      interaction_(std::make_unique<Interaction>(view)),
      window_(std::make_unique<WindowService>(view, std::move(channel))),
      backlight_(std::make_unique<Backlight>(QStringLiteral("/sys/class/backlight"), QDBusConnection::systemBus())),
      images_(std::make_unique<Images>(view, Images::Places::system())) {
    // Inside a sandbox the portal asks GeoClue for the App, and stands its
    // dialog over the game's window.
    const bool sandboxed = insideSandbox();
    const QString desktop = QGuiApplication::desktopFileName();
    geolocation_ = std::make_unique<Geolocation>(
        sandboxed ? QDBusConnection::sessionBus() : QDBusConnection::systemBus(),
        sandboxed ? Geolocation::Source::Portal : Geolocation::Source::GeoClue,
        desktop.isEmpty() ? QCoreApplication::applicationName() : desktop,
        [game = QPointer<QWidget>(&view)]() -> QString {
            if (game.isNull() || QGuiApplication::platformName() != QLatin1String("xcb")) return {};
            return QStringLiteral("x11:%1").arg(static_cast<quint64>(game->window()->winId()), 0, 16);
        });
    bluetooth_ = std::make_unique<Bluetooth>(
        QDBusConnection::systemBus(),
        [channel = channel_](std::uint32_t event, const QJsonObject &payload) {
            (void)channel->post(MIGO_HOST_SERVICE_BLUETOOTH, event, payload);
        },
        [channel = channel_](const QString &device, const QString &service, const QString &characteristic,
                             const QByteArray &value) {
            (void)channel->postBleCharacteristicValue(device, service, characteristic, value);
        });
}

MigoQtBuiltInHostServices::~MigoQtBuiltInHostServices() = default;

std::uint64_t MigoQtBuiltInHostServices::services() const noexcept {
    std::uint64_t builtIn = 0;
    builtIn |= hostServiceBit(MIGO_HOST_SERVICE_CLIPBOARD);
    builtIn |= hostServiceBit(MIGO_HOST_SERVICE_INTERACTION);
    builtIn |= hostServiceBit(MIGO_HOST_SERVICE_WINDOW);
    builtIn |= hostServiceBit(MIGO_HOST_SERVICE_SCREEN);
    builtIn |= hostServiceBit(MIGO_HOST_SERVICE_IMAGE);
    builtIn |= hostServiceBit(MIGO_HOST_SERVICE_LOCATION);
    builtIn |= hostServiceBit(MIGO_HOST_SERVICE_PERMISSION);
    builtIn |= hostServiceBit(MIGO_HOST_SERVICE_MOTION);
    builtIn |= hostServiceBit(MIGO_HOST_SERVICE_BLUETOOTH);
    return builtIn;
}

void MigoQtBuiltInHostServices::call(MigoHostService service, std::uint32_t method, const QJsonObject &request,
                                     MigoQtHostServiceReply reply) {
    switch (service) {
        case MIGO_HOST_SERVICE_INTERACTION:
            interaction_->call(method, request, std::move(reply));
            return;
        case MIGO_HOST_SERVICE_WINDOW:
            window_->call(method, request, std::move(reply));
            return;
        case MIGO_HOST_SERVICE_SCREEN:
            screen(method, request, std::move(reply));
            return;
        case MIGO_HOST_SERVICE_IMAGE:
            images_->call(method, request, std::move(reply));
            return;
        case MIGO_HOST_SERVICE_LOCATION:
            location(method, request, std::move(reply));
            return;
        case MIGO_HOST_SERVICE_PERMISSION:
            permission(method, request, std::move(reply));
            return;
        case MIGO_HOST_SERVICE_MOTION:
            motion(method, request, std::move(reply));
            return;
        case MIGO_HOST_SERVICE_BLUETOOTH:
            bluetooth_->call(method, request, std::move(reply));
            return;
        case MIGO_HOST_SERVICE_CLIPBOARD:
            if (method == MIGO_CLIPBOARD_SET_CLIPBOARD_DATA) {
                QGuiApplication::clipboard()->setText(request.value(QStringLiteral("data")).toString());
                (void)reply.succeed();
            } else if (method == MIGO_CLIPBOARD_GET_CLIPBOARD_DATA) {
                (void)reply.succeed({{QStringLiteral("data"), QGuiApplication::clipboard()->text()}});
            } else {
                (void)reply.fail(QStringLiteral("not supported"));
            }
            return;
        default:
            (void)reply.fail(QStringLiteral("not supported"));
            return;
    }
}

void MigoQtBuiltInHostServices::screen(std::uint32_t method, const QJsonObject &request,
                                       MigoQtHostServiceReply reply) {
    switch (method) {
        case MIGO_SCREEN_SET_DEVICE_ORIENTATION:
            window_->setOrientation(request, std::move(reply));
            return;
        case MIGO_SCREEN_SET_ENABLE_DEBUG:
            Q_EMIT debugAidsRequested(request.value(QStringLiteral("enableDebug")).toBool());
            return;
        case MIGO_SCREEN_GET_BRIGHTNESS:
            if (const std::optional<double> value = backlight_->brightness()) {
                (void)reply.succeed({{QStringLiteral("value"), *value}});
            } else {
                (void)reply.fail(QStringLiteral("the screen has no backlight the system controls"));
            }
            return;
        case MIGO_SCREEN_SET_BRIGHTNESS: {
            const QJsonValue value = request.value(QStringLiteral("value"));
            if (!value.isDouble() || !std::isfinite(value.toDouble())) {
                (void)reply.fail(QStringLiteral("value is not a number"));
                return;
            }
            backlight_->setBrightness(value.toDouble(), [reply](const QString &error) mutable {
                (void)(error.isEmpty() ? reply.succeed() : reply.fail(error));
            });
            return;
        }
        default:
            // A screenshot, a recording, keeping the window out of them: see the
            // class's comment. The observers are commands, and a command's
            // failure reaches no one.
            (void)reply.fail(QStringLiteral("not supported"));
            return;
    }
}

void MigoQtBuiltInHostServices::location(std::uint32_t method, const QJsonObject &request,
                                         MigoQtHostServiceReply reply) {
    // As long as the Android SDK waits for a position.
    constexpr std::chrono::milliseconds kWait{10'000};
    switch (method) {
        case MIGO_LOCATION_GET_LOCATION: {
            const bool high = request.value(QStringLiteral("isHighAccuracy")).toBool(false);
            const double expiry = request.value(QStringLiteral("highAccuracyExpireTime")).toDouble(0);
            const bool altitude = request.value(QStringLiteral("altitude")).toBool(false);
            const std::chrono::milliseconds wait =
                high && expiry >= 1 ? std::chrono::milliseconds(static_cast<std::int64_t>(qMin(expiry, 3.6e6))) : kWait;
            geolocation_->locate(high ? Geolocation::Accuracy::Exact : Geolocation::Accuracy::Street, wait,
                                 [reply, altitude](const Geolocation::Outcome &outcome) mutable {
                                     if (!outcome.position.has_value()) {
                                         (void)reply.fail(outcome.error);
                                         return;
                                     }
                                     const Geolocation::Position &at = *outcome.position;
                                     (void)reply.succeed({
                                         {QStringLiteral("latitude"), at.latitude},
                                         {QStringLiteral("longitude"), at.longitude},
                                         {QStringLiteral("speed"), at.speed.value_or(0)},
                                         {QStringLiteral("accuracy"), at.accuracy},
                                         {QStringLiteral("altitude"), altitude ? at.altitude.value_or(0) : 0.0},
                                         // GeoClue reports no vertical accuracy.
                                         {QStringLiteral("verticalAccuracy"), 0},
                                         {QStringLiteral("horizontalAccuracy"), at.accuracy},
                                     });
                                 });
            return;
        }
        case MIGO_LOCATION_GET_FUZZY_LOCATION:
            geolocation_->locate(Geolocation::Accuracy::City, kWait,
                                 [reply](const Geolocation::Outcome &outcome) mutable {
                                     if (!outcome.position.has_value()) {
                                         (void)reply.fail(outcome.error);
                                         return;
                                     }
                                     (void)reply.succeed({{QStringLiteral("latitude"), outcome.position->latitude},
                                                          {QStringLiteral("longitude"), outcome.position->longitude}});
                                 });
            return;
        default:
            (void)reply.fail(QStringLiteral("not supported"));
            return;
    }
}

void MigoQtBuiltInHostServices::permission(std::uint32_t method, const QJsonObject &request,
                                           MigoQtHostServiceReply reply) {
    if (method != MIGO_PERMISSION_REQUEST_SCOPE) {
        (void)reply.fail(QStringLiteral("not supported"));
        return;
    }
    // Recorded before the answer, so content acting on a grant finds it.
    const auto scope = static_cast<MigoScope>(request.value(QStringLiteral("scopeId")).toInt(-1));
    switch (scope) {
        case MIGO_SCOPE_USER_LOCATION:
        case MIGO_SCOPE_USER_LOCATION_BACKGROUND:
            geolocation_->decide([channel = channel_, scope, reply](const Geolocation::Outcome &outcome) mutable {
                if (outcome.allowed) {
                    (void)channel->setScopeState(scope, MIGO_SCOPE_STATE_GRANTED);
                    (void)reply.succeed();
                } else if (outcome.denied) {
                    (void)channel->setScopeState(scope, MIGO_SCOPE_STATE_DENIED);
                    (void)reply.fail(QStringLiteral("auth deny"));
                } else {
                    // Nobody decided: there was no one to ask.
                    (void)reply.fail(outcome.error);
                }
            });
            return;
        case MIGO_SCOPE_WRITE_PHOTOS_ALBUM:
        case MIGO_SCOPE_BLUETOOTH:
            (void)channel_->setScopeState(scope, MIGO_SCOPE_STATE_GRANTED);
            (void)reply.succeed();
            return;
        default:
            (void)reply.fail(QStringLiteral("auth deny"));
            return;
    }
}

void MigoQtBuiltInHostServices::motion(std::uint32_t method, const QJsonObject &request,
                                       MigoQtHostServiceReply reply) {
    if (motion_ == nullptr) {
        motion_ = std::make_unique<MotionSensors>(
            MotionSensors::enumerate(),
            [channel = channel_](const MigoSensorSample &sample) { (void)channel->postSensorSample(sample); });
    }
    // Each sensor's start and stop, in the order the header numbers them.
    static constexpr struct {
        std::uint32_t start;
        std::uint32_t stop;
        MigoSensorKind kind;
    } kSensors[] = {
        {MIGO_MOTION_START_ACCELEROMETER, MIGO_MOTION_STOP_ACCELEROMETER, MIGO_SENSOR_ACCELEROMETER},
        {MIGO_MOTION_START_GYROSCOPE, MIGO_MOTION_STOP_GYROSCOPE, MIGO_SENSOR_GYROSCOPE},
        {MIGO_MOTION_START_COMPASS, MIGO_MOTION_STOP_COMPASS, MIGO_SENSOR_COMPASS},
        {MIGO_MOTION_START_DEVICE_MOTION, MIGO_MOTION_STOP_DEVICE_MOTION, MIGO_SENSOR_DEVICE_MOTION},
    };
    for (const auto &sensor : kSensors) {
        if (method == sensor.stop) {
            motion_->stop(sensor.kind);
            return;
        }
        if (method != sensor.start) continue;
        if (const QString missing = motion_->missing(sensor.kind); !missing.isEmpty()) {
            (void)reply.fail(missing);
            return;
        }
        const QString interval = request.value(QStringLiteral("interval")).toString(QStringLiteral("normal"));
        const std::chrono::milliseconds every = interval == QLatin1String("game") ? std::chrono::milliseconds{20}
                                                : interval == QLatin1String("ui") ? std::chrono::milliseconds{60}
                                                                                   : std::chrono::milliseconds{200};
        motion_->start(sensor.kind, every);
        (void)reply.succeed();
        return;
    }
    (void)reply.fail(QStringLiteral("not supported"));
}

QString MigoQtBuiltInHostServices::indicatorTitle() const { return interaction_->indicatorTitle(); }

}  // namespace migo::linux_host::qt6

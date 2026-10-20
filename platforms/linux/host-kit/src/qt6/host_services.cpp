#include <migo/linux/qt6/host_services.hpp>

#include <QAction>
#include <QClipboard>
#include <QDialog>
#include <QDialogButtonBox>
#include <QEvent>
#include <QGuiApplication>
#include <QHBoxLayout>
#include <QJsonArray>
#include <QJsonDocument>
#include <QLabel>
#include <QLineEdit>
#include <QList>
#include <QMenu>
#include <QProgressBar>
#include <QPushButton>
#include <QStyle>
#include <QTimer>
#include <QVBoxLayout>
#include <QWidget>

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
        // A window of its own, over the game: the game is drawn in a native
        // window that a sibling widget cannot stand above.
        auto *panel = new QWidget(surface_->window(),
                                  Qt::ToolTip | Qt::FramelessWindowHint | Qt::WindowDoesNotAcceptFocus);
        panel->setAttribute(Qt::WA_ShowWithoutActivating);
        panel->setAttribute(Qt::WA_TransparentForMouseEvents);
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
        panel->show();
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

// ---- the built-in services --------------------------------------------------------------------

MigoQtBuiltInHostServices::MigoQtBuiltInHostServices(QWidget &surface, QObject *parent)
    : QObject(parent), surface_(&surface), interaction_(std::make_unique<Interaction>(surface)) {}

MigoQtBuiltInHostServices::~MigoQtBuiltInHostServices() = default;

std::uint64_t MigoQtBuiltInHostServices::services() const noexcept {
    std::uint64_t builtIn = 0;
    builtIn |= hostServiceBit(MIGO_HOST_SERVICE_CLIPBOARD);
    builtIn |= hostServiceBit(MIGO_HOST_SERVICE_INTERACTION);
    return builtIn;
}

void MigoQtBuiltInHostServices::call(MigoHostService service, std::uint32_t method, const QJsonObject &request,
                                     MigoQtHostServiceReply reply) {
    switch (service) {
        case MIGO_HOST_SERVICE_INTERACTION:
            interaction_->call(method, request, std::move(reply));
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

QString MigoQtBuiltInHostServices::indicatorTitle() const { return interaction_->indicatorTitle(); }

}  // namespace migo::linux_host::qt6

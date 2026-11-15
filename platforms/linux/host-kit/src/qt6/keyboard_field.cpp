#include "keyboard_field.hpp"

#include <QApplication>
#include <QFocusEvent>
#include <QFontMetrics>
#include <QKeyEvent>
#include <QLineEdit>
#include <QPlainTextEdit>
#include <QRegularExpression>
#include <QRegularExpressionValidator>
#include <QSignalBlocker>
#include <QTextCursor>
#include <QTextDocument>
#include <QVBoxLayout>

#include <cmath>
#include <limits>

namespace migo::linux_host::qt6 {

namespace {

/// A field of several lines shows this many, as Android's does.
constexpr int kVisibleLines = 4;

Qt::EnterKeyType enterKey(MigoKeyboardConfirmType confirm) {
    switch (confirm) {
        case MIGO_KEYBOARD_CONFIRM_NEXT: return Qt::EnterKeyNext;
        case MIGO_KEYBOARD_CONFIRM_SEARCH: return Qt::EnterKeySearch;
        case MIGO_KEYBOARD_CONFIRM_GO: return Qt::EnterKeyGo;
        case MIGO_KEYBOARD_CONFIRM_SEND: return Qt::EnterKeySend;
        default: return Qt::EnterKeyDone;
    }
}

}  // namespace

/// A one-line field that tells an input method which confirm key to show:
/// QLineEdit has no setter for it, only the query an input method makes.
class KeyboardField::Line final : public QLineEdit {
public:
    using QLineEdit::QLineEdit;

    Qt::EnterKeyType enter = Qt::EnterKeyDone;

    QVariant inputMethodQuery(Qt::InputMethodQuery query) const override {
        if (query == Qt::ImEnterKeyType) return enter;
        return QLineEdit::inputMethodQuery(query);
    }
};

KeyboardField::KeyboardField(MigoSession *session, MigoQtX11SurfaceView &view)
    : session_(session), view_(&view) {
    // Over the game's window, which keeps it above itself; owned by it, so a
    // window that goes first takes the field with it.
    window_ = new QWidget(view.window(), Qt::Tool | Qt::FramelessWindowHint);
    window_->setObjectName(QStringLiteral("migoKeyboardField"));
    line_ = new Line(window_);
    lines_ = new QPlainTextEdit(window_);
    lines_->setTabChangesFocus(false);
    digits_ = new QRegularExpressionValidator(QRegularExpression(QStringLiteral("[0-9]*")), line_);
    auto *layout = new QVBoxLayout(window_);
    layout->setContentsMargins(0, 0, 0, 0);
    layout->setSpacing(0);
    layout->addWidget(line_);
    layout->addWidget(lines_);
    for (QWidget *editor : {static_cast<QWidget *>(line_), static_cast<QWidget *>(lines_)}) {
        editor->installEventFilter(this);
    }
    // The player's edits only: content's own text (setText) is not one.
    connect(line_, &QLineEdit::textEdited, this,
            [this](const QString &text) { send(MIGO_KEYBOARD_EVENT_INPUT, text); });
    connect(line_, &QLineEdit::returnPressed, this, [this] {
        send(MIGO_KEYBOARD_EVENT_CONFIRM, line_->text());
        if (!hold_) finish(true);
    });
    connect(lines_, &QPlainTextEdit::textChanged, this, [this] { linesEdited(); });
    view.installEventFilter(this);
    view.window()->installEventFilter(this);
}

KeyboardField::~KeyboardField() {
    finish(false);
    delete window_.data();
}

void KeyboardField::show(const MigoKeyboardShowOptions &options) {
    if (window_.isNull() || view_.isNull()) return;
    const bool number = options.keyboard_type == MIGO_KEYBOARD_TYPE_NUMBER;
    // Digits are one line, as a number keyboard's field is.
    multiline_ = (options.flags & MIGO_KEYBOARD_FLAG_MULTIPLE) != 0 && !number;
    hold_ = (options.flags & MIGO_KEYBOARD_FLAG_CONFIRM_HOLD) != 0;
    max_length_ = options.max_length;
    const QString initial =
        options.default_value_utf8 == nullptr
            ? QString()
            : QString::fromUtf8(options.default_value_utf8, static_cast<qsizetype>(options.default_value_length));

    line_->setVisible(!multiline_);
    lines_->setVisible(multiline_);
    if (multiline_) {
        const QSignalBlocker quiet(lines_);
        lines_->setPlainText(max_length_ > 0 ? initial.left(static_cast<qsizetype>(max_length_)) : initial);
        lines_->moveCursor(QTextCursor::End);
        const QFontMetrics metrics(lines_->font());
        height_ = metrics.lineSpacing() * kVisibleLines +
                  2 * (lines_->frameWidth() + static_cast<int>(std::ceil(lines_->document()->documentMargin())));
    } else {
        line_->setMaxLength(max_length_ > 0 && max_length_ < static_cast<std::uint32_t>(std::numeric_limits<int>::max())
                                ? static_cast<int>(max_length_)
                                : std::numeric_limits<int>::max());
        line_->setValidator(number ? digits_ : nullptr);
        line_->setInputMethodHints(number ? Qt::ImhDigitsOnly : Qt::ImhNone);
        line_->enter = enterKey(options.confirm_type);
        line_->setText(initial);
        height_ = line_->sizeHint().height();
    }
    window_->setFixedHeight(height_);
    place();
    const bool was_showing = showing_;
    showing_ = true;
    focused_ = false;
    window_->show();
    window_->raise();
    window_->activateWindow();
    editor()->setFocus(Qt::OtherFocusReason);
    // In CSS pixels: Qt's logical pixels are the view's.
    if (!was_showing) send(MIGO_KEYBOARD_EVENT_HEIGHT_CHANGE, QString(), height_);
}

void KeyboardField::hide() { finish(true); }

void KeyboardField::replaceText(const QString &text) {
    if (!showing_) return;
    if (multiline_) {
        const QSignalBlocker quiet(lines_);
        lines_->setPlainText(text);
        lines_->moveCursor(QTextCursor::End);
    } else {
        line_->setText(text);
    }
}

bool KeyboardField::eventFilter(QObject *watched, QEvent *event) {
    if (watched == view_.data() || (!view_.isNull() && watched == view_->window())) {
        if (event->type() == QEvent::Move || event->type() == QEvent::Resize) place();
        return QObject::eventFilter(watched, event);
    }
    if (!showing_ || watched != editor()) return QObject::eventFilter(watched, event);
    switch (event->type()) {
        case QEvent::FocusIn:
            focused_ = true;
            break;
        case QEvent::FocusOut:
            // A context menu of the field's own is not the player turning away.
            if (focused_ && static_cast<QFocusEvent *>(event)->reason() != Qt::PopupFocusReason) finish(true);
            break;
        case QEvent::KeyPress:
            if (static_cast<QKeyEvent *>(event)->key() == Qt::Key_Escape) {
                finish(true);
                return true;
            }
            break;
        default:
            break;
    }
    return QObject::eventFilter(watched, event);
}

QWidget *KeyboardField::editor() const {
    return multiline_ ? static_cast<QWidget *>(lines_) : static_cast<QWidget *>(line_);
}

QString KeyboardField::text() const { return multiline_ ? lines_->toPlainText() : line_->text(); }

/// Along the bottom of the game, its full width.
void KeyboardField::place() {
    if (window_.isNull() || view_.isNull() || height_ <= 0) return;
    window_->setGeometry(QRect(view_->mapToGlobal(QPoint(0, view_->height() - height_)),
                               QSize(view_->width(), height_)));
}

void KeyboardField::finish(bool report) {
    if (!showing_) return;
    // Before the window goes: hiding it moves the focus, which comes back here.
    showing_ = false;
    const QString left = text();
    const bool had_keyboard = !window_.isNull() && QApplication::activeWindow() == window_.data();
    if (!window_.isNull()) window_->hide();
    // The keys go back to the game the field took them from; a player who
    // turned to another window keeps it.
    if (had_keyboard && !view_.isNull()) {
        view_->window()->activateWindow();
        view_->setFocus(Qt::OtherFocusReason);
    }
    if (!report) return;
    send(MIGO_KEYBOARD_EVENT_HEIGHT_CHANGE, QString(), 0.0);
    send(MIGO_KEYBOARD_EVENT_COMPLETE, left);
}

void KeyboardField::linesEdited() {
    if (!showing_ || !multiline_) return;
    QString text = lines_->toPlainText();
    if (max_length_ > 0 && static_cast<std::uint64_t>(text.size()) > max_length_) {
        text.truncate(static_cast<qsizetype>(max_length_));
        const QSignalBlocker quiet(lines_);
        lines_->setPlainText(text);
        lines_->moveCursor(QTextCursor::End);
    }
    send(MIGO_KEYBOARD_EVENT_INPUT, text);
}

void KeyboardField::send(MigoKeyboardEventType type, const QString &value, double height) {
    const QByteArray utf8 = value.toUtf8();
    MigoKeyboardEvent event{};
    event.struct_size = static_cast<std::uint32_t>(sizeof(event));
    event.abi_version = MIGO_ABI_VERSION_1;
    event.event_type = type;
    event.value_length = static_cast<std::uint32_t>(utf8.size());
    event.value_utf8 = utf8.constData();
    event.height_css_px = height;
    // Refused only without a Surface, when there is no content to tell.
    (void)migo_session_send_keyboard_event(session_, &event);
}

}  // namespace migo::linux_host::qt6

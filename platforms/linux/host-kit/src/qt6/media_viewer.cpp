#include "media_viewer.hpp"

#include <QFileDevice>
#include <QGuiApplication>
#include <QHBoxLayout>
#include <QImage>
#include <QImageIOHandler>
#include <QImageReader>
#include <QKeyEvent>
#include <QLabel>
#include <QPixmap>
#include <QPushButton>
#include <QResizeEvent>
#include <QShowEvent>
#include <QStyle>
#include <QVBoxLayout>

#include <utility>

namespace migo::linux_host::qt6 {

namespace {

QPushButton *iconButton(QWidget &parent, QStyle::StandardPixmap icon) {
    auto *button = new QPushButton(&parent);
    button->setIcon(parent.style()->standardIcon(icon));
    button->setIconSize({32, 32});
    button->setFlat(true);
    // The arrows are the viewer's, not a way between its buttons.
    button->setFocusPolicy(Qt::NoFocus);
    return button;
}

/// The image `bytes` hold, upright, no larger than `room` where the format
/// can decode smaller; null when it cannot be decoded. An empty `room` asks
/// for its own size.
QImage decodeImage(QIODevice &bytes, QSize room) {
    if (!bytes.seek(0)) return {};
    QImageReader reader(&bytes);
    reader.setDecideFormatFromContent(true);
    reader.setAutoTransform(true);
    if (const QSize stored = reader.size(); stored.isValid() && !room.isEmpty()) {
        const bool turned = (reader.transformation() & QImageIOHandler::TransformationRotate90) != 0;
        const QSize upright = turned ? stored.transposed() : stored;
        if ((upright.width() > room.width() || upright.height() > room.height()) &&
            reader.supportsOption(QImageIOHandler::ScaledSize)) {
            const QSize fitted = upright.scaled(room, Qt::KeepAspectRatio).expandedTo({1, 1});
            reader.setScaledSize(turned ? fitted.transposed() : fitted);
        }
    }
    return reader.read();
}

}  // namespace

MediaViewer::MediaViewer(QWidget &window, std::vector<Item> items, int current,
                         std::shared_ptr<const MigoQtVideoSupport> videos)
    : QDialog(&window), items_(std::move(items)), videos_(std::move(videos)) {
    setObjectName(QStringLiteral("migoMediaViewer"));
    setWindowTitle(QGuiApplication::applicationDisplayName());
    setWindowModality(Qt::WindowModal);
    setStyleSheet(QStringLiteral("#migoMediaViewer { background: #101010; } QLabel { color: white; }"));

    auto *layout = new QVBoxLayout(this);
    layout->setContentsMargins(8, 8, 8, 8);
    auto *bar = new QHBoxLayout;
    position_ = new QLabel(this);
    bar->addWidget(position_);
    bar->addStretch();
    QPushButton *close = iconButton(*this, QStyle::SP_DialogCloseButton);
    connect(close, &QPushButton::clicked, this, &QDialog::reject);
    bar->addWidget(close);
    layout->addLayout(bar);

    row_ = new QHBoxLayout;
    previous_ = iconButton(*this, QStyle::SP_ArrowLeft);
    next_ = iconButton(*this, QStyle::SP_ArrowRight);
    connect(previous_, &QPushButton::clicked, this, [this] { showPrevious(); });
    connect(next_, &QPushButton::clicked, this, [this] { showNext(); });
    picture_ = new QLabel(this);
    picture_->setAlignment(Qt::AlignCenter);
    picture_->setSizePolicy(QSizePolicy::Ignored, QSizePolicy::Ignored);
    picture_->setMinimumSize(1, 1);
    row_->addWidget(previous_);
    row_->addWidget(picture_, 1);
    row_->addWidget(next_);
    layout->addLayout(row_, 1);

    resize(window.size());
    present(qBound(0, current, count() - 1));
}

MediaViewer::~MediaViewer() {
    // The player stops before the files it may read are closed.
    delete player_.data();
}

void MediaViewer::showPrevious() {
    if (current_ > 0) present(current_ - 1);
}

void MediaViewer::showNext() {
    if (current_ + 1 < count()) present(current_ + 1);
}

void MediaViewer::keyPressEvent(QKeyEvent *event) {
    switch (event->key()) {
        case Qt::Key_Left:
            showPrevious();
            return;
        case Qt::Key_Right:
            showNext();
            return;
        default:
            // Esc rejects.
            QDialog::keyPressEvent(event);
            return;
    }
}

void MediaViewer::resizeEvent(QResizeEvent *event) {
    QDialog::resizeEvent(event);
    render();
}

void MediaViewer::showEvent(QShowEvent *event) {
    QDialog::showEvent(event);
    render();
}

void MediaViewer::present(int index) {
    current_ = index;
    position_->setText(QStringLiteral("%1 / %2").arg(index + 1).arg(count()));
    previous_->setEnabled(index > 0);
    next_->setEnabled(index + 1 < count());
    // The video shown before stops as its player goes.
    delete player_.data();
    Item &item = items_.at(static_cast<std::size_t>(index));
    if (!item.video || videos_ == nullptr) {
        picture_->show();
        render();
        return;
    }
    decoded_ = false;
    picture_->hide();
    const QImage poster = item.poster == nullptr ? QImage() : decodeImage(*item.poster, {});
    // A file held open is played through its descriptor: the engine may have
    // taken back the name it lent.
    const auto *file = dynamic_cast<QFileDevice *>(item.file.get());
    const QString source = file != nullptr ? QStringLiteral("/proc/self/fd/%1").arg(file->handle()) : item.url;
    player_ = videos_->player(source, poster, *this);
    row_->insertWidget(1, player_, 1);
}

void MediaViewer::render() {
    // Until it is shown, it has no room to fit an image to; a video's player
    // fits itself.
    if (!isVisible() || !player_.isNull()) return;
    // In the screen's pixels: the room, and what is decoded for it.
    const qreal ratio = devicePixelRatioF();
    const QSize room = (QSizeF(picture_->size()) * ratio).toSize().expandedTo({1, 1});
    const Item &item = items_.at(static_cast<std::size_t>(current_));
    QImage decoded = item.video ? QImage() : decodeImage(*item.file, room);
    decoded_ = !decoded.isNull();
    if (!decoded_) {
        picture_->setPixmap(style()->standardIcon(QStyle::SP_MessageBoxWarning).pixmap(64, 64));
        return;
    }
    if (decoded.width() > room.width() || decoded.height() > room.height()) {
        decoded = decoded.scaled(room, Qt::KeepAspectRatio, Qt::SmoothTransformation);
    }
    QPixmap pixmap = QPixmap::fromImage(std::move(decoded));
    pixmap.setDevicePixelRatio(ratio);
    picture_->setPixmap(pixmap);
}

}  // namespace migo::linux_host::qt6

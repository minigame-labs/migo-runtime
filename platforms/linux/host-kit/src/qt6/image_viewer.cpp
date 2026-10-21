#include "image_viewer.hpp"

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

}  // namespace

ImageViewer::ImageViewer(QWidget &window, std::vector<std::unique_ptr<QIODevice>> images, int current)
    : QDialog(&window), images_(std::move(images)) {
    setObjectName(QStringLiteral("migoImageViewer"));
    setWindowTitle(QGuiApplication::applicationDisplayName());
    setWindowModality(Qt::WindowModal);
    setStyleSheet(QStringLiteral("#migoImageViewer { background: #101010; } QLabel { color: white; }"));

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

    auto *row = new QHBoxLayout;
    previous_ = iconButton(*this, QStyle::SP_ArrowLeft);
    next_ = iconButton(*this, QStyle::SP_ArrowRight);
    connect(previous_, &QPushButton::clicked, this, [this] { showPrevious(); });
    connect(next_, &QPushButton::clicked, this, [this] { showNext(); });
    picture_ = new QLabel(this);
    picture_->setAlignment(Qt::AlignCenter);
    picture_->setSizePolicy(QSizePolicy::Ignored, QSizePolicy::Ignored);
    picture_->setMinimumSize(1, 1);
    row->addWidget(previous_);
    row->addWidget(picture_, 1);
    row->addWidget(next_);
    layout->addLayout(row, 1);

    resize(window.size());
    present(qBound(0, current, count() - 1));
}

ImageViewer::~ImageViewer() = default;

void ImageViewer::showPrevious() {
    if (current_ > 0) present(current_ - 1);
}

void ImageViewer::showNext() {
    if (current_ + 1 < count()) present(current_ + 1);
}

void ImageViewer::keyPressEvent(QKeyEvent *event) {
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

void ImageViewer::resizeEvent(QResizeEvent *event) {
    QDialog::resizeEvent(event);
    render();
}

void ImageViewer::showEvent(QShowEvent *event) {
    QDialog::showEvent(event);
    render();
}

void ImageViewer::present(int index) {
    current_ = index;
    position_->setText(QStringLiteral("%1 / %2").arg(index + 1).arg(count()));
    previous_->setEnabled(index > 0);
    next_->setEnabled(index + 1 < count());
    render();
}

void ImageViewer::render() {
    // Until it is shown, it has no room to fit an image to.
    if (!isVisible()) return;
    // In the screen's pixels: the room, and what is decoded for it.
    const qreal ratio = devicePixelRatioF();
    const QSize room = (QSizeF(picture_->size()) * ratio).toSize().expandedTo({1, 1});
    QIODevice &image = *images_.at(static_cast<std::size_t>(current_));
    QImage decoded;
    if (image.seek(0)) {
        QImageReader reader(&image);
        reader.setDecideFormatFromContent(true);
        reader.setAutoTransform(true);
        // No more than the room needs, where the format can decode smaller.
        if (const QSize stored = reader.size(); stored.isValid()) {
            const bool turned = (reader.transformation() & QImageIOHandler::TransformationRotate90) != 0;
            const QSize upright = turned ? stored.transposed() : stored;
            if ((upright.width() > room.width() || upright.height() > room.height()) &&
                reader.supportsOption(QImageIOHandler::ScaledSize)) {
                const QSize fitted = upright.scaled(room, Qt::KeepAspectRatio).expandedTo({1, 1});
                reader.setScaledSize(turned ? fitted.transposed() : fitted);
            }
        }
        decoded = reader.read();
    }
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

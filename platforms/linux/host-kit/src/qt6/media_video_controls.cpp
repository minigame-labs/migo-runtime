#include "media_video_controls.hpp"

#include <QApplication>
#include <QFontDatabase>
#include <QIcon>
#include <QLabel>
#include <QMouseEvent>
#include <QPaintEvent>
#include <QPainter>
#include <QPainterPath>
#include <QPixmap>
#include <QResizeEvent>
#include <QSlider>
#include <QToolButton>

#include <cmath>

namespace migo::linux_host::qt6 {

namespace {

constexpr int kBarShownMs = 3000;
constexpr int kBarHeight = 36;
constexpr int kCenterSize = 56;
constexpr int kClockWidth = 52;
/// The progress bar's steps: the whole video is this many.
constexpr int kProgressSteps = 1000;

enum class Glyph { Play, CenterPlay, Pause, EnterFullScreen, ExitFullScreen };

/// One of the controls' glyphs, white, `size` logical pixels square, drawn for
/// the screen `ratio` says: the style's own icons are drawn for a window's
/// background, not a video's.
QIcon glyph(Glyph kind, int size, qreal ratio) {
    QPixmap pixmap(QSize(size, size) * ratio);
    pixmap.setDevicePixelRatio(ratio);
    pixmap.fill(Qt::transparent);
    QPainter painter(&pixmap);
    painter.setRenderHint(QPainter::Antialiasing);
    painter.setPen(Qt::NoPen);
    const qreal s = size;
    switch (kind) {
        case Glyph::CenterPlay: {
            painter.setBrush(QColor(0, 0, 0, 115));
            painter.drawEllipse(QRectF(0, 0, s, s));
            QPainterPath triangle;
            triangle.moveTo(s * 0.40, s * 0.30);
            triangle.lineTo(s * 0.72, s * 0.50);
            triangle.lineTo(s * 0.40, s * 0.70);
            triangle.closeSubpath();
            painter.setBrush(Qt::white);
            painter.drawPath(triangle);
            break;
        }
        case Glyph::Play: {
            QPainterPath triangle;
            triangle.moveTo(s * 0.32, s * 0.25);
            triangle.lineTo(s * 0.75, s * 0.50);
            triangle.lineTo(s * 0.32, s * 0.75);
            triangle.closeSubpath();
            painter.setBrush(Qt::white);
            painter.drawPath(triangle);
            break;
        }
        case Glyph::Pause:
            painter.setBrush(Qt::white);
            painter.drawRect(QRectF(s * 0.30, s * 0.25, s * 0.13, s * 0.50));
            painter.drawRect(QRectF(s * 0.57, s * 0.25, s * 0.13, s * 0.50));
            break;
        case Glyph::EnterFullScreen:
        case Glyph::ExitFullScreen: {
            // A corner at each of the four, pointing out to enter, in to leave.
            QPen pen(Qt::white, qMax(1.5, s * 0.08), Qt::SolidLine, Qt::SquareCap, Qt::MiterJoin);
            painter.setPen(pen);
            painter.setBrush(Qt::NoBrush);
            const qreal in = s * 0.28;
            const qreal out = s * 0.72;
            const qreal arm = s * 0.16;
            const bool enter = kind == Glyph::EnterFullScreen;
            for (const QPointF &corner : {QPointF(in, in), QPointF(out, in), QPointF(in, out), QPointF(out, out)}) {
                // Toward the middle, or away from it.
                const qreal dx = (corner.x() < s / 2 ? 1 : -1) * (enter ? 1 : -1) * arm;
                const qreal dy = (corner.y() < s / 2 ? 1 : -1) * (enter ? 1 : -1) * arm;
                QPainterPath path;
                path.moveTo(corner.x() + dx, corner.y());
                path.lineTo(corner);
                path.lineTo(corner.x(), corner.y() + dy);
                painter.drawPath(path);
            }
            break;
        }
    }
    painter.end();
    return QIcon(pixmap);
}

QToolButton *button(QWidget &parent, const QString &name, int size) {
    auto *made = new QToolButton(&parent);
    made->setAccessibleName(name);
    made->setIconSize({size, size});
    made->setAutoRaise(true);
    made->setStyleSheet(QStringLiteral("QToolButton { border: none; background: transparent; }"));
    // The keyboard is the game's.
    made->setFocusPolicy(Qt::NoFocus);
    made->setCursor(Qt::PointingHandCursor);
    return made;
}

QLabel *label(QWidget &parent) {
    auto *made = new QLabel(&parent);
    QFont font = QFontDatabase::systemFont(QFontDatabase::FixedFont);
    font.setPixelSize(12);
    made->setFont(font);
    made->setStyleSheet(QStringLiteral("color: white; background: transparent;"));
    // Clicks on it are the controls'.
    made->setAttribute(Qt::WA_TransparentForMouseEvents);
    return made;
}

bool flag(const QJsonObject &options, const char *key, bool fallback) {
    const QJsonValue value = options.value(QLatin1String(key));
    return value.isBool() ? value.toBool() : fallback;
}

}  // namespace

const QStringList VideoControls::kOptions{
    QStringLiteral("controls"),          QStringLiteral("showProgress"),      QStringLiteral("showProgressInControlMode"),
    QStringLiteral("showCenterPlayBtn"), QStringLiteral("enablePlayGesture"), QStringLiteral("enableProgressGesture"),
};

QString videoClock(double seconds) {
    const qint64 whole = std::isfinite(seconds) ? static_cast<qint64>(std::floor(qMax(0.0, seconds))) : 0;
    const qint64 hours = whole / 3600;
    const qint64 minutes = whole / 60 % 60;
    const qint64 secs = whole % 60;
    const auto two = [](qint64 n) { return QStringLiteral("%1").arg(n, 2, 10, QLatin1Char('0')); };
    return hours > 0 ? QStringLiteral("%1:%2:%3").arg(hours).arg(two(minutes), two(secs))
                     : QStringLiteral("%1:%2").arg(minutes).arg(two(secs));
}

VideoControls::VideoControls(VideoControlled &player, QWidget &parent) : QWidget(&parent), player_(player) {
    setObjectName(QStringLiteral("migoVideoControls"));
    setFocusPolicy(Qt::NoFocus);
    const qreal ratio = devicePixelRatioF();

    centerPlay_ = button(*this, QStringLiteral("play"), kCenterSize);
    centerPlay_->setIcon(glyph(Glyph::CenterPlay, kCenterSize, ratio));
    connect(centerPlay_, &QToolButton::clicked, this, [this] { player_.togglePlay(); });

    bar_ = new QWidget(this);
    bar_->setObjectName(QStringLiteral("migoVideoBar"));
    bar_->setAutoFillBackground(true);
    QPalette shade = bar_->palette();
    shade.setColor(QPalette::Window, QColor(0, 0, 0, 128));
    bar_->setPalette(shade);
    barPlay_ = button(*bar_, QStringLiteral("play or pause"), 16);
    barPlay_->setIcon(glyph(Glyph::Play, 16, ratio));
    connect(barPlay_, &QToolButton::clicked, this, [this] { player_.togglePlay(); });
    time_ = label(*bar_);
    total_ = label(*bar_);
    time_->setAlignment(Qt::AlignCenter);
    total_->setAlignment(Qt::AlignCenter);
    progress_ = new QSlider(Qt::Horizontal, bar_);
    progress_->setRange(0, kProgressSteps);
    progress_->setPageStep(kProgressSteps / 10);
    progress_->setFocusPolicy(Qt::NoFocus);
    connect(progress_, &QSlider::sliderMoved, this, [this](int) { slid(false); });
    connect(progress_, &QSlider::sliderReleased, this, [this] { slid(true); });
    connect(progress_, &QSlider::actionTriggered, this, [this](int action) {
        // A click on the groove steps it; the step is where to go.
        if (action != QAbstractSlider::SliderMove) {
            QTimer::singleShot(0, this, [this] { slid(true); });
        }
    });
    fullScreen_ = button(*bar_, QStringLiteral("full screen"), 14);
    fullScreen_->setIcon(glyph(Glyph::EnterFullScreen, 14, ratio));
    connect(fullScreen_, &QToolButton::clicked, this, [this] { player_.toggleFullScreen(); });

    dragTime_ = label(*this);
    dragTime_->setAlignment(Qt::AlignCenter);

    hideBar_.setSingleShot(true);
    hideBar_.setInterval(kBarShownMs);
    connect(&hideBar_, &QTimer::timeout, this, [this] {
        if (player_.isPlaying() && !seekTarget_.has_value()) setBarShown(false);
    });
    click_.setSingleShot(true);
    click_.setInterval(QApplication::doubleClickInterval());
    connect(&click_, &QTimer::timeout, this, [this] { toggleBar(); });
    place();
    refresh();
}

VideoControls::~VideoControls() = default;

void VideoControls::configure(const QJsonObject &options) {
    controls_ = flag(options, "controls", true);
    showProgress_ = flag(options, "showProgress", true);
    showProgressInBar_ = flag(options, "showProgressInControlMode", true);
    showCenter_ = flag(options, "showCenterPlayBtn", true);
    playGesture_ = flag(options, "enablePlayGesture", false);
    progressGesture_ = flag(options, "enableProgressGesture", true);
    if (!controls_) barShown_ = false;
    refresh();
}

bool VideoControls::takesInput() const {
    return controls_ || playGesture_ || progressGesture_ || (showCenter_ && !player_.isPlaying());
}

double VideoControls::fraction() const {
    const double duration = player_.duration();
    const double now = seekTarget_.value_or(player_.position());
    return duration > 0 ? qBound(0.0, now / duration, 1.0) : 0.0;
}

void VideoControls::refresh() {
    const bool playing = player_.isPlaying();
    const double duration = player_.duration();
    const double now = seekTarget_.value_or(player_.position());
    centerPlay_->setVisible(showCenter_ && !playing && !seekTarget_.has_value());
    bar_->setVisible(controls_ && barShown_);
    progress_->setVisible(showProgressInBar_);
    dragTime_->setVisible(seekTarget_.has_value());
    const qreal ratio = devicePixelRatioF();
    if (playing != barPlayShowsPause_) {
        barPlayShowsPause_ = playing;
        barPlay_->setIcon(glyph(playing ? Glyph::Pause : Glyph::Play, 16, ratio));
    }
    if (const bool full = player_.isFullScreen(); full != fullScreenShowsExit_) {
        fullScreenShowsExit_ = full;
        fullScreen_->setIcon(glyph(full ? Glyph::ExitFullScreen : Glyph::EnterFullScreen, 14, ratio));
    }
    time_->setText(videoClock(now));
    total_->setText(videoClock(duration));
    dragTime_->setText(QStringLiteral("%1 / %2").arg(videoClock(now), videoClock(duration)));
    if (!draggingSlider_) progress_->setValue(static_cast<int>(std::lround(fraction() * kProgressSteps)));
    // The progress line.
    update();
    if (playing && barShown_ && !hideBar_.isActive()) hideBar_.start();
    if (const bool takes = takesInput(); takes != tookInput_) {
        tookInput_ = takes;
        player_.inputChanged();
    }
}

void VideoControls::toggleBar() {
    if (controls_) setBarShown(!barShown_);
}

void VideoControls::setBarShown(bool shown) {
    barShown_ = shown;
    hideBar_.stop();
    refresh();
}

void VideoControls::slid(bool done) {
    const double duration = player_.duration();
    if (duration <= 0) return;
    const double target = static_cast<double>(progress_->sliderPosition()) / kProgressSteps * duration;
    draggingSlider_ = !done;
    if (done) {
        seekTarget_.reset();
        player_.seekTo(target);
    } else {
        seekTarget_ = target;
    }
    refresh();
}

void VideoControls::seekEnded() {
    if (!seekTarget_.has_value()) return;
    const double target = *seekTarget_;
    seekTarget_.reset();
    player_.seekTo(target);
    refresh();
}

void VideoControls::place() {
    const int w = width();
    const int h = height();
    centerPlay_->setGeometry((w - kCenterSize) / 2, (h - kCenterSize) / 2, kCenterSize, kCenterSize);
    bar_->setGeometry(0, h - kBarHeight, w, kBarHeight);
    barPlay_->setGeometry(0, 0, kBarHeight, kBarHeight);
    fullScreen_->setGeometry(w - kBarHeight, 0, kBarHeight, kBarHeight);
    time_->setGeometry(kBarHeight, (kBarHeight - 16) / 2, kClockWidth, 16);
    total_->setGeometry(w - kBarHeight - kClockWidth, (kBarHeight - 16) / 2, kClockWidth, 16);
    progress_->setGeometry(kBarHeight + kClockWidth, 0, qMax(0, w - 2 * (kBarHeight + kClockWidth)), kBarHeight);
    dragTime_->setGeometry(0, (h - 20) / 2, w, 20);
}

void VideoControls::resizeEvent(QResizeEvent *event) {
    QWidget::resizeEvent(event);
    place();
}

void VideoControls::paintEvent(QPaintEvent *) {
    if (!(showProgress_ && bar_->isHidden() && player_.duration() > 0)) return;
    QPainter painter(this);
    const QRectF line(0, height() - 2, width(), 2);
    painter.fillRect(line, QColor(255, 255, 255, 77));
    painter.fillRect(QRectF(line.topLeft(), QSizeF(line.width() * fraction(), line.height())),
                     QColor(255, 255, 255, 230));
}

void VideoControls::mousePressEvent(QMouseEvent *event) {
    if (event->button() != Qt::LeftButton) return;
    pressedAt_ = event->position();
    dragging_ = false;
}

void VideoControls::mouseMoveEvent(QMouseEvent *event) {
    if (!pressedAt_.has_value()) return;
    const QPointF moved = event->position() - *pressedAt_;
    if (!dragging_) {
        // Sideways only: a drag more up than across is not a seek.
        if (!progressGesture_ || player_.duration() <= 0 || width() <= 0 ||
            std::abs(moved.x()) < QApplication::startDragDistance() || std::abs(moved.x()) <= std::abs(moved.y())) {
            return;
        }
        dragging_ = true;
        dragFrom_ = player_.position();
    }
    const double duration = player_.duration();
    seekTarget_ = qBound(0.0, dragFrom_ + moved.x() / width() * duration, duration);
    refresh();
}

void VideoControls::mouseReleaseEvent(QMouseEvent *event) {
    if (event->button() != Qt::LeftButton || !pressedAt_.has_value()) return;
    pressedAt_.reset();
    if (dragging_) {
        dragging_ = false;
        seekEnded();
        return;
    }
    // With the play gesture, a click waits until it is not one of two.
    if (playGesture_) {
        click_.start();
    } else {
        toggleBar();
    }
}

void VideoControls::mouseDoubleClickEvent(QMouseEvent *event) {
    if (event->button() != Qt::LeftButton || !playGesture_) {
        // Two clicks, each a click.
        mousePressEvent(event);
        return;
    }
    click_.stop();
    // Its release is not a click of its own.
    pressedAt_.reset();
    player_.togglePlay();
}

}  // namespace migo::linux_host::qt6

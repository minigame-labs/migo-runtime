#include <migo/linux/qt6/media.hpp>

#include "media_playback.hpp"
#include "media_recorder.hpp"
#include "media_videos.hpp"

#include <QHBoxLayout>
#include <QLabel>
#include <QMouseEvent>
#include <QPaintEvent>
#include <QPainter>
#include <QSlider>
#include <QStyle>
#include <QTimer>
#include <QToolButton>
#include <QVBoxLayout>
#include <QWidget>

#include <functional>
#include <memory>
#include <utility>

namespace migo::linux_host::qt6 {

namespace {

/// How long describing a video waits for its first frame.
constexpr GstClockTime kDescribeTimeout = 10 * GST_SECOND;
/// How often a playing video's position is shown.
constexpr int kPositionIntervalMs = 250;

/// "m:ss" for `ms`.
QString clock(qint64 ms) {
    const qint64 seconds = qMax<qint64>(0, ms) / 1000;
    return QStringLiteral("%1:%2").arg(seconds / 60).arg(seconds % 60, 2, 10, QLatin1Char('0'));
}

/// Where a video is drawn: its poster or frame fitted to it, on black, or a
/// warning sign when it cannot be played. A click is the player's.
class Screen final : public QWidget {
public:
    Screen(QWidget &parent, std::function<void()> clicked) : QWidget(&parent), clicked_(std::move(clicked)) {
        setObjectName(QStringLiteral("migoVideoScreen"));
        setSizePolicy(QSizePolicy::Ignored, QSizePolicy::Ignored);
        setMinimumSize(1, 1);
        setAttribute(Qt::WA_OpaquePaintEvent);
    }

    void present(Frame frame) {
        frame_ = std::move(frame);
        update();
    }

    void showWarning() {
        warning_ = true;
        frame_ = {};
        update();
    }

protected:
    void paintEvent(QPaintEvent *) override {
        QPainter painter(this);
        painter.fillRect(rect(), Qt::black);
        if (warning_) {
            const QPixmap sign = style()->standardIcon(QStyle::SP_MessageBoxWarning).pixmap(64, 64);
            QRect where({}, sign.deviceIndependentSize().toSize());
            where.moveCenter(rect().center());
            painter.drawPixmap(where, sign);
            return;
        }
        if (frame_.image.isNull()) return;
        QRect target({}, frame_.shown.scaled(size(), Qt::KeepAspectRatio));
        target.moveCenter(rect().center());
        painter.setRenderHint(QPainter::SmoothPixmapTransform);
        painter.drawImage(target, frame_.image);
    }

    void mousePressEvent(QMouseEvent *event) override {
        if (event->button() == Qt::LeftButton) clicked_();
    }

private:
    std::function<void()> clicked_;
    Frame frame_;
    bool warning_ = false;
};

/// The preview's player: a video, its controls under it -- play and pause,
/// where it is, dragged to move through it, and how long it plays. A click on
/// the video plays or pauses it too. It shows its poster, or its first frame,
/// until it plays, and stops when it goes.
class Player final : public QWidget {
public:
    /// Without GStreamer (`playable` false) it shows the poster's place as a
    /// warning sign.
    Player(const QByteArray &uri, const QImage &poster, bool playable, QWidget &parent) : QWidget(&parent) {
        auto *layout = new QVBoxLayout(this);
        layout->setContentsMargins(0, 0, 0, 0);
        screen_ = new Screen(*this, [this] { toggle(); });
        layout->addWidget(screen_, 1);
        auto *controls = new QHBoxLayout;
        play_ = new QToolButton(this);
        play_->setAutoRaise(true);
        play_->setIconSize({24, 24});
        // The arrows are the viewer's, not a way between the controls.
        play_->setFocusPolicy(Qt::NoFocus);
        connect(play_, &QToolButton::clicked, this, [this] { toggle(); });
        slider_ = new QSlider(Qt::Horizontal, this);
        slider_->setFocusPolicy(Qt::NoFocus);
        slider_->setRange(0, 0);
        // Dragged, it shows the nearest keyframe as it goes; let go, or
        // stepped, the very place.
        connect(slider_, &QSlider::sliderMoved, this, [this](int ms) { seek(ms, false); });
        connect(slider_, &QSlider::sliderReleased, this, [this] { seek(slider_->value(), true); });
        connect(slider_, &QSlider::actionTriggered, this, [this](int action) {
            if (action != QAbstractSlider::SliderMove) seek(slider_->sliderPosition(), true);
        });
        time_ = new QLabel(this);
        controls->addWidget(play_);
        controls->addWidget(slider_, 1);
        controls->addWidget(time_);
        layout->addLayout(controls);
        ticker_ = new QTimer(this);
        ticker_->setInterval(kPositionIntervalMs);
        connect(ticker_, &QTimer::timeout, this, [this] { showPosition(); });
        showButton();
        showPosition();

        if (!poster.isNull()) {
            screen_->present({poster, poster.size()});
            postered_ = true;
        }
        if (!playable) {
            fail();
            return;
        }
        Playback::Listener listener;
        listener.frameReady = [this] { takeFrame(); };
        listener.settled = [this] { showPosition(); };
        listener.durationChanged = [this] { showPosition(); };
        listener.ended = [this] { ended(); };
        listener.failed = [this](const QString &) { fail(); };
        listener.buffering = [this](int percent) { buffering(percent); };
        playback_ = Playback::open(uri, {}, std::move(listener));
        if (playback_ == nullptr) fail();
    }

    ~Player() override {
        // Stopped before what it reads goes.
        playback_.reset();
    }

private:
    void fail() {
        failed_ = true;
        playing_ = false;
        screen_->showWarning();
        play_->setEnabled(false);
        slider_->setEnabled(false);
        ticker_->stop();
    }

    void takeFrame() {
        Frame frame = playback_->takeFrame();
        // A poster stands until the video first plays.
        if (frame.image.isNull() || failed_ || (postered_ && !played_)) return;
        screen_->present(std::move(frame));
    }

    void ended() {
        playing_ = false;
        ended_ = true;
        playback_->setPlaying(false);
        showButton();
        showPosition();
        ticker_->stop();
    }

    /// A network stream filling its buffer: held while it fills.
    void buffering(int percent) {
        if (!playing_) return;
        const bool filling = percent < 100;
        if (filling == buffering_) return;
        buffering_ = filling;
        playback_->setPlaying(!filling);
    }

    void showPosition() {
        if (playback_ == nullptr) return;
        if (const qint64 duration = playback_->duration(); duration > 0) {
            slider_->setMaximum(static_cast<int>(duration / GST_MSECOND));
        }
        if (const qint64 position = playback_->position(); position >= 0 && !slider_->isSliderDown()) {
            slider_->setValue(static_cast<int>(ended_ ? slider_->maximum() : position / GST_MSECOND));
        }
        time_->setText(QStringLiteral("%1 / %2").arg(clock(slider_->value()), clock(slider_->maximum())));
    }

    void toggle() {
        if (failed_ || playback_ == nullptr) return;
        if (playing_) {
            playing_ = false;
            playback_->setPlaying(false);
            ticker_->stop();
        } else {
            // Played again from the start once it has ended.
            if (ended_) seek(0, true);
            playing_ = true;
            played_ = true;
            buffering_ = false;
            playback_->setPlaying(true);
            ticker_->start();
        }
        showButton();
    }

    void seek(int ms, bool exactly) {
        if (failed_ || playback_ == nullptr) return;
        ended_ = false;
        playback_->seek(static_cast<qint64>(qMax(0, ms)) * GST_MSECOND, exactly);
        showPosition();
    }

    void showButton() {
        play_->setIcon(style()->standardIcon(playing_ ? QStyle::SP_MediaPause : QStyle::SP_MediaPlay));
    }

    std::unique_ptr<Playback> playback_;
    Screen *screen_ = nullptr;
    QToolButton *play_ = nullptr;
    QSlider *slider_ = nullptr;
    QLabel *time_ = nullptr;
    QTimer *ticker_ = nullptr;
    bool postered_ = false;
    bool played_ = false;
    bool playing_ = false;
    bool ended_ = false;
    bool buffering_ = false;
    bool failed_ = false;
};

}  // namespace

MigoQtMedia::MigoQtMedia() {
    GError *error = nullptr;
    started_ = gst_init_check(nullptr, nullptr, &error) == TRUE;
    if (!started_) {
        qWarning("migo host kit: GStreamer did not start: %s", error != nullptr ? error->message : "no reason given");
        g_clear_error(&error);
    }
}

MigoQtMedia::~MigoQtMedia() = default;

std::optional<MigoQtVideoSupport::Description> MigoQtMedia::describe(const QString &file) const {
    if (!started_) return std::nullopt;
    const std::unique_ptr<Playbin> playbin = Playbin::make(uriOf(file), {false, false, false});
    if (playbin == nullptr) return std::nullopt;
    // Paused, it decodes its first frame and stops there.
    if (gst_element_set_state(playbin->pipeline(), GST_STATE_PAUSED) == GST_STATE_CHANGE_FAILURE ||
        gst_element_get_state(playbin->pipeline(), nullptr, nullptr, kDescribeTimeout) != GST_STATE_CHANGE_SUCCESS) {
        return std::nullopt;
    }
    GstSample *sample = gst_app_sink_try_pull_preroll(playbin->sink(), 0);
    if (sample == nullptr) return std::nullopt;
    Frame first = frameOf(sample);
    if (first.image.isNull()) return std::nullopt;
    Description described;
    gint64 duration = 0;
    if (gst_element_query_duration(playbin->pipeline(), GST_FORMAT_TIME, &duration) && duration > 0) {
        described.durationSeconds = static_cast<double>(duration) / GST_SECOND;
    }
    described.size = first.shown;
    // Its own pixels, before the sample they are over goes with the pipeline.
    described.cover = first.image.size() == first.shown
                          ? first.image.copy()
                          : first.image.scaled(first.shown, Qt::IgnoreAspectRatio, Qt::SmoothTransformation);
    return described;
}

QWidget *MigoQtMedia::player(const QString &source, const QImage &poster, QWidget &parent) const {
    return new Player(uriOf(source), poster, started_, parent);
}

std::unique_ptr<MigoQtRecorder> MigoQtMedia::recorder(MigoQtRecorder::Post post, MigoQtRecorder::PostFrame frame,
                                                      const QString &staging) const {
    if (!started_) return nullptr;
    return std::make_unique<GStreamerRecorder>(std::move(post), std::move(frame), staging);
}

std::unique_ptr<MigoQtVideoPlayers> MigoQtMedia::players(QWidget &stage, MigoQtVideoPlayers::Post post) const {
    // Without GStreamer nothing plays, and createVideo is the App's to answer.
    if (!started_) return nullptr;
    return std::make_unique<VideoPlayers>(stage, std::move(post));
}

}  // namespace migo::linux_host::qt6

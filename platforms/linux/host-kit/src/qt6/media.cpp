#include <migo/linux/qt6/media.hpp>

#include <gst/app/gstappsink.h>
#include <gst/gst.h>
#include <gst/video/video.h>

#include <QFileInfo>
#include <QHBoxLayout>
#include <QLabel>
#include <QMetaObject>
#include <QMouseEvent>
#include <QPaintEvent>
#include <QPainter>
#include <QSlider>
#include <QStyle>
#include <QTimer>
#include <QToolButton>
#include <QUrl>
#include <QVBoxLayout>
#include <QWidget>

#include <functional>
#include <memory>
#include <mutex>
#include <utility>

namespace migo::linux_host::qt6 {

namespace {

/// Where every frame goes: turned upright as the stream's tags say, as RGBx.
constexpr const char *kFrames =
    "videoconvert ! videoflip video-direction=auto ! videoconvert ! video/x-raw,format=RGBx ! appsink name=frames";
/// playbin's flags for describing: the video, deinterlaced, and no sound.
constexpr guint kVideoOnly = 0x1 | 0x200;
/// How long describing a video waits for its first frame.
constexpr GstClockTime kDescribeTimeout = 10 * GST_SECOND;
/// How often a playing video's position is shown.
constexpr int kPositionIntervalMs = 250;

/// The URI GStreamer reads `source` from: an http(s) URL as it is, a path as a
/// file URI.
QByteArray uriOf(const QString &source) {
    const QString scheme = QUrl(source).scheme().toLower();
    if (scheme == QLatin1String("http") || scheme == QLatin1String("https")) return source.toUtf8();
    return QUrl::fromLocalFile(QFileInfo(source).absoluteFilePath()).toEncoded();
}

/// A playbin and the sink its frames reach, stopped and released with it.
class Playbin final {
public:
    /// One playing `uri`, with its sound when `sound`; null when GStreamer
    /// lacks its base plugins. `sync` paces the frames by the clock -- playing
    /// -- or lets them come as decoded -- describing.
    static std::unique_ptr<Playbin> make(const QByteArray &uri, bool sound, bool sync) {
        GstElement *pipeline = gst_element_factory_make("playbin", nullptr);
        if (pipeline == nullptr) return nullptr;
        gst_object_ref_sink(pipeline);
        GError *error = nullptr;
        GstElement *output = gst_parse_bin_from_description(kFrames, TRUE, &error);
        if (output == nullptr) {
            g_clear_error(&error);
            gst_object_unref(pipeline);
            return nullptr;
        }
        auto made = std::unique_ptr<Playbin>(new Playbin(pipeline, gst_bin_get_by_name(GST_BIN(output), "frames")));
        // Only the latest frame is wanted: one shown late is one dropped.
        g_object_set(made->sink_, "sync", sync ? TRUE : FALSE, "max-buffers", 1U, "drop", TRUE, nullptr);
        // The pipeline takes the output bin, which keeps the sink.
        g_object_set(pipeline, "uri", uri.constData(), "video-sink", output, nullptr);
        if (!sound) g_object_set(pipeline, "flags", kVideoOnly, nullptr);
        return made;
    }

    ~Playbin() {
        gst_element_set_state(pipeline_, GST_STATE_NULL);
        gst_object_unref(sink_);
        gst_object_unref(pipeline_);
    }

    Playbin(const Playbin &) = delete;
    Playbin &operator=(const Playbin &) = delete;

    [[nodiscard]] GstElement *pipeline() const noexcept { return pipeline_; }
    [[nodiscard]] GstAppSink *sink() const noexcept { return GST_APP_SINK(sink_); }

private:
    Playbin(GstElement *pipeline, GstElement *sink) : pipeline_(pipeline), sink_(sink) {}

    GstElement *pipeline_;
    GstElement *sink_;
};

/// A decoded frame: the image over the sample's memory, which stays mapped
/// until the image goes, and the size it is meant to be seen at.
struct Frame {
    QImage image;
    QSize shown;
};

/// The frame `sample` holds, which it takes; an empty one when it cannot be read.
Frame frameOf(GstSample *sample) {
    GstVideoInfo info;
    GstBuffer *buffer = gst_sample_get_buffer(sample);
    GstCaps *caps = gst_sample_get_caps(sample);
    struct Mapped {
        GstSample *sample;
        GstMapInfo map;
    };
    auto *mapped = new Mapped{sample, {}};
    if (buffer == nullptr || caps == nullptr || !gst_video_info_from_caps(&info, caps) ||
        !gst_buffer_map(buffer, &mapped->map, GST_MAP_READ)) {
        gst_sample_unref(sample);
        delete mapped;
        return {};
    }
    const int width = GST_VIDEO_INFO_WIDTH(&info);
    const int height = GST_VIDEO_INFO_HEIGHT(&info);
    QImage image(
        mapped->map.data, width, height, GST_VIDEO_INFO_PLANE_STRIDE(&info, 0), QImage::Format_RGBX8888,
        [](void *data) {
            auto *done = static_cast<Mapped *>(data);
            gst_buffer_unmap(gst_sample_get_buffer(done->sample), &done->map);
            gst_sample_unref(done->sample);
            delete done;
        },
        mapped);
    // Pixels that are not square are seen wider or narrower than stored.
    const int par_n = qMax(1, GST_VIDEO_INFO_PAR_N(&info));
    const int par_d = qMax(1, GST_VIDEO_INFO_PAR_D(&info));
    const QSize shown(qMax(1, static_cast<int>(qRound64(static_cast<qint64>(width) * par_n / static_cast<double>(par_d)))),
                      height);
    return {std::move(image), shown};
}

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

class Player;

/// Between GStreamer's threads and the player: they reach it through this,
/// which it cuts before it goes. The latest frame waits here, so a player
/// behind takes only the newest.
struct Relay {
    std::mutex mutex;
    Player *player = nullptr;
    Frame latest;
    bool posted = false;
};

/// A video, its controls under it: play and pause, where it is -- dragged to
/// move through it -- and how long it plays. A click on the video plays or
/// pauses it too. It shows its poster, or its first frame, until it plays, and
/// stops when it goes.
class Player final : public QWidget {
public:
    Player(const QByteArray &uri, const QImage &poster, QWidget &parent)
        : QWidget(&parent), relay_(std::make_shared<Relay>()) {
        relay_->player = this;
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
        connect(slider_, &QSlider::sliderMoved, this, [this](int ms) { seek(ms, GST_SEEK_FLAG_KEY_UNIT); });
        connect(slider_, &QSlider::sliderReleased, this, [this] { seek(slider_->value(), GST_SEEK_FLAG_ACCURATE); });
        connect(slider_, &QSlider::actionTriggered, this, [this](int action) {
            if (action != QAbstractSlider::SliderMove) seek(slider_->sliderPosition(), GST_SEEK_FLAG_ACCURATE);
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
        playbin_ = Playbin::make(uri, true, true);
        if (playbin_ == nullptr) {
            fail();
            return;
        }
        listen();
        // Paused: its first frame, and its length, before it plays.
        if (gst_element_set_state(playbin_->pipeline(), GST_STATE_PAUSED) == GST_STATE_CHANGE_FAILURE) fail();
    }

    ~Player() override {
        // GStreamer's threads reach nothing from here on, and are stopped
        // before what they read goes.
        {
            const std::lock_guard<std::mutex> lock(relay_->mutex);
            relay_->player = nullptr;
            relay_->latest = {};
        }
        playbin_.reset();
    }

    /// The latest frame, posted by the sink's thread.
    void takeFrame() {
        Frame frame;
        {
            const std::lock_guard<std::mutex> lock(relay_->mutex);
            frame = std::move(relay_->latest);
            relay_->posted = false;
        }
        // A poster stands until the video first plays.
        if (frame.image.isNull() || (postered_ && !played_)) return;
        screen_->present(std::move(frame));
    }

    void ended() {
        playing_ = false;
        ended_ = true;
        if (playbin_ != nullptr) gst_element_set_state(playbin_->pipeline(), GST_STATE_PAUSED);
        showButton();
        showPosition();
        ticker_->stop();
    }

    void fail() {
        failed_ = true;
        playing_ = false;
        screen_->showWarning();
        play_->setEnabled(false);
        slider_->setEnabled(false);
        ticker_->stop();
    }

    /// A network stream filling its buffer: held while it fills.
    void buffering(int percent) {
        if (playbin_ == nullptr || !playing_) return;
        const bool filling = percent < 100;
        if (filling == buffering_) return;
        buffering_ = filling;
        gst_element_set_state(playbin_->pipeline(), filling ? GST_STATE_PAUSED : GST_STATE_PLAYING);
    }

    void showPosition() {
        gint64 duration = 0;
        gint64 position = 0;
        if (playbin_ != nullptr && gst_element_query_duration(playbin_->pipeline(), GST_FORMAT_TIME, &duration) &&
            duration > 0) {
            slider_->setMaximum(static_cast<int>(duration / GST_MSECOND));
        }
        if (playbin_ != nullptr && gst_element_query_position(playbin_->pipeline(), GST_FORMAT_TIME, &position) &&
            !slider_->isSliderDown()) {
            slider_->setValue(static_cast<int>(ended_ ? slider_->maximum() : position / GST_MSECOND));
        }
        time_->setText(QStringLiteral("%1 / %2").arg(clock(slider_->value()), clock(slider_->maximum())));
    }

private:
    /// Hears the pipeline's ends, failures and buffering, and its frames, on
    /// GStreamer's threads, and passes them to this on its own.
    void listen() {
        GstBus *bus = gst_element_get_bus(playbin_->pipeline());
        gst_bus_set_sync_handler(
            bus,
            [](GstBus *, GstMessage *message, gpointer data) -> GstBusSyncReply {
                const auto &relay = *static_cast<std::shared_ptr<Relay> *>(data);
                int percent = 0;
                const GstMessageType type = GST_MESSAGE_TYPE(message);
                if (type == GST_MESSAGE_BUFFERING) gst_message_parse_buffering(message, &percent);
                // Nothing reads the bus but this: every message is dropped here,
                // and one dropped is the handler's to release.
                gst_message_unref(message);
                const std::lock_guard<std::mutex> lock(relay->mutex);
                Player *player = relay->player;
                if (player == nullptr) return GST_BUS_DROP;
                switch (type) {
                    case GST_MESSAGE_EOS:
                        QMetaObject::invokeMethod(player, [player] { player->ended(); }, Qt::QueuedConnection);
                        break;
                    case GST_MESSAGE_ERROR:
                        QMetaObject::invokeMethod(player, [player] { player->fail(); }, Qt::QueuedConnection);
                        break;
                    case GST_MESSAGE_BUFFERING:
                        QMetaObject::invokeMethod(
                            player, [player, percent] { player->buffering(percent); }, Qt::QueuedConnection);
                        break;
                    case GST_MESSAGE_ASYNC_DONE:
                    case GST_MESSAGE_DURATION_CHANGED:
                        QMetaObject::invokeMethod(player, [player] { player->showPosition(); }, Qt::QueuedConnection);
                        break;
                    default:
                        break;
                }
                return GST_BUS_DROP;
            },
            new std::shared_ptr<Relay>(relay_), [](gpointer data) { delete static_cast<std::shared_ptr<Relay> *>(data); });
        gst_object_unref(bus);

        GstAppSinkCallbacks callbacks{};
        callbacks.new_preroll = [](GstAppSink *sink, gpointer data) { return deliver(gst_app_sink_pull_preroll(sink), data); };
        callbacks.new_sample = [](GstAppSink *sink, gpointer data) { return deliver(gst_app_sink_pull_sample(sink), data); };
        gst_app_sink_set_callbacks(playbin_->sink(), &callbacks, new std::shared_ptr<Relay>(relay_),
                                   [](gpointer data) { delete static_cast<std::shared_ptr<Relay> *>(data); });
    }

    /// `sample`, taken, as the latest frame: the player is told once, however
    /// many come before it looks.
    static GstFlowReturn deliver(GstSample *sample, gpointer data) {
        if (sample == nullptr) return GST_FLOW_OK;
        Frame frame = frameOf(sample);
        const auto &relay = *static_cast<std::shared_ptr<Relay> *>(data);
        const std::lock_guard<std::mutex> lock(relay->mutex);
        Player *player = relay->player;
        if (player == nullptr) return GST_FLOW_OK;
        relay->latest = std::move(frame);
        if (!relay->posted) {
            relay->posted = true;
            QMetaObject::invokeMethod(player, [player] { player->takeFrame(); }, Qt::QueuedConnection);
        }
        return GST_FLOW_OK;
    }

    void toggle() {
        if (failed_ || playbin_ == nullptr) return;
        if (playing_) {
            playing_ = false;
            gst_element_set_state(playbin_->pipeline(), GST_STATE_PAUSED);
            ticker_->stop();
        } else {
            // Played again from the start once it has ended.
            if (ended_) seek(0, GST_SEEK_FLAG_ACCURATE);
            playing_ = true;
            played_ = true;
            buffering_ = false;
            gst_element_set_state(playbin_->pipeline(), GST_STATE_PLAYING);
            ticker_->start();
        }
        showButton();
    }

    void seek(int ms, GstSeekFlags how) {
        if (failed_ || playbin_ == nullptr) return;
        ended_ = false;
        gst_element_seek_simple(playbin_->pipeline(), GST_FORMAT_TIME,
                                static_cast<GstSeekFlags>(GST_SEEK_FLAG_FLUSH | how), qMax(0, ms) * GST_MSECOND);
        showPosition();
    }

    void showButton() {
        play_->setIcon(style()->standardIcon(playing_ ? QStyle::SP_MediaPause : QStyle::SP_MediaPlay));
    }

    const std::shared_ptr<Relay> relay_;
    std::unique_ptr<Playbin> playbin_;
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
    const std::unique_ptr<Playbin> playbin = Playbin::make(uriOf(file), false, false);
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
    auto *made = new Player(uriOf(source), poster, parent);
    if (!started_) made->fail();
    return made;
}

}  // namespace migo::linux_host::qt6

#include "media_playback.hpp"

#include <gst/video/video.h>

#include <QFileInfo>
#include <QMetaObject>
#include <QUrl>

#include <algorithm>
#include <mutex>
#include <utility>

namespace migo::linux_host::qt6 {

namespace {

/// Where every frame goes: turned upright as the stream's tags say, as RGBx.
constexpr const char *kFrames =
    "videoconvert ! videoflip video-direction=auto ! videoconvert ! video/x-raw,format=RGBx ! appsink name=frames";
/// playbin's flags for no sound: the video, deinterlaced.
constexpr guint kVideoOnly = 0x1 | 0x200;
/// playbin's flag that downloads a network source as it plays.
constexpr guint kDownload = 0x80;

}  // namespace

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

bool isRemote(const QString &source) {
    const QString scheme = QUrl(source).scheme().toLower();
    return scheme == QLatin1String("http") || scheme == QLatin1String("https");
}

QByteArray uriOf(const QString &source) {
    if (isRemote(source) || QUrl(source).scheme().toLower() == QLatin1String("file")) return source.toUtf8();
    return QUrl::fromLocalFile(QFileInfo(source).absoluteFilePath()).toEncoded();
}

// ---- the playbin ---------------------------------------------------------------------------------

std::unique_ptr<Playbin> Playbin::make(const QByteArray &uri, Mode mode) {
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
    g_object_set(made->sink_, "sync", mode.sync ? TRUE : FALSE, "max-buffers", 1U, "drop", TRUE, nullptr);
    // The pipeline takes the output bin, which keeps the sink.
    g_object_set(pipeline, "uri", uri.constData(), "video-sink", output, nullptr);
    if (!mode.sound) {
        g_object_set(pipeline, "flags", kVideoOnly, nullptr);
    } else if (GstElement *tempo = gst_element_factory_make("scaletempo", nullptr); tempo != nullptr) {
        // Played faster or slower, its voices keep their pitch.
        g_object_set(pipeline, "audio-filter", tempo, nullptr);
    }
    if (mode.download) {
        guint flags = 0;
        g_object_get(pipeline, "flags", &flags, nullptr);
        g_object_set(pipeline, "flags", flags | kDownload, nullptr);
    }
    return made;
}

Playbin::~Playbin() {
    gst_element_set_state(pipeline_, GST_STATE_NULL);
    gst_object_unref(sink_);
    gst_object_unref(pipeline_);
}

// ---- the playback --------------------------------------------------------------------------------

/// Between GStreamer's threads and the playback: they reach it through this,
/// which it cuts before it goes. The newest frame waits here.
struct Playback::Relay {
    std::mutex mutex;
    Playback *playback = nullptr;
    Frame latest;
    bool posted = false;
};

std::unique_ptr<Playback> Playback::open(const QByteArray &uri, Playbin::Mode mode, Listener listener) {
    std::unique_ptr<Playbin> playbin = Playbin::make(uri, mode);
    if (playbin == nullptr) return nullptr;
    const bool local = QUrl::fromEncoded(uri).isLocalFile();
    auto made = std::unique_ptr<Playback>(new Playback(std::move(playbin), local, std::move(listener)));
    made->listen();
    // Paused: its first frame, and its length, before it plays.
    if (gst_element_set_state(made->playbin_->pipeline(), GST_STATE_PAUSED) == GST_STATE_CHANGE_FAILURE) {
        // GStreamer says why on the bus, as a refusal later would be told.
        GstBus *bus = gst_element_get_bus(made->playbin_->pipeline());
        GError *error = g_error_new_literal(GST_CORE_ERROR, GST_CORE_ERROR_STATE_CHANGE, "the video cannot be opened");
        gst_bus_post(bus, gst_message_new_error(GST_OBJECT(made->playbin_->pipeline()), error, nullptr));
        g_error_free(error);
        gst_object_unref(bus);
    }
    return made;
}

Playback::Playback(std::unique_ptr<Playbin> playbin, bool local, Listener listener)
    : relay_(std::make_shared<Relay>()), playbin_(std::move(playbin)), local_(local), listener_(std::move(listener)) {
    relay_->playback = this;
}

Playback::~Playback() {
    // GStreamer's threads reach nothing from here on, and are stopped before
    // what they read goes.
    {
        const std::lock_guard<std::mutex> lock(relay_->mutex);
        relay_->playback = nullptr;
        relay_->latest = {};
    }
    playbin_.reset();
}

Frame Playback::takeFrame() {
    const std::lock_guard<std::mutex> lock(relay_->mutex);
    relay_->posted = false;
    return std::move(relay_->latest);
}

bool Playback::setPlaying(bool playing) {
    return gst_element_set_state(playbin_->pipeline(), playing ? GST_STATE_PLAYING : GST_STATE_PAUSED) !=
           GST_STATE_CHANGE_FAILURE;
}

void Playback::seek(qint64 ns, bool exactly) {
    target_ = qMax<qint64>(0, ns);
    const auto flags =
        static_cast<GstSeekFlags>(GST_SEEK_FLAG_FLUSH | (exactly ? GST_SEEK_FLAG_ACCURATE : GST_SEEK_FLAG_KEY_UNIT));
    gst_element_seek(playbin_->pipeline(), rate_, GST_FORMAT_TIME, flags, GST_SEEK_TYPE_SET, target_,
                     GST_SEEK_TYPE_NONE, static_cast<gint64>(GST_CLOCK_TIME_NONE));
}

void Playback::setRate(double rate) {
    if (!(rate > 0) || rate == rate_) return;
    rate_ = rate;
    // A seek is how a pipeline is given a rate: to where it is, or to where a
    // seek still on its way is going. (GStreamer's instant rate change answers
    // a paused pipeline that it took the rate, and plays it at the old one.)
    if (const qint64 at = target_ >= 0 ? target_ : position(); at >= 0) seek(at, true);
}

void Playback::setMuted(bool muted) { g_object_set(playbin_->pipeline(), "mute", muted ? TRUE : FALSE, nullptr); }

qint64 Playback::position() const {
    gint64 at = 0;
    return gst_element_query_position(playbin_->pipeline(), GST_FORMAT_TIME, &at) && at >= 0 ? at : -1;
}

qint64 Playback::duration() const {
    gint64 length = 0;
    return gst_element_query_duration(playbin_->pipeline(), GST_FORMAT_TIME, &length) && length > 0 ? length : -1;
}

int Playback::buffered() const {
    if (local_) return 100;
    GstQuery *query = gst_query_new_buffering(GST_FORMAT_PERCENT);
    int percent = -1;
    if (gst_element_query(playbin_->pipeline(), query)) {
        // The furthest of what has arrived.
        GstFormat format = GST_FORMAT_UNDEFINED;
        gst_query_parse_buffering_range(query, &format, nullptr, nullptr, nullptr);
        if (format == GST_FORMAT_PERCENT) {
            gint64 furthest = -1;
            for (guint range = 0; range < gst_query_get_n_buffering_ranges(query); ++range) {
                gint64 start = 0;
                gint64 stop = 0;
                if (gst_query_parse_nth_buffering_range(query, range, &start, &stop)) furthest = qMax(furthest, stop);
            }
            if (furthest >= 0) {
                percent = static_cast<int>(std::clamp<gint64>(furthest * 100 / GST_FORMAT_PERCENT_MAX, 0, 100));
            }
        }
    }
    gst_query_unref(query);
    return percent;
}

void Playback::listen() {
    GstBus *bus = gst_element_get_bus(playbin_->pipeline());
    gst_bus_set_sync_handler(
        bus,
        [](GstBus *, GstMessage *message, gpointer data) -> GstBusSyncReply {
            const auto &relay = *static_cast<std::shared_ptr<Relay> *>(data);
            const GstMessageType type = GST_MESSAGE_TYPE(message);
            int percent = 0;
            QString reason;
            if (type == GST_MESSAGE_BUFFERING) gst_message_parse_buffering(message, &percent);
            if (type == GST_MESSAGE_ERROR) {
                GError *error = nullptr;
                gst_message_parse_error(message, &error, nullptr);
                reason = error != nullptr && error->message != nullptr ? QString::fromUtf8(error->message)
                                                                       : QStringLiteral("the video cannot be played");
                g_clear_error(&error);
            }
            // Nothing reads the bus but this: every message is dropped here,
            // and one dropped is the handler's to release.
            gst_message_unref(message);
            const std::lock_guard<std::mutex> lock(relay->mutex);
            Playback *playback = relay->playback;
            if (playback == nullptr) return GST_BUS_DROP;
            const auto tell = [playback](auto what) {
                QMetaObject::invokeMethod(playback, std::move(what), Qt::QueuedConnection);
            };
            switch (type) {
                case GST_MESSAGE_EOS:
                    tell([playback] {
                        if (playback->listener_.ended) playback->listener_.ended();
                    });
                    break;
                case GST_MESSAGE_ERROR:
                    tell([playback, reason] {
                        if (playback->listener_.failed) playback->listener_.failed(reason);
                    });
                    break;
                case GST_MESSAGE_BUFFERING:
                    tell([playback, percent] {
                        if (playback->listener_.buffering) playback->listener_.buffering(percent);
                    });
                    break;
                case GST_MESSAGE_ASYNC_DONE:
                    tell([playback] {
                        playback->target_ = -1;
                        if (playback->listener_.settled) playback->listener_.settled();
                    });
                    break;
                case GST_MESSAGE_DURATION_CHANGED:
                    tell([playback] {
                        if (playback->listener_.durationChanged) playback->listener_.durationChanged();
                    });
                    break;
                default:
                    break;
            }
            return GST_BUS_DROP;
        },
        new std::shared_ptr<Relay>(relay_), [](gpointer data) { delete static_cast<std::shared_ptr<Relay> *>(data); });
    gst_object_unref(bus);

    // `sample`, taken, as the newest frame: the playback is told once, however
    // many come before it looks.
    static constexpr auto deliver = [](GstSample *sample, gpointer data) -> GstFlowReturn {
        if (sample == nullptr) return GST_FLOW_OK;
        Frame frame = frameOf(sample);
        const auto &relay = *static_cast<std::shared_ptr<Relay> *>(data);
        const std::lock_guard<std::mutex> lock(relay->mutex);
        Playback *playback = relay->playback;
        if (playback == nullptr) return GST_FLOW_OK;
        relay->latest = std::move(frame);
        if (!relay->posted) {
            relay->posted = true;
            QMetaObject::invokeMethod(
                playback,
                [playback] {
                    if (playback->listener_.frameReady) playback->listener_.frameReady();
                },
                Qt::QueuedConnection);
        }
        return GST_FLOW_OK;
    };
    GstAppSinkCallbacks callbacks{};
    callbacks.new_preroll = [](GstAppSink *sink, gpointer data) { return deliver(gst_app_sink_pull_preroll(sink), data); };
    callbacks.new_sample = [](GstAppSink *sink, gpointer data) { return deliver(gst_app_sink_pull_sample(sink), data); };
    gst_app_sink_set_callbacks(playbin_->sink(), &callbacks, new std::shared_ptr<Relay>(relay_),
                               [](gpointer data) { delete static_cast<std::shared_ptr<Relay> *>(data); });
}

}  // namespace migo::linux_host::qt6

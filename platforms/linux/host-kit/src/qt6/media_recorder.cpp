#include "media_recorder.hpp"

#include "staging.hpp"

#include <gst/app/gstappsink.h>
#include <gst/app/gstappsrc.h>
#include <gst/gst.h>

#include <QFile>
#include <QFileInfo>
#include <QJsonValue>
#include <QMetaObject>
#include <QThreadPool>
#include <QtEndian>

#include <algorithm>
#include <array>
#include <atomic>
#include <cmath>
#include <limits>
#include <mutex>
#include <utility>

namespace migo::linux_host::qt6 {

namespace {

/// The sample rates the reference lists.
constexpr std::array<int, 9> kSampleRates{8000, 11025, 12000, 16000, 22050, 24000, 32000, 44100, 48000};
/// GStreamer's AAC encoders, the one most often installed first: libav's
/// (gstreamer1.0-libav), then FDK's, VisualOn's and FAAC's (the bad plugins).
constexpr std::array<const char *, 4> kAacEncoders{"avenc_aac", "fdkaacenc", "voaacenc", "faac"};
/// How long finishing a recording waits for the encoder to write its last.
constexpr GstClockTime kFinishTimeout = 10 * GST_SECOND;

/// What content's onError hears: the words after the API's.
QString errMsg(const QString &reason) { return QStringLiteral("recorderManager.start:fail %1").arg(reason); }

/// `key`'s whole number, `fallback` when it is not there, and one no option
/// takes when it is not a number an int holds.
int number(const QJsonObject &request, const char *key, int fallback) {
    const QJsonValue value = request.value(QLatin1String(key));
    if (value.isUndefined() || value.isNull()) return fallback;
    const double asked = value.toDouble(std::nan(""));
    if (!std::isfinite(asked) || asked < std::numeric_limits<int>::min() || asked > std::numeric_limits<int>::max()) {
        return std::numeric_limits<int>::min();
    }
    return static_cast<int>(asked);
}

/// A canonical WAV header for `data_bytes` of 16-bit samples.
QByteArray wavHeader(const RecordingOptions &options, qint64 data_bytes) {
    QByteArray header;
    header.reserve(44);
    const auto u32 = [&header](quint32 value) {
        const quint32 little = qToLittleEndian(value);
        header.append(reinterpret_cast<const char *>(&little), 4);
    };
    const auto u16 = [&header](quint16 value) {
        const quint16 little = qToLittleEndian(value);
        header.append(reinterpret_cast<const char *>(&little), 2);
    };
    const auto rate = static_cast<quint32>(options.sampleRate);
    const auto channels = static_cast<quint16>(options.channels);
    header.append("RIFF", 4);
    u32(static_cast<quint32>(36 + data_bytes));
    header.append("WAVEfmt ", 8);
    u32(16);
    u16(1);
    u16(channels);
    u32(rate);
    u32(rate * channels * 2);
    u16(static_cast<quint16>(channels * 2));
    u16(16);
    header.append("data", 4);
    u32(static_cast<quint32>(data_bytes));
    return header;
}

/// The caps of the samples a recording is made from.
QByteArray samplesCaps(const RecordingOptions &options) {
    return QByteArray("audio/x-raw,format=S16LE,layout=interleaved,rate=") + QByteArray::number(options.sampleRate) +
           ",channels=" + QByteArray::number(options.channels);
}

/// The reason in the first error on `bus`, if one is waiting there.
std::optional<QString> errorOn(GstBus *bus, GstClockTime timeout = 0) {
    GstMessage *message = gst_bus_timed_pop_filtered(bus, timeout, GST_MESSAGE_ERROR);
    if (message == nullptr) return std::nullopt;
    GError *error = nullptr;
    gst_message_parse_error(message, &error, nullptr);
    QString reason = error != nullptr && error->message != nullptr ? QString::fromUtf8(error->message)
                                                                   : QStringLiteral("no reason given");
    g_clear_error(&error);
    gst_message_unref(message);
    return reason;
}

}  // namespace

// ---- the options ---------------------------------------------------------------------------------

std::uint32_t RecordingOptions::recordingIdOf(const QJsonObject &request) {
    const double id = request.value(QStringLiteral("recordingId")).toDouble(0);
    return id >= 1 && id <= std::numeric_limits<std::uint32_t>::max() ? static_cast<std::uint32_t>(id) : 0;
}

std::optional<RecordingOptions> RecordingOptions::parse(const QJsonObject &request) {
    RecordingOptions options;
    options.recordingId = recordingIdOf(request);
    options.durationMs = number(request, "duration", options.durationMs);
    options.sampleRate = number(request, "sampleRate", options.sampleRate);
    options.channels = number(request, "numberOfChannels", options.channels);
    options.bitRate = number(request, "encodeBitRate", options.bitRate);
    const int frame_kb = number(request, "frameSize", 0);
    const QString format = request.value(QStringLiteral("format")).toString(QStringLiteral("aac")).toLower();
    if (format == QLatin1String("pcm")) {
        options.format = Format::Pcm;
    } else if (format == QLatin1String("wav")) {
        options.format = Format::Wav;
    } else if (format == QLatin1String("aac") || format == QLatin1String("mp3")) {
        options.format = Format::Aac;
    } else {
        return std::nullopt;
    }
    const bool rate = std::find(kSampleRates.begin(), kSampleRates.end(), options.sampleRate) != kSampleRates.end();
    // A frame is handed over whole, and the library takes up to 1 MiB at a time.
    if (!rate || options.channels < 1 || options.channels > 2 || options.bitRate <= 0 || options.durationMs < 1 ||
        options.durationMs > kLongestMs || frame_kb < 0 || frame_kb > 1024) {
        return std::nullopt;
    }
    options.frameBytes = frame_kb * 1024;
    return options;
}

QString RecordingOptions::suffix() const {
    switch (format) {
        case Format::Pcm:
            return QStringLiteral("pcm");
        case Format::Wav:
            return QStringLiteral("wav");
        case Format::Aac:
            return frameBytes > 0 ? QStringLiteral("aac") : QStringLiteral("m4a");
    }
    return {};
}

// ---- where the samples go ------------------------------------------------------------------------

/// Where a recording's samples go.
class RecordingWriter::Sink {
public:
    virtual ~Sink() = default;
    /// `length` bytes of samples, which begin `at` samples into the recording;
    /// the bytes they became appended to `out`, when it is given. False, and
    /// why in `error`, when they could not be written.
    virtual bool write(const char *bytes, qint64 length, qint64 at, QByteArray *out, QString &error) = 0;
    /// The file finished and closed; the bytes it still held appended to `out`,
    /// when it is given. Harmless twice.
    virtual bool finish(QByteArray *out, QString &error) = 0;
};

namespace {

/// The samples themselves, raw or behind a WAV header.
class SampleSink final : public RecordingWriter::Sink {
public:
    SampleSink(const RecordingOptions &options, const QString &path, bool wav)
        : options_(options), file_(path), wav_(wav) {}

    bool open(QString &error) {
        if (!file_.open(QIODevice::WriteOnly | QIODevice::Truncate) ||
            (wav_ && file_.write(wavHeader(options_, 0)) != 44)) {
            error = QStringLiteral("there is no file to record into: %1").arg(file_.errorString());
            return false;
        }
        return true;
    }

    bool write(const char *bytes, qint64 length, qint64, QByteArray *out, QString &error) override {
        if (file_.write(bytes, length) != length) {
            error = QStringLiteral("the recording could not be written: %1").arg(file_.errorString());
            return false;
        }
        written_ += length;
        if (out != nullptr) out->append(bytes, length);
        return true;
    }

    bool finish(QByteArray *, QString &error) override {
        if (!file_.isOpen()) return true;
        const bool headed = !wav_ || (file_.seek(0) && file_.write(wavHeader(options_, written_)) == 44);
        const bool flushed = file_.flush();
        file_.close();
        if (!headed || !flushed) {
            error = QStringLiteral("the recording could not be written: %1").arg(file_.errorString());
            return false;
        }
        return true;
    }

private:
    const RecordingOptions options_;
    QFile file_;
    const bool wav_;
    qint64 written_ = 0;
};

/// AAC, GStreamer's encoder's: an ADTS stream this writes as the packets come,
/// or an MPEG-4 file GStreamer's muxer writes whole.
class AacSink final : public RecordingWriter::Sink {
public:
    AacSink(const RecordingOptions &options, const QString &path) : options_(options), path_(path), file_(path) {}

    ~AacSink() override { close(); }

    bool open(QString &error) {
        const char *encoder = nullptr;
        for (const char *name : kAacEncoders) {
            if (GstElementFactory *factory = gst_element_factory_find(name); factory != nullptr) {
                gst_object_unref(factory);
                encoder = name;
                break;
            }
        }
        if (encoder == nullptr) {
            error = QStringLiteral("there is no AAC encoder: GStreamer's libav plugins (gstreamer1.0-libav) are one");
            return false;
        }
        adts_ = options_.frameBytes > 0;
        const QByteArray description =
            QByteArray("appsrc name=samples format=time block=true caps=\"") + samplesCaps(options_) + "\"" +
            " ! audioconvert ! " + encoder + " name=encoder ! aacparse ! " +
            (adts_ ? "audio/mpeg,mpegversion=4,stream-format=adts ! appsink name=packets sync=false"
                   : "mp4mux ! filesink name=file");
        GError *failure = nullptr;
        pipeline_ = gst_parse_launch(description.constData(), &failure);
        if (pipeline_ == nullptr) {
            error = QStringLiteral("the recording could not be encoded: %1")
                        .arg(failure != nullptr ? QString::fromUtf8(failure->message) : QString());
            g_clear_error(&failure);
            return false;
        }
        g_clear_error(&failure);
        gst_object_ref_sink(pipeline_);
        source_ = gst_bin_get_by_name(GST_BIN(pipeline_), "samples");
        GstElement *encoding = gst_bin_get_by_name(GST_BIN(pipeline_), "encoder");
        // The bit rate asked for, in whatever integer type the encoder takes it.
        GValue rate = G_VALUE_INIT;
        g_value_init(&rate, G_TYPE_INT);
        g_value_set_int(&rate, options_.bitRate);
        g_object_set_property(G_OBJECT(encoding), "bitrate", &rate);
        g_value_unset(&rate);
        gst_object_unref(encoding);
        if (adts_) {
            packets_ = gst_bin_get_by_name(GST_BIN(pipeline_), "packets");
            if (!file_.open(QIODevice::WriteOnly | QIODevice::Truncate)) {
                error = QStringLiteral("there is no file to record into: %1").arg(file_.errorString());
                return false;
            }
        } else {
            GstElement *file = gst_bin_get_by_name(GST_BIN(pipeline_), "file");
            const QByteArray location = QFile::encodeName(path_);
            g_object_set(file, "location", location.constData(), nullptr);
            gst_object_unref(file);
        }
        bus_ = gst_element_get_bus(pipeline_);
        if (gst_element_set_state(pipeline_, GST_STATE_PLAYING) == GST_STATE_CHANGE_FAILURE) {
            error = QStringLiteral("the recording could not be encoded: %1")
                        .arg(errorOn(bus_).value_or(QStringLiteral("the encoder did not start")));
            return false;
        }
        return true;
    }

    bool write(const char *bytes, qint64 length, qint64 at, QByteArray *out, QString &error) override {
        if (const std::optional<QString> failed = errorOn(bus_)) {
            error = QStringLiteral("the recording could not be encoded: %1").arg(*failed);
            return false;
        }
        GstBuffer *buffer = gst_buffer_new_memdup(bytes, static_cast<gsize>(length));
        const auto rate = static_cast<guint64>(options_.sampleRate);
        const auto samples = static_cast<guint64>(length / (2 * options_.channels));
        // Stamped by the samples recorded, so a pause leaves no gap.
        GST_BUFFER_PTS(buffer) = gst_util_uint64_scale(static_cast<guint64>(at), GST_SECOND, rate);
        GST_BUFFER_DURATION(buffer) = gst_util_uint64_scale(samples, GST_SECOND, rate);
        if (gst_app_src_push_buffer(GST_APP_SRC(source_), buffer) != GST_FLOW_OK) {
            error = QStringLiteral("the recording could not be encoded");
            return false;
        }
        pushed_ = true;
        return drain(0, out, error);
    }

    bool finish(QByteArray *out, QString &error) override {
        if (pipeline_ == nullptr || finished_) return true;
        finished_ = true;
        if (!pushed_) {
            // Nothing recorded: an empty file of its kind, which no encoder
            // makes of no samples.
            close();
            QFile empty(path_);
            if (!empty.open(QIODevice::WriteOnly | QIODevice::Truncate)) {
                error = QStringLiteral("the recording could not be written: %1").arg(empty.errorString());
                return false;
            }
            return true;
        }
        gst_app_src_end_of_stream(GST_APP_SRC(source_));
        bool ok = true;
        if (adts_) {
            ok = drain(kFinishTimeout, out, error);
            if (ok && !gst_app_sink_is_eos(GST_APP_SINK(packets_))) {
                error = QStringLiteral("the recording could not be encoded: the encoder did not finish");
                ok = false;
            }
        } else {
            GstMessage *ended = gst_bus_timed_pop_filtered(
                bus_, kFinishTimeout, static_cast<GstMessageType>(GST_MESSAGE_EOS | GST_MESSAGE_ERROR));
            if (ended == nullptr || GST_MESSAGE_TYPE(ended) != GST_MESSAGE_EOS) {
                QString reason = QStringLiteral("the muxer did not finish");
                if (ended != nullptr) {
                    GError *failure = nullptr;
                    gst_message_parse_error(ended, &failure, nullptr);
                    if (failure != nullptr) reason = QString::fromUtf8(failure->message);
                    g_clear_error(&failure);
                }
                error = QStringLiteral("the recording could not be encoded: %1").arg(reason);
                ok = false;
            }
            if (ended != nullptr) gst_message_unref(ended);
        }
        close();
        return ok;
    }

private:
    /// The ADTS packets the encoder has made, written and appended to `out`:
    /// those already there, or with a `timeout` every one until its end.
    bool drain(GstClockTime timeout, QByteArray *out, QString &error) {
        if (!adts_) return true;
        while (GstSample *sample = gst_app_sink_try_pull_sample(GST_APP_SINK(packets_), timeout)) {
            GstBuffer *buffer = gst_sample_get_buffer(sample);
            GstMapInfo map;
            bool written = true;
            if (buffer != nullptr && gst_buffer_map(buffer, &map, GST_MAP_READ)) {
                const auto *data = reinterpret_cast<const char *>(map.data);
                const auto size = static_cast<qint64>(map.size);
                written = file_.write(data, size) == size;
                if (written && out != nullptr) out->append(data, size);
                gst_buffer_unmap(buffer, &map);
            }
            gst_sample_unref(sample);
            if (!written) {
                error = QStringLiteral("the recording could not be written: %1").arg(file_.errorString());
                return false;
            }
        }
        if (const std::optional<QString> failed = errorOn(bus_)) {
            error = QStringLiteral("the recording could not be encoded: %1").arg(*failed);
            return false;
        }
        return true;
    }

    void close() {
        if (pipeline_ != nullptr) gst_element_set_state(pipeline_, GST_STATE_NULL);
        for (GstElement **element : {&source_, &packets_, &pipeline_}) {
            if (*element != nullptr) gst_object_unref(*element);
            *element = nullptr;
        }
        if (bus_ != nullptr) gst_object_unref(bus_);
        bus_ = nullptr;
        if (file_.isOpen()) file_.close();
    }

    const RecordingOptions options_;
    const QString path_;
    QFile file_;
    bool adts_ = false;
    bool pushed_ = false;
    bool finished_ = false;
    GstElement *pipeline_ = nullptr;
    GstElement *source_ = nullptr;
    GstElement *packets_ = nullptr;
    GstBus *bus_ = nullptr;
};

}  // namespace

// ---- the writer ----------------------------------------------------------------------------------

std::unique_ptr<RecordingWriter> RecordingWriter::open(const RecordingOptions &options, const QString &path,
                                                       QString &error) {
    if (path.isEmpty()) {
        error = QStringLiteral("there is nowhere to put the recording");
        return nullptr;
    }
    std::unique_ptr<Sink> sink;
    if (options.format == RecordingOptions::Format::Aac) {
        auto aac = std::make_unique<AacSink>(options, path);
        if (!aac->open(error)) {
            aac.reset();
            QFile::remove(path);
            return nullptr;
        }
        sink = std::move(aac);
    } else {
        auto samples = std::make_unique<SampleSink>(options, path, options.format == RecordingOptions::Format::Wav);
        if (!samples->open(error)) return nullptr;
        sink = std::move(samples);
    }
    return std::unique_ptr<RecordingWriter>(new RecordingWriter(options, path, std::move(sink)));
}

RecordingWriter::RecordingWriter(const RecordingOptions &options, QString path, std::unique_ptr<Sink> sink)
    : options_(options), path_(std::move(path)), sink_(std::move(sink)) {}

RecordingWriter::~RecordingWriter() = default;

RecordingWriter::Step RecordingWriter::append(const char *bytes, qint64 length, QString &error) {
    const qint64 frame = 2 * options_.channels;
    qint64 samples = qMin(length / frame, options_.sampleLimit() - recorded_);
    if (samples > 0) {
        QByteArray became;
        if (!sink_->write(bytes, samples * frame, recorded_, options_.frameBytes > 0 ? &became : nullptr, error)) {
            return {};
        }
        recorded_ += samples;
        cut(became);
    }
    Step step;
    step.frames = std::move(ready_);
    ready_.clear();
    step.full = recorded_ >= options_.sampleLimit();
    return step;
}

std::optional<RecordingWriter::Ending> RecordingWriter::finish(QString &error) {
    QByteArray rest;
    if (!sink_->finish(options_.frameBytes > 0 ? &rest : nullptr, error)) return std::nullopt;
    cut(rest);
    if (options_.frameBytes > 0) {
        // The last frame is what is left, which may be nothing: it is the one
        // that says the recording ended.
        ready_.push_back(pending_);
        pending_.clear();
    }
    Ending ending;
    ending.path = path_;
    // To the nearest millisecond: a duration that ends between two samples is
    // still that duration.
    const qint64 rate = options_.sampleRate;
    ending.durationMs = (recorded_ * 1000 + rate / 2) / rate;
    ending.fileSize = QFileInfo(path_).size();
    ending.frames = std::move(ready_);
    ready_.clear();
    return ending;
}

void RecordingWriter::discard() {
    QString ignored;
    (void)sink_->finish(nullptr, ignored);
    QFile::remove(path_);
}

void RecordingWriter::cut(const QByteArray &bytes) {
    const int size = options_.frameBytes;
    if (size <= 0 || bytes.isEmpty()) return;
    pending_.append(bytes);
    qsizetype offset = 0;
    while (pending_.size() - offset >= size) {
        ready_.push_back(pending_.mid(offset, size));
        offset += size;
    }
    if (offset > 0) pending_.remove(0, offset);
}

// ---- the recorder --------------------------------------------------------------------------------

/// Between GStreamer's threads and the recorder: they reach it through this,
/// which it cuts as it goes.
struct GStreamerRecorder::Alive {
    std::mutex mutex;
    GStreamerRecorder *recorder = nullptr;

    /// `what` done with the recorder on its thread, if it is there then; false
    /// when it has gone.
    template <typename What>
    bool later(What what) {
        const std::lock_guard<std::mutex> lock(mutex);
        GStreamerRecorder *target = recorder;
        if (target == nullptr) return false;
        QMetaObject::invokeMethod(
            target, [target, what = std::move(what)]() mutable { what(*target); }, Qt::QueuedConnection);
        return true;
    }
};

/// One recording: the input it records from, while it records, and its writer.
class GStreamerRecorder::Recording {
public:
    Recording(RecordingOptions options, std::unique_ptr<RecordingWriter> writer)
        : options_(options), writer_(std::move(writer)) {}

    /// Let go of on the recorder's thread, its input stopped first.
    ~Recording() {
        stopInput();
        if (writer_ != nullptr) writer_->discard();
    }

    Recording(const Recording &) = delete;
    Recording &operator=(const Recording &) = delete;

    [[nodiscard]] std::uint32_t id() const noexcept { return options_.recordingId; }

    /// The input of `source` converted to the recording's samples, listened
    /// to; false, and why in `error`, when there is none.
    bool listen(const QByteArray &source, const std::shared_ptr<Alive> &alive,
                const std::shared_ptr<Recording> &self, QString &error) {
        const QByteArray description = source + " ! queue ! audioconvert ! audioresample ! " +
                                       samplesCaps(options_) + " ! appsink name=samples sync=false";
        GError *failure = nullptr;
        input_ = gst_parse_launch(description.constData(), &failure);
        if (input_ == nullptr) {
            error = QStringLiteral("there is no microphone to record from: %1")
                        .arg(failure != nullptr ? QString::fromUtf8(failure->message) : QString());
            g_clear_error(&failure);
            return false;
        }
        g_clear_error(&failure);
        gst_object_ref_sink(input_);

        struct Watch {
            std::shared_ptr<Alive> alive;
            std::weak_ptr<Recording> recording;
            /// Whether what it still says is to be dropped: let go of, or failed.
            std::shared_ptr<bool> dropped;
            /// Whether GStreamer said something went wrong: read as the input starts.
            std::shared_ptr<std::atomic<bool>> errored;
            GstElement *input;
        };
        const auto release = [](gpointer data) { delete static_cast<Watch *>(data); };

        GstBus *bus = gst_element_get_bus(input_);
        gst_bus_set_sync_handler(
            bus,
            [](GstBus *, GstMessage *message, gpointer data) -> GstBusSyncReply {
                const auto &watch = *static_cast<Watch *>(data);
                std::optional<QString> failed;
                bool playing = false;
                if (GST_MESSAGE_TYPE(message) == GST_MESSAGE_ERROR) {
                    GError *error = nullptr;
                    gst_message_parse_error(message, &error, nullptr);
                    failed = error != nullptr && error->message != nullptr ? QString::fromUtf8(error->message)
                                                                           : QStringLiteral("no reason given");
                    g_clear_error(&error);
                } else if (GST_MESSAGE_TYPE(message) == GST_MESSAGE_STATE_CHANGED &&
                           GST_MESSAGE_SRC(message) == GST_OBJECT(watch.input)) {
                    GstState state = GST_STATE_NULL;
                    gst_message_parse_state_changed(message, nullptr, &state, nullptr);
                    playing = state == GST_STATE_PLAYING;
                }
                gst_message_unref(message);
                if (failed.has_value()) watch.errored->store(true);
                if (failed.has_value()) {
                    watch.alive->later([recording = watch.recording, reason = *failed](GStreamerRecorder &recorder) {
                        if (std::shared_ptr<Recording> failing = recording.lock()) {
                            recorder.fail(failing, QStringLiteral("the microphone could not record: %1").arg(reason));
                        }
                    });
                } else if (playing) {
                    watch.alive->later([recording = watch.recording](GStreamerRecorder &recorder) {
                        if (std::shared_ptr<Recording> listening = recording.lock()) recorder.began(listening);
                    });
                }
                return GST_BUS_DROP;
            },
            new Watch{alive, self, dropped_, errored_, input_}, release);
        gst_object_unref(bus);

        samples_ = gst_bin_get_by_name(GST_BIN(input_), "samples");
        GstAppSinkCallbacks callbacks{};
        callbacks.new_sample = [](GstAppSink *sink, gpointer data) -> GstFlowReturn {
            const auto &watch = *static_cast<Watch *>(data);
            GstSample *sample = gst_app_sink_pull_sample(sink);
            if (sample == nullptr) return GST_FLOW_OK;
            const std::shared_ptr<Recording> recording = watch.recording.lock();
            std::optional<RecordingWriter::Step> step;
            QString error;
            if (recording != nullptr) step = recording->write(sample, error);
            gst_sample_unref(sample);
            if (!step.has_value() || (step->frames.empty() && !step->full && error.isEmpty())) return GST_FLOW_OK;
            // Told even once the recording has ended -- these frames come
            // before its last -- unless it was let go of or failed.
            watch.alive->later([weak = watch.recording, dropped = watch.dropped, id = recording->id(),
                                taken = std::move(*step), error](GStreamerRecorder &recorder) {
                if (*dropped) return;
                if (!error.isEmpty()) {
                    if (std::shared_ptr<Recording> failing = weak.lock()) recorder.fail(failing, error);
                    return;
                }
                for (const QByteArray &frame : taken.frames) recorder.frame_(id, frame, false);
                if (std::shared_ptr<Recording> full = weak.lock(); full != nullptr && taken.full) recorder.finish(full);
            });
            return GST_FLOW_OK;
        };
        gst_app_sink_set_callbacks(GST_APP_SINK(samples_), &callbacks, new Watch{alive, self, dropped_, errored_, input_},
                                   release);

        // A refusal says why on the bus, and is reported from there; one that
        // did not even say is reported here.
        if (gst_element_set_state(input_, GST_STATE_PLAYING) == GST_STATE_CHANGE_FAILURE && !errored_->load()) {
            error = QStringLiteral("there is no microphone to record from");
            return false;
        }
        return true;
    }

    /// Samples no more: once this returns, none is written.
    void stopInput() {
        if (input_ != nullptr) gst_element_set_state(input_, GST_STATE_NULL);
        if (samples_ != nullptr) gst_object_unref(samples_);
        if (input_ != nullptr) gst_object_unref(input_);
        samples_ = nullptr;
        input_ = nullptr;
    }

    void setPaused(bool paused) {
        if (input_ != nullptr) gst_element_set_state(input_, paused ? GST_STATE_PAUSED : GST_STATE_PLAYING);
        paused_ = paused;
    }

    /// The writer, the input stopped: no thread but its taker's writes to it.
    std::unique_ptr<RecordingWriter> takeWriter() {
        stopInput();
        return std::move(writer_);
    }

    /// What it still says is not told: let go of, or failed. The recorder's thread's.
    void drop() { *dropped_ = true; }

    // The recorder's thread's.
    bool started_ = false;
    bool paused_ = false;
    /// Finished, failed or let go of.
    bool ended_ = false;

private:
    /// One run of samples written, on the input's thread.
    std::optional<RecordingWriter::Step> write(GstSample *sample, QString &error) {
        GstBuffer *buffer = gst_sample_get_buffer(sample);
        GstMapInfo map;
        if (buffer == nullptr || !gst_buffer_map(buffer, &map, GST_MAP_READ)) return std::nullopt;
        const std::lock_guard<std::mutex> lock(mutex_);
        std::optional<RecordingWriter::Step> step;
        if (writer_ != nullptr && !full_) {
            step = writer_->append(reinterpret_cast<const char *>(map.data), static_cast<qint64>(map.size), error);
            full_ = step->full;
        }
        gst_buffer_unmap(buffer, &map);
        return step;
    }

    const RecordingOptions options_;
    /// Guards the writer while the input writes to it.
    std::mutex mutex_;
    std::unique_ptr<RecordingWriter> writer_;
    /// It holds its whole duration: the input's thread writes no more.
    bool full_ = false;
    GstElement *input_ = nullptr;
    GstElement *samples_ = nullptr;
    const std::shared_ptr<bool> dropped_ = std::make_shared<bool>(false);
    const std::shared_ptr<std::atomic<bool>> errored_ = std::make_shared<std::atomic<bool>>(false);
};

GStreamerRecorder::GStreamerRecorder(Post post, PostFrame frame, QString staging, QByteArray source)
    : post_(std::move(post)),
      frame_(std::move(frame)),
      stagingRoot_(std::move(staging)),
      source_(std::move(source)),
      alive_(std::make_shared<Alive>()) {
    alive_->recorder = this;
}

GStreamerRecorder::~GStreamerRecorder() {
    {
        const std::lock_guard<std::mutex> lock(alive_->mutex);
        alive_->recorder = nullptr;
    }
    // The game ended: the recording under way is nobody's.
    if (capture_ != nullptr) cancel(capture_);
}

void GStreamerRecorder::call(std::uint32_t method, const QJsonObject &request) {
    const std::uint32_t id = RecordingOptions::recordingIdOf(request);
    switch (method) {
        case MIGO_RECORDER_START:
            start(request);
            return;
        case MIGO_RECORDER_PAUSE:
            if (Recording *recording = current(id); recording != nullptr && recording->started_ && !recording->paused_) {
                recording->setPaused(true);
                tell(MIGO_RECORDER_EVENT_PAUSE, id);
            }
            return;
        case MIGO_RECORDER_RESUME:
            if (Recording *recording = current(id); recording != nullptr && recording->paused_) {
                recording->setPaused(false);
                tell(MIGO_RECORDER_EVENT_RESUME, id);
            }
            return;
        case MIGO_RECORDER_STOP:
            if (current(id) != nullptr) {
                // One that has not started records nothing to hand over.
                if (capture_->started_) {
                    finish(capture_);
                } else {
                    cancel(capture_);
                }
            }
            return;
        default:
            return;
    }
}

void GStreamerRecorder::start(const QJsonObject &request) {
    // A recording under way stops first, with no STOP: the new one takes its place.
    if (capture_ != nullptr) cancel(capture_);
    const std::uint32_t id = RecordingOptions::recordingIdOf(request);
    const std::optional<RecordingOptions> options = RecordingOptions::parse(request);
    if (!options.has_value()) {
        tell(MIGO_RECORDER_EVENT_ERROR, id, {{QStringLiteral("errMsg"), errMsg(QStringLiteral("invalid audio parameters"))}});
        return;
    }
    if (staging_ == nullptr) staging_ = std::make_shared<Staging>(stagingRoot_);
    QString error;
    std::unique_ptr<RecordingWriter> writer = RecordingWriter::open(*options, staging_->path(options->suffix()), error);
    if (writer == nullptr) {
        tell(MIGO_RECORDER_EVENT_ERROR, id, {{QStringLiteral("errMsg"), errMsg(error)}});
        return;
    }
    auto recording = std::make_shared<Recording>(*options, std::move(writer));
    if (!recording->listen(source_, alive_, recording, error)) {
        recording->ended_ = true;
        tell(MIGO_RECORDER_EVENT_ERROR, id, {{QStringLiteral("errMsg"), errMsg(error)}});
        return;
    }
    capture_ = std::move(recording);
}

GStreamerRecorder::Recording *GStreamerRecorder::current(std::uint32_t id) const {
    return capture_ != nullptr && capture_->id() == id ? capture_.get() : nullptr;
}

void GStreamerRecorder::began(std::shared_ptr<Recording> recording) {
    if (recording != capture_ || recording->ended_ || recording->started_) return;
    recording->started_ = true;
    tell(MIGO_RECORDER_EVENT_START, recording->id());
}

void GStreamerRecorder::finish(std::shared_ptr<Recording> recording) {
    if (recording->ended_) return;
    recording->ended_ = true;
    if (capture_ == recording) capture_.reset();
    // Written off this thread: an encoder's last packets and a file's index.
    std::shared_ptr<RecordingWriter> writer = recording->takeWriter();
    if (writer == nullptr) return;
    const std::uint32_t id = recording->id();
    QThreadPool::globalInstance()->start([writer, id, alive = alive_] {
        QString error;
        std::optional<RecordingWriter::Ending> ending = writer->finish(error);
        const bool told = alive->later([id, ending, error](GStreamerRecorder &recorder) {
            if (!ending.has_value()) {
                recorder.tell(MIGO_RECORDER_EVENT_ERROR, id, {{QStringLiteral("errMsg"), errMsg(error)}});
                return;
            }
            for (std::size_t index = 0; index < ending->frames.size(); ++index) {
                recorder.frame_(id, ending->frames[index], index + 1 == ending->frames.size());
            }
            recorder.tell(MIGO_RECORDER_EVENT_STOP, id,
                          {{QStringLiteral("tempFilePath"), ending->path},
                           {QStringLiteral("duration"), ending->durationMs},
                           {QStringLiteral("fileSize"), ending->fileSize}});
            // The Session took the file as the event was posted; one it did
            // not take -- the game that asked has gone -- goes.
            QFile::remove(ending->path);
        });
        if (!told || !ending.has_value()) writer->discard();
    });
}

void GStreamerRecorder::fail(std::shared_ptr<Recording> recording, const QString &reason) {
    if (recording->ended_) return;
    recording->ended_ = true;
    recording->drop();
    if (capture_ == recording) capture_.reset();
    if (std::unique_ptr<RecordingWriter> writer = recording->takeWriter()) writer->discard();
    tell(MIGO_RECORDER_EVENT_ERROR, recording->id(), {{QStringLiteral("errMsg"), errMsg(reason)}});
}

void GStreamerRecorder::cancel(std::shared_ptr<Recording> recording) {
    recording->ended_ = true;
    recording->drop();
    if (capture_ == recording) capture_.reset();
    if (std::unique_ptr<RecordingWriter> writer = recording->takeWriter()) writer->discard();
}

void GStreamerRecorder::tell(std::uint32_t event, std::uint32_t id, QJsonObject payload) {
    payload.insert(QStringLiteral("recordingId"), static_cast<qint64>(id));
    post_(event, payload);
}

}  // namespace migo::linux_host::qt6

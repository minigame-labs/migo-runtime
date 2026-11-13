#ifndef MIGO_LINUX_QT6_MEDIA_RECORDER_HPP_
#define MIGO_LINUX_QT6_MEDIA_RECORDER_HPP_

// Internal to the media component (migo::qt6-media), not installed.

#include <migo/linux/qt6/host_services.hpp>

#include <QByteArray>
#include <QJsonObject>
#include <QObject>
#include <QString>

#include <cstdint>
#include <memory>
#include <optional>
#include <vector>

namespace migo::linux_host::qt6 {

class Staging;

/// What content asked of one recording: `RecorderManager.start`'s options as
/// the recorder service's START carries them, an option left out taking the
/// reference's default.
struct RecordingOptions {
    enum class Format {
        /// 16-bit samples, raw.
        Pcm,
        /// 16-bit samples behind a WAV header.
        Wav,
        /// AAC, which "mp3" records too: as on Android and the Apple
        /// platforms, the system's encoders encode no MP3.
        Aac,
    };

    /// The longest recording the reference allows: ten minutes.
    static constexpr int kLongestMs = 600'000;

    std::uint32_t recordingId = 0;
    int durationMs = 60'000;
    int sampleRate = 8000;
    int channels = 2;
    int bitRate = 48'000;
    Format format = Format::Aac;
    /// Bytes to a frame handed to onFrameRecorded; 0 when content asked for none.
    int frameBytes = 0;

    /// The options `request` names; none when they are not ones the reference
    /// allows ("invalid audio parameters").
    [[nodiscard]] static std::optional<RecordingOptions> parse(const QJsonObject &request);
    /// The recording a request names; 0 when it names none, which no recording is.
    [[nodiscard]] static std::uint32_t recordingIdOf(const QJsonObject &request);

    /// The most the recording holds, in samples of each channel: its duration,
    /// to the sample.
    [[nodiscard]] qint64 sampleLimit() const noexcept {
        return static_cast<qint64>(durationMs) * sampleRate / 1000;
    }
    /// The file's suffix: what it is.
    [[nodiscard]] QString suffix() const;
};

/// One recording being written: 16-bit samples at its own rate and channels,
/// as the microphone hands them over, into the file and -- when content asked
/// for frames -- the bytes the frames are cut from. It counts what it recorded
/// in samples, so a recording ends on the sample its duration ends on,
/// whatever sizes arrive. One thread at a time.
///
/// What it writes, by format, as the Apple SDK and the Android SDK write it:
///   PCM  the samples, raw; frames are the same bytes.
///   wav  the samples behind a WAV header; frames are the samples.
///   aac  AAC: in an MPEG-4 file (.m4a) when content asked for no frames, and
///        as an ADTS stream (.aac) when it did -- a stream is what can be handed
///        over as it is recorded, and frames are its bytes. GStreamer's AAC
///        encoder encodes it: libav's, else FDK's, VisualOn's or FAAC's.
class RecordingWriter {
public:
    /// What one run of samples came to.
    struct Step {
        /// Whole frames to hand content now.
        std::vector<QByteArray> frames;
        /// The recording holds its whole duration: it is to be finished.
        bool full = false;
    };

    /// The finished recording.
    struct Ending {
        QString path;
        qint64 durationMs = 0;
        qint64 fileSize = 0;
        /// The frames still to hand content, the last of them the recording's
        /// last; empty when it asked for none.
        std::vector<QByteArray> frames;
    };

    class Sink;

    /// A recording of `options` into the new file `path`; none, and why in
    /// `error`, when it cannot be made.
    [[nodiscard]] static std::unique_ptr<RecordingWriter> open(const RecordingOptions &options, const QString &path,
                                                               QString &error);
    ~RecordingWriter();

    RecordingWriter(const RecordingWriter &) = delete;
    RecordingWriter &operator=(const RecordingWriter &) = delete;

    /// `bytes` of interleaved samples, as far as the duration has room for
    /// them; empty with `error` set when they could not be written.
    [[nodiscard]] Step append(const char *bytes, qint64 length, QString &error);
    /// The recording ends: the encoder's last written, the file closed. None,
    /// and why in `error`, when it could not be.
    [[nodiscard]] std::optional<Ending> finish(QString &error);
    /// The recording is not wanted: its file goes.
    void discard();

    [[nodiscard]] const QString &path() const noexcept { return path_; }

private:
    RecordingWriter(const RecordingOptions &options, QString path, std::unique_ptr<Sink> sink);
    /// `bytes` joined to what was left, every whole frame of it cut off.
    void cut(const QByteArray &bytes);

    const RecordingOptions options_;
    const QString path_;
    std::unique_ptr<Sink> sink_;
    qint64 recorded_ = 0;
    QByteArray pending_;
    std::vector<QByteArray> ready_;
};

/// getRecorderManager's recorder, through GStreamer: the desktop's default
/// input -- PipeWire's, PulseAudio's or ALSA's, as GStreamer finds it --
/// converted to what content asked for (`MigoQtMedia::recorder`).
///
/// START is reported once the input records -- or ERROR, why it cannot -- PAUSE
/// and RESUME once done, STOP once the file is written, its frames before it:
/// a recording's frames come as they are recorded, on the GUI thread, in order.
/// A desktop has no interruptions to report, and one input: audioSource names
/// none of its own.
class GStreamerRecorder final : public QObject, public MigoQtRecorder {
public:
    /// `source` is where the sound comes from, as a GStreamer description:
    /// the desktop's default input, or a source of a test's own.
    GStreamerRecorder(Post post, PostFrame frame, QString staging, QByteArray source = "autoaudiosrc");
    ~GStreamerRecorder() override;

    GStreamerRecorder(const GStreamerRecorder &) = delete;
    GStreamerRecorder &operator=(const GStreamerRecorder &) = delete;

    void call(std::uint32_t method, const QJsonObject &request) override;

    class Recording;

private:
    struct Alive;
    void start(const QJsonObject &request);
    /// The recording under way, if it is `id`'s.
    [[nodiscard]] Recording *current(std::uint32_t id) const;
    /// The recording records: content is told.
    void began(std::shared_ptr<Recording> recording);
    /// The recording ends with its file, off this thread; content is told
    /// once it is written.
    void finish(std::shared_ptr<Recording> recording);
    /// The recording ends with an error, and no file.
    void fail(std::shared_ptr<Recording> recording, const QString &reason);
    /// The recording goes with nothing said of it.
    void cancel(std::shared_ptr<Recording> recording);
    void tell(std::uint32_t event, std::uint32_t id, QJsonObject payload = {});

    const Post post_;
    const PostFrame frame_;
    const QString stagingRoot_;
    const QByteArray source_;
    const std::shared_ptr<Alive> alive_;
    /// Made the first time content records.
    std::shared_ptr<Staging> staging_;
    std::shared_ptr<Recording> capture_;
};

}  // namespace migo::linux_host::qt6

#endif  // MIGO_LINUX_QT6_MEDIA_RECORDER_HPP_

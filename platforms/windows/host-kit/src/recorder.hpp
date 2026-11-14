#ifndef MIGO_WINDOWS_HOST_KIT_RECORDER_HPP_
#define MIGO_WINDOWS_HOST_KIT_RECORDER_HPP_

// Internal to the Host Kit, not installed: getRecorderManager's microphone.

#include <migo/windows/host_services.hpp>

#include "gui_queue.hpp"

#include <cstdint>
#include <functional>
#include <memory>
#include <optional>
#include <string>
#include <vector>

namespace migo::windows_host {

class Staging;

/// What content asked of one recording: `RecorderManager.start`'s options as
/// the recorder service's START carries them, an option left out taking the
/// reference's default -- the Linux Host Kit's, the same rules.
struct RecordingOptions {
    enum class Format {
        /// 16-bit samples, raw.
        Pcm,
        /// 16-bit samples behind a WAV header.
        Wav,
        /// AAC, which "mp3" records too: as on Android, the Apple platforms and
        /// Linux, the system's encoders encode no MP3.
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
    [[nodiscard]] static std::optional<RecordingOptions> parse(const Json &request);
    /// The recording a request names; 0 when it names none, which no recording is.
    [[nodiscard]] static std::uint32_t recordingIdOf(const Json &request);

    /// The rate the recording is made at: the one asked for -- but AAC at one
    /// Windows' AAC encoder takes, 44.1 or 48 kHz, the nearer to what was asked.
    [[nodiscard]] int recordedRate() const noexcept;
    /// The bit rate AAC is encoded at: the one of Windows' encoder's four
    /// (96, 128, 160 or 192 kbit/s) nearest to the one asked for.
    [[nodiscard]] int aacBitRate() const noexcept;
    /// The file's suffix: pcm, wav, m4a -- or aac, an ADTS stream, when
    /// content asked for frames, which a stream can be cut into as it is recorded.
    [[nodiscard]] const wchar_t *suffix() const noexcept;
};

/// Where a recording's sound comes from: the microphone, or a test's own.
/// Every call is on the recording's thread.
class AudioSource {
public:
    virtual ~AudioSource() = default;
    /// Opens the input, its samples 16-bit at `rate` and `channels`, interleaved.
    virtual bool open(int rate, int channels, std::string &error) = 0;
    virtual bool start(std::string &error) = 0;
    virtual void stop() = 0;
    /// An event set when samples wait to be read.
    [[nodiscard]] virtual void *ready() = 0;
    /// Appends the samples that wait to `samples`.
    virtual bool read(std::vector<std::int16_t> &samples, std::string &error) = 0;
};

using AudioSourceFactory = std::function<std::unique_ptr<AudioSource>()>;

/// The system's default input -- the microphone the player chose -- through
/// WASAPI in shared mode, converted by the system to what the recording asks.
[[nodiscard]] std::unique_ptr<AudioSource> systemMicrophone();

/// getRecorderManager's recorder -- the Linux Host Kit's `GStreamerRecorder`,
/// for Win32 -- one recording at a time, each on a thread of its own:
///
///   PCM  the samples, raw; frames are the same bytes.
///   wav  the samples behind a WAV header; frames are the samples.
///   aac  AAC through Media Foundation: in an MPEG-4 file (.m4a) when content
///        asked for no frames, and as an ADTS stream (.aac) when it did --
///        frames are its bytes.
///
/// START is reported once the input records -- or ERROR, why it cannot --
/// PAUSE and RESUME once done, STOP once the file is written, its frames before
/// it, in order. A desktop has one input: audioSource names none of its own.
/// Events and frames go through the Session's channel on the GUI thread, in the
/// order the recording made them. Destroyed, it ends the recording under way
/// with nothing said of it. GUI thread only.
///
/// Whether the App may record is the player's privacy settings' to say: an
/// input they keep from desktop Apps fails START ("the microphone is not
/// allowed").
class Recorder final {
public:
    Recorder(std::shared_ptr<HostServiceChannel> channel, std::wstring staging_root,
             AudioSourceFactory source = systemMicrophone);
    ~Recorder();

    Recorder(const Recorder &) = delete;
    Recorder &operator=(const Recorder &) = delete;

    /// One of MIGO_RECORDER_*: every one a command.
    void call(std::uint32_t method, const Json &request);

    class Recording;

private:
    const std::shared_ptr<HostServiceChannel> channel_;
    const std::wstring staging_root_;
    const AudioSourceFactory source_;
    std::shared_ptr<Staging> staging_;
    // Before the recording, so it outlives it: what the recording still posts
    // as it goes is dropped with the queue.
    GuiQueue queue_;
    std::unique_ptr<Recording> recording_;
};

}  // namespace migo::windows_host

#endif  // MIGO_WINDOWS_HOST_KIT_RECORDER_HPP_

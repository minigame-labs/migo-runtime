#include "recorder.hpp"

#include "gui_queue.hpp"
#include "staging.hpp"
#include "win32_text.hpp"

#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#ifndef NOMINMAX
#define NOMINMAX
#endif
#ifndef UNICODE
#define UNICODE
#endif
#include <windows.h>
#include <audioclient.h>
#include <mfapi.h>
#include <mferror.h>
#include <mfidl.h>
#include <mfreadwrite.h>
#include <mftransform.h>
#include <mmdeviceapi.h>
#include <wrl/client.h>

#include <algorithm>
#include <array>
#include <atomic>
#include <cmath>
#include <cstring>
#include <deque>
#include <limits>
#include <mutex>
#include <thread>
#include <utility>

namespace migo::windows_host {

namespace {

using Microsoft::WRL::ComPtr;

constexpr std::array<int, 9> kSampleRates{8000, 11025, 12000, 16000, 22050, 24000, 32000, 44100, 48000};
// Windows' AAC encoder's bit rates, in bits a second.
constexpr std::array<int, 4> kAacBitRates{96'000, 128'000, 160'000, 192'000};
// How long the input's buffer holds: what a wake-up may find waiting.
constexpr REFERENCE_TIME kInputBuffer = 2'000'000;  // 200 ms, in 100 ns
// How often the recording looks at its commands when no sound comes.
constexpr DWORD kIdleMs = 100;
constexpr char kInvalid[] = "invalid audio parameters";

/// A COM apartment for the calling thread, for as long as it lives.
class Apartment final {
public:
    Apartment() : joined_(SUCCEEDED(CoInitializeEx(nullptr, COINIT_MULTITHREADED))) {}
    ~Apartment() {
        if (joined_) CoUninitialize();
    }
    Apartment(const Apartment &) = delete;
    Apartment &operator=(const Apartment &) = delete;

private:
    const bool joined_;
};

/// Media Foundation, started for as long as it lives.
class MediaFoundation final {
public:
    MediaFoundation() : started_(SUCCEEDED(MFStartup(MF_VERSION, MFSTARTUP_LITE))) {}
    ~MediaFoundation() {
        if (started_) MFShutdown();
    }
    MediaFoundation(const MediaFoundation &) = delete;
    MediaFoundation &operator=(const MediaFoundation &) = delete;
    [[nodiscard]] bool started() const noexcept { return started_; }

private:
    const bool started_;
};

/// An integer option, or `fallback` when the request leaves it out; none of
/// an int's range is refused here, which the ranges below do.
std::optional<int> number(const Json &request, const char *key, int fallback) {
    const Json &value = request[key];
    if (value.isNull()) return fallback;
    const double asked = value.numberOr(std::nan(""));
    if (!std::isfinite(asked) || asked < std::numeric_limits<int>::min() || asked > std::numeric_limits<int>::max()) {
        return std::nullopt;
    }
    return static_cast<int>(asked);
}

std::string wav_header(int rate, int channels, std::uint64_t data_bytes) {
    std::string header;
    header.reserve(44);
    const auto u32 = [&header](std::uint32_t value) {
        for (int byte = 0; byte < 4; ++byte) header.push_back(static_cast<char>((value >> (8 * byte)) & 0xFF));
    };
    const auto u16 = [&header](std::uint16_t value) {
        header.push_back(static_cast<char>(value & 0xFF));
        header.push_back(static_cast<char>(value >> 8));
    };
    const auto data = static_cast<std::uint32_t>(std::min<std::uint64_t>(data_bytes, 0xFFFFFFFFULL - 36));
    header.append("RIFF", 4);
    u32(36 + data);
    header.append("WAVEfmt ", 8);
    u32(16);
    u16(1);
    u16(static_cast<std::uint16_t>(channels));
    u32(static_cast<std::uint32_t>(rate));
    u32(static_cast<std::uint32_t>(rate * channels * 2));
    u16(static_cast<std::uint16_t>(channels * 2));
    u16(16);
    header.append("data", 4);
    u32(data);
    return header;
}

// ---- the system's microphone ---------------------------------------------------------------------

class Microphone final : public AudioSource {
public:
    Microphone() : event_(CreateEventW(nullptr, FALSE, FALSE, nullptr)) {}
    ~Microphone() override {
        if (client_ != nullptr) client_->Stop();
        if (event_ != nullptr) CloseHandle(event_);
    }

    bool open(int rate, int channels, std::string &error) override {
        ComPtr<IMMDeviceEnumerator> devices;
        ComPtr<IMMDevice> input;
        if (FAILED(CoCreateInstance(__uuidof(MMDeviceEnumerator), nullptr, CLSCTX_ALL,
                                    IID_PPV_ARGS(devices.GetAddressOf()))) ||
            FAILED(devices->GetDefaultAudioEndpoint(eCapture, eConsole, input.GetAddressOf()))) {
            error = "there is no microphone";
            return false;
        }
        HRESULT result = input->Activate(__uuidof(IAudioClient), CLSCTX_ALL, nullptr,
                                         reinterpret_cast<void **>(client_.GetAddressOf()));
        // What the recording asks for, the system converting what the device gives.
        WAVEFORMATEX format{};
        format.wFormatTag = WAVE_FORMAT_PCM;
        format.nChannels = static_cast<WORD>(channels);
        format.nSamplesPerSec = static_cast<DWORD>(rate);
        format.wBitsPerSample = 16;
        format.nBlockAlign = static_cast<WORD>(channels * 2);
        format.nAvgBytesPerSec = format.nSamplesPerSec * format.nBlockAlign;
        if (SUCCEEDED(result)) {
            result = client_->Initialize(AUDCLNT_SHAREMODE_SHARED,
                                         AUDCLNT_STREAMFLAGS_EVENTCALLBACK | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM |
                                             AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
                                         kInputBuffer, 0, &format, nullptr);
        }
        if (SUCCEEDED(result)) result = client_->SetEventHandle(event_);
        if (SUCCEEDED(result)) result = client_->GetService(IID_PPV_ARGS(capture_.GetAddressOf()));
        if (FAILED(result)) {
            // The privacy settings' answer, or a device that went.
            error = result == E_ACCESSDENIED ? "the microphone is not allowed" : "the microphone cannot be opened";
            client_.Reset();
            return false;
        }
        channels_ = channels;
        return true;
    }

    bool start(std::string &error) override {
        if (FAILED(client_->Start())) {
            error = "the microphone cannot be started";
            return false;
        }
        return true;
    }

    void stop() override { client_->Stop(); }

    void *ready() override { return event_; }

    bool read(std::vector<std::int16_t> &samples, std::string &error) override {
        UINT32 waiting = 0;
        while (SUCCEEDED(capture_->GetNextPacketSize(&waiting)) && waiting > 0) {
            BYTE *data = nullptr;
            UINT32 frames = 0;
            DWORD flags = 0;
            if (FAILED(capture_->GetBuffer(&data, &frames, &flags, nullptr, nullptr))) {
                error = "the microphone stopped";
                return false;
            }
            const std::size_t count = static_cast<std::size_t>(frames) * static_cast<std::size_t>(channels_);
            if ((flags & AUDCLNT_BUFFERFLAGS_SILENT) != 0 || data == nullptr) {
                samples.insert(samples.end(), count, 0);
            } else {
                const auto *pcm = reinterpret_cast<const std::int16_t *>(data);
                samples.insert(samples.end(), pcm, pcm + count);
            }
            capture_->ReleaseBuffer(frames);
        }
        return true;
    }

private:
    HANDLE event_ = nullptr;
    ComPtr<IAudioClient> client_;
    ComPtr<IAudioCaptureClient> capture_;
    int channels_ = 1;
};

// ---- what the samples become ---------------------------------------------------------------------

/// Where a recording's samples go. `out`, when given, receives the bytes they
/// became, for the frames.
class Writer {
public:
    virtual ~Writer() = default;
    virtual bool write(const std::int16_t *samples, std::size_t count, std::int64_t first_sample, std::string *out,
                       std::string &error) = 0;
    /// The file finished and closed; harmless twice.
    virtual bool finish(std::string *out, std::string &error) = 0;
};

/// A file of the Host Kit's own, written through.
class File final {
public:
    explicit File(const std::wstring &path)
        : handle_(CreateFileW(path.c_str(), GENERIC_WRITE, FILE_SHARE_READ, nullptr, CREATE_NEW,
                              FILE_ATTRIBUTE_NORMAL, nullptr)) {}
    ~File() { close(); }
    File(const File &) = delete;
    File &operator=(const File &) = delete;

    [[nodiscard]] bool isOpen() const noexcept { return handle_ != INVALID_HANDLE_VALUE; }
    bool write(const void *bytes, std::size_t length) {
        DWORD written = 0;
        return length <= 0xFFFFFFFFULL && WriteFile(handle_, bytes, static_cast<DWORD>(length), &written, nullptr) &&
               written == length;
    }
    bool writeAt(std::uint64_t offset, const std::string &bytes) {
        LARGE_INTEGER at{};
        at.QuadPart = static_cast<LONGLONG>(offset);
        LARGE_INTEGER end{};
        LARGE_INTEGER here{};
        return SetFilePointerEx(handle_, LARGE_INTEGER{}, &end, FILE_END) &&
               SetFilePointerEx(handle_, at, &here, FILE_BEGIN) && write(bytes.data(), bytes.size()) &&
               SetFilePointerEx(handle_, end, nullptr, FILE_BEGIN);
    }
    void close() {
        if (handle_ != INVALID_HANDLE_VALUE) CloseHandle(handle_);
        handle_ = INVALID_HANDLE_VALUE;
    }

private:
    HANDLE handle_;
};

/// The samples themselves, raw or behind a WAV header.
class SampleWriter final : public Writer {
public:
    SampleWriter(const RecordingOptions &options, const std::wstring &path, bool wav)
        : file_(path), rate_(options.recordedRate()), channels_(options.channels), wav_(wav) {}

    bool open(std::string &error) {
        if (!file_.isOpen() || (wav_ && !file_.write(wav_header(rate_, channels_, 0).data(), 44))) {
            error = "there is no file to record into";
            return false;
        }
        return true;
    }

    bool write(const std::int16_t *samples, std::size_t count, std::int64_t, std::string *out,
               std::string &error) override {
        const std::size_t bytes = count * sizeof(std::int16_t);
        if (!file_.write(samples, bytes)) {
            error = "the recording could not be written";
            return false;
        }
        written_ += bytes;
        if (out != nullptr) out->append(reinterpret_cast<const char *>(samples), bytes);
        return true;
    }

    bool finish(std::string *, std::string &error) override {
        if (finished_) return true;
        finished_ = true;
        // The header says how much the file holds, once it is known.
        if (wav_ && !file_.writeAt(0, wav_header(rate_, channels_, written_))) {
            error = "the recording could not be written";
            file_.close();
            return false;
        }
        file_.close();
        return true;
    }

private:
    File file_;
    const int rate_;
    const int channels_;
    const bool wav_;
    std::uint64_t written_ = 0;
    bool finished_ = false;
};

ComPtr<IMFMediaType> pcm_type(int rate, int channels) {
    ComPtr<IMFMediaType> type;
    if (FAILED(MFCreateMediaType(type.GetAddressOf()))) return nullptr;
    type->SetGUID(MF_MT_MAJOR_TYPE, MFMediaType_Audio);
    type->SetGUID(MF_MT_SUBTYPE, MFAudioFormat_PCM);
    type->SetUINT32(MF_MT_AUDIO_BITS_PER_SAMPLE, 16);
    type->SetUINT32(MF_MT_AUDIO_SAMPLES_PER_SECOND, static_cast<UINT32>(rate));
    type->SetUINT32(MF_MT_AUDIO_NUM_CHANNELS, static_cast<UINT32>(channels));
    type->SetUINT32(MF_MT_AUDIO_BLOCK_ALIGNMENT, static_cast<UINT32>(channels * 2));
    type->SetUINT32(MF_MT_AUDIO_AVG_BYTES_PER_SECOND, static_cast<UINT32>(rate * channels * 2));
    return type;
}

ComPtr<IMFMediaType> aac_type(const RecordingOptions &options, bool adts) {
    ComPtr<IMFMediaType> type;
    if (FAILED(MFCreateMediaType(type.GetAddressOf()))) return nullptr;
    type->SetGUID(MF_MT_MAJOR_TYPE, MFMediaType_Audio);
    type->SetGUID(MF_MT_SUBTYPE, MFAudioFormat_AAC);
    type->SetUINT32(MF_MT_AUDIO_BITS_PER_SAMPLE, 16);
    type->SetUINT32(MF_MT_AUDIO_SAMPLES_PER_SECOND, static_cast<UINT32>(options.recordedRate()));
    type->SetUINT32(MF_MT_AUDIO_NUM_CHANNELS, static_cast<UINT32>(options.channels));
    type->SetUINT32(MF_MT_AUDIO_AVG_BYTES_PER_SECOND, static_cast<UINT32>(options.aacBitRate() / 8));
    if (adts) type->SetUINT32(MF_MT_AAC_PAYLOAD_TYPE, 1);
    return type;
}

/// `count` samples as a Media Foundation sample, timed from `first_sample`.
ComPtr<IMFSample> media_sample(const std::int16_t *samples, std::size_t count, std::int64_t first_sample, int rate,
                               int channels) {
    const auto bytes = static_cast<DWORD>(count * sizeof(std::int16_t));
    ComPtr<IMFMediaBuffer> buffer;
    ComPtr<IMFSample> sample;
    BYTE *target = nullptr;
    if (FAILED(MFCreateMemoryBuffer(bytes, buffer.GetAddressOf())) || FAILED(buffer->Lock(&target, nullptr, nullptr))) {
        return nullptr;
    }
    std::memcpy(target, samples, bytes);
    buffer->Unlock();
    buffer->SetCurrentLength(bytes);
    if (FAILED(MFCreateSample(sample.GetAddressOf())) || FAILED(sample->AddBuffer(buffer.Get()))) return nullptr;
    const auto frames = static_cast<LONGLONG>(count / static_cast<std::size_t>(channels));
    sample->SetSampleTime(first_sample * 10'000'000LL / rate);
    sample->SetSampleDuration(frames * 10'000'000LL / rate);
    return sample;
}

/// AAC in an MPEG-4 file, through Media Foundation's sink writer.
class AacFileWriter final : public Writer {
public:
    AacFileWriter(const RecordingOptions &options, std::wstring path) : options_(options), path_(std::move(path)) {}

    bool open(std::string &error) {
        ComPtr<IMFAttributes> attributes;
        ComPtr<IMFMediaType> output = aac_type(options_, false);
        ComPtr<IMFMediaType> input = pcm_type(options_.recordedRate(), options_.channels);
        HRESULT result = media_.started() ? MFCreateAttributes(attributes.GetAddressOf(), 1) : E_FAIL;
        if (SUCCEEDED(result)) result = attributes->SetGUID(MF_TRANSCODE_CONTAINERTYPE, MFTranscodeContainerType_MPEG4);
        if (SUCCEEDED(result)) {
            result = MFCreateSinkWriterFromURL(path_.c_str(), nullptr, attributes.Get(), writer_.GetAddressOf());
        }
        if (SUCCEEDED(result)) result = output != nullptr && input != nullptr ? S_OK : E_OUTOFMEMORY;
        if (SUCCEEDED(result)) result = writer_->AddStream(output.Get(), &stream_);
        if (SUCCEEDED(result)) result = writer_->SetInputMediaType(stream_, input.Get(), nullptr);
        if (SUCCEEDED(result)) result = writer_->BeginWriting();
        if (FAILED(result)) {
            error = "the system's AAC encoder cannot record this";
            writer_.Reset();
            return false;
        }
        return true;
    }

    bool write(const std::int16_t *samples, std::size_t count, std::int64_t first_sample, std::string *,
               std::string &error) override {
        ComPtr<IMFSample> sample =
            media_sample(samples, count, first_sample, options_.recordedRate(), options_.channels);
        if (sample == nullptr || FAILED(writer_->WriteSample(stream_, sample.Get()))) {
            error = "the recording could not be written";
            return false;
        }
        return true;
    }

    bool finish(std::string *, std::string &error) override {
        if (writer_ == nullptr) return true;
        const HRESULT result = writer_->Finalize();
        writer_.Reset();
        if (FAILED(result)) {
            error = "the recording could not be written";
            return false;
        }
        return true;
    }

private:
    const RecordingOptions options_;
    const std::wstring path_;
    MediaFoundation media_;
    ComPtr<IMFSinkWriter> writer_;
    DWORD stream_ = 0;
};

/// AAC as an ADTS stream: the system's AAC encoder driven directly, each frame
/// it gives written to the file and to the bytes content's frames are cut from.
class AacStreamWriter final : public Writer {
public:
    AacStreamWriter(const RecordingOptions &options, const std::wstring &path) : options_(options), file_(path) {}

    bool open(std::string &error) {
        if (!file_.isOpen()) {
            error = "there is no file to record into";
            return false;
        }
        MFT_REGISTER_TYPE_INFO produces{MFMediaType_Audio, MFAudioFormat_AAC};
        IMFActivate **found = nullptr;
        UINT32 count = 0;
        HRESULT result = media_.started() ? MFTEnumEx(MFT_CATEGORY_AUDIO_ENCODER,
                                                      MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_LOCALMFT |
                                                          MFT_ENUM_FLAG_SORTANDFILTER,
                                                      nullptr, &produces, &found, &count)
                                          : E_FAIL;
        if (SUCCEEDED(result) && count > 0) result = found[0]->ActivateObject(IID_PPV_ARGS(encoder_.GetAddressOf()));
        for (UINT32 index = 0; index < count; ++index) found[index]->Release();
        CoTaskMemFree(found);
        if (SUCCEEDED(result) && count == 0) result = MF_E_TOPO_CODEC_NOT_FOUND;
        // An encoder's output is set before its input.
        ComPtr<IMFMediaType> output = aac_type(options_, true);
        ComPtr<IMFMediaType> input = pcm_type(options_.recordedRate(), options_.channels);
        if (SUCCEEDED(result)) result = encoder_->SetOutputType(0, output.Get(), 0);
        if (SUCCEEDED(result)) result = encoder_->SetInputType(0, input.Get(), 0);
        if (SUCCEEDED(result)) result = encoder_->ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0);
        if (SUCCEEDED(result)) result = encoder_->ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0);
        if (FAILED(result)) {
            error = "the system's AAC encoder cannot record this";
            encoder_.Reset();
            return false;
        }
        return true;
    }

    bool write(const std::int16_t *samples, std::size_t count, std::int64_t first_sample, std::string *out,
               std::string &error) override {
        ComPtr<IMFSample> sample =
            media_sample(samples, count, first_sample, options_.recordedRate(), options_.channels);
        if (sample == nullptr) {
            error = "the recording could not be written";
            return false;
        }
        HRESULT result = encoder_->ProcessInput(0, sample.Get(), 0);
        if (result == MF_E_NOTACCEPTING) {
            // Full: what it has made is taken, then it takes the input.
            if (!drain(out, error)) return false;
            result = encoder_->ProcessInput(0, sample.Get(), 0);
        }
        if (FAILED(result)) {
            error = "the recording could not be encoded";
            return false;
        }
        return drain(out, error);
    }

    bool finish(std::string *out, std::string &error) override {
        if (encoder_ == nullptr) return true;
        encoder_->ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0);
        encoder_->ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0);
        const bool drained = drain(out, error);
        encoder_.Reset();
        file_.close();
        return drained;
    }

private:
    /// Every frame the encoder has made, into the file and `out`.
    bool drain(std::string *out, std::string &error) {
        MFT_OUTPUT_STREAM_INFO info{};
        if (FAILED(encoder_->GetOutputStreamInfo(0, &info))) {
            error = "the recording could not be encoded";
            return false;
        }
        for (;;) {
            ComPtr<IMFMediaBuffer> buffer;
            ComPtr<IMFSample> sample;
            if (FAILED(MFCreateMemoryBuffer(std::max<DWORD>(info.cbSize, 8192), buffer.GetAddressOf())) ||
                FAILED(MFCreateSample(sample.GetAddressOf())) || FAILED(sample->AddBuffer(buffer.Get()))) {
                error = "the recording could not be encoded";
                return false;
            }
            MFT_OUTPUT_DATA_BUFFER made{};
            made.pSample = sample.Get();
            DWORD status = 0;
            const HRESULT result = encoder_->ProcessOutput(0, 1, &made, &status);
            if (made.pEvents != nullptr) made.pEvents->Release();
            if (result == MF_E_TRANSFORM_NEED_MORE_INPUT) return true;
            if (FAILED(result)) {
                error = "the recording could not be encoded";
                return false;
            }
            BYTE *bytes = nullptr;
            DWORD length = 0;
            if (FAILED(buffer->Lock(&bytes, nullptr, &length))) {
                error = "the recording could not be encoded";
                return false;
            }
            const bool written = file_.write(bytes, length);
            if (written && out != nullptr) out->append(reinterpret_cast<const char *>(bytes), length);
            buffer->Unlock();
            if (!written) {
                error = "the recording could not be written";
                return false;
            }
        }
    }

    const RecordingOptions options_;
    MediaFoundation media_;
    File file_;
    ComPtr<IMFTransform> encoder_;
};

std::unique_ptr<Writer> open_writer(const RecordingOptions &options, const std::wstring &path, std::string &error) {
    switch (options.format) {
        case RecordingOptions::Format::Pcm:
        case RecordingOptions::Format::Wav: {
            auto writer = std::make_unique<SampleWriter>(options, path, options.format == RecordingOptions::Format::Wav);
            if (!writer->open(error)) return nullptr;
            return writer;
        }
        case RecordingOptions::Format::Aac:
            if (options.frameBytes > 0) {
                auto writer = std::make_unique<AacStreamWriter>(options, path);
                if (!writer->open(error)) return nullptr;
                return writer;
            } else {
                auto writer = std::make_unique<AacFileWriter>(options, path);
                if (!writer->open(error)) return nullptr;
                return writer;
            }
    }
    error = kInvalid;
    return nullptr;
}

}  // namespace

// ---- the options -------------------------------------------------------------------------------------

std::uint32_t RecordingOptions::recordingIdOf(const Json &request) {
    const double id = request["recordingId"].numberOr(0);
    return id >= 1 && id <= std::numeric_limits<std::uint32_t>::max() ? static_cast<std::uint32_t>(id) : 0;
}

std::optional<RecordingOptions> RecordingOptions::parse(const Json &request) {
    RecordingOptions options;
    options.recordingId = recordingIdOf(request);
    const std::optional<int> duration = number(request, "duration", options.durationMs);
    const std::optional<int> rate = number(request, "sampleRate", options.sampleRate);
    const std::optional<int> channels = number(request, "numberOfChannels", options.channels);
    const std::optional<int> bit_rate = number(request, "encodeBitRate", options.bitRate);
    const std::optional<int> frame_kb = number(request, "frameSize", 0);
    if (!duration || !rate || !channels || !bit_rate || !frame_kb) return std::nullopt;
    options.durationMs = *duration;
    options.sampleRate = *rate;
    options.channels = *channels;
    options.bitRate = *bit_rate;
    std::string format = request["format"].stringOr("aac");
    std::transform(format.begin(), format.end(), format.begin(),
                   [](char letter) { return static_cast<char>(std::tolower(static_cast<unsigned char>(letter))); });
    if (format == "pcm") {
        options.format = Format::Pcm;
    } else if (format == "wav") {
        options.format = Format::Wav;
    } else if (format == "aac" || format == "mp3") {
        options.format = Format::Aac;
    } else {
        return std::nullopt;
    }
    const bool known_rate =
        std::find(kSampleRates.begin(), kSampleRates.end(), options.sampleRate) != kSampleRates.end();
    // A frame is handed over whole, and the library takes up to 1 MiB at a time.
    if (!known_rate || options.channels < 1 || options.channels > 2 || options.bitRate <= 0 || options.durationMs < 1 ||
        options.durationMs > kLongestMs || *frame_kb < 0 || *frame_kb > 1024) {
        return std::nullopt;
    }
    options.frameBytes = *frame_kb * 1024;
    return options;
}

int RecordingOptions::recordedRate() const noexcept {
    if (format != Format::Aac) return sampleRate;
    return std::abs(sampleRate - 44'100) < std::abs(sampleRate - 48'000) ? 44'100 : 48'000;
}

int RecordingOptions::aacBitRate() const noexcept {
    return *std::min_element(kAacBitRates.begin(), kAacBitRates.end(),
                             [this](int left, int right) { return std::abs(left - bitRate) < std::abs(right - bitRate); });
}

const wchar_t *RecordingOptions::suffix() const noexcept {
    switch (format) {
        case Format::Pcm:
            return L"pcm";
        case Format::Wav:
            return L"wav";
        case Format::Aac:
            return frameBytes > 0 ? L"aac" : L"m4a";
    }
    return L"";
}

std::unique_ptr<AudioSource> systemMicrophone() { return std::make_unique<Microphone>(); }

// ---- one recording -------------------------------------------------------------------------------------

/// One recording, on a thread of its own: it opens the input and the file,
/// records until it is stopped, its duration is full or it fails, and tells
/// content through the GUI thread -- events and frames in the order made.
class Recorder::Recording final {
public:
    enum class Command { Pause, Resume, Stop, Cancel };

    Recording(RecordingOptions options, std::wstring path, std::shared_ptr<HostServiceChannel> channel,
              AudioSourceFactory source, GuiQueue::Post post)
        : options_(std::move(options)), path_(std::move(path)), channel_(std::move(channel)),
          source_(std::move(source)), post_(std::move(post)), wake_(CreateEventW(nullptr, FALSE, FALSE, nullptr)) {
        thread_ = std::thread([this] { run(); });
    }

    /// Ends the recording with nothing said of it, its file gone.
    ~Recording() {
        command(Command::Cancel);
        thread_.join();
        CloseHandle(wake_);
    }

    Recording(const Recording &) = delete;
    Recording &operator=(const Recording &) = delete;

    [[nodiscard]] std::uint32_t id() const noexcept { return options_.recordingId; }
    [[nodiscard]] bool ended() const noexcept { return ended_.load(); }

    void command(Command command) {
        {
            const std::lock_guard<std::mutex> lock(mutex_);
            commands_.push_back(command);
        }
        SetEvent(wake_);
    }

private:
    void tell(std::uint32_t event, Json payload = Json::object()) {
        payload.set("recordingId", options_.recordingId);
        post_([channel = channel_, event, payload = std::move(payload)] {
            (void)channel->post(MIGO_HOST_SERVICE_RECORDER, event, payload);
        });
    }

    void frame(std::string bytes, bool last) {
        post_([channel = channel_, id = options_.recordingId, bytes = std::move(bytes), last] {
            (void)channel->postRecorderFrame(id, bytes, last);
        });
    }

    /// The bytes recorded joined to what was left, every whole frame cut off.
    /// The newest is held back, so the recording's last frame is one there is.
    void cut(const std::string &bytes) {
        pending_ += bytes;
        const auto size = static_cast<std::size_t>(options_.frameBytes);
        while (size > 0 && pending_.size() >= size) {
            if (held_.has_value()) frame(std::move(*held_), false);
            held_ = pending_.substr(0, size);
            pending_.erase(0, size);
        }
    }

    void run() {
        const Apartment apartment;
        std::string error;
        std::unique_ptr<AudioSource> input = source_();
        std::unique_ptr<Writer> writer;
        if (input == nullptr || !input->open(options_.recordedRate(), options_.channels, error) ||
            (writer = open_writer(options_, path_, error)) == nullptr || !input->start(error)) {
            writer.reset();
            DeleteFileW(path_.c_str());
            if (!cancelled()) tell(MIGO_RECORDER_EVENT_ERROR, Json::object().set("errMsg", error));
            ended_.store(true);
            return;
        }
        tell(MIGO_RECORDER_EVENT_START);

        const std::int64_t limit = static_cast<std::int64_t>(options_.durationMs) * options_.recordedRate() / 1000;
        const bool frames = options_.frameBytes > 0;
        std::int64_t recorded = 0;
        bool paused = false;
        std::vector<std::int16_t> samples;
        const HANDLE waits[2] = {wake_, input->ready()};
        for (;;) {
            WaitForMultipleObjects(2, waits, FALSE, kIdleMs);
            // What content asked, in order.
            std::deque<Command> asked;
            {
                const std::lock_guard<std::mutex> lock(mutex_);
                asked.swap(commands_);
            }
            bool stopping = false;
            for (Command command : asked) {
                if (command == Command::Cancel) {
                    input->stop();
                    std::string ignored;
                    writer->finish(nullptr, ignored);
                    writer.reset();
                    DeleteFileW(path_.c_str());
                    ended_.store(true);
                    return;
                }
                if (command == Command::Stop) stopping = true;
                if (command == Command::Pause && !paused) {
                    input->stop();
                    paused = true;
                    tell(MIGO_RECORDER_EVENT_PAUSE);
                } else if (command == Command::Resume && paused) {
                    if (!input->start(error)) {
                        fail(*writer, error);
                        return;
                    }
                    paused = false;
                    tell(MIGO_RECORDER_EVENT_RESUME);
                }
            }
            bool full = false;
            if (!paused) {
                samples.clear();
                if (!input->read(samples, error)) {
                    fail(*writer, error);
                    return;
                }
                // As far as the duration has room for them, to the sample.
                const auto per_frame = static_cast<std::size_t>(options_.channels);
                std::size_t taken = std::min<std::size_t>(samples.size() / per_frame,
                                                          static_cast<std::size_t>(limit - recorded)) *
                                    per_frame;
                if (taken > 0) {
                    std::string made;
                    if (!writer->write(samples.data(), taken, recorded, frames ? &made : nullptr, error)) {
                        fail(*writer, error);
                        return;
                    }
                    recorded += static_cast<std::int64_t>(taken / per_frame);
                    if (frames) cut(made);
                }
                full = recorded >= limit;
            }
            if (stopping || full) {
                input->stop();
                finish(*writer, recorded);
                return;
            }
        }
    }

    [[nodiscard]] bool cancelled() {
        const std::lock_guard<std::mutex> lock(mutex_);
        return std::find(commands_.begin(), commands_.end(), Command::Cancel) != commands_.end();
    }

    void fail(Writer &writer, const std::string &reason) {
        std::string ignored;
        writer.finish(nullptr, ignored);
        DeleteFileW(path_.c_str());
        tell(MIGO_RECORDER_EVENT_ERROR, Json::object().set("errMsg", reason));
        ended_.store(true);
    }

    /// The file written, its last frames handed over, then content told.
    void finish(Writer &writer, std::int64_t recorded) {
        std::string error;
        std::string tail;
        const bool frames = options_.frameBytes > 0;
        if (!writer.finish(frames ? &tail : nullptr, error)) {
            DeleteFileW(path_.c_str());
            tell(MIGO_RECORDER_EVENT_ERROR, Json::object().set("errMsg", error));
            ended_.store(true);
            return;
        }
        if (frames) {
            cut(tail);
            // What is left, however short, is the last frame; else the one held.
            if (!pending_.empty()) {
                if (held_.has_value()) frame(std::move(*held_), false);
                held_ = std::move(pending_);
            }
            if (held_.has_value()) frame(std::move(*held_), true);
        }
        WIN32_FILE_ATTRIBUTE_DATA file{};
        const double size = GetFileAttributesExW(path_.c_str(), GetFileExInfoStandard, &file)
                                ? static_cast<double>((static_cast<std::uint64_t>(file.nFileSizeHigh) << 32) |
                                                      file.nFileSizeLow)
                                : 0.0;
        const double duration = std::floor(static_cast<double>(recorded) * 1000.0 / options_.recordedRate());
        // The file goes to the Session as the event is posted; what it does not take goes then.
        Json payload = Json::object().set("tempFilePath", utf8Of(path_)).set("duration", duration).set("fileSize", size);
        payload.set("recordingId", options_.recordingId);
        post_([channel = channel_, payload = std::move(payload), path = path_] {
            (void)channel->post(MIGO_HOST_SERVICE_RECORDER, MIGO_RECORDER_EVENT_STOP, payload);
            DeleteFileW(path.c_str());
        });
        ended_.store(true);
    }

    const RecordingOptions options_;
    const std::wstring path_;
    const std::shared_ptr<HostServiceChannel> channel_;
    const AudioSourceFactory source_;
    const GuiQueue::Post post_;
    const HANDLE wake_;
    std::mutex mutex_;
    std::deque<Command> commands_;
    std::atomic<bool> ended_{false};
    std::string pending_;
    std::optional<std::string> held_;
    std::thread thread_;
};

// ---- the service -----------------------------------------------------------------------------------

Recorder::Recorder(std::shared_ptr<HostServiceChannel> channel, std::wstring staging_root, AudioSourceFactory source)
    : channel_(std::move(channel)), staging_root_(std::move(staging_root)), source_(std::move(source)) {}

Recorder::~Recorder() = default;

void Recorder::call(std::uint32_t method, const Json &request) {
    // A recording that ended is gone once anything more is asked.
    if (recording_ != nullptr && recording_->ended()) recording_.reset();
    const std::uint32_t id = RecordingOptions::recordingIdOf(request);
    switch (method) {
        case MIGO_RECORDER_START: {
            // A recording already under way stops first, with no STOP.
            recording_.reset();
            const std::optional<RecordingOptions> options = RecordingOptions::parse(request);
            const auto refuse = [this, id](const char *why) {
                (void)channel_->post(MIGO_HOST_SERVICE_RECORDER, MIGO_RECORDER_EVENT_ERROR,
                                     Json::object().set("errMsg", why).set("recordingId", id));
            };
            if (!options.has_value() || options->recordingId == 0) {
                refuse(kInvalid);
                return;
            }
            if (staging_ == nullptr) staging_ = std::make_shared<Staging>(staging_root_);
            const std::wstring path = staging_->path(options->suffix());
            if (path.empty()) {
                refuse("there is nowhere to put the recording");
                return;
            }
            recording_ = std::make_unique<Recording>(*options, path, channel_, source_, queue_.poster());
            return;
        }
        case MIGO_RECORDER_PAUSE:
        case MIGO_RECORDER_RESUME:
        case MIGO_RECORDER_STOP:
            // A request in the wrong state, or for another recording, is ignored.
            if (recording_ == nullptr || recording_->id() != id) return;
            recording_->command(method == MIGO_RECORDER_PAUSE    ? Recording::Command::Pause
                                : method == MIGO_RECORDER_RESUME ? Recording::Command::Resume
                                                                 : Recording::Command::Stop);
            return;
        default:
            return;
    }
}

}  // namespace migo::windows_host

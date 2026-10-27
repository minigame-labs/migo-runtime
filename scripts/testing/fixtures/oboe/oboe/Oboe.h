// Controllable device fixture for the production bridge's ownership contract.
// This header is never included by the Android build.
#pragma once
#include <cstdint>
#include <memory>
#include <new>

namespace oboe {
enum class Result { OK = 0, ErrorIllegalArgument = -1, ErrorInvalidFormat = -2,
                    ErrorNoMemory = -3, ErrorInternal = -4 };
enum class DataCallbackResult { Continue, Stop };
enum class AudioApi { OpenSLES, AAudio };
enum class AudioFormat { Float, I16 };
enum class Direction { Output };
enum class PerformanceMode { LowLatency };
enum class SharingMode { Shared };
enum class Usage { Game };
class AudioStream;
struct AudioStreamDataCallback {
    virtual ~AudioStreamDataCallback() = default;
    virtual DataCallbackResult onAudioReady(AudioStream *, void *, int32_t) = 0;
};
struct AudioStreamErrorCallback {
    virtual ~AudioStreamErrorCallback() = default;
    virtual bool onError(AudioStream *, Result) = 0;
};
namespace fixture {
inline Result open_result = Result::OK;
inline AudioFormat format = AudioFormat::Float;
inline AudioApi api = AudioApi::AAudio;
inline std::weak_ptr<AudioStream> last_stream;
inline int closes = 0;
inline int tunes = 0;
inline bool throw_control = false;
}
class AudioStream {
public:
    std::shared_ptr<AudioStreamDataCallback> data;
    std::shared_ptr<AudioStreamErrorCallback> error;
    Result requestStop() { return Result::OK; }
    Result requestStart() { if (fixture::throw_control) throw std::bad_alloc(); return Result::OK; }
    Result requestPause() { if (fixture::throw_control) throw std::bad_alloc(); return Result::OK; }
    Result close() { ++fixture::closes; return Result::OK; }
    AudioFormat getFormat() { return fixture::format; }
    int32_t getChannelCount() { return 2; }
    int32_t getSampleRate() { return 48000; }
    int32_t getFramesPerBurst() { return 192; }
    int32_t getBufferCapacityInFrames() { return 768; }
    int32_t getBufferSizeInFrames() { return 384; }
    AudioApi getAudioApi() { return fixture::api; }
};
class AudioStreamBuilder {
public:
    std::shared_ptr<AudioStreamDataCallback> data;
    std::shared_ptr<AudioStreamErrorCallback> error;
    AudioStreamBuilder *setDirection(Direction) { return this; }
    AudioStreamBuilder *setPerformanceMode(PerformanceMode) { return this; }
    AudioStreamBuilder *setSharingMode(SharingMode) { return this; }
    AudioStreamBuilder *setUsage(Usage) { return this; }
    AudioStreamBuilder *setFormat(AudioFormat) { return this; }
    AudioStreamBuilder *setChannelCount(int32_t) { return this; }
    AudioStreamBuilder *setFormatConversionAllowed(bool) { return this; }
    AudioStreamBuilder *setChannelConversionAllowed(bool) { return this; }
    AudioStreamBuilder *setDataCallback(std::shared_ptr<AudioStreamDataCallback> value) {
        data = std::move(value); return this;
    }
    AudioStreamBuilder *setErrorCallback(std::shared_ptr<AudioStreamErrorCallback> value) {
        error = std::move(value); return this;
    }
    Result openStream(std::shared_ptr<AudioStream> &output) {
        if (fixture::open_result != Result::OK) return fixture::open_result;
        output = std::make_shared<AudioStream>();
        output->data = data;
        output->error = error;
        fixture::last_stream = output;
        return Result::OK;
    }
};
class LatencyTuner {
public:
    explicit LatencyTuner(AudioStream &) {}
    Result tune() { ++fixture::tunes; return Result::OK; }
};
} // namespace oboe

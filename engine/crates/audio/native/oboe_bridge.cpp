#include "oboe_bridge.h"
#include <oboe/Oboe.h>
#include <atomic>
#include <memory>
#include <new>
#include <utility>

namespace {
// The guard exists before allocating the shared callback. If allocation throws,
// Rust still gets its context back exactly once.
struct Context {
    MigoOboeCallbacks callbacks;
    explicit Context(MigoOboeCallbacks callbacks) : callbacks(callbacks) {}
    Context(Context &&other) noexcept : callbacks(other.callbacks) {
        other.callbacks.release = nullptr;
    }
    ~Context() {
        if (callbacks.release) callbacks.release(callbacks.context);
    }
    Context(const Context &) = delete;
    Context &operator=(const Context &) = delete;
};

class Callback final : public oboe::AudioStreamDataCallback,
                       public oboe::AudioStreamErrorCallback {
public:
    explicit Callback(Context &&context) : context(std::move(context)) {}
    oboe::DataCallbackResult onAudioReady(oboe::AudioStream *, void *data,
                                          int32_t frames) noexcept override {
        // Oboe serializes a stream's data callbacks. Rust's SPSC consumer and
        // render state are exclusively owned by this callback, never the error
        // or control thread. Error callbacks touch only the atomic below.
        if (frames > 0) {
            context.callbacks.render(context.callbacks.context,
                                     static_cast<float *>(data), size_t(frames) * 2);
        }
        if (tuner) (void)tuner->tune();
        return oboe::DataCallbackResult::Continue;
    }
    bool onError(oboe::AudioStream *, oboe::Result error) noexcept override {
        last_error.store(static_cast<int32_t>(error), std::memory_order_release);
        // Let Oboe stop/close on its own error thread. Its shared stream owner
        // protects this callback even if the audio thread drops its handle.
        return false;
    }
    Context context;
    std::atomic<int32_t> last_error{0};
    std::unique_ptr<oboe::LatencyTuner> tuner;
};
} // namespace

struct MigoOboeStream {
    // Member order also keeps the callback alive through stream destruction.
    std::shared_ptr<Callback> callback;
    std::shared_ptr<oboe::AudioStream> stream;
    ~MigoOboeStream() {
        if (stream) {
            (void)stream->requestStop();
            // Keep Oboe's device workarounds and delay-before-close enabled.
            // They handle Android's known late callback behaviour.
            (void)stream->close();
        }
    }
};

extern "C" int32_t migo_oboe_open(MigoOboeCallbacks callbacks, MigoOboeConfig *config,
                                   MigoOboeStream **output) noexcept {
    Context context(callbacks);
    if (output) *output = nullptr;
    if (!config || !output || !callbacks.render || !callbacks.release) {
        return static_cast<int32_t>(oboe::Result::ErrorIllegalArgument);
    }
    try {
        auto handle = std::make_unique<MigoOboeStream>();
        handle->callback = std::make_shared<Callback>(std::move(context));
        oboe::AudioStreamBuilder builder;
        builder.setDirection(oboe::Direction::Output)
            ->setPerformanceMode(oboe::PerformanceMode::LowLatency)
            ->setSharingMode(oboe::SharingMode::Shared)
            ->setUsage(oboe::Usage::Game)
            ->setFormat(oboe::AudioFormat::Float)
            ->setChannelCount(2)
            ->setFormatConversionAllowed(true)
            ->setChannelConversionAllowed(true)
            ->setDataCallback(handle->callback)
            ->setErrorCallback(handle->callback);
        // Leave sample rate/API unspecified: use the device rate and Oboe's
        // AAudio/OpenSL ES choice, including API26/device-specific fallbacks.
        const auto result = builder.openStream(handle->stream);
        if (result != oboe::Result::OK) return static_cast<int32_t>(result);
        auto &stream = *handle->stream;
        if (stream.getFormat() != oboe::AudioFormat::Float || stream.getChannelCount() != 2
            || stream.getSampleRate() <= 0) {
            return static_cast<int32_t>(oboe::Result::ErrorInvalidFormat);
        }
        // Oboe's supported burst/xrun algorithm controls the hardware buffer;
        // this does not pretend to tune Migo's separate software queue yet.
        if (stream.getAudioApi() == oboe::AudioApi::AAudio) {
            handle->callback->tuner = std::make_unique<oboe::LatencyTuner>(stream);
        }
        *config = {stream.getSampleRate(), stream.getChannelCount(), stream.getFramesPerBurst(),
                   stream.getBufferCapacityInFrames(), stream.getBufferSizeInFrames(),
                   static_cast<int32_t>(stream.getAudioApi())};
        *output = handle.release();
        return 0;
    } catch (const std::bad_alloc &) {
        return static_cast<int32_t>(oboe::Result::ErrorNoMemory);
    } catch (...) {
        // No C++ exception may cross the C ABI.
        return static_cast<int32_t>(oboe::Result::ErrorInternal);
    }
}

extern "C" int32_t migo_oboe_start(MigoOboeStream *handle) noexcept {
    try {
        return static_cast<int32_t>(handle->stream->requestStart());
    } catch (...) {
        return static_cast<int32_t>(oboe::Result::ErrorInternal);
    }
}
extern "C" int32_t migo_oboe_pause(MigoOboeStream *handle) noexcept {
    try {
        return static_cast<int32_t>(handle->stream->requestPause());
    } catch (...) {
        return static_cast<int32_t>(oboe::Result::ErrorInternal);
    }
}
extern "C" int32_t migo_oboe_error(const MigoOboeStream *handle) noexcept {
    return handle->callback->last_error.load(std::memory_order_acquire);
}
extern "C" void migo_oboe_close(MigoOboeStream *handle) noexcept { delete handle; }

static_assert(sizeof(MigoOboeCallbacks) == sizeof(void *) * 3);
static_assert(sizeof(MigoOboeConfig) == sizeof(int32_t) * 6);
static_assert(std::atomic<int32_t>::is_always_lock_free);

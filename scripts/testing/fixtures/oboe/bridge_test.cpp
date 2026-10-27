#include "oboe_bridge.h"
#include <oboe/Oboe.h>
#include <atomic>
#include <cassert>
#include <cstdlib>
#include <new>
#include <thread>

static std::atomic<int> allocations_before_failure{-1};
void *operator new(size_t size) {
    const int remaining = allocations_before_failure.load();
    if (remaining == 0) throw std::bad_alloc();
    if (remaining > 0) --allocations_before_failure;
    if (auto result = std::malloc(size)) return result;
    throw std::bad_alloc();
}
void operator delete(void *pointer) noexcept { std::free(pointer); }
void operator delete(void *pointer, size_t) noexcept { std::free(pointer); }

struct State {
    int renders = 0;
    int releases = 0;
};
static void render(void *context, float *data, size_t size) {
    auto &state = *static_cast<State *>(context);
    assert(state.releases == 0);
    ++state.renders;
    for (size_t i = 0; i < size; ++i) data[i] = 0.25f;
}
static void release(void *context) { ++static_cast<State *>(context)->releases; }
static MigoOboeCallbacks callbacks(State &state) { return {&state, render, release}; }

int main() {
    MigoOboeConfig config{};
    MigoOboeStream *handle = nullptr;
    {
        State state;
        assert(migo_oboe_open(callbacks(state), nullptr, &handle) != 0);
        assert(!handle && state.releases == 1);
    }
    for (int allocation = 0; allocation < 4; ++allocation) {
        State state;
        const int closes = oboe::fixture::closes;
        allocations_before_failure = allocation;
        const auto result = migo_oboe_open(callbacks(state), &config, &handle);
        allocations_before_failure = -1;
        assert(result != 0);
        assert(!handle && state.releases == 1);
        // A failure allocating the tuner happens after the native stream opens.
        if (allocation == 3) assert(oboe::fixture::closes == closes + 1);
    }
    {
        State state;
        oboe::fixture::open_result = oboe::Result::ErrorInternal;
        assert(migo_oboe_open(callbacks(state), &config, &handle) != 0);
        assert(!handle && state.releases == 1);
        oboe::fixture::open_result = oboe::Result::OK;
    }
    {
        State state;
        oboe::fixture::format = oboe::AudioFormat::I16;
        const int before = oboe::fixture::closes;
        assert(migo_oboe_open(callbacks(state), &config, &handle) != 0);
        assert(!handle && state.releases == 1);
        assert(oboe::fixture::closes == before + 1);
        oboe::fixture::format = oboe::AudioFormat::Float;
    }
    {
        State state;
        assert(migo_oboe_open(callbacks(state), &config, &handle) == 0);
        assert(config.sample_rate == 48000 && config.channels == 2);
        assert(config.frames_per_burst == 192);
        assert(migo_oboe_start(handle) == 0 && migo_oboe_pause(handle) == 0);
        oboe::fixture::throw_control = true;
        assert(migo_oboe_start(handle) != 0 && migo_oboe_pause(handle) != 0);
        oboe::fixture::throw_control = false;
        assert(migo_oboe_error(handle) == 0);
        auto stream = oboe::fixture::last_stream.lock();
        float samples[384]{};
        const int before_tunes = oboe::fixture::tunes;
        stream->data->onAudioReady(stream.get(), samples, 192);
        assert(state.renders == 1 && samples[383] == 0.25f);
        assert(oboe::fixture::tunes == before_tunes + 1);
        std::thread error_thread([stream] {
            assert(!stream->error->onError(stream.get(), oboe::Result::ErrorInternal));
        });
        error_thread.join();
        assert(migo_oboe_error(handle) != 0);
        migo_oboe_close(handle);
        // The native error worker may still own the shared stream. Dropping
        // the Rust-facing handle must not free its callback context early.
        assert(state.releases == 0);
        stream->data->onAudioReady(stream.get(), samples, 192);
        assert(state.renders == 2);
        stream.reset();
        assert(state.releases == 1);
    }
    {
        State state;
        oboe::fixture::api = oboe::AudioApi::OpenSLES;
        assert(migo_oboe_open(callbacks(state), &config, &handle) == 0);
        auto stream = oboe::fixture::last_stream.lock();
        float samples[384]{};
        const int before_tunes = oboe::fixture::tunes;
        stream->data->onAudioReady(stream.get(), samples, 192);
        assert(oboe::fixture::tunes == before_tunes);
        stream.reset();
        migo_oboe_close(handle);
        assert(state.releases == 1);
    }
}

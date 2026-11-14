// The Host Kit's recorder against the strict fake of the C ABI, its sound a
// tone of the test's own: what each format writes, the frames, the duration,
// pause and resume, a recording replaced, and an input that cannot be opened.

#include "../src/recorder.hpp"

#include "fake_migo.hpp"

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
#include <objbase.h>

#include <chrono>
#include <cmath>
#include <cstdio>
#include <filesystem>
#include <optional>
#include <string>
#include <vector>

namespace {

int g_failures = 0;

#define CHECK(condition)                                                                        \
    do {                                                                                        \
        if (!(condition)) {                                                                     \
            std::fprintf(stderr, "%s:%d: CHECK failed: %s\n", __FILE__, __LINE__, #condition); \
            ++g_failures;                                                                       \
        }                                                                                       \
    } while (false)

using migo::windows_host::AudioSource;
using migo::windows_host::hostServiceBit;
using migo::windows_host::HostServiceChannel;
using migo::windows_host::Json;
using migo::windows_host::Recorder;
using migo::windows_host::RecordingOptions;

template <typename Done>
bool pump_until(Done done, int ms = 10000) {
    const auto deadline = std::chrono::steady_clock::now() + std::chrono::milliseconds(ms);
    while (!done()) {
        MSG message;
        while (PeekMessageW(&message, nullptr, 0, 0, PM_REMOVE)) {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
        if (done()) return true;
        if (std::chrono::steady_clock::now() > deadline) return false;
        MsgWaitForMultipleObjects(0, nullptr, FALSE, 5, QS_ALLINPUT);
    }
    return true;
}

/// A 440 Hz tone, as fast as time goes: a waitable timer says when samples wait.
class Tone final : public AudioSource {
public:
    explicit Tone(bool opens = true) : opens_(opens), timer_(CreateWaitableTimerW(nullptr, FALSE, nullptr)) {}
    ~Tone() override { CloseHandle(timer_); }

    bool open(int rate, int channels, std::string &error) override {
        if (!opens_) {
            error = "there is no microphone";
            return false;
        }
        rate_ = rate;
        channels_ = channels;
        return true;
    }
    bool start(std::string &) override {
        // From where it stopped: a pause is no sound, not lost sound.
        started_ = std::chrono::steady_clock::now() -
                   std::chrono::microseconds(static_cast<long long>(produced_ * 1'000'000 / rate_));
        LARGE_INTEGER due{};
        due.QuadPart = -200'000;  // 20 ms, relative
        SetWaitableTimer(timer_, &due, 20, nullptr, nullptr, FALSE);
        return true;
    }
    void stop() override { CancelWaitableTimer(timer_); }
    void *ready() override { return timer_; }
    bool read(std::vector<std::int16_t> &samples, std::string &) override {
        const double elapsed = std::chrono::duration<double>(std::chrono::steady_clock::now() - started_).count();
        const auto due = static_cast<std::int64_t>(elapsed * rate_);
        for (; produced_ < due; ++produced_) {
            const auto value = static_cast<std::int16_t>(8000 * std::sin(2 * 3.14159265358979 * 440 * produced_ / rate_));
            for (int channel = 0; channel < channels_; ++channel) samples.push_back(value);
        }
        return true;
    }

private:
    const bool opens_;
    const HANDLE timer_;
    int rate_ = 8000;
    int channels_ = 1;
    std::int64_t produced_ = 0;
    std::chrono::steady_clock::time_point started_{};
};

MigoResult MIGO_CALL run_now(void *, MigoTaskFn task, void *context) {
    task(context);
    return MIGO_OK;
}
void MIGO_CALL ignore_call(void *, MigoSession *, const MigoHostServiceCall *) {}

/// A Session that declares the recorder, and its channel.
std::shared_ptr<HostServiceChannel> recorder_session() {
    fake_migo::reset();
    MigoSessionConfig config{};
    config.struct_size = static_cast<std::uint32_t>(sizeof config);
    config.abi_version = MIGO_ABI_VERSION_1;
    MigoSession *session = nullptr;
    CHECK(migo_session_create(fake_migo::engine(), &config, &session) == MIGO_OK);
    MigoHostCallbacks callbacks{};
    callbacks.struct_size = static_cast<std::uint32_t>(sizeof callbacks);
    callbacks.abi_version = MIGO_ABI_VERSION_1;
    callbacks.dispatch = &run_now;
    callbacks.on_host_service_call = &ignore_call;
    callbacks.host_services = hostServiceBit(MIGO_HOST_SERVICE_RECORDER);
    CHECK(migo_session_set_host_callbacks(session, &callbacks) == MIGO_OK);
    return HostServiceChannel::open(session);
}

std::optional<Json> event(std::uint32_t kind) {
    for (const auto &posted : fake_migo::host_service_events()) {
        if (posted.service == MIGO_HOST_SERVICE_RECORDER && posted.event == kind) return Json::parse(posted.payload);
    }
    return std::nullopt;
}

std::size_t events_of(std::uint32_t kind) {
    std::size_t count = 0;
    for (const auto &posted : fake_migo::host_service_events()) {
        if (posted.service == MIGO_HOST_SERVICE_RECORDER && posted.event == kind) ++count;
    }
    return count;
}

std::wstring staging() {
    const std::filesystem::path directory = std::filesystem::temp_directory_path() / L"migo-windows-recorder-test";
    std::filesystem::create_directories(directory);
    return directory.wstring();
}

Json start(std::uint32_t id, const char *format, int duration_ms, int rate, int channels, int frame_kb = 0) {
    Json request = Json::object()
                       .set("recordingId", id)
                       .set("duration", duration_ms)
                       .set("sampleRate", rate)
                       .set("numberOfChannels", channels)
                       .set("encodeBitRate", 48000)
                       .set("format", format);
    if (frame_kb > 0) request.set("frameSize", frame_kb);
    return request;
}

std::size_t frame_bytes() {
    std::size_t total = 0;
    for (const auto &frame : fake_migo::recorder_frames()) total += frame.bytes.size();
    return total;
}

void pcm_ends_on_its_duration_to_the_sample() {
    auto channel = recorder_session();
    Recorder recorder(channel, staging(), [] { return std::make_unique<Tone>(); });
    recorder.call(MIGO_RECORDER_START, start(1, "PCM", 500, 16000, 1, 1));
    CHECK(pump_until([] { return event(MIGO_RECORDER_EVENT_STOP).has_value(); }));
    CHECK(event(MIGO_RECORDER_EVENT_START).has_value());
    const std::optional<Json> stopped = event(MIGO_RECORDER_EVENT_STOP);
    if (stopped.has_value()) {
        CHECK((*stopped)["recordingId"].numberOr(0) == 1);
        // 500 ms at 16 kHz, one channel, 16-bit: 8000 samples.
        CHECK((*stopped)["duration"].numberOr(0) == 500);
        CHECK((*stopped)["fileSize"].numberOr(0) == 16000);
        const std::string path = (*stopped)["tempFilePath"].string();
        CHECK(path.size() > 4 && path.substr(path.size() - 4) == ".pcm");
    }
    // The same bytes as frames, 1 KB each but the last, which says so.
    CHECK(frame_bytes() == 16000);
    const auto &frames = fake_migo::recorder_frames();
    CHECK(!frames.empty() && frames.back().last);
    for (std::size_t index = 0; index + 1 < frames.size(); ++index) {
        CHECK(frames[index].bytes.size() == 1024 && !frames[index].last && frames[index].recording_id == 1);
    }
    channel->close();
}

void wav_has_its_header() {
    auto channel = recorder_session();
    Recorder recorder(channel, staging(), [] { return std::make_unique<Tone>(); });
    recorder.call(MIGO_RECORDER_START, start(2, "wav", 250, 8000, 2));
    CHECK(pump_until([] { return event(MIGO_RECORDER_EVENT_STOP).has_value(); }));
    const std::optional<Json> stopped = event(MIGO_RECORDER_EVENT_STOP);
    // 250 ms at 8 kHz, two channels: 2000 frames of 4 bytes, behind 44.
    CHECK(stopped.has_value() && (*stopped)["fileSize"].numberOr(0) == 8044);
    CHECK(fake_migo::recorder_frames().empty());
    channel->close();
}

void aac_is_written_through_media_foundation() {
    auto channel = recorder_session();
    Recorder recorder(channel, staging(), [] { return std::make_unique<Tone>(); });
    // An MPEG-4 file, and -- with frames -- an ADTS stream cut into them.
    recorder.call(MIGO_RECORDER_START, start(3, "aac", 400, 16000, 1));
    CHECK(pump_until([] { return event(MIGO_RECORDER_EVENT_STOP).has_value(); }));
    std::optional<Json> stopped = event(MIGO_RECORDER_EVENT_STOP);
    CHECK(stopped.has_value() && (*stopped)["fileSize"].numberOr(0) > 0);
    if (stopped.has_value()) {
        const std::string path = (*stopped)["tempFilePath"].string();
        CHECK(path.size() > 4 && path.substr(path.size() - 4) == ".m4a");
        // AAC at the encoder's 44.1 kHz, the nearer to 16 kHz.
        CHECK(std::fabs((*stopped)["duration"].numberOr(0) - 400) <= 1);
    }
    channel->close();

    channel = recorder_session();
    Recorder framed(channel, staging(), [] { return std::make_unique<Tone>(); });
    framed.call(MIGO_RECORDER_START, start(4, "mp3", 400, 48000, 2, 1));
    CHECK(pump_until([] { return event(MIGO_RECORDER_EVENT_STOP).has_value(); }));
    stopped = event(MIGO_RECORDER_EVENT_STOP);
    CHECK(stopped.has_value() && (*stopped)["tempFilePath"].string().size() > 4);
    if (stopped.has_value()) {
        const std::string path = (*stopped)["tempFilePath"].string();
        CHECK(path.substr(path.size() - 4) == ".aac");
        // The frames are the stream's bytes, ADTS from the first.
        CHECK(frame_bytes() == static_cast<std::size_t>((*stopped)["fileSize"].numberOr(-1)));
    }
    const auto &frames = fake_migo::recorder_frames();
    CHECK(!frames.empty() && frames.back().last);
    if (!frames.empty() && frames.front().bytes.size() >= 2) {
        CHECK(static_cast<unsigned char>(frames.front().bytes[0]) == 0xFF &&
              (static_cast<unsigned char>(frames.front().bytes[1]) & 0xF0) == 0xF0);
    }
    channel->close();
}

void pause_and_resume_are_reported() {
    auto channel = recorder_session();
    Recorder recorder(channel, staging(), [] { return std::make_unique<Tone>(); });
    recorder.call(MIGO_RECORDER_START, start(5, "PCM", 60000, 8000, 1));
    CHECK(pump_until([] { return event(MIGO_RECORDER_EVENT_START).has_value(); }));
    // Another recording's request is not this one's.
    recorder.call(MIGO_RECORDER_PAUSE, Json::object().set("recordingId", 99));
    recorder.call(MIGO_RECORDER_PAUSE, Json::object().set("recordingId", 5));
    CHECK(pump_until([] { return event(MIGO_RECORDER_EVENT_PAUSE).has_value(); }));
    // Paused twice is paused once.
    recorder.call(MIGO_RECORDER_PAUSE, Json::object().set("recordingId", 5));
    recorder.call(MIGO_RECORDER_RESUME, Json::object().set("recordingId", 5));
    CHECK(pump_until([] { return event(MIGO_RECORDER_EVENT_RESUME).has_value(); }));
    CHECK(events_of(MIGO_RECORDER_EVENT_PAUSE) == 1);
    recorder.call(MIGO_RECORDER_STOP, Json::object().set("recordingId", 5));
    CHECK(pump_until([] { return event(MIGO_RECORDER_EVENT_STOP).has_value(); }));
    const std::optional<Json> stopped = event(MIGO_RECORDER_EVENT_STOP);
    CHECK(stopped.has_value() && (*stopped)["duration"].numberOr(60000) < 60000);
    channel->close();
}

void a_new_start_replaces_the_recording_without_a_stop() {
    auto channel = recorder_session();
    Recorder recorder(channel, staging(), [] { return std::make_unique<Tone>(); });
    recorder.call(MIGO_RECORDER_START, start(6, "PCM", 60000, 8000, 1));
    CHECK(pump_until([] { return event(MIGO_RECORDER_EVENT_START).has_value(); }));
    recorder.call(MIGO_RECORDER_START, start(7, "PCM", 200, 8000, 1));
    CHECK(pump_until([] { return event(MIGO_RECORDER_EVENT_STOP).has_value(); }));
    const std::optional<Json> stopped = event(MIGO_RECORDER_EVENT_STOP);
    CHECK(stopped.has_value() && (*stopped)["recordingId"].numberOr(0) == 7);
    CHECK(events_of(MIGO_RECORDER_EVENT_STOP) == 1);
    channel->close();
}

void what_cannot_record_is_an_error() {
    auto channel = recorder_session();
    Recorder recorder(channel, staging(), [] { return std::make_unique<Tone>(false); });
    recorder.call(MIGO_RECORDER_START, start(8, "wav", 1000, 8000, 1));
    CHECK(pump_until([] { return event(MIGO_RECORDER_EVENT_ERROR).has_value(); }));
    std::optional<Json> failed = event(MIGO_RECORDER_EVENT_ERROR);
    CHECK(failed.has_value() && (*failed)["errMsg"].string() == "there is no microphone" &&
          (*failed)["recordingId"].numberOr(0) == 8);
    CHECK(!event(MIGO_RECORDER_EVENT_START).has_value());
    channel->close();

    channel = recorder_session();
    Recorder strict(channel, staging(), [] { return std::make_unique<Tone>(); });
    strict.call(MIGO_RECORDER_START, start(9, "ogg", 1000, 8000, 1));
    failed = event(MIGO_RECORDER_EVENT_ERROR);
    CHECK(failed.has_value() && (*failed)["errMsg"].string() == "invalid audio parameters");
    strict.call(MIGO_RECORDER_START, start(10, "wav", 1000, 9000, 1));
    CHECK(events_of(MIGO_RECORDER_EVENT_ERROR) == 2);
    channel->close();
}

void the_options_are_the_references() {
    const auto parsed = [](const Json &request) { return RecordingOptions::parse(request); };
    CHECK(parsed(start(1, "aac", 600000, 48000, 2)).has_value());
    CHECK(!parsed(start(1, "aac", 600001, 48000, 2)).has_value());
    CHECK(!parsed(start(1, "aac", 1000, 48000, 3)).has_value());
    CHECK(!parsed(start(1, "aac", 1000, 48000, 1, 1025)).has_value());
    RecordingOptions options = *parsed(start(1, "aac", 1000, 16000, 1));
    CHECK(options.recordedRate() == 44100 && options.aacBitRate() == 96000);
    options = *parsed(start(1, "wav", 1000, 16000, 1));
    CHECK(options.recordedRate() == 16000);
}

}  // namespace

int main() {
    CoInitializeEx(nullptr, COINIT_APARTMENTTHREADED);
    the_options_are_the_references();
    pcm_ends_on_its_duration_to_the_sample();
    wav_has_its_header();
    aac_is_written_through_media_foundation();
    pause_and_resume_are_reported();
    a_new_start_replaces_the_recording_without_a_stop();
    what_cannot_record_is_an_error();
    if (g_failures != 0) {
        std::fprintf(stderr, "%d Recorder assertion(s) failed\n", g_failures);
        return 1;
    }
    std::puts("Recorder contract: PASS");
    return 0;
}

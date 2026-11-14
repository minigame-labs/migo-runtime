#include "background.hpp"

#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#ifndef NOMINMAX
#define NOMINMAX
#endif
#include <windows.h>
#include <objbase.h>

#include <memory>
#include <thread>

namespace migo::windows_host {

namespace {

void NTAPI run(PTP_CALLBACK_INSTANCE, void *context) {
    const std::unique_ptr<std::function<void()>> work(static_cast<std::function<void()> *>(context));
    const bool joined = SUCCEEDED(CoInitializeEx(nullptr, COINIT_MULTITHREADED));
    (*work)();
    if (joined) CoUninitialize();
}

}  // namespace

void runInBackground(std::function<void()> work) {
    auto *job = new std::function<void()>(std::move(work));
    if (!TrySubmitThreadpoolCallback(&run, job, nullptr)) {
        // The pool is out of room: a thread of the job's own.
        std::thread([job] { run(nullptr, job); }).detach();
    }
}

}  // namespace migo::windows_host

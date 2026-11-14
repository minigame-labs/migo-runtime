#include "gui_queue.hpp"

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

#include <cstdio>
#include <exception>
#include <mutex>
#include <new>

namespace migo::windows_host {

namespace {

constexpr UINT kTaskMessage = WM_APP + 3;
constexpr wchar_t kQueueClass[] = L"MigoGuiQueue";

using Task = std::function<void()>;

LRESULT CALLBACK run(HWND window, UINT message, WPARAM wparam, LPARAM lparam) {
    if (message == kTaskMessage) {
        const std::unique_ptr<Task> task(reinterpret_cast<Task *>(lparam));
        (*task)();
        return 0;
    }
    return DefWindowProcW(window, message, wparam, lparam);
}

}  // namespace

struct GuiQueue::State {
    std::mutex mutex;
    HWND window = nullptr;
};

GuiQueue::GuiQueue() : state_(std::make_shared<State>()) {
    static std::once_flag registered;
    std::call_once(registered, [] {
        WNDCLASSEXW queue{};
        queue.cbSize = sizeof queue;
        queue.lpfnWndProc = &run;
        queue.hInstance = GetModuleHandleW(nullptr);
        queue.lpszClassName = kQueueClass;
        if (RegisterClassExW(&queue) == 0) {
            std::fprintf(stderr, "migo windows host kit: RegisterClassExW failed for the GUI queue\n");
            std::terminate();
        }
    });
    state_->window =
        CreateWindowExW(0, kQueueClass, L"", 0, 0, 0, 0, 0, HWND_MESSAGE, nullptr, GetModuleHandleW(nullptr), nullptr);
    if (state_->window == nullptr) {
        std::fprintf(stderr, "migo windows host kit: CreateWindowExW failed for the GUI queue\n");
        std::terminate();
    }
}

GuiQueue::~GuiQueue() {
    HWND window = nullptr;
    {
        // Nothing is posted from here.
        const std::lock_guard<std::mutex> lock(state_->mutex);
        window = state_->window;
        state_->window = nullptr;
    }
    // What still waits is dropped: freed, not run.
    MSG message;
    while (PeekMessageW(&message, window, kTaskMessage, kTaskMessage, PM_REMOVE)) {
        delete reinterpret_cast<Task *>(message.lParam);
    }
    DestroyWindow(window);
}

GuiQueue::Post GuiQueue::poster() const {
    return [state = state_](Task task) {
        const std::lock_guard<std::mutex> lock(state->mutex);
        if (state->window == nullptr) return;
        auto *queued = new (std::nothrow) Task(std::move(task));
        if (queued == nullptr) return;
        if (!PostMessageW(state->window, kTaskMessage, 0, reinterpret_cast<LPARAM>(queued))) delete queued;
    };
}

}  // namespace migo::windows_host

#ifndef MIGO_WINDOWS_HOST_KIT_GUI_QUEUE_HPP_
#define MIGO_WINDOWS_HOST_KIT_GUI_QUEUE_HPP_

// Internal to the Host Kit, not installed: work brought to the GUI thread.

#include <functional>
#include <memory>

namespace migo::windows_host {

/// Tasks for the thread that made the queue -- the GUI thread -- from any
/// thread: each posted to a message-only window of the queue's own and run as
/// the App's message loop dispatches it. A task posted once the queue is gone,
/// or still waiting as it goes, is dropped, never run.
class GuiQueue final {
public:
    using Post = std::function<void(std::function<void()>)>;

    GuiQueue();
    ~GuiQueue();

    GuiQueue(const GuiQueue &) = delete;
    GuiQueue &operator=(const GuiQueue &) = delete;

    /// What posts to this queue, from any thread, for as long as anything
    /// holds it -- after the queue is gone it drops what it is given.
    [[nodiscard]] Post poster() const;

private:
    struct State;
    std::shared_ptr<State> state_;
};

}  // namespace migo::windows_host

#endif  // MIGO_WINDOWS_HOST_KIT_GUI_QUEUE_HPP_

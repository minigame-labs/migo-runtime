#ifndef MIGO_WINDOWS_HOST_KIT_INTERACTION_HPP_
#define MIGO_WINDOWS_HOST_KIT_INTERACTION_HPP_

#include <migo/windows/host_services.hpp>
#include <migo/windows/win32_surface_view.hpp>

#include <cstdint>
#include <memory>

namespace migo::windows_host {

/// The interaction service, Windows' way -- the Linux Host Kit's, for Win32.
///
/// A toast or a loading indicator is a card over the middle of the game: one
/// at a time, showing one replaces the other, hiding a toast leaves a loading
/// indicator up and the other way round. A window of its own, owned by the
/// App's top-level window, that follows the game and takes no input; with
/// `mask` the game takes none either until it goes (the player's keys for the
/// system -- Alt+F4 -- still work). A toast goes after its duration (1.5 s by
/// default). Drawn with Direct2D onto a layered window: its corners, its
/// glyphs and its spinner antialiased, its text the system's (DirectWrite).
/// Its window text is its title, for the system's accessibility.
///
/// A modal and an action sheet are dialogs owned by the App's top-level
/// window, which they disable until they close: modal to it, the way a
/// Windows dialog is, without holding the GUI thread -- the App's message loop
/// keeps running, and a thread-local message hook gives them the keyboard's
/// dialog navigation (Tab, Enter, Esc). The system's controls and message
/// font, so they look as the App does; the buttons in Windows' order,
/// affirmative first. Closing either any other way -- Esc, its close box -- is
/// the player declining it. Destroyed with the service, a dialog still up is
/// answered as declined.
///
/// GUI thread only.
class Interaction final {
public:
    explicit Interaction(Win32SurfaceView &view);
    ~Interaction();

    Interaction(const Interaction &) = delete;
    Interaction &operator=(const Interaction &) = delete;

    /// One of MIGO_INTERACTION_*.
    void call(std::uint32_t method, const Json &request, HostServiceReply reply);

private:
    class Impl;
    std::unique_ptr<Impl> impl_;
};

}  // namespace migo::windows_host

#endif  // MIGO_WINDOWS_HOST_KIT_INTERACTION_HPP_

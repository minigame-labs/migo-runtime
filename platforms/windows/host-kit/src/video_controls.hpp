#ifndef MIGO_WINDOWS_HOST_KIT_VIDEO_CONTROLS_HPP_
#define MIGO_WINDOWS_HOST_KIT_VIDEO_CONTROLS_HPP_

// Internal to the Host Kit, not installed: a video player's own controls.

#include <migo/windows/json.hpp>

#include <cstdint>
#include <memory>
#include <string>

struct HWND__;

namespace migo::windows_host {

/// What a player's controls drive.
class VideoControlled {
public:
    virtual ~VideoControlled() = default;
    [[nodiscard]] virtual bool isPlaying() const = 0;
    /// Seconds; 0 before it knows.
    [[nodiscard]] virtual double position() const = 0;
    /// Seconds; 0 before it knows.
    [[nodiscard]] virtual double duration() const = 0;
    [[nodiscard]] virtual bool isFullScreen() const = 0;
    virtual void togglePlay() = 0;
    virtual void seekTo(double seconds) = 0;
    virtual void toggleFullScreen() = 0;
    /// Whether the controls take input changed (`takesInput`).
    virtual void inputChanged() = 0;
};

/// A video's own controls, over its picture, as content's options ask -- the
/// Linux Host Kit's, the Android SDK's and the Apple SDK's, one for one:
///
///   controls               a bar along the bottom -- play or pause, the time,
///                          a progress bar (showProgressInControlMode), the
///                          length, full screen. A click shows it or hides it;
///                          while playing it hides again after three seconds.
///   showProgress           while the bar is hidden, a thin progress line along
///                          the bottom.
///   showCenterPlayBtn      a play button in the middle while it is not playing.
///   enablePlayGesture      a double click plays or pauses.
///   enableProgressGesture  a sideways drag seeks, the whole width being the
///                          whole video, the time shown as it goes.
///
/// A window of their own above the player's, owned by the App's window: its
/// player's swap chain is the player's alone. What is not a control is the
/// window's colour key -- neither drawn nor hit -- so a click there reaches the
/// player beneath. The buttons are the system's, named as assistive technology
/// reads them -- "play", "play or pause", "full screen" -- the other SDKs' names
/// too; they never take the keyboard, which is the game's. What the player does
/// through them content hears as it hears its own calls. GUI thread only.
class VideoControls final {
public:
    VideoControls(HWND__ *owner, VideoControlled &player);
    ~VideoControls();

    VideoControls(const VideoControls &) = delete;
    VideoControls &operator=(const VideoControls &) = delete;

    /// The options that are the controls'.
    void configure(const Json &options);
    /// Whether any input on the video is the controls': with none of them
    /// asked for, a click on the video is the game's.
    [[nodiscard]] bool takesInput() const;
    /// The player changed: playing, paused, a new time.
    void refresh();
    /// Over `player`'s window, at `left`, `top`, `width` x `height` on the
    /// screen, `scale` its pixels to a logical one; hidden when not `shown`.
    void place(HWND__ *player, int left, int top, int width, int height, float scale, bool shown);
    /// The player's window's mouse: a click, a double click, a drag. True
    /// where it was the controls'.
    bool mouse(unsigned message, std::uintptr_t wparam, std::intptr_t lparam);

    [[nodiscard]] HWND__ *hwnd() const noexcept;

private:
    class Impl;
    std::unique_ptr<Impl> impl_;
};

/// `m:ss`, or `h:mm:ss` from an hour, as the other SDKs show a time.
[[nodiscard]] std::wstring videoClock(double seconds);

}  // namespace migo::windows_host

#endif  // MIGO_WINDOWS_HOST_KIT_VIDEO_CONTROLS_HPP_

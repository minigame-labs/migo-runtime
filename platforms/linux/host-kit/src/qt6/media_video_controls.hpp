#ifndef MIGO_LINUX_QT6_MEDIA_VIDEO_CONTROLS_HPP_
#define MIGO_LINUX_QT6_MEDIA_VIDEO_CONTROLS_HPP_

// Internal to the media component (migo::qt6-media), not installed.

#include <QJsonObject>
#include <QStringList>
#include <QTimer>
#include <QWidget>

#include <optional>

class QLabel;
class QMouseEvent;
class QPaintEvent;
class QResizeEvent;
class QSlider;
class QToolButton;

namespace migo::linux_host::qt6 {

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
/// Android SDK's and the Apple SDK's, one for one:
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
/// The buttons carry the names assistive technology reads -- "play", "play or
/// pause", "full screen" -- which are the other SDKs' too. What the player does
/// through them content hears as it hears its own calls. GUI thread.
class VideoControls final : public QWidget {
public:
    /// The options that are the controls'.
    static const QStringList kOptions;

    VideoControls(VideoControlled &player, QWidget &parent);
    ~VideoControls() override;

    void configure(const QJsonObject &options);
    /// Whether any input on the video is the controls': with none of them
    /// asked for, a click on the video is the game's.
    [[nodiscard]] bool takesInput() const;
    /// The player changed: playing, paused, a new time.
    void refresh();

protected:
    void resizeEvent(QResizeEvent *event) override;
    void paintEvent(QPaintEvent *event) override;
    void mousePressEvent(QMouseEvent *event) override;
    void mouseMoveEvent(QMouseEvent *event) override;
    void mouseReleaseEvent(QMouseEvent *event) override;
    void mouseDoubleClickEvent(QMouseEvent *event) override;

private:
    void place();
    void toggleBar();
    void setBarShown(bool shown);
    void slid(bool done);
    void seekEnded();
    [[nodiscard]] double fraction() const;

    VideoControlled &player_;
    bool controls_ = true;
    bool showProgress_ = true;
    bool showProgressInBar_ = true;
    bool showCenter_ = true;
    bool playGesture_ = false;
    bool progressGesture_ = true;
    bool tookInput_ = true;

    bool barShown_ = false;
    QTimer hideBar_;
    /// A click, waiting to tell whether it is half of a double click.
    QTimer click_;
    /// A drag across the picture, or along the bar's progress, is under way:
    /// where it would seek to.
    std::optional<double> seekTarget_;
    bool draggingSlider_ = false;
    /// The drag across the picture: where it began, and where the video was.
    std::optional<QPointF> pressedAt_;
    bool dragging_ = false;
    double dragFrom_ = 0;

    QToolButton *centerPlay_ = nullptr;
    QWidget *bar_ = nullptr;
    QToolButton *barPlay_ = nullptr;
    QLabel *time_ = nullptr;
    QSlider *progress_ = nullptr;
    QLabel *total_ = nullptr;
    QToolButton *fullScreen_ = nullptr;
    QLabel *dragTime_ = nullptr;
    /// What each glyph's button shows now, so it is drawn again only when it changes.
    bool barPlayShowsPause_ = false;
    bool fullScreenShowsExit_ = false;
};

/// `m:ss`, or `h:mm:ss` from an hour, as the other SDKs show a time.
[[nodiscard]] QString videoClock(double seconds);

}  // namespace migo::linux_host::qt6

#endif  // MIGO_LINUX_QT6_MEDIA_VIDEO_CONTROLS_HPP_

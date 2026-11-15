#ifndef MIGO_LINUX_QT6_MEDIA_PLAYBACK_HPP_
#define MIGO_LINUX_QT6_MEDIA_PLAYBACK_HPP_

// Internal to the media component (migo::qt6-media), not installed: the
// GStreamer under its players.

#include <gst/app/gstappsink.h>
#include <gst/gst.h>

#include <QByteArray>
#include <QImage>
#include <QObject>
#include <QSize>
#include <QString>

#include <functional>
#include <memory>

namespace migo::linux_host::qt6 {

/// A decoded frame: the image over the sample's memory, which stays mapped
/// until the image goes, and the size it is meant to be seen at.
struct Frame {
    QImage image;
    QSize shown;
};

/// The frame `sample` holds, which it takes; an empty one when it cannot be read.
[[nodiscard]] Frame frameOf(GstSample *sample);

/// The URI GStreamer reads `source` from: an http(s) or file URL as it is, a
/// path as a file URI.
[[nodiscard]] QByteArray uriOf(const QString &source);

/// Whether `source` is read over the network.
[[nodiscard]] bool isRemote(const QString &source);

/// A playbin and the sink its frames reach, stopped and released with it.
class Playbin final {
public:
    /// How it plays.
    struct Mode {
        /// With its sound.
        bool sound = true;
        /// Frames paced by the clock -- playing -- or as decoded -- describing.
        bool sync = true;
        /// A network source downloaded as it plays, into a file of GStreamer's
        /// own, so what has arrived can be told.
        bool download = false;
    };

    /// One playing `uri`; null when GStreamer lacks its base plugins.
    [[nodiscard]] static std::unique_ptr<Playbin> make(const QByteArray &uri, Mode mode);

    ~Playbin();
    Playbin(const Playbin &) = delete;
    Playbin &operator=(const Playbin &) = delete;

    [[nodiscard]] GstElement *pipeline() const noexcept { return pipeline_; }
    [[nodiscard]] GstAppSink *sink() const noexcept { return GST_APP_SINK(sink_); }

private:
    Playbin(GstElement *pipeline, GstElement *sink) : pipeline_(pipeline), sink_(sink) {}

    GstElement *pipeline_;
    GstElement *sink_;
};

/// One video GStreamer plays, driven and heard on the GUI thread.
///
/// It starts paused, decoding its first frame -- `settled` says when it has,
/// and when each seek has arrived. Its frames come one at a time: `frameReady`
/// says one is waiting, `takeFrame` takes the newest, and a player behind skips
/// the ones in between. What GStreamer reports on its own threads reaches the
/// GUI thread through a relay this cuts as it goes, so nothing it said arrives
/// after it.
class Playback final : public QObject {
public:
    /// What it tells, each on the GUI thread; any may be empty.
    struct Listener {
        /// A frame is waiting.
        std::function<void()> frameReady;
        /// Paused or playing at the place asked: after it first opened, and
        /// after a seek.
        std::function<void()> settled;
        /// It played to its end.
        std::function<void()> ended;
        /// It cannot play on, and why, in GStreamer's words.
        std::function<void(const QString &reason)> failed;
        /// A network source filling what it holds, in percent.
        std::function<void(int percent)> buffering;
        /// Its length is known, or changed.
        std::function<void()> durationChanged;
    };

    /// `uri` played as `mode` says; null when GStreamer lacks its base
    /// plugins. Already on its way to its first frame.
    [[nodiscard]] static std::unique_ptr<Playback> open(const QByteArray &uri, Playbin::Mode mode, Listener listener);
    ~Playback() override;

    Playback(const Playback &) = delete;
    Playback &operator=(const Playback &) = delete;

    /// The newest frame, taken; empty when none has come since the last.
    [[nodiscard]] Frame takeFrame();

    /// Plays, or pauses where it is. False when GStreamer refused at once.
    bool setPlaying(bool playing);
    /// Goes to `ns` from the start: exactly, or the nearest keyframe -- quick,
    /// for a place dragged through. `settled` follows once there.
    void seek(qint64 ns, bool exactly);
    /// How fast it plays, 1 being its own pace; kept by every seek after. A
    /// change seeks to where it is, or to where a seek on its way goes.
    void setRate(double rate);
    void setMuted(bool muted);

    /// Where it is, in nanoseconds; -1 before it knows.
    [[nodiscard]] qint64 position() const;
    /// How long it plays, in nanoseconds; -1 before it knows, or for a stream
    /// with no end.
    [[nodiscard]] qint64 duration() const;
    /// How much of it has arrived, in percent: all of a file, what has
    /// downloaded of a network source played with `download`; -1 when that
    /// cannot be told.
    [[nodiscard]] int buffered() const;

private:
    struct Relay;
    Playback(std::unique_ptr<Playbin> playbin, bool local, Listener listener);
    void listen();

    const std::shared_ptr<Relay> relay_;
    std::unique_ptr<Playbin> playbin_;
    const bool local_;
    Listener listener_;
    double rate_ = 1.0;
    /// Where the last seek goes, until it has arrived; -1 when none is on its way.
    qint64 target_ = -1;
};

}  // namespace migo::linux_host::qt6

#endif  // MIGO_LINUX_QT6_MEDIA_PLAYBACK_HPP_

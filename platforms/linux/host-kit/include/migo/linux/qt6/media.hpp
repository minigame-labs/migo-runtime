#ifndef MIGO_LINUX_QT6_MEDIA_HPP_
#define MIGO_LINUX_QT6_MEDIA_HPP_

#include <migo/linux/qt6/host_services.hpp>

#include <QImage>
#include <QString>

#include <memory>
#include <optional>

class QWidget;

namespace migo::linux_host::qt6 {

/// Videos and the microphone for the Host Kit's services, through GStreamer:
/// chooseMedia describes a picked video -- how long it plays, its size as
/// shown, its first frame as the cover -- previewMedia plays one, with its
/// sound, in the viewer, and createVideo's players play over the game. The
/// engine decodes video through GStreamer on Linux too.
///
/// createVideo's players (`players`) are each a window of their own over the
/// game -- an X window inside the game's, which the X server keeps above the
/// game's frames, and which goes, moves and hides with it -- painted with
/// GStreamer's frames, never the game's. As the Android SDK's and the Apple
/// SDK's, each shows its poster until it plays, fits its picture as objectFit
/// says on its backgroundColor, and has the controls content's options ask for:
/// a play button in the middle while it is not playing (showCenterPlayBtn); a
/// bar along the bottom -- play or pause, the time, a progress bar
/// (showProgressInControlMode), the length, full screen -- a click shows or
/// hides, and which hides again after three seconds of playing (controls); a
/// thin progress line while the bar is hidden (showProgress); a double click
/// that plays or pauses (enablePlayGesture); and a sideways drag that seeks
/// (enableProgressGesture). The buttons carry the names assistive technology
/// reads -- "play", "play or pause", "full screen" -- which are the other
/// SDKs' too. With none of them asked for, the video takes no input: a click
/// on it is the game's. Full screen fills the game's
/// view; a desktop's screen is not turned, so a direction is not either.
/// Its window is opaque: a backgroundColor's alpha is not shown through, nor is
/// a player under the game (underGameView) -- the other SDKs do not show one
/// either. A network poster is read up to 8 MiB, as the Apple SDK reads one.
///
/// The microphone (`recorder`) is the desktop's default input -- PipeWire's,
/// PulseAudio's or ALSA's, as GStreamer finds it -- converted to the rate and
/// channels content asks for, written as the Android and Apple SDKs write a
/// recording: PCM raw, wav behind its header, and AAC ("mp3" too: no system
/// encodes MP3) in an MPEG-4 file (.m4a), or as an ADTS stream (.aac) when
/// content asks for frames, which are then its bytes. AAC needs one of
/// GStreamer's AAC encoders: libav's (gstreamer1.0-libav), or FDK's, VisualOn's
/// or FAAC's. A recording ends on the sample its duration ends on; one paused
/// leaves no gap in its file. An App gives it as
/// `MigoManagedSession::Options::microphone`, or to `MigoQtBuiltInHostServices`.
///
/// Its own component, `migo::qt6-media`, since it links GStreamer, which the
/// Host Kit's core does not: an App that links it makes one -- shared, as
/// describing a video outlasts a call whose Session goes -- and gives it as
/// `MigoManagedSession::Options::videos`, or to `MigoQtBuiltInHostServices`.
///
/// Which videos play is GStreamer's plugins': its base and good plugins hold
/// the common containers, and libav (gstreamer1.0-libav) or the bad plugins
/// the codecs -- H.264 among them. A video it cannot play is described with
/// zeros and no cover, and shown as a warning sign.
///
/// A video turned as it was recorded is shown upright, as its tags say; a
/// stream of non-square pixels at the size it is meant to be seen at.
class MigoQtMedia final : public MigoQtVideoSupport, public MigoQtMicrophone {
public:
    /// Starts GStreamer, if no one has.
    MigoQtMedia();
    ~MigoQtMedia() override;

    MigoQtMedia(const MigoQtMedia &) = delete;
    MigoQtMedia &operator=(const MigoQtMedia &) = delete;

    /// Whether GStreamer started: without it, nothing is described or played.
    [[nodiscard]] bool isValid() const noexcept { return started_; }

    [[nodiscard]] std::optional<Description> describe(const QString &file) const override;
    [[nodiscard]] QWidget *player(const QString &source, const QImage &poster, QWidget &parent) const override;
    /// None without GStreamer.
    [[nodiscard]] std::unique_ptr<MigoQtVideoPlayers> players(QWidget &stage,
                                                              MigoQtVideoPlayers::Post post) const override;
    /// None without GStreamer.
    [[nodiscard]] std::unique_ptr<MigoQtRecorder> recorder(MigoQtRecorder::Post post, MigoQtRecorder::PostFrame frame,
                                                           const QString &staging) const override;

private:
    bool started_ = false;
};

}  // namespace migo::linux_host::qt6

#endif  // MIGO_LINUX_QT6_MEDIA_HPP_

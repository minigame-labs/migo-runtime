#ifndef MIGO_LINUX_QT6_MEDIA_HPP_
#define MIGO_LINUX_QT6_MEDIA_HPP_

#include <migo/linux/qt6/host_services.hpp>

#include <QImage>
#include <QString>

#include <optional>

class QWidget;

namespace migo::linux_host::qt6 {

/// Videos for the Host Kit's image service, described and played by GStreamer:
/// chooseMedia describes a picked video -- how long it plays, its size as
/// shown, its first frame as the cover -- and previewMedia plays one, with its
/// sound, in the viewer. The engine decodes video through GStreamer on Linux
/// too.
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
class MigoQtMedia final : public MigoQtVideoSupport {
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

private:
    bool started_ = false;
};

}  // namespace migo::linux_host::qt6

#endif  // MIGO_LINUX_QT6_MEDIA_HPP_

#ifndef MIGO_LINUX_QT6_IMAGES_HPP_
#define MIGO_LINUX_QT6_IMAGES_HPP_

#include <migo/linux/qt6/host_services.hpp>

#include <QJsonObject>
#include <QObject>
#include <QPointer>
#include <QSize>
#include <QString>
#include <QStringList>

#include <cstdint>
#include <functional>
#include <memory>

class QFileDialog;
class QWidget;

namespace migo::linux_host::qt6 {

/// The image service, answered with what Qt and the desktop offer every App:
/// compressing an image with Qt's own readers and writers; the file dialog --
/// the desktop's, through its portal, wherever Qt finds one -- to pick images
/// and files, and to save a file where the player chooses; and the player's
/// Pictures folder as the photo album.
///
/// A file a call answers with is the Host Kit's own copy, in a staging
/// directory of its own: the Session takes each file named as the call is
/// completed, and what it does not take -- the game that asked has gone -- is
/// removed then. A picked file is copied first, since it is the player's. The
/// staging directory is in the App's cache, usually on the volume of the
/// engine's `cache_dir`, so taking a file is a rename rather than a copy.
///
/// Not offered: the camera (a call that allows only it is refused, one that
/// allows the album too is shown the dialog); videos in chooseMedia, since
/// nothing here can say a video's length, size or cover; and the previews.
///
/// GUI thread only; decoding, encoding and copying run on Qt's thread pool and
/// answer from there.
class Images final : public QObject {
public:
    /// Where the service keeps and writes files.
    struct Places {
        /// The directory the staging directory is made in.
        QString staging;
        /// The photo album: where saveImageToPhotosAlbum writes.
        QString album;

        /// The desktop's: the App's cache and the player's Pictures folder.
        [[nodiscard]] static Places system();
    };

    /// `surface` is the game's view: the dialogs are its window's.
    Images(QWidget &surface, Places places);
    ~Images() override;

    void call(std::uint32_t method, const QJsonObject &request, MigoQtHostServiceReply reply);

    /// The most pixels one compression produces: the other platforms' ceiling,
    /// so the same call fails the same way everywhere.
    static constexpr std::int64_t kMaxPixels = 50'000'000;

    /// The size `width` x `height` asks of an image `source`: both when both
    /// are given, one with the aspect kept when only it is, the image's own
    /// when neither is. Zero or less is not given.
    [[nodiscard]] static QSize targetSize(QSize source, int width, int height);

    class Staging;

private:
    /// What the player picked, the originals, at most the count asked for.
    using Picked = std::function<void(QStringList files, MigoQtHostServiceReply reply)>;

    void compressImage(const QJsonObject &request, MigoQtHostServiceReply reply);
    void chooseImage(const QJsonObject &request, MigoQtHostServiceReply reply);
    void chooseMedia(const QJsonObject &request, MigoQtHostServiceReply reply);
    void chooseMessageFile(const QJsonObject &request, MigoQtHostServiceReply reply);
    void saveFileToDisk(const QJsonObject &request, MigoQtHostServiceReply reply);
    void saveImageToPhotosAlbum(const QJsonObject &request, MigoQtHostServiceReply reply);

    /// Shows the file dialog to open at most `count` files among `filters`
    /// (none: any file), starting in `directory` when given. A player who
    /// dismisses it is the failure "cancel".
    void pick(int count, const QStringList &filters, const QString &directory, MigoQtHostServiceReply reply,
              Picked picked);
    /// Whether a file dialog can be shown now; refuses `reply` if not.
    bool canShowDialog(MigoQtHostServiceReply &reply);
    void show(QFileDialog *dialog);

    /// The staging directory, made the first time a call needs it.
    std::shared_ptr<Staging> staging();

    QPointer<QWidget> surface_;
    Places places_;
    std::shared_ptr<Staging> staging_;
    /// The one file dialog up, if any.
    QPointer<QFileDialog> dialog_;
};

}  // namespace migo::linux_host::qt6

#endif  // MIGO_LINUX_QT6_IMAGES_HPP_

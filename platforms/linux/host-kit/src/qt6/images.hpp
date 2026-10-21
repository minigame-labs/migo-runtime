#ifndef MIGO_LINUX_QT6_IMAGES_HPP_
#define MIGO_LINUX_QT6_IMAGES_HPP_

#include <migo/linux/qt6/host_services.hpp>

#include <QJsonObject>
#include <QList>
#include <QObject>
#include <QPointer>
#include <QSize>
#include <QString>
#include <QStringList>

#include <cstdint>
#include <functional>
#include <memory>
#include <vector>

class QFileDialog;
class QIODevice;
class QNetworkAccessManager;
class QWidget;

namespace migo::linux_host::qt6 {

class ImageViewer;

/// The image service, answered with what Qt and the desktop offer every App:
/// compressing an image with Qt's own readers and writers; the file dialog --
/// the desktop's, through its portal, wherever Qt finds one -- to pick images
/// and files, and to save a file where the player chooses; the player's
/// Pictures folder as the photo album; and a viewer of the Host Kit's own for
/// the previews (`ImageViewer`).
///
/// A file a call answers with is the Host Kit's own copy, in a staging
/// directory of its own: the Session takes each file named as the call is
/// completed, and what it does not take -- the game that asked has gone -- is
/// removed then. A picked file is copied first, since it is the player's. The
/// staging directory is in the App's cache, usually on the volume of the
/// engine's `cache_dir`, so taking a file is a rename rather than a copy.
///
/// A preview's images are opened before the call is answered -- a file the
/// engine lends, which it may take back once the call is answered, is held
/// open rather than copied -- and an http(s) one, which the engine let through
/// as the session's network policy lets content download, is downloaded into
/// a file removed as it is made, read through the handle that stays open.
/// What the viewer shows goes when it closes; another preview replaces it.
///
/// Not offered: the camera (a call that allows only it is refused, one that
/// allows the album too is shown the dialog); and videos, in chooseMedia and in
/// previewMedia, since nothing here can say a video's length, size or cover,
/// nor play one.
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
    /// previewImage and previewMedia: `sources` -- real paths and http(s)
    /// URLs -- from the one at `current`.
    void preview(const QStringList &sources, int current, MigoQtHostServiceReply reply);
    /// Shows `images`, each open, from the one at `current`, and answers once
    /// shown.
    void showPreview(std::vector<std::unique_ptr<QIODevice>> images, int current, MigoQtHostServiceReply reply);

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
    /// What downloads a preview's images, made the first time one is.
    QNetworkAccessManager &network();

    class PreviewLoad;

    QPointer<QWidget> surface_;
    Places places_;
    std::shared_ptr<Staging> staging_;
    /// The one file dialog up, if any.
    QPointer<QFileDialog> dialog_;
    /// The preview shown, if any.
    QPointer<ImageViewer> viewer_;
    /// The previews still opening and downloading their images.
    QList<QPointer<QObject>> loads_;
    QNetworkAccessManager *network_ = nullptr;
};

}  // namespace migo::linux_host::qt6

#endif  // MIGO_LINUX_QT6_IMAGES_HPP_

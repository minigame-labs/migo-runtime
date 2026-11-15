#ifndef MIGO_LINUX_QT6_MEDIA_VIEWER_HPP_
#define MIGO_LINUX_QT6_MEDIA_VIEWER_HPP_

#include <migo/linux/qt6/host_services.hpp>

#include <QDialog>
#include <QIODevice>
#include <QPointer>
#include <QString>

#include <memory>
#include <vector>

class QHBoxLayout;
class QKeyEvent;
class QLabel;
class QPushButton;
class QResizeEvent;
class QShowEvent;

namespace migo::linux_host::qt6 {

/// previewImage's and previewMedia's viewer: the images and videos one at a
/// time, fitted to it, from the one content asked for. Left and Right, or the
/// buttons either side, go between them; Esc or the close button closes it.
/// Window-modal over the game, its window's size. An image that cannot be
/// decoded is shown as a warning sign in its place; a video is played by the
/// App's video support (`MigoQtVideoSupport::player`), its poster shown until
/// it plays.
///
/// It owns what it shows -- each file open, so the engine may take back the
/// file it lent once the call is answered -- and that goes with it. Nothing on
/// it is in words, so there is nothing to translate.
class MediaViewer final : public QDialog {
public:
    /// One thing shown.
    struct Item {
        /// An image's bytes, or a video file held open; null for a video
        /// played from its URL.
        std::unique_ptr<QIODevice> file;
        /// A video's: its http(s) URL, or empty when it is `file`.
        QString url;
        bool video = false;
        /// A video's poster's bytes, if it has one.
        std::unique_ptr<QIODevice> poster;
    };

    /// `items` each hold what they show, open; `current` is the one shown
    /// first. `videos` plays the videos, which there are only where it is given.
    MediaViewer(QWidget &window, std::vector<Item> items, int current,
                std::shared_ptr<const MigoQtVideoSupport> videos);
    ~MediaViewer() override;

    /// The item shown, from 0.
    [[nodiscard]] int current() const noexcept { return current_; }
    [[nodiscard]] int count() const noexcept { return static_cast<int>(items_.size()); }
    /// Whether the item shown is an image that could be decoded.
    [[nodiscard]] bool showsImage() const noexcept { return decoded_; }
    /// The player of the video shown, or null when an image is.
    [[nodiscard]] QWidget *player() const noexcept { return player_.data(); }

    void showPrevious();
    void showNext();

protected:
    void keyPressEvent(QKeyEvent *event) override;
    void resizeEvent(QResizeEvent *event) override;
    void showEvent(QShowEvent *event) override;

private:
    void present(int index);
    /// Draws the image shown at the size it now has room for, once shown.
    void render();

    std::vector<Item> items_;
    const std::shared_ptr<const MigoQtVideoSupport> videos_;
    int current_ = 0;
    bool decoded_ = false;
    QHBoxLayout *row_ = nullptr;
    QLabel *picture_ = nullptr;
    QPointer<QWidget> player_;
    QLabel *position_ = nullptr;
    QPushButton *previous_ = nullptr;
    QPushButton *next_ = nullptr;
};

}  // namespace migo::linux_host::qt6

#endif  // MIGO_LINUX_QT6_MEDIA_VIEWER_HPP_

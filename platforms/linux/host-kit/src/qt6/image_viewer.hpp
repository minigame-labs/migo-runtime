#ifndef MIGO_LINUX_QT6_IMAGE_VIEWER_HPP_
#define MIGO_LINUX_QT6_IMAGE_VIEWER_HPP_

#include <QDialog>
#include <QIODevice>

#include <memory>
#include <vector>

class QKeyEvent;
class QLabel;
class QPushButton;
class QResizeEvent;
class QShowEvent;

namespace migo::linux_host::qt6 {

/// previewImage's and previewMedia's viewer: the images one at a time, fitted
/// to it, from the one content asked for. Left and Right, or the buttons either
/// side, go between them; Esc or the close button closes it. Window-modal over
/// the game, its window's size. An image that cannot be decoded is shown as a
/// warning sign in its place.
///
/// It owns what it shows -- each image open, so the engine may take back the
/// file it lent once the call is answered -- and that goes with it. Nothing on
/// it is in words, so there is nothing to translate.
class ImageViewer final : public QDialog {
public:
    /// `images` holds each image's bytes, open for reading; `current` is the
    /// one shown first.
    ImageViewer(QWidget &window, std::vector<std::unique_ptr<QIODevice>> images, int current);
    ~ImageViewer() override;

    /// The image shown, from 0.
    [[nodiscard]] int current() const noexcept { return current_; }
    [[nodiscard]] int count() const noexcept { return static_cast<int>(images_.size()); }
    /// Whether the image shown could be decoded.
    [[nodiscard]] bool showsImage() const noexcept { return decoded_; }

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

    std::vector<std::unique_ptr<QIODevice>> images_;
    int current_ = 0;
    bool decoded_ = false;
    QLabel *picture_ = nullptr;
    QLabel *position_ = nullptr;
    QPushButton *previous_ = nullptr;
    QPushButton *next_ = nullptr;
};

}  // namespace migo::linux_host::qt6

#endif  // MIGO_LINUX_QT6_IMAGE_VIEWER_HPP_

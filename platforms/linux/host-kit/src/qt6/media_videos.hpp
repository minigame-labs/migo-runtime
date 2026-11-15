#ifndef MIGO_LINUX_QT6_MEDIA_VIDEOS_HPP_
#define MIGO_LINUX_QT6_MEDIA_VIDEOS_HPP_

// Internal to the media component (migo::qt6-media), not installed.

#include <migo/linux/qt6/host_services.hpp>

#include <QHash>
#include <QObject>
#include <QPointer>

#include <cstdint>

class QNetworkAccessManager;
class QWidget;

namespace migo::linux_host::qt6 {

class VideoPlayer;

/// createVideo's players over one game's view, played by GStreamer
/// (`MigoQtMedia::players`, documented there). GUI thread.
class VideoPlayers final : public QObject, public MigoQtVideoPlayers {
public:
    VideoPlayers(QWidget &stage, Post post);
    ~VideoPlayers() override;

    void call(std::uint32_t method, const QJsonObject &request, MigoQtHostServiceReply reply) override;
    void setRunning(bool running) override;

    /// What downloads the players' posters, made the first time one does.
    QNetworkAccessManager &network();

protected:
    bool eventFilter(QObject *watched, QEvent *event) override;

private:
    QPointer<QWidget> stage_;
    const Post post_;
    QHash<qint64, QPointer<VideoPlayer>> players_;
    bool running_ = true;
    QNetworkAccessManager *network_ = nullptr;
};

}  // namespace migo::linux_host::qt6

#endif  // MIGO_LINUX_QT6_MEDIA_VIDEOS_HPP_

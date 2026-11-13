#include "media_videos.hpp"

#include "media_playback.hpp"
#include "media_video_controls.hpp"

#include <QBuffer>
#include <QColor>
#include <QCoreApplication>
#include <QEvent>
#include <QFile>
#include <QImageReader>
#include <QJsonValue>
#include <QMetaObject>
#include <QNetworkAccessManager>
#include <QNetworkReply>
#include <QNetworkRequest>
#include <QPaintEvent>
#include <QPainter>
#include <QResizeEvent>
#include <QShowEvent>
#include <QThreadPool>
#include <QTimer>
#include <QUrl>
#include <QWidget>
#include <QWindow>

#include <cmath>
#include <optional>
#include <utility>
#include <vector>

namespace migo::linux_host::qt6 {

namespace {

/// How often a playing video tells content where it is, as the other SDKs do.
constexpr int kTimeUpdateMs = 250;
/// The largest poster read, from a file or the network: the Apple SDK's.
constexpr qint64 kMaxPosterBytes = 8 << 20;
/// How long a poster's download may go without a byte.
constexpr int kPosterTimeoutMs = 10'000;

const QString kDestroyed = QStringLiteral("video destroyed");

/// `#RRGGBB` or `#AARRGGBB`, as the other SDKs read backgroundColor; none
/// otherwise.
std::optional<QColor> colorOf(const QString &text) {
    if (!text.startsWith(QLatin1Char('#')) || (text.size() != 7 && text.size() != 9)) return std::nullopt;
    bool ok = false;
    const quint64 value = text.mid(1).toULongLong(&ok, 16);
    if (!ok) return std::nullopt;
    const auto channel = [value](int shift) { return static_cast<int>((value >> shift) & 0xFF); };
    return QColor(channel(16), channel(8), channel(0), text.size() == 9 ? channel(24) : 255);
}

/// The image `bytes` hold, upright; null when they are not one.
QImage decodePoster(const QByteArray &bytes) {
    if (bytes.isEmpty() || bytes.size() > kMaxPosterBytes) return {};
    QBuffer buffer;
    buffer.setData(bytes);
    if (!buffer.open(QIODevice::ReadOnly)) return {};
    QImageReader reader(&buffer);
    reader.setDecideFormatFromContent(true);
    reader.setAutoTransform(true);
    return reader.read();
}

}  // namespace

/// One of createVideo's players: a native window of its own in the game's
/// view, where content put it. GUI thread.
class VideoPlayer final : public QWidget, public VideoControlled {
public:
    VideoPlayer(qint64 id, QJsonObject options, QWidget &stage, VideoPlayers &owner, MigoQtVideoPlayers::Post post)
        : QWidget(&stage), id_(id), options_(std::move(options)), owner_(owner), post_(std::move(post)) {
        setObjectName(QStringLiteral("migoVideoPlayer"));
        // A window of its own: the game is drawn in the view's, which a
        // widget painted with the view's could not stand above. The X server
        // keeps a window inside the view's above the game's frames, and cuts
        // it to the view.
        setAttribute(Qt::WA_NativeWindow);
        setAttribute(Qt::WA_DontCreateNativeAncestors);
        // It paints every pixel of itself.
        setAttribute(Qt::WA_OpaquePaintEvent);
        setAttribute(Qt::WA_NoSystemBackground);
        // The keyboard is the game's.
        setFocusPolicy(Qt::NoFocus);
        startAt_ = number("initialTime", 0);
        playWanted_ = flag("autoplay", false);
        controls_ = new VideoControls(*this, *this);
        controls_->configure(options_);
        ticker_.setInterval(kTimeUpdateMs);
        QObject::connect(&ticker_, &QTimer::timeout, this, [this] { tick(); });
        place();
        (void)winId();
        show();
        raise();
        loadPoster();
        openSource();
    }

    ~VideoPlayer() override {
        // Gone with the game's view: its calls still unanswered fail.
        release(kDestroyed);
    }

    VideoPlayer(const VideoPlayer &) = delete;
    VideoPlayer &operator=(const VideoPlayer &) = delete;

    // ---- calls ----------------------------------------------------------------------------------

    void play(MigoQtHostServiceReply reply) {
        pendingPlays_.push_back(std::move(reply));
        if (playback_ == nullptr || !broken_.isEmpty()) {
            answerPlays(string("src").isEmpty() ? QStringLiteral("no src")
                        : broken_.isEmpty()      ? QStringLiteral("the src cannot be played")
                                                 : broken_);
            return;
        }
        playWanted_ = true;
        if (playing_) {
            answerPlays(std::nullopt);
        } else if (prepared_ && !suspended_) {
            start();
        }
    }

    void pause(MigoQtHostServiceReply reply) {
        // A play waiting for the player never plays: it is overtaken.
        playWanted_ = false;
        answerPlays(QStringLiteral("paused before it played"));
        pauseNow();
        (void)reply.succeed();
    }

    void stop(MigoQtHostServiceReply reply) {
        playWanted_ = false;
        answerPlays(QStringLiteral("stopped before it played"));
        halt();
        resumeAfterSuspend_ = false;
        if (prepared_ && broken_.isEmpty()) {
            seekPlayback(0);
        } else {
            startAt_ = 0;
        }
        controls_->refresh();
        (void)reply.succeed();
    }

    void seek(double seconds, MigoQtHostServiceReply reply) {
        pendingSeeks_.push_back(std::move(reply));
        if (playback_ == nullptr || !broken_.isEmpty()) {
            answerSeeks(string("src").isEmpty() ? QStringLiteral("no src") : QStringLiteral("the src cannot be played"));
        } else if (prepared_) {
            seekPlayback(seconds);
        } else {
            startAt_ = seconds;
        }
    }

    void requestFullScreen(MigoQtHostServiceReply reply) {
        fullScreen_ = true;
        raise();
        place();
        controls_->refresh();
        answerOnceLaidOut(std::move(reply));
    }

    void exitFullScreen(MigoQtHostServiceReply reply) {
        fullScreen_ = false;
        place();
        controls_->refresh();
        answerOnceLaidOut(std::move(reply));
    }

    /// What content changed.
    void set(const QJsonObject &changed) {
        if (released_) return;
        for (auto it = changed.begin(); it != changed.end(); ++it) {
            if (it.key() != QLatin1String("videoId")) options_.insert(it.key(), it.value());
        }
        const auto has = [&changed](const char *key) { return changed.contains(QLatin1String(key)); };
        if (has("x") || has("y") || has("width") || has("height")) place();
        if (has("objectFit") || has("backgroundColor")) update();
        if (has("muted") && playback_ != nullptr) playback_->setMuted(flag("muted", false));
        if (has("playbackRate") && playing_ && playback_ != nullptr) playback_->setRate(rate());
        for (const QString &key : VideoControls::kOptions) {
            if (changed.contains(key)) {
                controls_->configure(options_);
                break;
            }
        }
        if (has("poster")) loadPoster();
        if (has("src")) {
            playWanted_ = false;
            failPending(QStringLiteral("src changed"));
            startAt_ = number("initialTime", 0);
            openSource();
        }
    }

    // ---- the game ---------------------------------------------------------------------------------

    void suspend() {
        suspended_ = true;
        if (!playing_) return;
        halt();
        resumeAfterSuspend_ = true;
        controls_->refresh();
    }

    void resume() {
        suspended_ = false;
        if (released_ || !(resumeAfterSuspend_ || playWanted_)) return;
        resumeAfterSuspend_ = false;
        if (prepared_ && !playing_ && broken_.isEmpty()) start();
    }

    /// Gone: its calls still unanswered fail with `reason`.
    void release(const QString &reason) {
        if (released_) return;
        failPending(reason);
        answerScreens(reason);
        released_ = true;
        ++posterGeneration_;
        abortPosterDownload();
        ticker_.stop();
        playback_.reset();
        hide();
    }

    /// Where content put it, or the whole of the game's view when full screen:
    /// a desktop's screen does not turn, so neither does the video.
    void place() {
        if (released_ || parentWidget() == nullptr) return;
        if (fullScreen_) {
            setGeometry(parentWidget()->rect());
            return;
        }
        const double x = number("x", 0);
        const double y = number("y", 0);
        const int left = static_cast<int>(std::lround(x));
        const int top = static_cast<int>(std::lround(y));
        setGeometry(left, top, static_cast<int>(std::lround(x + number("width", 300))) - left,
                    static_cast<int>(std::lround(y + number("height", 150))) - top);
    }

    // ---- what the controls drive ------------------------------------------------------------------

    [[nodiscard]] bool isPlaying() const override { return playing_; }

    [[nodiscard]] double position() const override {
        if (!prepared_ || playback_ == nullptr) return 0;
        if (ended_) return duration();
        const qint64 at = playback_->position();
        return at > 0 ? static_cast<double>(at) / GST_SECOND : 0;
    }

    [[nodiscard]] double duration() const override {
        if (!prepared_ || playback_ == nullptr) return 0;
        const qint64 length = playback_->duration();
        return length > 0 ? static_cast<double>(length) / GST_SECOND : 0;
    }

    [[nodiscard]] bool isFullScreen() const override { return fullScreen_; }

    void togglePlay() override {
        if (released_ || playback_ == nullptr || !broken_.isEmpty()) return;
        if (playing_) {
            playWanted_ = false;
            pauseNow();
        } else {
            playWanted_ = true;
            if (prepared_ && !suspended_) start();
        }
    }

    void seekTo(double seconds) override {
        if (released_ || !prepared_ || playback_ == nullptr || !broken_.isEmpty()) return;
        seekPlayback(seconds);
        controls_->refresh();
    }

    void toggleFullScreen() override {
        if (released_) return;
        fullScreen_ = !fullScreen_;
        if (fullScreen_) raise();
        place();
        controls_->refresh();
    }

    void inputChanged() override { applyInput(); }

protected:
    void paintEvent(QPaintEvent *) override {
        QPainter painter(this);
        // Opaque: a translucent background is drawn over black, as nothing
        // shows through the window.
        QColor background = colorOf(string("backgroundColor")).value_or(Qt::black);
        background = QColor::fromRgbF(background.redF() * background.alphaF(), background.greenF() * background.alphaF(),
                                      background.blueF() * background.alphaF());
        painter.fillRect(rect(), background);
        painter.setRenderHint(QPainter::SmoothPixmapTransform);
        // Its poster stands until it plays.
        if (!poster_.isNull() && !played_) {
            QRect where({}, poster_.size().scaled(size(), Qt::KeepAspectRatio));
            where.moveCenter(rect().center());
            painter.drawImage(where, poster_);
            return;
        }
        if (frame_.image.isNull()) return;
        const QString fit = string("objectFit");
        QRect where;
        if (fit == QLatin1String("fill")) {
            where = rect();
        } else {
            where = QRect({}, frame_.shown.scaled(size(), fit == QLatin1String("cover") ? Qt::KeepAspectRatioByExpanding
                                                                                      : Qt::KeepAspectRatio));
            where.moveCenter(rect().center());
        }
        painter.drawImage(where, frame_.image);
    }

    void resizeEvent(QResizeEvent *event) override {
        QWidget::resizeEvent(event);
        controls_->setGeometry(rect());
    }

    void showEvent(QShowEvent *event) override {
        QWidget::showEvent(event);
        applyInput();
    }

private:
    // ---- the source -------------------------------------------------------------------------------

    void openSource() {
        releasePlayback();
        played_ = false;
        const QString src = string("src");
        if (src.isEmpty()) return;
        const QString scheme = QUrl(src).scheme().toLower();
        if (!src.startsWith(QLatin1Char('/')) && !isRemote(src) && scheme != QLatin1String("file")) {
            report(QStringLiteral("the src is not a file or a URL"));
            return;
        }
        Playback::Listener listener;
        listener.frameReady = [this] { takeFrame(); };
        listener.settled = [this] { settled(); };
        listener.ended = [this] { ended(); };
        listener.failed = [this](const QString &reason) { report(reason); };
        listener.buffering = [this](int percent) { buffering(percent); };
        listener.durationChanged = [this] { controls_->refresh(); };
        // A network source is downloaded as it plays, so what has arrived can be told.
        playback_ = Playback::open(uriOf(src), {true, true, isRemote(src)}, std::move(listener));
        if (playback_ == nullptr) {
            report(QStringLiteral("GStreamer's base plugins are missing"));
            return;
        }
        playback_->setMuted(flag("muted", false));
    }

    void releasePlayback() {
        ticker_.stop();
        playback_.reset();
        frame_ = {};
        prepared_ = false;
        playing_ = false;
        ended_ = false;
        waiting_ = false;
        lastBuffered_ = -1;
        broken_.clear();
        update();
    }

    void takeFrame() {
        Frame frame = playback_->takeFrame();
        if (frame.image.isNull()) return;
        frame_ = std::move(frame);
        update();
    }

    /// At its first frame, or where a seek asked.
    void settled() {
        if (released_ || !broken_.isEmpty()) return;
        if (!prepared_) {
            prepared_ = true;
            if (startAt_ > 0) {
                seekPlayback(startAt_);
                startAt_ = 0;
            } else {
                answerSeeks(std::nullopt);
            }
            if (playWanted_ && !suspended_) start();
            reportBuffered();
        } else {
            // A seek a later one overtakes arrives where the later one asked:
            // the later one's answer is the answer to both.
            answerSeeks(std::nullopt);
        }
        controls_->refresh();
    }

    void start() {
        if (playback_ == nullptr) return;
        // Played again from the start once it has ended.
        if (ended_) seekPlayback(0);
        playback_->setRate(rate());
        playback_->setPlaying(true);
        playing_ = true;
        played_ = true;
        waiting_ = false;
        answerPlays(std::nullopt);
        tell(MIGO_VIDEO_EVENT_PLAY);
        ticker_.start();
        update();
        controls_->refresh();
    }

    /// Paused, as content's call or the player's own control does.
    void pauseNow() {
        if (playing_) {
            halt();
            tell(MIGO_VIDEO_EVENT_PAUSE);
        }
        resumeAfterSuspend_ = false;
        controls_->refresh();
    }

    /// Not playing, without telling content.
    void halt() {
        if (playing_ && playback_ != nullptr) playback_->setPlaying(false);
        playing_ = false;
        waiting_ = false;
        ticker_.stop();
    }

    void seekPlayback(double seconds) {
        ended_ = false;
        playback_->seek(static_cast<qint64>(std::llround(qMax(0.0, seconds) * GST_SECOND)), true);
    }

    void ended() {
        if (released_ || playback_ == nullptr) return;
        if (flag("loop", false)) {
            // Still playing, from the start.
            seekPlayback(0);
            return;
        }
        halt();
        playWanted_ = false;
        ended_ = true;
        tell(MIGO_VIDEO_EVENT_ENDED);
        controls_->refresh();
    }

    /// A network source filling what it holds: held, and content told it
    /// waits, until it is full again.
    void buffering(int percent) {
        if (released_ || !playing_ || playback_ == nullptr) return;
        const bool filling = percent < 100;
        if (filling != waiting_) {
            waiting_ = filling;
            if (filling) tell(MIGO_VIDEO_EVENT_WAITING);
            playback_->setPlaying(!filling);
        }
        reportBuffered();
    }

    void tick() {
        if (released_ || !playing_) return;
        tell(MIGO_VIDEO_EVENT_TIME_UPDATE, {{QStringLiteral("position"), position()},
                                            {QStringLiteral("duration"), duration()}});
        reportBuffered();
        controls_->refresh();
    }

    void reportBuffered() {
        if (!prepared_ || playback_ == nullptr) return;
        const double total = duration();
        const int buffered = playback_->buffered();
        if (total <= 0 || buffered < 0 || buffered == lastBuffered_) return;
        lastBuffered_ = buffered;
        tell(MIGO_VIDEO_EVENT_PROGRESS, {{QStringLiteral("buffered"), buffered}, {QStringLiteral("duration"), total}});
    }

    /// It cannot play this source: content is told once, and its calls fail.
    void report(const QString &reason) {
        if (released_ || !broken_.isEmpty()) return;
        broken_ = reason;
        failPending(reason);
        halt();
        playWanted_ = false;
        tell(MIGO_VIDEO_EVENT_ERROR, {{QStringLiteral("errMsg"), QStringLiteral("video:fail %1").arg(reason)}});
        controls_->refresh();
    }

    // ---- the poster -------------------------------------------------------------------------------

    void loadPoster() {
        const int generation = ++posterGeneration_;
        abortPosterDownload();
        poster_ = QImage();
        update();
        const QString poster = string("poster");
        if (poster.isEmpty()) return;
        if (isRemote(poster)) {
            download(QUrl(poster), generation);
            return;
        }
        const QString path = QUrl(poster).isLocalFile() ? QUrl(poster).toLocalFile() : poster;
        if (!path.startsWith(QLatin1Char('/'))) return;
        QThreadPool::globalInstance()->start([path, generation, guard = QPointer<VideoPlayer>(this)] {
            QByteArray bytes;
            QFile file(path);
            if (file.open(QIODevice::ReadOnly) && file.size() <= kMaxPosterBytes) bytes = file.readAll();
            showPosterLater(guard, generation, decodePoster(bytes));
        });
    }

    /// A poster from the network, read only up to the most a poster may be: a
    /// response that says it is longer, or turns out to be, is abandoned.
    void download(const QUrl &url, int generation) {
        QNetworkRequest request(url);
        request.setTransferTimeout(kPosterTimeoutMs);
        QNetworkReply *reply = owner_.network().get(request);
        posterDownload_ = reply;
        auto received = std::make_shared<QByteArray>();
        QObject::connect(reply, &QNetworkReply::metaDataChanged, this, [reply] {
            const QVariant length = reply->header(QNetworkRequest::ContentLengthHeader);
            if (length.isValid() && length.toLongLong() > kMaxPosterBytes) reply->abort();
        });
        QObject::connect(reply, &QIODevice::readyRead, this, [reply, received] {
            received->append(reply->readAll());
            if (received->size() > kMaxPosterBytes) reply->abort();
        });
        QObject::connect(reply, &QNetworkReply::finished, this, [this, reply, received, generation] {
            reply->deleteLater();
            if (posterDownload_ == reply) posterDownload_ = nullptr;
            const int status = reply->attribute(QNetworkRequest::HttpStatusCodeAttribute).toInt();
            if (reply->error() != QNetworkReply::NoError || status < 200 || status >= 300) return;
            received->append(reply->readAll());
            QThreadPool::globalInstance()->start([bytes = *received, generation, guard = QPointer<VideoPlayer>(this)] {
                showPosterLater(guard, generation, decodePoster(bytes));
            });
        });
    }

    void abortPosterDownload() {
        if (posterDownload_.isNull()) return;
        QNetworkReply *reply = posterDownload_;
        posterDownload_ = nullptr;
        reply->disconnect(this);
        reply->abort();
        reply->deleteLater();
    }

    /// From any thread: `image` is the poster of `generation`, shown on the GUI
    /// thread if that is still the one the player wants.
    static void showPosterLater(QPointer<VideoPlayer> guard, int generation, QImage image) {
        QMetaObject::invokeMethod(
            QCoreApplication::instance(),
            [guard, generation, image = std::move(image)] {
                if (guard.isNull() || guard->released_ || generation != guard->posterGeneration_ || image.isNull()) {
                    return;
                }
                guard->poster_ = image;
                guard->update();
            },
            Qt::QueuedConnection);
    }

    // ---- answers and events -----------------------------------------------------------------------

    /// Done when `failure` is empty, failed with it otherwise.
    static void answer(std::vector<MigoQtHostServiceReply> &replies, const std::optional<QString> &failure) {
        std::vector<MigoQtHostServiceReply> taken = std::move(replies);
        replies.clear();
        for (MigoQtHostServiceReply &reply : taken) {
            if (failure.has_value()) {
                (void)reply.fail(*failure);
            } else {
                (void)reply.succeed();
            }
        }
    }

    void answerPlays(const std::optional<QString> &failure) { answer(pendingPlays_, failure); }
    void answerSeeks(const std::optional<QString> &failure) { answer(pendingSeeks_, failure); }
    void answerScreens(const std::optional<QString> &failure) { answer(pendingScreens_, failure); }

    void failPending(const QString &reason) {
        answerPlays(reason);
        answerSeeks(reason);
    }

    /// Answered once laid out at its new size.
    void answerOnceLaidOut(MigoQtHostServiceReply reply) {
        pendingScreens_.push_back(std::move(reply));
        QTimer::singleShot(0, this, [this] { answerScreens(std::nullopt); });
    }

    void tell(std::uint32_t event, QJsonObject payload = {}) {
        if (released_) return;
        payload.insert(QStringLiteral("videoId"), id_);
        post_(event, payload);
    }

    /// With none of its controls asked for, a click on the video is the game's:
    /// its window takes no input -- an empty input shape, which the X server
    /// passes through to the game's window -- and Qt, finding the widget under
    /// a click in the game's view, passes over it.
    void applyInput() {
        // The controls ask before they are the player's.
        if (controls_ == nullptr) return;
        const bool passes = !controls_->takesInput();
        setAttribute(Qt::WA_TransparentForMouseEvents, passes);
        if (QWindow *window = windowHandle(); window != nullptr) {
            window->setFlag(Qt::WindowTransparentForInput, passes);
        }
    }

    // ---- options ----------------------------------------------------------------------------------

    [[nodiscard]] double rate() const {
        const double value = number("playbackRate", 1);
        return value > 0 ? value : 1;
    }

    [[nodiscard]] QString string(const char *key) const { return options_.value(QLatin1String(key)).toString(); }

    [[nodiscard]] double number(const char *key, double fallback) const {
        const QJsonValue value = options_.value(QLatin1String(key));
        return value.isDouble() && std::isfinite(value.toDouble()) ? value.toDouble() : fallback;
    }

    [[nodiscard]] bool flag(const char *key, bool fallback) const {
        const QJsonValue value = options_.value(QLatin1String(key));
        return value.isBool() ? value.toBool() : fallback;
    }

    const qint64 id_;
    QJsonObject options_;
    VideoPlayers &owner_;
    const MigoQtVideoPlayers::Post post_;
    VideoControls *controls_ = nullptr;
    std::unique_ptr<Playback> playback_;
    QTimer ticker_;
    Frame frame_;
    QImage poster_;
    int posterGeneration_ = 0;
    QPointer<QNetworkReply> posterDownload_;

    /// Calls waiting to be answered: plays until it plays, seeks until it is
    /// there, full screen until laid out.
    std::vector<MigoQtHostServiceReply> pendingPlays_;
    std::vector<MigoQtHostServiceReply> pendingSeeks_;
    std::vector<MigoQtHostServiceReply> pendingScreens_;
    /// At its first frame: it can play and seek.
    bool prepared_ = false;
    /// What content was last told: playing since its last PLAY.
    bool playing_ = false;
    /// Played since its source opened: its poster no longer stands.
    bool played_ = false;
    /// Content, or the player's own controls, asked it to play.
    bool playWanted_ = false;
    /// Where to go once prepared: initialTime, or a seek asked before.
    double startAt_ = 0;
    bool ended_ = false;
    bool suspended_ = false;
    bool resumeAfterSuspend_ = false;
    bool waiting_ = false;
    int lastBuffered_ = -1;
    bool fullScreen_ = false;
    bool released_ = false;
    /// Why this source cannot play, once it has failed.
    QString broken_;
};

// ---- the players ---------------------------------------------------------------------------------

VideoPlayers::VideoPlayers(QWidget &stage, Post post) : stage_(&stage), post_(std::move(post)) {
    // A full-screen player fills the view as it changes size.
    stage.installEventFilter(this);
}

VideoPlayers::~VideoPlayers() {
    // The game ended: every player goes, and its calls still unanswered fail.
    for (const QPointer<VideoPlayer> &player : std::as_const(players_)) {
        if (player.isNull()) continue;
        player->release(kDestroyed);
        delete player.data();
    }
}

QNetworkAccessManager &VideoPlayers::network() {
    if (network_ == nullptr) network_ = new QNetworkAccessManager(this);
    return *network_;
}

void VideoPlayers::call(std::uint32_t method, const QJsonObject &request, MigoQtHostServiceReply reply) {
    const qint64 id = request.value(QStringLiteral("videoId")).toInteger();
    switch (method) {
        case MIGO_VIDEO_CREATE: {
            if (stage_.isNull() || !players_.value(id).isNull()) return;
            auto *player = new VideoPlayer(id, request, *stage_, *this, post_);
            players_.insert(id, player);
            if (!running_) player->suspend();
            return;
        }
        case MIGO_VIDEO_DESTROY:
            if (const QPointer<VideoPlayer> player = players_.take(id); !player.isNull()) {
                player->release(kDestroyed);
                player->deleteLater();
            }
            return;
        case MIGO_VIDEO_SET:
            if (const QPointer<VideoPlayer> player = players_.value(id); !player.isNull()) player->set(request);
            return;
        default:
            break;
    }
    const QPointer<VideoPlayer> player = players_.value(id);
    if (player.isNull()) {
        (void)reply.fail(QStringLiteral("video not found"));
        return;
    }
    switch (method) {
        case MIGO_VIDEO_PLAY:
            player->play(std::move(reply));
            return;
        case MIGO_VIDEO_PAUSE:
            player->pause(std::move(reply));
            return;
        case MIGO_VIDEO_STOP:
            player->stop(std::move(reply));
            return;
        case MIGO_VIDEO_SEEK:
            player->seek(request.value(QStringLiteral("time")).toDouble(0), std::move(reply));
            return;
        case MIGO_VIDEO_REQUEST_FULL_SCREEN:
            player->requestFullScreen(std::move(reply));
            return;
        case MIGO_VIDEO_EXIT_FULL_SCREEN:
            player->exitFullScreen(std::move(reply));
            return;
        default:
            (void)reply.fail(QStringLiteral("not supported"));
            return;
    }
}

void VideoPlayers::setRunning(bool running) {
    if (running == running_) return;
    running_ = running;
    for (const QPointer<VideoPlayer> &player : std::as_const(players_)) {
        if (player.isNull()) continue;
        if (running) {
            player->resume();
        } else {
            player->suspend();
        }
    }
}

bool VideoPlayers::eventFilter(QObject *watched, QEvent *event) {
    if (watched == stage_.data() && event->type() == QEvent::Resize) {
        for (const QPointer<VideoPlayer> &player : std::as_const(players_)) {
            if (!player.isNull() && player->isFullScreen()) player->place();
        }
    }
    return QObject::eventFilter(watched, event);
}

}  // namespace migo::linux_host::qt6

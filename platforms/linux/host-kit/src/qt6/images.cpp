#include "images.hpp"

#include "media_viewer.hpp"

#include <QByteArray>
#include <QDateTime>
#include <QDir>
#include <QFile>
#include <QFileDialog>
#include <QFileInfo>
#include <QHash>
#include <QImage>
#include <QImageIOHandler>
#include <QImageReader>
#include <QImageWriter>
#include <QJsonArray>
#include <QJsonValue>
#include <QMimeDatabase>
#include <QMimeType>
#include <QNetworkAccessManager>
#include <QNetworkReply>
#include <QNetworkRequest>
#include <QSaveFile>
#include <QSet>
#include <QStandardPaths>
#include <QTemporaryDir>
#include <QTemporaryFile>
#include <QThreadPool>
#include <QUrl>
#include <QUuid>
#include <QVariant>
#include <QWidget>

#include <algorithm>
#include <cmath>
#include <limits>
#include <optional>
#include <utility>

namespace migo::linux_host::qt6 {

// ---- the staging directory -----------------------------------------------------------------

/// The Host Kit's own files until the Session takes them. Shared with the work
/// in flight, so the directory lasts as long as the last of it.
class Images::Staging final {
public:
    explicit Staging(const QString &root) : dir_(make(root)) {}

    /// A new file's path, with `suffix` when it has one; empty when there is
    /// nowhere to put one.
    [[nodiscard]] QString path(const QString &suffix) const {
        if (dir_ == nullptr) return {};
        QString name = QUuid::createUuid().toString(QUuid::WithoutBraces);
        if (!suffix.isEmpty()) name += QLatin1Char('.') + suffix;
        return dir_->filePath(name);
    }

    /// The directory, or empty when there is none.
    [[nodiscard]] QString directory() const { return dir_ == nullptr ? QString() : dir_->path(); }

private:
    static std::unique_ptr<QTemporaryDir> make(const QString &root) {
        const QString pattern = QStringLiteral("/migo-host-files-XXXXXX");
        if (!root.isEmpty() && QDir().mkpath(root)) {
            auto dir = std::make_unique<QTemporaryDir>(root + pattern);
            if (dir->isValid()) return dir;
        }
        // The system's temporary directory: another volume more often than not,
        // where taking a file costs the Session a copy.
        auto dir = std::make_unique<QTemporaryDir>(QDir::tempPath() + pattern);
        if (dir->isValid()) return dir;
        qWarning("migo host kit: no staging directory for images: %s", qPrintable(dir->errorString()));
        return nullptr;
    }

    std::unique_ptr<QTemporaryDir> dir_;
};

namespace {

const QString kNotSupported = QStringLiteral("not supported");
const QString kNoWindow = QStringLiteral("there is no window to show it in");
/// How long a preview's download may go without a byte: the network calls' own default.
constexpr int kTransferTimeoutMs = 60'000;
const QString kNowhere = QStringLiteral("there is nowhere to put the file");
const QString kCancel = QStringLiteral("cancel");

/// Runs `work` on Qt's thread pool.
template <typename Work>
void offTheGuiThread(Work &&work) {
    QThreadPool::globalInstance()->start(std::forward<Work>(work));
}

/// Completes `reply` with `result`, which names `files` of the Host Kit's own.
/// The Session takes each as the call completes; what it did not take -- the
/// game that asked has gone -- is removed.
void handOver(MigoQtHostServiceReply &reply, const QJsonObject &result, const QStringList &files) {
    (void)reply.succeed(result);
    for (const QString &file : files) QFile::remove(file);
}

/// Fails `reply` with `reason`, removing what was staged for it.
void giveUp(MigoQtHostServiceReply &reply, const QString &reason, const QStringList &staged) {
    for (const QString &file : staged) QFile::remove(file);
    (void)reply.fail(reason);
}

QString reasonOf(const QImageReader &reader) {
    switch (reader.error()) {
        case QImageReader::FileNotFoundError:
            return QStringLiteral("file not found");
        case QImageReader::UnsupportedFormatError:
        case QImageReader::InvalidDataError:
            return QStringLiteral("invalid image");
        default:
            return reader.errorString().isEmpty() ? QStringLiteral("decode failed") : reader.errorString();
    }
}

/// An image read upright, and how it was stored: Qt's name for its format.
struct Decoded {
    QImage image;
    QByteArray format;
};

/// Reads the image at `path` upright -- its orientation applied, not carried --
/// at the size `width` x `height` asks of it (`Images::targetSize`), refusing
/// one larger than `Images::kMaxPixels`.
std::optional<Decoded> decode(const QString &path, int width, int height, QString &error) {
    QImageReader reader(path);
    reader.setDecideFormatFromContent(true);
    reader.setAutoTransform(true);
    const auto tooLarge = [](QSize size) {
        return std::int64_t{size.width()} * size.height() > Images::kMaxPixels;
    };

    // What the image is stored as, where its format says without decoding it.
    const QSize stored = reader.size();
    if (!stored.isValid() && !reader.canRead()) {
        error = reasonOf(reader);
        return std::nullopt;
    }
    std::optional<QSize> target;
    if (stored.isValid()) {
        // A quarter turn exchanges the stored width and height.
        const bool turned = (reader.transformation() & QImageIOHandler::TransformationRotate90) != 0;
        const QSize upright = turned ? stored.transposed() : stored;
        target = Images::targetSize(upright, width, height);
        if (tooLarge(*target)) {
            error = QStringLiteral("image too large");
            return std::nullopt;
        }
        // Decoded no larger than the target needs where the format can -- a JPEG
        // can -- in the orientation it is stored in.
        if (*target != upright && reader.supportsOption(QImageIOHandler::ScaledSize)) {
            reader.setScaledSize(turned ? target->transposed() : *target);
        }
    }
    Decoded decoded{QImage(), reader.format()};
    if (!reader.read(&decoded.image)) {
        error = reasonOf(reader);
        return std::nullopt;
    }
    if (!target.has_value()) {
        target = Images::targetSize(decoded.image.size(), width, height);
        if (tooLarge(*target)) {
            error = QStringLiteral("image too large");
            return std::nullopt;
        }
    }
    if (decoded.image.size() != *target) {
        decoded.image = decoded.image.scaled(*target, Qt::IgnoreAspectRatio, Qt::SmoothTransformation);
    }
    return decoded;
}

/// Writes `image` to `path` as `format`, at `quality` (0 to 100, or -1 for the
/// format's default).
bool write(const QImage &image, const QString &path, const char *format, int quality, QString &error) {
    QImageWriter writer(path, format);
    writer.setQuality(quality);
    if (writer.write(image)) return true;
    error = writer.errorString();
    QFile::remove(path);
    return false;
}

/// Copies `from` to `to`, replacing what is there only once the copy is whole.
bool copyFile(const QString &from, const QString &to, QString &error) {
    QFile in(from);
    if (!in.open(QIODevice::ReadOnly)) {
        error = in.error() == QFileDevice::OpenError && !in.exists() ? QStringLiteral("file not found") : in.errorString();
        return false;
    }
    QSaveFile out(to);
    if (!out.open(QIODevice::WriteOnly)) {
        error = out.errorString();
        return false;
    }
    QByteArray buffer(256 * 1024, Qt::Uninitialized);
    qint64 read = 0;
    while ((read = in.read(buffer.data(), buffer.size())) > 0) {
        if (out.write(buffer.constData(), read) != read) {
            error = out.errorString();
            out.cancelWriting();
            return false;
        }
    }
    if (read < 0) {
        error = in.errorString();
        out.cancelWriting();
        return false;
    }
    if (!out.commit()) {
        error = out.errorString();
        return false;
    }
    return true;
}

/// Qt's names for the formats content draws, with the suffix each is staged under.
const QHash<QByteArray, QString> &drawableFormats() {
    static const QHash<QByteArray, QString> formats = {
        {"jpeg", QStringLiteral("jpg")}, {"png", QStringLiteral("png")}, {"gif", QStringLiteral("gif")},
        {"bmp", QStringLiteral("bmp")},  {"webp", QStringLiteral("webp")},
    };
    return formats;
}

/// A staged copy of the picked image at `file` that content can draw: a JPEG,
/// PNG, GIF, BMP or WebP as it is; anything else Qt reads -- a TIFF, an SVG, a
/// HEIF where a plugin reads it -- written upright, as a PNG when it has
/// transparency and as a JPEG when it has none. Empty, with `error`, when there
/// is no image to give.
QString stageImage(const Images::Staging &staging, const QString &file, QString &error) {
    const QString name = QFileInfo(file).fileName();
    QImageReader reader(file);
    reader.setDecideFormatFromContent(true);
    if (const auto drawable = drawableFormats().constFind(reader.format()); drawable != drawableFormats().cend()) {
        const QString copy = staging.path(*drawable);
        if (copy.isEmpty()) {
            error = kNowhere;
            return {};
        }
        if (!QFile::copy(file, copy)) {
            error = QStringLiteral("%1 could not be copied").arg(name);
            return {};
        }
        return copy;
    }
    QString reason;
    const std::optional<Decoded> decoded = decode(file, 0, 0, reason);
    if (!decoded.has_value()) {
        error = QStringLiteral("%1: %2").arg(name, reason);
        return {};
    }
    const bool transparent = decoded->image.hasAlphaChannel();
    const QString copy = staging.path(transparent ? QStringLiteral("png") : QStringLiteral("jpg"));
    if (copy.isEmpty()) {
        error = kNowhere;
        return {};
    }
    if (!write(decoded->image, copy, transparent ? "png" : "jpeg", transparent ? -1 : 92, reason)) {
        error = QStringLiteral("%1: %2").arg(name, reason);
        return {};
    }
    return copy;
}

/// The size an image is seen at: upright.
QSize uprightSize(const QString &file) {
    QImageReader reader(file);
    reader.setDecideFormatFromContent(true);
    const QSize stored = reader.size();
    if (!stored.isValid()) return {0, 0};
    return (reader.transformation() & QImageIOHandler::TransformationRotate90) != 0 ? stored.transposed() : stored;
}

/// "image", "video" or "file", as chooseMessageFile words it: by what the file
/// is, as the desktop knows it.
QString kindOf(const QString &file) {
    const QString type = QMimeDatabase().mimeTypeForFile(file).name();
    if (type.startsWith(QLatin1String("image/"))) return QStringLiteral("image");
    if (type.startsWith(QLatin1String("video/"))) return QStringLiteral("video");
    return QStringLiteral("file");
}

/// A dialog filter: `label` and `patterns`.
QString filter(const QString &label, QStringList patterns) {
    patterns.removeDuplicates();
    patterns.sort();
    return QStringLiteral("%1 (%2)").arg(label, patterns.join(QLatin1Char(' ')));
}

/// The names of every image Qt reads.
QStringList imagePatterns() {
    const QMimeDatabase types;
    QStringList patterns;
    for (const QByteArray &name : QImageReader::supportedMimeTypes()) {
        patterns << types.mimeTypeForName(QString::fromLatin1(name)).globPatterns();
    }
    return patterns;
}

/// The names of every video the desktop knows of.
QStringList videoPatterns() {
    QStringList patterns;
    for (const QMimeType &type : QMimeDatabase().allMimeTypes()) {
        if (type.name().startsWith(QLatin1String("video/"))) patterns << type.globPatterns();
    }
    return patterns;
}

QString imageFilter() { return filter(QStringLiteral("Images"), imagePatterns()); }

QString videoFilter() { return filter(QStringLiteral("Videos"), videoPatterns()); }

/// A staged copy of the picked video at `file`, as chooseMedia describes it:
/// its length, its size as shown and its cover where `videos` reads it, and
/// zeros and no cover where it cannot, as the other platforms answer. What it
/// stages is added to `staged`; none, with `error`, when there is nowhere to
/// put it.
std::optional<QJsonObject> stageVideo(const Images::Staging &staging, const MigoQtVideoSupport &videos,
                                      const QString &file, QStringList &staged, QString &error) {
    const QFileInfo original(file);
    const QString copy = staging.path(original.suffix());
    if (copy.isEmpty()) {
        error = kNowhere;
        return std::nullopt;
    }
    if (!QFile::copy(file, copy)) {
        error = QStringLiteral("%1 could not be copied").arg(original.fileName());
        return std::nullopt;
    }
    staged << copy;
    QJsonObject item{{QStringLiteral("tempFilePath"), copy},
                     {QStringLiteral("size"), QFileInfo(copy).size()},
                     {QStringLiteral("duration"), 0},
                     {QStringLiteral("width"), 0},
                     {QStringLiteral("height"), 0},
                     {QStringLiteral("fileType"), QStringLiteral("video")}};
    const std::optional<MigoQtVideoSupport::Description> described = videos.describe(copy);
    if (!described.has_value()) return item;
    item[QStringLiteral("duration")] = described->durationSeconds;
    item[QStringLiteral("width")] = described->size.width();
    item[QStringLiteral("height")] = described->size.height();
    if (described->cover.isNull()) return item;
    // The cover as the other platforms write it: a JPEG at 80.
    const QString cover = staging.path(QStringLiteral("jpg"));
    QString unwritten;
    if (!cover.isEmpty() && write(described->cover, cover, "jpeg", 80, unwritten)) {
        staged << cover;
        item[QStringLiteral("thumbTempFilePath")] = cover;
    }
    return item;
}

/// Whether `source` is an http(s) URL rather than a path.
bool remote(const QString &source) {
    const QString scheme = QUrl(source).scheme().toLower();
    return scheme == QLatin1String("http") || scheme == QLatin1String("https");
}

/// The most the player may pick: `value`, or `fallback`, from 1 to 100.
int countOf(const QJsonValue &value, int fallback) {
    return static_cast<int>(qBound(1.0, value.toDouble(fallback), 100.0));
}

/// A width or height asked for in pixels, or 0 when none is.
int dimensionOf(const QJsonValue &value) {
    const double asked = value.toDouble(0);
    return asked >= 1 ? static_cast<int>(qMin(asked, 1e7)) : 0;
}

/// The camera is not offered: a call that allows only it is refused, one that
/// allows the album too is shown the dialog.
bool albumAllowed(const QJsonValue &sources) {
    const QJsonArray names = sources.toArray();
    return names.isEmpty() || names.contains(QStringLiteral("album"));
}

}  // namespace

// ---- the service ---------------------------------------------------------------------------

Images::Places Images::Places::system() {
    return {QStandardPaths::writableLocation(QStandardPaths::CacheLocation),
            QStandardPaths::writableLocation(QStandardPaths::PicturesLocation)};
}

Images::Images(QWidget &surface, Places places, std::shared_ptr<const MigoQtVideoSupport> videos)
    : surface_(&surface), places_(std::move(places)), videos_(std::move(videos)) {}

Images::~Images() {
    // A dialog still up is dismissed with the service that showed it; the
    // answer it gives reaches nobody once the channel is closed. A preview
    // shown goes too, and those still loading stop.
    if (!dialog_.isNull()) dialog_->reject();
    if (!viewer_.isNull()) viewer_->close();
    for (const QPointer<QObject> &load : std::as_const(loads_)) delete load.data();
}

QSize Images::targetSize(QSize source, int width, int height) {
    // `side` scaled as `asked` is to `other`: at least a pixel, and no more than
    // an int holds, which the ceiling on pixels refuses anyway.
    const auto scaled = [](int side, int asked, int other) {
        const double value = std::round(static_cast<double>(side) * asked / other);
        return static_cast<int>(qBound(1.0, value, static_cast<double>(std::numeric_limits<int>::max())));
    };
    if (width > 0 && height > 0) return {width, height};
    if (width > 0) return {width, scaled(source.height(), width, source.width())};
    if (height > 0) return {scaled(source.width(), height, source.height()), height};
    return source;
}

void Images::call(std::uint32_t method, const QJsonObject &request, MigoQtHostServiceReply reply) {
    switch (method) {
        case MIGO_IMAGE_COMPRESS_IMAGE:
            compressImage(request, std::move(reply));
            return;
        case MIGO_IMAGE_CHOOSE_IMAGE:
            chooseImage(request, std::move(reply));
            return;
        case MIGO_IMAGE_CHOOSE_MEDIA:
            chooseMedia(request, std::move(reply));
            return;
        case MIGO_IMAGE_CHOOSE_MESSAGE_FILE:
            chooseMessageFile(request, std::move(reply));
            return;
        case MIGO_IMAGE_SAVE_FILE_TO_DISK:
            saveFileToDisk(request, std::move(reply));
            return;
        case MIGO_IMAGE_SAVE_IMAGE_TO_PHOTOS_ALBUM:
            saveImageToPhotosAlbum(request, std::move(reply));
            return;
        case MIGO_IMAGE_PREVIEW_IMAGE: {
            QStringList urls;
            for (const QJsonValue &url : request.value(QStringLiteral("urls")).toArray()) urls << url.toString();
            // `current` is one of them, made a real path as they were.
            const int current = qMax(0, urls.indexOf(request.value(QStringLiteral("current")).toString()));
            std::vector<PreviewSource> sources;
            for (const QString &url : std::as_const(urls)) sources.push_back({url, false, {}});
            preview(std::move(sources), current, std::move(reply));
            return;
        }
        case MIGO_IMAGE_PREVIEW_MEDIA: {
            std::vector<PreviewSource> sources;
            for (const QJsonValue &source : request.value(QStringLiteral("sources")).toArray()) {
                const QJsonObject item = source.toObject();
                const bool video = item.value(QStringLiteral("type")).toString() == QLatin1String("video");
                // Videos where the App gives what plays them: see the class's comment.
                if (video && videos_ == nullptr) {
                    (void)reply.fail(kNotSupported);
                    return;
                }
                sources.push_back({item.value(QStringLiteral("url")).toString(), video,
                                   video ? item.value(QStringLiteral("poster")).toString() : QString()});
            }
            preview(std::move(sources), request.value(QStringLiteral("current")).toInt(0), std::move(reply));
            return;
        }
        default:
            (void)reply.fail(kNotSupported);
            return;
    }
}

std::shared_ptr<Images::Staging> Images::staging() {
    if (staging_ == nullptr) staging_ = std::make_shared<Staging>(places_.staging);
    return staging_;
}

QNetworkAccessManager &Images::network() {
    if (network_ == nullptr) network_ = new QNetworkAccessManager(this);
    return *network_;
}

// ---- compressImage -------------------------------------------------------------------------

void Images::compressImage(const QJsonObject &request, MigoQtHostServiceReply reply) {
    const QString source = request.value(QStringLiteral("src")).toString();
    if (source.isEmpty()) {
        (void)reply.fail(QStringLiteral("src is required"));
        return;
    }
    const int quality = static_cast<int>(std::lround(qBound(0.0, request.value(QStringLiteral("quality")).toDouble(80), 100.0)));
    const int width = dimensionOf(request.value(QStringLiteral("compressedWidth")));
    const int height = dimensionOf(request.value(QStringLiteral("compressedHeight")));
    offTheGuiThread([staging = staging(), reply, source, quality, width, height]() mutable {
        QString error;
        const std::optional<Decoded> decoded = decode(source, width, height, error);
        if (!decoded.has_value()) {
            (void)reply.fail(error);
            return;
        }
        // A PNG stays a PNG; anything else becomes a JPEG, at the quality asked.
        const bool png = decoded->format == "png";
        const QString output = staging->path(png ? QStringLiteral("png") : QStringLiteral("jpg"));
        if (output.isEmpty()) {
            (void)reply.fail(kNowhere);
            return;
        }
        if (!write(decoded->image, output, png ? "png" : "jpeg", png ? -1 : quality, error)) {
            (void)reply.fail(error);
            return;
        }
        handOver(reply, {{QStringLiteral("tempFilePath"), output}}, {output});
    });
}

// ---- the pickers ---------------------------------------------------------------------------

bool Images::canShowDialog(MigoQtHostServiceReply &reply) {
    if (surface_.isNull()) {
        (void)reply.fail(kNoWindow);
        return false;
    }
    if (!dialog_.isNull()) {
        (void)reply.fail(QStringLiteral("another file dialog is showing"));
        return false;
    }
    return true;
}

void Images::show(QFileDialog *dialog) {
    dialog->setAttribute(Qt::WA_DeleteOnClose);
    dialog_ = dialog;
    // One dialog at a time: the next may be shown once this one has answered.
    connect(dialog, &QDialog::finished, this, [this] { dialog_.clear(); });
    dialog->open();
}

void Images::pick(int count, const QStringList &filters, const QString &directory, MigoQtHostServiceReply reply,
                  Picked picked) {
    if (!canShowDialog(reply)) return;
    auto *dialog = new QFileDialog(surface_->window());
    dialog->setAcceptMode(QFileDialog::AcceptOpen);
    dialog->setFileMode(count > 1 ? QFileDialog::ExistingFiles : QFileDialog::ExistingFile);
    if (!filters.isEmpty()) dialog->setNameFilters(filters);
    if (!directory.isEmpty() && QFileInfo(directory).isDir()) dialog->setDirectory(directory);
    connect(dialog, &QDialog::finished, dialog,
            [dialog, count, reply, picked = std::move(picked)](int code) mutable {
                if (code != QDialog::Accepted) {
                    (void)reply.fail(kCancel);
                    return;
                }
                // A dialog does not count what the player selects.
                picked(dialog->selectedFiles().mid(0, count), reply);
            });
    show(dialog);
}

void Images::chooseImage(const QJsonObject &request, MigoQtHostServiceReply reply) {
    if (!albumAllowed(request.value(QStringLiteral("sourceType")))) {
        (void)reply.fail(kNotSupported);
        return;
    }
    // The originals, whichever size is asked for: the player picked them.
    pick(countOf(request.value(QStringLiteral("count")), 9), {imageFilter()}, places_.album, std::move(reply),
         [staging = staging()](QStringList files, MigoQtHostServiceReply reply) {
             offTheGuiThread([staging, files = std::move(files), reply]() mutable {
                 QStringList staged;
                 QJsonArray paths;
                 QJsonArray described;
                 for (const QString &file : std::as_const(files)) {
                     QString error;
                     const QString copy = stageImage(*staging, file, error);
                     if (copy.isEmpty()) {
                         giveUp(reply, error, staged);
                         return;
                     }
                     staged << copy;
                     paths.append(copy);
                     described.append(QJsonObject{{QStringLiteral("path"), copy},
                                                  {QStringLiteral("size"), QFileInfo(copy).size()}});
                 }
                 handOver(reply, {{QStringLiteral("tempFilePaths"), paths}, {QStringLiteral("tempFiles"), described}},
                          staged);
             });
         });
}

void Images::chooseMedia(const QJsonObject &request, MigoQtHostServiceReply reply) {
    const QJsonArray types = request.value(QStringLiteral("mediaType")).toArray();
    // None named, or "mix", is both.
    const bool both = types.isEmpty() || types.contains(QStringLiteral("mix"));
    const bool images = both || types.contains(QStringLiteral("image"));
    // Videos where the App gives what describes them (see the class's comment):
    // without it, a call for only them is refused, and one that allows images
    // too is shown images.
    const bool videos = videos_ != nullptr && (both || types.contains(QStringLiteral("video")));
    if (!(images || videos) || !albumAllowed(request.value(QStringLiteral("sourceType")))) {
        (void)reply.fail(kNotSupported);
        return;
    }
    QStringList filters;
    if (images && videos) filters << filter(QStringLiteral("Images and videos"), imagePatterns() + videoPatterns());
    if (images) filters << imageFilter();
    if (videos) filters << videoFilter();
    pick(countOf(request.value(QStringLiteral("count")), 9), filters, places_.album, std::move(reply),
         [staging = staging(), support = videos ? videos_ : nullptr](QStringList files, MigoQtHostServiceReply reply) {
             offTheGuiThread([staging, support, files = std::move(files), reply]() mutable {
                 QStringList staged;
                 QJsonArray items;
                 bool sawImage = false;
                 bool sawVideo = false;
                 for (const QString &file : std::as_const(files)) {
                     QString error;
                     if (support != nullptr && kindOf(file) == QLatin1String("video")) {
                         const std::optional<QJsonObject> item = stageVideo(*staging, *support, file, staged, error);
                         if (!item.has_value()) {
                             giveUp(reply, error, staged);
                             return;
                         }
                         items.append(*item);
                         sawVideo = true;
                         continue;
                     }
                     const QString copy = stageImage(*staging, file, error);
                     if (copy.isEmpty()) {
                         giveUp(reply, error, staged);
                         return;
                     }
                     staged << copy;
                     const QSize size = uprightSize(copy);
                     items.append(QJsonObject{{QStringLiteral("tempFilePath"), copy},
                                              {QStringLiteral("size"), QFileInfo(copy).size()},
                                              {QStringLiteral("width"), size.width()},
                                              {QStringLiteral("height"), size.height()},
                                              {QStringLiteral("fileType"), QStringLiteral("image")}});
                     sawImage = true;
                 }
                 const QString type = sawImage && sawVideo ? QStringLiteral("mix")
                                      : sawVideo           ? QStringLiteral("video")
                                                           : QStringLiteral("image");
                 handOver(reply, {{QStringLiteral("type"), type}, {QStringLiteral("tempFiles"), items}}, staged);
             });
         });
}

void Images::chooseMessageFile(const QJsonObject &request, MigoQtHostServiceReply reply) {
    // "file" is anything but an image or a video; "all", or a type not known, anything.
    QString type = request.value(QStringLiteral("type")).toString(QStringLiteral("all"));
    if (type != QLatin1String("image") && type != QLatin1String("video") && type != QLatin1String("file")) {
        type = QStringLiteral("all");
    }
    // The extensions narrow a pick of files, and only of files.
    QSet<QString> extensions;
    if (type == QLatin1String("file")) {
        for (const QJsonValue &value : request.value(QStringLiteral("extension")).toArray()) {
            QString extension = value.toString().toLower();
            if (extension.startsWith(QLatin1Char('.'))) extension.remove(0, 1);
            if (!extension.isEmpty()) extensions.insert(extension);
        }
    }
    QStringList filters;
    if (type == QLatin1String("image")) {
        filters << imageFilter();
    } else if (type == QLatin1String("video")) {
        filters << videoFilter();
    } else if (!extensions.isEmpty()) {
        QStringList patterns;
        for (const QString &extension : std::as_const(extensions)) patterns << QStringLiteral("*.") + extension;
        filters << filter(QStringLiteral("Files"), std::move(patterns));
    }
    pick(countOf(request.value(QStringLiteral("count")), 1), filters, {}, std::move(reply),
         [staging = staging(), type, extensions](QStringList files, MigoQtHostServiceReply reply) {
             offTheGuiThread([staging, type, extensions, files = std::move(files), reply]() mutable {
                 QStringList staged;
                 QJsonArray described;
                 for (const QString &file : std::as_const(files)) {
                     // The dialog's filter is the player's to change: what it let
                     // through that the call did not ask for is left out.
                     const QString kind = kindOf(file);
                     const QFileInfo original(file);
                     if (type != QLatin1String("all") && kind != type) continue;
                     if (!extensions.isEmpty() && !extensions.contains(original.suffix().toLower())) continue;
                     const QString copy = staging->path(original.suffix());
                     if (copy.isEmpty()) {
                         giveUp(reply, kNowhere, staged);
                         return;
                     }
                     if (!QFile::copy(file, copy)) {
                         giveUp(reply, QStringLiteral("%1 could not be copied").arg(original.fileName()), staged);
                         return;
                     }
                     staged << copy;
                     described.append(QJsonObject{{QStringLiteral("path"), copy},
                                                  {QStringLiteral("size"), QFileInfo(copy).size()},
                                                  {QStringLiteral("name"), original.fileName()},
                                                  {QStringLiteral("type"), kind},
                                                  {QStringLiteral("time"), original.lastModified().toSecsSinceEpoch()}});
                 }
                 handOver(reply, {{QStringLiteral("tempFiles"), described}}, staged);
             });
         });
}

// ---- saving --------------------------------------------------------------------------------

void Images::saveFileToDisk(const QJsonObject &request, MigoQtHostServiceReply reply) {
    const QString source = request.value(QStringLiteral("filePath")).toString();
    if (source.isEmpty()) {
        (void)reply.fail(QStringLiteral("filePath is required"));
        return;
    }
    const QFileInfo file(source);
    if (!file.isFile()) {
        (void)reply.fail(QStringLiteral("file not found"));
        return;
    }
    if (!canShowDialog(reply)) return;
    auto *dialog = new QFileDialog(surface_->window());
    dialog->setAcceptMode(QFileDialog::AcceptSave);
    dialog->setFileMode(QFileDialog::AnyFile);
    dialog->selectFile(file.fileName());
    // The dialog asks before replacing a file; the copy replaces it only once whole.
    connect(dialog, &QDialog::finished, dialog, [dialog, source, reply](int code) mutable {
        const QString destination = code == QDialog::Accepted ? dialog->selectedFiles().value(0) : QString();
        if (destination.isEmpty()) {
            (void)reply.fail(kCancel);
            return;
        }
        offTheGuiThread([source, destination, reply]() mutable {
            QString error;
            (void)(copyFile(source, destination, error) ? reply.succeed() : reply.fail(error));
        });
    });
    show(dialog);
}

void Images::saveImageToPhotosAlbum(const QJsonObject &request, MigoQtHostServiceReply reply) {
    const QString source = request.value(QStringLiteral("filePath")).toString();
    if (source.isEmpty()) {
        (void)reply.fail(QStringLiteral("filePath is required"));
        return;
    }
    if (places_.album.isEmpty()) {
        (void)reply.fail(QStringLiteral("there is no photo album"));
        return;
    }
    offTheGuiThread([album = places_.album, source, reply]() mutable {
        QImageReader reader(source);
        reader.setDecideFormatFromContent(true);
        if (!reader.canRead()) {
            (void)reply.fail(reader.error() == QImageReader::FileNotFoundError ? QStringLiteral("file not found")
                                                                               : QStringLiteral("not an image"));
            return;
        }
        if (!QDir().mkpath(album)) {
            (void)reply.fail(QStringLiteral("the photo album cannot be written to"));
            return;
        }
        // Named as a camera names a photo, with the suffix of what it holds.
        QString suffix = QMimeDatabase().mimeTypeForFile(source, QMimeDatabase::MatchContent).preferredSuffix();
        if (suffix.isEmpty()) suffix = QString::fromLatin1(reader.format());
        const QString stem = QStringLiteral("IMG_%1").arg(QDateTime::currentMSecsSinceEpoch());
        const QDir folder(album);
        QString destination = folder.filePath(stem + QLatin1Char('.') + suffix);
        for (int n = 1; QFileInfo::exists(destination); ++n) {
            destination = folder.filePath(QStringLiteral("%1_%2.%3").arg(stem).arg(n).arg(suffix));
        }
        QString error;
        (void)(copyFile(source, destination, error) ? reply.succeed() : reply.fail(error));
    });
}

// ---- the previews --------------------------------------------------------------------------

/// One preview's images, videos and posters, each opened -- a file -- or
/// downloaded -- an http(s) image or poster -- and then shown, an http(s) video
/// left to stream: the call is answered as the viewer shows, or failed with
/// the first that cannot be had. It goes once done, or with the service, its
/// downloads stopped.
class Images::PreviewLoad final : public QObject {
public:
    PreviewLoad(Images &images, std::size_t count, int current, MigoQtHostServiceReply reply)
        : QObject(&images), images_(images), items_(count), current_(current), reply_(std::move(reply)) {}

    ~PreviewLoad() override {
        for (const QPointer<QNetworkReply> &download : std::as_const(downloads_)) {
            if (download.isNull()) continue;
            download->disconnect(this);
            download->abort();
            download->deleteLater();
        }
    }

    /// Opens what `sources` name, the viewer shown once all are.
    void start(const std::vector<PreviewSource> &sources) {
        // Everything to open is counted first, so none settles the whole before
        // the rest have started.
        for (const PreviewSource &source : sources) {
            if (!source.video || !remote(source.url)) ++pending_;
            if (!source.poster.isEmpty()) ++pending_;
        }
        for (std::size_t index = 0; index < sources.size() && !done_; ++index) {
            const PreviewSource &source = sources[index];
            MediaViewer::Item &item = items_[index];
            item.video = source.video;
            if (source.video && remote(source.url)) {
                item.url = source.url;
            } else {
                open(index, &MediaViewer::Item::file, source.url);
            }
            if (!done_ && !source.poster.isEmpty()) open(index, &MediaViewer::Item::poster, source.poster);
        }
        if (!done_ && pending_ == 0) finish();
    }

private:
    using Part = std::unique_ptr<QIODevice> MediaViewer::Item::*;

    /// Opens `source` as the `part` of the `index`th item.
    void open(std::size_t index, Part part, const QString &source) {
        if (remote(source)) {
            download(index, part, QUrl(source));
            return;
        }
        auto file = std::make_unique<QFile>(source);
        if (!file->open(QIODevice::ReadOnly)) {
            settle(index, part, nullptr,
                   file->exists() ? QStringLiteral("%1 cannot be read").arg(QFileInfo(source).fileName())
                                  : QStringLiteral("file not found"));
            return;
        }
        settle(index, part, std::move(file), {});
    }

    /// Downloads `url` into a file removed as it is made: what arrives is
    /// read back through the handle that stays open, and nothing is left behind.
    void download(std::size_t index, Part part, const QUrl &url) {
        const QString directory = images_.staging()->directory();
        auto file = std::make_unique<QTemporaryFile>(
            (directory.isEmpty() ? QDir::tempPath() : directory) + QStringLiteral("/preview-XXXXXX"));
        if (!file->open()) {
            settle(index, part, nullptr, kNowhere);
            return;
        }
        QFile::remove(file->fileName());
        file->setAutoRemove(false);
        // Shared by the download's two handlers until it passes to the viewer.
        auto sink = std::make_shared<std::unique_ptr<QTemporaryFile>>(std::move(file));

        QNetworkRequest request(url);
        // No referrer, whatever referrerPolicy says: the host has none to send.
        request.setTransferTimeout(kTransferTimeoutMs);
        QNetworkReply *download = images_.network().get(request);
        downloads_.append(download);
        auto unwritten = std::make_shared<QString>();
        connect(download, &QIODevice::readyRead, this, [download, sink, unwritten] {
            if (unwritten->isEmpty() && (*sink)->write(download->readAll()) < 0) {
                *unwritten = (*sink)->errorString();
                download->abort();
            }
        });
        connect(download, &QNetworkReply::finished, this, [this, index, part, download, sink, unwritten] {
            download->deleteLater();
            QTemporaryFile &received = **sink;
            const QVariant status = download->attribute(QNetworkRequest::HttpStatusCodeAttribute);
            QString error;
            if (!unwritten->isEmpty()) {
                error = QStringLiteral("download failed: %1").arg(*unwritten);
            } else if (status.isValid() && (status.toInt() < 200 || status.toInt() >= 300)) {
                error = QStringLiteral("download failed: HTTP %1").arg(status.toInt());
            } else if (download->error() != QNetworkReply::NoError) {
                error = QStringLiteral("download failed: %1").arg(download->errorString());
            } else if (received.write(download->readAll()) < 0 || !received.flush() || !received.seek(0)) {
                error = QStringLiteral("download failed: %1").arg(received.errorString());
            }
            settle(index, part, error.isEmpty() ? std::move(*sink) : nullptr, error);
        });
    }

    void settle(std::size_t index, Part part, std::unique_ptr<QIODevice> opened, const QString &error) {
        if (done_) return;
        if (!error.isEmpty()) {
            done_ = true;
            (void)reply_.fail(error);
            deleteLater();
            return;
        }
        items_.at(index).*part = std::move(opened);
        if (--pending_ == 0) finish();
    }

    void finish() {
        done_ = true;
        images_.showPreview(std::move(items_), current_, reply_);
        deleteLater();
    }

    Images &images_;
    std::vector<MediaViewer::Item> items_;
    int pending_ = 0;
    const int current_;
    MigoQtHostServiceReply reply_;
    bool done_ = false;
    QList<QPointer<QNetworkReply>> downloads_;
};

void Images::preview(std::vector<PreviewSource> sources, int current, MigoQtHostServiceReply reply) {
    const bool unnamed =
        std::any_of(sources.cbegin(), sources.cend(), [](const PreviewSource &source) { return source.url.isEmpty(); });
    if (sources.empty() || unnamed) {
        (void)reply.fail(QStringLiteral("nothing to preview"));
        return;
    }
    if (surface_.isNull()) {
        (void)reply.fail(kNoWindow);
        return;
    }
    const int last = static_cast<int>(sources.size()) - 1;
    auto *load = new PreviewLoad(*this, sources.size(), qBound(0, current, last), std::move(reply));
    loads_.removeAll(nullptr);
    loads_.append(load);
    load->start(sources);
}

void Images::showPreview(std::vector<MediaViewer::Item> items, int current, MigoQtHostServiceReply reply) {
    if (surface_.isNull()) {
        (void)reply.fail(kNoWindow);
        return;
    }
    // Another preview replaces the one shown.
    if (!viewer_.isNull()) viewer_->close();
    auto *viewer = new MediaViewer(*surface_->window(), std::move(items), current, videos_);
    viewer->setAttribute(Qt::WA_DeleteOnClose);
    viewer_ = viewer;
    viewer->open();
    (void)reply.succeed();
}

}  // namespace migo::linux_host::qt6

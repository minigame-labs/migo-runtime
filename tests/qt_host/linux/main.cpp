// A Linux host built on the Qt Host Kit's Managed shape: one window, one game.
//
// It is what migo-conformance's Host Kit lane runs, and it is written as an App
// would write it -- MigoManagedSession owns the Session, the view and the
// built-in host services -- so that what the lane verifies is the platform's
// own host layer, not a host made for the test. The C host beside it
// (tests/c_host/linux) is the other shape: the public C ABI with nothing on top.
//
//   migo-qt-host FILES_DIR CONTENT_ID SECONDS
//
// The game is the installed content CONTENT_ID under FILES_DIR (as
// scripts/dev-run-qt-host.sh deploys it), run for SECONDS or until it asks to
// exit. Its settings come as the C host's do, from files named by the
// environment: MIGO_QT_HOST_NETWORK_POLICY, the network (none at all without
// it), and MIGO_QT_HOST_LAUNCH_OPTIONS, what the game is launched with.
//
// The player is scripted, as the C host's device is: MIGO_QT_HOST_FEED names a
// file that whoever runs the host appends lines to while it runs, and
//
//   TAP-TEXT <text>    the player taps the button, or the menu item, that says <text>
//
// is done once something on screen says it -- a dialog may not be up yet when
// the line arrives.

#include <migo/linux/qt6/managed_session.hpp>

#include <QAbstractButton>
#include <QAction>
#include <QApplication>
#include <QByteArray>
#include <QFile>
#include <QMenu>
#include <QTimer>
#include <QVBoxLayout>
#include <QWidget>

#include <cstdio>
#include <cstdlib>
#include <string>

using migo::linux_host::qt6::MigoManagedSession;

namespace {

constexpr int kWindowWidth = 720;
constexpr int kWindowHeight = 1280;

/// A setting's JSON, read whole from the file the environment variable `name`
/// names, or empty when it names none. A file that cannot be read stops the
/// host: running without the setting would fail every check that needs it for a
/// reason no check names.
QByteArray readSetting(const char *name) {
    const char *path = std::getenv(name);
    if (path == nullptr || path[0] == '\0') return {};
    QFile file(QString::fromLocal8Bit(path));
    if (!file.open(QIODevice::ReadOnly)) {
        std::fprintf(stderr, "[qt-host] cannot read %s=%s\n", name, path);
        std::exit(2);
    }
    return file.readAll();
}

/// Taps what says `text`, if anything on screen does.
bool tap(const QString &text) {
    for (QWidget *window : QApplication::topLevelWidgets()) {
        if (!window->isVisible()) continue;
        if (auto *menu = qobject_cast<QMenu *>(window)) {
            for (QAction *action : menu->actions()) {
                if (action->isEnabled() && !action->isSeparator() && action->text() == text) {
                    // As a click does: the menu closes, then reports the item.
                    menu->hide();
                    action->trigger();
                    return true;
                }
            }
        }
        for (QAbstractButton *button : window->findChildren<QAbstractButton *>()) {
            if (button->isVisible() && button->isEnabled() && button->text() == text) {
                button->click();
                return true;
            }
        }
    }
    return false;
}

/// The scripted player: reads the feed as it grows, and keeps each tap until
/// something on screen says its text.
class Player final : public QObject {
public:
    explicit Player(const char *path) {
        if (path == nullptr || path[0] == '\0') return;
        feed_.setFileName(QString::fromLocal8Bit(path));
        auto *timer = new QTimer(this);
        connect(timer, &QTimer::timeout, this, [this] { step(); });
        timer->start(50);
    }

private:
    void step() {
        if (!feed_.isOpen() && !feed_.open(QIODevice::ReadOnly)) return;
        while (true) {
            // A line is read only once it is whole: its writer may be part way through it.
            buffered_ += feed_.readAll();
            const qsizetype end = buffered_.indexOf('\n');
            if (end < 0) break;
            const QString line = QString::fromUtf8(buffered_.left(end)).trimmed();
            buffered_.remove(0, end + 1);
            if (line.startsWith(QStringLiteral("TAP-TEXT "))) taps_.append(line.mid(9));
        }
        while (!taps_.isEmpty() && tap(taps_.first())) taps_.removeFirst();
    }

    QFile feed_;
    QByteArray buffered_;
    QStringList taps_;
};

}  // namespace

int main(int argc, char **argv) {
    QApplication app(argc, argv);
    const QByteArray files_dir = argc > 1 ? QByteArray(argv[1]) : QByteArray("/tmp/migo-qt-host/files");
    const QString content_id = argc > 2 ? QString::fromLocal8Bit(argv[2]) : QStringLiteral("qt-host-demo");
    const int seconds = argc > 3 ? std::atoi(argv[3]) : 10;

    // ---- the engine: the host names every storage root ----
    const QByteArray cache_dir = files_dir + "/../cache";
    const QByteArray code_cache_dir = files_dir + "/../code-cache";
    MigoEngineConfig engine_config{};
    engine_config.struct_size = static_cast<std::uint32_t>(sizeof(engine_config));
    engine_config.abi_version = MIGO_ABI_VERSION_CURRENT;
    // Development host: the deployed game carries no signing receipt.
    engine_config.flags = MIGO_ENGINE_FLAG_ALLOW_UNSIGNED_CONTENT;
    engine_config.files_dir_utf8 = files_dir.constData();
    engine_config.cache_dir_utf8 = cache_dir.constData();
    engine_config.code_cache_dir_utf8 = code_cache_dir.constData();
    MigoEngine *engine = nullptr;
    if (const MigoResult created = migo_engine_create(&engine_config, &engine); created != MIGO_OK) {
        std::fprintf(stderr, "[qt-host] migo_engine_create failed: %d\n", static_cast<int>(created));
        return 1;
    }

    // ---- the window and the game ----
    QWidget window;
    window.setWindowTitle(QStringLiteral("migo-qt-host"));
    window.resize(kWindowWidth, kWindowHeight);
    auto *layout = new QVBoxLayout(&window);
    layout->setContentsMargins(0, 0, 0, 0);

    MigoManagedSession::Options options;
    options.networkPolicyJson = readSetting("MIGO_QT_HOST_NETWORK_POLICY");
    options.launchOptionsJson = readSetting("MIGO_QT_HOST_LAUNCH_OPTIONS");
    auto *game = new MigoManagedSession(*engine, window, options, &app);
    if (!game->isValid()) {
        std::fprintf(stderr, "[qt-host] the session was not created: %d\n", static_cast<int>(game->lastError()));
        return 1;
    }
    layout->addWidget(game->view());

    QObject::connect(game, &MigoManagedSession::contentReady, [] {
        std::printf("[qt-host] content is ready\n");
        std::fflush(stdout);
    });
    QObject::connect(game, &MigoManagedSession::contentFailed, [](MigoResult code, const QString &message) {
        std::fprintf(stderr, "[qt-host] content failed %d: %s\n", static_cast<int>(code), qPrintable(message));
    });
    QObject::connect(game, &MigoManagedSession::migoError, [](MigoResult code) {
        std::fprintf(stderr, "[qt-host] refused: %d\n", static_cast<int>(code));
    });
    // The line every conformance host prints when content asks to exit; then the game goes.
    QObject::connect(game, &MigoManagedSession::exitRequested, [game] {
        std::printf("[host] exit requested\n");
        std::fflush(stdout);
        (void)game->close();
    });
    QObject::connect(game, &MigoManagedSession::sessionClosed, &app, [] {
        std::printf("[qt-host] done\n");
        std::fflush(stdout);
        QCoreApplication::quit();
    });
    QTimer::singleShot(seconds * 1000, game, [game] { (void)game->close(); });

    // Closing the window ends the game the way every other ending does: through
    // close(), and the application quits once the Session is gone.
    app.setQuitOnLastWindowClosed(false);
    QObject::connect(&app, &QGuiApplication::lastWindowClosed, game, [game] { (void)game->close(); });

    Player player(std::getenv("MIGO_QT_HOST_FEED"));
    window.show();
    if (const MigoResult loaded = game->loadContent(content_id, QStringLiteral("game.js")); loaded != MIGO_OK) {
        std::fprintf(stderr, "[qt-host] loadContent failed: %d\n", static_cast<int>(loaded));
        (void)game->close();
    }
    std::printf("[qt-host] running '%s' for %ds\n", qPrintable(content_id), seconds);
    std::fflush(stdout);

    const int status = app.exec();
    delete game;
    (void)migo_engine_destroy(engine);
    return status;
}

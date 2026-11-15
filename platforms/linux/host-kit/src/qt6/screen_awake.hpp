#ifndef MIGO_LINUX_QT6_SCREEN_AWAKE_HPP_
#define MIGO_LINUX_QT6_SCREEN_AWAKE_HPP_

// Internal to the Host Kit, not installed: content's setKeepScreenOn.

#include <QDBusConnection>
#include <QObject>
#include <QString>

#include <cstdint>

namespace migo::linux_host::qt6 {

/// Keeps the display awake while the game asks: the desktop's idle -- the
/// screen blanking, the session locking -- held off, through the XDG desktop
/// portal's Inhibit, or where no portal answers, the freedesktop screen saver's.
/// What it holds it lets go when the game stops asking or this goes.
class ScreenAwake final : public QObject {
public:
    /// `bus` is where the portal and the screen saver answer: the session's.
    explicit ScreenAwake(QDBusConnection bus, QObject *parent = nullptr);
    ~ScreenAwake() override;

    ScreenAwake(const ScreenAwake &) = delete;
    ScreenAwake &operator=(const ScreenAwake &) = delete;

    /// Hold the display awake for the game shown in X11 window `window`, or let it go.
    void keepAwake(bool on, std::uint64_t window);

    /// Whether the desktop holds the display awake for the game now.
    [[nodiscard]] bool isHeld() const noexcept { return held_ != Held::None; }

private:
    enum class Held { None, Portal, ScreenSaver };

    void inhibit(std::uint64_t window);
    void inhibitThroughScreenSaver();
    /// What the desktop granted, with the game still asking or not.
    void granted(Held how, const QString &portal_request, std::uint32_t cookie);
    void release();

    QDBusConnection bus_;
    /// Whether the game asks now.
    bool wanted_ = false;
    /// Whether an inhibition is on its way: its answer decides what is held.
    bool asking_ = false;
    Held held_ = Held::None;
    /// The portal's request, which closing ends the inhibition.
    QString portal_request_;
    /// The screen saver's cookie, which uninhibiting ends it.
    std::uint32_t cookie_ = 0;
};

}  // namespace migo::linux_host::qt6

#endif  // MIGO_LINUX_QT6_SCREEN_AWAKE_HPP_

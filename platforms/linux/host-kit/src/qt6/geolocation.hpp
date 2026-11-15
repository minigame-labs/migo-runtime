#ifndef MIGO_LINUX_QT6_GEOLOCATION_HPP_
#define MIGO_LINUX_QT6_GEOLOCATION_HPP_

// Internal to the Host Kit, not installed: where the device is.

#include <QDBusConnection>
#include <QDBusObjectPath>
#include <QObject>
#include <QString>
#include <QVariantMap>

#include <chrono>
#include <cstdint>
#include <functional>
#include <list>
#include <optional>

class QTimer;

namespace migo::linux_host::qt6 {

/// Where the device is, as the desktop knows it: GeoClue's answer, asked of
/// GeoClue itself on the system bus outside a sandbox, and of the desktop
/// portal's Location inside one (Flatpak, Snap), which asks GeoClue for the
/// App. Each decides whether the App may know: GeoClue's agent asks the player
/// the first time an App asks, by its desktop file name, and the portal asks
/// inside a sandbox.
///
/// A request starts the source, is answered with the first position it
/// reports -- or, when only the decision was asked for, as soon as it has
/// started -- and the source stops once no request waits. Requests made while
/// it runs share it, at the finest accuracy any of them asked for when it
/// started. GUI thread.
class Geolocation final : public QObject {
    Q_OBJECT

public:
    enum class Source { GeoClue, Portal };

    /// How finely: GeoClue's levels.
    enum class Accuracy : std::uint32_t { City = 4, Street = 6, Exact = 8 };

    struct Position {
        double latitude = 0;
        double longitude = 0;
        /// Metres, horizontally.
        double accuracy = 0;
        /// Metres above sea level, when the source knows.
        std::optional<double> altitude;
        /// Metres a second, when the source knows.
        std::optional<double> speed;
    };

    /// What a request is answered with: a position, or why there is none.
    struct Outcome {
        /// The App may know where the device is: the source started.
        bool allowed = false;
        /// Refused by the player or the system, as opposed to unavailable.
        bool denied = false;
        std::optional<Position> position;
        /// Why the request is not answered as asked; empty when it is.
        QString error;
    };
    using Answer = std::function<void(const Outcome &)>;

    /// `bus` is where `source` answers: the system's for GeoClue, the session's
    /// for the portal. `desktop_id` names the App to GeoClue's agent; a portal
    /// knows the App already. `parent_window` is the window a portal's dialog
    /// stands over (`x11:<id>`), asked when one starts.
    Geolocation(QDBusConnection bus, Source source, QString desktop_id, std::function<QString()> parent_window,
                QObject *parent = nullptr);
    ~Geolocation() override;

    /// A position at `accuracy` or finer, within `timeout`: failed with "unable
    /// to get location" when none comes by then.
    void locate(Accuracy accuracy, std::chrono::milliseconds timeout, Answer answer);

    /// Whether the App may know where the device is, as the source decides --
    /// asking the player if it has not -- answered once it has.
    void decide(Answer answer);

    /// One request, waiting.
    struct Waiter {
        bool wants_position = false;
        Answer answer;
        QTimer *expiry = nullptr;
    };

private Q_SLOTS:
    void geoclueLocationUpdated(const QDBusObjectPath &old_location, const QDBusObjectPath &new_location);
    void portalLocationUpdated(const QDBusObjectPath &session, const QVariantMap &location);
    void portalResponse(uint response, const QVariantMap &results);

private:
    void join(Waiter waiter, Accuracy accuracy);
    void start();
    void startGeoClue();
    void startPortal();
    /// The source has started: the App may know.
    void started();
    /// The source could not start, or stopped: every request is answered with `outcome`.
    void failed(const Outcome &outcome);
    void reported(const Position &position);
    void stop();
    void settle(std::list<Waiter>::iterator waiter, const Outcome &outcome);

    QDBusConnection bus_;
    const Source source_;
    const QString desktop_id_;
    const std::function<QString()> parent_window_;

    std::list<Waiter> waiters_;
    /// The accuracy the running source was started at.
    Accuracy running_accuracy_ = Accuracy::City;
    bool running_ = false;
    bool started_ = false;
    /// The running source: GeoClue's client, or the portal's session and its
    /// start request.
    QString client_;
    QString session_;
    QString request_;
    /// Tells a run's late replies from the current one's.
    std::uint64_t run_ = 0;
};

}  // namespace migo::linux_host::qt6

#endif  // MIGO_LINUX_QT6_GEOLOCATION_HPP_

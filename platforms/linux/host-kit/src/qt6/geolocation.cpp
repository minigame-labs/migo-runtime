#include "geolocation.hpp"

#include <QDBusError>
#include <QDBusMessage>
#include <QDBusPendingCallWatcher>
#include <QDBusPendingReply>
#include <QDBusVariant>
#include <QTimer>

#include <atomic>
#include <cfloat>
#include <cmath>
#include <utility>

namespace migo::linux_host::qt6 {

namespace {

const QString kGeoClue = QStringLiteral("org.freedesktop.GeoClue2");
const QString kManagerPath = QStringLiteral("/org/freedesktop/GeoClue2/Manager");
const QString kManager = QStringLiteral("org.freedesktop.GeoClue2.Manager");
const QString kClient = QStringLiteral("org.freedesktop.GeoClue2.Client");
const QString kLocation = QStringLiteral("org.freedesktop.GeoClue2.Location");
const QString kProperties = QStringLiteral("org.freedesktop.DBus.Properties");
const QString kPortal = QStringLiteral("org.freedesktop.portal.Desktop");
const QString kPortalPath = QStringLiteral("/org/freedesktop/portal/desktop");
const QString kPortalLocation = QStringLiteral("org.freedesktop.portal.Location");
const QString kPortalRequest = QStringLiteral("org.freedesktop.portal.Request");
const QString kPortalSession = QStringLiteral("org.freedesktop.portal.Session");
const QString kNoPosition = QStringLiteral("unable to get location");

/// The portal's accuracy for GeoClue's level: its scale has no gaps.
uint portalAccuracy(Geolocation::Accuracy accuracy) {
    switch (accuracy) {
        case Geolocation::Accuracy::City:
            return 2;
        case Geolocation::Accuracy::Street:
            return 4;
        case Geolocation::Accuracy::Exact:
            return 5;
    }
    return 5;
}

/// Tokens a portal names its handles by: one sequence for the process, which
/// shares one connection among its Sessions.
std::atomic<std::uint64_t> next_token{1};

Geolocation::Outcome refused(const QDBusError &error) {
    Geolocation::Outcome outcome;
    outcome.denied = error.type() == QDBusError::AccessDenied;
    outcome.error = outcome.denied ? QStringLiteral("location access denied: %1").arg(error.message())
                                   : QStringLiteral("location services are not available: %1").arg(error.message());
    return outcome;
}

/// A position as GeoClue reports it -- the Location object's properties, or the
/// portal's dictionary of the same names -- where an unknown altitude is the
/// lowest double and an unknown speed is negative.
std::optional<Geolocation::Position> positionOf(const QVariantMap &reported) {
    if (!reported.contains(QStringLiteral("Latitude")) || !reported.contains(QStringLiteral("Longitude"))) {
        return std::nullopt;
    }
    Geolocation::Position position;
    position.latitude = reported.value(QStringLiteral("Latitude")).toDouble();
    position.longitude = reported.value(QStringLiteral("Longitude")).toDouble();
    position.accuracy = reported.value(QStringLiteral("Accuracy")).toDouble();
    bool known = false;
    if (const double altitude = reported.value(QStringLiteral("Altitude")).toDouble(&known);
        known && std::isfinite(altitude) && altitude > -DBL_MAX) {
        position.altitude = altitude;
    }
    if (const double speed = reported.value(QStringLiteral("Speed")).toDouble(&known);
        known && std::isfinite(speed) && speed >= 0) {
        position.speed = speed;
    }
    return position;
}

}  // namespace

Geolocation::Geolocation(QDBusConnection bus, Source source, QString desktop_id,
                         std::function<QString()> parent_window, QObject *parent)
    : QObject(parent),
      bus_(std::move(bus)),
      source_(source),
      desktop_id_(std::move(desktop_id)),
      parent_window_(std::move(parent_window)) {}

Geolocation::~Geolocation() {
    // What waits is answered by nobody: the Session it came from is gone.
    waiters_.clear();
    stop();
}

void Geolocation::locate(Accuracy accuracy, std::chrono::milliseconds timeout, Answer answer) {
    auto *expiry = new QTimer(this);
    expiry->setSingleShot(true);
    join(Waiter{true, std::move(answer), expiry}, accuracy);
    const auto waiter = std::prev(waiters_.end());
    connect(expiry, &QTimer::timeout, this, [this, waiter] {
        Outcome outcome;
        outcome.allowed = started_;
        outcome.error = kNoPosition;
        settle(waiter, outcome);
        if (waiters_.empty()) stop();
    });
    expiry->start(timeout);
}

void Geolocation::decide(Answer answer) {
    join(Waiter{false, std::move(answer), nullptr}, Accuracy::City);
    if (running_ && started_) {
        Outcome allowed;
        allowed.allowed = true;
        settle(std::prev(waiters_.end()), allowed);
    }
}

void Geolocation::join(Waiter waiter, Accuracy accuracy) {
    waiters_.push_back(std::move(waiter));
    if (running_) return;
    running_accuracy_ = accuracy;
    start();
}

void Geolocation::settle(std::list<Waiter>::iterator waiter, const Outcome &outcome) {
    Answer reply = std::move(waiter->answer);
    if (waiter->expiry != nullptr) {
        waiter->expiry->stop();
        waiter->expiry->deleteLater();
    }
    waiters_.erase(waiter);
    reply(outcome);
}

void Geolocation::start() {
    running_ = true;
    started_ = false;
    ++run_;
    if (source_ == Source::GeoClue) {
        startGeoClue();
    } else {
        startPortal();
    }
}

// ---- GeoClue, on the system bus --------------------------------------------------------------

void Geolocation::startGeoClue() {
    const std::uint64_t run = run_;
    // A client of this run's own: the App's Sessions share one connection,
    // and GetClient's one client per connection would be theirs to share.
    auto *creating = new QDBusPendingCallWatcher(
        bus_.asyncCall(QDBusMessage::createMethodCall(kGeoClue, kManagerPath, kManager, QStringLiteral("CreateClient"))),
        this);
    connect(creating, &QDBusPendingCallWatcher::finished, this, [this, run](QDBusPendingCallWatcher *call) {
        call->deleteLater();
        const QDBusPendingReply<QDBusObjectPath> created = *call;
        if (run != run_) {
            // Stopped meanwhile: the client is no one's.
            if (!created.isError()) {
                bus_.asyncCall(QDBusMessage::createMethodCall(kGeoClue, kManagerPath, kManager,
                                                              QStringLiteral("DeleteClient"))
                               << QVariant::fromValue(created.value()));
            }
            return;
        }
        if (created.isError()) {
            failed(refused(created.error()));
            return;
        }
        client_ = created.value().path();
        // Sent in order, and handled in order: both are set before it starts.
        const auto set = [this](const QString &property, const QVariant &value) {
            bus_.asyncCall(QDBusMessage::createMethodCall(kGeoClue, client_, kProperties, QStringLiteral("Set"))
                           << kClient << property << QVariant::fromValue(QDBusVariant(value)));
        };
        set(QStringLiteral("DesktopId"), desktop_id_);
        set(QStringLiteral("RequestedAccuracyLevel"), static_cast<uint>(running_accuracy_));
        bus_.connect(kGeoClue, client_, kClient, QStringLiteral("LocationUpdated"), this,
                     SLOT(geoclueLocationUpdated(QDBusObjectPath,QDBusObjectPath)));
        // Starting is when GeoClue's agent decides, asking the player if it
        // has not: the reply waits for that.
        auto *starting = new QDBusPendingCallWatcher(
            bus_.asyncCall(QDBusMessage::createMethodCall(kGeoClue, client_, kClient, QStringLiteral("Start"))), this);
        connect(starting, &QDBusPendingCallWatcher::finished, this, [this, run](QDBusPendingCallWatcher *start) {
            start->deleteLater();
            if (run != run_) return;
            const QDBusPendingReply<> reply = *start;
            if (reply.isError()) {
                failed(refused(reply.error()));
            } else {
                started();
            }
        });
    });
}

void Geolocation::geoclueLocationUpdated(const QDBusObjectPath &, const QDBusObjectPath &new_location) {
    if (!running_) return;
    // Reporting a position is having started, whichever reply comes first.
    if (!started_) started();
    if (!running_) return;
    const std::uint64_t run = run_;
    auto *reading = new QDBusPendingCallWatcher(
        bus_.asyncCall(QDBusMessage::createMethodCall(kGeoClue, new_location.path(), kProperties,
                                                      QStringLiteral("GetAll"))
                       << kLocation),
        this);
    connect(reading, &QDBusPendingCallWatcher::finished, this, [this, run](QDBusPendingCallWatcher *call) {
        call->deleteLater();
        if (run != run_) return;
        const QDBusPendingReply<QVariantMap> read = *call;
        const std::optional<Position> position = read.isError() ? std::nullopt : positionOf(read.value());
        // One that cannot be read is no position: the next update may be.
        if (position.has_value()) reported(*position);
    });
}

// ---- the desktop portal, inside a sandbox -----------------------------------------------------

void Geolocation::startPortal() {
    const std::uint64_t run = run_;
    const QString session_token = QStringLiteral("migo_location_%1").arg(next_token.fetch_add(1));
    QDBusMessage create =
        QDBusMessage::createMethodCall(kPortal, kPortalPath, kPortalLocation, QStringLiteral("CreateSession"));
    create << QVariantMap{{QStringLiteral("session_handle_token"), session_token},
                          {QStringLiteral("distance-threshold"), uint{0}},
                          {QStringLiteral("time-threshold"), uint{0}},
                          {QStringLiteral("accuracy"), portalAccuracy(running_accuracy_)}};
    auto *creating = new QDBusPendingCallWatcher(bus_.asyncCall(create), this);
    connect(creating, &QDBusPendingCallWatcher::finished, this, [this, run](QDBusPendingCallWatcher *call) {
        call->deleteLater();
        const QDBusPendingReply<QDBusObjectPath> created = *call;
        if (run != run_) {
            if (!created.isError()) {
                bus_.asyncCall(QDBusMessage::createMethodCall(kPortal, created.value().path(), kPortalSession,
                                                              QStringLiteral("Close")));
            }
            return;
        }
        if (created.isError()) {
            failed(refused(created.error()));
            return;
        }
        session_ = created.value().path();
        bus_.connect(kPortal, kPortalPath, kPortalLocation, QStringLiteral("LocationUpdated"), this,
                     SLOT(portalLocationUpdated(QDBusObjectPath,QVariantMap)));
        // The request's path is known before the call -- its sender and its
        // token -- so its answer is listened for before it can be sent.
        const QString token = QStringLiteral("migo_location_%1").arg(next_token.fetch_add(1));
        QString sender = bus_.baseService();
        sender.remove(0, 1).replace(QLatin1Char('.'), QLatin1Char('_'));
        request_ = QStringLiteral("%1/request/%2/%3").arg(kPortalPath, sender, token);
        bus_.connect(kPortal, request_, kPortalRequest, QStringLiteral("Response"), this,
                     SLOT(portalResponse(uint,QVariantMap)));
        QDBusMessage start = QDBusMessage::createMethodCall(kPortal, kPortalPath, kPortalLocation, QStringLiteral("Start"));
        start << QVariant::fromValue(QDBusObjectPath(session_)) << parent_window_()
              << QVariantMap{{QStringLiteral("handle_token"), token}};
        auto *starting = new QDBusPendingCallWatcher(bus_.asyncCall(start), this);
        connect(starting, &QDBusPendingCallWatcher::finished, this, [this, run](QDBusPendingCallWatcher *requested) {
            requested->deleteLater();
            if (run != run_) return;
            const QDBusPendingReply<QDBusObjectPath> reply = *requested;
            if (reply.isError()) {
                failed(refused(reply.error()));
                return;
            }
            // A portal older than predictable paths answers on the one it says.
            if (const QString path = reply.value().path(); path != request_) {
                bus_.disconnect(kPortal, request_, kPortalRequest, QStringLiteral("Response"), this,
                                SLOT(portalResponse(uint,QVariantMap)));
                request_ = path;
                bus_.connect(kPortal, request_, kPortalRequest, QStringLiteral("Response"), this,
                             SLOT(portalResponse(uint,QVariantMap)));
            }
        });
    });
}

void Geolocation::portalResponse(uint response, const QVariantMap &) {
    if (!running_ || request_.isEmpty()) return;
    bus_.disconnect(kPortal, request_, kPortalRequest, QStringLiteral("Response"), this,
                    SLOT(portalResponse(uint,QVariantMap)));
    request_.clear();
    // 0 started; 1 the player said no; 2 it could not.
    if (response == 0) {
        started();
        return;
    }
    Outcome outcome;
    outcome.denied = response == 1;
    outcome.error = outcome.denied ? QStringLiteral("location access denied")
                                   : QStringLiteral("location services are not available");
    failed(outcome);
}

void Geolocation::portalLocationUpdated(const QDBusObjectPath &session, const QVariantMap &location) {
    if (!running_ || session.path() != session_) return;
    if (!started_) started();
    if (!running_) return;
    if (const std::optional<Position> position = positionOf(location)) reported(*position);
}

// ---- answering --------------------------------------------------------------------------------

namespace {

/// Answers each of `waiting` with `outcome`, from a list of their own: an
/// answer may ask again.
void answerAll(std::list<Geolocation::Waiter> &waiting, const Geolocation::Outcome &outcome) {
    while (!waiting.empty()) {
        Geolocation::Answer reply = std::move(waiting.front().answer);
        if (QTimer *expiry = waiting.front().expiry; expiry != nullptr) {
            expiry->stop();
            expiry->deleteLater();
        }
        waiting.pop_front();
        reply(outcome);
    }
}

}  // namespace

void Geolocation::started() {
    started_ = true;
    std::list<Waiter> decided;
    for (auto waiter = waiters_.begin(); waiter != waiters_.end();) {
        const auto next = std::next(waiter);
        if (!waiter->wants_position) decided.splice(decided.end(), waiters_, waiter);
        waiter = next;
    }
    if (waiters_.empty()) stop();
    Outcome allowed;
    allowed.allowed = true;
    answerAll(decided, allowed);
}

void Geolocation::failed(const Outcome &outcome) {
    std::list<Waiter> waiting;
    waiting.swap(waiters_);
    stop();
    answerAll(waiting, outcome);
}

void Geolocation::reported(const Position &position) {
    Outcome outcome;
    outcome.allowed = true;
    outcome.position = position;
    std::list<Waiter> waiting;
    waiting.swap(waiters_);
    stop();
    answerAll(waiting, outcome);
}

void Geolocation::stop() {
    if (!running_) return;
    running_ = false;
    started_ = false;
    ++run_;
    if (!client_.isEmpty()) {
        bus_.disconnect(kGeoClue, client_, kClient, QStringLiteral("LocationUpdated"), this,
                        SLOT(geoclueLocationUpdated(QDBusObjectPath,QDBusObjectPath)));
        bus_.asyncCall(QDBusMessage::createMethodCall(kGeoClue, client_, kClient, QStringLiteral("Stop")));
        bus_.asyncCall(QDBusMessage::createMethodCall(kGeoClue, kManagerPath, kManager, QStringLiteral("DeleteClient"))
                       << QVariant::fromValue(QDBusObjectPath(client_)));
        client_.clear();
    }
    if (!request_.isEmpty()) {
        bus_.disconnect(kPortal, request_, kPortalRequest, QStringLiteral("Response"), this,
                        SLOT(portalResponse(uint,QVariantMap)));
        // A dialog still asking goes with the request.
        bus_.asyncCall(QDBusMessage::createMethodCall(kPortal, request_, kPortalRequest, QStringLiteral("Close")));
        request_.clear();
    }
    if (!session_.isEmpty()) {
        bus_.disconnect(kPortal, kPortalPath, kPortalLocation, QStringLiteral("LocationUpdated"), this,
                        SLOT(portalLocationUpdated(QDBusObjectPath,QVariantMap)));
        bus_.asyncCall(QDBusMessage::createMethodCall(kPortal, session_, kPortalSession, QStringLiteral("Close")));
        session_.clear();
    }
}

}  // namespace migo::linux_host::qt6

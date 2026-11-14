#ifndef MIGO_WINDOWS_HOST_KIT_GEOLOCATION_HPP_
#define MIGO_WINDOWS_HOST_KIT_GEOLOCATION_HPP_

// Internal to the Host Kit, not installed: where the device is, as Windows says.

#include <chrono>
#include <functional>
#include <optional>
#include <string>

namespace migo::windows_host {

/// Where the device is, from Windows' location service (Windows.Devices.
/// Geolocation) -- the Linux Host Kit's `Geolocation`, for Win32. Windows
/// decides whether the App may know: its privacy settings, which the player
/// keeps, for every desktop App ("let desktop apps access your location").
/// Each request is its own; the system shares the source among them. The
/// answers come on whatever thread the system completes them on. Positions are
/// WGS-84, as the service reports them.
class Geolocation final {
public:
    /// How finely.
    enum class Accuracy { City, Street, Exact };

    struct Position {
        double latitude = 0;
        double longitude = 0;
        /// Metres, horizontally.
        double accuracy = 0;
        /// Metres above sea level, and how sure, when the source knows.
        std::optional<double> altitude;
        std::optional<double> altitude_accuracy;
        /// Metres a second, when the source knows.
        std::optional<double> speed;
    };

    /// What a request is answered with: a position, or why there is none.
    struct Outcome {
        /// The App may know where the device is.
        bool allowed = false;
        /// Refused by the player or the system, as opposed to unavailable.
        bool denied = false;
        std::optional<Position> position;
        /// Why the request is not answered as asked; empty when it is.
        std::string error;
    };
    using Answer = std::function<void(const Outcome &)>;

    /// A position at `accuracy` or finer, within `timeout`: failed with "unable
    /// to get location" when none comes by then, "auth deny" when the App may
    /// not know.
    static void locate(Accuracy accuracy, std::chrono::milliseconds timeout, Answer answer);

    /// Whether the App may know where the device is, as Windows decides.
    static void decide(Answer answer);
};

}  // namespace migo::windows_host

#endif  // MIGO_WINDOWS_HOST_KIT_GEOLOCATION_HPP_

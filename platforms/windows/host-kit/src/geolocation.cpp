#include "geolocation.hpp"

#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#ifndef NOMINMAX
#define NOMINMAX
#endif
#include <windows.h>

#include <winrt/Windows.Devices.Geolocation.h>
#include <winrt/Windows.Foundation.h>

#include <utility>

namespace migo::windows_host {

namespace {

namespace geo = winrt::Windows::Devices::Geolocation;
using winrt::Windows::Foundation::AsyncStatus;
using winrt::Windows::Foundation::IAsyncOperation;
using winrt::Windows::Foundation::TimeSpan;

constexpr char kNoPosition[] = "unable to get location";
constexpr char kUnavailable[] = "location services are not available";
constexpr char kDenied[] = "auth deny";

Geolocation::Outcome failed(const char *why) {
    Geolocation::Outcome outcome;
    outcome.error = why;
    return outcome;
}

}  // namespace

void Geolocation::decide(Answer answer) {
    try {
        geo::Geolocator::RequestAccessAsync().Completed(
            [answer](IAsyncOperation<geo::GeolocationAccessStatus> const &request, AsyncStatus status) {
                if (status != AsyncStatus::Completed) {
                    answer(failed(kUnavailable));
                    return;
                }
                Outcome outcome;
                switch (request.GetResults()) {
                    case geo::GeolocationAccessStatus::Allowed:
                        outcome.allowed = true;
                        break;
                    case geo::GeolocationAccessStatus::Denied:
                        outcome.denied = true;
                        outcome.error = kDenied;
                        break;
                    default:
                        outcome.error = kUnavailable;
                        break;
                }
                answer(outcome);
            });
    } catch (const winrt::hresult_error &) {
        answer(failed(kUnavailable));
    }
}

void Geolocation::locate(Accuracy accuracy, std::chrono::milliseconds timeout, Answer answer) {
    decide([accuracy, timeout, answer = std::move(answer)](const Outcome &decided) {
        if (!decided.allowed) {
            answer(decided);
            return;
        }
        try {
            geo::Geolocator locator;
            switch (accuracy) {
                case Accuracy::City:
                    locator.DesiredAccuracyInMeters(3000U);
                    break;
                case Accuracy::Street:
                    locator.DesiredAccuracyInMeters(100U);
                    break;
                case Accuracy::Exact:
                    locator.DesiredAccuracy(geo::PositionAccuracy::High);
                    break;
            }
            // A fresh position, within the time content waits.
            locator.GetGeopositionAsync(TimeSpan::zero(), std::chrono::duration_cast<TimeSpan>(timeout))
                .Completed([answer, locator](IAsyncOperation<geo::Geoposition> const &operation, AsyncStatus status) {
                    Outcome outcome;
                    outcome.allowed = true;
                    if (status != AsyncStatus::Completed) {
                        outcome.error = kNoPosition;
                        answer(outcome);
                        return;
                    }
                    try {
                        const geo::Geocoordinate coordinate = operation.GetResults().Coordinate();
                        const geo::BasicGeoposition at = coordinate.Point().Position();
                        Position position;
                        position.latitude = at.Latitude;
                        position.longitude = at.Longitude;
                        position.accuracy = coordinate.Accuracy();
                        // An altitude is known where its accuracy is.
                        if (const auto altitude_accuracy = coordinate.AltitudeAccuracy()) {
                            position.altitude = at.Altitude;
                            position.altitude_accuracy = altitude_accuracy.Value();
                        }
                        if (const auto speed = coordinate.Speed()) position.speed = speed.Value();
                        outcome.position = position;
                    } catch (const winrt::hresult_error &) {
                        outcome.error = kNoPosition;
                    }
                    answer(outcome);
                });
        } catch (const winrt::hresult_error &) {
            answer(failed(kUnavailable));
        }
    });
}

}  // namespace migo::windows_host

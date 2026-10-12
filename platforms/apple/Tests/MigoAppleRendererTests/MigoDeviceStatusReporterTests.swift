import Foundation
import MigoEngine
import UserNotifications
import XCTest

@testable import MigoAppleRenderer

/// The system's notification grants as the engine words them, and every other
/// grant left to its own reporter.
final class MigoDeviceStatusReporterTests: XCTestCase {
    private let authorized = UInt8(MIGO_AUTHORIZATION_AUTHORIZED)
    private let denied = UInt8(MIGO_AUTHORIZATION_DENIED)
    private let undetermined = UInt8(MIGO_AUTHORIZATION_NOT_DETERMINED)

    func testNotificationGrantsAreTheSystemsAndTheRestAreLeftAlone() {
        var setting = MigoAppAuthorizeSetting()
        setting.camera = denied
        MigoDeviceStatusReporter.setNotifications(
            &setting, notification: .authorized, alert: .enabled, badge: .disabled, sound: .notSupported)
        XCTAssertEqual(setting.notification, authorized)
        XCTAssertEqual(setting.notification_alert, authorized)
        XCTAssertEqual(setting.notification_badge, denied)
        XCTAssertEqual(setting.notification_sound, undetermined)
        // Another reporter's field is theirs.
        XCTAssertEqual(setting.camera, denied)
        for other in [setting.album, setting.bluetooth, setting.location, setting.microphone, setting.phone_calendar] {
            XCTAssertEqual(other, undetermined)
        }
        XCTAssertEqual(setting.location_reduced_accuracy, 0)
    }

    func testEachNotificationStatusHasItsWord() {
        func word(_ status: UNAuthorizationStatus) -> UInt8 {
            var setting = MigoAppAuthorizeSetting()
            MigoDeviceStatusReporter.setNotifications(
                &setting, notification: status, alert: .notSupported, badge: .notSupported, sound: .notSupported)
            return setting.notification
        }
        XCTAssertEqual(word(.notDetermined), undetermined)
        XCTAssertEqual(word(.denied), denied)
        XCTAssertEqual(word(.authorized), authorized)
        // Delivered quietly, but delivered.
        XCTAssertEqual(word(.provisional), authorized)
    }
}

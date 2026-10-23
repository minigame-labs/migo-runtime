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

    #if os(macOS)
        func testAMacsBatteryIsItsInternalOneAsTheAPIWordsIt() {
            func battery(
                _ type: String = kIOPSInternalBatteryType, present: Bool = true, current: Int, max: Int = 100,
                state: String = kIOPSBatteryPowerValue, charging: Bool = false, charged: Bool = false
            ) -> [String: Any] {
                [
                    kIOPSTypeKey: type, kIOPSIsPresentKey: present, kIOPSCurrentCapacityKey: current,
                    kIOPSMaxCapacityKey: max, kIOPSPowerSourceStateKey: state, kIOPSIsChargingKey: charging,
                    kIOPSIsChargedKey: charged,
                ]
            }
            func read(_ sources: [[String: Any]]) -> [String]? {
                MigoDeviceStatusReporter.internalBattery(in: sources).map { ["\($0.levelPercent)", "\($0.charging)"] }
            }
            XCTAssertEqual(read([battery(current: 57)]), ["57", "false"])
            XCTAssertEqual(read([battery(current: 57, state: kIOPSACPowerValue, charging: true)]), ["57", "true"])
            // Full on the charger is charging, as iOS's `.full` is.
            XCTAssertEqual(read([battery(current: 100, state: kIOPSACPowerValue, charged: true)]), ["100", "true"])
            // Held below full on the charger, the battery is not charging.
            XCTAssertEqual(read([battery(current: 80, state: kIOPSACPowerValue)]), ["80", "false"])
            // A capacity in mAh reads as its fraction.
            XCTAssertEqual(read([battery(current: 2_995, max: 6_000)]), ["50", "false"])
            // The internal battery, wherever the system lists it.
            XCTAssertEqual(read([battery(kIOPSUPSType, current: 10), battery(current: 64)]), ["64", "false"])
            // A desktop: no source, or only a UPS, or a bay with no battery in it.
            XCTAssertNil(read([]))
            XCTAssertNil(read([battery(kIOPSUPSType, current: 90)]))
            XCTAssertNil(read([battery(present: false, current: 0)]))
            XCTAssertNil(read([battery(current: 0, max: 0)]))
        }
    #endif
}

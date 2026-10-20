import Foundation
import MigoEngine
import Network
import UserNotifications

#if os(iOS)
    import UIKit
#elseif os(macOS)
    import AppKit
    import CoreWLAN
#endif

/// Tells one engine session what the device's network and battery are, what
/// the app says about itself (`getAppBaseInfo`: its bundle identifier, the
/// language its interface is in, its text scale), the Wi-Fi switch
/// (`getSystemSetting`) and what the system has granted the app
/// (`getAppAuthorizeSetting`), now and at every change, and passes on the
/// system's memory warnings (`onMemoryWarning`), for as long as the session
/// runs.
///
/// The engine answers content's `getNetworkType` and `getBatteryInfo` from the
/// last report and forwards a network change while content listens, so the host
/// reports and never answers: the platform already notifies changes, and a read
/// between two notifications is the last value anyway.
///
/// The battery is reported on iOS only. A Mac's is a laptop's power source,
/// read through IOKit, and a desktop has none; content there hears "not
/// supported", which is the API's answer for a device without a battery.
///
/// Of the system's grants it reports what it reads without the app declaring a
/// purpose for it: notifications. Location, the camera, the microphone, the
/// photo library, Bluetooth and calendars are not read here -- App Store
/// review asks for a purpose string for every such API a binary references,
/// used or not -- but by the optional capability that uses each
/// (`MigoSystemCapability`), and read as "not determined" without one. Grants
/// are read again whenever the app comes back to the front, which is where a
/// player who changed one in Settings returns.
///
/// Main thread only, like the session it reports to.
public final class MigoDeviceStatusReporter {
    private weak var session: MigoEngineSession?
    private let monitor = NWPathMonitor()
    /// The monitor's own queue: its first path is waited for from the main
    /// thread (see `start()`), which a main-queue handler could never deliver.
    private let monitorQueue = DispatchQueue(label: "dev.migo.device-status")
    private var observers: [NSObjectProtocol] = []
    /// The Wi-Fi switch, as last reported.
    private var wifiOn: Bool?
    #if os(macOS)
        /// The system's memory-pressure notifications, which a Mac has instead
        /// of an app memory warning.
        private var memoryPressure: DispatchSourceMemoryPressure?
        /// The Wi-Fi interface's power, which CoreWLAN reads exactly.
        private var wifi: WiFiPower?
    #endif
    #if os(iOS)
        /// The app's own setting, put back on `stop()`: battery monitoring is
        /// process-wide, and turning it on for a game is not turning it on for
        /// the app.
        private var batteryMonitoringWasEnabled = false
    #endif
    private var running = false

    /// What `start()` and the monitor's handler share while `start()` waits
    /// for the first path. Touched only on `monitorQueue` once the monitor runs.
    private final class Startup {
        var waiting = true
        var path: NWPath?
        let arrived = DispatchSemaphore(value: 0)
    }

    public init(session: MigoEngineSession) {
        self.session = session
    }

    deinit {
        // `stop()` is the owner's job; this only keeps a forgotten one from
        // leaving the monitor and the observers running.
        monitor.cancel()
        observers.forEach(NotificationCenter.default.removeObserver)
    }

    public func start() {
        precondition(Thread.isMainThread, "MigoDeviceStatusReporter is main-thread only")
        guard !running else { return }
        running = true
        // The first path is reported before this returns, so it is in the
        // engine before content can ask: the V8 lane evaluates content within
        // milliseconds of `loadContent`, and a report still queued behind it
        // would answer the game's first `getNetworkType` "not supported". The
        // monitor answers its first path at once -- the wait is a bound, not a
        // cost. Later changes hop to the main thread, where the session lives.
        let startup = Startup()
        monitor.pathUpdateHandler = { [weak self] path in
            // On `monitorQueue`, as is every access to `startup`.
            if startup.waiting {
                startup.path = path
                startup.arrived.signal()
            } else {
                DispatchQueue.main.async { self?.report(path) }
            }
        }
        monitor.start(queue: monitorQueue)
        if startup.arrived.wait(timeout: .now() + .milliseconds(250)) == .timedOut {
            NSLog("MigoDeviceStatusReporter: no network path within 250 ms; content reads it once one arrives")
        }
        // Closed on the monitor's queue, so a path is either taken here or
        // reported by the handler -- never both, never neither.
        let path: NWPath? = monitorQueue.sync {
            startup.waiting = false
            return startup.path
        }
        if let path { report(path) }
        reportHostApp()
        reportAuthorizations()
        let activation: Notification.Name
        #if os(iOS)
            activation = UIApplication.didBecomeActiveNotification
        #else
            activation = NSApplication.didBecomeActiveNotification
        #endif
        observers.append(
            NotificationCenter.default.addObserver(forName: activation, object: nil, queue: .main) {
                [weak self] _ in
                guard let self else { return }
                self.reportAuthorizations()
                // A proxy set while the app was away changes no path.
                self.report(self.monitor.currentPath)
            })
        #if os(macOS)
            let wifi = WiFiPower { [weak self] in self?.reportWiFi() }
            self.wifi = wifi
            reportWiFi()
            let pressure = DispatchSource.makeMemoryPressureSource(eventMask: [.warning, .critical], queue: .main)
            pressure.setEventHandler { [weak self, weak pressure] in
                guard let self, self.running, let event = pressure?.data else { return }
                let level = event.contains(.critical) ? MIGO_MEMORY_WARNING_CRITICAL : MIGO_MEMORY_WARNING_LOW
                self.session?.postMemoryWarning(MigoMemoryWarningLevel(level))
            }
            pressure.resume()
            memoryPressure = pressure
        #endif
        #if os(iOS)
            observers.append(
                NotificationCenter.default.addObserver(
                    forName: UIContentSizeCategory.didChangeNotification, object: nil, queue: .main
                ) { [weak self] _ in self?.reportHostApp() })
            // An app memory warning is the system about to end the process.
            observers.append(
                NotificationCenter.default.addObserver(
                    forName: UIApplication.didReceiveMemoryWarningNotification, object: nil, queue: .main
                ) { [weak self] _ in
                    guard let self, self.running else { return }
                    self.session?.postMemoryWarning(MigoMemoryWarningLevel(MIGO_MEMORY_WARNING_CRITICAL))
                })
            let device = UIDevice.current
            batteryMonitoringWasEnabled = device.isBatteryMonitoringEnabled
            device.isBatteryMonitoringEnabled = true
            let center = NotificationCenter.default
            for name in [
                UIDevice.batteryLevelDidChangeNotification, UIDevice.batteryStateDidChangeNotification,
                Notification.Name.NSProcessInfoPowerStateDidChange,
            ] {
                observers.append(
                    center.addObserver(forName: name, object: nil, queue: .main) { [weak self] _ in
                        self?.reportBattery()
                    })
            }
            reportBattery()
        #endif
    }

    public func stop() {
        guard running else { return }
        running = false
        #if os(macOS)
            memoryPressure?.cancel()
            memoryPressure = nil
            wifi = nil
        #endif
        wifiOn = nil
        monitor.cancel()
        observers.forEach(NotificationCenter.default.removeObserver)
        observers = []
        #if os(iOS)
            UIDevice.current.isBatteryMonitoringEnabled = batteryMonitoringWasEnabled
        #endif
    }

    /// The API's vocabulary: a wired connection is `wifi`, the nearest thing it
    /// has, and a cellular one is `unknown` -- its generation is behind a
    /// telephony API a game should not need, and Android reports the same.
    static func networkType(of path: NWPath) -> (MigoNetworkType, Bool) {
        guard path.status == .satisfied else { return (MigoNetworkType(MIGO_NETWORK_NONE), false) }
        if path.usesInterfaceType(.wifi) || path.usesInterfaceType(.wiredEthernet) {
            return (MigoNetworkType(MIGO_NETWORK_WIFI), true)
        }
        return (MigoNetworkType(MIGO_NETWORK_UNKNOWN), true)
    }

    /// Whether the system routes the network through a proxy -- one set by hand
    /// or a configuration script -- as `getNetworkType`'s `hasSystemProxy` says.
    /// The keys are the system's own, read the same on iOS and macOS.
    static func systemProxy() -> Bool {
        guard let settings = CFNetworkCopySystemProxySettings()?.takeRetainedValue() as? [String: Any] else {
            return false
        }
        return ["HTTPEnable", "HTTPSEnable", "SOCKSEnable", "ProxyAutoConfigEnable"].contains {
            (settings[$0] as? NSNumber)?.boolValue == true
        }
    }

    private func report(_ path: NWPath) {
        guard running else { return }
        let (type, connected) = Self.networkType(of: path)
        var signal: Int32 = 0
        #if os(macOS)
            // CoreWLAN reads the Wi-Fi signal; iOS offers apps no read of one.
            if type == MigoNetworkType(MIGO_NETWORK_WIFI), path.usesInterfaceType(.wifi), let wifi {
                signal = wifi.signalStrengthDBm
            }
        #endif
        session?.reportNetwork(
            type, connected: connected, systemProxy: Self.systemProxy(), signalStrengthDBm: signal)
        #if os(iOS)
            // iOS offers apps no read of the Wi-Fi switch. A path with a Wi-Fi
            // interface has it on; a switch that is on with no network joined
            // reads as off.
            report(wifiOn: path.availableInterfaces.contains { $0.type == .wifi })
        #endif
    }

    #if os(macOS)
        private func reportWiFi() {
            guard let wifi else { return }
            report(wifiOn: wifi.isOn)
        }

        /// CoreWLAN's Wi-Fi interface: its power, and each change of it. Its own
        /// client, so an app's use of the shared one is left alone.
        private final class WiFiPower: NSObject, CWEventDelegate {
            private let client = CWWiFiClient()
            private let changed: () -> Void

            init(changed: @escaping () -> Void) {
                self.changed = changed
                super.init()
                client.delegate = self
                do {
                    try client.startMonitoringEvent(with: .powerDidChange)
                } catch {
                    NSLog("MigoDeviceStatusReporter: CoreWLAN will not report Wi-Fi power changes: \(error)")
                }
            }

            deinit {
                try? client.stopMonitoringAllEvents()
            }

            /// Off on a Mac with no Wi-Fi interface.
            var isOn: Bool { client.interface()?.powerOn() ?? false }

            /// The received signal of the network joined, in dBm; 0 when none is
            /// joined, which is the API's "unknown".
            var signalStrengthDBm: Int32 { Int32(clamping: min(client.interface()?.rssiValue() ?? 0, 0)) }

            func powerStateDidChangeForWiFiInterface(withName interfaceName: String) {
                DispatchQueue.main.async { [weak self] in self?.changed() }
            }
        }
    #endif

    /// The system switch the SDK reads itself: Wi-Fi. Location services and
    /// Bluetooth are the capabilities' to read.
    private func report(wifiOn on: Bool) {
        guard running, on != wifiOn else { return }
        wifiOn = on
        session?.setSystemSetting(MigoSystemSettingFlags(MIGO_SYSTEM_SETTING_FLAG_WIFI_ENABLED), enabled: on)
    }

    /// The notification grants, read from the system -- asynchronously, so a
    /// game that asks before the answer arrives reads "not determined", which
    /// is what the engine answers until the first report.
    private func reportAuthorizations() {
        guard running else { return }
        // UserNotifications answers for an application bundle only, and raises
        // in any other process -- a command-line host, a test runner -- which
        // has no notification setting to read.
        guard Bundle.main.bundleIdentifier != nil, Bundle.main.bundleURL.pathExtension == "app" else { return }
        UNUserNotificationCenter.current().getNotificationSettings { [weak self] settings in
            let (status, alert, badge, sound) =
                (settings.authorizationStatus, settings.alertSetting, settings.badgeSetting, settings.soundSetting)
            DispatchQueue.main.async {
                guard let self, self.running else { return }
                self.session?.updateAppAuthorization {
                    Self.setNotifications(&$0, notification: status, alert: alert, badge: badge, sound: sound)
                }
            }
        }
    }

    /// The notification grants as the engine words them; the other fields are
    /// other reporters' and are left as they are.
    static func setNotifications(
        _ setting: inout MigoAppAuthorizeSetting, notification: UNAuthorizationStatus,
        alert: UNNotificationSetting, badge: UNNotificationSetting, sound: UNNotificationSetting
    ) {
        switch notification {
        case .notDetermined: setting.notification = UInt8(MIGO_AUTHORIZATION_NOT_DETERMINED)
        case .denied: setting.notification = UInt8(MIGO_AUTHORIZATION_DENIED)
        // Authorized, provisionally (quietly delivered) or for an App Clip's
        // while: notifications reach the player.
        default: setting.notification = UInt8(MIGO_AUTHORIZATION_AUTHORIZED)
        }
        func word(_ value: UNNotificationSetting) -> UInt8 {
            switch value {
            case .enabled: return UInt8(MIGO_AUTHORIZATION_AUTHORIZED)
            case .disabled: return UInt8(MIGO_AUTHORIZATION_DENIED)
            default: return UInt8(MIGO_AUTHORIZATION_NOT_DETERMINED)
            }
        }
        setting.notification_alert = word(alert)
        setting.notification_badge = word(badge)
        setting.notification_sound = word(sound)
    }

    /// The app's id, the locale its interface runs in -- `Locale.current`
    /// carries the app's own language, chosen per app on iOS, with the region
    /// the person set -- and on iOS the text scale the person chose, as the
    /// body style's size against its standard one. A Mac has no system text
    /// scale; it is not reported there, which is the standard.
    private func reportHostApp() {
        guard running else { return }
        #if os(iOS)
            let fontScale = Float(UIFontMetrics(forTextStyle: .body).scaledValue(for: Self.standardBodySize) / Self.standardBodySize)
        #else
            let fontScale: Float = 0
        #endif
        session?.setHostAppInfo(
            appId: Bundle.main.bundleIdentifier, language: Locale.current.identifier, fontScale: fontScale)
    }

    #if os(iOS)
        /// The body style's size at the standard content size category.
        private static let standardBodySize: CGFloat = 17

        private func reportBattery() {
            guard running else { return }
            let device = UIDevice.current
            // -1 while the level is unknown -- the simulator, for one. No
            // report then, rather than a made-up level.
            guard device.batteryLevel >= 0 else { return }
            let charging = device.batteryState == .charging || device.batteryState == .full
            session?.setBatteryStatus(
                levelPercent: UInt32((device.batteryLevel * 100).rounded()), charging: charging,
                lowPowerMode: ProcessInfo.processInfo.isLowPowerModeEnabled)
        }
    #endif
}

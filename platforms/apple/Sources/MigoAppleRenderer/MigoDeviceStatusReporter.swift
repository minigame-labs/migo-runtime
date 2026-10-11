import Foundation
import MigoEngine
import Network

#if os(iOS)
    import UIKit
#endif

/// Tells one engine session what the device's network and battery are, and what
/// the app says about itself (`getAppBaseInfo`: its bundle identifier, the
/// language its interface is in, its text scale), now and at every change, and
/// passes on the system's memory warnings (`onMemoryWarning`), for as long as
/// the session runs.
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
/// Main thread only, like the session it reports to.
public final class MigoDeviceStatusReporter {
    private weak var session: MigoEngineSession?
    private let monitor = NWPathMonitor()
    /// The monitor's own queue: its first path is waited for from the main
    /// thread (see `start()`), which a main-queue handler could never deliver.
    private let monitorQueue = DispatchQueue(label: "dev.migo.device-status")
    private var observers: [NSObjectProtocol] = []
    #if os(macOS)
        /// The system's memory-pressure notifications, which a Mac has instead
        /// of an app memory warning.
        private var memoryPressure: DispatchSourceMemoryPressure?
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
        #if os(macOS)
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
        #endif
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

    private func report(_ path: NWPath) {
        guard running else { return }
        let (type, connected) = Self.networkType(of: path)
        session?.setNetworkStatus(type, connected: connected)
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

import Foundation
import Network

#if os(iOS)
    import UIKit
#endif

/// Tells one engine session what the device's network and battery are, now and
/// at every change, for as long as the session runs.
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
    private var observers: [NSObjectProtocol] = []
    #if os(iOS)
        /// The app's own setting, put back on `stop()`: battery monitoring is
        /// process-wide, and turning it on for a game is not turning it on for
        /// the app.
        private var batteryMonitoringWasEnabled = false
    #endif
    private var running = false

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
        monitor.pathUpdateHandler = { [weak self] path in self?.report(path) }
        // The main queue, so each report reaches the session on the thread
        // the session is used from. The first update is the current path.
        monitor.start(queue: .main)
        #if os(iOS)
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

    #if os(iOS)
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

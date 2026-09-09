import Foundation
import MigoProbeCore
import WebKit

#if canImport(UIKit)
    import UIKit
#endif

/// What the device was, captured at the moment a record is written.
///
/// Every field here is one the decision tools hold fixed when they compare
/// arms: two records that differ in the hardware, the OS build or the WebKit
/// build are not two samples of the same question. A harness that guessed any
/// of them -- a hardcoded model name, a marketing version standing in for a
/// build -- would put records from two conditions into one comparison and the
/// tools would have no way to tell.
public struct MigoProbeEnvironment: Sendable {
    public var deviceClass: MigoProbeDeviceClass
    public var hardwareIdentifier: String
    public var ramBytes: UInt64
    public var osVersion: String
    public var osBuild: String
    public var webkitBuild: String
    public var appBuild: String

    /// The WebKit build, parsed out of the user agent once and cached.
    ///
    /// `evaluateJavaScript` is asynchronous and a record is assembled
    /// synchronously, so the agent is read when the gate starts and reused. A
    /// blocking read on the main thread would deadlock against the very web
    /// view being asked.
    private static let userAgentLock = NSLock()
    nonisolated(unsafe) private static var cachedUserAgent: String?

    public static func primeUserAgent(webView: WKWebView, completion: @escaping () -> Void) {
        webView.evaluateJavaScript("navigator.userAgent") { value, _ in
            userAgentLock.lock()
            cachedUserAgent = value as? String
            userAgentLock.unlock()
            completion()
        }
    }

    public static func capture(webView: WKWebView) -> MigoProbeEnvironment {
        MigoProbeEnvironment(
            deviceClass: currentDeviceClass,
            hardwareIdentifier: currentHardwareIdentifier,
            ramBytes: ProcessInfo.processInfo.physicalMemory,
            osVersion: currentOSVersion,
            osBuild: sysctlString("kern.osversion") ?? "unknown",
            webkitBuild: currentWebKitBuild,
            appBuild: currentAppBuild)
    }

    /// The simulator is recorded rather than refused. The decision tools
    /// exclude it, which is the right place for that rule: a simulator run is
    /// how the harness itself is debugged, and a harness that wrote nothing
    /// there would give no signal that it ran at all.
    static var currentDeviceClass: MigoProbeDeviceClass {
        #if targetEnvironment(simulator)
            return .simulator
        #else
            return .device
        #endif
    }

    /// `iPhone14,5` and not `iPhone 13`. The marketing name maps many machines
    /// onto one string, and two of them are different conditions.
    static var currentHardwareIdentifier: String {
        #if targetEnvironment(simulator)
            // Inside the simulator `hw.machine` is the Mac's. The environment
            // carries what is being simulated, and falling back to the host
            // would silently label the record with a machine that was not
            // under test.
            if let simulated = ProcessInfo.processInfo.environment["SIMULATOR_MODEL_IDENTIFIER"] {
                return simulated
            }
            return "simulator-unknown"
        #else
            return sysctlString("hw.machine") ?? "unknown"
        #endif
    }

    static var currentOSVersion: String {
        let version = ProcessInfo.processInfo.operatingSystemVersion
        return "\(version.majorVersion).\(version.minorVersion).\(version.patchVersion)"
    }

    static var currentAppBuild: String {
        let info = Bundle.main.infoDictionary
        let build = info?["CFBundleVersion"] as? String ?? "0"
        let short = info?["CFBundleShortVersionString"] as? String ?? "0"
        return "\(short) (\(build))"
    }

    static var currentWebKitBuild: String {
        userAgentLock.lock()
        let agent = cachedUserAgent
        userAgentLock.unlock()
        guard let agent else { return "unknown" }
        return webKitBuild(fromUserAgent: agent) ?? "unknown"
    }

    /// The `AppleWebKit/<build>` token, or nil when the agent does not carry
    /// one. Parsed rather than assumed: the agent string is the only public
    /// place the WebContent build appears, and it is a held-fixed field.
    public static func webKitBuild(fromUserAgent agent: String) -> String? {
        let marker = "AppleWebKit/"
        guard let range = agent.range(of: marker) else { return nil }
        let rest = agent[range.upperBound...]
        let build = rest.prefix { !$0.isWhitespace }
        return build.isEmpty ? nil : String(build)
    }

    static func sysctlString(_ name: String) -> String? {
        var size = 0
        guard sysctlbyname(name, nil, &size, nil, 0) == 0, size > 0 else { return nil }
        var buffer = [CChar](repeating: 0, count: size)
        guard sysctlbyname(name, &buffer, &size, nil, 0) == 0 else { return nil }
        return String(cString: buffer)
    }
}

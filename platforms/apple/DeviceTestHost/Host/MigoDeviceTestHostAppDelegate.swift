import UIKit

/// The application the renderer and Performance+ tests are hosted in on a phone.
///
/// It exists for one refusal. SwiftPM test targets are tool-hosted, and
/// `xcodebuild test` will not run tool-hosted tests on a device:
/// "Tool-hosted testing is unavailable on device destinations. Select a host
/// application for the test target." An Xcode project's unit-test bundle can
/// name a host application, and a package's test target cannot. So this is the
/// host, and `MigoDeviceTests` compiles the package's own test directories --
/// the same files the simulator runs, not copies of them.
///
/// It does nothing else, on purpose. The tests build their own windows, engines
/// and sessions; a host that set up state of its own would be state every test
/// on the phone inherited and no test on the simulator did.
@main
final class MigoDeviceTestHostAppDelegate: UIResponder, UIApplicationDelegate {
    var window: UIWindow?

    func application(
        _ application: UIApplication,
        didFinishLaunchingWithOptions options: [UIApplication.LaunchOptionsKey: Any]? = nil
    ) -> Bool {
        let window = UIWindow(frame: UIScreen.main.bounds)
        window.rootViewController = UIViewController()
        window.makeKeyAndVisible()
        self.window = window
        // A locked phone suspends the host, and an off-screen WKWebView in a
        // suspended app stops running JavaScript -- a run that outlasts the idle
        // timer would measure the lock screen.
        application.isIdleTimerDisabled = true
        return true
    }
}

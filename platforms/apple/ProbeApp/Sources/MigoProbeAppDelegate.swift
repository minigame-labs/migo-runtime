import UIKit

/// The G0 probe app.
///
/// It is deliberately the thinnest thing that can host measurement gate 1: a
/// window, a web view, and a button. Everything that decides an answer lives in
/// `MigoProbeHarness`, in the engine-free package that the free macOS runner
/// builds and tests on every pull request. What is here is what cannot be
/// tested without a device, and keeping that set small is the point -- code in
/// this target is code no lane compiles until somebody opens Xcode.
@main
final class MigoProbeAppDelegate: UIResponder, UIApplicationDelegate {
    var window: UIWindow?

    func application(
        _ application: UIApplication,
        didFinishLaunchingWithOptions options: [UIApplication.LaunchOptionsKey: Any]? = nil
    ) -> Bool {
        let window = UIWindow(frame: UIScreen.main.bounds)
        window.rootViewController = UINavigationController(
            rootViewController: MigoProbeViewController())
        window.makeKeyAndVisible()
        self.window = window

        // A4: an unmounted or hidden WKWebView is killed or throttled, and a
        // gate run against throttled WebContent measures the throttling. The
        // screen is therefore kept awake for the length of a run rather than
        // relying on somebody tapping the phone every thirty seconds.
        application.isIdleTimerDisabled = true
        return true
    }
}

import MigoProbeCore
import MigoProbeHarness
import UIKit
import WebKit

/// The bench screen: mount the gate's web view, run it, write the records out.
///
/// The records go to the app's Documents directory, which is exposed through
/// `UIFileSharingEnabled` so an operator can pull them off with the Files app or
/// `devicectl` without a debugger attached. A number read off a screen is a
/// number nobody can re-check, and the whole discipline of this gate is that
/// its output is a file the decision tools consume.
final class MigoProbeViewController: UIViewController {

    private var gate: MigoCapabilityGate?
    private var webView: WKWebView?
    private let status = UILabel()
    private let output = UITextView()
    private let runButton = UIButton(type: .system)
    private let lockdownControl = UISegmentedControl(items: ["Lockdown ?", "off", "on"])
    private let promptControl = UISegmentedControl(items: ["Alert ?", "none", "shown"])

    override func viewDidLoad() {
        super.viewDidLoad()
        title = "Migo capability gate"
        view.backgroundColor = .systemBackground

        status.numberOfLines = 0
        status.font = .monospacedSystemFont(ofSize: 12, weight: .regular)
        status.text = "idle"

        runButton.setTitle("Run gate 1", for: .normal)
        runButton.addTarget(self, action: #selector(runTapped), for: .touchUpInside)

        // Two answers no API reports, so the operator declares them and the
        // record says who said so. Defaulting them to the common case is how a
        // Lockdown-Mode device's numbers get filed as a JIT device's.
        //
        // The launch arguments seed the controls rather than bypassing them, so
        // there is one place the run reads its attestation from whether a person
        // tapped it in or a script declared it. A run driven by
        // `scripts/run-apple-probe.sh` shows on screen exactly what it will
        // record.
        lockdownControl.selectedSegmentIndex = 0
        promptControl.selectedSegmentIndex = 0

        output.isEditable = false
        output.font = .monospacedSystemFont(ofSize: 10, weight: .regular)

        let webView = makeGateWebView()
        self.webView = webView

        let stack = UIStackView(arrangedSubviews: [
            status, lockdownControl, promptControl, runButton, webView, output,
        ])
        stack.axis = .vertical
        stack.spacing = 8
        stack.translatesAutoresizingMaskIntoConstraints = false
        view.addSubview(stack)

        NSLayoutConstraint.activate([
            stack.topAnchor.constraint(equalTo: view.safeAreaLayoutGuide.topAnchor, constant: 8),
            stack.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 8),
            stack.trailingAnchor.constraint(equalTo: view.trailingAnchor, constant: -8),
            stack.bottomAnchor.constraint(equalTo: view.safeAreaLayoutGuide.bottomAnchor, constant: -8),
            // A4 again: the web view is on screen and has a real size. A
            // zero-height view is mounted in the view hierarchy and is not
            // visible, which is one of the five host shapes gate 6 measures and
            // is not the one this gate wants.
            webView.heightAnchor.constraint(equalToConstant: 200),
        ])

        // A lab tool that can only be driven by a finger is a lab tool that
        // cannot be regression-tested. `--migo-autorun` runs the gate once the
        // view is on screen and prints the records, so an automated run exercises
        // the same path an operator does -- and `--migo-lockdown` /
        // `--migo-local-network-prompt` let the person who checked the device
        // state declare it without staying at the bench to tap it.
        //
        // A misparsed declaration does not run. `unknown` is what a run nobody
        // attested says, so silently recording it here would file an attested run
        // as an unattended one, and the record would not say which happened. The
        // parser is in the harness package, where a lane compiles it and tests it;
        // this target only reports what it said.
        do {
            let options = try MigoProbeLaunchOptions.parse(
                arguments: ProcessInfo.processInfo.arguments)
            switch options.lockdownMode {
            case .off: lockdownControl.selectedSegmentIndex = 1
            case .on: lockdownControl.selectedSegmentIndex = 2
            case .unknown: break
            }
            if let observed = options.localNetworkPromptObserved {
                promptControl.selectedSegmentIndex = observed ? 2 : 1
            }
            launchRunId = options.runId
            autorun = options.autorun
        } catch {
            status.text = "the launch arguments were refused: \(error)"
            runButton.isEnabled = false
            FileHandle.standardError.write(
                Data("MIGO_PROBE_REFUSED \(error)\n".utf8))
        }
    }

    private var autorun = false
    private var hasAutorun = false
    private var launchRunId: String?

    override func viewDidAppear(_ animated: Bool) {
        super.viewDidAppear(animated)
        guard autorun, !hasAutorun else { return }
        hasAutorun = true
        runTapped()
    }

    private func makeGateWebView() -> WKWebView {
        do {
            let gate = try MigoCapabilityGate()
            self.gate = gate
            return gate.makeWebView()
        } catch {
            status.text = "the gate could not be built: \(error)"
            return WKWebView(frame: .zero)
        }
    }

    @objc private func runTapped() {
        guard let webView, let gate else { return }

        let lockdown: MigoLockdownMode = {
            switch lockdownControl.selectedSegmentIndex {
            case 1: return .off
            case 2: return .on
            default: return .unknown
            }
        }()
        let prompt: Bool? = {
            switch promptControl.selectedSegmentIndex {
            case 1: return false
            case 2: return true
            default: return nil
            }
        }()

        // Named by the caller when there was one, so the collector asks for the
        // file this run wrote instead of globbing the directory -- a glob returns
        // the previous run's records when this run wrote none, and both are valid
        // JSON. Consumed once: a second tap must not overwrite a record somebody
        // has already collected under that name.
        let runId = launchRunId ?? UUID().uuidString
        launchRunId = nil

        // One gate for the life of the screen, because it owns this web view's
        // configuration. The attestation is per run and is passed per run.
        runButton.isEnabled = false
        status.text = "running..."

        // The WebKit build is read from the live agent before the run, because a
        // record is assembled synchronously and evaluateJavaScript is not.
        MigoProbeEnvironment.primeUserAgent(webView: webView) { [weak self] in
            DispatchQueue.main.async {
                gate.run(
                    in: webView,
                    attestation: .init(
                        lockdownMode: lockdown, localNetworkPromptObserved: prompt,
                        runId: runId)
                ) { result in
                    self?.finish(result)
                }
            }
        }
    }

    private func finish(_ result: Result<MigoCapabilityGate.Outcome, Error>) {
        runButton.isEnabled = true
        switch result {
        case .failure(let error):
            status.text = "the run failed: \(error)"
            output.text = ""
        case .success(let outcome):
            do {
                let encoder = MigoProbeRecord.makeEncoder()
                let runId = outcome.capabilities.first?.runId ?? "run"
                let documents = try FileManager.default
                    .url(for: .documentDirectory, in: .userDomainMask, appropriateFor: nil, create: true)

                let capabilityData = try encoder.encode(outcome.capabilities)
                let capabilityName = "capability-\(runId).json"
                try capabilityData.write(to: documents.appendingPathComponent(capabilityName))

                // Two files rather than one, and the capability file is written
                // FIRST. They answer different questions and the collector pulls
                // them separately; writing the one gate 1 exists for before the
                // one the gate after it draws on means a run interrupted between
                // them still produced its own answer.
                //
                // The transport file is written even when empty, because absent
                // and empty are different: absent is a build that cannot measure
                // transports, empty is a visit where no transport could be
                // reached. A collector that had to tell those apart from a
                // missing file could not.
                let transportData = try encoder.encode(outcome.transports)
                let transportName = "transport-\(runId).json"
                try transportData.write(to: documents.appendingPathComponent(transportName))

                status.text = "wrote \(outcome.capabilities.count) capability and "
                    + "\(outcome.transports.count) transport record(s) to Documents/"
                output.text = String(decoding: capabilityData, as: UTF8.self)
            } catch {
                status.text = "the records could not be written: \(error)"
            }
        }
        if autorun {
            // stderr, not stdout: the simulator's log collector reads one of
            // them, and this project has already lost six round trips to a
            // Swift lane writing where nothing was listening.
            FileHandle.standardError.write(
                Data(("MIGO_PROBE_RESULT " + (output.text ?? "") + "\n").utf8))
        }
    }
}

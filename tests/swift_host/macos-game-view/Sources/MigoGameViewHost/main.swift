import AppKit
import CryptoKit
import MigoMacV8

// usage: MigoGameViewHost <package-dir> <work-dir> [seconds] [unsigned|signed|tampered]
//
// The signing mode is the content-signing question, asked of the real engine:
//   unsigned  the package as it is, with contentSigning .unsigned
//   signed    signed here with a fresh Ed25519 key (manifest.json + manifest.sig,
//             the format shared::vfs::integrity reads), verified with its key
//   tampered  signed, then one byte of game.js changed: must be refused
//
// Exit status is the verdict, and each failure has its own so the script can
// say which question failed:
//   0  the game became ready, ran, and asked to exit
//   2  bad arguments
//   3  the view refused to start (printed reason) -- the expected answer for a
//      process signed without allow-jit
//   4  the game failed after starting
//   5  nothing was heard before the deadline
let arguments = CommandLine.arguments
guard arguments.count >= 3 else {
    FileHandle.standardError.write(Data("usage: MigoGameViewHost <package> <work> [seconds]\n".utf8))
    exit(2)
}
let package = URL(fileURLWithPath: arguments[1], isDirectory: true)
let work = URL(fileURLWithPath: arguments[2], isDirectory: true)
let seconds = arguments.count > 3 ? Double(arguments[3]) ?? 120 : 120
let signingMode = arguments.count > 4 ? arguments[4] : "unsigned"

func say(_ text: String) {
    print("[game-view-host] \(text)")
    fflush(stdout)
}

let app = NSApplication.shared
app.setActivationPolicy(.regular)

let directories = MigoGameDirectories(
    files: work.appendingPathComponent("files"), cache: work.appendingPathComponent("cache"),
    codeCache: work.appendingPathComponent("code-cache"))
/// A signed copy of `package`: every file's SHA-256 in manifest.json, and the
/// raw Ed25519 signature of those exact bytes in manifest.sig.
func signedCopy(of package: URL, tamper: Bool) throws -> (URL, Data) {
    let copy = work.appendingPathComponent("signed-package", isDirectory: true)
    try? FileManager.default.removeItem(at: copy)
    // copyItem needs the destination's parent, and says the SOURCE is missing
    // when it is not there -- which is how the first CI run reported it.
    try FileManager.default.createDirectory(at: work, withIntermediateDirectories: true)
    try FileManager.default.copyItem(at: package, to: copy)
    var files: [String: String] = [:]
    let walker = FileManager.default.enumerator(at: copy, includingPropertiesForKeys: [.isRegularFileKey])!
    for case let url as URL in walker where (try? url.resourceValues(forKeys: [.isRegularFileKey]))?.isRegularFile == true {
        let relative = String(url.standardizedFileURL.path.dropFirst(copy.standardizedFileURL.path.count + 1))
        files[relative] = SHA256.hash(data: try Data(contentsOf: url)).map { String(format: "%02x", $0) }.joined()
    }
    let manifest = try JSONSerialization.data(
        withJSONObject: ["version": 1, "entry": "game.js", "timestamp": 1_700_000_000, "files": files],
        options: [.sortedKeys])
    let key = Curve25519.Signing.PrivateKey()
    try manifest.write(to: copy.appendingPathComponent("manifest.json"))
    try key.signature(for: manifest).write(to: copy.appendingPathComponent("manifest.sig"))
    if tamper {
        let game = copy.appendingPathComponent("game.js")
        var bytes = try Data(contentsOf: game)
        bytes.append(contentsOf: Array("\n// changed after signing\n".utf8))
        try bytes.write(to: game)
    }
    return (copy, key.publicKey.rawRepresentation)
}

let contentSigning: MigoContentSigning
do {
    switch signingMode {
    case "unsigned":
        contentSigning = .unsigned
        try MigoGameInstaller.install(package: package, id: "probe", into: directories)
    case "signed", "tampered":
        let (signed, publicKey) = try signedCopy(of: package, tamper: signingMode == "tampered")
        contentSigning = .verified(publicKey: publicKey)
        try MigoGameInstaller.install(package: signed, id: "probe", into: directories)
    default:
        say("unknown signing mode \(signingMode)")
        exit(2)
    }
} catch {
    say("install failed: \(error)")
    exit(2)
}

let window = NSWindow(
    contentRect: NSRect(x: 0, y: 0, width: 256, height: 256), styleMask: [.titled, .resizable],
    backing: .buffered, defer: false)
let view = MigoGameView(configuration: .init(directories: directories, contentSigning: contentSigning))
window.contentView = view
window.center()
window.makeKeyAndOrderFront(nil)
app.activate(ignoringOtherApps: true)

var sawReady = false
var frames = 0
view.onEvent = { event in
    switch event {
    case .ready:
        sawReady = true
        say("ready")
    case .exitRequested:
        say("exit requested after ready=\(sawReady), clock delivered \(frames) frames")
        exit(sawReady && frames > 0 ? 0 : 4)
    case .failed(let reason):
        say("failed: \(reason)")
        exit(sawReady ? 4 : 3)
    case .error(let code, let message, let recoverable):
        say("error \(code) recoverable=\(recoverable): \(message)")
        if !recoverable { exit(4) }
    case .gameLog(let entry):
        say("game log: \(entry)")
    }
}
// Sampled on the main queue, where the view lives; the last sample before
// exit is what is reported.
Timer.scheduledTimer(withTimeInterval: 0.02, repeats: true) { _ in
    if let delivered = view.frameClockStatistics?.delivered { frames = delivered }
}
say("unavailability: \(MigoGameView.unavailabilityReason ?? "none")")
// What the window server says about the window, for a run that turns no
// frames: an occluded view gets no display-link ticks, by design.
DispatchQueue.main.asyncAfter(deadline: .now() + 2) {
    say("window visible=\(window.occlusionState.contains(.visible)) key=\(window.isKeyWindow)")
}
view.loadGame(id: "probe")
DispatchQueue.main.asyncAfter(deadline: .now() + seconds) {
    say("nothing heard in \(seconds) s (ready=\(sawReady), frames=\(frames))")
    exit(5)
}
app.run()

import Foundation

/// Puts a game's package where the engine loads installed content from.
///
/// The engine does not load from an arbitrary directory -- `MigoContentDescriptor`
/// names content by id and resolves it under
/// `<files>/migo/games/<id>/code`, so that a game's code, its saves and its cache
/// all hang off one identity. Installing is therefore the host's step, and this
/// is it for Apple: the same layout the Android SDK writes, the same id rule the
/// engine enforces (`shared::vfs::game_paths`), and an install that is atomic --
/// a crash halfway through leaves the previous version in place, never half of
/// the new one.
public enum MigoContentInstaller {

    public enum Failure: Error, CustomStringConvertible, Equatable {
        /// The id is not one the engine accepts; see `isValidID`.
        case invalidID(String)
        /// The package directory does not exist, or is not a directory.
        case packageMissing(URL)
        /// The package has no entry file at its root.
        case entryMissing(String)

        public var description: String {
            switch self {
            case .invalidID(let id):
                return "\"\(id)\" is not a game id: 1-64 of a-z 0-9 _ -, and not a Windows device name"
            case .packageMissing(let url): return "no game package directory at \(url.path)"
            case .entryMissing(let entry): return "the package has no \(entry) at its root"
            }
        }
    }

    /// The engine's rule, repeated so a bad id fails here with a sentence
    /// rather than at load time with a result code. Lower case only, because the
    /// id is a path component and must mean one directory on a case-insensitive
    /// filesystem, which is what APFS is by default.
    public static func isValidID(_ id: String) -> Bool {
        guard (1...64).contains(id.utf8.count) else { return false }
        guard id.utf8.allSatisfy({ byte in
            (0x61...0x7a).contains(byte) || (0x30...0x39).contains(byte) || byte == 0x5f || byte == 0x2d
        }) else { return false }
        if ["con", "prn", "aux", "nul"].contains(id) { return false }
        for prefix in ["com", "lpt"] where id.hasPrefix(prefix) {
            let suffix = id.dropFirst(prefix.count)
            if suffix.count == 1, suffix.first!.isASCII, suffix.first!.isNumber { return false }
        }
        return true
    }

    /// Install `package` as game `id`, unless `version` is already installed.
    ///
    /// `version` is the host's name for what it is installing -- the app's build
    /// number, a content hash -- and makes a relaunch free: a package that is
    /// already there is not copied again. `nil` always installs.
    ///
    /// Returns the installed code directory.
    @discardableResult
    public static func install(
        package: URL, id: String, entry: String = "game.js", version: String? = nil,
        into directories: MigoEngineSession.Directories
    ) throws -> URL {
        guard isValidID(id) else { throw Failure.invalidID(id) }
        let manager = FileManager.default
        var isDirectory: ObjCBool = false
        guard manager.fileExists(atPath: package.path, isDirectory: &isDirectory), isDirectory.boolValue
        else { throw Failure.packageMissing(package) }
        guard manager.fileExists(atPath: package.appendingPathComponent(entry).path) else {
            throw Failure.entryMissing(entry)
        }

        let code = directories.installedCode(contentID: id)
        let gameRoot = code.deletingLastPathComponent()
        // Beside `code`, not in it: the marker describes the directory, and a
        // file inside the package would be content the game can read.
        let marker = gameRoot.appendingPathComponent(".installed-version")
        if let version,
            manager.fileExists(atPath: code.path),
            let installed = try? String(contentsOf: marker, encoding: .utf8),
            installed == version
        {
            return code
        }

        try manager.createDirectory(at: gameRoot, withIntermediateDirectories: true)
        // Staged on the same volume so the swap below is a rename, not a copy.
        let staging = gameRoot.appendingPathComponent(".staging-\(UUID().uuidString)", isDirectory: true)
        try manager.copyItem(at: package, to: staging)
        do {
            if manager.fileExists(atPath: code.path) {
                _ = try manager.replaceItemAt(code, withItemAt: staging)
            } else {
                try manager.moveItem(at: staging, to: code)
            }
        } catch {
            try? manager.removeItem(at: staging)
            throw error
        }
        if let version {
            try Data(version.utf8).write(to: marker, options: .atomic)
        } else {
            try? manager.removeItem(at: marker)
        }
        return code
    }
}

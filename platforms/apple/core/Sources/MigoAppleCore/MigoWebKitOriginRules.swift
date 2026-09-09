import Foundation

#if canImport(UniformTypeIdentifiers)
    import UniformTypeIdentifiers
#endif

/// The decisions the content origin makes, separated from the WebKit types that
/// deliver them.
///
/// Path containment, MIME typing and `Range` parsing are the three places a content
/// origin goes wrong, and all three are pure functions of a string. They are here,
/// in the package a pull request compiles and tests, rather than inside the
/// `WKURLSchemeHandler` in the shipping package -- which resolves only after an
/// xcframework exists and has no test target at all. A traversal check that has
/// never been run is a traversal check.
public enum MigoWebKitOriginRules {

    /// The scheme content is served on. One label, so the navigation policy and the
    /// handler cannot disagree about what counts as inside the content.
    public static let scheme = "migo-content"

    /// The authority. Content's origin is `migo-content://content`; a request naming
    /// another host is refused rather than served from the same root, because two
    /// hosts over one directory would be two origins sharing storage.
    public static let host = "content"

    public static let baseURLString = "\(scheme)://\(host)/"

    /// Where a request lands, or why it does not land anywhere.
    public enum Resolution: Equatable {
        case file(path: String)
        /// The path resolves outside the package. Refused rather than clamped: an
        /// escape answered with the index page is a different answer than the one
        /// asked for, and content cannot tell that it got one.
        case outsidePackage
        /// A different authority on the same scheme.
        case wrongHost(String?)
    }

    /// Resolve a request path against the package root.
    ///
    /// The resolution is done here rather than taken as an argument, so the rule and
    /// the filesystem step that makes it true are one thing a test can run. Symlinks
    /// are resolved on both sides: on the root because containment must not depend on
    /// the filesystem's shape at request time, and on the candidate because a symlink
    /// inside the package pointing out of it is exactly the escape a textual check
    /// misses.
    public static func resolve(
        requestPath: String, host requestHost: String?, root: URL,
        indexPath: String = "index.html"
    ) -> Resolution {
        guard requestHost == host else { return .wrongHost(requestHost) }
        var path = requestPath
        if path.isEmpty || path == "/" { path = "/" + indexPath }
        let resolvedRoot = root.resolvingSymlinksInPath().standardizedFileURL.path
        // Relative to the root, then standardised: `URL(fileURLWithPath:relativeTo:)`
        // happily resolves `..` past the root and hands back somewhere else, which is
        // why the check is on the result and never on the input.
        let candidate = URL(fileURLWithPath: String(path.dropFirst()), relativeTo: root)
            .resolvingSymlinksInPath().standardizedFileURL.path
        let prefix = resolvedRoot.hasSuffix("/") ? resolvedRoot : resolvedRoot + "/"
        guard candidate == resolvedRoot || candidate.hasPrefix(prefix) else {
            return .outsidePackage
        }
        return .file(path: candidate)
    }

    /// The MIME type for a file name, from the system's own table where it has one.
    ///
    /// Derived rather than listed, because a hand-written extension map stops
    /// covering formats somebody adds -- and getting it wrong on a script or a
    /// WebAssembly module is a page that receives the bytes and refuses to run them.
    /// The three fallbacks below are the ones that must be right for mini-game
    /// content whether or not the system table carries them.
    public static func mimeType(forPathExtension extension_: String) -> String {
        let lowered = extension_.lowercased()
        switch lowered {
        case "wasm": return "application/wasm"
        case "js", "mjs": return "text/javascript"
        case "json": return "application/json"
        default: break
        }
        #if canImport(UniformTypeIdentifiers)
            if let type = UTType(filenameExtension: lowered), let mime = type.preferredMIMEType {
                return mime
            }
        #endif
        return "application/octet-stream"
    }

    /// What a `Range` header asked for.
    public enum ByteRange: Equatable {
        case whole
        case partial(start: Int, length: Int)
        /// The request starts past the end. Answered with the real length so a media
        /// loader that guessed learns it instead of retrying the same guess.
        case unsatisfiable
    }

    /// Parse `bytes=a-b`, `bytes=a-` and `bytes=-n`, the three forms WebKit's media
    /// loader sends.
    ///
    /// Anything else is served whole rather than refused: an origin that answers 416
    /// to a header it merely does not recognise breaks a load that would have
    /// worked. A multi-range request is legal and rare, and answering its first
    /// range as though it were the only one would be a wrong answer rather than a
    /// partial one -- so it too is served whole and WebKit slices it.
    public static func byteRange(fromHeader header: String?, size: Int) -> ByteRange {
        guard let header else { return .whole }
        let trimmed = header.trimmingCharacters(in: .whitespaces)
        guard trimmed.lowercased().hasPrefix("bytes=") else { return .whole }
        let spec = String(trimmed.dropFirst("bytes=".count)).trimmingCharacters(in: .whitespaces)
        guard !spec.contains(",") else { return .whole }
        let parts = spec.split(separator: "-", maxSplits: 1, omittingEmptySubsequences: false)
        guard parts.count == 2 else { return .whole }
        let first = String(parts[0]).trimmingCharacters(in: .whitespaces)
        let second = String(parts[1]).trimmingCharacters(in: .whitespaces)

        if first.isEmpty {
            guard let count = Int(second), count > 0 else { return .whole }
            // A suffix longer than the file is the whole file, not an error: the
            // client asked for "the last n bytes" of something shorter than n.
            let length = min(count, size)
            guard size > 0 else { return .whole }
            return .partial(start: size - length, length: length)
        }
        guard let start = Int(first), start >= 0 else { return .whole }
        if start >= size { return .unsatisfiable }
        if second.isEmpty { return .partial(start: start, length: size - start) }
        guard let end = Int(second), end >= start else { return .whole }
        let last = min(end, size - 1)
        return .partial(start: start, length: last - start + 1)
    }

    /// The response headers for a served file.
    ///
    /// `Accept-Ranges` is declared on whole-file responses too: WebKit's media
    /// loader only issues ranges against a source that says it accepts them, so an
    /// audio element served without it cannot seek however well the range code
    /// works.
    public static func responseHeaders(
        pathExtension: String, start: Int, length: Int, totalSize: Int, partial: Bool
    ) -> [String: String] {
        var headers = [
            "Content-Type": mimeType(forPathExtension: pathExtension),
            "Content-Length": String(length),
            "Accept-Ranges": "bytes",
        ]
        if partial {
            headers["Content-Range"] = "bytes \(start)-\(start + length - 1)/\(totalSize)"
        }
        return headers
    }
}

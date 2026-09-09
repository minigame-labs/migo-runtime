import Foundation

/// The JavaScript content actually sees, generated from the surface.
///
/// WHY GENERATED. `MigoWebKitSurface` decides which native methods a host serves,
/// and that decision has to reach content. Writing the shim by hand would make the
/// content-facing surface a second list, and the two lists disagreeing is not a
/// visible failure: a function that exists in the shim and is served by nobody
/// returns a promise that never settles, and a game that awaits it hangs on a
/// splash screen with no error anywhere.
///
/// WHY THE THREE ANSWERS ARE THREE SHAPES. The contract distinguishes a capability
/// the host turned off from one that has no mechanism at all, and that distinction
/// is worth nothing if content cannot tell them apart:
///
///   * served -- a function that posts to its handler and resolves.
///   * declared but withheld -- a function that throws immediately, naming the
///     contract. A host app debugging its own content learns this is a
///     configuration answer rather than a missing feature.
///   * absent -- nothing. `typeof` says `undefined`, because there is no mechanism
///     to refuse, and a stub that threw would imply there is one to turn on.
///
/// WHY NOT `globalThis.migo`. The engine's capability surface is named `migo`, and
/// this lane has no engine: content that feature-detects `typeof migo === "object"`
/// and then calls `migo.createCanvas()` would find an object and no canvas. So the
/// host's own services live under a name that cannot be mistaken for the engine's,
/// and content that wants a canvas in this lane uses the DOM, which is the whole
/// point of the lane.
public enum MigoWebKitBridgeShim {

    /// The global the host's own services hang from. Deliberately not `migo`.
    public static let namespace = "migoHost"

    /// The property the host calls to deliver a subscription event.
    ///
    /// Reached from `evaluateJavaScript`, so its name is part of the contract
    /// between the two halves rather than an implementation detail of either.
    public static let deliveryEntryPoint = "deliver"

    /// The channel a subscription method delivers on.
    ///
    /// The method minus its verb: `lifecycle.observe` delivers on `lifecycle`. One
    /// derivation, so the host and the shim cannot disagree about the name a
    /// delivery is addressed to -- and a disagreement there is a listener that is
    /// registered and never called.
    public static func channel(forMethod method: String) -> String {
        let parts = method.split(separator: ".").map(String.init)
        return parts.count > 1 ? parts.dropLast().joined(separator: ".") : method
    }

    /// The JavaScript a host evaluates to deliver one event.
    public static func deliveryScript(channel: String, payloadJSON: String) -> String {
        "globalThis.\(namespace) && globalThis.\(namespace).\(deliveryEntryPoint)"
            + "(\(quote(channel)), \(payloadJSON))"
    }

    /// The message-handler name for a dotted method.
    ///
    /// Dots are legal in a handler name and reach JavaScript only through bracket
    /// access, which makes every call site in the shim quoted and easy to get
    /// subtly wrong. One mangling, in one place, tested.
    public static func handlerName(forMethod method: String) -> String {
        "migo_" + method.replacingOccurrences(of: ".", with: "_")
    }

    /// Every handler the host installs for this surface, in the order it installs
    /// them. Derived from the surface, so a handler is installed exactly when the
    /// shim has a function that posts to it.
    public static func handlerNames(for surface: MigoWebKitSurface) -> [String] {
        surface.servedBridgeMethods.map(handlerName(forMethod:))
    }

    /// The script, to be injected at document start in the page world.
    ///
    /// The page world, not an isolated one: content has to be able to call this,
    /// and an isolated world is invisible to it. So the properties are made
    /// non-writable and non-configurable and the objects frozen, which is as far as
    /// a shared world goes -- content that wants to shadow them can still run
    /// before this does only if it is injected earlier, and nothing is injected
    /// earlier than document start.
    /// `entries` is a parameter so a test can hand it a table this one does not
    /// contain. The generator emits one `const` per namespace group, and two
    /// methods under one group emitting it twice is a `SyntaxError` that takes the
    /// whole shim with it -- a failure the shipping table cannot currently produce,
    /// which is exactly why nothing would notice it being introduced.
    public static func script(
        for surface: MigoWebKitSurface,
        entries: [MigoWebKitSurface.Entry] = MigoWebKitSurface.entries
    ) -> String {
        var lines: [String] = [
            "(() => {",
            "  'use strict';",
            "  const post = (handler, payload) => {",
            "    const handlers = globalThis.webkit && globalThis.webkit.messageHandlers;",
            "    const target = handlers && handlers[handler];",
            "    if (!target) {",
            // A served method whose handler is missing is a host bug, and the
            // failure has to be an error rather than a promise nobody settles.
            "      return Promise.reject(new Error(",
            "        'migoHost: the host did not install ' + handler +",
            "        '. A method the shim serves and the host does not is a call that never returns'));",
            "    }",
            "    return target.postMessage(payload);",
            "  };",
            "  const define = (target, name, value) => {",
            "    Object.defineProperty(target, name, {",
            "      value, writable: false, enumerable: true, configurable: false });",
            "  };",
            "  const group = (root, name) => {",
            "    if (!Object.prototype.hasOwnProperty.call(root, name)) { define(root, name, {}); }",
            "    return root[name];",
            "  };",
            "  const root = {};",
            // Subscriptions. The registry is a closure variable rather than a
            // property, so content cannot empty somebody else's listener list, and
            // the delivery entry point is a property because the host reaches it by
            // name from evaluateJavaScript. Content can reach it too -- everything
            // in a shared world can reach everything -- and content that delivers
            // itself a fake lifecycle event has only misled itself. Hiding it would
            // be obscurity, not a boundary.
            "  const listeners = new Map();",
            "  const subscribe = (channel, handler, fn) => {",
            "    if (typeof fn !== 'function') {",
            "      throw new TypeError('migoHost: ' + channel +",
            "        ' takes a function; the host delivers to it and there is nothing to call');",
            "    }",
            "    if (!listeners.has(channel)) { listeners.set(channel, []); }",
            "    listeners.get(channel).push(fn);",
            "    return post(handler, { channel });",
            "  };",
            "  const deliver = (channel, payload) => {",
            "    const registered = listeners.get(channel);",
            "    if (!registered) { return 0; }",
            // One listener throwing must not stop the rest: a lifecycle event that
            // reaches half its listeners because the first one threw is a save that
            // half happened, and the host has no way to see it.
            "    let delivered = 0;",
            "    for (const fn of registered.slice()) {",
            "      try { fn(payload); delivered += 1; } catch (error) {",
            "        if (globalThis.console && console.error) {",
            "          console.error('migoHost: a ' + channel + ' listener threw', error);",
            "        }",
            "      }",
            "    }",
            "    return delivered;",
            "  };",
        ]

        var declaredGroups: Set<String> = []
        for entry in entries where entry.provenance == .hostBridge {
            guard let method = entry.bridgeMethod else { continue }
            let parts = method.split(separator: ".").map(String.init)
            guard let leaf = parts.last else { continue }
            var target = "root"
            var path: [String] = []
            for group in parts.dropLast() {
                path.append(group)
                let identifier = local(path)
                if declaredGroups.insert(identifier).inserted {
                    lines.append("  const \(identifier) = group(\(target), \(quote(group)));")
                }
                target = identifier
            }

            if surface.isEnabled(entry.capability) {
                let handler = handlerName(forMethod: method)
                switch entry.bridgeKind ?? .call {
                case .call:
                    lines.append(
                        "  define(\(target), \(quote(leaf)), "
                            + "(payload) => post(\(quote(handler)), payload === undefined ? null : payload));")
                case .subscription:
                    lines.append(
                        "  define(\(target), \(quote(leaf)), "
                            + "(fn) => subscribe(\(quote(channel(forMethod: method))), \(quote(handler)), fn));")
                }
            } else {
                // A throwing stub, not a missing property: this capability has a
                // mechanism and the host chose not to serve it, and the message
                // says so in the words of the file that decides.
                let message =
                    "\(method) is withheld by this host's configuration "
                    + "(contracts/apple/webkit-full-surface.json). It is not missing: the host app "
                    + "can enable it."
                lines.append(
                    "  define(\(target), \(quote(leaf)), () => { throw new Error("
                        + "\(quote("migoHost: " + message)) ); });")
            }
        }

        lines += [
            "  define(root, \(quote(deliveryEntryPoint)), deliver);",
            "  const freeze = (node) => {",
            "    for (const key of Object.keys(node)) {",
            "      if (node[key] && typeof node[key] === 'object') { freeze(node[key]); }",
            "    }",
            "    Object.freeze(node);",
            "  };",
            "  freeze(root);",
            "  Object.defineProperty(globalThis, \(quote(namespace)), {",
            "    value: root, writable: false, enumerable: true, configurable: false });",
            "})();",
        ]
        return lines.joined(separator: "\n") + "\n"
    }

    /// A JavaScript string literal. Everything interpolated into the script goes
    /// through here: a method name or a reason containing a quote would otherwise
    /// end the literal and inject whatever followed it into the page world.
    static func quote(_ text: String) -> String {
        var escaped = ""
        for character in text.unicodeScalars {
            switch character {
            case "\\": escaped += "\\\\"
            case "'": escaped += "\\'"
            case "\n": escaped += "\\n"
            case "\r": escaped += "\\r"
            case "\u{2028}": escaped += "\\u2028"
            case "\u{2029}": escaped += "\\u2029"
            default: escaped.unicodeScalars.append(character)
            }
        }
        return "'" + escaped + "'"
    }

    /// A JavaScript identifier for an intermediate namespace object.
    ///
    /// Named after the whole path and not the last segment: `a.x.m` and `b.x.n`
    /// both have an `x`, and one identifier for two different objects would make
    /// the second method land on the first one's parent.
    static func local(_ path: [String]) -> String {
        "g_" + path.joined(separator: "_").unicodeScalars.map { scalar -> String in
            CharacterSet.alphanumerics.contains(scalar) ? String(scalar) : "_"
        }.joined()
    }
}

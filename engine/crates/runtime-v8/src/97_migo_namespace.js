// migo namespace: the one surface every game gets, unconditionally.
//
// Earlier revisions of this file also built a mini-game-platform-compatible
// mirror unconditionally (later, behind a Cargo feature). Neither is here
// anymore: `migo` is the engine's only default global, full stop. Content
// ported from a mini-game platform (a mainstream mini-game client, a quick-game alliance member,
// etc.) gets that platform's global from an external, platform-specific
// adapter package instead (the same pattern the BOM/DOM adapter already
// uses -- see minigame-labs/migo-web-adapter), loaded by the host or the game
// itself, not baked into every build whether it's wanted or not. Every name
// published here is the common mini-game platform's -- the gamepad API too
// (getGamepads, on/offGamepadConnected, on/offGamepadDisconnected), which a
// list here once excluded as if the platform had none.
//
// migo registers its low-level APIs directly on globalThis; this file
// projects them onto one deliberate namespace object so app code has a
// stable surface to extend without mutating globalThis itself.
//
// Rules:
//   - GameGlobal, global remain aliases of globalThis (matches the
//     mini-game-platform content model this engine is compatible with).
//   - JS built-ins (Object/Array/Promise/...), self-references
//     (globalThis/migo/GameGlobal/global), and runtime internals are NOT
//     mirrored onto `migo`.

import { primordials } from "ext:core/mod.js";
const { ObjectDefineProperty } = primordials;

// Anything in this set is intentionally NOT mirrored onto the `migo` object.
// JS built-ins -- present on every globalThis. Sourced from V8's standard
// realm; we list them statically because they don't change between sessions.
const _JS_BUILTINS = new Set([
    // Value properties
    "globalThis", "Infinity", "NaN", "undefined",
    // Function properties
    "eval", "isFinite", "isNaN", "parseFloat", "parseInt",
    "decodeURI", "decodeURIComponent", "encodeURI", "encodeURIComponent",
    "escape", "unescape",
    // Fundamental objects
    "Object", "Function", "Boolean", "Symbol",
    // Error objects
    "Error", "AggregateError", "EvalError", "RangeError", "ReferenceError",
    "SyntaxError", "TypeError", "URIError", "SuppressedError",
    // Numbers, dates, text
    "Number", "BigInt", "Math", "Date", "String", "RegExp",
    // Indexed collections
    "Array", "Int8Array", "Uint8Array", "Uint8ClampedArray",
    "Int16Array", "Uint16Array", "Int32Array", "Uint32Array",
    "BigInt64Array", "BigUint64Array", "Float32Array", "Float64Array",
    // Keyed collections
    "Map", "Set", "WeakMap", "WeakSet",
    // Structured data
    "ArrayBuffer", "SharedArrayBuffer", "Atomics", "DataView", "JSON",
    // Control abstraction
    "Promise", "Iterator", "AsyncIterator", "Generator", "GeneratorFunction",
    "AsyncFunction", "AsyncGenerator", "AsyncGeneratorFunction",
    "DisposableStack", "AsyncDisposableStack",
    // Reflection / proxies
    "Reflect", "Proxy",
    // Internationalization
    "Intl",
    // Resource management
    "FinalizationRegistry", "WeakRef",
    // WebAssembly
    "WebAssembly",
]);

// Self-references and runtime internals are not part of the migo namespace.
//
// ORDERING HAZARD -- read before adding anything to globalThis.
//
// This namespace is built here, during bootstrap. `harden_global_scope`
// (Rust, `lib.rs`) deletes the deno_core internals from globalThis *after*
// that, because deleting them from JS breaks deno_core's snapshot restore
// path. So a mirror built first captures whatever hardening deletes later, and
// deleting the global afterwards does nothing to the copy.
//
// That is not hypothetical: `Deno` was once mirrored onto the published
// namespace and `<namespace>.Deno.core.ops` handed content 616 invocable ops
// -- the whole native surface, past every JS-level API and policy.
// `__bootstrap` escaped only because it happens to start with an underscore.
//
// So anything hardening removes must be listed here too, and the two lists are
// kept in agreement by `scripts/test-runtime-internals-not-published.sh`. The
// behavioural backstop is `tests/published_namespace_isolation.rs`, which
// searches the published namespace for an op table by shape rather than by
// name -- a new internal that leaks fails there even if nobody updates a list.
const _RUNTIME_INTERNALS = new Set([
    "Deno",             // deno_core: `.core.ops` is the entire native surface
    "__bootstrap",      // deno_core: internal module registry
]);

const _NON_API = new Set([
    "GameGlobal", "global", "migo",     // self-references (added below or by 99_main.js)
    "console",                           // standard JS, not namespaced
    "_CCSettings",                       // engine-compat shim (Cocos)
    ..._RUNTIME_INTERNALS,
]);

function _shouldMirrorApi(key) {
    if (_JS_BUILTINS.has(key)) return false;
    if (_NON_API.has(key)) return false;
    // Exclude any underscore-prefixed name. Two classes:
    //   - V8 / engine internals exposed on globalThis (e.g. a mini-game platform's own config global,
    //     __WAGameSubContextEndTime__).
    //   - migo's _internal* event-pump hooks intended only for the host's
    //     evaluateJavaScript channel.
    // The common mini-game namespace has zero underscore-prefixed APIs (verified
    // against a real device capture), so this rule is the right alignment.
    if (key.charCodeAt(0) === 95 /* '_' */) return false;
    return true;
}

function _copyApiNamespace(excluded) {
    const namespace = {};
    const keys = Object.getOwnPropertyNames(globalThis);
    for (let i = 0; i < keys.length; i++) {
        const key = keys[i];
        if (!_shouldMirrorApi(key) || excluded.has(key)) continue;
        // Mirror the descriptor as-is so getters/setters and writability are
        // preserved. The namespace object holds the same function references
        // as globalThis -- equality holds, identity holds.
        try {
            const desc = Object.getOwnPropertyDescriptor(globalThis, key);
            if (desc) ObjectDefineProperty(namespace, key, desc);
        } catch (_) {
            // ignore property that cannot be mirrored
        }
    }
    return namespace;
}

function installApiNamespaces() {
    const migo = _copyApiNamespace(new Set());

    // Enumerable to match the common mini-game content model, where the namespace
    // appears in Object.getOwnPropertyNames(GameGlobal).
    ObjectDefineProperty(globalThis, "migo", {
        value: migo, writable: true, enumerable: true, configurable: true,
    });
}

export { installApiNamespaces };

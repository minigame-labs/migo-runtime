import { op_download_subpackage, op_install_subpackage, op_is_subpackage_installed, op_is_subpackage_persisted, op_get_mount_generation, op_get_subpackage_identity, op_get_sub_packages, op_get_workers_path } from "ext:core/ops";
import { require as amdRequire } from "ext:host_v8_base/01_amdshim.js";
import { allocateHostCallbackId, createListenerGroup, failMessage, invokeCallback } from "ext:host_v8_base/02_async.js";

const noop = () => {};

// ---- RuntimeConfig subpackage cache ----

const _subpackageByName = new Map();
const _subpackageByRoot = new Map();
let _configLoaded = false;
let _workersRoot = null;

function _normalizeRoot(root) {
    let value = String(root || "").trim();
    value = value.replace(/^\.?\//, "");
    value = value.replace(/^\/+/, "");
    value = value.replace(/\/+$/, "");
    // Reject path traversal segments
    const parts = value.split("/");
    for (let i = 0; i < parts.length; i++) {
        if (parts[i] === "..") throw new Error("invalid path: contains ..");
    }
    return value;
}

function _deriveNameFromRoot(root) {
    const normalized = _normalizeRoot(root);
    if (!normalized) return "";
    const parts = normalized.split("/").filter(Boolean);
    return parts.length > 0 ? parts[parts.length - 1] : "";
}

function _errorText(err) {
    if (typeof err === "string") return err;
    if (err && typeof err.errMsg === "string") return err.errMsg;
    if (err && typeof err.message === "string") return err.message;
    try { return String(err); } catch (_) { return "unknown error"; }
}

function _isNotFoundError(err) {
    const text = _errorText(err).toLowerCase();
    return text.includes("cannot find module")
        || text.includes("cannot read")
        || text.includes("no such file")
        || text.includes("not found")
        || text.includes("shadowed by overlay");
}

function _loadRuntimeConfig() {
    if (_configLoaded) return;
    _configLoaded = true;

    // Load subpackages from RuntimeConfig
    try {
        const json = op_get_sub_packages();
        if (json && json !== '[]') {
            const list = JSON.parse(json);
            for (let i = 0; i < list.length; i++) {
                const item = list[i];
                if (!item || typeof item !== "object") continue;
                const root = _normalizeRoot(item.root || "");
                if (!root) continue;
                const name = typeof item.name === "string" && item.name.trim().length > 0
                    ? item.name.trim()
                    : _deriveNameFromRoot(root);
                if (!name) continue;
                const record = { name, root };
                _subpackageByName.set(name, record);
                _subpackageByRoot.set(root, record);
            }
        }
    } catch (_) {}

    // Load workers path from RuntimeConfig
    try {
        const path = op_get_workers_path();
        if (path) {
            const r = _normalizeRoot(path);
            if (r) _workersRoot = r;
        }
    } catch (_) {}
}

function _resolveSubpackage(options) {
    _loadRuntimeConfig();

    const rawName = typeof options.name === "string"
        ? options.name
        : (typeof options.root === "string" ? options.root : "");

    const requested = String(rawName || "").trim();
    if (!requested) throw new Error("name is required");

    if (requested === "__GAME__") {
        return { name: "__GAME__", root: "", fromConfig: true };
    }

    const byName = _subpackageByName.get(requested);
    if (byName) return { ...byName, fromConfig: true };

    const normalized = _normalizeRoot(requested);
    const byRoot = _subpackageByRoot.get(normalized);
    if (byRoot) return { ...byRoot, fromConfig: true };

    throw new Error(`subpackage not configured: ${requested}`);
}

// ---- entry point execution ----

// Maps subpackageKey -> mount generation at load time.
// If mount generation changes (package replaced), the entry is re-executed.
const _loadedSubpackages = new Map();

function _buildEntrypoints(pkg) {
    const list = [];
    function push(path) {
        if (path && !list.includes(path)) list.push(path);
    }

    if (pkg.root) {
        push(`${pkg.root}/game.js`);
        push(`${pkg.root}/index.js`);
        push(`${pkg.root}/main.js`);
    }

    if (pkg.name) {
        push(`${pkg.name}/game.js`);
        push(`${pkg.name}/index.js`);
        push(`subpackages/${pkg.name}/game.js`);
        push(`subpackages/${pkg.name}/index.js`);
    }

    return list;
}

function _subpackageKey(pkg) {
    return `${pkg.name}::${pkg.root}`;
}

function _executeSubpackage(pkg) {
    if (!_tryLocalExecute(pkg)) {
        throw new Error(`subpackage entry not found: ${pkg.name}`);
    }
}

// ---- pending task tracking ----

const _pendingTasks = new Map();

class SubpackageTask {
    constructor() {
        this._progressListeners = createListenerGroup("subpackage progress");
    }

    onProgressUpdate(listener) {
        this._progressListeners.on(listener);
    }

    _triggerProgress(progress, totalBytesWritten, totalBytesExpectedToWrite) {
        this._progressListeners.trigger({ progress, totalBytesWritten, totalBytesExpectedToWrite });
    }
}

function _settle(requestId, error) {
    const pending = _pendingTasks.get(requestId);
    if (!pending) return;
    _pendingTasks.delete(requestId);

    if (error) {
        // loadSubpackage: download failed, but files might already be local
        if (pending.executeAfter) {
            try {
                if (_tryLocalExecute(pending.pkg)) {
                    // Local execution succeeded despite download error
                    pending.task._triggerProgress(100, 1, 1);
                    const res = { errMsg: `${pending.apiName}:ok` };
                    invokeCallback(pending.apiName, 'success', pending.success, res);
                    invokeCallback(pending.apiName, 'complete', pending.complete, res);
                    return;
                }
            } catch (_) {
                // Local execution also failed, report original download error
            }
        }
        const res = { errMsg: failMessage(pending.apiName, error) };
        invokeCallback(pending.apiName, 'fail', pending.fail, res);
        invokeCallback(pending.apiName, 'complete', pending.complete, res);
        return;
    }

    // Download succeeded -- execute entry file if needed
    if (pending.executeAfter) {
        try {
            _executeSubpackage(pending.pkg);
        } catch (e) {
            const res = { errMsg: failMessage(pending.apiName, _errorText(e)) };
            invokeCallback(pending.apiName, 'fail', pending.fail, res);
            invokeCallback(pending.apiName, 'complete', pending.complete, res);
            return;
        }
    }

    pending.task._triggerProgress(100, 1, 1);
    const res = { errMsg: `${pending.apiName}:ok` };
    invokeCallback(pending.apiName, 'success', pending.success, res);
    invokeCallback(pending.apiName, 'complete', pending.complete, res);
}

// Local-only fallback when platform service is unavailable.
// Tries to execute entry file directly (all code already on disk).
function _localFallback(requestId) {
    const pending = _pendingTasks.get(requestId);
    if (!pending) return;

    try {
        pending.task._triggerProgress(0, 0, 1);
        _settle(requestId, null);
    } catch (err) {
        _settle(requestId, _errorText(err));
    }
}

// Try to execute a subpackage from local files.
// Returns true if found and executed (or already loaded).
// Returns false if entry not found on disk.
// Throws if entry exists but has a runtime error.
function _tryLocalExecute(pkg) {
    const key = _subpackageKey(pkg);
    // Per-subpackage identity: only changes when THIS subpackage is replaced.
    // Does NOT change when other subpackages are installed/replaced.
    let identity;
    try {
        identity = pkg.root ? op_get_subpackage_identity(pkg.root) : '';
    } catch (_) {
        identity = '';
    }
    const token = identity || 'base';

    // Already loaded with same identity -- skip re-execution.
    if (_loadedSubpackages.has(key) && _loadedSubpackages.get(key) === token) {
        return true;
    }

    if (pkg.name === "__GAME__") {
        _loadedSubpackages.set(key, token);
        return true;
    }

    const candidates = _buildEntrypoints(pkg);
    for (const entry of candidates) {
        try {
            amdRequire(entry);
            _loadedSubpackages.set(key, token);
            return true;
        } catch (err) {
            if (_isNotFoundError(err)) continue;
            throw err;
        }
    }
    return false;
}

function _startDownload(apiName, options, pkg, executeAfter) {
    const task = new SubpackageTask();

    // Before the pending entry and before any local fast path: with no id there
    // is nothing to register a task under, and `_settle` is keyed by it. Failure
    // is reported the way `_settle` reports one, so an exhausted id space is not
    // the single error in this module that throws into content.
    let requestId;
    try {
        requestId = allocateHostCallbackId();
    } catch (e) {
        const fail = typeof options.fail === "function" ? options.fail : noop;
        const complete = typeof options.complete === "function" ? options.complete : noop;
        const res = { errMsg: failMessage(apiName, _errorText(e)) };
        queueMicrotask(() => {
            fail(res);
            complete(res);
        });
        return task;
    }

    _pendingTasks.set(requestId, {
        task,
        pkg,
        apiName,
        executeAfter,
        success: typeof options.success === "function" ? options.success : noop,
        fail: typeof options.fail === "function" ? options.fail : noop,
        complete: typeof options.complete === "function" ? options.complete : noop,
    });

    // Fast path: check if subpackage is already available locally.
    // This covers: already loaded, installed .mpkg overlay, or code tree content.
    if (executeAfter) {
        // loadSubpackage: try to execute directly.
        try {
            if (_tryLocalExecute(pkg)) {
                queueMicrotask(() => _localFallback(requestId));
                return task;
            }
        } catch (e) {
            queueMicrotask(() => _settle(requestId, _errorText(e)));
            return task;
        }
    } else {
        // preDownloadSubpackage: check if DURABLY installed (survives restart).
        // Must not use mount-only check -- a session-mounted but non-persisted
        // package would give a false "predownload success".
        // Pass both name and root for precise manifest key matching.
        try {
            if (pkg.name && pkg.root && op_is_subpackage_persisted(pkg.name, pkg.root)) {
                queueMicrotask(() => _settle(requestId, null));
                return task;
            }
        } catch (_) {
            // Check failed, proceed to download.
        }
    }

    try {
        op_download_subpackage(JSON.stringify({
            requestId,
            name: pkg.name,
            root: pkg.root,
        }));
    } catch (_) {
        if (executeAfter) {
            // loadSubpackage: platform unavailable, fall back to local execution
            queueMicrotask(() => _localFallback(requestId));
        } else {
            // preDownloadSubpackage: platform unavailable, report real failure
            queueMicrotask(() => _settle(requestId, "download service not available"));
        }
    }

    return task;
}

// ---- EvalScript callbacks (called from JNI inbound) ----

function _internalOnSubpackageProgress(resultJson) {
    let data;
    try { data = JSON.parse(resultJson); } catch (_) { return; }
    const pending = _pendingTasks.get(data.requestId);
    if (!pending) return;
    pending.task._triggerProgress(
        data.progress || 0,
        data.totalBytesWritten || 0,
        data.totalBytesExpectedToWrite || 0,
    );
}

function _internalOnSubpackageResult(resultJson) {
    let data;
    try { data = JSON.parse(resultJson); } catch (_) { return; }

    if (data.error) {
        _settle(data.requestId, data.error);
        return;
    }

    // Package-native install: ingest zip, validate, mount as overlay. The zip the
    // host downloaded is held by the runtime and named by request id -- a path
    // taken from this payload would let a game install any zip the process can
    // read, so the payload does not carry one.
    const pending = _pendingTasks.get(data.requestId);
    if (!pending) return;

    const installOpts = JSON.stringify({
        requestId: data.requestId,
        name: pending.pkg.name || '',
        root: pending.pkg.root || '',
        version: data.version || '1.0',
        ensure_persistent: !pending.executeAfter,
    });

    (async () => {
        try {
            await op_install_subpackage(installOpts);
            _settle(data.requestId, null);
        } catch (e) {
            _settle(data.requestId, 'install failed: ' + (e.message || e));
        }
    })();
}

// ---- public API ----

function loadSubpackage(options = {}) {
    let pkg;
    try {
        pkg = _resolveSubpackage(options);
    } catch (e) {
        // Return a task that immediately fails
        const task = new SubpackageTask();
        const fail = typeof options.fail === "function" ? options.fail : noop;
        const complete = typeof options.complete === "function" ? options.complete : noop;
        queueMicrotask(() => {
            const res = { errMsg: failMessage('loadSubpackage', _errorText(e)) };
            fail(res);
            complete(res);
        });
        return task;
    }
    return _startDownload("loadSubpackage", options, pkg, true);
}

function preDownloadSubpackage(options = {}) {
    const packageType = typeof options.packageType === "string" && options.packageType.length > 0
        ? options.packageType
        : "normal";

    if (packageType === "workers") {
        _loadRuntimeConfig();
        if (!_workersRoot) {
            const task = new SubpackageTask();
            const fail = typeof options.fail === "function" ? options.fail : noop;
            const complete = typeof options.complete === "function" ? options.complete : noop;
            queueMicrotask(() => {
                const res = { errMsg: "preDownloadSubpackage:fail workers subpackage is not configured" };
                fail(res);
                complete(res);
            });
            return task;
        }
        const pkg = { name: _workersRoot, root: _workersRoot, fromConfig: true };
        return _startDownload("preDownloadSubpackage", options, pkg, false);
    }

    if (packageType !== "normal") {
        const task = new SubpackageTask();
        const fail = typeof options.fail === "function" ? options.fail : noop;
        const complete = typeof options.complete === "function" ? options.complete : noop;
        queueMicrotask(() => {
            const res = { errMsg: `preDownloadSubpackage:fail invalid packageType: ${packageType}` };
            fail(res);
            complete(res);
        });
        return task;
    }

    let pkg;
    try {
        pkg = _resolveSubpackage(options);
    } catch (e) {
        const task = new SubpackageTask();
        const fail = typeof options.fail === "function" ? options.fail : noop;
        const complete = typeof options.complete === "function" ? options.complete : noop;
        queueMicrotask(() => {
            const res = { errMsg: failMessage('preDownloadSubpackage', _errorText(e)) };
            fail(res);
            complete(res);
        });
        return task;
    }

    // preDownloadSubpackage only allows subpackages defined in RuntimeConfig
    if (!pkg.fromConfig) {
        const task = new SubpackageTask();
        const fail = typeof options.fail === "function" ? options.fail : noop;
        const complete = typeof options.complete === "function" ? options.complete : noop;
        const reqName = options.name || options.root || "";
        queueMicrotask(() => {
            const res = { errMsg: `preDownloadSubpackage:fail subpackage not configured: ${reqName}` };
            fail(res);
            complete(res);
        });
        return task;
    }

    return _startDownload("preDownloadSubpackage", options, pkg, false);
}

export {
    loadSubpackage,
    preDownloadSubpackage,
    _internalOnSubpackageProgress,
    _internalOnSubpackageResult,
};

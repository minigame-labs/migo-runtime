// Wraps a function with success/fail/complete callbacks + Promise return.
//
// Usage:
//   return wrapAsync('startDeviceMotionListening', () => op_start(interval), options);
//
// - If fn() returns a Promise, chains on it; otherwise wraps in Promise.resolve().
// - Always returns a Promise (supports both callback and await patterns).
// - On success: calls success(res), complete(res), resolves with res.
// - On failure: calls fail(res), complete(res), rejects with res.
import { op_alloc_host_callback_id } from "ext:core/ops";
import { _perf } from "ext:host_v8_base/05_perf.js";

// Host callback ids cross JNI and the C ABI as signed 32-bit values, so the
// space ends here and not at `Number.MAX_SAFE_INTEGER`.
const MAX_HOST_CALLBACK_ID = 2147483647;

// What the engine will accept as an id, applied to anything a platform sends
// back. Deliberately strict: `Number()` turns `null` into 0 and `"1e3"` into
// 1000, and an id that was silently coerced is an id that can match the wrong
// pending request.
function parseHostCallbackId(value) {
    // The type check is not redundant with the range check below it: Number()
    // maps `true` to 1, `[]` to 0 and `null` to 0, so a boolean `requestId`
    // would otherwise parse as the id 1 and settle whichever request holds it.
    // Strings stay acceptable because a platform that serialises the id it was
    // given is still echoing it exactly.
    if (typeof value !== 'number' && typeof value !== 'string') return null;
    const id = Number(value);
    return Number.isInteger(id) && id > 0 && id <= MAX_HOST_CALLBACK_ID ? id : null;
}

// One id space per Host, allocated natively, shared with every Worker and
// carried across a runtime restart. A per-runtime counter would restart at 1,
// and a result the retired runtime is still owed would then match a request the
// replacement has just registered.
function allocateHostCallbackId() {
    const id = op_alloc_host_callback_id();
    if (parseHostCallbackId(id) === null) throw new Error("invalid host callback id");
    return id;
}

// Invoke one of the caller's callbacks without letting it escape.
//
// These are the app's functions, not ours. One that throws must not decide
// whether the rest run: `complete` is documented as running either way, and a
// caller awaiting the promise asked about the *operation*, not about its own
// callback. Reported rather than swallowed, matching the `ready` callback in
// 16_jssdk.js, so a throwing callback stays debuggable instead of vanishing.
// The message of something that was thrown, when there is no guarantee it is
// an Error.
//
// `(e.message || String(e))` reads as if it already handles that, and does not:
// a thrown `undefined` makes the property read itself throw, so the handler
// meant to report the failure becomes a second, worse one. That is not
// hypothetical -- a failing `#[op2(fast)]` returning AudioError arrives here as
// literal `undefined`, which is how `setInnerAudioOption` came to throw a
// TypeError out of itself on any platform without audio services, running
// neither `fail` nor `complete`.
//
// Deliberately not `errorToString`: that one prepends the name and appends the
// stack, and these strings are content-visible `errMsg` values.
function errorMessage(err) {
    if (err == null) return String(err);
    var m = err.message;
    return (typeof m === 'string' && m) ? m : String(err);
}

function invokeCallback(apiName, kind, cb, res) {
    if (typeof cb !== 'function') return;
    try {
        cb(res);
    } catch (e) {
        console.error(apiName + ' ' + kind + ' callback error:', e);
    }
}

function wrapAsync(apiName, fn, options) {
    const { success, fail, complete } = options || {};
    const profiling = _perf.enabled;
    const t0 = profiling ? performance.now() : 0;
    try {
        const result = fn();
        if (profiling) {
            const syncElapsed = performance.now() - t0;
            if (syncElapsed >= _perf.syncMs) {
                console.warn('[MigoPerf][Sync] ' + apiName + ': ' + syncElapsed.toFixed(1) + 'ms');
            }
        }
        const p = (result instanceof Promise) ? result : Promise.resolve(result);
        return p.then(
            function (value) {
                if (profiling) {
                    const totalElapsed = performance.now() - t0;
                    if (totalElapsed >= _perf.asyncMs) {
                        console.warn('[MigoPerf][Async] ' + apiName + ': ' + totalElapsed.toFixed(0) + 'ms');
                    }
                }
                const res = (typeof value === 'object' && value !== null)
                    ? { errMsg: apiName + ':ok', ...value }
                    : { errMsg: apiName + ':ok' };
                invokeCallback(apiName, 'success', success, res);
                invokeCallback(apiName, 'complete', complete, res);
                return res;
            },
            function (e) {
                if (profiling) {
                    const totalElapsed = performance.now() - t0;
                    if (totalElapsed >= _perf.asyncMs) {
                        console.warn('[MigoPerf][Async] ' + apiName + ' (fail): ' + totalElapsed.toFixed(0) + 'ms');
                    }
                }
                const res = { errMsg: apiName + ':fail ' + errorMessage(e) };
                invokeCallback(apiName, 'fail', fail, res);
                invokeCallback(apiName, 'complete', complete, res);
                throw res;
            }
        );
    } catch (e) {
        const res = { errMsg: apiName + ':fail ' + errorMessage(e) };
        queueMicrotask(function () { invokeCallback(apiName, 'fail', fail, res); });
        queueMicrotask(function () { invokeCallback(apiName, 'complete', complete, res); });
        return Promise.reject(res);
    }
}

// Higher-level factory: creates a callback+Promise async API from an executor.
//
// Usage:
//   const getStorage = promisify("getStorage", (opts) => {
//       return { data: getStorageSync(opts.key) };
//   });
//
//   // Callback style
//   getStorage({ key: "k", success(res) { console.log(res.data); } });
//
//   // Promise style
//   const res = await getStorage({ key: "k" });
//
// The executor receives the options object (minus callbacks) and should:
//   - Return nothing for void APIs (setStorage, removeStorage, ...)
//   - Return an object whose fields are merged into the success result
//   - Throw on error
//   - Return a Promise for truly async work
function promisify(apiName, executor) {
    return function (options) {
        return wrapAsync(apiName, function () {
            return executor(options || {});
        }, options);
    };
}

// Factory for Mode C async APIs (platform callback via EvalScript).
//
// Creates a deferred Promise+callback pair for APIs where the op only fires
// an async platform request and the result arrives later through a separate
// `_internalOn*Result` callback.
//
// Supports concurrent requests via Map-based tracking (requestId per call).
// settle() matches by parsed.requestId if present, otherwise settles the
// oldest pending request (FIFO fallback for backward compatibility).
//
// Usage:
//   const _loc = createDeferredApi('getLocation');
//
//   function getLocation(options = {}) {
//       return _loc.invoke(options, function (opts, requestId) {
//           op_get_location(JSON.stringify({ type: opts.type || 'wgs84' }));
//       });
//   }
//   function _internalOnLocationResult(json) { _loc.settle(json); }
//
// - invoke(options, executor): stores callbacks + resolve/reject, calls executor, returns Promise
// - settle(resultJson): parses JSON, resolves/rejects, fires success/fail/complete
//
// Both callback and Promise styles are supported:
//   getLocation({ success(res) { ... } });        // callback
//   const res = await getLocation({ type: 'gcj02' }); // promise
// defaultTimeoutMs: auto-reject after this many ms if platform never settles.
// Pass 0 to disable timeout.  Individual calls can override via opts._timeout.
var MAX_DEFERRED_PENDING = 256;
var _deferredPendingCount = 0;
// This is a runtime-wide item budget, shared by every deferred API instance
// in this JavaScript runtime. A timer limit is not a request budget: timer
// admission can fail, and APIs with timeout disabled would otherwise have no
// bound.

function createDeferredApi(apiName, defaultTimeoutMs) {
    var _pending = new Map();
    if (defaultTimeoutMs === undefined) defaultTimeoutMs = 30000;

    function removePending(requestId) {
        var entry = _pending.get(requestId);
        if (!entry) return null;
        _pending.delete(requestId);
        _deferredPendingCount--;
        return entry;
    }

    function _settleEntry(entry, parsed) {
        clearTimeout(entry._timer);
        if (entry._t0) {
            var elapsed = performance.now() - entry._t0;
            if (elapsed >= _perf.deferredMs) {
                console.warn('[MigoPerf][Deferred] ' + apiName + ': ' + elapsed.toFixed(0) + 'ms');
            }
        }
        if (parsed.error) {
            var res = { errMsg: parsed.error };
            if (parsed.errCode !== undefined) res.errCode = parsed.errCode;
            invokeCallback(apiName, 'fail', entry.fail, res);
            invokeCallback(apiName, 'complete', entry.complete, res);
            entry.reject(res);
        } else {
            var res = { errMsg: apiName + ':ok' };
            var keys = Object.keys(parsed);
            for (var i = 0; i < keys.length; i++) {
                var k = keys[i];
                if (k !== 'requestId') res[k] = parsed[k];
            }
            invokeCallback(apiName, 'success', entry.success, res);
            invokeCallback(apiName, 'complete', entry.complete, res);
            entry.resolve(res);
        }
    }

    function invoke(options, executor) {
        var opts = options || {};
        var success = typeof opts.success === 'function' ? opts.success : null;
        var fail = typeof opts.fail === 'function' ? opts.fail : null;
        var complete = typeof opts.complete === 'function' ? opts.complete : null;

        return new Promise(function (resolve, reject) {
            // Admission is runtime-wide and independent of timer capacity:
            // timeout=0 requests still need a finite in-flight bound.
            if (_deferredPendingCount >= MAX_DEFERRED_PENDING) {
                var capFailure = { errMsg: apiName + ':fail pending limit' };
                invokeCallback(apiName, 'fail', fail, capFailure);
                invokeCallback(apiName, 'complete', complete, capFailure);
                reject(capFailure);
                return;
            }

            // Before the pending entry and before the platform call: an
            // exhausted id space must leave nothing registered and dispatch
            // nothing, rather than register under an id it could not obtain.
            var requestId;
            try {
                requestId = allocateHostCallbackId();
            } catch (e) {
                var allocFailure = { errMsg: apiName + ':fail ' + errorMessage(e) };
                invokeCallback(apiName, 'fail', fail, allocFailure);
                invokeCallback(apiName, 'complete', complete, allocFailure);
                reject(allocFailure);
                return;
            }

            var pendingEntry = {
                resolve: resolve,
                reject: reject,
                success: success,
                fail: fail,
                complete: complete,
                _timer: 0,
                _t0: _perf.enabled ? performance.now() : 0,
            };
            _pending.set(requestId, pendingEntry);
            _deferredPendingCount++;

            // Timer admission and executor dispatch are one rollback scope:
            // either both start, or the pending entry and timer are removed and
            // the caller receives exactly one failure settlement.
            try {
                var ms = (opts && typeof opts._timeout === 'number') ? opts._timeout : defaultTimeoutMs;
                if (ms > 0) {
                    pendingEntry._timer = setTimeout(function () {
                        var e = removePending(requestId);
                        if (!e) return;
                        var res = { errMsg: apiName + ':fail timeout' };
                        invokeCallback(apiName, 'fail', e.fail, res);
                        invokeCallback(apiName, 'complete', e.complete, res);
                        e.reject(res);
                    }, ms);
                }
                executor(opts, requestId);
            } catch (e) {
                var entry = removePending(requestId);
                if (!entry) return;
                clearTimeout(entry._timer);
                var res = { errMsg: apiName + ':fail ' + errorMessage(e) };
                invokeCallback(apiName, 'fail', entry.fail, res);
                invokeCallback(apiName, 'complete', entry.complete, res);
                entry.reject(res);
            }
        });
    }

    function settle(resultJson) {
        var parsed;
        try { parsed = JSON.parse(resultJson); } catch (e) { parsed = {}; }
        settleParsed(parsed);
    }

    // The same settlement from an object the caller already holds.
    //
    // The results that cross JNI as integers -- a modal's confirm/cancel, an
    // action sheet's tap index -- have no JSON to parse, and stringifying them
    // just so this function could parse them back would be the only reason the
    // encoding existed. Correlation is one implementation either way: the hooks
    // build the object, this decides whose request it answers.
    function settleParsed(parsed) {
        // A result that carries the key at all is correlated by it, in any
        // form. `null`, `1.5`, `-1`, `0`, `2147483648` and `"abc"` are all
        // *present and not an id*, so they are discarded rather than allowed
        // to reach the fallback below and settle somebody else's request.
        if (parsed !== null && typeof parsed === 'object' && 'requestId' in parsed) {
            var requestId = parseHostCallbackId(parsed.requestId);
            if (requestId === null) return;
            var entry = removePending(requestId);
            if (entry) _settleEntry(entry, parsed);
            // Present but unknown: already settled, or timed out. Discarding is
            // the only safe answer -- there is no second candidate.
            return;
        }

        // Fallback: settle the oldest pending request (FIFO), reached only when
        // the platform omits `requestId` entirely.
        //
        // Do not delete this yet, and the reason has moved. Every Android path
        // now echoes the id -- location, scan, image, video, modal, action
        // sheet, Bluetooth and application settings. What is left is the other
        // three platforms: a host that answers without an id is not a bug in
        // this file, and deleting the fallback would not make it fail loudly,
        // it would leave its promises unsettled forever. This goes when every
        // shipped host echoes, not when Android does.
        var iter = _pending.keys();
        var first = iter.next();
        if (!first.done) {
            console.warn(
                'createDeferredApi(' + apiName + '): response has no requestId, ' +
                'using FIFO fallback. Platform Manager should include requestId ' +
                'in the result JSON to support concurrent requests correctly.'
            );
            var entry = removePending(first.value);
            _settleEntry(entry, parsed);
        }
    }

    // Native cancellation, when a platform supports it, uses the same
    // settlement gate as timeout and completion. It never returns a request
    // slot twice, and it does not claim native work was cancelled.
    function cancel(requestId, reason) {
        var entry = removePending(requestId);
        if (!entry) return false;
        clearTimeout(entry._timer);
        var res = { errMsg: apiName + ':fail ' + (reason || 'cancelled') };
        invokeCallback(apiName, 'fail', entry.fail, res);
        invokeCallback(apiName, 'complete', entry.complete, res);
        entry.reject(res);
        return true;
    }

    function pendingCount() {
        return _pending.size;
    }

    return {
        invoke: invoke,
        settle: settle,
        settleParsed: settleParsed,
        cancel: cancel,
        pendingCount: pendingCount,
    };

}

// Factory for event listener groups (on/off/trigger pattern).
//
// Standardizes the on/off semantics used across 20+ modules:
//   - on(fn)         adds a listener
//   - off(fn)        removes that specific listener
//   - off()          removes all listeners
//   - trigger(data)  calls each listener, catching errors
//
// Usage:
//   const grp = createListenerGroup('onAccelerometerChange');
//   export const { on: onAccelerometerChange, off: offAccelerometerChange } = grp;
//   function _internalTrigger(x, y, z) { grp.trigger({ x, y, z }); }
//
// Pass unique=true to dedupe listeners (DOM-style addEventListener semantics).
function createListenerGroup(errorLabel, unique) {
    var _listeners = [];
    if (unique === undefined) unique = false;
    return {
        on: function (listener) {
            if (typeof listener !== 'function') return;
            if (unique && _listeners.indexOf(listener) !== -1) return;
            _listeners.push(listener);
        },
        off: function (listener) {
            if (typeof listener === 'function') {
                var i = _listeners.indexOf(listener);
                if (i !== -1) _listeners.splice(i, 1);
            } else {
                _listeners.length = 0;
            }
        },
        trigger: function (data, thisArg) {
            for (var i = 0; i < _listeners.length; i++) {
                try {
                    if (thisArg !== undefined) {
                        _listeners[i].call(thisArg, data);
                    } else {
                        _listeners[i](data);
                    }
                } catch (e) {
                    // Log the normalized error string rather than the raw
                    // object.  Some game exceptions are plain objects (or
                    // Error objects crossing V8/native formatting) and showed
                    // up in logcat as just "{}"; that made onShow/onHide
                    // resume failures indistinguishable from a swallowed
                    // callback.  Keep dispatching remaining listeners.
                    console.error(errorLabel + ' listener error: ' + errorToString(e));
                }
            }
        },
        snapshot: function () {
            return _listeners.slice();
        },
        size: function () {
            return _listeners.length;
        },
    };
}

// Create a lightweight callback event object with optional extra fields.
//
// Usage:
//   createCallbackEvent('load', image)
//   createCallbackEvent('error', image, { error: err })
function createCallbackEvent(type, target, detail) {
    var ev = {
        type: type,
        target: target,
        currentTarget: target,
        timeStamp: Date.now(),
    };
    if (detail && typeof detail === 'object') {
        var keys = Object.keys(detail);
        for (var i = 0; i < keys.length; i++) {
            ev[keys[i]] = detail[keys[i]];
        }
    }
    return ev;
}

// Format an error value into a human-readable string (safe for any thrown value).
function errorToString(err) {
    if (err == null) return String(err);
    var t = typeof err;
    if (t === 'string' || t === 'number' || t === 'boolean') return String(err);
    var name = typeof err.name === 'string' ? err.name : 'Error';
    var message = typeof err.message === 'string' ? err.message : '';
    var stack = typeof err.stack === 'string' ? err.stack : '';
    var base = message ? name + ': ' + message : name;
    return stack ? base + '\n' + stack : base;
}

export {
    wrapAsync,
    promisify,
    createDeferredApi,
    createListenerGroup,
    createCallbackEvent,
    errorToString,
    // Exported for the modules that own their own pending maps rather
    // than going through createDeferredApi. They must draw from the same
    // space and apply the same parser -- a second allocator or a looser
    // parser here is a second answer to who an id belongs to.
    allocateHostCallbackId,
    parseHostCallbackId,
    // Same reasoning as the id allocator above: a module that settles its own
    // pending map still owes the caller the documented callback sequence, and a
    // second local implementation of "run these without letting one stop the
    // rest" is a second answer to that.
    invokeCallback,
    errorMessage,
};

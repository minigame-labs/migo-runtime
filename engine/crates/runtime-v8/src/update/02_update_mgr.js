// getUpdateManager: the host's updates of the content package, as content's
// UpdateManager hears them.
//
// The host checks for a new version every time the game starts, warm starts
// included, downloads what it finds and restarts the game on it when content
// asks (shared::services::UpdateService). Each step it reports -- the check's
// result, then ready or failed -- is kept here as well as delivered, so a
// listener registered after the step still hears it: the host's launch check
// may well answer before the game asks for its manager. A host that offers no
// updates has none to report, and content is told "no update" at once.
//
// Nothing here decides that an update exists. The manager once did, by coin
// flip, and a quarter of launches offered a restart into a version that was
// not there (tests/update_manager.rs).

import { op_apply_update, op_update_available } from "ext:core/ops";
import { createListenerGroup } from "ext:host_v8_base/02_async.js";

// What the host last reported: the check's result, and what became of the
// update it found.
let _check = null;
let _outcome = null;

const _checkListeners = createListenerGroup('UpdateManager onCheckForUpdate');
const _readyListeners = createListenerGroup('UpdateManager onUpdateReady');
const _failedListeners = createListenerGroup('UpdateManager onUpdateFailed');

function _hearLate(listener, data) {
    queueMicrotask(function () {
        try {
            listener(data);
        } catch (e) {
            console.error('UpdateManager listener error:', e);
        }
    });
}

class UpdateManager {
    onCheckForUpdate(listener) {
        if (typeof listener !== 'function') return;
        _checkListeners.on(listener);
        if (_check !== null) _hearLate(listener, { hasUpdate: _check.hasUpdate });
    }

    onUpdateReady(listener) {
        if (typeof listener !== 'function') return;
        _readyListeners.on(listener);
        if (_outcome === 'ready') _hearLate(listener, {});
    }

    onUpdateFailed(listener) {
        if (typeof listener !== 'function') return;
        _failedListeners.on(listener);
        if (_outcome === 'failed') _hearLate(listener, {});
    }

    // Restarts the game on the downloaded version: asked of the host, and only
    // once it said the version is ready.
    applyUpdate() {
        if (_outcome !== 'ready') {
            console.warn('UpdateManager.applyUpdate: no update is ready; call it after onUpdateReady');
            return;
        }
        try {
            op_apply_update();
        } catch (e) {
            console.error('UpdateManager.applyUpdate: ' + e);
        }
    }
}

let _manager = null;

function getUpdateManager() {
    if (_manager === null) {
        _manager = new UpdateManager();
        // No host to check: the answer is already known.
        if (_check === null && !op_update_available()) _check = { hasUpdate: false };
    }
    return _manager;
}

// `{"event": "checkForUpdate", "hasUpdate"}`, `{"event": "updateReady"}` or
// `{"event": "updateFailed"}`, from the host.
function _internalOnUpdateEvent(eventJson) {
    let event;
    try {
        event = JSON.parse(eventJson);
    } catch (_) {
        return;
    }
    if (event === null || typeof event !== 'object') return;
    if (event.event === 'checkForUpdate' && typeof event.hasUpdate === 'boolean') {
        // A new check -- every launch has one -- starts the story again.
        _check = { hasUpdate: event.hasUpdate };
        _outcome = null;
        _checkListeners.trigger({ hasUpdate: event.hasUpdate });
    } else if (event.event === 'updateReady') {
        _outcome = 'ready';
        _readyListeners.trigger({});
    } else if (event.event === 'updateFailed') {
        _outcome = 'failed';
        _failedListeners.trigger({});
    }
}

export { getUpdateManager, UpdateManager, _internalOnUpdateEvent };

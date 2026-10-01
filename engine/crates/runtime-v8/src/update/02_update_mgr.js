// @stub getUpdateManager reports that no update exists, because this runtime has
//       no update channel to find one on. Every listener is real; onUpdateReady
//       and onUpdateFailed simply never fire, and applyUpdate has nothing to
//       apply.
//
// It used to invent them. `Math.random() < 0.3` decided at construction whether
// content was told an update was waiting; a random 2-5 s later, a second coin
// flip fired onUpdateReady (90%) or onUpdateFailed (10%). So roughly a quarter
// of launches showed the game's own "new version -- restart?" prompt, the player
// accepted, applyUpdate() logged "Application restarted with new version", and
// nothing restarted. Nondeterministically, which is the worst way for content to
// meet a missing capability: it works in testing and fails in the field, in a
// different place each time.
//
// The truthful answer was already in this file. `checkUpdate()` below -- the
// callback-style API for the same question -- has always answered
// `hasUpdate: false`. Two entry points to one question disagreed, and the one
// that fabricated was the one with no test and no @stub marker, so the prescreen
// report told customers it was supported.

import { createListenerGroup, failMessage } from "ext:host_v8_base/02_async.js";

class UpdateManager {
    constructor() {
        this.checkForUpdateListeners = createListenerGroup('UpdateManager onCheckForUpdate');
        this.updateReadyListeners = createListenerGroup('UpdateManager onUpdateReady');
        this.updateFailedListeners = createListenerGroup('UpdateManager onUpdateFailed');
        this.hasUpdate = false;
        this.isReady = false;

        // The real API reports the result of a launch-time check asynchronously,
        // so the shape is kept: listeners registered during startup still hear
        // an answer. The answer is the same one checkUpdate() gives.
        setTimeout(() => {
            this._reportNoUpdate();
        }, 1000);
    }
    
    onCheckForUpdate(listener) {
        this.checkForUpdateListeners.on(listener);
    }
    
    onUpdateReady(listener) {
        this.updateReadyListeners.on(listener);
    }
    
    onUpdateFailed(listener) {
        this.updateFailedListeners.on(listener);
    }
    
    applyUpdate() {
        // isReady can never become true, so this is the only branch. Saying so is
        // the point: the previous version's other branch claimed a restart that
        // never happened.
        console.warn(
            'UpdateManager.applyUpdate: no update is ready to apply. This build has ' +
            'no update channel, so onUpdateReady never fires and there is nothing ' +
            'to apply.'
        );
    }

    _reportNoUpdate() {
        this.hasUpdate = false;
        this.checkForUpdateListeners.trigger({ hasUpdate: false });
    }
}

let updateManagerInstance = null;

function getUpdateManager() {
    if (!updateManagerInstance) {
        updateManagerInstance = new UpdateManager();
    }
    return updateManagerInstance;
}

function checkUpdate(options = {}) {
    const { success, fail, complete } = options;
    try {
        const result = {
            hasUpdate: false,
            errMsg: 'checkUpdate:ok',
        };
        if (typeof success === 'function') {
            success(result);
        }
        if (typeof complete === 'function') {
            complete(result);
        }
        return Promise.resolve(result);
    } catch (error) {
        const errorResult = {
            errMsg: failMessage('checkUpdate', error),
        };
        if (typeof fail === 'function') {
            fail(errorResult);
        }
        if (typeof complete === 'function') {
            complete(errorResult);
        }
        return Promise.reject(errorResult);
    }
}

export { getUpdateManager, UpdateManager, checkUpdate };

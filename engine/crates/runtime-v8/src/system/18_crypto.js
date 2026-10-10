// UserCryptoManager
//
// @stub getUserCryptoManager: its getRandomValues always calls fail callback
// with "not supported". Its getLatestUserKey is the host's to answer: the
// user's key is issued by the host's backend, so it is an ecosystem request
// (20_ecosystem.js).

import { wrapAsync } from "ext:host_v8_base/02_async.js";
import { ecosystemMethod } from "ext:host_v8_system/20_ecosystem.js";

class UserCryptoManager {
    getLatestUserKey(options) {
        return ecosystemMethod('getLatestUserKey', options);
    }

    getRandomValues(options) {
        return wrapAsync('getRandomValues', function () {
            throw new Error('not supported');
        }, options);
    }
}

function getUserCryptoManager() {
    return new UserCryptoManager();
}

export { getUserCryptoManager };

// UserCryptoManager
//
// getRandomValues is the OS's cryptographically secure random source, as many
// bytes as asked for; getLatestUserKey is the host's to answer -- the user's key
// is issued by the host's backend -- so it is an ecosystem request
// (20_ecosystem.js).

import { op_crypto_random_values } from "ext:core/ops";
import { wrapAsync } from "ext:host_v8_base/02_async.js";

// The most bytes one call may ask for, as on the common platform.
const RANDOM_VALUES_MAX = 1048576;
import { ecosystemObjectCall } from "ext:host_v8_system/20_ecosystem.js";

class UserCryptoManager {
    getLatestUserKey(options) {
        return ecosystemObjectCall('UserCryptoManager.getLatestUserKey', options);
    }

    getRandomValues(options) {
        const opts = options || {};
        return wrapAsync('getRandomValues', function () {
            const length = opts.length;
            if (!Number.isInteger(length) || length < 1 || length > RANDOM_VALUES_MAX) {
                throw new Error('length must be an integer from 1 to ' + RANDOM_VALUES_MAX);
            }
            const bytes = new Uint8Array(length);
            op_crypto_random_values(bytes);
            return { randomValues: bytes.buffer };
        }, options);
    }
}

function getUserCryptoManager() {
    return new UserCryptoManager();
}

export { getUserCryptoManager };

import { core } from "ext:core/mod.js";
import { op_game_log_report } from "ext:core/ops";
import { isCallbackStyle, wrapAsync } from "ext:host_v8_base/02_async.js";

// The reference's limits, for one run of the game: at most 999 logs, each of
// at most 16 KB as it leaves for the host.
const MAX_LOG_COUNT = 999;
const MAX_LOG_BYTES = 16 * 1024;

// The logs this run of the game has reported, through every manager it made:
// the limit is the run's, so a new manager does not start it again.
let _reported = 0;

class GameLogManager {
    #commonInfo;
    #debug;

    constructor(commonInfo, debug) {
        this.#commonInfo = (commonInfo && typeof commonInfo === 'object')
            ? JSON.parse(JSON.stringify(commonInfo))
            : {};
        this.#debug = !!debug;
    }

    log(param) {
        const options = param || {};
        const self = this;
        return wrapAsync('gameLog.log', function () {
            const level = options.level;
            if (level !== 'info' && level !== 'warn' && level !== 'error' && level !== 'debug') {
                throw new Error('level must be one of info/warn/error/debug');
            }
            if (_reported >= MAX_LOG_COUNT) {
                throw new Error('log count limit exceeded (max ' + MAX_LOG_COUNT + ')');
            }
            var payload = JSON.stringify({
                source: 'gameLog',
                level: level,
                key: (options.key !== undefined && options.key !== null) ? String(options.key) : 'default',
                value: options.value,
                commonInfo: self.#commonInfo,
            });
            if (core.encode(payload).byteLength > MAX_LOG_BYTES) {
                throw new Error('log size exceeds limit (max 16KB)');
            }
            op_game_log_report(payload);
            _reported++;
            if (self.#debug) {
                console.log('[GameLog]', payload);
            }
        }, options);
    }

    tag(key) {
        const tagKey = (key !== undefined && key !== null) ? String(key) : 'default';
        const self = this;
        function makeTagMethod(level) {
            return function (value) {
                return self.log({ level: level, key: tagKey, value: value });
            };
        }
        return {
            info: makeTagMethod('info'),
            warn: makeTagMethod('warn'),
            error: makeTagMethod('error'),
            debug: makeTagMethod('debug'),
        };
    }

    getCommonInfo() {
        return JSON.parse(JSON.stringify(this.#commonInfo));
    }

    updateCommonInfo(newCommonInfo) {
        if (newCommonInfo && typeof newCommonInfo === 'object') {
            var keys = Object.keys(newCommonInfo);
            for (var i = 0; i < keys.length; i++) {
                this.#commonInfo[keys[i]] = newCommonInfo[keys[i]];
            }
        }
    }
}

function getGameLogManager(param) {
    const options = param || {};
    const mgr = new GameLogManager(options.commonInfo, options.debug);
    // Answered once the manager is returned, as every callback is: a success
    // callback that uses the manager reads the variable it was assigned to,
    // which would not exist yet were it called from in here.
    if (isCallbackStyle(options)) wrapAsync('getGameLogManager', function () {}, options);
    return mgr;
}

export { getGameLogManager };

import { op_game_log_report } from "ext:core/ops";
import { wrapAsync } from "ext:host_v8_base/02_async.js";

const MAX_LOG_COUNT = 999;
const MAX_LOG_SIZE = 16 * 1024; // 16KB

class GameLogManager {
    constructor(commonInfo, debug) {
        this._commonInfo = (commonInfo && typeof commonInfo === 'object')
            ? JSON.parse(JSON.stringify(commonInfo))
            : {};
        this._debug = !!debug;
        this._logCount = 0;
    }

    log(param) {
        const options = param || {};
        const self = this;
        return wrapAsync('gameLog.log', function () {
            const level = options.level;
            if (level !== 'info' && level !== 'warn' && level !== 'error' && level !== 'debug') {
                throw new Error('level must be one of info/warn/error/debug');
            }
            if (self._logCount >= MAX_LOG_COUNT) {
                throw new Error('log count limit exceeded (max ' + MAX_LOG_COUNT + ')');
            }
            var payload = JSON.stringify({
                source: 'gameLog',
                level: level,
                key: (options.key !== undefined && options.key !== null) ? String(options.key) : 'default',
                value: options.value,
                commonInfo: self._commonInfo,
            });
            if (payload.length > MAX_LOG_SIZE) {
                throw new Error('log size exceeds limit (max 16KB)');
            }
            op_game_log_report(payload);
            self._logCount++;
            if (self._debug) {
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
        return JSON.parse(JSON.stringify(this._commonInfo));
    }

    updateCommonInfo(newCommonInfo) {
        if (newCommonInfo && typeof newCommonInfo === 'object') {
            var keys = Object.keys(newCommonInfo);
            for (var i = 0; i < keys.length; i++) {
                this._commonInfo[keys[i]] = newCommonInfo[keys[i]];
            }
        }
    }
}

function getGameLogManager(param) {
    const options = param || {};
    const mgr = new GameLogManager(options.commonInfo, options.debug);
    var res = { errMsg: 'getGameLogManager:ok' };
    if (typeof options.success === 'function') {
        try { options.success(res); } catch (_) {}
    }
    if (typeof options.complete === 'function') {
        try { options.complete(res); } catch (_) {}
    }
    return mgr;
}

export { getGameLogManager };

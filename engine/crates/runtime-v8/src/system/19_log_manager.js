// LogManager / RealtimeLogManager
//
// LogManager writes the local log -- the console, which every host keeps.
// RealtimeLogManager reports as it logs, to the host's game log (09_game_log.js's
// channel: a C ABI host's on_game_log, an Android GameLogHandler), each entry
// marked `source: "realtime"` with the filter keywords content set; with no host
// to take them the entries go to the console instead, where they are still read.

import { op_game_log_report } from "ext:core/ops";

class LogManager {
    debug() { console.debug.apply(console, arguments); }
    info() { console.info.apply(console, arguments); }
    log() { console.log.apply(console, arguments); }
    warn() { console.warn.apply(console, arguments); }
}

// The most one realtime entry may carry, and one filter keyword, as on the
// common platform.
const REALTIME_ENTRY_MAX = 5 * 1024;
const FILTER_MSG_MAX = 1024;

// The filter keywords, shared by every realtime logger: the platform keeps one
// set per program.
let _filterMsg = [];

function _report(level, key, value) {
    let entry;
    try {
        entry = JSON.stringify({ source: 'realtime', level: level, key: key, value: value,
            filterMsg: _filterMsg });
    } catch (e) {
        console.warn('RealtimeLogManager: an entry JSON cannot carry was dropped:', e);
        return;
    }
    if (entry.length > REALTIME_ENTRY_MAX) {
        console.warn('RealtimeLogManager: an entry over ' + REALTIME_ENTRY_MAX + ' bytes was dropped');
        return;
    }
    try {
        op_game_log_report(entry);
    } catch (_) {
        // No host takes game logs: the console does.
        (level === 'error' ? console.error : level === 'warn' ? console.warn : console.info)
            .apply(console, ['[realtime]'].concat(value));
    }
}

class RealtimeLogManager {
    info() { _report('info', 'default', Array.from(arguments)); }
    warn() { _report('warn', 'default', Array.from(arguments)); }
    error() { _report('error', 'default', Array.from(arguments)); }

    setFilterMsg(msg) {
        if (typeof msg === 'string' && msg.length <= FILTER_MSG_MAX) _filterMsg = [msg];
    }

    addFilterMsg(msg) {
        if (typeof msg === 'string' && msg.length <= FILTER_MSG_MAX) _filterMsg = _filterMsg.concat(msg);
    }

    // A logger whose entries carry `tagName` as their key, logging key-value pairs.
    tag(tagName) {
        const key = typeof tagName === 'string' && tagName.length > 0 ? tagName : 'default';
        return {
            info(name, value) { _report('info', key, [name, value]); },
            warn(name, value) { _report('warn', key, [name, value]); },
            error(name, value) { _report('error', key, [name, value]); },
        };
    }
}

const _realtimeLogManager = new RealtimeLogManager();

function getLogManager() {
    return new LogManager();
}

function getRealtimeLogManager() {
    return _realtimeLogManager;
}

export { getLogManager, getRealtimeLogManager };

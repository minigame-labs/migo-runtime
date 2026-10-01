// The time of an input event, on the clock content reads.
//
// A host stamps input with its own clock, and the C ABI does not name one: Apple hosts send the system uptime, Android
// the uptime of the motion event, and the number has nothing to do with when this game started. Content compares
// `event.timeStamp` with `performance.now()` and with the timestamp requestAnimationFrame hands it -- input smoothing,
// double-tap windows measured against the frame clock, latency probes, every web game that was ever ported -- so a
// host's epoch must not reach it: a touch stamped 62067560 ms "ahead of now" is not an input event, it is a bug that
// only shows on the one platform whose clock disagrees.
//
// What the ABI does promise is that one host's stamps are on one clock whose differences are real time. So the host's
// clock is anchored to the page's at the first event of a burst of input (any gap of a quarter second starts a new one,
// which also keeps the two clocks from drifting apart over a long session or a sleep), and inside the burst the spacing
// is the host's: a drag's samples are as far apart as the finger moved them, not as far apart as the game got around to
// reading them. The result is never in the future.
const BURST_GAP_MS = 250;

let _anchorHost = NaN;
let _anchorPage = 0;
let _lastSeen = -Infinity;

function pageTime(hostMs) {
    const now = performance.now();
    if (typeof hostMs !== 'number' || hostMs !== hostMs || hostMs === Infinity || hostMs === -Infinity) {
        return now;
    }
    if (_anchorHost !== _anchorHost || now - _lastSeen > BURST_GAP_MS || hostMs < _anchorHost) {
        _anchorHost = hostMs;
        _anchorPage = now;
    }
    _lastSeen = now;
    const at = _anchorPage + (hostMs - _anchorHost);
    return at > now ? now : at;
}

export { pageTime };

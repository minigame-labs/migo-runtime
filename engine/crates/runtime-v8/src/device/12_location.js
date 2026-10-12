import { op_get_location, op_get_fuzzy_location } from "ext:core/ops";
import { createDeferredApi } from "ext:host_v8_base/02_async.js";

// Hosts answer WGS-84, the datum every positioning system reports. The common
// platform also offers GCJ-02, the offset datum maps in mainland China require;
// the engine converts here, so every host gives the same answer. The engine
// also rounds getFuzzyLocation's position to 0.01 degrees -- about a kilometre
// -- whatever the host's precision, so the API's privacy does not rest on each
// host remembering to.

const MathAbs = Math.abs;
const MathCos = Math.cos;
const MathPI = Math.PI;
const MathRound = Math.round;
const MathSin = Math.sin;
const MathSqrt = Math.sqrt;

const _locationApi = createDeferredApi('getLocation');
const _fuzzyLocationApi = createDeferredApi('getFuzzyLocation');

// ---- WGS-84 to GCJ-02 ----

// The published transform on the Krasovsky 1940 ellipsoid, applied inside
// mainland China's bounding box only: elsewhere the two datums agree.
const KRASOVSKY_A = 6378245.0;
const KRASOVSKY_EE = 0.00669342162296594323;

function _outOfChina(lat, lng) {
    return lng < 72.004 || lng > 137.8347 || lat < 0.8293 || lat > 55.8271;
}

function _offsetLat(x, y) {
    let r = -100.0 + 2.0 * x + 3.0 * y + 0.2 * y * y + 0.1 * x * y + 0.2 * MathSqrt(MathAbs(x));
    r += (20.0 * MathSin(6.0 * x * MathPI) + 20.0 * MathSin(2.0 * x * MathPI)) * 2.0 / 3.0;
    r += (20.0 * MathSin(y * MathPI) + 40.0 * MathSin(y / 3.0 * MathPI)) * 2.0 / 3.0;
    r += (160.0 * MathSin(y / 12.0 * MathPI) + 320.0 * MathSin(y * MathPI / 30.0)) * 2.0 / 3.0;
    return r;
}

function _offsetLng(x, y) {
    let r = 300.0 + x + 2.0 * y + 0.1 * x * x + 0.1 * x * y + 0.1 * MathSqrt(MathAbs(x));
    r += (20.0 * MathSin(6.0 * x * MathPI) + 20.0 * MathSin(2.0 * x * MathPI)) * 2.0 / 3.0;
    r += (20.0 * MathSin(x * MathPI) + 40.0 * MathSin(x / 3.0 * MathPI)) * 2.0 / 3.0;
    r += (150.0 * MathSin(x / 12.0 * MathPI) + 300.0 * MathSin(x / 30.0 * MathPI)) * 2.0 / 3.0;
    return r;
}

// `result` with its latitude and longitude in GCJ-02.
function _toGcj02(result) {
    const lat = result.latitude;
    const lng = result.longitude;
    if (typeof lat !== 'number' || typeof lng !== 'number' || _outOfChina(lat, lng)) return result;
    const radLat = lat / 180.0 * MathPI;
    let magic = MathSin(radLat);
    magic = 1 - KRASOVSKY_EE * magic * magic;
    const sqrtMagic = MathSqrt(magic);
    const dLat = (_offsetLat(lng - 105.0, lat - 35.0) * 180.0)
        / ((KRASOVSKY_A * (1 - KRASOVSKY_EE)) / (magic * sqrtMagic) * MathPI);
    const dLng = (_offsetLng(lng - 105.0, lat - 35.0) * 180.0)
        / (KRASOVSKY_A / sqrtMagic * MathCos(radLat) * MathPI);
    result.latitude = lat + dLat;
    result.longitude = lng + dLng;
    return result;
}

function _wantsGcj02(options) {
    return options !== null && typeof options === 'object' && options.type === 'gcj02';
}

// ---- getLocation ----

function getLocation(options = {}) {
    return _locationApi.invoke(options, function (opts, requestId) {
        op_get_location(JSON.stringify({
            requestId: requestId,
            altitude: !!opts.altitude,
            isHighAccuracy: !!opts.isHighAccuracy,
            highAccuracyExpireTime: opts.highAccuracyExpireTime || 0,
        }));
    }, _wantsGcj02(options) ? _toGcj02 : undefined);
}

function _internalOnLocationResult(resultJson) {
    _locationApi.settle(resultJson);
}

// ---- getFuzzyLocation ----

// The position only, rounded in WGS-84 and then, if asked, converted -- the
// order the rounding's kilometre is defined in.
function _round(degrees) {
    return typeof degrees === 'number' ? MathRound(degrees * 100) / 100 : degrees;
}

function _fuzzy(gcj02) {
    return function (result) {
        const fuzzy = { latitude: _round(result.latitude), longitude: _round(result.longitude) };
        return gcj02 ? _toGcj02(fuzzy) : fuzzy;
    };
}

function getFuzzyLocation(options = {}) {
    return _fuzzyLocationApi.invoke(options, function (opts, requestId) {
        op_get_fuzzy_location(JSON.stringify({ requestId: requestId }));
    }, _fuzzy(_wantsGcj02(options)));
}

function _internalOnFuzzyLocationResult(resultJson) {
    _fuzzyLocationApi.settle(resultJson);
}

export {
    getLocation,
    _internalOnLocationResult,
    getFuzzyLocation,
    _internalOnFuzzyLocationResult,
};

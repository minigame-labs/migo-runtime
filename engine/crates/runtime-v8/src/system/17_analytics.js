// Analytics: reportEvent, reportMonitor, reportPerformance, reportScene.
//
// Each reports to the host's analytics backend, a request on the ecosystem
// channel (20_ecosystem.js) under its own name. The first three answer content
// nothing, as on the common platform, so they leave and are not waited on;
// reportScene's checks are the platform's own, made before the report leaves,
// and its callbacks hear the host's answer.

import { isCallbackStyle, withCallbacks, withoutCallbacks } from "ext:host_v8_base/02_async.js";
import { ecosystemObjectCall } from "ext:host_v8_system/20_ecosystem.js";

function _noop() {}

function _send(name, request) {
    ecosystemObjectCall(name, request).then(undefined, _noop);
}

function _jsonObject(value) {
    if (value === null || typeof value !== 'object') return undefined;
    try {
        return JSON.parse(JSON.stringify(value));
    } catch (_) {
        return undefined;
    }
}

function reportEvent(eventId, data) {
    if (typeof eventId !== 'string' || eventId.length === 0) return;
    _send('reportEvent', { eventId: eventId, data: _jsonObject(data) || {} });
}

function reportMonitor(name, value) {
    if (typeof name !== 'string' || typeof value !== 'number' || !Number.isFinite(value)) return;
    _send('reportMonitor', { name: name, value: value });
}

function reportPerformance(id, value, dimensions) {
    if (typeof id !== 'number' || typeof value !== 'number' || !Number.isFinite(value)) return;
    const request = { id: id, value: value };
    if (typeof dimensions === 'string' || Array.isArray(dimensions)) request.dimensions = dimensions;
    _send('reportPerformance', request);
}

// ---- reportScene -------------------------------------------------------------

// A scene is reported once per launch.
const _reportedScenes = new Set();
const SCENE_FIELD_MAX = 1024;

function _typeName(value) {
    if (value === null) return 'Null';
    if (Array.isArray(value)) return 'Array';
    const type = typeof value;
    return type.charAt(0).toUpperCase() + type.slice(1);
}

// The platform's check of one dimension or metric object, or null when it passes.
function _sceneFieldError(name, value, numeric) {
    if (value === undefined) return null;
    if (_typeName(value) !== 'Object') {
        return name + ' should be Object instead of ' + _typeName(value);
    }
    let json;
    try {
        json = JSON.stringify(value);
    } catch (_) {
        return 'failed to serialize parameter.' + name + ' by JSON.stringify';
    }
    if (json.length > SCENE_FIELD_MAX) return 'parameter.' + name + ' cannot exceed 1024 characters';
    for (const key of Object.keys(value)) {
        const entry = value[key];
        if (typeof entry !== 'string' || entry.length === 0) {
            return 'parameter.' + name + '.' + key + ' needs to be a string type and a non-empty string';
        }
        if (numeric && !/^-?\d+(\.\d+)?$/.test(entry)) {
            return 'parameter.' + name + '.' + key + ' needs to be a numeric value of type string';
        }
    }
    return null;
}

function reportScene(options) {
    const opts = options || {};
    const data = withoutCallbacks(opts);
    let error = null;
    if (typeof opts.sceneId !== 'number') {
        error = 'sceneId should be Number instead of ' + _typeName(opts.sceneId);
    } else if (opts.costTime !== undefined &&
        (typeof opts.costTime !== 'number' || !(opts.costTime >= 0))) {
        error = typeof opts.costTime !== 'number'
            ? 'costTime should be Number instead of ' + _typeName(opts.costTime)
            : 'parameter.costTime should greater than or equal to zero';
    } else {
        error = _sceneFieldError('dimension', opts.dimension, false) ||
            _sceneFieldError('metric', opts.metric, true);
    }
    if (error === null && _reportedScenes.has(opts.sceneId)) {
        error = 'report sceneId:' + opts.sceneId + ' repeatedly';
    }
    let outcome;
    if (error !== null) {
        outcome = Promise.reject({ errMsg: 'reportScene:fail ' + error, data: data });
    } else {
        _reportedScenes.add(opts.sceneId);
        const request = { sceneId: opts.sceneId, costTime: opts.costTime || 0 };
        if (opts.dimension !== undefined) request.dimension = opts.dimension;
        if (opts.metric !== undefined) request.metric = opts.metric;
        outcome = ecosystemObjectCall('reportScene', request).then(function (res) {
            res.data = data;
            return res;
        }, function (res) {
            res.data = data;
            throw res;
        });
    }
    withCallbacks('reportScene', outcome, opts);
    if (isCallbackStyle(opts)) return undefined;
    return outcome;
}

export { reportEvent, reportMonitor, reportScene, reportPerformance };

// createVideoDecoder: a video decoded frame by frame for content to draw.
//
// The platform decodes; the engine runs the decoding on a thread of its own per
// decoder (media/video_decoder.rs), paced by the frames' presentation times
// (mode 0) or as fast as content takes them (mode 1, the default), and reports
// what happens as the decoder's events. getFrameData() takes a decoded frame
// whose pixels are the buffer the platform decoded into -- RGBA, top row first.
//
// Where no decoder is integrated, start() fails as not supported rather than
// leaving content waiting for frames that will never come, and the rest answers
// as a decoder that is not decoding -- without the decoding ops, which a profile
// with no decoder need not offer.

import {
    op_video_decoder_available,
    op_video_decoder_remove,
    op_video_decoder_seek,
    op_video_decoder_start,
    op_video_decoder_stop,
    op_video_decoder_take_frame,
} from "ext:core/ops";
import { createListenerGroup, errorMessage } from "ext:host_v8_base/02_async.js";

const EVENTS = ['start', 'stop', 'seek', 'bufferchange', 'ended'];

let _nextId = 1;
// The decoders that are decoding, by id: each one's events reach it here.
const _decoding = new Map();
// Width, height, pts, dts of the frame last taken.
const _meta = new Float64Array(4);

function _fail(member, reason) {
    return Promise.reject({ errMsg: member + ':fail ' + reason });
}

class VideoDecoder {
    #id = _nextId++;
    #listeners = {};
    #removed = false;
    // Whether the platform was ever asked to decode for this one: until then
    // there is nothing of it outside this object to ask or tell.
    #native = false;
    // The settlement start(), seek() and stop() wait for.
    #starting = null;
    #seeking = [];
    #stopping = [];

    constructor() {
        for (let i = 0; i < EVENTS.length; i++) {
            this.#listeners[EVENTS[i]] = createListenerGroup('VideoDecoder ' + EVENTS[i]);
        }
    }

    start(options) {
        if (this.#removed) return _fail('start', 'the decoder is removed');
        const opts = options || {};
        if (typeof opts.source !== 'string' || opts.source.length === 0) {
            return _fail('start', 'source is required');
        }
        if (!op_video_decoder_available()) return _fail('start', 'not supported');
        if (this.#starting !== null) this.#starting.reject({ errMsg: 'start:fail restarted' });
        const self = this;
        this.#native = true;
        _decoding.set(this.#id, function (event) { self.#event(event); });
        return new Promise(function (resolve, reject) {
            self.#starting = { resolve: resolve, reject: reject };
            op_video_decoder_start(self.#id, JSON.stringify({
                source: opts.source,
                mode: opts.mode === 0 ? 0 : 1,
                abortVideo: opts.abortVideo === true,
            })).then(undefined, function (e) {
                if (self.#starting !== null && self.#starting.reject === reject) self.#starting = null;
                reject({ errMsg: 'start:fail ' + errorMessage(e) });
            });
        });
    }

    getFrameData() {
        if (!this.#native) return null;
        const data = op_video_decoder_take_frame(this.#id, _meta);
        if (data.byteLength === 0) return null;
        return {
            width: _meta[0],
            height: _meta[1],
            data: data.buffer,
            pkPts: _meta[2],
            pkDts: _meta[3],
        };
    }

    seek(position) {
        if (typeof position !== 'number' || !(position >= 0)) {
            return _fail('seek', 'position must be a non-negative number of milliseconds');
        }
        if (!this.#native || !op_video_decoder_seek(this.#id, position)) {
            return _fail('seek', 'the decoder is not decoding');
        }
        const self = this;
        return new Promise(function (resolve, reject) {
            self.#seeking.push({ resolve: resolve, reject: reject });
        });
    }

    stop() {
        if (!this.#native || !op_video_decoder_stop(this.#id)) return Promise.resolve({ errMsg: 'stop:ok' });
        const self = this;
        return new Promise(function (resolve, reject) {
            self.#stopping.push({ resolve: resolve, reject: reject });
        });
    }

    remove() {
        if (!this.#removed) {
            this.#removed = true;
            if (this.#native) op_video_decoder_remove(this.#id);
            _decoding.delete(this.#id);
            this.#settleAll('remove', 'reject', { errMsg: 'the decoder is removed' });
            for (let i = 0; i < EVENTS.length; i++) this.#listeners[EVENTS[i]].off();
        }
        return Promise.resolve({ errMsg: 'remove:ok' });
    }

    on(eventName, callback) {
        const group = this.#listeners[eventName];
        if (group !== undefined && typeof callback === 'function') group.on(callback);
    }

    off(eventName, callback) {
        const group = this.#listeners[eventName];
        // Without a callback, every listener for the event goes.
        if (group !== undefined) group.off(callback);
    }

    #settleAll(member, how, value) {
        const waiting = [];
        if (this.#starting !== null) waiting.push(['start', this.#starting]);
        for (let i = 0; i < this.#seeking.length; i++) waiting.push(['seek', this.#seeking[i]]);
        for (let i = 0; i < this.#stopping.length; i++) waiting.push(['stop', this.#stopping[i]]);
        this.#starting = null;
        this.#seeking = [];
        this.#stopping = [];
        for (let i = 0; i < waiting.length; i++) {
            const [name, settle] = waiting[i];
            settle[how](how === 'reject' ? { errMsg: name + ':fail ' + value.errMsg } : value);
        }
    }

    #event(event) {
        switch (event.event) {
            case 'start': {
                const res = { width: event.width, height: event.height };
                if (this.#starting !== null) {
                    this.#starting.resolve(Object.assign({ errMsg: 'start:ok' }, res));
                    this.#starting = null;
                }
                this.#listeners.start.trigger(res);
                break;
            }
            case 'seek': {
                const waiting = this.#seeking;
                this.#seeking = [];
                for (let i = 0; i < waiting.length; i++) waiting[i].resolve({ errMsg: 'seek:ok' });
                this.#listeners.seek.trigger({});
                break;
            }
            case 'stop': {
                _decoding.delete(this.#id);
                const waiting = this.#stopping;
                this.#stopping = [];
                for (let i = 0; i < waiting.length; i++) waiting[i].resolve({ errMsg: 'stop:ok' });
                // A start or seek still waiting will not be answered now.
                this.#settleAll('stop', 'reject', { errMsg: 'the decoder stopped' });
                this.#listeners.stop.trigger({});
                break;
            }
            case 'bufferchange':
                this.#listeners.bufferchange.trigger({});
                break;
            case 'ended':
                this.#listeners.ended.trigger({});
                break;
            case 'error': {
                const reason = typeof event.errMsg === 'string' ? event.errMsg : 'decoding failed';
                if (this.#starting !== null) {
                    this.#starting.reject({ errMsg: 'start:fail ' + reason });
                    this.#starting = null;
                } else if (this.#seeking.length > 0) {
                    const waiting = this.#seeking;
                    this.#seeking = [];
                    for (let i = 0; i < waiting.length; i++) waiting[i].reject({ errMsg: 'seek:fail ' + reason });
                } else {
                    console.error('VideoDecoder: ' + reason);
                }
                break;
            }
        }
    }
}

function createVideoDecoder() {
    return new VideoDecoder();
}

// `[id, {event, ...}]` from the decode thread.
function _internalOnVideoDecoderEvent(id, event) {
    const deliver = _decoding.get(id);
    if (deliver === undefined || event === null || typeof event !== 'object') return;
    deliver(event);
}

export { createVideoDecoder, _internalOnVideoDecoderEvent };

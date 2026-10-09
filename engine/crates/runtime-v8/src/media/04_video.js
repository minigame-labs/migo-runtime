import {
    op_video_create,
    op_video_play,
    op_video_pause,
    op_video_stop,
    op_video_seek,
    op_video_request_fullscreen,
    op_video_exit_fullscreen,
    op_video_set_property,
    op_video_destroy,
} from "ext:core/ops";
import {
    allocateHostCallbackId,
    createListenerGroup,
    wrapAsync,
} from "ext:host_v8_base/02_async.js";

// Video instance registry: videoId -> Video
var _videos = new Map();

/**
 * Video - Provides video playback capability.
 *
 * Created via createVideo(). Each instance has a unique ID
 * used to route native events back to the correct instance.
 */
class Video {
    #id;
    #destroyed = false;

    // Playback state (updated by native events)
    #src = '';
    #duration = 0;
    #currentTime = 0;
    #paused = true;
    #loop = false;
    #muted = false;
    #playbackRate = 1.0;
    #objectFit = 'contain';

    // Multi-listener arrays for event callbacks
    #listeners = {
        play: createListenerGroup('Video play'),
        pause: createListenerGroup('Video pause'),
        ended: createListenerGroup('Video ended'),
        timeupdate: createListenerGroup('Video timeupdate'),
        error: createListenerGroup('Video error'),
        waiting: createListenerGroup('Video waiting'),
        progress: createListenerGroup('Video progress'),
        fullscreenchange: createListenerGroup('Video fullscreenchange'),
    };

    constructor(id, options) {
        this.#id = id;
        if (options.src !== undefined) this.#src = String(options.src);
        if (options.loop !== undefined) this.#loop = !!options.loop;
        if (options.muted !== undefined) this.#muted = !!options.muted;
        if (options.playbackRate !== undefined) this.#playbackRate = Number(options.playbackRate) || 1.0;
        if (options.objectFit !== undefined) this.#objectFit = String(options.objectFit);
    }

    // ==================== Properties ====================

    get src() { return this.#src; }
    set src(val) {
        this.#src = String(val);
        this.#syncProperty('src', this.#src);
    }

    get duration() { return this.#duration; }
    get currentTime() { return this.#currentTime; }
    get paused() { return this.#paused; }

    get loop() { return this.#loop; }
    set loop(val) {
        this.#loop = !!val;
        this.#syncProperty('loop', this.#loop);
    }

    get muted() { return this.#muted; }
    set muted(val) {
        this.#muted = !!val;
        this.#syncProperty('muted', this.#muted);
    }

    get playbackRate() { return this.#playbackRate; }
    set playbackRate(val) {
        this.#playbackRate = Number(val) || 1.0;
        this.#syncProperty('playbackRate', this.#playbackRate);
    }

    get objectFit() { return this.#objectFit; }
    set objectFit(val) {
        this.#objectFit = String(val);
        this.#syncProperty('objectFit', this.#objectFit);
    }

    // ==================== Playback Control ====================

    /**
     * Start or resume playback.
     * @param {Object} [options] - success/fail/complete callbacks
     * @returns {Promise}
     */
    play(options) {
        var self = this;
        return wrapAsync('video.play', function () {
            op_video_play(self.#id);
        }, options);
    }

    /**
     * Pause playback.
     * @param {Object} [options] - success/fail/complete callbacks
     * @returns {Promise}
     */
    pause(options) {
        var self = this;
        return wrapAsync('video.pause', function () {
            op_video_pause(self.#id);
        }, options);
    }

    /**
     * Stop playback and reset to beginning.
     * @param {Object} [options] - success/fail/complete callbacks
     * @returns {Promise}
     */
    stop(options) {
        var self = this;
        return wrapAsync('video.stop', function () {
            op_video_stop(self.#id);
        }, options);
    }

    /**
     * Seek to a given position.
     * @param {number} time - Position in seconds
     * @param {Object} [options] - success/fail/complete callbacks
     * @returns {Promise}
     */
    seek(time, options) {
        var self = this;
        var pos = Number(time) || 0;
        return wrapAsync('video.seek', function () {
            op_video_seek(self.#id, pos);
        }, options);
    }

    /**
     * Enter fullscreen mode.
     * @param {Object} [options]
     * @param {number} [options.direction=0] - 0: normal, 90: rotate 90, -90: rotate -90
     * @returns {Promise}
     */
    requestFullScreen(options) {
        var self = this;
        var opts = options || {};
        var direction = (opts.direction !== undefined) ? Number(opts.direction) : 0;
        return wrapAsync('video.requestFullScreen', function () {
            op_video_request_fullscreen(self.#id, direction);
        }, opts);
    }

    /**
     * Exit fullscreen mode.
     * @param {Object} [options] - success/fail/complete callbacks
     * @returns {Promise}
     */
    exitFullScreen(options) {
        var self = this;
        return wrapAsync('video.exitFullScreen', function () {
            op_video_exit_fullscreen(self.#id);
        }, options);
    }

    // ==================== Event Listeners ====================

    onPlay(callback) {
        this.#listeners.play.on(callback);
    }
    offPlay(callback) {
        this.#removeListener('play', callback);
    }

    onPause(callback) {
        this.#listeners.pause.on(callback);
    }
    offPause(callback) {
        this.#removeListener('pause', callback);
    }

    onEnded(callback) {
        this.#listeners.ended.on(callback);
    }
    offEnded(callback) {
        this.#removeListener('ended', callback);
    }

    onTimeUpdate(callback) {
        this.#listeners.timeupdate.on(callback);
    }
    offTimeUpdate(callback) {
        this.#removeListener('timeupdate', callback);
    }

    onError(callback) {
        this.#listeners.error.on(callback);
    }
    offError(callback) {
        this.#removeListener('error', callback);
    }

    onWaiting(callback) {
        this.#listeners.waiting.on(callback);
    }
    offWaiting(callback) {
        this.#removeListener('waiting', callback);
    }

    onProgress(callback) {
        this.#listeners.progress.on(callback);
    }
    offProgress(callback) {
        this.#removeListener('progress', callback);
    }

    onFullScreenChange(callback) {
        this.#listeners.fullscreenchange.on(callback);
    }
    offFullScreenChange(callback) {
        this.#removeListener('fullscreenchange', callback);
    }

    // ==================== Lifecycle ====================

    /**
     * Destroy the video player and release all resources.
     */
    destroy() {
        if (this.#destroyed) return;
        this.#destroyed = true;
        _videos.delete(this.#id);

        try {
            op_video_destroy(this.#id);
        } catch (e) {
            // Ignore errors during cleanup
        }

        // Clear all listeners
        for (var key of Object.keys(this.#listeners)) {
            this.#listeners[key].off();
        }
    }

    // ==================== Internal ====================

    /** Remove a listener for the given event type. */
    #removeListener(type, callback) {
        var group = this.#listeners[type];
        if (!group) return;
        group.off(callback);
    }

    /** Fire all listeners for the given event type. */
    #fireListeners(type, arg) {
        var group = this.#listeners[type];
        if (!group) return;
        group.trigger(arg);
    }

    /** Sync a single property to native. */
    #syncProperty(name, value) {
        if (this.#destroyed) return;
        try {
            var obj = {};
            obj[name] = value;
            op_video_set_property(this.#id, JSON.stringify(obj));
        } catch (e) {
            // Silently ignore sync errors
        }
    }

    /**
     * Internal: handle event from native layer.
     * @param {string} eventType - Event type string
     * @param {string} dataJson - JSON payload
     */
    _handleEvent(eventType, dataJson) {
        var data = {};
        if (dataJson) {
            try { data = JSON.parse(dataJson); } catch (e) { /* empty */ }
        }

        switch (eventType) {
            case 'play':
                this.#paused = false;
                this.#fireListeners('play', data);
                break;
            case 'pause':
                this.#paused = true;
                this.#fireListeners('pause', data);
                break;
            case 'ended':
                this.#paused = true;
                this.#fireListeners('ended', data);
                break;
            case 'timeupdate':
                if (data.currentTime !== undefined) {
                    this.#currentTime = Number(data.currentTime);
                }
                if (data.duration !== undefined) {
                    this.#duration = Number(data.duration);
                }
                this.#fireListeners('timeupdate', {
                    position: this.#currentTime,
                    duration: this.#duration,
                });
                break;
            case 'waiting':
                this.#fireListeners('waiting', data);
                break;
            case 'progress':
                if (data.buffered !== undefined) {
                    this.#fireListeners('progress', { buffered: Number(data.buffered) });
                } else {
                    this.#fireListeners('progress', data);
                }
                break;
            case 'error':
                this.#fireListeners('error', data);
                break;
            case 'fullscreenchange':
                this.#fireListeners('fullscreenchange', data);
                break;
            default:
                break;
        }
    }
}

/**
 * Create a Video instance.
 *
 * @param {Object} [options={}]
 * @param {string} [options.src] - Video source URL
 * @param {number} [options.x=0] - Left position
 * @param {number} [options.y=0] - Top position
 * @param {number} [options.width=300] - Width
 * @param {number} [options.height=150] - Height
 * @param {boolean} [options.autoplay=false] - Auto-play on create
 * @param {boolean} [options.loop=false] - Loop playback
 * @param {boolean} [options.muted=false] - Muted
 * @param {number} [options.playbackRate=1.0] - Playback rate
 * @param {string} [options.objectFit='contain'] - Object fit mode
 * @param {string} [options.poster] - Poster image URL
 * @param {boolean} [options.controls=true] - Show native controls
 * @param {boolean} [options.showCenterPlayBtn=true] - Show center play button
 * @param {boolean} [options.enableProgressGesture=true] - Enable progress gesture
 * @returns {Video}
 */
function createVideo(options) {
    var opts = options || {};
    // From the Host's id space, not a module counter: the platform keeps its
    // players in a map that outlives this isolate, so a counter restarting at 1
    // hands the replacement runtime an id the retired one still owns -- and the
    // events of the video that is still playing would arrive at the new object.
    var videoId = allocateHostCallbackId();
    var createJson = JSON.stringify({
        videoId: videoId,
        src: opts.src !== undefined ? String(opts.src) : '',
        x: opts.x !== undefined ? Number(opts.x) : 0,
        y: opts.y !== undefined ? Number(opts.y) : 0,
        width: opts.width !== undefined ? Number(opts.width) : 300,
        height: opts.height !== undefined ? Number(opts.height) : 150,
        autoplay: !!opts.autoplay,
        loop: !!opts.loop,
        muted: !!opts.muted,
        playbackRate: (opts.playbackRate !== undefined) ? Number(opts.playbackRate) : 1.0,
        objectFit: opts.objectFit !== undefined ? String(opts.objectFit) : 'contain',
        poster: opts.poster !== undefined ? String(opts.poster) : '',
        controls: (opts.controls !== undefined) ? !!opts.controls : true,
        showCenterPlayBtn: (opts.showCenterPlayBtn !== undefined) ? !!opts.showCenterPlayBtn : true,
        enableProgressGesture: (opts.enableProgressGesture !== undefined) ? !!opts.enableProgressGesture : true,
    });

    try {
        op_video_create(createJson);
    } catch (e) {
        console.error('createVideo failed:', e);
    }

    var video = new Video(videoId, opts);
    _videos.set(videoId, video);
    return video;
}

/**
 * Internal: called from JsBindings to dispatch video events to the correct
 * Video instance.
 *
 * @param {number} videoId - Video instance ID
 * @param {string} eventType - Event type string
 * @param {string} dataJson - JSON-encoded event data
 */
function _internalTriggerVideoEvent(videoId, eventType, dataJson) {
    var video = _videos.get(videoId);
    if (video) {
        video._handleEvent(eventType, dataJson);
    }
}

export {
    Video,
    createVideo,
    _internalTriggerVideoEvent,
};

// Global scope for the main window (CORE APIs only).
//
// OPTIONAL API groups (sensors, media, connectivity, commerce, system-utils)
// register their own globals via per-extension 99_global_scope.js files.
// When a feature gate is disabled the extension is excluded from the chain
// and its globals simply do not appear on globalThis.

import * as lifecycle from "ext:host_v8_lifecycle/02_restart_exit.js";
import * as alert from "ext:host_v8_console/01_alert.js";
import * as url from "ext:host_v8_url/03_url.js";
import * as subpackage from 'ext:host_v8_base/04_subpackage.js';
import * as gcApi from 'ext:host_v8_base/03_gc.js';
import * as performance from "ext:host_v8_web/12_performance.js";
import * as raf from "ext:host_v8_webgl/03_raf.js";
import * as fontApi from "ext:host_v8_webgl/04_font.js";
import * as canvas from "ext:host_v8_web/03_canvas.js";
import * as webgl from 'ext:host_v8_webgl/02_webgl_context.js';
import * as context2d from 'ext:host_v8_webgl/02_2d_context.js';
import * as touch from 'ext:host_v8_touch/01_touch.js';
import * as keyboard from 'ext:host_v8_touch/02_keyboard.js';
import * as envApi from 'ext:host_v8_env/00_env.js';
import * as appLifecycle from 'ext:host_v8_lifecycle/01_lifecycle.js';
import * as storageApi from 'ext:host_v8_storage/01_storage.js';
import * as tcpSocket from 'ext:host_v8_network/08_tcp_socket.js';
import * as udpSocket from 'ext:host_v8_network/09_udp_socket.js';
import * as mouseApi from 'ext:host_v8_touch/03_mouse.js';
import * as gamepadApi from 'ext:host_v8_touch/04_gamepad.js';
import * as compositionApi from 'ext:host_v8_touch/05_composition.js';
import * as timersInternal from 'ext:host_v8_web/02_timers.js';
import * as imageApi from 'ext:host_v8_image/01_image.js';

import { core } from "ext:core/mod.js";

const WindowGlobalScope = {
    alert: core.propWritable(alert.alert),
    URL: core.propNonEnumerable(url.URL),
    createCanvas: core.propNonEnumerable(canvas.createCanvas),
    // Host hook: 99_main.js relocates `_internal*` onto the Symbol-keyed host
    // bridge, from where js_bindings resolves it to fire webglcontextlost/
    // restored on the main canvas after a context-loss rebuild.
    _internalTriggerWebglContextEvent: core.propNonEnumerable(canvas.dispatchWebglContextEvent),
    // Host hook: the surface changed. In the core scope because the main canvas
    // must follow the surface in every product profile. The window-geometry half
    // of a resize lives in the `system` extension, which `api-connectivity`
    // gates out, and a Slim build that never adopted its surface size kept
    // drawing at the size the window had before the rotation.
    _internalTriggerWindowResize: core.propNonEnumerable(canvas.handleSurfaceResized),
    requestAnimationFrame: core.propWritable(raf.requestAnimationFrame),
    cancelAnimationFrame: core.propWritable(raf.cancelAnimationFrame),
    setPreferredFramesPerSecond: core.propWritable(raf.setPreferredFramesPerSecond),


    // Image prefetch -- ahead-of-time decode + upload, bypasses the
    // later `new Image()` -> `src =` round trip for assets the game
    // knows it's about to draw (splash screens, scene transitions).
    // Returns a Promise<Array<{path, success, width, height, error}>>.
    prefetchImage: core.propNonEnumerable(imageApi.prefetchImage),

    // Subpackage
    loadSubpackage: core.propNonEnumerable(subpackage.loadSubpackage),
    preDownloadSubpackage: core.propNonEnumerable(subpackage.preDownloadSubpackage),
    _internalOnSubpackageProgress: core.propNonEnumerable(subpackage._internalOnSubpackageProgress),
    _internalOnSubpackageResult: core.propNonEnumerable(subpackage._internalOnSubpackageResult),

    WebGLRenderingContext: core.propNonEnumerable(webgl.WebGLRenderingContext),
    WebGL2RenderingContext: core.propNonEnumerable(webgl.WebGL2RenderingContext),
    // Beside the two WebGL context classes, and for the same reason: content
    // writes `ctx instanceof CanvasRenderingContext2D` as readily as it writes
    // the WebGL one, and a browser has all three. It is also what lets the
    // Canvas2D parity fixture build a context without a render thread, the way
    // the WebGL fixture already does.
    CanvasRenderingContext2D: core.propNonEnumerable(context2d.CanvasRenderingContext2D),
    CanvasGradient: core.propNonEnumerable(context2d.CanvasGradient),

    performance: core.propNonEnumerable(performance.performance),
    getPerformance: core.propNonEnumerable(performance.getPerformance),

    // Touch
    onTouchStart: core.propNonEnumerable(touch.onTouchStart),
    onTouchMove: core.propNonEnumerable(touch.onTouchMove),
    onTouchEnd: core.propNonEnumerable(touch.onTouchEnd),
    onTouchCancel: core.propNonEnumerable(touch.onTouchCancel),
    offTouchStart: core.propNonEnumerable(touch.offTouchStart),
    offTouchMove: core.propNonEnumerable(touch.offTouchMove),
    offTouchEnd: core.propNonEnumerable(touch.offTouchEnd),
    offTouchCancel: core.propNonEnumerable(touch.offTouchCancel),
    _internalEnqueueRawTouchEvent: core.propNonEnumerable(touch._internalEnqueueRawTouchEvent),

    // Keyboard (soft keyboard)
    showKeyboard: core.propNonEnumerable(keyboard.showKeyboard),
    hideKeyboard: core.propNonEnumerable(keyboard.hideKeyboard),
    updateKeyboard: core.propNonEnumerable(keyboard.updateKeyboard),
    onKeyboardInput: core.propNonEnumerable(keyboard.onKeyboardInput),
    offKeyboardInput: core.propNonEnumerable(keyboard.offKeyboardInput),
    onKeyboardHeightChange: core.propNonEnumerable(keyboard.onKeyboardHeightChange),
    offKeyboardHeightChange: core.propNonEnumerable(keyboard.offKeyboardHeightChange),
    onKeyboardConfirm: core.propNonEnumerable(keyboard.onKeyboardConfirm),
    offKeyboardConfirm: core.propNonEnumerable(keyboard.offKeyboardConfirm),
    onKeyboardComplete: core.propNonEnumerable(keyboard.onKeyboardComplete),
    offKeyboardComplete: core.propNonEnumerable(keyboard.offKeyboardComplete),
    _internalTriggerKeyboardInput: core.propNonEnumerable(keyboard._internalTriggerKeyboardInput),
    _internalTriggerKeyboardHeightChange: core.propNonEnumerable(keyboard._internalTriggerKeyboardHeightChange),
    _internalTriggerKeyboardConfirm: core.propNonEnumerable(keyboard._internalTriggerKeyboardConfirm),
    _internalTriggerKeyboardComplete: core.propNonEnumerable(keyboard._internalTriggerKeyboardComplete),

    // Keyboard (PC physical keys)
    onKeyDown: core.propNonEnumerable(keyboard.onKeyDown),
    offKeyDown: core.propNonEnumerable(keyboard.offKeyDown),
    onKeyUp: core.propNonEnumerable(keyboard.onKeyUp),
    offKeyUp: core.propNonEnumerable(keyboard.offKeyUp),
    _internalTriggerKeyDown: core.propNonEnumerable(keyboard._internalTriggerKeyDown),
    _internalTriggerKeyUp: core.propNonEnumerable(keyboard._internalTriggerKeyUp),

    // Font
    loadFont: core.propNonEnumerable(fontApi.loadFont),
    getTextLineHeight: core.propNonEnumerable(fontApi.getTextLineHeight),

    // Storage
    setStorage: core.propNonEnumerable(storageApi.setStorage),
    setStorageSync: core.propNonEnumerable(storageApi.setStorageSync),
    getStorage: core.propNonEnumerable(storageApi.getStorage),
    getStorageSync: core.propNonEnumerable(storageApi.getStorageSync),
    removeStorage: core.propNonEnumerable(storageApi.removeStorage),
    removeStorageSync: core.propNonEnumerable(storageApi.removeStorageSync),
    clearStorage: core.propNonEnumerable(storageApi.clearStorage),
    clearStorageSync: core.propNonEnumerable(storageApi.clearStorageSync),
    getStorageInfo: core.propNonEnumerable(storageApi.getStorageInfo),
    getStorageInfoSync: core.propNonEnumerable(storageApi.getStorageInfoSync),
    createBufferURL: core.propNonEnumerable(storageApi.createBufferURL),
    revokeBufferURL: core.propNonEnumerable(storageApi.revokeBufferURL),

    // Env
    env: core.propNonEnumerable(envApi.env),

    // App Lifecycle (show/hide)
    onShow: core.propNonEnumerable(appLifecycle.onShow),
    onHide: core.propNonEnumerable(appLifecycle.onHide),
    onAppShow: core.propNonEnumerable(appLifecycle.onShow),
    onAppHide: core.propNonEnumerable(appLifecycle.onHide),
    offShow: core.propNonEnumerable(appLifecycle.offShow),
    offHide: core.propNonEnumerable(appLifecycle.offHide),
    offAppShow: core.propNonEnumerable(appLifecycle.offShow),
    offAppHide: core.propNonEnumerable(appLifecycle.offHide),
    getLaunchOptionsSync: core.propNonEnumerable(appLifecycle.getLaunchOptionsSync),
    getEnterOptionsSync: core.propNonEnumerable(appLifecycle.getEnterOptionsSync),
    _internalTriggerOnShow: core.propNonEnumerable(function (option) {
        timersInternal._internalSetTimerBackgrounded(false);
        appLifecycle._internalTriggerOnShow(option);
    }),
    _internalTriggerOnHide: core.propNonEnumerable(function () {
        appLifecycle._internalTriggerOnHide();
        timersInternal._internalSetTimerBackgrounded(true);
    }),
    _internalTriggerFocusChanged: core.propNonEnumerable(
        appLifecycle._internalTriggerFocusChanged,
    ),
    _internalInstallFocusAdapter: core.propNonEnumerable(
        appLifecycle._internalInstallFocusAdapter,
    ),
    _internalGetFocusState: core.propNonEnumerable(
        appLifecycle._internalGetFocusState,
    ),
    onAddToFavorites: core.propNonEnumerable(appLifecycle.onAddToFavorites),
    offAddToFavorites: core.propNonEnumerable(appLifecycle.offAddToFavorites),
    _internalTriggerAddToFavorites: core.propNonEnumerable(appLifecycle._internalTriggerAddToFavorites),

    // App Lifecycle (restart/exit)
    restartMiniProgram: core.propNonEnumerable(lifecycle.restartMiniProgram),
    restartMiniProgramSync: core.propNonEnumerable(lifecycle.restartMiniProgramSync),
    exitMiniProgram: core.propNonEnumerable(lifecycle.exitMiniProgram),
    exitApplication: core.propNonEnumerable(lifecycle.exitApplication),
    saveAppToDesktop: core.propNonEnumerable(lifecycle.saveAppToDesktop),

    // TCP Socket
    createTCPSocket: core.propNonEnumerable(tcpSocket.createTCPSocket),

    // UDP Socket
    createUDPSocket: core.propNonEnumerable(udpSocket.createUDPSocket),

    // GC / Memory
    triggerGC: core.propNonEnumerable(gcApi.triggerGC),
    getHeapStatistics: core.propNonEnumerable(gcApi.getHeapStatistics),

    // Mouse/Wheel events (PC-only)
    onMouseDown: core.propNonEnumerable(mouseApi.onMouseDown),
    offMouseDown: core.propNonEnumerable(mouseApi.offMouseDown),
    onMouseMove: core.propNonEnumerable(mouseApi.onMouseMove),
    offMouseMove: core.propNonEnumerable(mouseApi.offMouseMove),
    onMouseUp: core.propNonEnumerable(mouseApi.onMouseUp),
    offMouseUp: core.propNonEnumerable(mouseApi.offMouseUp),
    onWheel: core.propNonEnumerable(mouseApi.onWheel),
    offWheel: core.propNonEnumerable(mouseApi.offWheel),
    _internalTriggerMouseDown: core.propNonEnumerable(mouseApi._internalTriggerMouseDown),
    _internalTriggerMouseMove: core.propNonEnumerable(mouseApi._internalTriggerMouseMove),
    _internalTriggerMouseUp: core.propNonEnumerable(mouseApi._internalTriggerMouseUp),
    _internalTriggerWheel: core.propNonEnumerable(mouseApi._internalTriggerWheel),

    // Gamepad transport for the HTML5 content adapter. Mainstream mini-game platforms have no gamepad API,
    // so 97_migo_namespace.js exposes these names on `migo` only; the adapter
    // maps them to navigator.getGamepads() and Window connection events.
    getGamepads: core.propNonEnumerable(gamepadApi.getGamepads),
    onGamepadConnected: core.propNonEnumerable(gamepadApi.onGamepadConnected),
    offGamepadConnected: core.propNonEnumerable(gamepadApi.offGamepadConnected),
    onGamepadDisconnected: core.propNonEnumerable(gamepadApi.onGamepadDisconnected),
    offGamepadDisconnected: core.propNonEnumerable(gamepadApi.offGamepadDisconnected),
    _internalTriggerGamepadConnected: core.propNonEnumerable(
        gamepadApi._internalTriggerGamepadConnected,
    ),
    _internalTriggerGamepadDisconnected: core.propNonEnumerable(
        gamepadApi._internalTriggerGamepadDisconnected,
    ),
    _internalTriggerGamepadState: core.propNonEnumerable(gamepadApi._internalTriggerGamepadState),

    // IME composition. The in-progress half of text input, which the soft
    // keyboard above does not report -- see input/05_composition.js.
    onCompositionStart: core.propNonEnumerable(compositionApi.onCompositionStart),
    offCompositionStart: core.propNonEnumerable(compositionApi.offCompositionStart),
    onCompositionUpdate: core.propNonEnumerable(compositionApi.onCompositionUpdate),
    offCompositionUpdate: core.propNonEnumerable(compositionApi.offCompositionUpdate),
    onCompositionEnd: core.propNonEnumerable(compositionApi.onCompositionEnd),
    offCompositionEnd: core.propNonEnumerable(compositionApi.offCompositionEnd),
    _internalTriggerCompositionStart: core.propNonEnumerable(
        compositionApi._internalTriggerCompositionStart,
    ),
    _internalTriggerCompositionUpdate: core.propNonEnumerable(
        compositionApi._internalTriggerCompositionUpdate,
    ),
    _internalTriggerCompositionEnd: core.propNonEnumerable(
        compositionApi._internalTriggerCompositionEnd,
    ),
};

export { WindowGlobalScope };

// Ad APIs -- host-authoritative bridge with a no-reward fallback.
//
// Every ad object is a thin handle over an ad that the *host* owns. Commands
// go out through `op_ad_*`; events come back on the inbound host bridge as
// `_internalOnAdEvent` and are routed here by `adId`.
//
// REWARD INTEGRITY (the reason this file is shaped the way it is):
//
//   `isEnded` on a rewarded-video `close` event is what content uses to decide
//   whether to grant a reward. It must come from the host's ad SDK. This file
//   therefore never writes a truthy `isEnded` literal: the hosted path forwards
//   `event.isEnded === true` from native, and the no-host path reports
//   `isEnded: false` because no advert was shown and no reward is owed.
//
//   Guarded by `scripts/test-ad-reward-integrity-contract.sh`.
//
// This module is baked into the V8 startup snapshot, so it must stay ASCII and
// must not call ops at module scope (there is no HostOpState at snapshot time).

import { allocateHostCallbackId, createListenerGroup } from "ext:host_v8_base/02_async.js";
import { primordials } from "ext:core/mod.js";
import {
  op_ad_is_supported,
  op_ad_create,
  op_ad_load,
  op_ad_show,
  op_ad_hide,
  op_ad_update_style,
  op_ad_destroy,
} from "ext:core/ops";

const {
  JSONParse,
  Number,
  NumberIsFinite,
  SafeMap,
} = primordials;

const FALLBACK_LOAD_DELAY_MS = 100;
const FALLBACK_INTERSTITIAL_DURATION_MS = 100;
const FALLBACK_REWARDED_VIDEO_DURATION_MS = 500;

// Layout fields that a positioned ad forwards to the host when content mutates
// them (mini-game content does `banner.style.top = y` directly on the style object).
const POSITION_KEYS = ["left", "top", "width", "height"];

// ==================== Host availability ====================

// Asked once per ad object rather than cached at module scope.
//
// A module-level cache would be sound only after proving that no isolate ever
// outlives the services it was asked about -- isolates are pooled and prewarmed
// here, so that is a real question rather than an obvious no. The op is a borrow
// and an Option check, and ad objects are created a handful of times per
// session, so paying it per construction costs nothing measurable and removes
// the question entirely.
//
// Called from the AdBase constructor, never at module scope: embedded modules
// are evaluated while the V8 startup snapshot is built, where there is no
// HostOpState to ask.
function _hostAdsAvailable() {
  try {
    return op_ad_is_supported() === true;
  } catch (_) {
    return false;
  }
}

// ==================== Ad handle registry ====================

// adId -> private event ingress closure. Strong references on purpose, and not
// a WeakRef registry:
// the host holds native resources for every live ad and may deliver an event at
// any moment, so an ad must stay routable until content destroys it. Letting GC
// reclaim one early would drop the host's events on the floor -- including a
// reward verdict -- with no way for either side to notice. Entries are released
// in `_markDestroyed`, which is also where the host is told to let go.
const _adRegistry = new SafeMap();
const _adCapabilities = new SafeMap();

// From the Host's id space rather than a module counter. The host keeps native
// resources per ad in a registry that outlives this isolate, so a counter
// restarting at 1 would let a replacement runtime's ad receive the retired
// one's events -- including a reward verdict, which is the one event here that
// must never reach the wrong ad. Exhaustion throws out of the `AdBase`
// constructor, which is what `migo.createRewardedVideoAd` and its siblings
// already do for a refused creation: they return an ad or they do not return.
function _allocAdId() {
  return allocateHostCallbackId();
}

function _parseEventJson(json) {
  if (typeof json !== "string" || json.length === 0) return null;
  try {
    const parsed = JSONParse(json);
    return parsed && typeof parsed === "object" ? parsed : null;
  } catch (_) {
    return null;
  }
}

// Inbound host bridge hook: the single entry point for every ad event.
//
// Payload shape: {"adId":<number>,"event":"<name>", ...event fields}
function _internalOnAdEvent(eventJson) {
  const event = _parseEventJson(eventJson);
  if (!event) return;

  const adId = Number(event.adId);
  if (!NumberIsFinite(adId)) return;

  const dispatch = _adRegistry.get(adId);
  if (!dispatch) return;

  const type = typeof event.event === "string" ? event.event : "";
  if (!type) return;

  dispatch(type, event);
}

// ==================== Direct ad status ====================

const _directAdStatus = {
  isInMask: false,
  isInDirectGameAd: false,
};

const _directAdStatusListeners = createListenerGroup("onDirectAdStatusChange");

function _notifyDirectAdStatus(status) {
  _directAdStatusListeners.trigger(status);
}

function onDirectAdStatusChange(listener) {
  if (typeof listener !== "function") return;
  _directAdStatusListeners.on(listener);
  queueMicrotask(() => {
    try {
      listener(getDirectAdStatusSync());
    } catch (e) {
      console.error("onDirectAdStatusChange listener error:", e);
    }
  });
}

function offDirectAdStatusChange(listener) {
  _directAdStatusListeners.off(listener);
}

function _internalTriggerDirectAdStatusChange(status) {
  if (!status || typeof status !== "object") return;
  if (status.isInMask !== undefined) {
    _directAdStatus.isInMask = !!status.isInMask;
  }
  if (status.isInDirectGameAd !== undefined) {
    _directAdStatus.isInDirectGameAd = !!status.isInDirectGameAd;
  }
  _notifyDirectAdStatus(getDirectAdStatusSync());
}

// ==================== AdBase ====================

function _fireAdEvent(ad, type, arg) {
  const capability = _adCapabilities.get(ad);
  if (capability) capability.fire(type, arg);
}

function _setAdHostEventObserver(ad, observer) {
  const capability = _adCapabilities.get(ad);
  if (capability) capability.observe = observer;
}

// The content API each ad type is created by, which names its failures.
const _AD_CREATE_API = {
  banner: "createBannerAd",
  custom: "createCustomAd",
  grid: "createGridAd",
  interstitial: "createInterstitialAd",
  rewardedVideo: "createRewardedVideoAd",
  gameBanner: "createGameBanner",
  gameIcon: "createGameIcon",
  gamePortal: "createGamePortal",
};

class AdBase {
  #listeners = {};
  #destroyed = false;
  #adId = 0;
  #hosted = false;
  #createApi = "";
  // Promises content holds on this ad's load() and show(), settled by the
  // host's events for it: an advert has loaded when the host says `load`, is on
  // screen when it says `show` -- or `close`, since an advert that closed was
  // shown -- and failed when it says `error`. Resolving when the command left
  // told content an advert was ready, or showing, before any SDK had said so.
  #awaiting = { load: [], show: [] };

  // `adType` and `adUnitId` are what the host needs to resolve a real ad slot;
  // `options` carries the remaining platform-style creation fields verbatim.
  constructor(eventTypes, adType, adUnitId, options) {
    for (const type of eventTypes) {
      this.#listeners[type] = createListenerGroup(`Ad ${type}`);
    }
    this.#adId = _allocAdId();
    this.#hosted = _hostAdsAvailable();
    this.#createApi = _AD_CREATE_API[adType] || adType;
    const capability = {
      fire: (type, arg) => this.#fire(type, arg),
      observe: null,
    };
    _adCapabilities.set(this, capability);
    _adRegistry.set(this.#adId, (type, event) => {
      this.#handleHostEvent(type, event);
    });

    if (this.#hosted) {
      this._command(op_ad_create, {
        adType,
        adUnitId,
        options: options || {},
      });
    }
  }

  get _adId() {
    return this.#adId;
  }

  get _hosted() {
    return this.#hosted;
  }

  _on(type, listener) {
    if (this.#destroyed) return;
    const group = this.#listeners[type];
    if (!group) return;
    group.on(listener);
  }

  _off(type, listener) {
    const group = this.#listeners[type];
    if (!group) return;
    group.off(listener);
  }

  #fire(type, arg) {
    const group = this.#listeners[type];
    if (!group) return;
    group.trigger(arg);
  }

  _isDestroyed() {
    return this.#destroyed;
  }

  _markDestroyed() {
    this.#rejectAwaiting({ errMsg: this.#createApi + ":fail already destroyed" });
    this.#destroyed = true;
    _adRegistry.delete(this.#adId);
    _adCapabilities.delete(this);
    for (const type in this.#listeners) {
      this.#listeners[type].off();
    }
  }

  // Send one command to the host. Host-side failures surface as an `error`
  // event rather than a thrown exception, matching the common mini-game platform's semantics (content
  // listens on onError; it does not try/catch show()).
  _command(op, extra) {
    if (!this.#hosted || this.#destroyed) return;
    const request = { adId: this.#adId };
    if (extra) {
      for (const key in extra) {
        request[key] = extra[key];
      }
    }
    try {
      op(JSON.stringify(request));
    } catch (error) {
      const message = error && error.message ? error.message : String(error);
      queueMicrotask(() => {
        if (!this.#destroyed) {
          const res = { errCode: -1, errMsg: message };
          this.#fire("error", res);
          this.#rejectAwaiting(res);
        }
      });
    }
  }

  // Ask the host to load or show this advert, and answer with a Promise the
  // host's events settle (see #awaiting). Registered before the command is
  // sent, so an answer can never arrive ahead of the waiter.
  _hostRequest(op, kind) {
    const settled = new Promise((resolve, reject) => {
      this.#awaiting[kind].push({ resolve, reject });
    });
    this._command(op);
    return settled;
  }

  #resolveAwaiting(kind) {
    const waiting = this.#awaiting[kind];
    this.#awaiting[kind] = [];
    for (const waiter of waiting) waiter.resolve();
  }

  #rejectAwaiting(res) {
    const waiting = this.#awaiting.load.concat(this.#awaiting.show);
    this.#awaiting = { load: [], show: [] };
    for (const waiter of waiting) waiter.reject(res);
  }

  // Make layout writes on a style object reach the host.
  //
  // Mini-game content mutates the style object directly (`banner.style.top = y`), so
  // the write has to be intercepted. Accessors are installed in place, once per
  // ad, over just the positional keys.
  //
  // Deliberately not a Proxy. Content also *reads* `.style` from its own layout
  // code, and V8 cannot inline-cache a property access through a Proxy -- every
  // read would take the generic path, which is exactly the megamorphic dispatch
  // this engine has measured as its dominant cost elsewhere. A plain accessor
  // gets a monomorphic IC and optimises down to roughly a field load, while a
  // Proxy would also add one allocation and an extra object in the chain.
  //
  // Untracked fields (`realWidth`, `fixed`, ...) stay plain data properties:
  // they are not layout inputs, so a write to them owes the host nothing.
  _trackStyle(raw, keys) {
    if (!this.#hosted) return raw;
    const ad = this;
    for (const key of keys) {
      if (!(key in raw)) continue;
      let current = raw[key];
      Object.defineProperty(raw, key, {
        configurable: true,
        // Enumerable so the object still serialises and iterates the way a
        // plain style object does -- content does `JSON.stringify(style)`.
        enumerable: true,
        get() {
          return current;
        },
        set(next) {
          current = next;
          const style = {};
          style[key] = next;
          ad._command(op_ad_update_style, { style });
        },
      });
    }
    return raw;
  }

  // Route one host event onto this ad's listener groups. Payloads are rebuilt
  // field by field rather than forwarded wholesale so that content can never
  // observe transport-level fields, and so that every value crossing into
  // content has a known type.
  #handleHostEvent(type, event) {
    if (this.#destroyed) return;
    const observer = _adCapabilities.get(this)?.observe;
    if (observer) observer(type, event);
    switch (type) {
      case "load":
        this.#fire("load", this._loadPayload(event));
        this.#resolveAwaiting("load");
        break;
      case "show":
        this.#resolveAwaiting("show");
        break;
      case "error": {
        const res = {
          errCode: NumberIsFinite(Number(event.errCode))
            ? Number(event.errCode)
            : -1,
          errMsg: typeof event.errMsg === "string" ? event.errMsg : "ad error",
        };
        this.#fire("error", res);
        this.#rejectAwaiting(res);
        break;
      }
      case "close":
        this.#fire("close", this._closePayload(event));
        this.#resolveAwaiting("show");
        break;
      case "resize":
        this.#fire("resize", {
          width: Number(event.width) || 0,
          height: Number(event.height) || 0,
        });
        break;
      case "hide":
        this.#fire("hide");
        break;
      default:
        break;
    }
  }

  // Overridden by ad types that carry data on `load`.
  _loadPayload(_event) {
    return undefined;
  }

  // Overridden by ad types that carry data on `close`.
  _closePayload(_event) {
    return undefined;
  }

  // No-host fallback: report a successful load so content's UI flow proceeds.
  // Showing is what stays inert -- see each type's show().
  _scheduleFallbackLoad() {
    if (this.#hosted) return;
    setTimeout(() => {
      if (!this.#destroyed) {
        this.#fire("load", this._loadPayload({}));
      }
    }, FALLBACK_LOAD_DELAY_MS);
  }
}

// ==================== BannerAd ====================

class BannerAd extends AdBase {
  #style;
  #adIntervals;
  #refreshTimer = null;
  #visible = false;

  constructor({ adUnitId, adIntervals, style }) {
    super(["load", "error", "resize"], "banner", adUnitId, {
      adIntervals,
      style,
    });
    this.#adIntervals = adIntervals;
    const ratio = 0.35;
    const w = Math.max(300, style.width || 300);
    const h = Math.round(w * ratio);
    this.#style = this._trackStyle({
      left: style.left || 0,
      top: style.top || 0,
      width: w,
      height: style.height || h,
      realWidth: w,
      realHeight: h,
    }, POSITION_KEYS);
    this._scheduleFallbackLoad();
  }

  get style() {
    return this.#style;
  }

  show() {
    if (this._isDestroyed()) {
      return Promise.reject({ errMsg: "createBannerAd:fail already destroyed" });
    }
    this.#visible = true;
    if (this._hosted) {
      return this._hostRequest(op_ad_show, "show");
    }
    _fireAdEvent(this, "resize", {
      width: this.#style.realWidth,
      height: this.#style.realHeight,
    });
    if (this.#adIntervals && this.#adIntervals >= 30 && !this.#refreshTimer) {
      this.#refreshTimer = setInterval(() => {
        if (!this._isDestroyed() && this.#visible) {
          this._scheduleFallbackLoad();
        }
      }, this.#adIntervals * 1000);
    }
    return Promise.resolve();
  }

  hide() {
    this.#visible = false;
    this._command(op_ad_hide);
  }

  destroy() {
    if (this.#refreshTimer) {
      clearInterval(this.#refreshTimer);
      this.#refreshTimer = null;
    }
    this.#visible = false;
    this._command(op_ad_destroy);
    this._markDestroyed();
  }

  onResize(listener) { this._on("resize", listener); }
  offResize(listener) { this._off("resize", listener); }
  onLoad(listener) { this._on("load", listener); }
  offLoad(listener) { this._off("load", listener); }
  onError(listener) { this._on("error", listener); }
  offError(listener) { this._off("error", listener); }
}

// ==================== CustomAd ====================

class CustomAd extends AdBase {
  #style;
  #adIntervals;
  #refreshTimer = null;
  #visible = false;

  constructor({ adUnitId, adIntervals, style }) {
    super(["load", "error", "close", "hide", "resize"], "custom", adUnitId, {
      adIntervals,
      style,
    });
    this.#adIntervals = adIntervals;
    this.#style = this._trackStyle({
      left: style.left || 0,
      top: style.top || 0,
      fixed: style.fixed || false,
    }, ["left", "top"]);
    _setAdHostEventObserver(this, (type) => {
      if (type === "hide") this.#visible = false;
    });
    this._scheduleFallbackLoad();
  }

  get style() {
    return this.#style;
  }

  show() {
    if (this._isDestroyed()) {
      return Promise.reject({ errMsg: "createCustomAd:fail already destroyed" });
    }
    this.#visible = true;
    if (this._hosted) {
      return this._hostRequest(op_ad_show, "show");
    }
    if (this.#adIntervals && this.#adIntervals >= 30 && !this.#refreshTimer) {
      this.#refreshTimer = setInterval(() => {
        if (!this._isDestroyed() && this.#visible) {
          this._scheduleFallbackLoad();
        }
      }, this.#adIntervals * 1000);
    }
    return Promise.resolve();
  }

  hide() {
    if (this._isDestroyed()) {
      return Promise.reject({ errMsg: "createCustomAd:fail already destroyed" });
    }
    if (this.#visible) {
      this.#visible = false;
      if (this._hosted) {
        this._command(op_ad_hide);
      } else {
        _fireAdEvent(this, "hide");
      }
    }
    return Promise.resolve();
  }

  isShow() {
    return this.#visible;
  }

  destroy() {
    if (this.#refreshTimer) {
      clearInterval(this.#refreshTimer);
      this.#refreshTimer = null;
    }
    this.#visible = false;
    this._command(op_ad_destroy);
    this._markDestroyed();
  }

  onClose(listener) { this._on("close", listener); }
  offClose(listener) { this._off("close", listener); }
  onHide(listener) { this._on("hide", listener); }
  offHide(listener) { this._off("hide", listener); }
  onLoad(listener) { this._on("load", listener); }
  offLoad(listener) { this._off("load", listener); }
  onResize(listener) { this._on("resize", listener); }
  offResize(listener) { this._off("resize", listener); }
  onError(listener) { this._on("error", listener); }
  offError(listener) { this._off("error", listener); }
}

// ==================== GridAd ====================

class GridAd extends AdBase {
  #style;
  #adIntervals;
  #adTheme;
  #gridCount;
  #refreshTimer = null;
  #visible = false;

  constructor({ adUnitId, adIntervals, style, adTheme, gridCount }) {
    super(["load", "error", "resize"], "grid", adUnitId, {
      adIntervals,
      style,
      adTheme,
      gridCount,
    });
    this.#adIntervals = adIntervals;
    this.#adTheme = adTheme || "white";
    this.#gridCount = gridCount || 5;
    const w = Math.max(300, style.width || 300);
    const h = style.height || w;
    this.#style = this._trackStyle({
      left: style.left || 0,
      top: style.top || 0,
      width: w,
      height: h,
      realWidth: w,
      realHeight: h,
    }, POSITION_KEYS);
    this._scheduleFallbackLoad();
  }

  get style() {
    return this.#style;
  }

  get adTheme() {
    return this.#adTheme;
  }

  get gridCount() {
    return this.#gridCount;
  }

  show() {
    if (this._isDestroyed()) {
      return Promise.reject({ errMsg: "createGridAd:fail already destroyed" });
    }
    this.#visible = true;
    if (this._hosted) {
      return this._hostRequest(op_ad_show, "show");
    }
    _fireAdEvent(this, "resize", {
      width: this.#style.realWidth,
      height: this.#style.realHeight,
    });
    if (this.#adIntervals && this.#adIntervals >= 30 && !this.#refreshTimer) {
      this.#refreshTimer = setInterval(() => {
        if (!this._isDestroyed() && this.#visible) {
          this._scheduleFallbackLoad();
        }
      }, this.#adIntervals * 1000);
    }
    return Promise.resolve();
  }

  hide() {
    this.#visible = false;
    this._command(op_ad_hide);
  }

  destroy() {
    if (this.#refreshTimer) {
      clearInterval(this.#refreshTimer);
      this.#refreshTimer = null;
    }
    this.#visible = false;
    this._command(op_ad_destroy);
    this._markDestroyed();
  }

  onResize(listener) { this._on("resize", listener); }
  offResize(listener) { this._off("resize", listener); }
  onLoad(listener) { this._on("load", listener); }
  offLoad(listener) { this._off("load", listener); }
  onError(listener) { this._on("error", listener); }
  offError(listener) { this._off("error", listener); }
}

// ==================== InterstitialAd ====================

class InterstitialAd extends AdBase {
  constructor({ adUnitId }) {
    super(["load", "error", "close"], "interstitial", adUnitId, {});
    this._scheduleFallbackLoad();
  }

  load() {
    if (this._isDestroyed()) {
      return Promise.reject({ errMsg: "createInterstitialAd:fail already destroyed" });
    }
    if (this._hosted) {
      return this._hostRequest(op_ad_load, "load");
    }
    return new Promise((resolve) => {
      setTimeout(() => {
        if (!this._isDestroyed()) {
          _fireAdEvent(this, "load");
        }
        resolve();
      }, FALLBACK_LOAD_DELAY_MS);
    });
  }

  show() {
    if (this._isDestroyed()) {
      return Promise.reject({ errMsg: "createInterstitialAd:fail already destroyed" });
    }
    if (this._hosted) {
      return this._hostRequest(op_ad_show, "show");
    }
    // No host: nothing is displayed. Close immediately so content's flow
    // continues instead of stalling on a modal that never appears.
    setTimeout(() => {
      if (!this._isDestroyed()) {
        _fireAdEvent(this, "close");
      }
    }, FALLBACK_INTERSTITIAL_DURATION_MS);
    return Promise.resolve();
  }

  destroy() {
    this._command(op_ad_destroy);
    this._markDestroyed();
  }

  onLoad(listener) { this._on("load", listener); }
  offLoad(listener) { this._off("load", listener); }
  onError(listener) { this._on("error", listener); }
  offError(listener) { this._off("error", listener); }
  onClose(listener) { this._on("close", listener); }
  offClose(listener) { this._off("close", listener); }
}

// ==================== RewardedVideoAd ====================

const _rewardedVideoSingletons = new SafeMap();

class RewardedVideoAd extends AdBase {
  #adUnitId;

  constructor({ adUnitId, multiton }) {
    super(["load", "error", "close"], "rewardedVideo", adUnitId, { multiton });
    this.#adUnitId = adUnitId;
    this._scheduleFallbackLoad();
  }

  // The reward verdict. `event.isEnded` is produced by the host's ad SDK and
  // is the only source of a truthy value here; anything else collapses to
  // false. Do not "simplify" this to a pass-through of event.isEnded -- the
  // strict comparison is what stops a non-boolean from being forwarded.
  _closePayload(event) {
    return { isEnded: event.isEnded === true };
  }

  _loadPayload(event) {
    return { useFallbackSharePage: event.useFallbackSharePage === true };
  }

  load() {
    if (this._isDestroyed()) {
      return Promise.reject({ errMsg: "createRewardedVideoAd:fail already destroyed" });
    }
    if (this._hosted) {
      return this._hostRequest(op_ad_load, "load");
    }
    return new Promise((resolve) => {
      setTimeout(() => {
        if (!this._isDestroyed()) {
          _fireAdEvent(this, "load", this._loadPayload({}));
        }
        resolve();
      }, FALLBACK_LOAD_DELAY_MS);
    });
  }

  show() {
    if (this._isDestroyed()) {
      return Promise.reject({ errMsg: "createRewardedVideoAd:fail already destroyed" });
    }
    if (this._hosted) {
      return this._hostRequest(op_ad_show, "show");
    }
    // No host ad service: no advert was played, so no reward is owed. The
    // close event still fires so content is not left waiting forever, but it
    // reports an unfinished view. This is the whole point -- the runtime
    // cannot mint rewards.
    setTimeout(() => {
      if (!this._isDestroyed()) {
        _fireAdEvent(this, "close", this._closePayload({}));
      }
    }, FALLBACK_REWARDED_VIDEO_DURATION_MS);
    return Promise.resolve();
  }

  destroy() {
    _rewardedVideoSingletons.delete(this.#adUnitId);
    this._command(op_ad_destroy);
    this._markDestroyed();
  }

  onLoad(listener) { this._on("load", listener); }
  offLoad(listener) { this._off("load", listener); }
  onError(listener) { this._on("error", listener); }
  offError(listener) { this._off("error", listener); }
  onClose(listener) { this._on("close", listener); }
  offClose(listener) { this._off("close", listener); }
}

// ==================== Factory Functions ====================

function createBannerAd(obj) {
  if (!obj || !obj.adUnitId) {
    throw { errMsg: "createBannerAd:fail missing adUnitId" };
  }
  if (!obj.style) {
    throw { errMsg: "createBannerAd:fail missing style" };
  }
  return new BannerAd(obj);
}

function createCustomAd(obj) {
  if (!obj || !obj.adUnitId) {
    throw { errMsg: "createCustomAd:fail missing adUnitId" };
  }
  if (!obj.style) {
    throw { errMsg: "createCustomAd:fail missing style" };
  }
  return new CustomAd(obj);
}

function createGridAd(obj) {
  if (!obj || !obj.adUnitId) {
    throw { errMsg: "createGridAd:fail missing adUnitId" };
  }
  if (!obj.style) {
    throw { errMsg: "createGridAd:fail missing style" };
  }
  return new GridAd(obj);
}

function createInterstitialAd(obj) {
  if (!obj || !obj.adUnitId) {
    throw { errMsg: "createInterstitialAd:fail missing adUnitId" };
  }
  return new InterstitialAd(obj);
}

function createRewardedVideoAd(obj) {
  if (!obj || !obj.adUnitId) {
    throw { errMsg: "createRewardedVideoAd:fail missing adUnitId" };
  }
  // Singleton by adUnitId unless multiton is enabled
  if (!obj.multiton) {
    const existing = _rewardedVideoSingletons.get(obj.adUnitId);
    if (existing && !existing._isDestroyed()) {
      return existing;
    }
  }
  const ad = new RewardedVideoAd(obj);
  if (!obj.multiton) {
    _rewardedVideoSingletons.set(obj.adUnitId, ad);
  }
  return ad;
}

function getDirectAdStatusSync() {
  return {
    isInMask: _directAdStatus.isInMask,
    isInDirectGameAd: _directAdStatus.isInDirectGameAd,
  };
}

function getShowSplashAdStatus(obj = {}) {
  const res = {
    status: "success",
    code: 1,
    errMsg: "getShowSplashAdStatus:ok",
  };
  queueMicrotask(() => {
    try {
      if (typeof obj.success === "function") {
        obj.success(res);
      }
    } catch (e) {
      console.error("getShowSplashAdStatus success callback error:", e);
    }
    try {
      if (typeof obj.complete === "function") {
        obj.complete(res);
      }
    } catch (e) {
      console.error("getShowSplashAdStatus complete callback error:", e);
    }
  });
}

// ==================== GameBanner (createGameBanner) ====================
// Same shape as BannerAd but uses the "game recommend" semantics.

class GameBanner extends AdBase {
  #style;
  #visible = false;

  constructor({ adUnitId, style }) {
    super(["load", "error", "resize"], "gameBanner", adUnitId, { style });
    const s = style || {};
    const w = Math.max(300, s.width || 300);
    const h = s.height || Math.round(w * 0.35);
    this.#style = this._trackStyle({
      left: s.left || 0,
      top: s.top || 0,
      width: w,
      height: h,
      realWidth: w,
      realHeight: h,
    }, POSITION_KEYS);
    this._scheduleFallbackLoad();
  }

  get style() { return this.#style; }

  show() {
    if (this._isDestroyed()) return Promise.reject({ errMsg: "createGameBanner:fail already destroyed" });
    this.#visible = true;
    if (this._hosted) {
      return this._hostRequest(op_ad_show, "show");
    }
    _fireAdEvent(this, "resize", { width: this.#style.realWidth, height: this.#style.realHeight });
    return Promise.resolve();
  }

  hide() {
    this.#visible = false;
    this._command(op_ad_hide);
  }

  destroy() {
    this.#visible = false;
    this._command(op_ad_destroy);
    this._markDestroyed();
  }

  onResize(listener) { this._on("resize", listener); }
  offResize(listener) { this._off("resize", listener); }
  onLoad(listener) { this._on("load", listener); }
  offLoad(listener) { this._off("load", listener); }
  onError(listener) { this._on("error", listener); }
  offError(listener) { this._off("error", listener); }
}

function createGameBanner(obj) {
  if (!obj || !obj.adUnitId) {
    throw { errMsg: "createGameBanner:fail missing adUnitId" };
  }
  return new GameBanner(obj);
}

// ==================== GameIcon (createGameIcon) ====================

class GameIcon extends AdBase {
  #style;
  #count;
  #visible = false;

  constructor({ adUnitId, count, style }) {
    super(["load", "error", "resize"], "gameIcon", adUnitId, { count, style });
    this.#count = count || 1;
    const s = style || {};
    this.#style = this._trackStyle({
      left: s.left || 0,
      top: s.top || 0,
      width: s.width || 40,
      height: s.height || 40,
    }, POSITION_KEYS);
    this._scheduleFallbackLoad();
  }

  get style() { return this.#style; }
  get count() { return this.#count; }

  show() {
    if (this._isDestroyed()) return Promise.reject({ errMsg: "createGameIcon:fail already destroyed" });
    this.#visible = true;
    if (this._hosted) {
      return this._hostRequest(op_ad_show, "show");
    }
    _fireAdEvent(this, "resize", { width: this.#style.width, height: this.#style.height });
    return Promise.resolve();
  }

  hide() {
    this.#visible = false;
    this._command(op_ad_hide);
  }

  destroy() {
    this.#visible = false;
    this._command(op_ad_destroy);
    this._markDestroyed();
  }

  onResize(listener) { this._on("resize", listener); }
  offResize(listener) { this._off("resize", listener); }
  onLoad(listener) { this._on("load", listener); }
  offLoad(listener) { this._off("load", listener); }
  onError(listener) { this._on("error", listener); }
  offError(listener) { this._off("error", listener); }
}

function createGameIcon(obj) {
  if (!obj || !obj.adUnitId) {
    throw { errMsg: "createGameIcon:fail missing adUnitId" };
  }
  return new GameIcon(obj);
}

// ==================== GamePortal (createGamePortal) ====================
// Similar to InterstitialAd: load -> show -> close.

class GamePortal extends AdBase {
  constructor({ adUnitId }) {
    super(["load", "error", "close"], "gamePortal", adUnitId, {});
    this._scheduleFallbackLoad();
  }

  load() {
    if (this._isDestroyed()) return Promise.reject({ errMsg: "createGamePortal:fail already destroyed" });
    if (this._hosted) {
      return this._hostRequest(op_ad_load, "load");
    }
    return new Promise((resolve) => {
      setTimeout(() => {
        if (!this._isDestroyed()) {
          _fireAdEvent(this, "load");
        }
        resolve();
      }, FALLBACK_LOAD_DELAY_MS);
    });
  }

  show() {
    if (this._isDestroyed()) return Promise.reject({ errMsg: "createGamePortal:fail already destroyed" });
    if (this._hosted) {
      return this._hostRequest(op_ad_show, "show");
    }
    setTimeout(() => {
      if (!this._isDestroyed()) {
        _fireAdEvent(this, "close");
      }
    }, FALLBACK_INTERSTITIAL_DURATION_MS);
    return Promise.resolve();
  }

  destroy() {
    this._command(op_ad_destroy);
    this._markDestroyed();
  }

  onLoad(listener) { this._on("load", listener); }
  offLoad(listener) { this._off("load", listener); }
  onError(listener) { this._on("error", listener); }
  offError(listener) { this._off("error", listener); }
  onClose(listener) { this._on("close", listener); }
  offClose(listener) { this._off("close", listener); }
}

function createGamePortal(obj) {
  if (!obj || !obj.adUnitId) {
    throw { errMsg: "createGamePortal:fail missing adUnitId" };
  }
  return new GamePortal(obj);
}

export {
  createBannerAd,
  createCustomAd,
  createGridAd,
  createInterstitialAd,
  createRewardedVideoAd,
  getDirectAdStatusSync,
  onDirectAdStatusChange,
  offDirectAdStatusChange,
  _internalTriggerDirectAdStatusChange,
  _internalOnAdEvent,
  getShowSplashAdStatus,
  createGameBanner,
  createGameIcon,
  createGamePortal,
};

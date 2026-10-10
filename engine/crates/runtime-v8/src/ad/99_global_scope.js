// Global scope registration for host_v8_ad APIs (api-system feature gate).

import * as adApi from 'ext:host_v8_ad/01_ad.js';

import { primordials, core } from "ext:core/mod.js";
const { ObjectDefineProperties } = primordials;

ObjectDefineProperties(globalThis, {
    // Ad
    createAdSkipCard: core.propNonEnumerable(adApi.createAdSkipCard),
    createBannerAd: core.propNonEnumerable(adApi.createBannerAd),
    createCustomAd: core.propNonEnumerable(adApi.createCustomAd),
    createGridAd: core.propNonEnumerable(adApi.createGridAd),
    createInterstitialAd: core.propNonEnumerable(adApi.createInterstitialAd),
    createRewardedVideoAd: core.propNonEnumerable(adApi.createRewardedVideoAd),
    getDirectAdStatusSync: core.propNonEnumerable(adApi.getDirectAdStatusSync),
    onDirectAdStatusChange: core.propNonEnumerable(adApi.onDirectAdStatusChange),
    offDirectAdStatusChange: core.propNonEnumerable(adApi.offDirectAdStatusChange),
    _internalTriggerDirectAdStatusChange: core.propNonEnumerable(adApi._internalTriggerDirectAdStatusChange),
    // Inbound host bridge: the single channel every ad event arrives on.
    // 99_main.js relocates `_internal*` onto the Symbol-keyed host-bridge
    // holder, so this is not reachable from content.
    _internalOnAdEvent: core.propNonEnumerable(adApi._internalOnAdEvent),
    getShowSplashAdStatus: core.propNonEnumerable(adApi.getShowSplashAdStatus),
    createGameBanner: core.propNonEnumerable(adApi.createGameBanner),
    createGameIcon: core.propNonEnumerable(adApi.createGameIcon),
    createGamePortal: core.propNonEnumerable(adApi.createGamePortal),
});

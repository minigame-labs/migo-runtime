// Global scope registration for host_v8_share APIs (api-commerce feature gate).

import * as shareApi from 'ext:host_v8_share/01_share.js';

import { primordials, core } from "ext:core/mod.js";
const { ObjectDefineProperties } = primordials;

ObjectDefineProperties(globalThis, {
    // Share
    showShareMenu: core.propNonEnumerable(shareApi.showShareMenu),
    hideShareMenu: core.propNonEnumerable(shareApi.hideShareMenu),
    updateShareMenu: core.propNonEnumerable(shareApi.updateShareMenu),
    onShareAppMessage: core.propNonEnumerable(shareApi.onShareAppMessage),
    offShareAppMessage: core.propNonEnumerable(shareApi.offShareAppMessage),
    shareAppMessage: core.propNonEnumerable(shareApi.shareAppMessage),
    _internalOnShareAppMessageResult: core.propNonEnumerable(shareApi._internalOnShareAppMessageResult),
    onShareTimeline: core.propNonEnumerable(shareApi.onShareTimeline),
    offShareTimeline: core.propNonEnumerable(shareApi.offShareTimeline),
    onAddToFavorites: core.propNonEnumerable(shareApi.onAddToFavorites),
    offAddToFavorites: core.propNonEnumerable(shareApi.offAddToFavorites),
    _internalOnShareMenuEvent: core.propNonEnumerable(shareApi._internalOnShareMenuEvent),
    shareMessageToFriend: core.propNonEnumerable(shareApi.shareMessageToFriend),
    _internalOnShareMessageToFriendResult: core.propNonEnumerable(shareApi._internalOnShareMessageToFriendResult),
    onShareMessageToFriend: core.propNonEnumerable(shareApi.onShareMessageToFriend),
    offShareMessageToFriend: core.propNonEnumerable(shareApi.offShareMessageToFriend),
    setMessageToFriendQuery: core.propNonEnumerable(shareApi.setMessageToFriendQuery),
    showShareImageMenu: core.propNonEnumerable(shareApi.showShareImageMenu),
    _internalOnShowShareImageMenuResult: core.propNonEnumerable(shareApi._internalOnShowShareImageMenuResult),
});

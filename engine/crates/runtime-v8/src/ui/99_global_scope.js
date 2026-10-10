// Global scope registration for host_v8_ui APIs (api-system feature gate).

import * as interaction from 'ext:host_v8_ui/01_interaction.js';
import * as buttonsApi from 'ext:host_v8_ui/02_buttons.js';
import * as pageManagerApi from 'ext:host_v8_ui/03_page_manager.js';
import * as windowApi from 'ext:host_v8_ui/04_window.js';

import { primordials, core } from "ext:core/mod.js";
const { ObjectDefineProperties } = primordials;

ObjectDefineProperties(globalThis, {
    // UI Interaction
    showToast: core.propNonEnumerable(interaction.showToast),
    hideToast: core.propNonEnumerable(interaction.hideToast),
    showModal: core.propNonEnumerable(interaction.showModal),
    _internalOnModalResult: core.propNonEnumerable(interaction._internalOnModalResult),
    showLoading: core.propNonEnumerable(interaction.showLoading),
    hideLoading: core.propNonEnumerable(interaction.hideLoading),
    showActionSheet: core.propNonEnumerable(interaction.showActionSheet),
    _internalOnActionSheetResult: core.propNonEnumerable(interaction._internalOnActionSheetResult),

    // UI Buttons
    createUserInfoButton: core.propNonEnumerable(buttonsApi.createUserInfoButton),
    createGameClubButton: core.propNonEnumerable(buttonsApi.createGameClubButton),
    createFeedbackButton: core.propNonEnumerable(buttonsApi.createFeedbackButton),
    getMenuButtonBoundingClientRect: core.propNonEnumerable(buttonsApi.getMenuButtonBoundingClientRect),

    // Page Manager
    createPageManager: core.propNonEnumerable(pageManagerApi.createPageManager),

    // Desktop window: size, cursor, pointer lock
    setWindowSize: core.propNonEnumerable(windowApi.setWindowSize),
    _internalOnSetWindowSizeResult: core.propNonEnumerable(windowApi._internalOnSetWindowSizeResult),
    onWindowStateChange: core.propNonEnumerable(windowApi.onWindowStateChange),
    offWindowStateChange: core.propNonEnumerable(windowApi.offWindowStateChange),
    _internalOnWindowStateEvent: core.propNonEnumerable(windowApi._internalOnWindowStateEvent),
    setCursor: core.propNonEnumerable(windowApi.setCursor),
    requestPointerLock: core.propNonEnumerable(windowApi.requestPointerLock),
    exitPointerLock: core.propNonEnumerable(windowApi.exitPointerLock),
    isPointerLocked: core.propNonEnumerable(windowApi.isPointerLocked),
    _internalOnPointerLockEvent: core.propNonEnumerable(windowApi._internalOnPointerLockEvent),
});

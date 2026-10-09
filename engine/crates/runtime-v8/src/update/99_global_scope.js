// Global scope registration for host_v8_update APIs (api-system feature gate).

import * as updateMgr from 'ext:host_v8_update/02_update_mgr.js';

import { primordials, core } from "ext:core/mod.js";
const { ObjectDefineProperties } = primordials;

ObjectDefineProperties(globalThis, {
    // Update
    getUpdateManager: core.propNonEnumerable(updateMgr.getUpdateManager),
    checkUpdate: core.propNonEnumerable(updateMgr.checkUpdate),
});

import { op_save_file_to_disk } from "ext:core/ops";
import { createDeferredApi, isCallbackStyle } from "ext:host_v8_base/02_async.js";
import { getFileSystemManager } from "./02_file_manager.js";

// saveFileToDisk: a file of content's, saved wherever the player chooses on the
// computer's disk -- the host's save dialog. It waits on the player, so it has
// no timeout; a player who cancels fails it.
const _saveFileToDiskApi = createDeferredApi('saveFileToDisk', 0);

// Callbacks only: the platform does not offer this one as a promise.
function saveFileToDisk(options) {
    const settled = _saveFileToDiskApi.invoke(options, function (opts, requestId) {
        if (typeof opts.filePath !== 'string' || opts.filePath.length === 0) {
            throw new Error('filePath is required');
        }
        return op_save_file_to_disk(JSON.stringify({ requestId: requestId, filePath: opts.filePath }));
    });
    if (!isCallbackStyle(options) && settled !== undefined) settled.then(undefined, function () {});
}

function _internalOnSaveFileToDiskResult(resultJson) {
    _saveFileToDiskApi.settle(resultJson);
}

function saveFile(options = {}) {
    return getFileSystemManager().saveFile(options);
}

function saveFileSync(options = {}) {
    return getFileSystemManager().saveFileSync(options);
}

export { saveFileToDisk, _internalOnSaveFileToDiskResult, getFileSystemManager, saveFile, saveFileSync };

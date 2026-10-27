import { op_save_file_to_disk } from "ext:core/ops";
import { createDeferredApi } from "ext:host_v8_base/02_async.js";
import { getFileSystemManager } from "./02_file_manager.js";

// saveFileToDisk: a file of content's, saved wherever the player chooses on the
// computer's disk -- the host's save dialog. It waits on the player, so it has
// no timeout; a player who cancels fails it.
const _saveFileToDiskApi = createDeferredApi('saveFileToDisk', 0);

// Answered as every asynchronous call is: by its callbacks, or by the Promise it
// returns when it has none. The platform offers it no Promise style, so that
// Promise's rejection is not reported as unhandled (WITHOUT_PROMISE_STYLE).
function saveFileToDisk(options) {
    return _saveFileToDiskApi.invoke(options, function (opts, requestId) {
        if (typeof opts.filePath !== 'string' || opts.filePath.length === 0) {
            throw new Error('filePath is required');
        }
        return op_save_file_to_disk(JSON.stringify({ requestId: requestId, filePath: opts.filePath }));
    });
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

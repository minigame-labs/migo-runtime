import { op_restart_mini_program, op_exit_mini_program } from "ext:core/ops";
import { promisify } from "ext:host_v8_base/02_async.js";

// The query a restart is launched with: what `path` has after its `?` (the reference: the part of path after "?"
// becomes the query), the empty query for a path without one, or null -- the query the game was last shown with -- when
// there is no path. A game has no pages, so the rest of the path names nothing.
function launchQuery(opts) {
    const path = opts == null ? undefined : opts.path;
    if (path === undefined || path === null) return null;
    const text = String(path);
    const at = text.indexOf("?");
    return at < 0 ? "" : text.slice(at + 1);
}

// Restarts the current mini-program: it is launched and shown again, with the query `path` names.
export const restartMiniProgram = promisify("restartMiniProgram", (opts) => {
    op_restart_mini_program(launchQuery(opts));
});

// Synchronous version of restartMiniProgram (used by some games).
export function restartMiniProgramSync(opts) {
    op_restart_mini_program(launchQuery(opts));
}

// Exits the current mini-program.
export const exitMiniProgram = promisify("exitMiniProgram", (_opts) => {
    op_exit_mini_program();
});

// Alias for exitMiniProgram (used by some games).
export const exitApplication = promisify("exitApplication", (_opts) => {
    op_exit_mini_program();
});


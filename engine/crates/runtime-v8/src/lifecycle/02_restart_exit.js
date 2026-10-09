import { op_restart_mini_program, op_exit_mini_program } from "ext:core/ops";
import { promisify } from "ext:host_v8_base/02_async.js";

// Restarts the current mini-program.
// TODO: support `path` parameter (open a specific page after restart)
export const restartMiniProgram = promisify("restartMiniProgram", (_opts) => {
    op_restart_mini_program();
});

// Synchronous version of restartMiniProgram (used by some games).
export function restartMiniProgramSync() {
    op_restart_mini_program();
}

// Exits the current mini-program.
export const exitMiniProgram = promisify("exitMiniProgram", (_opts) => {
    op_exit_mini_program();
});

// Alias for exitMiniProgram (used by some games).
export const exitApplication = promisify("exitApplication", (_opts) => {
    op_exit_mini_program();
});


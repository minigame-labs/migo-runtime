// Backend implementation of host_v8_web/01_task_context.js. WebKit cannot
// synchronously drain promise jobs from JavaScript. Keep the task's nesting
// through its native checkpoint and clear it when the next owned task starts.
import { core } from "./engine/core/mod.mjs";
import { onTaskStart } from "./task-gate.mjs";

const ReflectApply = Reflect.apply;
let timerDepth = 0;
onTaskStart(() => {
  timerDepth = 0;
  core.setAsyncContext(undefined);
});

export function getTimerDepth() {
  return timerDepth;
}

export function runTimerTask(depth, callback, args, asyncContext) {
  timerDepth = depth;
  core.setAsyncContext(asyncContext);
  ReflectApply(callback, globalThis, args);
}

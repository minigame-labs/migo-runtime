// The task gate: a task that arrives while a synchronous call is outstanding
// runs after the script that made the call, never inside it.
//
// WebKit dispatches a Worker's tasks from a synchronous request's nested run
// loop; node has no such loop, so a test simulates the dispatch the only way it
// happens -- a task source calling `runTask` while `synchronously` is on the
// stack -- and checks what the producer promises around it: when the task runs,
// in what order, and that each held task gets a turn of its own.
//
// Run:  node test/task-gate.test.mjs
// Gate: scripts/test-frame-wire-js-encoder.sh

import assert from "node:assert/strict";

import { runTask, synchronously } from "../src/task-gate.mjs";

/** Resolves once every message and microtask queued so far has run. */
const settle = () => new Promise((resolve) => setTimeout(resolve, 20));

let failures = 0;
async function check(name, body) {
  try {
    await body();
    console.log(`ok - ${name}`);
  } catch (error) {
    failures += 1;
    console.log(`not ok - ${name}\n  ${error && error.stack}`);
  }
}

await check("an idle gate runs the task at once, inside the caller", () => {
  const seen = [];
  runTask(() => seen.push("task"));
  seen.push("after");
  assert.deepEqual(seen, ["task", "after"]);
});

await check("a task arriving during a synchronous call runs after the script that made it", async () => {
  const seen = [];
  synchronously(() => {
    runTask(() => seen.push("held"));
    seen.push("call returns");
  });
  seen.push("rest of the script");
  assert.deepEqual(seen, ["call returns", "rest of the script"]);
  await settle();
  assert.deepEqual(seen, ["call returns", "rest of the script", "held"]);
});

await check("held tasks run in arrival order, each in a turn of its own", async () => {
  const seen = [];
  synchronously(() => {
    runTask(() => {
      seen.push("first");
      // A microtask the first task queues runs before the second task: each
      // held task ends with the checkpoint a task ends with.
      Promise.resolve().then(() => seen.push("first's microtask"));
    });
    runTask(() => seen.push("second"));
  });
  await settle();
  assert.deepEqual(seen, ["first", "first's microtask", "second"]);
});

await check("a task arriving while held tasks drain joins the back of the queue", async () => {
  const seen = [];
  synchronously(() => {
    runTask(() => {
      seen.push("held 1");
      // Arrives after the call, while "held 2" is still waiting: it must not
      // overtake it, or a downlink message could run ahead of an earlier one.
      runTask(() => seen.push("arrived while draining"));
    });
    runTask(() => seen.push("held 2"));
  });
  await settle();
  assert.deepEqual(seen, ["held 1", "held 2", "arrived while draining"]);
});

await check("nested calls hold tasks until the outermost returns", async () => {
  const seen = [];
  synchronously(() => {
    synchronously(() => runTask(() => seen.push("held")));
    seen.push("inner returned");
  });
  seen.push("outer returned");
  await settle();
  assert.deepEqual(seen, ["inner returned", "outer returned", "held"]);
});

await check("a held task's own synchronous call holds what arrives during it", async () => {
  const seen = [];
  synchronously(() => {
    runTask(() => {
      synchronously(() => runTask(() => seen.push("arrived during the second call")));
      seen.push("held task finished");
    });
  });
  await settle();
  assert.deepEqual(seen, ["held task finished", "arrived during the second call"]);
});

await check("a held task that throws does not stall the ones behind it", async () => {
  const seen = [];
  const uncaught = [];
  const onUncaught = (error) => uncaught.push(error.message);
  process.on("uncaughtException", onUncaught);
  try {
    synchronously(() => {
      runTask(() => {
        throw new Error("boom");
      });
      runTask(() => seen.push("after the throw"));
    });
    await settle();
  } finally {
    process.off("uncaughtException", onUncaught);
  }
  assert.deepEqual(uncaught, ["boom"]);
  assert.deepEqual(seen, ["after the throw"]);
});

await check("the call's own result and exception pass through", () => {
  assert.equal(synchronously(() => 42), 42);
  assert.throws(() => synchronously(() => {
    throw new RangeError("from the call");
  }), RangeError);
  // And the depth came back down: an idle gate runs tasks at once again.
  const seen = [];
  runTask(() => seen.push("now"));
  assert.deepEqual(seen, ["now"]);
});

if (failures > 0) {
  console.log(`${failures} failure(s)`);
  process.exit(1);
}
console.log("task gate: all checks passed");

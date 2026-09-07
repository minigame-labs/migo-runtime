// WebGL 2 sync-object conformance probe.
//
// Three things this asserts, all of which were broken before:
//
//   1. The sync-object constants exist. Every constant reachable through
//      `fenceSync` / `deleteSync` / `clientWaitSync` was absent, so the standard
//      async-readback pattern passed `undefined` as the fence condition and
//      compared the poll status against `undefined` for every branch.
//   2. `MAX_CLIENT_WAIT_TIMEOUT_WEBGL` is reported, and is zero. The WebGL 2
//      conformance suite requires it to be non-negative and no more than one
//      second; zero is what browsers report, so content written for the web
//      polls with timeout 0 and works unchanged.
//   3. A timeout above the maximum is `INVALID_OPERATION` and returns
//      `WAIT_FAILED`. Unbounded before, and honoured to its full 64-bit range on
//      the render thread -- a thread shared by every canvas and the frame loop.
//
// Prints one PASS/FAIL line per check on stderr so the runner can grep them, and
// a final verdict line.
const canvas = migo.createCanvas();
const gl = canvas.getContext("webgl2");

let failures = 0;

function check(name, ok, detail) {
  if (ok) {
    console.error(`[webgl-sync] PASS ${name}`);
  } else {
    failures += 1;
    console.error(`[webgl-sync] FAIL ${name}: ${detail}`);
  }
}

function run() {
  if (!gl) {
    check("webgl2 context", false, "getContext('webgl2') returned null");
    verdict();
    return;
  }

  // 1. Constants present. `undefined` is the failure this catches, so the test
  //    is on the type, not on the value being truthy -- SYNC_FLUSH_COMMANDS_BIT
  //    is 1 and MAX_CLIENT_WAIT_TIMEOUT_WEBGL is 0, both of which a truthiness
  //    check would get wrong in opposite directions.
  const names = [
    "SYNC_GPU_COMMANDS_COMPLETE",
    "SYNC_FLUSH_COMMANDS_BIT",
    "ALREADY_SIGNALED",
    "TIMEOUT_EXPIRED",
    "CONDITION_SATISFIED",
    "WAIT_FAILED",
    "MAX_CLIENT_WAIT_TIMEOUT_WEBGL",
  ];
  for (const name of names) {
    check(`constant ${name}`, typeof gl[name] === "number", `is ${typeof gl[name]}`);
  }

  // 2. The reported maximum.
  const max = gl.getParameter(gl.MAX_CLIENT_WAIT_TIMEOUT_WEBGL);
  check(
    "MAX_CLIENT_WAIT_TIMEOUT_WEBGL is a non-negative number",
    typeof max === "number" && max >= 0,
    `got ${max} (${typeof max})`,
  );
  check(
    "MAX_CLIENT_WAIT_TIMEOUT_WEBGL is within the conformance bound",
    max <= 1000000000,
    `got ${max}, suite allows at most 1e9 ns`,
  );

  // A fence to poll. Drawing something first so the fence has work behind it.
  gl.clearColor(0.1, 0.24, 0.36, 1.0);
  gl.clear(gl.COLOR_BUFFER_BIT);
  const sync = gl.fenceSync(gl.SYNC_GPU_COMMANDS_COMPLETE, 0);
  check("fenceSync returns an object", sync !== null && sync !== undefined, `got ${sync}`);
  if (!sync) {
    verdict();
    return;
  }

  // Drain any error the setup left, so the next check reads its own.
  while (gl.getError() !== gl.NO_ERROR) { /* drain */ }

  // 3. Above the maximum: INVALID_OPERATION, and WAIT_FAILED returned.
  const refused = gl.clientWaitSync(sync, 0, max + 1);
  check(
    "a timeout above the maximum returns WAIT_FAILED",
    refused === gl.WAIT_FAILED,
    `got ${refused}, want ${gl.WAIT_FAILED}`,
  );
  check(
    "a timeout above the maximum is INVALID_OPERATION",
    gl.getError() === gl.INVALID_OPERATION,
    "getError did not report INVALID_OPERATION",
  );

  // And a legal poll answers with one of the four statuses and no error.
  while (gl.getError() !== gl.NO_ERROR) { /* drain */ }
  const polled = gl.clientWaitSync(sync, gl.SYNC_FLUSH_COMMANDS_BIT, 0);
  const legal = [
    gl.ALREADY_SIGNALED,
    gl.TIMEOUT_EXPIRED,
    gl.CONDITION_SATISFIED,
    gl.WAIT_FAILED,
  ];
  check(
    "a poll with timeout 0 answers with a sync status",
    legal.indexOf(polled) !== -1,
    `got ${polled}, want one of ${legal.join()}`,
  );
  check(
    "a legal poll raises no error",
    gl.getError() === gl.NO_ERROR,
    "getError reported an error for a conforming poll",
  );

  gl.deleteSync(sync);
  verdict();
}

function verdict() {
  if (failures === 0) {
    console.error("[webgl-sync] VERDICT ok");
  } else {
    console.error(`[webgl-sync] VERDICT ${failures} check(s) failed`);
  }
}

requestAnimationFrame(() => {
  run();
});

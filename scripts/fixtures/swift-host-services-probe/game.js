// Proves an app's MigoHostServiceProvider answers content through the
// host-service channel, end to end through the product surface:
// tests/swift_host/macos-game-view supplies a clipboard of its own, and this
// writes to it, reads it back, and checks that a service the app did not name is
// refused. Any failure is thrown out of a timer, so it reaches the app as an
// error with the step in it; success is migo.exitMiniProgram().

function call(api, options) {
  return new Promise(function (resolve, reject) {
    migo[api](Object.assign({}, options || {}, { success: resolve, fail: reject }));
  });
}

function expect(condition, step, value) {
  if (!condition) throw new Error("migo-swift-host-probe: " + step + " got " + JSON.stringify(value));
}

async function run() {
  const set = await call("setClipboardData", { data: "migo-swift-host" });
  expect(set.errMsg === "setClipboardData:ok", "setClipboardData", set);
  const got = await call("getClipboardData");
  expect(got.data === "migo-swift-host", "getClipboardData", got);
  let refused = null;
  try {
    await call("scanCode");
  } catch (e) {
    refused = e;
  }
  expect(refused !== null && /^scanCode:fail/.test(refused.errMsg), "scanCode refused", refused);
  // The SDK's own interaction service, which the app did not name: a toast is
  // shown and hidden by the system's indicator.
  const toast = await call("showToast", { title: "migo", icon: "none", duration: 60000 });
  expect(toast.errMsg === "showToast:ok", "showToast", toast);
  const hidden = await call("hideToast");
  expect(hidden.errMsg === "hideToast:ok", "hideToast", hidden);
  console.error("migo-swift-host-probe: clipboard=" + got.data);
}

// A few frames first, so the app's frame clock is part of what is proven.
let frames = 0;
function frame() {
  frames += 1;
  if (frames < 3) {
    requestAnimationFrame(frame);
    return;
  }
  run().then(
    function () { migo.exitMiniProgram(); },
    function (error) { setTimeout(function () { throw error; }, 0); }
  );
}
requestAnimationFrame(frame);

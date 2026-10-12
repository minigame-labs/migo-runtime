// Proves an app's MigoHostServiceProvider answers content through the
// host-service channel, end to end through the product surface:
// tests/swift_host/macos-game-view supplies a clipboard of its own, and this
// writes to it, reads it back, and checks that a service the app did not name is
// refused; the SDK's own services and the view's window answer the rest. Any
// failure is thrown out of a timer, so it reaches the app as an error with the
// step in it; success is migo.exitMiniProgram().

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
  // The game view's own window service: the window grows so the game gets the
  // size it asked for, in pixels, and the game hears it as a resize.
  let seen = null;
  const resized = new Promise(function (resolve, reject) {
    const timer = setTimeout(function () {
      reject(new Error("migo-swift-host-probe: resize got " + JSON.stringify(seen)));
    }, 5000);
    migo.onWindowResize(function listener(size) {
      seen = size;
      if (size.windowWidth !== 320 || size.windowHeight !== 200) return;
      clearTimeout(timer);
      migo.offWindowResize(listener);
      resolve(size);
    });
  });
  const ratio = migo.getWindowInfo().pixelRatio;
  const sized = await call("setWindowSize", { width: 320 * ratio, height: 200 * ratio });
  expect(sized.errMsg === "setWindowSize:ok", "setWindowSize", sized);
  await resized;
  expect(migo.setCursor("pointer") === true, "setCursor", false);
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

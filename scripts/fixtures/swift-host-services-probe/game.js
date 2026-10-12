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
  // What the SDK reports of the system: the Wi-Fi switch, which the script
  // compares with the Mac's own, and the notification grant, in the API's words.
  const system = migo.getSystemSetting();
  const grants = migo.getAppAuthorizeSetting();
  expect(typeof system.wifiEnabled === "boolean", "getSystemSetting", system);
  const words = ["authorized", "denied", "not determined"];
  expect(words.indexOf(grants.notificationAuthorized) >= 0, "getAppAuthorizeSetting", grants);
  console.error("migo-swift-host-probe: wifi=" + system.wifiEnabled + " camera=" + grants.cameraAuthorized);
  // The location capability the app linked reports its grant and decides
  // scope.userLocation with the system's prompt -- which this app, declaring no
  // purpose for location, never shows: the scope is refused without asking.
  expect(words.indexOf(grants.locationAuthorized) >= 0, "locationAuthorized", grants);
  expect(typeof system.locationEnabled === "boolean", "locationEnabled", system);
  let located = null;
  try {
    await call("authorize", { scope: "scope.userLocation" });
  } catch (e) {
    located = e;
  }
  expect(located !== null && located.errMsg === "authorize:fail auth deny", "authorize location", located);
  console.error("migo-swift-host-probe: location=" + grants.locationAuthorized + " scope=refused");
  // The SDK's image service compresses with the system's encoder: a 64 x 32
  // PNG of the game's, scaled to 16 wide, handed back as a file of the game's.
  const canvas = migo.createCanvas();
  canvas.width = 64;
  canvas.height = 32;
  const ctx = canvas.getContext("2d");
  ctx.fillStyle = "#0080ff";
  ctx.fillRect(0, 0, 64, 32);
  const png = canvas.toTempFilePathSync({ fileType: "png" });
  const compressed = await call("compressImage", { src: png, compressedWidth: 16 });
  expect(typeof compressed.tempFilePath === "string", "compressImage", compressed);
  const size = await new Promise(function (resolve, reject) {
    const image = migo.createImage();
    image.onload = function () { resolve(image.width + "x" + image.height); };
    image.onerror = function () { reject(new Error("migo-swift-host-probe: the compressed image did not load")); };
    image.src = compressed.tempFilePath;
  });
  expect(size === "16x8", "compressed size", size);
  console.error("migo-swift-host-probe: compressed=" + size);
  // The system's viewer shows the game's image, and the call is answered once
  // it does.
  const previewed = await call("previewImage", { urls: [compressed.tempFilePath] });
  expect(previewed.errMsg === "previewImage:ok", "previewImage", previewed);
  console.error("migo-swift-host-probe: previewed");
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

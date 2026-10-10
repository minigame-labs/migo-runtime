// Proves the host-service channel end to end, through the shipping archive.
//
// Sign-in, payment, sharing, navigation and ads are the host's to supply, and
// on every platform but Android they reach the host through one C ABI channel.
// Unit tests prove each half -- that a request leaves the engine with the right
// numbers, that a completion becomes the right JSON -- and neither proves that
// content calling migo.login() on a real session gets the host's answer back.
// This does: tests/c_host/macos-headless answers every call as a fixed fake
// backend, and each step below checks the exact result content receives.
//
// Every expectation is a line the host's answer determines. A failure throws
// out of the promise chain into an uncaught error, which the host receives
// through on_error with the step named; success is migo.exitMiniProgram().

function expect(condition, step, value) {
  if (!condition) {
    throw new Error("migo-host-services-probe: " + step + " got " + JSON.stringify(value));
  }
}

// The callback form, so the probe sees the success/fail split content sees,
// not only the promise's.
function settle(api, options) {
  return new Promise(function (resolve) {
    const opts = Object.assign({}, options || {});
    opts.success = function (res) { resolve({ ok: true, res: res }); };
    opts.fail = function (res) { resolve({ ok: false, res: res }); };
    migo[api](opts);
  });
}

async function run() {
  // A success's fields reach content as the host wrote them.
  const login = await settle("login", { timeout: 5000 });
  expect(login.ok && login.res.errMsg === "login:ok" && login.res.code === "probe-login-code",
    "login", login);

  // A failure is the API's errMsg around the host's reason, with the code in
  // the field this API defines (errno).
  const check = await settle("checkSession");
  expect(!check.ok && check.res.errMsg === "checkSession:fail session expired" &&
    check.res.errno === 1, "checkSession", check);

  // getUserInfo needs scope.userInfo, which the host granted before content
  // ran: a scope check reads the host's standing decision.
  const info = await settle("getUserInfo");
  expect(info.ok && info.res.userInfo && info.res.userInfo.nickName === "probe",
    "getUserInfo", info);

  // getPhoneNumber's failure had no code from the host, so none is invented.
  const phone = await settle("getPhoneNumber");
  expect(!phone.ok && phone.res.errMsg === "getPhoneNumber:fail no phone bound" &&
    !("errno" in phone.res), "getPhoneNumber", phone);

  // Declaring the payment service is what makes a host one that takes payments.
  const support = await settle("checkIsSupportMidasPayment");
  expect(support.ok && support.res.data && support.res.data.allow_pay === true,
    "checkIsSupportMidasPayment", support);

  const pay = await settle("requestMidasPayment", { mode: "game", offerId: "1", buyQuantity: 10 });
  expect(pay.ok && pay.res.errMsg === "requestMidasPayment:ok", "requestMidasPayment", pay);

  // Payment reports its code as errCode, not errno.
  const item = await settle("requestMidasPaymentGameItem",
    { signData: "s", paySig: "p", signature: "g" });
  expect(!item.ok && item.res.errMsg === "requestMidasPaymentGameItem:fail cancel" &&
    item.res.errCode === 1, "requestMidasPaymentGameItem", item);

  const share = await settle("shareAppMessage", { title: "probe" });
  expect(share.ok && share.res.errMsg === "shareAppMessage:ok", "shareAppMessage", share);

  const nav = await settle("navigateToMiniProgram", { appId: "probe-app" });
  expect(nav.ok && nav.res.errMsg === "navigateToMiniProgram:ok", "navigateToMiniProgram", nav);

  // A command is answered by nothing; its success is the host taking it.
  const back = await settle("navigateBackMiniProgram");
  expect(back.ok, "navigateBackMiniProgram", back);

  // Permission. getSetting reports what the host decided and nothing else: the
  // granted scope is true and a scope nobody has been asked about is absent.
  const before = await settle("getSetting");
  expect(before.ok && before.res.authSetting["scope.userInfo"] === true &&
    !("scope.camera" in before.res.authSetting), "getSetting before authorize", before);

  // authorize asks the host; the host records its decision and settles.
  const camera = await settle("authorize", { scope: "scope.camera" });
  expect(camera.ok && camera.res.errMsg === "authorize:ok", "authorize camera", camera);
  const record = await settle("authorize", { scope: "scope.record" });
  expect(!record.ok && record.res.errMsg === "authorize:fail auth deny",
    "authorize record", record);

  // openSetting answers with the settings as the player left them -- the
  // host's standing decisions, refusal included.
  const opened = await settle("openSetting");
  expect(opened.ok && opened.res.authSetting["scope.camera"] === true &&
    opened.res.authSetting["scope.record"] === false &&
    opened.res.authSetting["scope.userInfo"] === true, "openSetting", opened);

  const bluetoothPage = await settle("openSystemBluetoothSetting");
  expect(bluetoothPage.ok, "openSystemBluetoothSetting", bluetoothPage);
  const appPage = await settle("openAppAuthorizeSetting");
  expect(!appPage.ok && appPage.res.errMsg === "openAppAuthorizeSetting:fail no settings app" &&
    appPage.res.errCode === -1, "openAppAuthorizeSetting", appPage);

  // Standing reports, read synchronously. The orientation is the attached
  // window's shape (256x256 is not wider than tall), not a report.
  const system = migo.getSystemSetting();
  expect(system.bluetoothEnabled === true && system.wifiEnabled === true &&
    system.locationEnabled === false && system.deviceOrientation === "portrait",
    "getSystemSetting", system);
  const app = migo.getAppAuthorizeSetting();
  expect(app.cameraAuthorized === "authorized" && app.albumAuthorized === "denied" &&
    app.microphoneAuthorized === "not determined", "getAppAuthorizeSetting", app);

  // Subpackages, as this package's own game.json declares them: stage1 is not
  // on disk, so loading it asks the host to download it. The host reports
  // half the bytes, then fails -- the progress reaches the task, and the
  // failure is the host's reason under the API content called.
  const progress = [];
  const sub = await new Promise(function (resolve) {
    const task = migo.loadSubpackage({
      name: "stage1",
      success: function (res) { resolve({ ok: true, res: res }); },
      fail: function (res) { resolve({ ok: false, res: res }); },
    });
    task.onProgressUpdate(function (update) { progress.push(update.progress); });
  });
  expect(!sub.ok && sub.res.errMsg === "loadSubpackage:fail offline" && progress[0] === 50,
    "loadSubpackage", { sub: sub, progress: progress });

  // Native UI is the host's: toasts and loading are commands, a modal and an
  // action sheet are answered -- an editable modal with what was typed.
  const toast = await settle("showToast", { title: "probe" });
  expect(toast.ok, "showToast", toast);
  expect((await settle("hideToast")).ok, "hideToast", null);
  const loading = await settle("showLoading", { title: "probe" });
  expect(loading.ok && (await settle("hideLoading")).ok, "showLoading/hideLoading", loading);
  const modal = await settle("showModal", { title: "probe", content: "ok?" });
  expect(modal.ok && modal.res.confirm === true && modal.res.cancel === false &&
    !("content" in modal.res), "showModal", modal);
  const prompt = await settle("showModal", { title: "probe", editable: true, placeholderText: "name" });
  expect(prompt.ok && prompt.res.content === "typed", "showModal editable", prompt);
  const sheet = await settle("showActionSheet", { itemList: ["a", "b"] });
  expect(sheet.ok && sheet.res.tapIndex === 1, "showActionSheet", sheet);

  // The clipboard round-trips through the host, asynchronously.
  const copied = await settle("setClipboardData", { data: "probe-clip" });
  expect(copied.ok && copied.res.errMsg === "setClipboardData:ok", "setClipboardData", copied);
  const pasted = await settle("getClipboardData");
  expect(pasted.ok && pasted.res.data === "probe-clip", "getClipboardData", pasted);

  const scanned = await settle("scanCode", { onlyFromCamera: true });
  expect(scanned.ok && scanned.res.result === "probe-qr" && scanned.res.scanType === "QR_CODE",
    "scanCode", scanned);

  // Location is gated on scope.userLocation, which the host granted.
  const located = await settle("getLocation", { type: "wgs84" });
  expect(located.ok && located.res.latitude === 31.2 && located.res.longitude === 121.5,
    "getLocation", located);

  // Files cross both ways as files. A path content names reaches the host as the
  // real file behind it (the host checks), and only a path content can itself
  // read: one outside the sandbox fails here and never reaches the host. A file
  // a result names arrives as a /tmp path content's own file APIs read.
  const fs = migo.getFileSystemManager();
  const png = new Uint8Array([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 1, 2, 3, 4]);
  fs.writeFileSync("/user/probe.png", png.buffer);
  const bytesOf = function (path) { return new Uint8Array(fs.readFileSync(path)); };
  const isPng = function (path) {
    const bytes = bytesOf(path);
    return bytes.length >= 8 && bytes[0] === 0x89 && bytes[1] === 0x50 && bytes[3] === 0x47;
  };
  const inTmp = function (path) { return typeof path === "string" && path.indexOf("/tmp/") === 0; };

  const saved = await settle("saveImageToPhotosAlbum", { filePath: "/user/probe.png" });
  expect(saved.ok && saved.res.errMsg === "saveImageToPhotosAlbum:ok", "saveImageToPhotosAlbum", saved);
  const escaped = await settle("saveImageToPhotosAlbum", { filePath: "/etc/hosts" });
  expect(!escaped.ok && escaped.res.errMsg.indexOf("saveImageToPhotosAlbum:fail Path not allowed") === 0,
    "saveImageToPhotosAlbum outside the sandbox", escaped);
  const viewed = await settle("previewImage", {
    urls: ["https://example.com/a.png", "/user/probe.png"], current: "/user/probe.png",
  });
  expect(viewed.ok, "previewImage", viewed);
  const played = await settle("previewMedia", { sources: [{ url: "/user/probe.png", type: "image" }] });
  expect(played.ok, "previewMedia", played);

  const compressed = await settle("compressImage", { src: "/user/probe.png", quality: 50 });
  expect(compressed.ok && inTmp(compressed.res.tempFilePath) && isPng(compressed.res.tempFilePath),
    "compressImage", compressed);
  const picked = await settle("chooseImage", { count: 1 });
  expect(picked.ok && inTmp(picked.res.tempFilePaths[0]) &&
    picked.res.tempFiles[0].path === picked.res.tempFilePaths[0] &&
    picked.res.tempFiles[0].size === bytesOf(picked.res.tempFilePaths[0]).length &&
    isPng(picked.res.tempFilePaths[0]), "chooseImage", picked);
  const chosen = await settle("chooseMessageFile", { count: 1 });
  expect(chosen.ok && inTmp(chosen.res.tempFiles[0].path) && chosen.res.tempFiles[0].name === "doc.txt" &&
    fs.readFileSync(chosen.res.tempFiles[0].path, "utf8") === "probe document",
    "chooseMessageFile", chosen);
  const media = await settle("chooseMedia", { mediaType: ["video"] });
  const clip = media.ok ? media.res.tempFiles[0] : null;
  expect(media.ok && media.res.type === "video" && inTmp(clip.tempFilePath) &&
    /\.mp4$/.test(clip.tempFilePath) && inTmp(clip.thumbTempFilePath) && clip.duration === 1.5 &&
    fs.readFileSync(clip.tempFilePath, "utf8") === "probe video" &&
    fs.readFileSync(clip.thumbTempFilePath, "utf8") === "probe cover", "chooseMedia", media);

  // Motion sensors: a start is answered by the host -- the compass it lacks
  // fails -- and readings arrive typed, in g, as onAccelerometerChange.
  const reading = new Promise(function (resolve) { migo.onAccelerometerChange(resolve); });
  const accel = await settle("startAccelerometer", { interval: "game" });
  expect(accel.ok && accel.res.errMsg === "startAccelerometer:ok", "startAccelerometer", accel);
  const flat = await reading;
  expect(flat.x === 0 && flat.y === 0 && flat.z === 1, "onAccelerometerChange", flat);
  expect((await settle("stopAccelerometer")).ok, "stopAccelerometer", null);
  const compass = await settle("startCompass");
  expect(!compass.ok && compass.res.errMsg === "startCompass:fail no such sensor on this device",
    "startCompass without a compass", compass);

  // The screen: brightness, orientation and capture are the host's to answer,
  // and what it observes arrives as events.
  const bright = await settle("getScreenBrightness");
  expect(bright.ok && bright.res.value === 0.5, "getScreenBrightness", bright);
  expect((await settle("setScreenBrightness", { value: 0.25 })).ok, "setScreenBrightness", null);
  const turned = new Promise(function (resolve) { migo.onDeviceOrientationChange(resolve); });
  expect((await settle("setDeviceOrientation", { value: "landscape" })).ok, "setDeviceOrientation", null);
  const orientation = await turned;
  expect(orientation.value === "landscape", "onDeviceOrientationChange", orientation);
  const recording = await settle("getScreenRecordingState");
  expect(recording.ok && recording.res.state === "off", "getScreenRecordingState", recording);
  const recorded = await new Promise(function (resolve) { migo.onScreenRecordingStateChanged(resolve); });
  expect(recorded.state === "on", "onScreenRecordingStateChanged", recorded);
  const captured = await new Promise(function (resolve) { migo.onUserCaptureScreen(resolve); });
  expect(captured !== undefined, "onUserCaptureScreen", captured);
  expect((await settle("setVisualEffectOnCapture", { visualEffect: "hidden" })).ok,
    "setVisualEffectOnCapture", null);

  // Bluetooth: every operation is answered by the host when it has happened,
  // binary data crosses as hex and reaches content as ArrayBuffers, and a
  // failure carries the platform's Bluetooth errCode.
  const bytes = function (buffer) { return Array.prototype.slice.call(new Uint8Array(buffer)); };
  const adapterState = new Promise(function (resolve) { migo.onBluetoothAdapterStateChange(resolve); });
  const adapter = await settle("openBluetoothAdapter");
  expect(adapter.ok && adapter.res.errMsg === "openBluetoothAdapter:ok", "openBluetoothAdapter", adapter);
  const state = await adapterState;
  expect(state.available === true && state.discovering === false, "onBluetoothAdapterStateChange", state);
  const found = new Promise(function (resolve) { migo.onBluetoothDeviceFound(resolve); });
  expect((await settle("startBluetoothDevicesDiscovery")).ok, "startBluetoothDevicesDiscovery", null);
  const device = (await found).devices[0];
  expect(device.deviceId === "AA:BB:CC:DD:EE:FF" && device.advertisData instanceof ArrayBuffer &&
    bytes(device.advertisData).join() === "76,0,1,2" &&
    bytes(device.serviceData["0000180d-0000-1000-8000-00805f9b34fb"]).join() === "10,11",
    "onBluetoothDeviceFound", device);
  const linked = new Promise(function (resolve) { migo.onBLEConnectionStateChange(resolve); });
  expect((await settle("createBLEConnection", { deviceId: device.deviceId })).ok, "createBLEConnection", null);
  expect((await linked).connected === true, "onBLEConnectionStateChange", null);
  const services = await settle("getBLEDeviceServices", { deviceId: device.deviceId });
  expect(services.ok && services.res.services[0].uuid === "0000ffe0-0000-1000-8000-00805f9b34fb",
    "getBLEDeviceServices", services);
  const ble = { deviceId: device.deviceId, serviceId: "0000ffe0-0000-1000-8000-00805f9b34fb",
    characteristicId: "0000ffe1-0000-1000-8000-00805f9b34fb" };
  const notified = new Promise(function (resolve) { migo.onBLECharacteristicValueChange(resolve); });
  expect((await settle("notifyBLECharacteristicValueChange", Object.assign({ state: true }, ble))).ok,
    "notifyBLECharacteristicValueChange", null);
  const change = await notified;
  expect(change.characteristicId === ble.characteristicId && bytes(change.value).join() === "1,2,3",
    "onBLECharacteristicValueChange", change);
  const wrote = await settle("writeBLECharacteristicValue",
    Object.assign({ value: new Uint8Array([0xca, 0xfe]).buffer }, ble));
  expect(wrote.ok, "writeBLECharacteristicValue", wrote);
  const rssi = await settle("getBLEDeviceRSSI", { deviceId: device.deviceId });
  expect(rssi.ok && rssi.res.RSSI === -61, "getBLEDeviceRSSI", rssi);
  const mtu = await settle("getBLEMTU", { deviceId: device.deviceId });
  expect(!mtu.ok && mtu.res.errCode === 10006 && mtu.res.errMsg === "getBLEMTU:fail no connection",
    "getBLEMTU failure", mtu);
  expect((await settle("closeBLEConnection", { deviceId: device.deviceId })).ok, "closeBLEConnection", null);
  expect((await settle("closeBluetoothAdapter")).ok, "closeBluetoothAdapter", null);

  // The desktop window: the cursor is a keyword or content's own file, the size
  // is the host's to set, and the pointer is locked while the host says so.
  expect(migo.setCursor("pointer") === true, "setCursor keyword", null);
  expect(migo.setCursor("/user/probe.png", 1, 2) === true, "setCursor file", null);
  expect(migo.setCursor("/etc/hosts", 0, 0) === false, "setCursor outside the sandbox", null);
  const windowState = new Promise(function (resolve) { migo.onWindowStateChange(resolve); });
  expect((await settle("setWindowSize", { width: 800, height: 600 })).ok, "setWindowSize", null);
  expect((await windowState).state === "normalize", "onWindowStateChange", null);
  const lockedTo = function (locked) {
    return new Promise(function (resolve, reject) {
      const deadline = Date.now() + 2000;
      (function poll() {
        if (migo.isPointerLocked() === locked) return resolve();
        if (Date.now() > deadline) return reject(new Error("pointer lock never became " + locked));
        setTimeout(poll, 5);
      })();
    });
  };
  migo.requestPointerLock();
  await lockedTo(true);
  migo.exitPointerLock();
  await lockedTo(false);
  expect(migo.createPath2D() instanceof Path2D, "createPath2D", null);

  // The host's ecosystem: requests by name, a file through the sandbox, a code
  // the API defines, getters from what the host reported, and an event content
  // answers -- the host confirms the answer with the event after it.
  expect(migo.isChatTool() === true && migo.getExtConfigSync().channel === "probe",
    "ecosystem values", null);
  const replied = new Promise(function (resolve) { migo.onOfficialComponentsInfoChange(resolve); });
  migo.onCopyUrl(function () { return { query: "from=probe" }; });
  const enter = await settle("getGroupEnterInfo");
  expect(enter.ok && enter.res.encryptedData === "probe", "getGroupEnterInfo", enter);
  expect((await replied).replied === true, "onCopyUrl reply", null);
  const groupShare = await settle("shareImageToGroup", { imagePath: "/user/probe.png" });
  expect(groupShare.ok, "shareImageToGroup", groupShare);
  const subscribe = await settle("requestSubscribeMessage", { tmplIds: ["t"] });
  expect(!subscribe.ok && subscribe.res.errCode === 20001 &&
    subscribe.res.errMsg === "requestSubscribeMessage:fail template not found",
    "requestSubscribeMessage failure", subscribe);
  const gameServer = migo.getGameServerManager();
  const firstFrame = new Promise(function (resolve) { gameServer.onSyncFrame(resolve); });
  const room = await gameServer.createRoom({ maxMemberNum: 2 });
  expect(room.errMsg === "createRoom:ok" && room.data.accessInfo === "probe-room", "createRoom", room);
  const frame = await firstFrame;
  expect(frame.frameId === 1 && Array.from(new Uint8Array(frame.actionList[0])).join() === "10,255",
    "onSyncFrame", { frameId: frame.frameId, actions: frame.actionList.length });

  // Sharing names content's image, which the host receives as the real file; a
  // share to one friend carries setMessageToFriendQuery's query, and the game
  // hears how it ended.
  const imageShare = await settle("shareAppMessage", { imageUrl: "/user/probe.png" });
  expect(imageShare.ok, "shareAppMessage with an image", imageShare);
  expect(migo.setMessageToFriendQuery({ query: "room=7", shareMessageToFriendScene: 1 }) === true,
    "setMessageToFriendQuery", null);
  const heard = new Promise(function (resolve) { migo.onShareMessageToFriend(resolve); });
  const friendShare = await settle("shareMessageToFriend",
    { openId: "probe-friend", imageUrl: "/user/probe.png" });
  expect(friendShare.ok, "shareMessageToFriend", friendShare);
  const told = await heard;
  expect(told.success === true && told.errMsg === "shareMessageToFriend:ok", "onShareMessageToFriend", told);
  const imageSheet = await settle("showShareImageMenu", { path: "/user/probe.png" });
  expect(!imageSheet.ok && imageSheet.res.errMsg === "showShareImageMenu:fail cancel", "showShareImageMenu", imageSheet);

  // An advert's Promises are settled by what the host's SDK says: load()
  // when it has loaded, show() when it is on screen -- before it closes.
  // The reward is the host's word too: the close event's isEnded comes from it.
  const ad = migo.createRewardedVideoAd({ adUnitId: "probe-unit" });
  await ad.load();
  const order = [];
  const closed = new Promise(function (resolve) {
    ad.onClose(function (res) { order.push("close"); resolve(res); });
  });
  await ad.show().then(function () { order.push("shown"); });
  const close = await closed;
  expect(close && close.isEnded === true && order.join(",") === "shown,close",
    "rewarded video", { close: close, order: order });

  console.error("migo-host-services-probe: every host service round-tripped");
  migo.exitMiniProgram();
}

run().catch(function (error) {
  // Out of the promise chain, so it arrives as an uncaught error with its text.
  // A file API fails with a plain `{errMsg}` object, which has no text of its
  // own to report: it is carried in an Error that does.
  const reported = error instanceof Error ? error
    : new Error("migo-host-services-probe: threw " + JSON.stringify(error));
  setTimeout(function () { throw reported; }, 0);
});

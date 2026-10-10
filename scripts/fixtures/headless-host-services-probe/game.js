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

  // Not something this host offers, and not something the engine pretends to:
  // the request fails rather than claiming a share nobody made.
  const friend = await settle("shareMessageToFriend", { openId: "probe-friend" });
  expect(!friend.ok && friend.res.errMsg === "shareMessageToFriend:fail not supported",
    "shareMessageToFriend", friend);

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

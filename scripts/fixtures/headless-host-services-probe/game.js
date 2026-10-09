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

  // The reward is the host's word: the close event's isEnded comes from it.
  const ad = migo.createRewardedVideoAd({ adUnitId: "probe-unit" });
  await new Promise(function (resolve) { ad.onLoad(resolve); });
  const closed = new Promise(function (resolve) { ad.onClose(resolve); });
  await ad.show();
  const close = await closed;
  expect(close && close.isEnded === true, "rewarded video close", close);

  console.error("migo-host-services-probe: every host service round-tripped");
  migo.exitMiniProgram();
}

run().catch(function (error) {
  // Out of the promise chain, so it arrives as an uncaught error with its text.
  setTimeout(function () { throw error; }, 0);
});

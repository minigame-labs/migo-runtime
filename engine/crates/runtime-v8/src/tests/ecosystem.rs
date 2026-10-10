//! The host's ecosystem features: what crosses to the host, what comes back, and
//! what content is told when there is no host to ask.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use deno_core::{FastString, JsRuntime, serde_json};
use shared::{
    protocol::error::ServiceError,
    services::{
        CommerceServices, ConnectivityServices, DeviceServices, EcosystemService, MediaServices,
        SensorServices, SystemUtilServices,
    },
    vfs::VirtualFS,
};

use super::support::{SETTLE, Sandbox, boot_with_services, is_true, run_to_idle as run};

#[derive(Default)]
struct FakeEcosystem {
    calls: Mutex<Vec<String>>,
    replies: Mutex<Vec<String>>,
    values: HashMap<&'static str, &'static str>,
}

impl EcosystemService for FakeEcosystem {
    fn call(&self, request_json: &str) -> Result<(), ServiceError> {
        self.calls.lock().unwrap().push(request_json.to_string());
        Ok(())
    }
    fn reply(&self, json: &str) -> Result<(), ServiceError> {
        self.replies.lock().unwrap().push(json.to_string());
        Ok(())
    }
    fn value(&self, name: &str) -> Option<String> {
        self.values.get(name).map(|value| value.to_string())
    }
}

struct Host(Arc<FakeEcosystem>);

impl SensorServices for Host {}
impl MediaServices for Host {}
impl ConnectivityServices for Host {}
impl CommerceServices for Host {}
impl SystemUtilServices for Host {
    fn ecosystem(&self) -> Option<Arc<dyn EcosystemService>> {
        Some(self.0.clone())
    }
}

fn boot(host: Option<Arc<FakeEcosystem>>, vfs: Option<Arc<VirtualFS>>) -> JsRuntime {
    boot_with_services(
        host.map(|host| Arc::new(Host(host)) as Arc<dyn DeviceServices>),
        vfs,
    )
}

#[test]
fn without_a_host_only_true_answers_are_given() {
    let mut runtime = boot(None, None);
    runtime
        .execute_script("<test:ecosystem-setup>", FastString::from_static(SETTLE))
        .unwrap();
    let results = run(
        &mut runtime,
        r#"
        __settle('requestSubscribeMessage', { tmplIds: ['a'] });
        __settle('setUserCloudStorage', { KVDataList: [] });
        __settle('getPrivacySetting');
        __settle('checkIsAddedToMyMiniProgram');
        migo.getUserCryptoManager().getLatestUserKey({
            fail: function (res) { __results.getLatestUserKey = { ok: false, res: res }; },
        });
        "#,
        r#"JSON.stringify([__results, migo.isChatTool(), migo.getExtConfigSync(),
            migo.getExptInfoSync(['color'])])"#,
    );
    let [results, chat_tool, ext, expt]: [serde_json::Value; 4] =
        serde_json::from_str(&results).unwrap();
    assert_eq!(
        results["requestSubscribeMessage"]["res"]["errMsg"],
        "requestSubscribeMessage:fail not supported",
        "no host has accepted a subscription: {results}"
    );
    assert_eq!(
        results["setUserCloudStorage"]["res"]["errMsg"], "setUserCloudStorage:fail not supported",
        "nothing was stored: {results}"
    );
    assert_eq!(
        results["getPrivacySetting"]["res"]["needAuthorization"],
        false
    );
    assert_eq!(
        results["checkIsAddedToMyMiniProgram"]["res"]["added"],
        false
    );
    assert_eq!(
        results["getLatestUserKey"]["res"]["errMsg"], "getLatestUserKey:fail not supported",
        "no key was issued: {results}"
    );
    assert_eq!(chat_tool, false);
    assert_eq!(ext, serde_json::json!({}));
    assert_eq!(expt, serde_json::json!({}));
}

#[test]
fn a_request_reaches_the_host_by_name_and_its_files_through_the_sandbox() {
    let sandbox = Sandbox::new("ecosystem");
    let image = sandbox.paths.user_data_dir().join("share.png");
    std::fs::write(&image, b"png").unwrap();
    let host = Arc::new(FakeEcosystem::default());
    let mut runtime = boot(Some(host.clone()), Some(sandbox.vfs()));
    runtime
        .execute_script("<test:ecosystem-setup>", FastString::from_static(SETTLE))
        .unwrap();
    let results = run(
        &mut runtime,
        r#"
        __settle('getGroupEnterInfo', { foo: 1 });
        migo.getUserCryptoManager().getLatestUserKey({});
        __settle('shareImageToGroup', { imagePath: '/user/share.png' });
        __settle('shareEmojiToGroup', { imagePath: '/etc/hosts' });
        "#,
        "JSON.stringify(__results)",
    );
    let results: serde_json::Value = serde_json::from_str(&results).unwrap();
    assert!(
        results["shareEmojiToGroup"]["res"]["errMsg"]
            .as_str()
            .unwrap()
            .starts_with("shareEmojiToGroup:fail Path not allowed"),
        "{results}"
    );

    let calls = host.calls.lock().unwrap().clone();
    assert_eq!(
        calls.len(),
        3,
        "only the requests the sandbox allows: {calls:?}"
    );
    let first: serde_json::Value = serde_json::from_str(&calls[0]).unwrap();
    assert_eq!(first["api"], "getGroupEnterInfo");
    assert_eq!(first["options"]["foo"], 1);
    let key: serde_json::Value = serde_json::from_str(&calls[1]).unwrap();
    assert_eq!(
        key["api"], "UserCryptoManager.getLatestUserKey",
        "a method's request names its class and itself"
    );
    assert!(
        is_true(&mut runtime, "typeof migo.getLatestUserKey === 'undefined'"),
        "a method is not published as a global"
    );
    let share: serde_json::Value = serde_json::from_str(&calls[2]).unwrap();
    assert_eq!(
        std::fs::canonicalize(share["options"]["imagePath"].as_str().unwrap()).unwrap(),
        std::fs::canonicalize(&image).unwrap()
    );
}

#[test]
fn events_reach_listeners_and_answers_go_back_to_the_host() {
    let host = Arc::new(FakeEcosystem {
        values: HashMap::from([("isChatTool", "true"), ("getExtConfigSync", r#"{"a":1}"#)]),
        ..Default::default()
    });
    let mut runtime = boot(Some(host.clone()), None);
    let checked = run(
        &mut runtime,
        r#"
        migo.onCopyUrl(function () { return { query: 'a=1' }; });
        migo.onNeedPrivacyAuthorization(function (resolve, info) {
            resolve({ event: 'exposureAuthorization' });
            resolve({ event: 'agree', referrer: info.referrer });
            resolve({ event: 'disagree' });
        });
        const dispatch = (json) => globalThis[Symbol.for('Migo.hostBridge')]
            ._internalDispatch('_internalOnEcosystemEvent', JSON.stringify([json]));
        dispatch(JSON.stringify({ name: 'onCopyUrl', data: {}, replyId: 7 }));
        dispatch(JSON.stringify({ name: 'onNeedPrivacyAuthorization',
            data: { referrer: 'getUserInfo' }, replyId: 8 }));
        dispatch(JSON.stringify({ name: 'onVoIPChatStateChanged', data: {}, replyId: 9 }));
        dispatch(JSON.stringify({ name: 'onNoSuchEvent', data: {}, replyId: 10 }));
        "#,
        "JSON.stringify([migo.isChatTool(), migo.getExtConfigSync()])",
    );
    assert_eq!(checked, r#"[true,{"a":1}]"#);
    let replies = host.replies.lock().unwrap().clone();
    assert_eq!(
        replies,
        [
            r#"{"replyId":7,"data":{"query":"a=1"},"done":true}"#,
            r#"{"replyId":8,"data":{"event":"exposureAuthorization"},"done":false}"#,
            r#"{"replyId":8,"data":{"event":"agree","referrer":"getUserInfo"},"done":true}"#,
            r#"{"replyId":9,"data":null,"done":true}"#,
            r#"{"replyId":10,"data":null,"done":true}"#,
        ],
        "every event with a replyId is answered, finally with done"
    );
}

// ---- the objects ---------------------------------------------------------------

/// The request `api` the host was handed, by name.
fn request_for(host: &FakeEcosystem, api: &str) -> serde_json::Value {
    host.calls
        .lock()
        .unwrap()
        .iter()
        .map(|call| serde_json::from_str::<serde_json::Value>(call).unwrap())
        .find(|call| call["api"] == api)
        .unwrap_or_else(|| panic!("no {api} request reached the host"))
}

/// Script that answers `request` with `fields` through the result hook.
fn answer(request: &serde_json::Value, fields: &str) -> String {
    format!(
        "globalThis[Symbol.for('Migo.hostBridge')]._internalDispatch('_internalOnEcosystemResult', \
         JSON.stringify([JSON.stringify(Object.assign({{ requestId: {} }}, {fields}))]));",
        request["requestId"]
    )
}

/// Script that posts the event `name` with `data`.
fn post(name: &str, data: &str) -> String {
    format!(
        "globalThis[Symbol.for('Migo.hostBridge')]._internalDispatch('_internalOnEcosystemEvent', \
         JSON.stringify([JSON.stringify({{ name: '{name}', data: {data} }})]));"
    )
}

#[test]
fn the_game_service_crosses_by_class_and_member_and_frames_keep_their_bytes() {
    let host = Arc::new(FakeEcosystem::default());
    let mut runtime = boot(Some(host.clone()), None);
    run(
        &mut runtime,
        r#"
        globalThis.__cb = [];
        globalThis.__frames = [];
        const gsm = migo.getGameServerManager();
        globalThis.__single = gsm === migo.getGameServerManager();
        const room = gsm.createRoom({ maxMemberNum: 2, success(res) { __cb.push(res.errMsg); } });
        globalThis.__promised = room instanceof Promise;
        globalThis.__invite = gsm.setInviteData('lvl=3');
        gsm.inviteFriend({ openId: 'friend' });
        gsm.uploadFrame({ actionList: [new Uint8Array([1, 2, 255]).buffer] });
        gsm.uploadFrame({ actionList: ['a', new ArrayBuffer(1)] })
            .catch(function (e) { globalThis.__mixed = e.errMsg; });
        gsm.onSyncFrame(function (frame) {
            __frames.push([frame.frameId, frame.actionList.map(function (b) {
                return Array.from(new Uint8Array(b));
            })]);
        });
        "#,
        "0",
    );
    let apis: Vec<String> = host
        .calls
        .lock()
        .unwrap()
        .iter()
        .map(|call| serde_json::from_str::<serde_json::Value>(call).unwrap()["api"].to_string())
        .collect();
    assert_eq!(
        apis,
        [
            "\"GameServerManager.createRoom\"",
            "\"GameServerManager.inviteFriend\"",
            "\"GameServerManager.uploadFrame\"",
        ],
        "a mixed action list never leaves"
    );
    let invite = request_for(&host, "GameServerManager.inviteFriend");
    assert_eq!(
        invite["options"],
        serde_json::json!({ "openId": "friend", "data": "lvl=3" })
    );
    let frame = request_for(&host, "GameServerManager.uploadFrame");
    assert_eq!(
        frame["options"],
        serde_json::json!({ "actionList": ["0102ff"], "binary": true })
    );

    let room = request_for(&host, "GameServerManager.createRoom");
    assert_eq!(room["options"]["maxMemberNum"], 2);
    let script = answer(&room, "{ data: { accessInfo: 'room' } }")
        + &post(
            "GameServerManager.onSyncFrame",
            "{ frameId: 4, actionList: ['0aff'], binary: true }",
        );
    let checked = run(
        &mut runtime,
        &script,
        "JSON.stringify([__single, __promised, __invite, __cb, __frames, __mixed])",
    );
    assert_eq!(
        checked,
        r#"[true,true,true,["createRoom:ok"],[[4,[[10,255]]]],"uploadFrame:fail actionList mixes strings and binary actions"]"#
    );
}

#[test]
fn a_challenge_accepted_before_the_game_listened_reaches_its_first_listener() {
    let host = Arc::new(FakeEcosystem::default());
    let mut runtime = boot(Some(host), None);
    let early = post(
        "RankManager.onChallengeStart",
        "{ scoreKey: 'k', subScoreKey: 2 }",
    );
    run(&mut runtime, &early, "0");
    let checked = run(
        &mut runtime,
        r#"
        globalThis.__heard = [];
        const rank = migo.getRankManager();
        rank.onChallengeStart(function (res) { __heard.push(['first', res.scoreKey, res.subScoreKey]); });
        rank.onChallengeStart(function (res) { __heard.push(['second', res.scoreKey]); });
        "#,
        "JSON.stringify(__heard)",
    );
    assert_eq!(
        checked, r#"[["first","k",2]]"#,
        "held once, for the first listener"
    );
}

#[test]
fn reports_carry_their_merged_scene_and_a_gift_opens_through_the_host() {
    let host = Arc::new(FakeEcosystem {
        values: HashMap::from([("StoreGift.isSupported", "true")]),
        ..Default::default()
    });
    let mut runtime = boot(Some(host.clone()), None);
    run(
        &mut runtime,
        r#"
        globalThis.__out = {};
        const scene = migo.getScenePerformanceManager({
            commonInfo: { tier: 'high', role: 1 },
            success(res) { __out.ready = res.errMsg; },
        });
        scene.setCommonInfo({ role: 2, tier: 'high' });
        __out.common = scene.getCommonInfo();
        scene.setData({ sceneId: 7, sceneData: { role: 3, level: 1 } });
        const report = migo.getMiniReportManager({ eventList: ['1001', 5] });
        report.report({ eventID: '1001', levelID: 2 });
        report.report({ eventID: '1001', levelTime: NaN, fail(res) { __out.bad = res.errMsg; } });
        const gift = migo.createStoreGift({ presentOrderId: '42x' });
        __out.supported = gift.isSupported();
        gift.open().then(function (res) { __out.opened = res; });
        "#,
        "0",
    );
    let scene = request_for(&host, "ScenePerformanceManager.setData");
    assert_eq!(scene["options"]["sceneId"], 7);
    assert_eq!(
        scene["options"]["sceneData"],
        serde_json::json!({ "role": 3, "tier": "high", "level": 1 }),
        "the scene's own fields win over the common info"
    );
    assert_eq!(
        request_for(&host, "getMiniReportManager")["options"],
        serde_json::json!({ "eventList": ["1001"], "debug": false })
    );
    assert_eq!(
        request_for(&host, "MiniReportManager.report")["options"],
        serde_json::json!({ "eventID": "1001", "levelID": 2 })
    );
    let gift = request_for(&host, "StoreGift.open");
    assert_eq!(
        gift["options"],
        serde_json::json!({ "presentOrderId": "42x" })
    );
    let checked = run(&mut runtime, &answer(&gift, "{}"), "JSON.stringify(__out)");
    let out: serde_json::Value = serde_json::from_str(&checked).unwrap();
    assert_eq!(out["ready"], "getScenePerformanceManager:ok");
    assert_eq!(
        out["common"],
        serde_json::json!({ "role": 2, "tier": "high" })
    );
    assert_eq!(out["bad"], "report:fail invalid value for levelTime");
    assert_eq!(out["supported"], true);
    assert_eq!(
        out["opened"],
        serde_json::json!({ "errCode": 0, "errMsg": "ok" })
    );
}

#[test]
fn without_a_host_the_objects_say_so() {
    let mut runtime = boot(None, None);
    let checked = run(
        &mut runtime,
        r#"
        globalThis.__out = {};
        const gift = migo.createStoreGift({ presentOrderId: '42x' });
        __out.supported = gift.isSupported();
        gift.open().catch(function (e) { __out.open = e; });
        migo.getGameServerManager().login().catch(function (e) { __out.login = e.errMsg; });
        migo.getScenePerformanceManager({ fail(res) { __out.scene = res.errMsg; } });
        migo.getRankManager().update({ scoreKey: 'k', score: 1 })
            .catch(function (e) { __out.rank = e.errMsg; });
        "#,
        "JSON.stringify(__out)",
    );
    let out: serde_json::Value = serde_json::from_str(&checked).unwrap();
    assert_eq!(out["supported"], false);
    assert_eq!(
        out["open"],
        serde_json::json!({ "errCode": -1005, "errMsg": "open:fail not supported" })
    );
    assert_eq!(out["login"], "login:fail not supported");
    assert_eq!(
        out["scene"],
        "getScenePerformanceManager:fail not supported"
    );
    assert_eq!(out["rank"], "update:fail not supported");
}

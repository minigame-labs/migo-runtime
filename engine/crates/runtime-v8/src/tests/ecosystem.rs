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
        key["api"], "getLatestUserKey",
        "a method's request names it"
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

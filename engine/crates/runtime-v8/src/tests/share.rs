//! Sharing: what each request hands the host, and what content hears back.

use std::sync::{Arc, Mutex};

use deno_core::serde_json::{self, Value};
use shared::{
    protocol::error::ServiceError,
    services::{
        CommerceServices, ConnectivityServices, DeviceServices, MediaServices, SensorServices,
        ShareService, SystemUtilServices,
    },
};

use super::support::{SETTLE, Sandbox, boot_with_services, run_to_idle};

#[derive(Default)]
struct FakeShare {
    requests: Mutex<Vec<(&'static str, Value)>>,
}

impl FakeShare {
    fn record(&self, api: &'static str, request_json: &str) -> Result<(), ServiceError> {
        let request = serde_json::from_str(request_json).expect("a share request is JSON");
        self.requests.lock().unwrap().push((api, request));
        Ok(())
    }

    fn requests(&self) -> Vec<(&'static str, Value)> {
        self.requests.lock().unwrap().clone()
    }
}

impl ShareService for FakeShare {
    fn share_app_message(&self, request_json: &str) -> Result<(), ServiceError> {
        self.record("shareAppMessage", request_json)
    }
    fn share_message_to_friend(&self, request_json: &str) -> Result<(), ServiceError> {
        self.record("shareMessageToFriend", request_json)
    }
    fn show_share_image_menu(&self, request_json: &str) -> Result<(), ServiceError> {
        self.record("showShareImageMenu", request_json)
    }
    fn set_share_menu(&self, json: &str) -> Result<(), ServiceError> {
        self.record("setShareMenu", json)
    }
    fn menu_share_reply(&self, json: &str) -> Result<(), ServiceError> {
        self.record("menuShareReply", json)
    }
}

struct Host(Arc<FakeShare>);

impl SensorServices for Host {}
impl MediaServices for Host {}
impl ConnectivityServices for Host {}
impl SystemUtilServices for Host {}
impl CommerceServices for Host {
    fn share(&self) -> Option<Arc<dyn ShareService>> {
        Some(self.0.clone())
    }
}

fn canonical(path: &Value) -> std::path::PathBuf {
    std::fs::canonicalize(path.as_str().expect("a path")).expect("the path names a file")
}

#[test]
fn a_share_carries_what_content_set_and_its_image_through_the_sandbox() {
    let sandbox = Sandbox::new("share");
    let image = sandbox.paths.user_data_dir().join("card.png");
    std::fs::write(&image, b"png").unwrap();
    let host = Arc::new(FakeShare::default());
    let mut runtime = boot_with_services(
        Some(Arc::new(Host(host.clone())) as Arc<dyn DeviceServices>),
        Some(sandbox.vfs()),
    );
    runtime
        .execute_script(
            "<test:share-setup>",
            deno_core::FastString::from_static(SETTLE),
        )
        .unwrap();
    let results = run_to_idle(
        &mut runtime,
        r#"
        migo.onShareAppMessage(function () { return { title: 'from the menu listener' }; });
        __settle('shareAppMessage', { title: 'mine', imageUrl: '/user/card.png', toCurrentGroup: false });
        migo.shareAppMessage({ imageUrl: 'https://example.com/a.png' });
        __settle('showShareImageMenu', { path: '/user/card.png' });
        __settle('shareMessageToFriend', { openId: 'friend', imageUrl: 'https://example.com/a.png' });
        __settle('showShareImageMenu', { path: '/etc/hosts' });
        "#,
        "JSON.stringify(__results)",
    );
    let results: Value = serde_json::from_str(&results).unwrap();
    assert!(
        results["shareMessageToFriend"]["res"]["errMsg"]
            .as_str()
            .unwrap()
            .starts_with("shareMessageToFriend:fail"),
        "a friend share takes no network image: {results}"
    );

    let requests = host.requests();
    assert_eq!(
        requests.iter().map(|(api, _)| *api).collect::<Vec<_>>(),
        ["shareAppMessage", "shareAppMessage", "showShareImageMenu"],
        "only the requests the sandbox allows reach the host: {requests:?}"
    );
    let first = &requests[0].1;
    assert_eq!(
        first["title"], "mine",
        "the menu listener has no say over this call"
    );
    assert_eq!(first["toCurrentGroup"], false);
    assert!(
        first.get("query").is_none(),
        "an option content did not set is absent"
    );
    assert_eq!(
        canonical(&first["imageUrl"]),
        std::fs::canonicalize(&image).unwrap()
    );
    assert_eq!(requests[1].1["imageUrl"], "https://example.com/a.png");
    assert_eq!(
        canonical(&requests[2].1["path"]),
        std::fs::canonicalize(&image).unwrap()
    );
    assert_eq!(requests[2].1["needShowEntrance"], false);
}

#[test]
fn a_friend_share_carries_its_query_and_its_outcome_reaches_the_game() {
    let host = Arc::new(FakeShare::default());
    let mut runtime = boot_with_services(
        Some(Arc::new(Host(host.clone())) as Arc<dyn DeviceServices>),
        None,
    );
    let checked = run_to_idle(
        &mut runtime,
        r#"
        globalThis.__heard = [];
        globalThis.__callbacks = [];
        globalThis.__set = [
            migo.setMessageToFriendQuery({ query: 'x'.repeat(129) }),
            migo.setMessageToFriendQuery({ query: 'q', shareMessageToFriendScene: 51 }),
            migo.setMessageToFriendQuery({ query: 'room=7', shareMessageToFriendScene: 3 }),
        ];
        migo.onShareMessageToFriend(function (res) { __heard.push(res); });
        migo.shareMessageToFriend({
            openId: 'friend',
            success: function (res) { __callbacks.push(res.errMsg); },
        });
        "#,
        "JSON.stringify(__heard)",
    );
    assert_eq!(checked, "[]", "nothing is heard before the host answers");
    let request = host.requests()[0].1.clone();
    assert_eq!(request["openId"], "friend");
    assert_eq!(request["query"], "room=7");
    assert_eq!(request["shareMessageToFriendScene"], 3);

    let answer = format!(
        "globalThis[Symbol.for('Migo.hostBridge')]._internalDispatch(\
         '_internalOnShareMessageToFriendResult', JSON.stringify([JSON.stringify({{ requestId: {} }})]));",
        request["requestId"]
    );
    let checked = run_to_idle(
        &mut runtime,
        &answer,
        "JSON.stringify([__set, __heard, __callbacks])",
    );
    assert_eq!(
        checked,
        r#"[[false,false,true],[{"success":true,"errMsg":"shareMessageToFriend:ok"}],["shareMessageToFriend:ok"]]"#
    );
}

#[test]
fn without_a_host_a_friend_share_fails_and_the_game_hears_it() {
    let mut runtime = boot_with_services(None, None);
    let checked = run_to_idle(
        &mut runtime,
        r#"
        globalThis.__heard = [];
        migo.onShareMessageToFriend(function (res) { __heard.push(res); });
        migo.shareMessageToFriend({ openId: 'friend' }).catch(function () {});
        "#,
        "JSON.stringify(__heard)",
    );
    assert_eq!(
        checked,
        r#"[{"success":false,"errMsg":"shareMessageToFriend:fail not supported"}]"#
    );
}

#[test]
fn the_host_s_menu_follows_what_content_shows_and_hides() {
    let host = Arc::new(FakeShare::default());
    let mut runtime = boot_with_services(
        Some(Arc::new(Host(host.clone())) as Arc<dyn DeviceServices>),
        None,
    );
    run_to_idle(
        &mut runtime,
        r#"
        migo.showShareMenu({ menus: ['shareTimeline'], withShareTicket: true });
        migo.hideShareMenu({ menus: ['shareTimeline'] });
        migo.updateShareMenu({ isUpdatableMessage: true, activityId: 'act' });
        migo.hideShareMenu();
        "#,
        "0",
    );
    let menus: Vec<Value> = host
        .requests()
        .into_iter()
        .filter(|(api, _)| *api == "setShareMenu")
        .map(|(_, menu)| menu)
        .collect();
    assert_eq!(
        menus,
        [
            serde_json::json!({ "menus": ["shareAppMessage", "shareTimeline"], "withShareTicket": true }),
            serde_json::json!({ "menus": ["shareAppMessage"], "withShareTicket": true }),
            serde_json::json!({
                "menus": ["shareAppMessage"], "withShareTicket": true,
                "isUpdatableMessage": true, "activityId": "act",
            }),
            serde_json::json!({
                "menus": [], "withShareTicket": true,
                "isUpdatableMessage": true, "activityId": "act",
            }),
        ],
        "moments sharing comes with sharing to a friend, and the whole state follows each change"
    );
}

#[test]
fn a_menu_share_is_answered_by_the_game_with_its_images_through_the_sandbox() {
    let sandbox = Sandbox::new("menu-share");
    let image = sandbox.paths.user_data_dir().join("card.png");
    std::fs::write(&image, b"png").unwrap();
    let host = Arc::new(FakeShare::default());
    let mut runtime = boot_with_services(
        Some(Arc::new(Host(host.clone())) as Arc<dyn DeviceServices>),
        Some(sandbox.vfs()),
    );
    run_to_idle(
        &mut runtime,
        r#"
        migo.onShareAppMessage(function () {
            return {
                title: 'now', query: 'a=1',
                promise: Promise.resolve({ title: 'later', imageUrl: '/user/card.png' }),
            };
        });
        migo.onShareTimeline(function () {
            return { title: 'moments', imagePreviewUrl: 'https://example.com/p.png' };
        });
        migo.onAddToFavorites(function () {
            return { title: 'kept', imageUrl: '/etc/hosts', disableForward: true };
        });
        const menu = (json) => globalThis[Symbol.for('Migo.hostBridge')]
            ._internalDispatch('_internalOnShareMenuEvent', JSON.stringify([json]));
        menu(JSON.stringify({ menu: 'shareAppMessage', replyId: 1 }));
        menu(JSON.stringify({ menu: 'shareTimeline', replyId: 2 }));
        menu(JSON.stringify({ menu: 'addToFavorites', replyId: 3 }));
        menu(JSON.stringify({ menu: 'somethingElse', replyId: 4 }));
        "#,
        "0",
    );
    let mut replies: Vec<Value> = host
        .requests()
        .into_iter()
        .filter(|(api, _)| *api == "menuShareReply")
        .map(|(_, reply)| reply)
        .collect();
    replies.sort_by_key(|reply| reply["replyId"].as_u64());
    assert_eq!(
        replies.len(),
        4,
        "every menu share is answered: {replies:?}"
    );
    assert_eq!(replies[0]["menu"], "shareAppMessage");
    assert_eq!(
        replies[0]["content"]["title"], "later",
        "the promise's answer decides"
    );
    assert!(replies[0]["content"].get("query").is_none());
    assert_eq!(
        canonical(&replies[0]["content"]["imageUrl"]),
        std::fs::canonicalize(&image).unwrap()
    );
    assert_eq!(
        replies[1]["content"],
        serde_json::json!({ "title": "moments", "imagePreviewUrl": "https://example.com/p.png" })
    );
    assert_eq!(
        replies[2]["content"],
        serde_json::json!({ "title": "kept", "disableForward": true }),
        "an image that is not the game's file is dropped"
    );
    assert_eq!(replies[3]["content"], Value::Null);
}

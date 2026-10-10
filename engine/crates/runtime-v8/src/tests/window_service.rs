//! The desktop window APIs: what reaches the host, and what content is told.

use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

use deno_core::{FastString, JsRuntime, RuntimeOptions};
use shared::{
    protocol::error::ServiceError,
    services::{
        CommerceServices, ConnectivityServices, DeviceServices, MediaServices, SensorServices,
        SystemUtilServices, WindowService,
    },
    vfs::{GamePaths, VirtualFS},
};

#[derive(Default)]
struct FakeWindow(Mutex<Vec<String>>);

impl WindowService for FakeWindow {
    fn set_cursor(&self, json: &str) -> Result<(), ServiceError> {
        self.0.lock().unwrap().push(json.to_string());
        Ok(())
    }
}

struct Desktop(Arc<FakeWindow>);

impl SensorServices for Desktop {}
impl MediaServices for Desktop {}
impl ConnectivityServices for Desktop {}
impl CommerceServices for Desktop {}
impl SystemUtilServices for Desktop {
    fn window(&self) -> Option<Arc<dyn WindowService>> {
        Some(self.0.clone())
    }
}

struct Sandbox {
    root: PathBuf,
    paths: GamePaths,
}

impl Sandbox {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "migo-window-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let paths = GamePaths::new(root.join("files"), root.join("cache"), "windowed", 1)
            .expect("game paths");
        paths.ensure_directories().expect("sandbox directories");
        Self { root, paths }
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn boot(services: Option<Arc<dyn DeviceServices>>, vfs: Option<Arc<VirtualFS>>) -> JsRuntime {
    let mut state = super::support::test_host_state();
    state.device_services = services;
    state.vfs = vfs;
    let mut runtime = JsRuntime::new(RuntimeOptions {
        extensions: crate::main_extensions(state),
        ..Default::default()
    });
    crate::harden_global_scope(&mut runtime);
    runtime
}

fn eval_bool(runtime: &mut JsRuntime, source: &'static str) -> bool {
    let value = runtime
        .execute_script("<test:window>", FastString::from_static(source))
        .expect("window script");
    deno_core::scope!(scope, runtime);
    let local = deno_core::v8::Local::new(scope, value);
    local.is_true()
}

#[test]
fn set_cursor_hands_the_host_a_keyword_or_the_sandbox_file_and_nothing_else() {
    let sandbox = Sandbox::new();
    let cursor = sandbox.paths.user_data_dir().join("aim.cur");
    std::fs::write(&cursor, b"cur").unwrap();
    let window = Arc::new(FakeWindow::default());
    let mut runtime = boot(
        Some(Arc::new(Desktop(window.clone()))),
        Some(Arc::new(VirtualFS::from_game_paths(&sandbox.paths))),
    );

    assert!(eval_bool(&mut runtime, "migo.setCursor('pointer')"));
    assert!(eval_bool(
        &mut runtime,
        "migo.setCursor('/user/aim.cur', 4, 6)"
    ));
    assert!(!eval_bool(
        &mut runtime,
        "migo.setCursor('/etc/hosts', 0, 0)"
    ));
    assert!(!eval_bool(
        &mut runtime,
        "migo.setCursor('/user/missing.cur', 0, 0)"
    ));

    let sent = window.0.lock().unwrap().clone();
    assert_eq!(sent.len(), 2, "{sent:?}");
    assert_eq!(sent[0], r#"{"keyword":"pointer"}"#);
    let file: deno_core::serde_json::Value = deno_core::serde_json::from_str(&sent[1]).unwrap();
    assert_eq!(
        std::fs::canonicalize(file["path"].as_str().unwrap()).unwrap(),
        std::fs::canonicalize(&cursor).unwrap()
    );
    assert_eq!(
        (file["x"].as_f64(), file["y"].as_f64()),
        (Some(4.0), Some(6.0))
    );
}

#[test]
fn without_a_window_the_cursor_is_refused_and_pointer_lock_is_never_taken() {
    let mut runtime = boot(None, None);
    assert!(!eval_bool(&mut runtime, "migo.setCursor('default')"));
    assert!(eval_bool(
        &mut runtime,
        "migo.requestPointerLock(); migo.isPointerLocked() === false"
    ));
}

#[test]
fn the_pointer_is_locked_exactly_while_the_host_says_so() {
    let mut runtime = boot(Some(Arc::new(Desktop(Arc::default()))), None);
    assert!(eval_bool(
        &mut runtime,
        "migo.requestPointerLock(); \
         const before = migo.isPointerLocked(); \
         const dispatch = (json) => globalThis[Symbol.for('Migo.hostBridge')] \
             ._internalDispatch('_internalOnPointerLockEvent', JSON.stringify([json])); \
         dispatch('{\"locked\":true}'); \
         const during = migo.isPointerLocked(); \
         dispatch('{\"locked\":false}'); \
         !before && during && !migo.isPointerLocked()"
    ));
}

#[test]
fn create_path2d_makes_a_path2d() {
    let mut runtime = boot(None, None);
    assert!(eval_bool(
        &mut runtime,
        "const path = migo.createPath2D(); path.rect(0, 0, 1, 1); path instanceof Path2D"
    ));
}

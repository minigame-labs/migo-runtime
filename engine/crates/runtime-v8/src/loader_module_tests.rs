use super::*;
use std::{
    collections::BTreeMap,
    io,
    path::{Path, PathBuf},
};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "migo-module-types-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        Self(root)
    }
    fn loader(&self, cache: bool) -> MyModuleLoader {
        MyModuleLoader::new(
            cache.then(|| crate::code_cache::create_code_cache(&self.0)),
            Rc::new(RefCell::new(Some(Arc::new(MountTable::new(
                self.0.clone(),
            ))))),
        )
    }
    fn url(&self, name: &str) -> ModuleSpecifier {
        ModuleSpecifier::from_file_path(self.0.join(name)).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn options(requested_module_type: RequestedModuleType) -> ModuleLoadOptions {
    ModuleLoadOptions {
        requested_module_type,
        is_dynamic_import: false,
        is_synchronous: false,
    }
}
async fn loaded(
    loader: &MyModuleLoader,
    url: &ModuleSpecifier,
    requested: RequestedModuleType,
) -> Result<ModuleSource, ModuleLoaderError> {
    match loader.load(url, None, options(requested)) {
        ModuleLoadResponse::Sync(result) => result,
        ModuleLoadResponse::Async(future) => future.await,
    }
}

#[test]
fn explicit_extensions_encoded_names_and_query_identity_survive_resolution() {
    let fixture = Fixture::new();
    let loader = fixture.loader(false);
    let base = fixture.url("main.js");
    for (input, filename, query, fragment) in [
        ("feature", "feature.js", None, None),
        (
            "feature?build=2#one",
            "feature.js",
            Some("build=2"),
            Some("one"),
        ),
        (
            "./data.json?build=2#two",
            "data.json",
            Some("build=2"),
            Some("two"),
        ),
        ("./data%2Ejson", "data.json", None, None),
        ("./compute.wasm", "compute.wasm", None, None),
        ("./custom.extension", "custom.extension", None, None),
        ("./.hidden", ".hidden", None, None),
        (".hidden", ".hidden", None, None),
    ] {
        let url = loader
            .resolve(input, base.as_str(), ResolutionKind::Import)
            .unwrap();
        assert_eq!(
            url.to_file_path().unwrap(),
            fixture.0.join(filename),
            "{input}"
        );
        assert_eq!(url.query(), query, "{input}");
        assert_eq!(url.fragment(), fragment, "{input}");
    }
    assert!(
        loader
            .resolve("?variant=2#next", base.as_str(), ResolutionKind::Import)
            .is_err(),
        "core rejects query-only bare module specifiers; ordinary URL resolution is separate"
    );
}

const WASM: &[u8] = b"\0asm\x01\0\0\0\x01\x05\x01\x60\0\x01\x7f\x03\x02\x01\0\x07\x0a\x01\x06answer\0\0\x0a\x06\x01\x04\0\x41\x2a\x0b";

#[tokio::test]
async fn types_are_checked_before_io_and_only_js_receives_a_js_cache() {
    let fixture = Fixture::new();
    let loader = fixture.loader(true);
    fs::write(
        fixture.0.join("data.json"),
        r#"{"text":"module.exports = not code", "value": 42}"#,
    )
    .unwrap();
    fs::write(fixture.0.join("compute.wasm"), WASM).unwrap();
    fs::write(fixture.0.join("main.js"), "export const answer = 42;").unwrap();
    let json = loaded(
        &loader,
        &fixture.url("data.json"),
        RequestedModuleType::Json,
    )
    .await
    .unwrap();
    assert_eq!(json.module_type, ModuleType::Json);
    assert!(json.code_cache.is_none());
    assert_eq!(
        json.code.as_bytes()[0],
        b'{',
        "JSON must bypass CJS rewriting"
    );
    let wasm = loaded(
        &loader,
        &fixture.url("compute.wasm"),
        RequestedModuleType::None,
    )
    .await
    .unwrap();
    assert_eq!(wasm.module_type, ModuleType::Wasm);
    assert!(wasm.code_cache.is_none());
    assert_eq!(wasm.code.as_bytes(), WASM);
    assert!(
        loaded(&loader, &fixture.url("main.js"), RequestedModuleType::None)
            .await
            .unwrap()
            .code_cache
            .is_some()
    );
    for (name, requested) in [
        ("data.json", RequestedModuleType::None),
        ("main.js", RequestedModuleType::Json),
        ("compute.wasm", RequestedModuleType::Json),
        ("main.js", RequestedModuleType::Other("unexpected".into())),
    ] {
        assert!(
            loaded(&loader, &fixture.url(name), requested)
                .await
                .is_err(),
            "{name}"
        );
    }
}

#[tokio::test]
async fn core_evaluates_real_json_and_wasm_imports_and_query_variants() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("data.json"), r#"{"value":10}"#).unwrap();
    fs::write(fixture.0.join("compute.wasm"), WASM).unwrap();
    fs::write(
        fixture.0.join("counter.js"),
        "globalThis.count = (globalThis.count || 0) + 1; export const value = globalThis.count;",
    )
    .unwrap();
    fs::write(
        fixture.0.join("main.js"),
        r#"
        import data from './data.json' with {type:'json'};
        import {answer} from './compute.wasm';
        import {value as a} from './counter?variant=1';
        import {value as b} from './counter?variant=2';
        globalThis.result = data.value + answer();
        if (a === b) throw new Error('query variants lost their identity');
    "#,
    )
    .unwrap();
    crate::icu::install_icu_data();
    let mut rt = deno_core::JsRuntime::new(deno_core::RuntimeOptions {
        module_loader: Some(Rc::new(fixture.loader(false))),
        ..Default::default()
    });
    let id = rt
        .load_main_es_module(&fixture.url("main.js"))
        .await
        .unwrap();
    let evaluated = rt.mod_evaluate(id);
    rt.run_event_loop(Default::default()).await.unwrap();
    evaluated.await.unwrap();
    rt.execute_script(
        "module-types-result",
        "if (globalThis.result !== 52) throw new Error('JSON/Wasm import result');",
    )
    .unwrap();
}

#[derive(Debug)]
struct MemoryModules(BTreeMap<String, Vec<u8>>);
impl shared::vfs::mount::MountBackend for MemoryModules {
    fn read(&self, path: &str) -> io::Result<Vec<u8>> {
        self.0
            .get(path)
            .cloned()
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))
    }
    fn exists(&self, path: &str) -> bool {
        self.0.contains_key(path)
    }
    fn real_path(&self, _: &str) -> Option<PathBuf> {
        None
    }
    fn root_dir(&self) -> Option<&Path> {
        None
    }
    fn entry_size(&self, path: &str) -> Option<u64> {
        self.0.get(path).map(|bytes| bytes.len() as u64)
    }
    fn is_file(&self, path: &str) -> bool {
        self.exists(path)
    }
}

#[tokio::test]
async fn pack_backed_generation_preserves_types_and_query_fragment() {
    let fixture = Fixture::new();
    let mounts = Arc::new(MountTable::new(fixture.0.clone()));
    mounts.swap_base(Arc::new(MemoryModules(BTreeMap::from([
        ("data.json".into(), br#"{"text":"module.exports"}"#.to_vec()),
        ("compute.wasm".into(), WASM.to_vec()),
    ]))));
    let loader = MyModuleLoader::new(None, Rc::new(RefCell::new(Some(mounts))));
    for (name, requested, kind) in [
        ("data.json", RequestedModuleType::Json, ModuleType::Json),
        ("compute.wasm", RequestedModuleType::None, ModuleType::Wasm),
    ] {
        let url = loader
            .resolve(
                &format!("./{name}?generation=1#kept"),
                fixture.url("main.js").as_str(),
                ResolutionKind::Import,
            )
            .unwrap();
        assert!(url.path().contains(".pack_gen"));
        assert_eq!(url.query(), Some("generation=1"));
        assert_eq!(url.fragment(), Some("kept"));
        let source = loaded(&loader, &url, requested).await.unwrap();
        assert_eq!(source.module_type, kind);
    }
}

#[tokio::test]
async fn admitted_package_read_is_pinned_and_retired_urls_fail_closed() {
    let fixture = Fixture::new();
    let mounts = Arc::new(MountTable::new(fixture.0.clone()));
    let package = |label: &str| {
        Arc::new(MemoryModules(BTreeMap::from([(
            "data.json".into(),
            format!(r#"{{"generation":"{label}"}}"#).into_bytes(),
        )])))
    };
    mounts.swap_base(package("old"));
    let loader = MyModuleLoader::new(None, Rc::new(RefCell::new(Some(mounts.clone()))));
    let base = fixture.url("main.js");
    let old = loader
        .resolve("./data.json", base.as_str(), ResolutionKind::Import)
        .unwrap();
    // Do not poll the read until after the mount changes. Admission must have
    // captured ownership, rather than looking up the then-current backend.
    let ModuleLoadResponse::Async(pending) =
        loader.load(&old, None, options(RequestedModuleType::Json))
    else {
        panic!("package read must use the bounded IO scheduler");
    };
    let pin = mounts
        .pin_pack_read("data.json", mounts.generation())
        .unwrap();
    assert!(
        pin.read_limited(1).is_err(),
        "reject metadata before reading"
    );
    mounts.swap_base(package("new"));
    let source = pending.await.unwrap();
    assert_eq!(source.code.as_bytes(), br#"{"generation":"old"}"#);
    assert!(
        loaded(&loader, &old, RequestedModuleType::Json)
            .await
            .is_err()
    );
    let new = loader
        .resolve("./data.json", base.as_str(), ResolutionKind::Import)
        .unwrap();
    assert_ne!(old, new);
    assert_eq!(
        loaded(&loader, &new, RequestedModuleType::Json)
            .await
            .unwrap()
            .code
            .as_bytes(),
        br#"{"generation":"new"}"#
    );
    for suffix in [
        "0/data.json",
        "bad/data.json",
        "999999/data.json",
        "2/missing.json",
    ] {
        let invalid = fixture.url(&format!(".pack_gen/{suffix}"));
        assert!(
            matches!(
                loader.load(&invalid, None, options(RequestedModuleType::Json)),
                ModuleLoadResponse::Sync(Err(_))
            ),
            "{suffix}"
        );
    }
}

#[test]
fn oversized_regular_module_is_rejected_before_read_admission() {
    let fixture = Fixture::new();
    let path = fixture.0.join("oversized.js");
    let file = fs::File::create(&path).unwrap();
    file.set_len(MAX_READ_LENGTH + 1).unwrap();
    assert!(module_read(&path).is_err());
}

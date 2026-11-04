use std::{env, fs, path::PathBuf};

fn main() {
    graph_table_layout();
    println!("cargo:rerun-if-changed=build.rs");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("android") {
        return;
    }
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let source = root.join("../../third_party/oboe-1.11.0");
    println!("cargo:rerun-if-changed={}", source.display());
    println!("cargo:rerun-if-changed=native");
    // Upstream's default includes both lists, with data conversion enabled.
    // Consume that list directly so an upgrade cannot silently omit a new unit.
    let cmake = fs::read_to_string(source.join("CMakeLists.txt")).unwrap();
    let sources: Vec<_> = cmake
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("src/") && line.ends_with(".cpp"))
        .map(|line| source.join(line))
        .collect();
    assert_eq!(sources.len(), 62, "review Oboe's native compilation inputs");
    cc::Build::new()
        .cpp(true)
        .std("c++17")
        // These objects are an SDK-private backend, not another process-wide
        // Oboe ABI. Prevent interposition with a host's different Oboe version.
        .flag("-fvisibility=hidden")
        .cpp_link_stdlib("c++_shared")
        .include(source.join("include"))
        .include(source.join("src"))
        .files(sources)
        .file(root.join("native/oboe_bridge.cpp"))
        .compile("migo_oboe");
    for library in ["log", "OpenSLES", "dl"] {
        println!("cargo:rustc-link-lib={library}");
    }
}

// The model follows the pinned compiler's std/hashbrown, whose target features
// are those used to build std, not arbitrary consumer -C target-feature flags.
fn graph_table_layout() {
    use std::process::Command;
    println!("cargo:rerun-if-env-changed=RUSTC");
    let rustc = env::var_os("RUSTC").expect("Cargo RUSTC");
    let version = Command::new(&rustc)
        .arg("--version")
        .output()
        .expect("rustc version");
    assert!(version.status.success());
    let version = String::from_utf8(version.stdout).unwrap();
    assert_eq!(
        version.split_whitespace().nth(1),
        Some("1.95.0"),
        "review graph hash allocation layout before changing Rust"
    );
    let cfg = Command::new(rustc)
        .args(["--print", "cfg", "--target", &env::var("TARGET").unwrap()])
        .output()
        .expect("standard target cfg");
    assert!(cfg.status.success(), "standard target cfg failed");
    let cfg = String::from_utf8(cfg.stdout).unwrap();
    let has = |key: &str| cfg.lines().any(|line| line == key);
    let group = if (has("target_arch=\"x86_64\"") || has("target_arch=\"x86\""))
        && has("target_feature=\"sse2\"")
        || has("target_arch=\"loongarch64\"") && has("target_feature=\"lsx\"")
    {
        16
    } else if has("target_pointer_width=\"64\"")
        || has("target_arch=\"aarch64\"")
        || has("target_arch=\"x86_64\"")
        || has("target_arch=\"wasm32\"")
    {
        8
    } else {
        4
    };
    println!("cargo:rustc-env=MIGO_HASH_GROUP_WIDTH={group}");
}

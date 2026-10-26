use std::{env, fs, path::PathBuf};

fn main() {
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

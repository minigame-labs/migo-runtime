use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rustc-check-cfg=cfg(migo_native_alloc_probe)");
    variable_rate();
    minimp3();
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

/// Upstream minimp3 with mp3dec_ex (see native/mp3.c for the symbol renaming).
fn minimp3() {
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let source = root.join("../../third_party/minimp3-afb604c");
    println!("cargo:rerun-if-changed={}", source.display());
    println!("cargo:rerun-if-changed=native/mp3.c");
    cc::Build::new()
        .std("c11")
        .opt_level(3)
        .flag_if_supported("-fvisibility=hidden")
        // Upstream code: its own warnings are not ours to fix.
        .warnings(false)
        .include(&source)
        .file("native/mp3.c")
        .compile("migo_minimp3");
}

fn variable_rate() {
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let source = root.join("../../third_party/libsamplerate-0.2.2");
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let little = env::var("CARGO_CFG_TARGET_ENDIAN").unwrap() == "little";
    fs::write(
        out.join("config.h"),
        format!(
            "#define PACKAGE \"libsamplerate\"\n#define VERSION \"0.2.2\"\n\
         #define HAVE_STDBOOL_H 1\n#define HAVE_STDINT_H 1\n\
         #define ENABLE_SINC_FAST_CONVERTER 1\n\
         #define CPU_CLIPS_NEGATIVE 0\n#define CPU_CLIPS_POSITIVE 0\n\
         #define CPU_IS_LITTLE_ENDIAN {}\n#define CPU_IS_BIG_ENDIAN {}\n",
            u8::from(little),
            u8::from(!little)
        ),
    )
    .unwrap();
    println!("cargo:rerun-if-changed={}", source.display());
    println!("cargo:rerun-if-changed=native/variable_rate.c");
    cc::Build::new()
        .std("c11")
        .opt_level(3)
        .flag_if_supported("-fvisibility=hidden")
        .define("HAVE_CONFIG_H", None)
        .include(&out)
        .include(source.join("include"))
        .include(source.join("src"))
        .files(
            ["samplerate.c", "src_linear.c", "src_zoh.c"].map(|name| source.join("src").join(name)),
        )
        .file(root.join("native/variable_rate.c"))
        .compile("migo_variable_rate");
    // libsamplerate calls into libm. MSVC has no separate math library -- the CRT
    // carries it -- and `m.lib` does not exist there: naming it stopped the
    // Windows SDK link with LNK1181.
    if env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc") {
        println!("cargo:rustc-link-lib=m");
    }
}

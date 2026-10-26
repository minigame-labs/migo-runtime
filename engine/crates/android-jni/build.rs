fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("android") {
        return;
    }
    let script = std::path::PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap())
        .join("../../version.script")
        .canonicalize()
        .expect("the Android JNI export script must exist");
    println!("cargo:rerun-if-changed={}", script.display());
    // Android unit executables and the C ABI archive do not export JNI_OnLoad.
    // Only this crate's cdylib gets the JNI export filter.
    println!(
        "cargo:rustc-cdylib-link-arg=-Wl,--version-script={}",
        script.display()
    );
}

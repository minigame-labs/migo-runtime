//! Compiles the C host example and links the window-system libraries it needs.
//!
//! The C source lives outside the crate (`tests/c_host/linux/main.c`) because it is
//! documentation as much as it is a test: a host author should be able to read
//! it without knowing anything about cargo.

use std::path::PathBuf;

fn main() {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repo root");
    let source = repo_root.join("tests/c_host/linux/main.c");
    let wayland_source = repo_root.join("tests/c_host/linux/wayland_host.c");
    let include = repo_root.join("include");

    println!("cargo:rerun-if-changed={}", source.display());
    println!("cargo:rerun-if-changed={}", wayland_source.display());
    println!("cargo:rerun-if-changed={}", include.display());

    // xdg-shell is a protocol, not a library: wayland-scanner turns its XML
    // into the stubs and interface tables a client links. Generated at build
    // time rather than vendored so the example always matches the
    // wayland-protocols the machine actually has.
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let xml = PathBuf::from("/usr/share/wayland-protocols/stable/xdg-shell/xdg-shell.xml");
    // The XML and the scanner come from the build machine, but the code they
    // produce is compiled against the *target's* libwayland -- which, for an SDK
    // build, is a pinned sysroot far older than a current desktop. A modern
    // scanner emits calls to wl_proxy_marshal_flags, added in libwayland 1.19.91;
    // against an older client header that is a hard compile error in generated
    // code nobody wrote. Probing the target decides whether these two halves can
    // agree, instead of discovering that they cannot part-way through the build.
    let generated = xml.exists()
        && wayland_client_supports_marshal_flags(&out_dir)
        && generate_xdg_shell(&xml, &out_dir);

    let mut build = cc::Build::new();
    build
        .file(&source)
        .include(&include)
        .std("c11")
        .warnings(true);

    if generated {
        build
            .file(&wayland_source)
            .file(out_dir.join("xdg-shell-protocol.c"))
            .include(&out_dir);
        println!("cargo:rustc-link-lib=wayland-client");
    } else {
        // Without usable protocol code there is no Wayland host to build. Say so
        // rather than emitting a binary whose --backend=wayland silently does
        // nothing.
        println!(
            "cargo:warning=no usable xdg-shell protocol code for this target \
             (missing wayland-protocols, or the target's libwayland predates \
             wl_proxy_marshal_flags); the Wayland host is omitted from this build"
        );
        build.define("MIGO_C_HOST_NO_WAYLAND", None);
    }
    build.compile("c_host_main");

    // The example owns its window, so it -- not the engine -- needs Xlib. GL and
    // EGL are not declared here: the graphics crate now declares the GL
    // dependency its Skia backend creates, and EGL arrives through the engine's
    // own link libraries. A consumer should not have to know either.
    println!("cargo:rustc-link-lib=X11");
}

/// Whether the libwayland this build targets declares `wl_proxy_marshal_flags`.
///
/// Compiled rather than version-sniffed: the sysroot in effect comes from the
/// C flags cargo hands this build, so asking the compiler is the only check that
/// cannot disagree with what the real compile will do.
fn wayland_client_supports_marshal_flags(out_dir: &std::path::Path) -> bool {
    let probe = out_dir.join("migo_wayland_probe.c");
    if std::fs::write(
        &probe,
        "#include <wayland-client-core.h>\n\
         const void *migo_wayland_probe(void) { return (const void *)&wl_proxy_marshal_flags; }\n",
    )
    .is_err()
    {
        return false;
    }
    cc::Build::new()
        .file(&probe)
        .std("c11")
        // A probe must not contribute link directives or fail the build; only
        // its success or failure is wanted.
        .cargo_metadata(false)
        .warnings(false)
        .try_compile("migo_wayland_probe")
        .is_ok()
}

/// Run wayland-scanner for the client header and the private protocol code.
fn generate_xdg_shell(xml: &std::path::Path, out_dir: &std::path::Path) -> bool {
    let run = |mode: &str, output: PathBuf| {
        std::process::Command::new("wayland-scanner")
            .arg(mode)
            .arg(xml)
            .arg(&output)
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    };
    run("client-header", out_dir.join("xdg-shell-client-protocol.h"))
        && run("private-code", out_dir.join("xdg-shell-protocol.c"))
}

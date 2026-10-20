//! Compiles the Qt host (`tests/qt_host/linux`) and the Linux Host Kit it is
//! built on, and links Qt.
//!
//! The C++ is what an App compiles: the Host Kit's sources and its own, against
//! the public headers. Qt's meta-object compiler runs here over the Host Kit's
//! QObject headers -- what CMake's AUTOMOC does for an App -- and pkg-config
//! says where Qt is.

use std::{
    path::{Path, PathBuf},
    process::Command,
};

/// The Host Kit's headers that declare QObjects, which moc reads.
const MOC_HEADERS: [&str; 3] = [
    "include/migo/linux/qt6/x11_surface_view.hpp",
    "include/migo/linux/qt6/host_services.hpp",
    "include/migo/linux/qt6/managed_session.hpp",
];

/// The Host Kit's sources this host links: the surface controller, the Qt view,
/// the host services and the Managed shape.
const KIT_SOURCES: [&str; 6] = [
    "src/surface_host.cpp",
    "src/qt6/x11_surface_view.cpp",
    "src/qt6/dom_key_names.cpp",
    "src/qt6/xi2_raw_motion.cpp",
    "src/qt6/host_services.cpp",
    "src/qt6/managed_session.cpp",
];

fn pkg_config(arguments: &[&str]) -> String {
    let output = Command::new("pkg-config")
        .args(arguments)
        .output()
        .expect("pkg-config is required to find Qt 6");
    assert!(
        output.status.success(),
        "pkg-config {arguments:?} failed: the Qt host needs Qt 6 Widgets (qt6-base-dev)"
    );
    String::from_utf8(output.stdout)
        .expect("pkg-config prints UTF-8")
        .trim()
        .to_owned()
}

fn main() {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repo root");
    let kit = repo.join("platforms/linux/host-kit");
    let host = repo.join("tests/qt_host/linux/main.cpp");
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));

    println!("cargo:rerun-if-changed={}", kit.join("include").display());
    println!("cargo:rerun-if-changed={}", kit.join("src").display());
    println!("cargo:rerun-if-changed={}", host.display());
    println!("cargo:rerun-if-changed={}", repo.join("include").display());

    let flags = pkg_config(&["--cflags", "Qt6Widgets"]);
    let includes: Vec<&str> = flags
        .split_whitespace()
        .filter_map(|flag| flag.strip_prefix("-I"))
        .collect();
    let defines: Vec<&str> = flags
        .split_whitespace()
        .filter_map(|flag| flag.strip_prefix("-D"))
        .collect();
    let moc = Path::new(&pkg_config(&["--variable=libexecdir", "Qt6Core"])).join("moc");

    let mut build = cc::Build::new();
    build
        .cpp(true)
        .std("c++17")
        .include(repo.join("include"))
        .include(kit.join("include"))
        .warnings(true);
    for directory in &includes {
        build.include(directory);
    }
    for define in &defines {
        build.define(define, None);
    }
    for header in MOC_HEADERS {
        let input = kit.join(header);
        let stem = input.file_stem().expect("a header name").to_string_lossy().into_owned();
        let output = out.join(format!("moc_{stem}.cpp"));
        let mut command = Command::new(&moc);
        // The generated file includes its header as an App does, through the
        // include path, rather than by a path relative to OUT_DIR that a
        // symlinked target directory makes wrong.
        let included = header.strip_prefix("include/").expect("a public header");
        command.arg(&input).arg("-o").arg(&output).arg(format!("-f{included}"));
        for directory in &includes {
            command.arg(format!("-I{directory}"));
        }
        let status = command
            .status()
            .unwrap_or_else(|error| panic!("running {}: {error}", moc.display()));
        assert!(status.success(), "moc failed on {}", input.display());
        build.file(output);
    }
    for source in KIT_SOURCES {
        build.file(kit.join(source));
    }
    build.file(&host);
    build.compile("migo_qt_host");

    // The host owns its window and the Host Kit's view reads its X11 handle and
    // the mouse's raw motion (XInput 2), so these -- not the engine -- link Qt,
    // Xlib and xcb.
    for library in ["Qt6Widgets", "Qt6Gui", "Qt6Core", "X11", "xcb-xinput", "xcb"] {
        println!("cargo:rustc-link-lib={library}");
    }
}

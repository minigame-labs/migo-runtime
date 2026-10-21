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

/// The Host Kit's headers that declare QObjects, which moc reads, each with the
/// name its source includes it by: a public header through the public include
/// path, an internal one beside the sources.
const MOC_HEADERS: [(&str, &str); 7] = [
    (
        "include/migo/linux/qt6/x11_surface_view.hpp",
        "migo/linux/qt6/x11_surface_view.hpp",
    ),
    (
        "include/migo/linux/qt6/host_services.hpp",
        "migo/linux/qt6/host_services.hpp",
    ),
    (
        "include/migo/linux/qt6/managed_session.hpp",
        "migo/linux/qt6/managed_session.hpp",
    ),
    ("src/qt6/memory_monitor.hpp", "memory_monitor.hpp"),
    ("src/qt6/geolocation.hpp", "geolocation.hpp"),
    ("src/qt6/bluetooth.hpp", "bluetooth.hpp"),
    ("src/qt6/beacons.hpp", "beacons.hpp"),
];

/// The Host Kit's sources this host links: the surface controller, the Qt view,
/// the host services, the Managed shape and the videos (`migo::qt6-media`).
const KIT_SOURCES: [&str; 18] = [
    "src/surface_host.cpp",
    "src/qt6/x11_surface_view.cpp",
    "src/qt6/dom_key_names.cpp",
    "src/qt6/xi2_raw_motion.cpp",
    "src/qt6/host_services.cpp",
    "src/qt6/backlight.cpp",
    "src/qt6/images.cpp",
    "src/qt6/media_viewer.cpp",
    "src/qt6/geolocation.cpp",
    "src/qt6/motion.cpp",
    "src/qt6/bluetooth.cpp",
    "src/qt6/beacons.cpp",
    "src/qt6/managed_session.cpp",
    "src/qt6/screen_awake.cpp",
    "src/qt6/system_reports.cpp",
    "src/qt6/memory_monitor.cpp",
    "src/qt6/gamepads.cpp",
    "src/qt6/media.cpp",
];

/// What the videos link: GStreamer, its app sink and its video helpers.
const GSTREAMER: [&str; 3] = ["gstreamer-1.0", "gstreamer-app-1.0", "gstreamer-video-1.0"];

fn pkg_config(arguments: &[&str]) -> String {
    let output = Command::new("pkg-config")
        .args(arguments)
        .output()
        .expect("pkg-config is required to find Qt 6 and GStreamer");
    assert!(
        output.status.success(),
        "pkg-config {arguments:?} failed: the Qt host needs Qt 6 Widgets (qt6-base-dev) and \
         GStreamer (libgstreamer1.0-dev, libgstreamer-plugins-base1.0-dev)"
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

    let flags = pkg_config(
        &[
            &["--cflags", "Qt6Widgets", "Qt6DBus", "Qt6Network", "libudev"][..],
            &GSTREAMER[..],
        ]
        .concat(),
    );
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
        .include(kit.join("src/qt6"))
        .warnings(true);
    for directory in &includes {
        build.include(directory);
    }
    for define in &defines {
        build.define(define, None);
    }
    for (header, included) in MOC_HEADERS {
        let input = kit.join(header);
        let stem = input
            .file_stem()
            .expect("a header name")
            .to_string_lossy()
            .into_owned();
        let output = out.join(format!("moc_{stem}.cpp"));
        let mut command = Command::new(&moc);
        // The generated file includes its header as the sources do, through the
        // include path, rather than by a path relative to OUT_DIR that a
        // symlinked target directory makes wrong.
        command
            .arg(&input)
            .arg("-o")
            .arg(&output)
            .arg(format!("-f{included}"));
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
    // the mouse's raw motion (XInput 2), and its services ask the system over
    // D-Bus, download a preview's images and read the motion sensors through
    // udev, and play videos through GStreamer, so these -- not the engine --
    // link Qt, Xlib, xcb, libudev and GStreamer.
    for library in [
        "Qt6Widgets",
        "Qt6DBus",
        "Qt6Network",
        "Qt6Gui",
        "Qt6Core",
        "X11",
        "xcb-xinput",
        "xcb",
        "udev",
    ] {
        println!("cargo:rustc-link-lib={library}");
    }
    let gstreamer = pkg_config(&[&["--libs-only-l"][..], &GSTREAMER[..]].concat());
    for library in gstreamer.split_whitespace() {
        if let Some(name) = library.strip_prefix("-l") {
            println!("cargo:rustc-link-lib={name}");
        }
    }
}

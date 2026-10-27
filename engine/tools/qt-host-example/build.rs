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

/// The Host Kit's files of one kind, as its CMakeLists.txt names them, in its
/// order: the list an App builds the kit from. This host used to restate it and
/// fell behind -- keyboard_field.cpp joined the Managed target for showKeyboard,
/// and the lane's host stopped linking -- so it reads the list instead.
fn kit_files(cmake: &str, extension: &str) -> Vec<String> {
    let mut files: Vec<String> = Vec::new();
    for line in cmake.lines() {
        let line = line.split('#').next().unwrap_or_default();
        for token in line.split_whitespace() {
            let token = token.trim_matches(|c| c == '(' || c == ')' || c == '"');
            let named = token.starts_with("src/") || token.starts_with("include/");
            if named && token.ends_with(extension) && !files.iter().any(|file| file == token) {
                files.push(token.to_owned());
            }
        }
    }
    assert!(
        !files.is_empty(),
        "the Host Kit's CMakeLists.txt names no {extension} file"
    );
    files
}

/// The name a source includes a QObject header by: a public header through the
/// public include path, an internal one beside the sources.
fn included_name(header: &str) -> &str {
    header
        .strip_prefix("include/")
        .or_else(|| header.strip_prefix("src/qt6/"))
        .unwrap_or_else(|| panic!("{header} is on no include path this host passes"))
}

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

    let cmake_lists = kit.join("CMakeLists.txt");
    let cmake = std::fs::read_to_string(&cmake_lists)
        .unwrap_or_else(|error| panic!("reading {}: {error}", cmake_lists.display()));
    println!("cargo:rerun-if-changed={}", cmake_lists.display());
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
    // The headers moc reads are the ones that declare a QObject, as CMake's
    // AUTOMOC decides for an App.
    for header in kit_files(&cmake, ".hpp") {
        let input = kit.join(&header);
        let text = std::fs::read_to_string(&input)
            .unwrap_or_else(|error| panic!("reading {}: {error}", input.display()));
        if !text.contains("Q_OBJECT") {
            continue;
        }
        let included = included_name(&header);
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
    for source in kit_files(&cmake, ".cpp") {
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

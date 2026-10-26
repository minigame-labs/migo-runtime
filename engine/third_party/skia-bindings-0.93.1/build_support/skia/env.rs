/// Environment variables used for configuring the Skia build.
use crate::build_support::cargo;
use std::path::PathBuf;

/// A boolean specifying whether to build Skia's dependencies or not. If not, the system's
/// provided libraries are used.
pub fn use_system_libraries() -> bool {
    cargo::env_var("SKIA_USE_SYSTEM_LIBRARIES").is_some()
}

/// The header directories of an ICU that the final link already provides (an embedder's own, such as
/// V8's). When set, Skia is built against those headers and links no ICU of its own, so a binary that
/// carries both does not carry two. The ICU's version-suffixed symbols are resolved at the final link.
pub fn external_icu_include_dirs() -> Option<Vec<PathBuf>> {
    cargo::env_var("SKIA_EXTERNAL_ICU_INCLUDE_DIRS").map(|dirs| std::env::split_paths(&dirs).collect())
}

/// The full path of the ninja command to run.
pub fn ninja_command() -> Option<PathBuf> {
    cargo::env_var("SKIA_NINJA_COMMAND").map(PathBuf::from)
}

/// The full path of the gn command to run.
pub fn gn_command() -> Option<PathBuf> {
    cargo::env_var("SKIA_GN_COMMAND").map(PathBuf::from)
}

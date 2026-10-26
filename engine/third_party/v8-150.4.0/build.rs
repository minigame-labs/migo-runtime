// Copyright 2018-2019 the Deno authors. All rights reserved. MIT license.
// Migo consumes a separately built, verified native component. Native source
// builds belong to scripts/build-v8-<os>.sh, never this Cargo dependency.
use std::{env, path::PathBuf};

fn env_bool(key: &str) -> bool {
  matches!(env::var(key).unwrap_or_default().as_str(), "true" | "1" | "yes")
}

fn local_input(name: &str) -> PathBuf {
  let value = env::var_os(name).unwrap_or_else(|| {
    panic!("Migo V8 requires {name}; materialise a verified native component first")
  });
  let path = PathBuf::from(value);
  assert!(path.is_file(), "{name} must name a materialised local file: {path:?}");
  println!("cargo:rerun-if-changed={}", path.display());
  path.canonicalize().expect("cannot resolve materialised V8 input")
}

fn main() {
  for name in ["V8_FROM_SOURCE", "RUSTY_V8_ARCHIVE", "RUSTY_V8_SRC_BINDING_PATH",
               "DOCS_RS", "DENO_TRYBUILD", "CARGO", "GN_ARGS", "CXXSTDLIB", "V8_FORCE_DEBUG"] {
    println!("cargo:rerun-if-env-changed={name}");
  }
  assert!(!env_bool("V8_FROM_SOURCE"),
          "Migo's Rust-only V8 consumer cannot build native sources; use scripts/build-v8-<os>.sh and materialise the component");
  let docs = env::var_os("DOCS_RS").is_some() || env::var_os("CARGO")
    .map(PathBuf::from).as_ref().and_then(|p| p.file_stem())
    .and_then(|p| p.to_str()).is_some_and(|p| p.starts_with("rls"));
  let binding = if docs && env::var_os("RUSTY_V8_SRC_BINDING_PATH").is_none() {
    let target = env::var("TARGET").expect("TARGET");
    let profile = if env::var("CARGO_CFG_TARGET_OS").unwrap_or_default() != "windows"
      && env_bool("V8_FORCE_DEBUG") { "debug" } else { "release" };
    let mut features = String::new();
    for (name, suffix) in [("V8_ENABLE_POINTER_COMPRESSION", "_ptrcomp"),
                            ("V8_ENABLE_SANDBOX", "_sandbox"), ("SIMDUTF", "_simdutf")] {
      if env::var_os(format!("CARGO_FEATURE_{name}")).is_some() { features.push_str(suffix); }
    }
    let path = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest directory"))
      .join("gen").join(format!("src_binding{features}_{profile}_{target}.rs"));
    assert!(path.is_file(), "documentation binding missing: {path:?}");
    path
  } else {
    local_input("RUSTY_V8_SRC_BINDING_PATH")
  };
  println!("cargo:rustc-env=RUSTY_V8_SRC_BINDING_PATH={}", binding.display());
  if docs { return; }
  if env::var_os("DENO_TRYBUILD").is_none() {
    let archive = local_input("RUSTY_V8_ARCHIVE");
    let expected = if env::var("CARGO_CFG_TARGET_OS").unwrap_or_default() == "windows" {
      "rusty_v8.lib"
    } else { "librusty_v8.a" };
    assert_eq!(archive.file_name().and_then(|name| name.to_str()), Some(expected),
               "materialised V8 archive must use its canonical library filename");
    // Link the content-addressed component directly; no repeated 130MiB copy,
    // shared gn_out directory, downloader, bindgen or target-cache artifact.
    println!("cargo:rustc-link-search=native={}", archive.parent().unwrap().display());
  }
  print_link_flags();
}

fn print_link_flags() {
  println!("cargo:rustc-link-lib=static=rusty_v8");
  let should_dyn_link_libcxx = env::var("CARGO_FEATURE_USE_CUSTOM_LIBCXX")
    .is_err()
    || env::var("GN_ARGS").is_ok_and(|gn_args| {
      gn_args
        .split_whitespace()
        .any(|ba| ba == "use_custom_libcxx=false")
    });

  if should_dyn_link_libcxx {
    // Based on https://github.com/alexcrichton/cc-rs/blob/fba7feded71ee4f63cfe885673ead6d7b4f2f454/src/lib.rs#L2462
    if let Ok(stdlib) = env::var("CXXSTDLIB") {
      if !stdlib.is_empty() {
        println!("cargo:rustc-link-lib=dylib={stdlib}");
      }
    } else {
      let target = env::var("TARGET").unwrap();
      if target.contains("msvc") {
        // nothing to link to
      } else if target.contains("apple")
        || target.contains("freebsd")
        || target.contains("openbsd")
      {
        println!("cargo:rustc-link-lib=dylib=c++");
      } else if target.contains("android") {
        println!("cargo:rustc-link-lib=dylib=c++_shared");
      } else {
        println!("cargo:rustc-link-lib=dylib=stdc++");
      }
    }
  }
  let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap();
  let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap();

  if target_os == "windows" {
    println!("cargo:rustc-link-lib=dylib=winmm");
    println!("cargo:rustc-link-lib=dylib=dbghelp");
  }

  if target_env == "msvc" {
    // On Windows, including libcpmt[d]/msvcprt[d] explicitly links the C++
    // standard library, which libc++ needs for exception_ptr internals.
    let crt_static = env::var("CARGO_CFG_TARGET_FEATURE")
      .unwrap_or_default()
      .contains("crt-static");
    if crt_static {
      println!("cargo:rustc-link-lib=libcpmt");
    } else {
      println!("cargo:rustc-link-lib=dylib=msvcprt");
    }
  }
}


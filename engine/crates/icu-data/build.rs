//! Filters ICU's common data into `$OUT_DIR/icudtl.dat`.
//!
//! The full data comes from `deno_core_icudata`, the file deno_core would hand V8 itself; what is written is what
//! `policy::keep` leaves in it. Nothing is downloaded and nothing is committed: the filter is code, the input is a
//! pinned crate, and the result is reproducible from both.

#[path = "src/package.rs"]
mod package;
#[path = "src/policy.rs"]
mod policy;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/package.rs");
    println!("cargo:rerun-if-changed=src/policy.rs");

    let full = package::Package::parse(deno_core_icudata::ICU_DATA)
        .expect("deno_core_icudata is an ICU package");
    let filtered = full.filtered(policy::keep);
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("cargo sets OUT_DIR"))
        .join("icudtl.dat");
    std::fs::write(&out, filtered).expect("write the filtered ICU data");
}

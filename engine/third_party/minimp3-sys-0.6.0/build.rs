extern crate cc;

fn main() {
    println!("cargo:rerun-if-changed=minimp3.c");
    println!("cargo:rerun-if-changed=migo_presentation.h");
    println!("cargo:rerun-if-changed=minimp3/minimp3.h");
    println!("cargo:rerun-if-changed=minimp3/minimp3_ex.h");
    cc::Build::new()
        .include("minimp3/")
        .file("minimp3.c")
        .define("MINIMP3_IMPLEMENTATION", None)
        .compile("minimp3");
}

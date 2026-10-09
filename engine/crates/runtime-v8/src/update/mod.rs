//! App update extension for update checking and applying.

use deno_core::{Extension, extension};

extension!(host_v8_update,
    esm_entry_point = "ext:host_v8_update/99_global_scope.js",
    esm = [
        dir "src/update",
        "02_update_mgr.js",
        "99_global_scope.js",
    ]
);

pub fn update_extensions() -> Vec<Extension> {
    vec![host_v8_update::init()]
}

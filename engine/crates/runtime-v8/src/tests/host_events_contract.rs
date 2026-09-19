//! Every host event the external session sends names a hook the embedded
//! bindings call.
//!
//! `contracts/runtime/host-events.json` numbers the host-bridge functions a
//! host's input reaches; the producer calls them by those names on the same
//! engine JavaScript this crate evaluates. A name the bindings do not resolve
//! is an event the producer would call and the embedded runtime never would --
//! two behaviours for one touch -- so each must appear as a hook
//! `js_bindings.rs` looks up on the bridge.

use deno_core::serde_json;

#[test]
fn every_host_event_is_a_hook_the_embedded_bindings_resolve() {
    let contract: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../contracts/runtime/host-events.json"
    ))
    .expect("the contract is JSON");
    let bindings = include_str!("../js_bindings.rs");
    let unresolved: Vec<&String> = contract["events"]
        .as_object()
        .expect("an events object")
        .keys()
        .filter(|hook| !bindings.contains(&format!("\"{hook}\"")))
        .collect();
    assert!(
        unresolved.is_empty(),
        "host events the embedded bindings never resolve: {unresolved:?}"
    );
}

//! The namespace lists what it publishes, as the platform's own namespace does.
//!
//! `migo` mirrors the engine's globals, and most of those are defined the way a browser defines
//! its window's: not enumerable. Copied with that attribute, 422 of the 452 names `migo`
//! published were invisible to `Object.keys(migo)` and `for...in` -- and to the `wx` an adapter
//! mirrors from it. The platform's namespace is a plain object whose every API is enumerable, and
//! content enumerates it: Cocos Creator 3 builds its own environment from `Object.keys(wx)`, so
//! its `minigame.onHide` was undefined and its start-up threw.

#[cfg(test)]
mod namespace_members_tests {
    use super::super::support::{boot, is_true, test_host_state};

    #[test]
    fn every_name_the_namespace_publishes_is_enumerable() {
        let mut runtime = boot(test_host_state());
        assert!(
            is_true(
                &mut runtime,
                "Object.getOwnPropertyNames(migo).length > 100"
            ),
            "the namespace publishes its API"
        );
        assert!(
            is_true(
                &mut runtime,
                "Object.getOwnPropertyNames(migo).every(function (k) {
                     return Object.getOwnPropertyDescriptor(migo, k).enumerable;
                 })"
            ),
            "a name the namespace publishes is hidden from Object.keys"
        );
        assert!(
            is_true(
                &mut runtime,
                "(function () {
                     var n = 0;
                     for (var k in migo) n++;
                     return Object.keys(migo).indexOf('onHide') >= 0 && n === Object.getOwnPropertyNames(migo).length;
                 })()"
            ),
            "for...in and Object.keys do not list the whole namespace"
        );
    }

    #[test]
    fn each_name_is_otherwise_the_global_it_mirrors() {
        let mut runtime = boot(test_host_state());
        assert!(
            is_true(
                &mut runtime,
                "Object.getOwnPropertyNames(migo).every(function (k) {
                     var m = Object.getOwnPropertyDescriptor(migo, k), g = Object.getOwnPropertyDescriptor(globalThis, k);
                     return g !== undefined && m.value === g.value && m.get === g.get && m.set === g.set
                         && m.writable === g.writable && m.configurable === g.configurable;
                 })"
            ),
            "a namespace member differs from the global it mirrors in more than enumerability"
        );
    }
}

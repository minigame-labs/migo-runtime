//! Core codec prerequisites for bounded Worker messages. Transport is separate.
use deno_core::{JsRuntime, RuntimeOptions};

deno_core::extension!(
    bounded_clone_test,
    esm_entry_point = "ext:bounded_clone_test/bridge.js",
    esm = ["ext:bounded_clone_test/bridge.js" = {
        source = r#"
        import { core } from "ext:core/mod.js";
        const cloneError = (message) => {
            const error = new Error(message);
            error.name = 'DataCloneError';
            throw error;
        };
        globalThis.__serialize = (value, options) => core.serialize(value, options, cloneError);
        globalThis.__clone = (value, maxBytes = 16 * 1024 * 1024) => core.deserialize(
            __serialize(value, {forStorage: true, maxBytes}), {forStorage: true});
        globalThis.check = (condition, message) => { if (!condition) throw new Error(message); };
    "#
    }],
);

fn runtime() -> JsRuntime {
    crate::install_icu_data();
    JsRuntime::new(RuntimeOptions {
        extensions: vec![bounded_clone_test::init()],
        ..Default::default()
    })
}

#[test]
fn ordinary_clone_values_preserve_types_cycles_and_shared_views() {
    let executor = super::support::v8_test_executor();
    let _enter = executor.as_ref().map(|executor| executor.enter());
    runtime().execute_script("clone-value-matrix", r#"
        const buffer = new ArrayBuffer(32);
        new Uint8Array(buffer)[7] = 42;
        const original = {
            number: NaN, negativeZero: -0, missing: undefined, bigint: 12345678901234567890n,
            date: new Date('2024-01-02T03:04:05Z'), re: /game/giu,
            map: new Map(), set: new Set(), buffer,
            view: new Uint8Array(buffer, 7, 4), dataView: new DataView(buffer, 3, 12),
        };
        original.self = original;
        original.map.set(original, original.view);
        original.set.add(original);
        const result = __clone(original);
        check(result !== original && result.self === result, 'cycle/identity');
        check(Number.isNaN(result.number) && Object.is(result.negativeZero, -0), 'numbers');
        check('missing' in result && result.missing === undefined && result.bigint === original.bigint, 'primitives');
        check(result.date instanceof Date && +result.date === +original.date, 'Date');
        check(result.re instanceof RegExp && result.re.source === 'game' && result.re.flags === 'giu', 'RegExp');
        check(result.map instanceof Map && result.map.get(result) === result.view, 'Map');
        check(result.set instanceof Set && result.set.has(result), 'Set');
        check(result.buffer !== buffer && buffer.byteLength === 32, 'copy contract');
        check(result.view.buffer === result.buffer && result.view.byteOffset === 7 && result.view[0] === 42, 'typed view');
        check(result.dataView.buffer === result.buffer && result.dataView.byteOffset === 3, 'DataView');
        new Uint8Array(buffer)[7] = 99;
        check(result.view[0] === 42, 'clone still aliases caller memory');
        check(__clone(undefined) === undefined && Number.isNaN(__clone(NaN)), 'top-level values');
    "#).unwrap();
}

#[test]
fn unsupported_values_fail_without_detaching_or_poisoning_the_runtime() {
    let executor = super::support::v8_test_executor();
    let _enter = executor.as_ref().map(|executor| executor.enter());
    runtime().execute_script("clone-error-matrix", r#"
        const module = new WebAssembly.Module(new Uint8Array([0,97,115,109,1,0,0,0]));
        const values = [() => {}, Symbol('x'), new WeakMap(), new Proxy({}, {}),
                        new SharedArrayBuffer(32), module];
        for (let index = 0; index < values.length; index++) {
            const value = values[index];
            let caught;
            try { __clone({value}); } catch (error) { caught = error; }
            check(caught?.name === 'DataCloneError', 'unsupported value ' + index + ': ' + caught);
        }
        const buffer = new ArrayBuffer(32);
        let refused = false;
        try { __serialize(buffer, {forStorage: true, maxBytes: 4096, transferredArrayBuffers: [buffer]}); }
        catch (error) { refused = true; }
        check(refused && buffer.byteLength === 32, 'failed send detached caller buffer');
        check(__clone({after: true}).after === true, 'codec did not recover');
    "#).unwrap();
}

#[test]
fn a_large_clone_is_refused_by_the_allocator_before_posthoc_length_checks() {
    let executor = super::support::v8_test_executor();
    let _enter = executor.as_ref().map(|executor| executor.enter());
    runtime().execute_script("clone-allocation-limit", r#"
        const source = new Uint8Array(8 * 1024 * 1024);
        let caught;
        try { __clone({prefix: 'grow the initial allocation', source}, 1024); }
        catch (error) { caught = error; }
        // An unbounded serializer followed by a length check throws RangeError.
        // The delegate's pre-allocation refusal produces V8's DataCloneError.
        check(caught?.name === 'DataCloneError', 'allocation was not bounded: ' + caught);
        check(source.byteLength === 8 * 1024 * 1024, 'source buffer was detached');
        for (let i = 0; i < 100; i++) check(__clone({i}, 1024).i === i, 'recovery after allocation failure');
    "#).unwrap();
}

#[test]
fn encoded_byte_limit_is_inclusive_and_invalid_options_fail_closed() {
    let executor = super::support::v8_test_executor();
    let _enter = executor.as_ref().map(|executor| executor.enter());
    runtime()
        .execute_script(
            "clone-byte-budget",
            r#"
        const value = {buffer: new Uint8Array(1024), text: 'game'};
        const exact = __serialize(value, {forStorage: true}).byteLength;
        check(__clone(value, exact).buffer.length === 1024, 'exact encoded limit rejected');
        let caught;
        try { __clone(value, exact - 1); } catch (error) { caught = error; }
        check(caught instanceof RangeError, 'limit plus one accepted');
        for (const maxBytes of [0, -1, 1.5, NaN, Infinity, 2 ** 31, '1024']) {
            let invalid;
            try { __clone(0, maxBytes); } catch (error) { invalid = error; }
            check(invalid instanceof RangeError, 'invalid bound accepted: ' + maxBytes);
        }
        let unsafeMode;
        try { __serialize(value, {maxBytes: 4096}); } catch (error) { unsafeMode = error; }
        check(unsafeMode instanceof TypeError, 'out-of-band stores allowed in bounded mode');
    "#,
        )
        .unwrap();
}

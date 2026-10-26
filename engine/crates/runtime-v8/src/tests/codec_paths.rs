use deno_core::{JsRuntime, RuntimeOptions, extension};

extension!(
    codec_test,
    deps = [host_v8_utility],
    esm_entry_point = "ext:codec_test/setup.js",
    esm = ["ext:codec_test/setup.js" = {
        source = r#"
        import 'ext:host_v8_console/01_console.js';
        import 'ext:host_v8_console/01_alert.js';
        import { encode, decode } from 'ext:host_v8_utility/01_codec.js';
        import { op_decode_multi_formats } from 'ext:core/ops';
        globalThis.encode = encode; globalThis.decode = decode;
        globalThis.legacyDecode = op_decode_multi_formats;
        globalThis.check = (value, message) => { if (!value) throw new Error(message); };
    "#
    }],
);
fn runtime() -> JsRuntime {
    crate::icu::install_icu_data();
    let mut extensions = crate::console::console_extensions();
    extensions.extend(crate::utility::utility_extensions());
    extensions.push(codec_test::init());
    let mut rt = JsRuntime::new(RuntimeOptions {
        extensions,
        ..Default::default()
    });
    rt.op_state()
        .borrow_mut()
        .put(super::support::test_host_state());
    rt
}

#[test]
fn utf8_matches_the_previous_codec_including_invalid_bytes_and_bom() {
    let executor = super::support::v8_test_executor();
    let _enter = executor.as_ref().map(|rt| rt.enter());
    let mut rt = runtime();
    rt.execute_script("utf8-agreement", r#"
        const bytes = new Uint8Array(2);
        for (let first = 0; first < 256; first++) for (let second = 0; second < 256; second++) {
            bytes[0] = first; bytes[1] = second;
            check(decode({data:bytes}) === legacyDecode(bytes, 'utf8'), 'UTF-8 replacement changed');
        }
        for (const values of [[239,187,191,65],[240,159,167,169],[237,160,128],[240,128,128,128],[226,130]]) {
            const bytes = new Uint8Array(values);
            check(decode({data:bytes}) === legacyDecode(bytes,'utf8'), 'UTF-8 BOM/truncation changed');
        }
        check(decode({data:new Uint8Array([239,187,191,65])}) === '\uFEFFA', 'preserve existing BOM contract');
        check(decode({data:encode({data:'\uD800'})}) === '\uFFFD', 'encode lone surrogate');
        check(decode({data:new Uint8Array([65,0,66,0]),format:'utf16le'}) === 'AB', 'UTF-16 routing');
        check(decode({data:encode({data:'4142',format:'hex'})}) === 'AB', 'hex routing');
        check(decode({data:encode({data:'QUI=',format:'base64'})}) === 'AB', 'base64 routing');
    "#).unwrap();
}

#[test]
fn views_use_internal_slots_and_reject_objects_that_only_claim_a_buffer() {
    let executor = super::support::v8_test_executor();
    let _enter = executor.as_ref().map(|rt| rt.enter());
    let mut rt = runtime();
    rt.execute_script("codec-view-slots", r#"
        const backing = new Uint8Array([0,65,66,67,0]);
        const view = new DataView(backing.buffer, 1, 3);
        for (const name of ['buffer','byteOffset','byteLength']) Object.defineProperty(view, name, {get() {throw new Error('shadowed '+name);}});
        check(decode({data:view}) === 'ABC', 'DataView window');
        let rejected = false;
        try { decode({data:{buffer:new ArrayBuffer(8),byteOffset:0,byteLength:8}}); } catch (e) {rejected = e instanceof TypeError;}
        check(rejected, 'non-view object accepted as bytes');
    "#).unwrap();
}

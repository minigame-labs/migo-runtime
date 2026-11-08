//! `canvas.toTempFilePath` / `toTempFilePathSync`: what they refuse, and how
//! each answers. The pixels themselves are the renderer's and are checked
//! against a real one (the Linux player); here the renderer only answers the
//! canvas queries the JavaScript half waits on.

use deno_core::{FastString, JsRuntime, RuntimeOptions};
use shared::protocol::render_cmd::{CanvasCmd, RenderCommand};

use super::support::run_to_idle;

fn boot() -> (JsRuntime, std::thread::JoinHandle<()>) {
    let (render_tx, render_rx) = shared::render_command_sender::CommandSender::new();
    let renderer = std::thread::spawn(move || {
        while let Ok(command) = render_rx.recv_timeout(std::time::Duration::from_secs(5)) {
            match command {
                RenderCommand::Canvas(CanvasCmd::GetInfo { resp, .. }) => resp.ok((64, 32)),
                RenderCommand::Canvas(CanvasCmd::DestroyCanvas { resp, .. }) => resp.ok(()),
                _ => {}
            }
        }
    });
    let mut state = super::support::test_host_state();
    state.render_tx = render_tx;
    let mut runtime = JsRuntime::new(RuntimeOptions {
        extensions: crate::main_extensions(state),
        ..Default::default()
    });
    crate::harden_global_scope(&mut runtime);
    runtime
        .execute_script(
            "<test:canvas>",
            FastString::from_static("globalThis.__canvas = migo.createCanvas();"),
        )
        .expect("a canvas");
    (runtime, renderer)
}

/// A file of no size, or of an empty region, is refused before anything is read: the sync form throws its errMsg,
/// the callback form fails with it, and neither has a promise form.
#[test]
fn an_empty_file_or_region_is_refused_in_the_api_s_words() {
    let v8_executor = crate::tests::support::v8_test_executor();
    let _v8_enter = v8_executor.as_ref().map(|executor| executor.enter());
    let (mut runtime, renderer) = boot();
    let checked = run_to_idle(
        &mut runtime,
        r#"
        globalThis.__out = [];
        for (const options of [{ destWidth: 0 }, { width: 0 }, { height: -1 }, { x: NaN }]) {
            try {
                __canvas.toTempFilePathSync(options);
                __out.push('wrote');
            } catch (e) {
                __out.push(e.message);
            }
        }
        const returned = __canvas.toTempFilePath({
            destHeight: 0,
            fail: function (res) { __out.push(res.errMsg); },
            complete: function () { __out.push('complete'); },
        });
        __out.push(typeof returned);
        "#,
        "JSON.stringify(__out)",
    );
    drop(runtime);
    renderer.join().unwrap();
    assert_eq!(
        checked,
        concat!(
            r#"["toTempFilePathSync:fail destWidth and destHeight must be positive","#,
            r#""toTempFilePathSync:fail the region is empty","toTempFilePathSync:fail the region is empty","#,
            r#""toTempFilePathSync:fail the region is empty","undefined","#,
            r#""toTempFilePath:fail destWidth and destHeight must be positive","complete"]"#
        )
    );
}

#[test]
fn canvas_exports_use_the_native_encoder() {
    let v8_executor = crate::tests::support::v8_test_executor();
    let _v8_enter = v8_executor.as_ref().map(|executor| executor.enter());
    let (mut runtime, renderer) = boot();
    let checked = run_to_idle(
        &mut runtime,
        r#"
        __canvas.width = 1;
        __canvas.height = 1;
        const backing = new Uint8Array([99, 99, 200, 100, 50, 128, 99]);
        __canvas._readAllPixels = () => backing.subarray(2, 6);
        const png = __canvas._encode('image/png');
        const jpeg = __canvas._encode('image/jpeg', 1);
        const url = __canvas.toDataURL('image/unknown');
        globalThis.__nativeUrl = url;
        // pHYs/sRGB and 96 dpi JFIF identify the native path; the external JS
        // fallback has neither metadata chunk. The borrowed view must keep its
        // offset and length through the op rather than exposing its backing.
        const pngText = Array.from(png.bytes, c => String.fromCharCode(c)).join('');
        globalThis.__encoded = [png.mime, jpeg.mime, pngText.includes('pHYs'),
          pngText.includes('sRGB'), Array.from(jpeg.bytes.slice(13, 18)).join(','),
          url.startsWith('data:image/png;base64,'), backing.join(',')];
        let rejected = false;
        __canvas._readAllPixels = () => new Uint8Array(3);
        try { __canvas.toDataURL(); } catch (e) { rejected = e.message.includes('InvalidArgument'); }
        __encoded.push(rejected);
        __canvas.width = 0;
        __encoded.push(__canvas.toDataURL());
        "#,
        "JSON.stringify(__encoded)",
    );
    let url = run_to_idle(&mut runtime, "", "__nativeUrl");
    drop(runtime);
    renderer.join().unwrap();
    assert_eq!(
        checked,
        r#"["image/png","image/jpeg",true,true,"1,0,96,0,96",true,"99,99,200,100,50,128,99",true,"data:,"]"#
    );
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(url.strip_prefix("data:image/png;base64,").unwrap())
        .unwrap();
    let expected =
        migo_io::image_ops::encode_canvas_image(&[200, 100, 50, 128], 1, 1, false, 92).unwrap();
    assert_eq!(
        bytes, expected,
        "native encoding must borrow only the view's pixels"
    );
}

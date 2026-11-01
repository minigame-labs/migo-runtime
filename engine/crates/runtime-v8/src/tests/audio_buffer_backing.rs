//! Real public JS construction reaches the owned native backing and keeps
//! physical credit while channel views or native backing-store clones survive.
use super::support;
use deno_core::{FastString, JsRuntime, RuntimeOptions, v8};
use shared::{
    audio_physical::AudioAggregateLedger,
    audio_resources::{AudioBufferKey, AudioResourceRegistry},
    channel::ThreadWakeup,
    op_state::{AudioHostStartSignal, AudioSender},
};
use std::sync::Arc;
const BYTES: usize = 4 << 20;
fn boot(ledger: Arc<AudioAggregateLedger>) -> (JsRuntime, AudioResourceRegistry) {
    let registry = AudioResourceRegistry::with_physical_ledger(ledger);
    let (commands, _receiver) = shared::audio_channel::channel();
    let mut host = support::test_host_state();
    host.audio_tx = AudioSender::hosted(
        commands,
        ThreadWakeup::new(),
        AudioHostStartSignal::new(),
        registry.clone(),
    );
    let mut extensions = crate::main_extensions(host);
    // A declared runtime-JS counterfactual uses the actual public class with
    // its old uncharged allocator, without altering production files.
    if let Some(mode) = std::env::var_os("MIGO_INITIAL_BACKING_JS_FAULT") {
        let mut changed = 0;
        for ext in &mut extensions {
            if ext.name != "host_v8_audio" {
                continue;
            }
            for file in ext.esm_files.to_mut() {
                if file.specifier != "ext:host_v8_audio/00_audio_buffer.js" {
                    continue;
                }
                let source = include_str!("../audio/00_audio_buffer.js");
                let old = "const backing = op_audio_materialize_buffer(id);";
                assert_eq!(source.matches(old).count(), 1);
                let mutant = if mode == "unowned" {
                    source.replace(old,"const backing = new ArrayBuffer(checkedPcmBytes(numberOfChannels, length));")
                } else if mode == "abort" {
                    assert_eq!(source.matches("op_audio_abort_buffer(id);").count(), 1);
                    source.replace("op_audio_abort_buffer(id);", "")
                } else {
                    panic!("unknown declared JS counterfactual")
                };
                *file =
                    deno_core::ExtensionFileSource::new_computed(file.specifier, Arc::from(mutant));
                changed += 1;
            }
        }
        assert_eq!(
            changed, 1,
            "runtime JS counterfactual failed to replace constructor allocator"
        );
    }
    (
        JsRuntime::new(RuntimeOptions {
            extensions,
            ..Default::default()
        }),
        registry,
    )
}
fn run(rt: &mut JsRuntime, source: &'static str) {
    rt.execute_script(
        "<test:initial-audio-backing>",
        FastString::from_static(source),
    )
    .expect("public AudioBuffer constructor assertions");
}
fn store(rt: &mut JsRuntime, script: &'static str) -> v8::SharedRef<v8::BackingStore> {
    let value = rt
        .execute_script(
            "<test:initial-audio-store>",
            FastString::from_static(script),
        )
        .unwrap();
    deno_core::scope!(scope, rt);
    let local = v8::Local::new(scope, value);
    v8::Local::<v8::ArrayBuffer>::try_from(local)
        .unwrap()
        .get_backing_store()
}
#[test]
fn constructor_views_keep_physical_credit_after_logical_release() {
    let executor = support::v8_test_executor();
    let _guard = executor.as_ref().map(|e| e.enter());
    for detach in [false, true] {
        let ledger = Arc::new(AudioAggregateLedger::new(BYTES));
        let (mut rt, registry) = boot(ledger.clone());
        run(
            &mut rt,
            r#"
          (() => {
            const b=new AudioBuffer({numberOfChannels:2,length:524288,sampleRate:48000});
            if(b.length!==524288||b.numberOfChannels!==2||b.sampleRate!==48000||b.duration!==524288/48000)throw new Error("initial metadata changed");
            const a=b.getChannelData(0),c=b.getChannelData(1);
            if(a!==b.getChannelData(0)||a.buffer!==c.buffer||a.byteOffset!==0||c.byteOffset!==2097152||a.buffer.byteLength!==4194304)throw new Error("initial channel view identity changed");
            for(let i=0;i<a.length;i++)if(a[i]!==0||c[i]!==0||Object.is(a[i],-0)||Object.is(c[i],-0))throw new Error("initial PCM is not fully positive zero");
            a[0]=-0;a[a.length-1]=0.75;c[0]=-0.25;c[c.length-1]=0.5;
            const copy=new Float32Array(2);b.copyFromChannel(copy,1);if(copy[0]!==-0.25||copy[1]!==0)throw new Error("copyFromChannel changed");
            b.copyToChannel(new Float32Array([0.125,-0.5]),0,1);
            globalThis.__initialViews=[a,c];globalThis.__initialId=b._id;
          })();
        "#,
        );
        assert_eq!(
            ledger.used_bytes(),
            BYTES,
            "initial JS constructor bypassed physical admission"
        );
        // Invoke the same idempotent release the AudioBuffer finalizer invokes;
        // no AudioBuffer object is retained by the test, only its channel views.
        registry.release_buffer(AudioBufferKey {
            runtime_generation: 1,
            serial: 1,
        });
        run(
            &mut rt,
            r#"
          if(!Object.is(__initialViews[0][0],-0)||__initialViews[0][1]!==0.125||__initialViews[0][2]!==-0.5||__initialViews[0][524287]!==0.75||__initialViews[1][0]!==-0.25||__initialViews[1][524287]!==0.5)throw new Error("retained initial channel PCM changed");
        "#,
        );
        assert_eq!(
            ledger.used_bytes(),
            BYTES,
            "AudioBuffer release returned live initial channel view credit"
        );
        assert!(
            ledger.try_reserve(1, "peer host").is_err(),
            "initial channel views reopened physical admission"
        );
        let retained = store(&mut rt, "__initialViews[0].buffer");
        if detach {
            let value = rt
                .execute_script(
                    "<test:initial-detach>",
                    FastString::from_static("__initialViews[0].buffer"),
                )
                .unwrap();
            deno_core::scope!(scope, rt);
            let value = v8::Local::new(scope, value);
            let buffer = v8::Local::<v8::ArrayBuffer>::try_from(value).unwrap();
            assert_eq!(buffer.detach(None), Some(true));
        }
        drop(registry);
        drop(rt);
        assert_eq!(
            ledger.used_bytes(),
            BYTES,
            "initial isolate drop returned retained backing-store credit"
        );
        let clone = retained.clone();
        drop(retained);
        assert_eq!(
            ledger.used_bytes(),
            BYTES,
            "initial backing clone returned physical credit"
        );
        drop(clone);
        assert_eq!(
            ledger.used_bytes(),
            0,
            "initial V8 last owner leaked physical credit"
        );
    }
}
#[test]
fn physical_refusal_aborts_initial_reservation_and_allows_retry() {
    let executor = support::v8_test_executor();
    let _guard = executor.as_ref().map(|e| e.enter());
    let ledger = Arc::new(AudioAggregateLedger::new(BYTES));
    let blocker = ledger.try_reserve(BYTES, "other audio domain").unwrap();
    let (mut rt, registry) = boot(ledger.clone());
    let result = rt.execute_script(
        "<test:initial-refusal>",
        FastString::from_static(
            "new AudioBuffer({numberOfChannels:2,length:524288,sampleRate:48000})",
        ),
    );
    assert!(
        result.is_err(),
        "initial constructor ignored physical saturation"
    );
    assert_eq!(
        registry.format(AudioBufferKey {
            runtime_generation: 1,
            serial: 1
        }),
        None,
        "failed JS constructor retained logical reservation"
    );
    assert_eq!(ledger.used_bytes(), BYTES);
    drop(blocker);
    run(
        &mut rt,
        r#"globalThis.__retry=new AudioBuffer({numberOfChannels:2,length:524288,sampleRate:48000});if(__retry._id!==2||__retry.getChannelData(0)[0]!==0)throw new Error("initial constructor retry failed");"#,
    );
    assert_eq!(ledger.used_bytes(), BYTES);
    drop(rt);
    drop(registry);
    assert_eq!(
        ledger.used_bytes(),
        0,
        "initial retry leaked physical credit"
    );
}
#[test]
fn channel_view_setup_failure_aborts_the_native_entry() {
    let executor = support::v8_test_executor();
    let _guard = executor.as_ref().map(|e| e.enter());
    let ledger = Arc::new(AudioAggregateLedger::new(BYTES));
    let (mut rt, registry) = boot(ledger.clone());
    run(
        &mut rt,
        r#"
      (() => {
        const Original=globalThis.Float32Array;let reached=false,caught=false;
        globalThis.Float32Array=new Proxy(Original,{construct(){reached=true;throw new Error("injected channel view setup");}});
        try {new AudioBuffer({numberOfChannels:2,length:524288,sampleRate:48000});}
        catch(error){caught=String(error).includes("injected channel view setup");}
        finally{globalThis.Float32Array=Original;}
        if(!reached||!caught)throw new Error("channel setup failure did not reach initialized native backing");
      })();
    "#,
    );
    assert_eq!(
        registry.format(AudioBufferKey {
            runtime_generation: 1,
            serial: 1
        }),
        None,
        "failed channel view installation retained native entry"
    );
    drop(rt);
    drop(registry);
    assert_eq!(
        ledger.used_bytes(),
        0,
        "failed channel view installation leaked actual native credit"
    );
}

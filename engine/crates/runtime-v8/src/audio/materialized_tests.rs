//! The real V8 backing owns credit beyond logical release, detach and isolate drop.
use super::*;
use deno_core::ToV8;
use shared::{audio_physical::AudioAggregateLedger, audio_resources::AudioBufferFormat};
use std::sync::Arc;
#[test]
fn materialized_pcm_credit_follows_actual_v8_backing() {
    crate::install_icu_data();
    let executor = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let _guard = executor.enter();
    for detach in [false, true] {
        let bytes = 4 << 20;
        let ledger = Arc::new(AudioAggregateLedger::new(bytes * 2));
        let registry = AudioResourceRegistry::with_physical_ledger(ledger.clone());
        let count = bytes / 4;
        let data: Vec<f32> = (0..count)
            .map(|i| {
                if i == 0 {
                    -0.0
                } else {
                    (i % 31) as f32 / 32.0 - 0.5
                }
            })
            .collect();
        let format = AudioBufferFormat {
            sample_rate: 48000,
            channels: 1,
            frames: count as u32,
        };
        let key = registry.adopt_frozen(77, format, data).unwrap().key();
        let (commands, _) = shared::audio_channel::channel();
        let sender = AudioSender::hosted(
            commands,
            shared::channel::ThreadWakeup::new(),
            shared::op_state::AudioHostStartSignal::new(),
            registry.clone(),
        );
        let scope = BufferScope {
            tx: &sender,
            runtime_generation: 77,
        };
        let native = service::materialize_owned_buffer(scope, key.serial).unwrap();
        let pointer = native.samples().as_ptr();
        assert_eq!(
            ledger.used_bytes(),
            bytes,
            "materialized V8 source credit was released before backing adoption"
        );
        let mut runtime = deno_core::JsRuntime::new(deno_core::RuntimeOptions::default());
        let (view, store) = {
            deno_core::scope!(scope, runtime);
            let value = MaterializedAudioBacking(native).to_v8(scope).unwrap();
            let buffer = v8::Local::<v8::ArrayBuffer>::try_from(value).unwrap();
            let store = buffer.get_backing_store();
            assert_eq!(store.byte_length(), bytes);
            assert_eq!(
                store.data().unwrap().as_ptr() as *const f32,
                pointer,
                "V8 materialize copied original PCM"
            );
            let view = v8::Float32Array::new(scope, buffer, 0, count).unwrap();
            let global = v8::Global::new(scope, view);
            let cells = &*store;
            assert!(
                cells
                    .chunks_exact(4)
                    .enumerate()
                    .all(|(i, v)| f32::from_ne_bytes([
                        v[0].get(),
                        v[1].get(),
                        v[2].get(),
                        v[3].get()
                    ])
                    .to_bits()
                        == if i == 0 {
                            (-0.0_f32).to_bits()
                        } else {
                            ((i % 31) as f32 / 32.0 - 0.5).to_bits()
                        }),
                "V8 backing changed complete PCM"
            );
            if detach {
                assert_eq!(buffer.detach(None), Some(true));
            }
            (global, store)
        };
        registry.release_buffer(key);
        drop(sender);
        drop(registry);
        assert_eq!(
            ledger.used_bytes(),
            bytes,
            "logical release returned V8 backing physical credit"
        );
        assert!(
            ledger.try_reserve(bytes + 1, "peer host").is_err(),
            "V8 live backing reopened physical admission"
        );
        drop(view);
        drop(runtime);
        assert_eq!(
            ledger.used_bytes(),
            bytes,
            "isolate drop returned retained BackingStore credit"
        );
        let clone = store.clone();
        drop(store);
        assert_eq!(
            ledger.used_bytes(),
            bytes,
            "one BackingStore clone returned physical credit"
        );
        drop(clone);
        assert_eq!(
            ledger.used_bytes(),
            0,
            "final V8 BackingStore leaked physical credit"
        );
    }
}

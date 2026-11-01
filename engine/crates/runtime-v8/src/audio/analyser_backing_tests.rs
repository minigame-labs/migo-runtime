use super::*;
use deno_core::ToV8;
use shared::{
    audio_analyser_samples::{AnalyserSamples, AnalyserStorage},
    audio_physical::AudioAggregateLedger,
};
use std::sync::Arc;
struct Charged<T> {
    samples: Vec<T>,
    _permit: shared::audio_physical::AudioAggregatePermit,
}
impl<T: Send> AnalyserStorage<T> for Charged<T> {
    fn samples(&self) -> &[T] {
        &self.samples
    }
    fn samples_mut(&mut self) -> &mut [T] {
        &mut self.samples
    }
}
fn charged<T: Send + 'static>(
    ledger: &Arc<AudioAggregateLedger>,
    samples: Vec<T>,
) -> AnalyserSamples<T> {
    let permit = ledger
        .try_reserve(
            samples.capacity() * std::mem::size_of::<T>(),
            "analyser snapshot",
        )
        .unwrap();
    AnalyserSamples::new(Charged {
        samples,
        _permit: permit,
    })
}
#[test]
fn analyser_time_buffers_keep_original_storage_through_detach_and_isolate_drop() {
    crate::install_icu_data();
    let executor = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let _enter = executor.enter();
    for floats in [false, true] {
        for detach in [false, true] {
            let ledger = Arc::new(AudioAggregateLedger::new(1 << 20));
            let (data, want, pointer) = if floats {
                let samples: Vec<_> = (0..32768)
                    .map(|i| {
                        f32::from_bits(match i {
                            0 => 0x80000000,
                            1 => 0x7fc00003,
                            _ => (i as f32 * 0.125).to_bits(),
                        })
                    })
                    .collect();
                let want: Vec<_> = samples.iter().flat_map(|v| v.to_le_bytes()).collect();
                let pointer = samples.as_ptr() as *const u8;
                (
                    service::AnalyserData::Floats(charged(&ledger, samples)),
                    want,
                    pointer,
                )
            } else {
                let samples: Vec<_> = (0..32768).map(|i| (i % 251) as u8).collect();
                let want = samples.clone();
                let pointer = samples.as_ptr();
                (
                    service::AnalyserData::Bytes(charged(&ledger, samples)),
                    want,
                    pointer,
                )
            };
            let bytes = want.len();
            let mut runtime = deno_core::JsRuntime::new(deno_core::RuntimeOptions::default());
            let store = {
                deno_core::scope!(scope, runtime);
                let value = if std::env::var("MIGO_ANALYSER_BACKING_FAULT").as_deref() == Ok("copy")
                {
                    // The old buffer return copied native samples into an uncharged byte Vec.
                    let mut native = analyser_buffer(data);
                    let copied = native.as_mut().to_vec();
                    drop(native);
                    let backing = v8::ArrayBuffer::new_backing_store_from_boxed_slice(
                        copied.into_boxed_slice(),
                    )
                    .make_shared();
                    let buffer = v8::ArrayBuffer::with_backing_store(scope, &backing);
                    v8::Uint8Array::new(scope, buffer, 0, bytes).unwrap().into()
                } else {
                    analyser_buffer(data).to_v8(scope).unwrap()
                };
                let view = v8::Local::<v8::Uint8Array>::try_from(value)
                    .expect("analyser op changed Uint8Array return type");
                assert_eq!(view.length(), bytes);
                assert_eq!(view.byte_offset(), 0);
                let buffer = view.buffer(scope).unwrap();
                let store = buffer.get_backing_store();
                assert_eq!(
                    store.data().unwrap().as_ptr() as *const u8,
                    pointer,
                    "V8 analyser copied native snapshot"
                );
                assert!(
                    store
                        .iter()
                        .zip(want)
                        .all(|(cell, byte)| cell.get() == byte),
                    "V8 analyser changed complete LE bytes"
                );
                if detach {
                    assert_eq!(buffer.detach(None), Some(true));
                }
                store
            };
            assert_eq!(
                ledger.used_bytes(),
                bytes,
                "V8 analyser detached native credit"
            );
            drop(runtime);
            assert_eq!(
                ledger.used_bytes(),
                bytes,
                "isolate returned retained analyser credit"
            );
            let last = store.clone();
            drop(store);
            assert_eq!(ledger.used_bytes(), bytes);
            drop(last);
            assert_eq!(ledger.used_bytes(), 0);
        }
    }
}
#[test]
fn analyser_frequency_serde_keeps_the_owned_element_type_and_credit() {
    crate::install_icu_data();
    let executor = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let _enter = executor.enter();
    let ledger = Arc::new(AudioAggregateLedger::new(128));
    let mut runtime = deno_core::JsRuntime::new(deno_core::RuntimeOptions::default());
    deno_core::scope!(scope, runtime);
    let bytes = charged(&ledger, vec![0_u8, 128, 255]);
    let value = deno_core::serde_v8::to_v8(scope, &bytes).unwrap();
    let got: Vec<u8> = deno_core::serde_v8::from_v8(scope, value).unwrap();
    assert_eq!(got, [0, 128, 255]);
    assert_eq!(ledger.used_bytes(), 3);
    drop(bytes);
    assert_eq!(ledger.used_bytes(), 0);
    let floats = charged(&ledger, vec![-0.0_f32, -100.0, 3.5, f32::INFINITY]);
    let value = deno_core::serde_v8::to_v8(scope, &floats).unwrap();
    let got: Vec<f32> = deno_core::serde_v8::from_v8(scope, value).unwrap();
    assert_eq!(
        got.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
        floats.iter().map(|v| v.to_bits()).collect::<Vec<_>>()
    );
    assert_eq!(ledger.used_bytes(), 16);
    drop(floats);
    assert_eq!(ledger.used_bytes(), 0);
}

//! A cache eviction can release the final Arc on the audio thread.
//! Keep large PCM and its original physical permit together until CPU disposal.
use super::{AudioAggregatePermit, DecodedAudio};
use migo_io::audio_cpu::AudioCpu;
use std::ops::Deref;
use tokio::sync::oneshot;

pub(super) struct Value {
    data: Option<Data>,
    retired: Option<oneshot::Sender<Data>>,
}
struct Data {
    audio: DecodedAudio,
    _permit: Option<AudioAggregatePermit>,
    #[cfg(test)]
    _witness: Witness,
}
impl Value {
    pub(super) fn new(
        audio: DecodedAudio,
        permit: AudioAggregatePermit,
        cpu: Option<&AudioCpu>,
    ) -> Self {
        // Match the existing command payload small/large boundary. Capacity,
        // rather than logical length, measures the allocation being retained.
        let retired = cpu
            .filter(|_| audio.samples.capacity() >= 4096 / size_of::<f32>())
            .map(|cpu| {
                let (send, receive) = oneshot::channel::<Data>();
                let cpu = cpu.clone();
                crate::streaming::get_stream_runtime().spawn(async move {
                    if let Ok(value) = receive.await {
                        cpu.retirement().retire(value).await;
                    }
                });
                send
            });
        Self {
            data: Some(Data {
                audio,
                _permit: Some(permit),
                #[cfg(test)]
                _witness: Witness(OBSERVE.with(|slot| slot.borrow().clone())),
            }),
            retired,
        }
    }
}
impl Deref for Value {
    type Target = DecodedAudio;
    fn deref(&self) -> &DecodedAudio {
        &self.data.as_ref().unwrap().audio
    }
}
impl Drop for Value {
    fn drop(&mut self) {
        let Some(retired) = self.retired.take() else {
            return;
        };
        if retired.send(self.data.take().unwrap()).is_err() {
            unreachable!("process-lifetime cached PCM disposal receiver disappeared");
        }
    }
}
#[cfg(test)]
#[derive(Default)]
struct Witness(Option<std::sync::Arc<dyn Fn() + Send + Sync>>);
#[cfg(test)]
impl Drop for Witness {
    fn drop(&mut self) {
        if let Some(hook) = &self.0 {
            hook();
        }
    }
}
#[cfg(test)]
thread_local! {
    static OBSERVE: std::cell::RefCell<Option<std::sync::Arc<dyn Fn()+Send+Sync>>> = const {std::cell::RefCell::new(None)};
}
#[cfg(test)]
pub(super) fn observe<R>(hook: std::sync::Arc<dyn Fn() + Send + Sync>, f: impl FnOnce() -> R) -> R {
    struct Reset(Option<std::sync::Arc<dyn Fn() + Send + Sync>>);
    impl Drop for Reset {
        fn drop(&mut self) {
            OBSERVE.with(|slot| *slot.borrow_mut() = self.0.take());
        }
    }
    let _reset = Reset(OBSERVE.with(|slot| slot.replace(Some(hook))));
    f()
}

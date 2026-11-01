//! An owned player source can disappear on replacement, failure or player Drop.
//! Arm while the source is installed; its original PCM/permit retire together.
use crate::limits::AudioAggregatePermit;
use migo_io::audio_cpu::AudioCpu;
use tokio::sync::oneshot;

pub(super) struct Slot {
    send: oneshot::Sender<Data>,
    #[cfg(test)]
    hook: Option<std::sync::Arc<dyn Fn() + Send + Sync>>,
}
struct Data {
    _samples: Vec<f32>,
    _permit: Option<AudioAggregatePermit>,
    #[cfg(test)]
    _witness: Witness,
}
impl Slot {
    pub(super) fn new(cpu: AudioCpu) -> Self {
        let (send, receive) = oneshot::channel::<Data>();
        crate::streaming::get_stream_runtime().spawn(async move {
            if let Ok(value) = receive.await {
                cpu.retirement().retire(value).await;
            }
        });
        Self {
            send,
            #[cfg(test)]
            hook: OBSERVE.with(|slot| slot.borrow().clone()),
        }
    }
}
pub(super) fn dispose(samples: Vec<f32>, permit: Option<AudioAggregatePermit>, slot: Option<Slot>) {
    if samples.capacity() >= 4096 / size_of::<f32>()
        && let Some(slot) = slot
    {
        let value = Data {
            _samples: samples,
            _permit: permit,
            #[cfg(test)]
            _witness: Witness(slot.hook),
        };
        if slot.send.send(value).is_err() {
            unreachable!("process-lifetime owned-source disposal receiver disappeared");
        }
        return;
    }
    drop(samples);
    drop(permit);
}
#[cfg(test)]
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
thread_local! {static OBSERVE:std::cell::RefCell<Option<std::sync::Arc<dyn Fn()+Send+Sync>>>=const {std::cell::RefCell::new(None)};}
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

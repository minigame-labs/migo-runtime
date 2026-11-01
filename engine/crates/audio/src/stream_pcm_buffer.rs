//! Returned PCM used to free on audio when its pool was full or gone.
//! One pre-registered CPU disposal slot follows the same recyclable allocation.
use migo_io::audio_cpu::AudioCpu;
use std::ops::{Deref, DerefMut};
use tokio::sync::oneshot;

pub(super) struct Buffer {
    data: Option<Data>,
    retired: Option<oneshot::Sender<Data>>,
}
struct Data {
    buffer: Vec<f32>,
    _slot: Option<tokio::sync::OwnedSemaphorePermit>,
    #[cfg(test)]
    witness: Witness,
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
impl Default for Buffer {
    fn default() -> Self {
        Self::new(Vec::new(), None, None)
    }
}
impl Buffer {
    pub(super) fn new(
        buffer: Vec<f32>,
        cpu: Option<&AudioCpu>,
        slot: Option<tokio::sync::OwnedSemaphorePermit>,
    ) -> Self {
        let retired = cpu.map(|cpu| {
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
                buffer,
                _slot: slot,
                #[cfg(test)]
                witness: Witness::default(),
            }),
            retired,
        }
    }
    #[cfg(test)]
    pub(super) fn observe(&mut self, hook: std::sync::Arc<dyn Fn() + Send + Sync>) {
        self.data.as_mut().unwrap().witness.0 = Some(hook);
    }
}
impl Deref for Buffer {
    type Target = Vec<f32>;
    fn deref(&self) -> &Vec<f32> {
        &self.data.as_ref().unwrap().buffer
    }
}
impl DerefMut for Buffer {
    fn deref_mut(&mut self) -> &mut Vec<f32> {
        &mut self.data.as_mut().unwrap().buffer
    }
}
impl Drop for Buffer {
    fn drop(&mut self) {
        let Some(retired) = self.retired.take() else {
            return;
        };
        if retired.send(self.data.take().unwrap()).is_err() {
            unreachable!("process-lifetime PCM disposal receiver disappeared");
        }
    }
}

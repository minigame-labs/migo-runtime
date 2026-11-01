//! Waiting decode futures used to destroy encoded input on the async worker.
//! Their original pending permit bounds this drop-only CPU handoff too.
use super::State;
use migo_io::audio_cpu::AudioCpu;
use std::ops::{Deref, DerefMut};
use tokio::sync::mpsc;

pub(super) struct Retirement {
    cpu: AudioCpu,
    sender: Option<mpsc::Sender<Retired>>,
    #[cfg(test)]
    pub(super) hook: Option<std::sync::Arc<dyn Fn() + Send + Sync>>,
    #[cfg(test)]
    pub(super) queued: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}
struct Retired {
    state: Box<State>,
    #[cfg(test)]
    witness: Witness,
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
impl Retirement {
    pub(super) fn new(cpu: AudioCpu) -> Self {
        Self {
            cpu,
            sender: None,
            #[cfg(test)]
            hook: None,
            #[cfg(test)]
            queued: Default::default(),
        }
    }
    pub(super) fn own(&mut self, state: State) -> Owner {
        let sender = self.sender.get_or_insert_with(|| {
            let (sender, incoming) = mpsc::channel(super::DECODE_JOB_QUEUE_CAPACITY);
            crate::streaming::get_stream_runtime().spawn(retire(incoming, self.cpu.clone()));
            sender
        });
        Owner {
            value: Some(Retired {
                state: Box::new(state),
                #[cfg(test)]
                witness: Witness(self.hook.clone()),
            }),
            sender: sender.clone(),
            #[cfg(test)]
            queued: self.queued.clone(),
        }
    }
}
pub(super) struct Owner {
    value: Option<Retired>,
    sender: mpsc::Sender<Retired>,
    #[cfg(test)]
    queued: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}
impl Owner {
    pub(super) fn into_box(mut self) -> Box<State> {
        let value = self.value.take().unwrap();
        #[cfg(test)]
        let value = {
            let mut value = value;
            value.witness.0 = None;
            value
        };
        value.state
    }
}
impl Deref for Owner {
    type Target = State;
    fn deref(&self) -> &State {
        &self.value.as_ref().unwrap().state
    }
}
impl DerefMut for Owner {
    fn deref_mut(&mut self) -> &mut State {
        &mut self.value.as_mut().unwrap().state
    }
}
impl Drop for Owner {
    fn drop(&mut self) {
        if let Some(value) = self.value.take() {
            #[cfg(test)]
            self.queued
                .fetch_add(1, std::sync::atomic::Ordering::Release);
            if self.sender.try_send(value).is_err() {
                unreachable!("decode waiting retirement exceeded original pending admission");
            }
        }
    }
}
async fn retire(mut incoming: mpsc::Receiver<Retired>, cpu: AudioCpu) {
    while let Some(value) = incoming.recv().await {
        let mut batch = Vec::with_capacity(super::DECODE_JOB_QUEUE_CAPACITY);
        batch.push(value);
        while batch.len() < super::DECODE_JOB_QUEUE_CAPACITY {
            match incoming.try_recv() {
                Ok(value) => batch.push(value),
                Err(_) => break,
            }
        }
        cpu.retirement().retire(batch).await;
    }
    cpu.retirement().retire(incoming).await;
}

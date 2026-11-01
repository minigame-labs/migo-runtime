//! Queue admission used to end at dequeue, before stale payload destruction.
//! Keep the original ticket through startup, processing and bounded CPU cleanup.
use migo_io::audio_cpu::AudioCpu;
use shared::audio_channel::{AudioCommandDelivery, AudioCommandReceiver, AudioDiscardedPayload};
use tokio::sync::mpsc;

pub(crate) struct Inbox {
    receiver: AudioCommandReceiver,
    backlog: std::vec::IntoIter<AudioCommandDelivery>,
    retirement: Retirement,
}
struct Retirement {
    cpu: AudioCpu,
    capacity: usize,
    retired: Option<mpsc::Sender<Retired>>,
    #[cfg(test)]
    hook: Option<std::sync::Arc<dyn Fn() + Send + Sync>>,
}
struct Retired {
    _payload: AudioDiscardedPayload,
    #[cfg(test)]
    _witness: Witness,
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
impl Inbox {
    pub(crate) fn new(
        receiver: AudioCommandReceiver,
        backlog: Vec<AudioCommandDelivery>,
        cpu: AudioCpu,
    ) -> Self {
        assert!(
            backlog
                .iter()
                .all(|delivery| receiver.owns_delivery(delivery)),
            "foreign audio startup delivery"
        );
        // Every message owns a unique original data-item ticket. At most the
        // original channel capacity can exist across queued/running cleanup.
        let capacity = receiver.data_capacity().max(1);
        Self {
            receiver,
            backlog: backlog.into_iter(),
            retirement: Retirement {
                cpu,
                capacity,
                retired: None,
                #[cfg(test)]
                hook: None,
            },
        }
    }
    pub(crate) fn next(&mut self) -> Option<AudioCommandDelivery> {
        if self.backlog.len() != 0 {
            if let Ok(command) = self.receiver.try_recv_urgent() {
                return Some(AudioCommandDelivery::unmanaged(command));
            }
            return self.backlog.next();
        }
        self.receiver.try_recv_owned().ok()
    }
    pub(crate) fn discard_pending(&mut self) {
        for delivery in self.backlog.by_ref() {
            self.retirement.discard(delivery);
        }
        self.backlog = Vec::new().into_iter();
        self.receiver
            .discard_data_queue_with(|delivery| self.retirement.discard(delivery));
    }
    pub(crate) fn complete_release_all_contexts(&self) {
        self.receiver.complete_release_all_contexts();
    }
    pub(crate) fn reject(
        &mut self,
        delivery: AudioCommandDelivery,
        error: shared::error::EngineError,
    ) {
        if let Some(payload) = delivery.into_rejected_payload(error) {
            self.retirement.enqueue(payload);
        }
    }
    pub(crate) fn finish_copy(
        &mut self,
        delivery: AudioCommandDelivery,
        result: shared::error::EngineResult<()>,
    ) {
        if let Some(payload) = delivery.finish_copy_to_channel(result) {
            self.retirement.enqueue(payload);
        }
    }
    pub(crate) fn finish_frequency(
        &mut self,
        delivery: AudioCommandDelivery,
        result: shared::error::EngineResult<shared::audio_frequency_response::FrequencyResponse>,
    ) {
        if let Some(payload) = delivery.finish_frequency_response(result) {
            self.retirement.enqueue(payload);
        }
    }
    pub(crate) fn submit_decode(
        &mut self,
        delivery: AudioCommandDelivery,
        submit: impl FnOnce(
            shared::protocol::audio_cmd::AudioCmd,
        ) -> Option<(
            shared::protocol::audio_cmd::AudioCmd,
            shared::error::EngineError,
        )>,
    ) {
        if let Some(payload) = delivery.finish_decode_submission(submit) {
            self.retirement.enqueue(payload);
        }
    }
    pub(crate) fn apply_replacement<R>(
        &mut self,
        delivery: AudioCommandDelivery,
        apply: impl FnOnce(&mut shared::protocol::audio_cmd::AudioCmd) -> R,
    ) -> R {
        let (result, payload) = delivery.finish_replacement(apply);
        if let Some(payload) = payload {
            self.retirement.enqueue(payload);
        }
        result
    }
    pub(crate) fn finish_parameters(&mut self, delivery: AudioCommandDelivery) {
        self.retirement.discard(delivery);
    }
    #[cfg(test)]
    pub(crate) fn observe(&mut self, hook: std::sync::Arc<dyn Fn() + Send + Sync>) {
        self.retirement.hook = Some(hook);
    }
}
impl Retirement {
    fn discard(&mut self, delivery: AudioCommandDelivery) {
        if let Some(payload) = delivery.into_discarded_payload() {
            self.enqueue(payload);
        }
    }
    fn enqueue(&mut self, payload: AudioDiscardedPayload) {
        let value = Retired {
            _payload: payload,
            #[cfg(test)]
            _witness: Witness(self.hook.clone()),
        };
        let sender = self.retired.get_or_insert_with(|| {
            let (sender, incoming) = mpsc::channel(self.capacity);
            crate::streaming::get_stream_runtime().spawn(retire(incoming, self.cpu.clone()));
            sender
        });
        if sender.try_send(value).is_err() {
            unreachable!("audio command retirement exceeded original item admission");
        }
    }
}
impl Drop for Inbox {
    fn drop(&mut self) {
        // Stop commits before draining, including an admitted producer racing
        // shutdown. The receiver then contains no payload to free on this caller.
        self.receiver.close();
        self.discard_pending();
    }
}
async fn retire(mut incoming: mpsc::Receiver<Retired>, cpu: AudioCpu) {
    while let Some(value) = incoming.recv().await {
        let mut batch = Vec::with_capacity(32);
        batch.push(value);
        while batch.len() < 32 {
            match incoming.try_recv() {
                Ok(value) => batch.push(value),
                Err(_) => break,
            }
        }
        cpu.retirement().retire(batch).await;
    }
    cpu.retirement().retire(incoming).await;
}

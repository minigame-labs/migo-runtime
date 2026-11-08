//! Graph collection used to destroy node/PCM owners inside the device mix.
//! Each admitted node reserves its handoff and physical credit on the command
//! path; collection transfers ownership to the existing AudioCpu retirement lane.
use crate::limits::{AudioAggregateLedger, AudioAggregatePermit};
use futures_util::{FutureExt, StreamExt, stream::FuturesUnordered};
use migo_io::audio_cpu::AudioCpu;
use shared::error::EngineResult;
use std::{
    ops::{Deref, DerefMut},
    sync::Arc,
};
use tokio::sync::{mpsc, oneshot};

// Conservative task/channel/cell overhead. Every live or waiting owner keeps
// this charge and its inline node. Render buffers keep independent credit.
// PCM, delay storage and other existing resource permits remain with the node.
pub(crate) const HANDOFF_BYTES: usize = 4096;

#[derive(Clone)]
pub(crate) struct Host {
    cpu: AudioCpu,
    aggregate: Arc<AudioAggregateLedger>,
    channels: u32,
    incoming: mpsc::UnboundedSender<oneshot::Receiver<Retired>>,
}
impl Host {
    pub(crate) fn new(
        cpu: AudioCpu,
        aggregate: Arc<AudioAggregateLedger>,
        channels: u32,
    ) -> EngineResult<Self> {
        if cpu.is_closed() {
            return Err(migo_io::pools::PoolError::Closed.into());
        }
        let permit = aggregate.try_reserve(HANDOFF_BYTES, "graph retirement dispatcher")?;
        // Registration allocates only on the command/async paths. Its live and
        // queued messages are bounded by the charged node leases below.
        let (incoming, receiver) = mpsc::unbounded_channel();
        let dispatcher = Dispatcher {
            ready: FuturesUnordered::new(),
            incoming: receiver,
            _permit: permit,
        };
        let worker = cpu.clone();
        crate::streaming::get_stream_runtime().spawn(dispatcher.run(worker));
        Ok(Self {
            cpu,
            aggregate,
            channels,
            incoming,
        })
    }
    pub(crate) fn accepts_inline(&self, permit: &AudioAggregatePermit) -> bool {
        permit.belongs_to(&self.aggregate)
    }
    fn ensure_open(&self) -> EngineResult<()> {
        if self.cpu.is_closed() {
            return Err(migo_io::pools::PoolError::Closed.into());
        }
        Ok(())
    }
    fn register(&self, permit: AudioAggregatePermit) -> Lease {
        let (send, reply) = oneshot::channel::<Retired>();
        if self.incoming.send(reply).is_err() {
            unreachable!("process-lifetime graph retirement dispatcher disappeared");
        }
        Lease { send, permit }
    }
    pub(crate) fn reserve_value<T: Send + 'static>(&self) -> EngineResult<Reservation<T>> {
        self.reserve_value_with_bytes(0)
    }
    pub(crate) fn reserve_value_with_bytes<T: Send + 'static>(
        &self,
        retained_bytes: usize,
    ) -> EngineResult<Reservation<T>> {
        self.ensure_open()?;
        let bytes = (std::mem::size_of::<Resource<T>>() + HANDOFF_BYTES)
            .checked_add(retained_bytes)
            .ok_or_else(|| {
                shared::error::EngineError::from_detail(
                    shared::error::ErrorCode::InputSaturated,
                    "graph retained resource size overflow",
                )
            })?;
        let permit = self
            .aggregate
            .try_reserve(bytes, "graph retained resource")?;
        Ok(Reservation {
            lease: Some(self.register(permit)),
            marker: std::marker::PhantomData,
        })
    }

    /// Small response data stays on its caller but retains physical credit.
    pub(crate) fn reserve_inline(&self, bytes: usize) -> EngineResult<AudioAggregatePermit> {
        self.ensure_open()?;
        self.aggregate.try_reserve(bytes, "audio response storage")
    }
}

const RETIRE_BATCH: usize = 32;
struct Dispatcher {
    ready: FuturesUnordered<oneshot::Receiver<Retired>>,
    incoming: mpsc::UnboundedReceiver<oneshot::Receiver<Retired>>,
    _permit: AudioAggregatePermit,
}
impl Dispatcher {
    async fn run(mut self, cpu: AudioCpu) {
        loop {
            // Bound registration work per turn; no polling timer or render-side
            // lock/queue growth is introduced. Payloads keep their node credit.
            for _ in 0..RETIRE_BATCH {
                match self.incoming.try_recv() {
                    Ok(receiver) => self.ready.push(receiver),
                    Err(_) => break,
                }
            }
            let value = tokio::select! {
                Some(receiver) = self.incoming.recv() => {
                    self.ready.push(receiver);
                    continue;
                }
                Some(value) = self.ready.next(), if !self.ready.is_empty() => value,
                else => break,
            };
            if let Ok(value) = value {
                let mut batch = Vec::with_capacity(RETIRE_BATCH);
                batch.push(value);
                while batch.len() < RETIRE_BATCH {
                    match self.ready.next().now_or_never() {
                        Some(Some(Ok(value))) => batch.push(value),
                        Some(Some(Err(_))) => {}
                        _ => break,
                    }
                }
                retire_batch(&cpu, batch).await;
            }
        }
        // Retire the channel/future-list storage and its dispatcher credit too.
        cpu.retirement().retire(self).await;
    }
}
async fn retire_batch(cpu: &AudioCpu, value: Vec<Retired>) {
    cpu.retirement().retire(value).await;
}

struct Retired {
    // Actual node/PCM and excess render storage die before physical credit.
    _resource: Box<dyn Send>,
    _permit: AudioAggregatePermit,
}
struct Lease {
    send: oneshot::Sender<Retired>,
    permit: AudioAggregatePermit,
}

/// A command-side reservation made before allocating or replacing the value.
/// Dropping an unused reservation owns no resource and cancels its empty slot.
pub(crate) struct Reservation<T: Send + 'static> {
    lease: Option<Lease>,
    marker: std::marker::PhantomData<T>,
}
impl<T: Send + 'static> Reservation<T> {
    pub(crate) fn unmanaged() -> Self {
        Self {
            lease: None,
            marker: std::marker::PhantomData,
        }
    }
    pub(crate) fn finish(self, value: T) -> Value<T> {
        Value {
            value: Some(Box::new(Resource {
                value,
                #[cfg(test)]
                witness: Witness::default(),
            })),
            lease: self.lease,
        }
    }
}
/// Command/context resources use the same bounded, pre-registered CPU handoff.
pub(crate) struct Value<T: Send + 'static> {
    value: Option<Box<Resource<T>>>,
    lease: Option<Lease>,
}
impl<T: Send + 'static> Value<T> {
    pub(crate) fn accepts_physical_ledger(&self, host: &Host) -> bool {
        self.lease
            .as_ref()
            .is_none_or(|lease| lease.permit.belongs_to(&host.aggregate))
    }

    #[cfg(test)]
    pub(crate) fn observe(&mut self, hook: Arc<dyn Fn() + Send + Sync>) {
        self.value.as_mut().unwrap().witness.0 = Some(hook);
    }
}
impl<T: Send + 'static> Deref for Value<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.value.as_ref().unwrap().value
    }
}
impl<T: Send + 'static> DerefMut for Value<T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.value.as_mut().unwrap().value
    }
}
impl<T: Send + 'static> Drop for Value<T> {
    fn drop(&mut self) {
        if self.lease.is_some() {
            let lease = self.lease.take().unwrap();
            let value = Retired {
                _resource: self.value.take().unwrap(),
                _permit: lease.permit,
            };
            if lease.send.send(value).is_err() {
                unreachable!("process-lifetime retained-resource receiver disappeared");
            }
        }
    }
}

// Field order also lets the test witness observe destruction after the real T.
struct Resource<T> {
    value: T,
    #[cfg(test)]
    witness: Witness,
}
#[cfg(test)]
#[derive(Default)]
struct Witness(Option<Arc<dyn Fn() + Send + Sync>>);
#[cfg(test)]
impl Drop for Witness {
    fn drop(&mut self) {
        if let Some(hook) = &self.0 {
            hook();
        }
    }
}

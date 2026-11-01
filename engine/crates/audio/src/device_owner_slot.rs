//! Managed route preparation owns one host authority, context epoch and opaque
//! request. Neither a repeated format nor a reused numeric ID grants authority.
use super::*;
use crate::context::{AudioContext, DeviceEpoch};
use shared::channel::ThreadWakeup;
use std::collections::HashMap;
use tokio::task::JoinHandle;

struct Operation {
    live: DeviceEpoch,
}
pub(super) struct Binding {
    epoch: DeviceEpoch,
    operation: Arc<Operation>,
}
impl Binding {
    pub(super) fn is_live(&self) -> bool {
        self.epoch.is_live() && self.operation.live.is_live()
    }
}
struct Completion {
    device: Device,
}
struct Pending {
    task: JoinHandle<()>,
    reply: oneshot::Receiver<EngineResult<Completion>>,
}
impl Drop for Pending {
    fn drop(&mut self) {
        self.task.abort();
    }
}
struct Slot {
    host: Host,
    context: u32,
    epoch: DeviceEpoch,
    format: Format,
    operation: Arc<Operation>,
    pending: Option<Pending>,
    result: Option<EngineResult<Device>>,
}
impl Slot {
    fn new(host: Host, context: u32, epoch: DeviceEpoch, format: Format) -> Self {
        Self {
            host,
            context,
            epoch,
            format,
            operation: Arc::new(Operation {
                live: DeviceEpoch::new(),
            }),
            pending: None,
            result: None,
        }
    }
    fn invalidate(&mut self) {
        self.operation.live.invalidate();
        self.pending = None;
        self.result = None;
    }
    fn request(&mut self, wakeup: ThreadWakeup, hook: Option<Hook>) {
        self.invalidate();
        self.operation = Arc::new(Operation {
            live: DeviceEpoch::new(),
        });
        if self.format.source == self.format.target {
            wakeup.notify();
            return;
        }
        let host = self.host.clone();
        let context = self.context;
        let format = self.format;
        let epoch = self.epoch.clone();
        let operation = self.operation.clone();
        let (send, reply) = oneshot::channel();
        let task = crate::streaming::get_stream_runtime().spawn(async move {
            let result = host
                .prepare(
                    context,
                    format,
                    Limits {
                        source_ahead: MAX_AHEAD,
                        pending_events: MAX_EVENTS,
                    },
                    hook,
                )
                .await
                .map(|mut device| {
                    device.binding = Some(Binding { epoch, operation });
                    Completion { device }
                });
            let _ = send.send(result);
            wakeup.notify();
        });
        self.pending = Some(Pending { task, reply });
    }
    fn valid(&self, device: &Device) -> bool {
        let Some(binding) = &device.binding else {
            return false;
        };
        let Some(lease) = &device.lease else {
            return false;
        };
        let State::Session(session) = &lease.retired.as_ref().unwrap().state else {
            return false;
        };
        let ready = &session.pull.ready;
        binding.is_live()
            && self.epoch.same(&binding.epoch)
            && Arc::ptr_eq(&self.operation, &binding.operation)
            && Arc::ptr_eq(&self.host.usage, &device.authority)
            && !self.host.cpu.is_closed()
            && ready.request.context == self.context
            && ready.request.format == self.format
            && !ready.request.cancel.load(Ordering::Acquire)
            && ready.is_ready()
            && device.error.is_none()
    }
    fn accept(&mut self, completion: Completion) -> bool {
        if !self.valid(&completion.device) {
            return false;
        }
        self.result = Some(Ok(completion.device));
        true
    }
    fn poll(&mut self) {
        if !self.epoch.is_live() || self.host.cpu.is_closed() {
            let code = if self.host.cpu.is_closed() {
                EngineError::from(migo_io::pools::PoolError::Closed).code
            } else {
                ErrorCode::Cancelled
            };
            self.invalidate();
            self.result = Some(Err(EngineError::from_detail(
                code,
                "managed route no longer live",
            )));
            return;
        }
        let Some(pending) = self.pending.as_mut() else {
            return;
        };
        let result = match pending.reply.try_recv() {
            Ok(result) => result,
            Err(oneshot::error::TryRecvError::Empty) => return,
            Err(oneshot::error::TryRecvError::Closed) => {
                Err(invalid("managed route preparation task stopped"))
            }
        };
        self.pending = None;
        match result {
            Ok(completion) => {
                if !self.accept(completion) {
                    self.result = Some(Err(cancelled()));
                }
            }
            Err(error) => self.result = Some(Err(error)),
        }
    }
    fn take(&mut self) -> Option<EngineResult<Device>> {
        self.poll();
        let result = self.result.take()?;
        Some(match result {
            Ok(device) if self.valid(&device) => Ok(device),
            Ok(_) => Err(cancelled()),
            Err(error) => Err(error),
        })
    }
}
impl Drop for Slot {
    fn drop(&mut self) {
        self.invalidate();
    }
}
struct Controller {
    host: Host,
    slots: HashMap<u32, Slot>,
}
impl Controller {
    fn new(cpu: AudioCpu) -> Self {
        Self {
            host: Host::new(cpu),
            slots: HashMap::new(),
        }
    }
    fn ensure(
        &mut self,
        context: &AudioContext,
        target: u32,
        wakeup: ThreadWakeup,
        hook: Option<Hook>,
    ) -> EngineResult<()> {
        let epoch = context.device_epoch();
        if !epoch.is_live() {
            self.remove(context);
            return Err(cancelled());
        }
        if self.host.cpu.is_closed() {
            return Err(migo_io::pools::PoolError::Closed.into());
        }
        let format = Format {
            source: context.sample_rate(),
            target,
            channels: context.channels(),
        };
        if self
            .slots
            .get(&context.id())
            .is_some_and(|slot| slot.epoch.same(&epoch) && slot.format == format)
        {
            return Ok(());
        }
        let mut slot = Slot::new(self.host.clone(), context.id(), epoch, format);
        slot.request(wakeup, hook);
        self.slots.insert(context.id(), slot);
        Ok(())
    }
    fn take(&mut self, context: &AudioContext) -> Option<EngineResult<Device>> {
        let slot = self.slots.get_mut(&context.id())?;
        if !slot.epoch.same(&context.device_epoch()) {
            if !slot.epoch.is_live() {
                slot.invalidate();
            }
            return Some(Err(cancelled()));
        }
        slot.take()
    }
    fn remove(&mut self, context: &AudioContext) {
        if self
            .slots
            .get(&context.id())
            .is_some_and(|slot| slot.epoch.same(&context.device_epoch()))
        {
            self.slots.remove(&context.id());
        }
    }
    fn restart(&mut self) {
        self.slots.clear();
    }
}

#[path = "device_owner_slot_tests.rs"]
#[cfg(test)]
mod tests;

#[path = "device_graph.rs"]
mod graph;

#[path = "device_mixer.rs"]
pub(crate) mod mixer;

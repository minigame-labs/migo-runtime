//! Admission and off-render retirement for a whole device session.
//! One reserved handoff per admitted owner uses the process-lifetime streaming
//! runtime and the existing AudioCpu lane, including after host close.
use super::*;
use crate::device_prepare::{Format, Hook, Request};
use crate::limits::{AudioAggregateLedger, AudioAggregatePermit, MAX_SAMPLE_RATE, MIN_SAMPLE_RATE};
use migo_io::audio_cpu::AudioCpu;
use std::sync::{Arc, OnceLock, atomic::AtomicUsize};
use tokio::sync::oneshot;

// Four default sinc stages, including construction temporaries, async history,
// adapters, pull/event scratch and the reserved handoff. See resource tests and
// pinned Rubato make_sincs/new_with_sinc_interpolator; this is not a PCM quota.
const OWNER_BYTES: usize = 4 * 1024 * 1024;
const HOST_OWNERS: usize = 8;
const PROCESS_OWNERS: usize = 32;
struct Usage {
    owners: AtomicUsize,
    max: usize,
}
impl Usage {
    fn new(max: usize) -> Arc<Self> {
        Arc::new(Self {
            owners: AtomicUsize::new(0),
            max,
        })
    }
    fn reserve(self: &Arc<Self>) -> EngineResult<Credit> {
        self.owners
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_add(1).filter(|next| *next <= self.max)
            })
            .map_err(|_| {
                EngineError::from_detail(
                    ErrorCode::InputSaturated,
                    "device filter owner limit exceeded",
                )
            })?;
        Ok(Credit(self.clone()))
    }
}
struct Credit(Arc<Usage>);
impl Drop for Credit {
    fn drop(&mut self) {
        self.0.owners.fetch_sub(1, Ordering::AcqRel);
    }
}
struct Ticket {
    _host: Credit,
    _process: Credit,
    _aggregate: AudioAggregatePermit,
}
type RetireHook = Arc<dyn Fn(bool) + Send + Sync>;
struct Witness(Option<RetireHook>);
impl Drop for Witness {
    fn drop(&mut self) {
        if let Some(hook) = &self.0 {
            hook(false);
        }
    }
}
#[derive(Clone)]
struct Host {
    cpu: AudioCpu,
    usage: Arc<Usage>,
    process: Arc<Usage>,
    aggregate: Arc<AudioAggregateLedger>,
    retire_hook: Option<RetireHook>,
}
impl Host {
    /// Construct once in the host audio controller; every route clones this
    /// authority. Distinct hosts still use one process quota and PCM ledger.
    fn new(cpu: AudioCpu) -> Self {
        static PROCESS: OnceLock<Arc<Usage>> = OnceLock::new();
        Self {
            cpu: cpu.background(),
            usage: Usage::new(HOST_OWNERS),
            process: PROCESS.get_or_init(|| Usage::new(PROCESS_OWNERS)).clone(),
            aggregate: AudioAggregateLedger::process_global(),
            retire_hook: None,
        }
    }
    fn reserve(&self, format: Format) -> EngineResult<Ticket> {
        if self.cpu.is_closed() {
            return Err(migo_io::pools::PoolError::Closed.into());
        }
        if !(MIN_SAMPLE_RATE..=MAX_SAMPLE_RATE).contains(&format.source)
            || !(MIN_SAMPLE_RATE..=MAX_SAMPLE_RATE).contains(&format.target)
            || !(1..=32).contains(&format.channels)
            || format.source == format.target
        {
            return Err(invalid("invalid managed device SRC format"));
        }
        let host = self.usage.reserve()?;
        let process = self.process.reserve()?;
        let aggregate = self.aggregate.try_reserve(OWNER_BYTES, "device filter")?;
        Ok(Ticket {
            _host: host,
            _process: process,
            _aggregate: aggregate,
        })
    }
    async fn prepare(
        &self,
        context: u32,
        format: Format,
        limits: Limits,
        hook: Option<Hook>,
    ) -> EngineResult<Device> {
        let ticket = self.reserve(format)?;
        let preparation = Some(self.cpu.preparation_permit().await?);
        let request = Arc::new(Request {
            context,
            format,
            cancel: std::sync::atomic::AtomicBool::new(false),
            cpu: self.cpu.clone(),
        });
        let (send, reply) = oneshot::channel::<Retired>();
        let retirement = self.cpu.retirement();
        // The shared process runtime is static and has no shutdown/abort API.
        // This receiver is deliberately not tied to a host/domain task handle.
        crate::streaming::get_stream_runtime().spawn(async move {
            if let Ok(retired) = reply.await {
                // This drop-only capability survives host cancellation and
                // retains the physical owner while normal Audio queues are full.
                retirement.retire(retired).await;
            }
        });
        let lease = Lease {
            send: Some(send),
            retired: Some(Retired {
                state: State::Empty,
                preparation,
                _ticket: ticket,
                witness: Witness(self.retire_hook.clone()),
            }),
        };
        let initial = hook.clone();
        let mut lease = self
            .cpu
            .run(move || {
                let mut lease = lease;
                let ready = Ready::new(request)?;
                if let Some(hook) = initial {
                    hook(&ready);
                }
                lease.retired.as_mut().unwrap().state = State::Ready(ready);
                Ok::<_, EngineError>(lease)
            })
            .await??;
        while lease.ready().is_preparing() {
            let hook = hook.clone();
            lease = self
                .cpu
                .run(move || {
                    let mut lease = lease;
                    let State::Ready(ready) = &mut lease.retired.as_mut().unwrap().state else {
                        unreachable!()
                    };
                    ready.step(&[], false)?;
                    if let Some(hook) = hook {
                        hook(ready);
                    }
                    Ok::<_, EngineError>(lease)
                })
                .await??;
        }
        lease = self
            .cpu
            .run(move || {
                let mut lease = lease;
                let State::Ready(mut ready) =
                    std::mem::replace(&mut lease.retired.as_mut().unwrap().state, State::Empty)
                else {
                    unreachable!()
                };
                if ready.request.cancel.load(Ordering::Acquire) {
                    return Err(cancelled());
                }
                // Internal SRC errors and the check/step cancellation race must
                // retain tables until the entire owner reaches its reserved handoff.
                ready.deferred_retirement = true;
                ready.stream.as_mut().unwrap().deferred_retirement = true;
                let session = Session::allocate(ready, limits)?;
                lease.retired.as_mut().unwrap().state = State::Session(session);
                Ok::<_, EngineError>(lease)
            })
            .await??;
        if self.cpu.is_closed() {
            return Err(migo_io::pools::PoolError::Closed.into());
        }
        // CPU publication already returned its admission. Active sessions do
        // not retain preparation slots; cancelled running steps keep theirs in
        // Retired until the real native owner is destroyed.
        drop(lease.retired.as_mut().unwrap().preparation.take());
        Ok(Device {
            authority: self.usage.clone(),
            binding: None,
            lease: Some(lease),
            error: None,
        })
    }
}
enum State {
    Empty,
    Ready(Box<Ready>),
    Session(Box<Session>),
}
struct Retired {
    // Declaration order is load-bearing: actual resources die before credit.
    state: State,
    preparation: Option<tokio::sync::OwnedSemaphorePermit>,
    _ticket: Ticket,
    witness: Witness,
}

#[cfg(test)]
#[path = "device_preparation_tests.rs"]
mod preparation_tests;
impl Drop for Retired {
    fn drop(&mut self) {
        if let Some(hook) = &self.witness.0 {
            hook(true);
        }
    }
}
struct Lease {
    send: Option<oneshot::Sender<Retired>>,
    retired: Option<Retired>,
}
impl Lease {
    fn ready(&self) -> &Ready {
        let State::Ready(ready) = &self.retired.as_ref().unwrap().state else {
            unreachable!()
        };
        ready
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        if let Some(send) = self.send.take() {
            // A private, prestarted process-lifetime receiver owns this slot.
            // No try-send/full fallback can destroy a filter on this caller.
            if send.send(self.retired.take().unwrap()).is_err() {
                unreachable!("process-lifetime device retirement receiver disappeared");
            }
        }
    }
}
struct Device {
    authority: Arc<Usage>,
    binding: Option<owner::Binding>,
    lease: Option<Lease>,
    error: Option<ErrorCode>,
}
impl Device {
    fn session(&mut self) -> EngineResult<&mut Session> {
        if self.error.is_none()
            && self
                .binding
                .as_ref()
                .is_some_and(|binding| !binding.is_live())
        {
            self.retire(ErrorCode::Cancelled);
        }
        if let Some(code) = self.error {
            return Err(EngineError::from_detail(
                code,
                "managed device owner retired",
            ));
        }
        let State::Session(session) =
            &mut self.lease.as_mut().unwrap().retired.as_mut().unwrap().state
        else {
            unreachable!()
        };
        Ok(session)
    }
    fn retire(&mut self, code: ErrorCode) {
        self.error = Some(code);
        self.lease = None;
    }
    fn step(&mut self, graph: &mut impl Graph, output: &mut [f32]) -> EngineResult<SessionTurn> {
        let result = self.session()?.step(graph, output);
        if let Err(error) = &result {
            self.retire(error.code);
        }
        result
    }
    fn pop_ended(&mut self) -> EngineResult<Option<u32>> {
        let result = self.session()?.pop_ended();
        if let Err(error) = &result {
            self.retire(error.code);
        }
        result
    }
}

#[path = "device_resources_tests.rs"]
#[cfg(test)]
mod tests;

#[path = "device_owner_slot.rs"]
mod owner;

pub(crate) use owner::mixer::DeviceMixer;

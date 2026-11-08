//! Owner limits and off-render preparation for a whole device session.
use super::*;
use crate::device_prepare::{Format, Hook, Request};
use crate::limits::{MAX_SAMPLE_RATE, MIN_SAMPLE_RATE};
use migo_io::audio_cpu::AudioCpu;
use std::sync::{Arc, OnceLock, atomic::AtomicUsize};

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
}
#[derive(Clone)]
struct Host {
    cpu: AudioCpu,
    usage: Arc<Usage>,
    process: Arc<Usage>,
}
impl Host {
    /// Construct once in the host audio controller; every route clones this
    /// authority. Distinct hosts still share one process owner limit.
    fn new(cpu: AudioCpu) -> Self {
        static PROCESS: OnceLock<Arc<Usage>> = OnceLock::new();
        Self {
            cpu: cpu.background(),
            usage: Usage::new(HOST_OWNERS),
            process: PROCESS.get_or_init(|| Usage::new(PROCESS_OWNERS)).clone(),
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
        Ok(Ticket {
            _host: host,
            _process: process,
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
        let lease = Lease {
            retired: Some(Retired {
                state: State::Empty,
                preparation,
                _ticket: ticket,
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
                let session = Session::allocate(ready, limits)?;
                lease.retired.as_mut().unwrap().state = State::Session(session);
                Ok::<_, EngineError>(lease)
            })
            .await??;
        if self.cpu.is_closed() {
            return Err(migo_io::pools::PoolError::Closed.into());
        }
        // Active sessions do not retain preparation slots.
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
    // Resources drop before the owner credit that admitted them.
    state: State,
    preparation: Option<tokio::sync::OwnedSemaphorePermit>,
    _ticket: Ticket,
}

struct Lease {
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

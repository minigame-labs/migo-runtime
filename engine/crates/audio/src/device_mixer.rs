//! Audio-thread adapter for asynchronously prepared device sinc routes.
//! Only published PCM enters the shared bus; source steps and cached suffixes
//! stay with one managed owner across callbacks and idle/suspend intervals.
use super::*;
use crate::audio_thread::{ContextAccess, ContextView};
use crate::context::AudioContext;
use shared::protocol::audio_cmd::AudioContextState;
#[cfg(test)]
use std::collections::HashMap;
#[path = "device_route_storage.rs"]
mod route_storage;
use route_storage::Routes;
use std::time::{Duration, Instant};

const MAX_MIX_TURNS: usize = 4096;
const ADMISSION_RETRY: Duration = Duration::from_millis(100);

enum RouteState {
    Pending,
    Active(Device),
    Failed {
        code: ErrorCode,
        retry: Option<Instant>,
    },
}
struct Route {
    epoch: DeviceEpoch,
    format: Format,
    state: RouteState,
}
pub(crate) struct DeviceMixer {
    controller: Controller,
    routes: Routes,
    admission_retry: Option<Instant>,
    // A fixed host stack buffer, independent of route count and backend layout.
    scratch: [f32; QUANTUM * 32],
}
impl DeviceMixer {
    pub(crate) fn managed(cpu: AudioCpu, storage: crate::node_retirement::Host) -> Self {
        Self {
            controller: Controller::managed(cpu, storage.clone()),
            routes: Routes::managed(storage),
            admission_retry: None,
            scratch: [0.0; QUANTUM * 32],
        }
    }
    #[cfg(test)]
    pub(crate) fn new(cpu: AudioCpu) -> Self {
        Self {
            controller: Controller::new(cpu),
            routes: Routes::new(),
            admission_retry: None,
            scratch: [0.0; QUANTUM * 32],
        }
    }
    pub(crate) fn restart(&mut self) {
        self.controller.restart();
        self.routes.clear();
        self.admission_retry = None;
    }
    fn failed(error: EngineError, retry_admission: bool) -> RouteState {
        tracing::warn!("device SRC route stopped: {}", error);
        RouteState::Failed {
            retry: (retry_admission && error.code == ErrorCode::InputSaturated)
                .then(|| Instant::now() + ADMISSION_RETRY),
            code: error.code,
        }
    }
    fn control_only(
        context: &mut AudioContext,
        paused: bool,
        collected: &mut impl FnMut(u32),
        ended: &mut impl FnMut(u32),
    ) {
        if context.has_device_controls() {
            for &id in context.collect_device_controls() {
                collected(id);
            }
        }
        if !paused && context.state() == AudioContextState::Running {
            context.drain_ended().for_each(ended);
        }
    }
    /// Command/management phase only: registration, completion and retry may
    /// allocate. No filter construction or CPU wait occurs on this caller.
    pub(crate) fn service(
        &mut self,
        contexts: &mut impl ContextAccess,
        target: u32,
        paused: bool,
        wakeup: &ThreadWakeup,
        mut collected: impl FnMut(u32),
        mut ended: impl FnMut(u32),
    ) {
        self.admission_retry = None;
        self.routes.retain(|id, route| {
            contexts.get(id).is_some_and(|ctx| {
                route.epoch.same(&ctx.device_epoch()) && route.format.target == target
            })
        });
        self.controller.slots.retain(|id, slot| {
            contexts.get(id).is_some_and(|ctx| {
                slot.epoch.same(&ctx.device_epoch()) && slot.format.target == target
            })
        });
        for (&id, context) in contexts.iter_mut() {
            if context.sample_rate() == target {
                Self::control_only(context, paused, &mut collected, &mut ended);
                continue;
            }
            if context.state() == AudioContextState::Running
                && !context.has_active_sources()
                && self
                    .routes
                    .get(&id)
                    .is_some_and(|r| !matches!(r.state, RouteState::Active(_)))
            {
                self.routes.remove(&id);
                self.controller.remove(context);
            }
            if !paused && context.has_active_sources() && !self.routes.contains_key(&id) {
                let result = self.routes.create_with(id, || {
                    let state = match self
                        .controller
                        .ensure(context, target, wakeup.clone(), None)
                    {
                        Ok(()) => RouteState::Pending,
                        Err(error) => Self::failed(error, true),
                    };
                    Ok(Route {
                        epoch: context.device_epoch(),
                        format: Format {
                            source: context.sample_rate(),
                            target,
                            channels: context.channels(),
                        },
                        state,
                    })
                });
                if let Err(error) = result {
                    if error.code == ErrorCode::InputSaturated {
                        // A refusal cannot allocate a per-route failure record.
                        // Keep one inline deadline so an idle host retries.
                        self.admission_retry = Some(Instant::now() + ADMISSION_RETRY);
                    }
                    Self::control_only(context, paused, &mut collected, &mut ended);
                    continue;
                }
            }
            let Some(route) = self.routes.get_mut(&id) else {
                Self::control_only(context, paused, &mut collected, &mut ended);
                continue;
            };
            if !paused
                && context.has_active_sources()
                && matches!(route.state, RouteState::Failed { retry: Some(at), .. } if Instant::now() >= at)
            {
                // ensure is intentionally idempotent; a timed admission retry
                // must issue a new operation on the same host authority.
                if let Some(slot) = self.controller.slots.get_mut(&id) {
                    slot.request(wakeup.clone(), None);
                    route.state = RouteState::Pending;
                } else {
                    route.state =
                        match self
                            .controller
                            .ensure(context, target, wakeup.clone(), None)
                        {
                            Ok(()) => RouteState::Pending,
                            Err(error) => Self::failed(error, true),
                        };
                }
            }
            if matches!(route.state, RouteState::Pending)
                && let Some(result) = self.controller.take(context)
            {
                route.state = match result {
                    Ok(device) => RouteState::Active(device),
                    Err(error) => Self::failed(error, true),
                };
            }
            if let RouteState::Active(device) = &mut route.state {
                let result = (|| {
                    device.session()?.pull.check_live()?;
                    device.service_context_controls(context, &mut collected)?;
                    if !paused {
                        while let Some(id) = device.pop_ended_context(context)? {
                            ended(id);
                        }
                    }
                    Ok::<_, EngineError>(())
                })();
                if let Err(error) = result {
                    route.state = Self::failed(error, false);
                }
            } else {
                // No source PCM has been rendered for this route. Explicit stop
                // collection/ended must not depend on a filter being admitted.
                Self::control_only(context, paused, &mut collected, &mut ended);
            }
        }
    }
    pub(crate) fn has_output_work(&self, contexts: &impl ContextView, target: u32) -> bool {
        contexts.iter().any(|(id, context)| {
            if context.state() != AudioContextState::Running {
                return false;
            }
            if context.sample_rate() == target {
                return context.has_active_sources();
            }
            let Some(route) = self.routes.get(id) else {
                return false;
            };
            let RouteState::Active(device) = &route.state else {
                return false;
            };
            if route.format.target != target
                || !route.epoch.same(&context.device_epoch())
                || !route.epoch.is_live()
            {
                return false;
            }
            let Some(lease) = &device.lease else {
                return false;
            };
            let State::Session(session) = &lease.retired.as_ref().unwrap().state else {
                return false;
            };
            context.has_active_sources()
                || !session.sleeping
                || !session.pull.buffers.as_ref().unwrap().events.is_empty()
        })
    }
    pub(crate) fn retry_wait(&self, contexts: &impl ContextView) -> Option<Duration> {
        let now = Instant::now();
        self.routes
            .iter()
            .filter_map(|(id, route)| {
                if !contexts
                    .get(id)
                    .is_some_and(AudioContext::has_active_sources)
                {
                    return None;
                }
                match route.state {
                    RouteState::Failed {
                        code: ErrorCode::InputSaturated,
                        retry: Some(at),
                    } => Some(at.saturating_duration_since(now)),
                    _ => None,
                }
            })
            .chain(
                self.admission_retry
                    .map(|at| at.saturating_duration_since(now)),
            )
            .min()
    }
    /// At most one device quantum, with a fixed turn ceiling. Every individual
    /// turn retains the source/SRC/publication bounds established by Pull.
    pub(crate) fn mix(
        &mut self,
        context: &mut AudioContext,
        output: &mut [f32],
        target: u32,
        channels: u32,
        collected: impl FnMut(u32),
        ended: impl FnMut(u32),
    ) {
        self.mix_bounded(
            context,
            output,
            (target, channels),
            MAX_MIX_TURNS,
            collected,
            ended,
        );
    }
    fn mix_bounded(
        &mut self,
        context: &mut AudioContext,
        output: &mut [f32],
        format: (u32, u32),
        budget: usize,
        mut collected: impl FnMut(u32),
        mut ended: impl FnMut(u32),
    ) {
        let (target, channels) = format;
        let target_channels = channels.max(1) as usize;
        let frames = (output.len() / target_channels).min(QUANTUM);
        if frames == 0 || context.state() != AudioContextState::Running {
            return;
        }
        if context.sample_rate() == target {
            match context.process_native_device(
                &mut output[..frames * target_channels],
                target,
                channels,
            ) {
                Ok(ids) => ids.iter().copied().for_each(&mut collected),
                Err(error) => tracing::warn!("native device format rejected: {}", error),
            }
            context.drain_ended().for_each(ended);
            return;
        }
        let Some(route) = self.routes.get_mut(&context.id()) else {
            return;
        };
        if route.format.target != target || !route.epoch.same(&context.device_epoch()) {
            return;
        }
        let RouteState::Active(device) = &mut route.state else {
            return;
        };
        let source_channels = context.channels() as usize;
        let result = (|| {
            let mut written = 0;
            for _ in 0..budget {
                let scratch = &mut self.scratch[..(frames - written) * source_channels];
                scratch.fill(0.0);
                match device.step_context(context, scratch, &mut collected)? {
                    SessionTurn::Work(Turn::Published(n)) => {
                        AudioContext::mix_device_channels(
                            &scratch[..n * source_channels],
                            &mut output[written * target_channels..(written + n) * target_channels],
                            source_channels,
                            target_channels,
                        );
                        written += n;
                    }
                    SessionTurn::Quiescent | SessionTurn::Work(Turn::Idle) => return Ok(()),
                    SessionTurn::Work(Turn::Finished) => {
                        return Err(invalid("continuous device route unexpectedly finished"));
                    }
                    _ => {}
                }
                while let Some(id) = device.pop_ended_context(context)? {
                    ended(id);
                }
                if written == frames {
                    return Ok(());
                }
            }
            Err(EngineError::from_detail(
                ErrorCode::InputSaturated,
                "device mix turn budget exhausted",
            ))
        })();
        if let Err(error) = result {
            route.state = Self::failed(error, false);
        }
    }
}

#[cfg(test)]
#[path = "device_mixer_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "device_route_storage_tests.rs"]
mod storage_tests;

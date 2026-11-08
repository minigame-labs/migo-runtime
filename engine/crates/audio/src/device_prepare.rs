//! Device SRC preparation on the existing background AudioCpu.
//! Managed routes retain physical admission through off-render retirement;
//! the unmanaged Slot below is only a preparation regression fixture.
use crate::{resample_job::bandlimited, streaming_sinc::Stream};
use migo_io::{audio_cpu::AudioCpu, pools::PoolError};
#[cfg(test)]
use shared::channel::ThreadWakeup;
use shared::error::{EngineError, EngineResult, ErrorCode};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
#[cfg(test)]
use tokio::{sync::oneshot, task::JoinHandle};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Format {
    source: u32,
    target: u32,
    channels: u32,
}
struct Request {
    context: u32,
    format: Format,
    cancel: AtomicBool,
    cpu: AudioCpu,
}
struct Ready {
    request: Arc<Request>,
    stream: Option<Stream>,
    output: Vec<f32>,
    error: Option<ErrorCode>,
}
impl Ready {
    fn new(request: Arc<Request>) -> EngineResult<Box<Self>> {
        if request.cancel.load(Ordering::Acquire) {
            return Err(cancelled());
        }
        if request.cpu.is_closed() {
            return Err(PoolError::Closed.into());
        }
        let f = request.format;
        let bypass = f.source == f.target;
        Ok(Box::new(Self {
            request,
            stream: if bypass {
                None
            } else {
                Some(Stream::new(f.source, f.target, f.channels, None)?)
            },
            output: if bypass {
                Vec::new()
            } else {
                Vec::with_capacity(crate::streaming_sinc::STEP_SAMPLES)
            },
            error: None,
        }))
    }
    fn is_preparing(&self) -> bool {
        self.stream.as_ref().is_some_and(Stream::is_preparing)
    }
    fn is_ready(&self) -> bool {
        self.error.is_none() && !self.is_preparing()
    }
    fn is_bypass(&self) -> bool {
        self.request.format.source == self.request.format.target
    }
    fn retire(&mut self, code: ErrorCode) {
        self.stream = None;
        self.output = Vec::new();
        self.error = Some(code);
    }
    fn step(&mut self, input: &[f32], eof: bool) -> EngineResult<bandlimited::Step> {
        self.output.clear();
        if self.error.is_none() {
            if self.request.cancel.load(Ordering::Acquire) {
                self.retire(ErrorCode::Cancelled);
            } else if self.request.cpu.is_closed() {
                self.retire(EngineError::from(PoolError::Closed).code);
            }
        }
        if let Some(code) = self.error {
            return Err(EngineError::from_detail(code, "device SRC owner retired"));
        }
        let result = match self.stream.as_mut() {
            Some(stream) => stream.step(input, eof, &mut self.output, &self.request.cancel),
            None => Err(EngineError::from_detail(
                ErrorCode::InvalidArgument,
                "native route needs no SRC",
            )),
        };
        if let Err(error) = &result {
            self.retire(error.code);
        }
        result
    }
}
type Hook = Arc<dyn Fn(&Ready) + Send + Sync>;

#[cfg(test)]
async fn prepare(request: Arc<Request>, hook: Option<Hook>) -> EngineResult<Box<Ready>> {
    let cpu = request.cpu.clone();
    let initial_hook = hook.clone();
    let initialize = move || {
        let ready = Ready::new(request)?;
        if let Some(hook) = initial_hook {
            hook(&ready);
        }
        Ok::<_, EngineError>(ready)
    };
    let mut ready = cpu.run(initialize).await??;
    while ready.is_preparing() {
        let hook = hook.clone();
        ready = cpu
            .run(move || {
                ready.step(&[], false)?;
                if let Some(hook) = hook {
                    hook(&ready);
                }
                Ok::<_, EngineError>(ready)
            })
            .await??;
    }
    if ready.request.cancel.load(Ordering::Acquire) {
        return Err(cancelled());
    }
    if cpu.is_closed() {
        return Err(PoolError::Closed.into());
    }
    Ok(ready)
}

#[cfg(test)]
struct Pending {
    task: JoinHandle<()>,
    reply: oneshot::Receiver<EngineResult<Box<Ready>>>,
}
#[cfg(test)]
impl Drop for Pending {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[cfg(test)]
struct Slot {
    context: u32,
    request: Option<Arc<Request>>,
    pending: Option<Pending>,
    result: Option<EngineResult<Box<Ready>>>,
}
#[cfg(test)]
impl Slot {
    fn new(context: u32) -> Self {
        Self {
            context,
            request: None,
            pending: None,
            result: None,
        }
    }
    fn invalidate(&mut self) {
        if let Some(request) = self.request.take() {
            request.cancel.store(true, Ordering::Release);
        }
        self.pending = None;
        self.result = None;
    }
    fn request(
        &mut self,
        format: Format,
        cpu: AudioCpu,
        wakeup: ThreadWakeup,
        hook: Option<Hook>,
    ) -> EngineResult<()> {
        bandlimited::validate_format(format.source, format.target, format.channels)
            .map_err(|e| EngineError::from_detail(ErrorCode::InvalidArgument, e))?;
        if cpu.is_closed() {
            return Err(PoolError::Closed.into());
        }
        self.invalidate();
        let request = Arc::new(Request {
            context: self.context,
            format,
            cancel: AtomicBool::new(false),
            cpu: cpu.background(),
        });
        self.request = Some(request.clone());
        if format.source == format.target {
            self.result = Some(Ready::new(request));
            wakeup.notify();
            return Ok(());
        }
        let (send, reply) = oneshot::channel();
        let task = crate::streaming::get_stream_runtime().spawn(async move {
            let result = prepare(request, hook).await;
            let _ = send.send(result);
            wakeup.notify();
        });
        self.pending = Some(Pending { task, reply });
        Ok(())
    }
    fn accept(&mut self, ready: Box<Ready>) -> bool {
        let valid = self.request.as_ref().is_some_and(|request| {
            Arc::ptr_eq(request, &ready.request)
                && request.context == self.context
                && !request.cancel.load(Ordering::Acquire)
                && !request.cpu.is_closed()
                && ready.is_ready()
        });
        if valid {
            self.result = Some(Ok(ready));
        }
        valid
    }
    fn poll(&mut self) {
        let Some(pending) = self.pending.as_mut() else {
            return;
        };
        let result = match pending.reply.try_recv() {
            Ok(result) => result,
            Err(oneshot::error::TryRecvError::Empty) => return,
            Err(oneshot::error::TryRecvError::Closed) => Err(EngineError::from_detail(
                ErrorCode::Internal,
                "device preparation task stopped",
            )),
        };
        self.pending = None;
        match result {
            Ok(ready) => {
                if !self.accept(ready) {
                    let error = if self.request.as_ref().is_some_and(|r| r.cpu.is_closed()) {
                        PoolError::Closed.into()
                    } else {
                        cancelled()
                    };
                    self.result = Some(Err(error));
                }
            }
            Err(error) => self.result = Some(Err(error)),
        }
    }
    fn take(&mut self) -> Option<EngineResult<Box<Ready>>> {
        self.poll();
        self.result.take()
    }
}
#[cfg(test)]
impl Drop for Slot {
    fn drop(&mut self) {
        self.invalidate();
    }
}
fn cancelled() -> EngineError {
    EngineError::from_detail(ErrorCode::Cancelled, "device route superseded")
}

#[path = "device_prepare_tests.rs"]
#[cfg(test)]
mod tests;

#[path = "device_pull.rs"]
mod pull;

pub(crate) use pull::DeviceMixer;

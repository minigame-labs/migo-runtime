//! Seal an already-open HTTP body on the session's existing Fs executor.
use super::*;
use migo_io::{
    spool::{SpoolBudget, SpoolWriter},
    task::{PoolKind, PriorityClass},
};
use shared::audio_file::{
    AudioFile, AudioFileCancel, AudioHttpSpool, AudioSpoolBody, AudioSpoolFuture,
};
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::time::Instant;

struct Spool {
    context: LocalAudioFile,
    budget: SpoolBudget,
    maximum: u64,
}
pub(super) fn capability(context: &LocalAudioFile) -> Arc<dyn AudioHttpSpool> {
    let mut context = context.clone();
    context.mounts = None;
    Arc::new(Spool {
        context,
        budget: SpoolBudget::process(),
        maximum: migo_io::spool::DEFAULT_SPOOL_FILE_BYTES,
    })
}
fn error() -> EngineError {
    EngineError::from_detail(ErrorCode::IoError, "HTTP audio spool unavailable")
}
struct Active(Arc<AtomicBool>);
impl Drop for Active {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}
struct Cancellation {
    active: Arc<AtomicBool>,
    upstream: Arc<dyn AudioFileCancel>,
}
impl AudioFileCancel for Cancellation {
    fn is_cancelled(&self) -> bool {
        !self.active.load(Ordering::Acquire) || self.upstream.is_cancelled()
    }
}
struct Session {
    owner: Arc<parking_lot::Mutex<Option<SpoolWriter>>>,
    scheduler: Arc<migo_io::scheduler::IoScheduler>,
    active: Arc<AtomicBool>,
    cancel: Arc<dyn AudioFileCancel>,
    deadline: Instant,
}
impl Session {
    async fn fs<T: Send + 'static>(
        &self,
        op: impl FnOnce() -> Result<T, EngineError> + Send + 'static,
    ) -> Result<T, EngineError> {
        self.check()?;
        let scheduler = self.scheduler.clone();
        let active = self.active.clone();
        let cancel = self.cancel.clone();
        let deadline = self.deadline;
        let work = self
            .scheduler
            .pools()
            .submit_async_bytes(PoolKind::Fs, PriorityClass::ForegroundAsync, 0, move || {
                check(&scheduler, &active, cancel.as_ref(), deadline)?;
                let value = op()?;
                check(&scheduler, &active, cancel.as_ref(), deadline)?;
                Ok::<T, EngineError>(value)
            })
            .map_err(|_| error())?;
        let result = tokio::time::timeout_at(self.deadline, work)
            .await
            .map_err(|_| EngineError::new(ErrorCode::Timeout))?
            .map_err(|_| error())?
            .map_err(|_| error())??;
        self.check()?;
        Ok(result)
    }
    fn check(&self) -> Result<(), EngineError> {
        check(
            &self.scheduler,
            &self.active,
            self.cancel.as_ref(),
            self.deadline,
        )
    }
}
fn check(
    scheduler: &migo_io::scheduler::IoScheduler,
    active: &AtomicBool,
    cancel: &dyn AudioFileCancel,
    deadline: Instant,
) -> Result<(), EngineError> {
    if !active.load(Ordering::Acquire) || cancel.is_cancelled() {
        return Err(EngineError::new(ErrorCode::Cancelled));
    }
    scheduler.ensure_open().map_err(|_| error())?;
    if Instant::now() >= deadline {
        return Err(EngineError::new(ErrorCode::Timeout));
    }
    Ok(())
}
impl Spool {
    async fn finish(
        self: Arc<Self>,
        mut body: Box<dyn AudioSpoolBody>,
        cancel: Arc<dyn AudioFileCancel>,
        deadline: std::time::Instant,
    ) -> Result<Arc<dyn AudioFile>, EngineError> {
        let deadline = Instant::from_std(deadline);
        let active = Arc::new(AtomicBool::new(true));
        let _active = Active(active.clone());
        let session = Session {
            owner: Arc::new(parking_lot::Mutex::new(None)),
            scheduler: self.context.scheduler.clone(),
            active,
            cancel,
            deadline,
        };
        let expected = body.length();
        let dir = self.context.spool_dir.clone();
        let budget = self.budget.clone();
        let maximum = self.maximum;
        let owner = session.owner.clone();
        session
            .fs(move || {
                *owner.lock() = Some(
                    SpoolWriter::create(&dir, &budget, maximum, expected).map_err(|_| error())?,
                );
                Ok(())
            })
            .await?;
        loop {
            session.check()?;
            let chunk = tokio::time::timeout_at(
                deadline.min(Instant::now() + std::time::Duration::from_secs(30)),
                body.chunk(),
            )
            .await
            .map_err(|_| EngineError::new(ErrorCode::Timeout))??;
            let Some(chunk) = chunk else { break };
            let owner = session.owner.clone();
            session
                .fs(move || {
                    // Own the complete loan and its admission until real Fs completion.
                    let bytes = chunk;
                    owner
                        .lock()
                        .as_mut()
                        .ok_or_else(error)?
                        .append(bytes.as_ref())
                        .map_err(|_| error())
                })
                .await?;
        }
        let owner = session.owner.clone();
        let pools = session.scheduler.pools().clone();
        let sealed = session
            .fs(move || {
                owner
                    .lock()
                    .take()
                    .ok_or_else(error)?
                    .seal_with_budget(2 * 1024 * 1024, &pools)
                    .map(Arc::new)
                    .map_err(|_| error())
            })
            .await?;
        session.check()?;
        file_stream::wrap(
            sealed,
            self.context.clone(),
            deadline,
            Arc::new(Cancellation {
                active: session.active.clone(),
                upstream: session.cancel.clone(),
            }),
        )
        .await
        .map_err(|_| error())
    }
}
struct Sparse(migo_io::spool::SparseSpool);
impl shared::audio_file::AudioSparseSpool for Sparse {
    fn len(&self) -> u64 {
        self.0.len()
    }
    fn write_at(&self, offset: u64, bytes: &[u8]) -> Result<(), EngineError> {
        self.0.write_at(offset, bytes).map_err(|_| error())
    }
    fn read_at(&self, offset: u64, output: &mut [u8]) -> Result<(), EngineError> {
        self.0.read_at(offset, output).map_err(|_| error())
    }
}

impl AudioHttpSpool for Spool {
    fn sparse(
        &self,
        len: u64,
    ) -> Result<Box<dyn shared::audio_file::AudioSparseSpool>, EngineError> {
        migo_io::spool::SparseSpool::create(
            &self.context.spool_dir,
            &self.budget,
            len,
            self.maximum,
        )
        .map(|spool| Box::new(Sparse(spool)) as Box<dyn shared::audio_file::AudioSparseSpool>)
        .map_err(|error| EngineError::from_detail(ErrorCode::InputSaturated, error.to_string()))
    }
    fn prepare(
        self: Arc<Self>,
        body: Box<dyn AudioSpoolBody>,
        cancel: Arc<dyn AudioFileCancel>,
        deadline: std::time::Instant,
    ) -> AudioSpoolFuture<'static, Arc<dyn AudioFile>> {
        Box::pin(async move {
            let domain = self.context.scheduler.domain();
            tokio::select! {biased;
                _=domain.closed()=>Err(error()),
                result=self.finish(body,cancel,deadline)=>result,
            }
        })
    }
}

#[cfg(test)]
#[path = "audio_http_spool_tests.rs"]
mod tests;

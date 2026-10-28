//! Re-run only the bounded library probe at geometric prefix sizes. No header,
//! ID3 or container parser lives here; retained chunks move intact to the source.
use crate::{
    format_source::{self, Format},
    replay_body::Body,
    streaming::{StreamMsg, StreamingState},
    streaming_transport::while_active,
};
use migo_io::{domain::IoDomain, scheduler::IoScheduler};
use shared::error::{EngineError, EngineResult, ErrorCode};
use std::{
    io::{self, Read, Seek, SeekFrom},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use symphonia::core::{
    formats::probe::Hint,
    io::{MediaSource, MediaSourceStream},
};
use tokio::{sync::mpsc, time::Instant};
pub(crate) const MAX_PREFIX_BYTES: u64 = 64 * 1024;
const MAX_PREFIX_READS: usize = 128;
const SCRATCH_BYTES: u64 = 128 * 1024;
#[cfg(test)]
pub(crate) struct Hook {
    pub started: tokio::sync::Notify,
    pub reads: std::sync::atomic::AtomicUsize,
    pub release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
}
#[cfg(test)]
pub(crate) static HOOK: std::sync::Mutex<Option<Arc<Hook>>> = std::sync::Mutex::new(None);
fn error(e: impl std::fmt::Display) -> EngineError {
    EngineError::from_detail(ErrorCode::IoError, e.to_string())
}
struct Active(Arc<AtomicBool>);
impl Drop for Active {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}
fn check(domain: &IoDomain, active: &AtomicBool, deadline: Instant) -> io::Result<()> {
    if domain.is_closed() || !active.load(Ordering::Acquire) {
        return Err(io::Error::other("audio prefix cancelled or closed"));
    }
    if Instant::now() >= deadline {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "audio prefix deadline exceeded",
        ));
    }
    Ok(())
}
struct View<'a> {
    body: &'a Body,
    index: usize,
    offset: usize,
    read: u64,
    domain: &'a IoDomain,
    active: &'a AtomicBool,
    deadline: Instant,
}
impl Read for View<'_> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        check(self.domain, self.active, self.deadline)?;
        #[cfg(test)]
        if let Some(hook) = HOOK.lock().unwrap().as_ref() {
            hook.reads.fetch_add(1, Ordering::Relaxed);
        }
        let len = out.len().min((MAX_PREFIX_BYTES - self.read) as usize);
        let mut done = 0;
        while done < len {
            let Some(chunk) = self.body.prefix().get(self.index) else {
                break;
            };
            let bytes = &chunk.as_ref()[self.offset..];
            let n = bytes.len().min(len - done);
            out[done..done + n].copy_from_slice(&bytes[..n]);
            done += n;
            self.offset += n;
            if self.offset == chunk.data.len() {
                self.index += 1;
                self.offset = 0;
            }
        }
        self.read += done as u64;
        check(self.domain, self.active, self.deadline)?;
        Ok(done)
    }
}
impl Seek for View<'_> {
    fn seek(&mut self, _: SeekFrom) -> io::Result<u64> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "prefix is forward only",
        ))
    }
}
impl MediaSource for View<'_> {
    fn is_seekable(&self) -> bool {
        false
    }
    fn byte_len(&self) -> Option<u64> {
        None
    }
}
fn identify(
    body: &Body,
    domain: &IoDomain,
    active: &AtomicBool,
    deadline: Instant,
) -> EngineResult<Option<Format>> {
    check(domain, active, deadline).map_err(error)?;
    let view = View {
        body,
        index: 0,
        offset: 0,
        read: 0,
        domain,
        active,
        deadline,
    };
    let stream = MediaSourceStream::new(Box::new(view), Default::default());
    let result = symphonia::default::get_probe().probe_format(
        &Hint::new(),
        stream,
        format_source::metadata_options(),
    );
    check(domain, active, deadline).map_err(error)?;
    match result {
        Ok(found) => format_source::selected_format(found.info().format).map(Some),
        Err(e) if body.complete() && body.received() <= MAX_PREFIX_BYTES => Err(error(e)),
        Err(_) => Ok(None),
    }
}
pub(crate) async fn probe(
    mut body: Body,
    owner: &IoScheduler,
    deadline: Instant,
    tx: &mpsc::Sender<StreamMsg>,
    state: &Arc<StreamingState>,
) -> EngineResult<Option<(Body, Option<Format>)>> {
    let active = Arc::new(AtomicBool::new(true));
    let _guard = Active(active.clone());
    let mut target = 1024;
    let domain = owner.domain();
    loop {
        check(&domain, &active, deadline).map_err(error)?;
        while !body.complete() && body.received() < target && body.prefix().len() < MAX_PREFIX_READS
        {
            if !body.read_prefix(owner.pools(), deadline, tx, state).await? {
                return Ok(None);
            }
        }
        let lease = owner.pools().reserve_bytes(SCRATCH_BYTES).map_err(error)?;
        let worker_domain = domain.clone();
        let flag = active.clone();
        let cpu = owner.audio_cpu();
        #[cfg(test)]
        let hook = HOOK.lock().unwrap().clone();
        #[cfg(test)]
        let io_thread = std::thread::current().id();
        let job = move || {
            let _lease = lease;
            #[cfg(test)]
            if let Some(hook) = hook {
                hook.started.notify_one();
                let _ = hook.release.lock().unwrap().recv();
            }
            #[cfg(test)]
            assert_ne!(
                std::thread::current().id(),
                io_thread,
                "prefix parser ran on IO"
            );
            let result = identify(&body, &worker_domain, &flag, deadline);
            (body, result)
        };
        let operation = cpu.run(job);
        let Some(result) =
            while_active(tx, state, tokio::time::timeout_at(deadline, operation)).await
        else {
            return Ok(None);
        };
        let (returned, found) =
            result.map_err(|_| error("audio prefix CPU deadline exceeded"))??;
        body = returned;
        let found = found?;
        check(&domain, &active, deadline).map_err(error)?;
        if found.is_some()
            || body.complete()
            || body.received() >= MAX_PREFIX_BYTES
            || body.prefix().len() >= MAX_PREFIX_READS
        {
            // Only peek when Content-Length already proves no data remains.
            if found.is_some() && !body.complete() && body.at_declared_end() {
                if !body.read_prefix(owner.pools(), deadline, tx, state).await? {
                    return Ok(None);
                }
            }
            return Ok(Some((body, found)));
        }
        target = body
            .received()
            .max(target)
            .saturating_mul(2)
            .min(MAX_PREFIX_BYTES);
    }
}

//! Complete one HTTP response into a private bounded file before decoding.
//! No request retry, cross-representation ranges, parser or CPU loop lives here.
use crate::{
    local_source::{FileReader, MAX_CONTROL_BYTES},
    streaming::{
        BODY_IDLE_TIMEOUT, HttpClient, RESPONSE_HEADER_TIMEOUT, StreamMsg, StreamingState,
    },
    streaming_transport::while_active,
};
use bytes::Bytes;
use migo_io::{
    domain::IoDomain,
    pools::{ByteTicket, IoPools},
    read_at_file::FILE_CHUNK_BYTES,
    scheduler::IoScheduler,
    spool::{DEFAULT_SPOOL_FILE_BYTES, SealedSpool, SpoolBudget, SpoolWriter},
    task::{PoolKind, PriorityClass},
};
use parking_lot::Mutex;
use shared::error::{EngineError, EngineResult, ErrorCode};
use std::{
    io,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::{
    sync::mpsc,
    time::{Duration, Instant},
};
const MAX_HTTP_CHUNK_BYTES: usize = 1024 * 1024;
#[cfg(test)]
#[derive(Default)]
pub struct Trace {
    pub written: std::sync::atomic::AtomicU64,
    pub waiting: tokio::sync::Notify,
}
#[derive(Clone)]
pub struct Limits {
    pub dir: PathBuf,
    pub budget: SpoolBudget,
    pub max_bytes: u64,
    pub deadline: Instant,
    #[cfg(test)]
    pub trace: Option<Arc<Trace>>,
}
impl Limits {
    pub fn session(paths: &shared::vfs::GamePaths) -> Self {
        Self {
            dir: paths.spool_dir().to_path_buf(),
            budget: SpoolBudget::process(),
            max_bytes: DEFAULT_SPOOL_FILE_BYTES,
            deadline: Instant::now() + Duration::from_secs(120),
            #[cfg(test)]
            trace: None,
        }
    }
}
fn error(e: impl std::fmt::Display) -> EngineError {
    EngineError::from_detail(ErrorCode::IoError, e.to_string())
}
fn check(domain: &IoDomain, active: &AtomicBool, deadline: Instant) -> io::Result<()> {
    if domain.is_closed() || !active.load(Ordering::Acquire) || Instant::now() >= deadline {
        return Err(io::Error::other("spool cancelled, closed or expired"));
    }
    Ok(())
}
struct Active(Arc<AtomicBool>);
impl Drop for Active {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}
async fn fs<T: Send + 'static>(
    pools: &IoPools,
    domain: Arc<IoDomain>,
    active: Arc<AtomicBool>,
    deadline: Instant,
    bytes: u64,
    op: impl FnOnce() -> io::Result<T> + Send + 'static,
) -> EngineResult<T> {
    active.store(true, Ordering::Release);
    let _guard = Active(active.clone());
    check(&domain, &active, deadline).map_err(error)?;
    let worker_domain = domain.clone();
    let task = pools
        .submit_async_bytes(
            PoolKind::Fs,
            PriorityClass::ForegroundAsync,
            bytes,
            move || {
                check(&worker_domain, &active, deadline)?;
                let value = op()?;
                check(&worker_domain, &active, deadline)?;
                Ok::<T, io::Error>(value)
            },
        )
        .map_err(error)?;
    let value = tokio::time::timeout_at(deadline, task)
        .await
        .map_err(|_| error("spool IO deadline exceeded"))?
        .map_err(error)?
        .map_err(|_| error("spool IO worker panicked"))?
        .map_err(error)?;
    if domain.is_closed() || Instant::now() >= deadline {
        return Err(error("late spool IO completion"));
    }
    Ok(value)
}
struct BodyChunk {
    data: Bytes,
    _credit: Arc<ByteTicket>,
}
impl AsRef<[u8]> for BodyChunk {
    fn as_ref(&self) -> &[u8] {
        &self.data
    }
}
impl BodyChunk {
    fn new(data: Bytes, pools: &IoPools) -> EngineResult<Self> {
        if data.len() > MAX_HTTP_CHUNK_BYTES {
            return Err(error("HTTP spool chunk exceeds bound"));
        }
        let credit = pools.reserve_bytes(data.len() as u64).map_err(error)?;
        Ok(Self {
            data,
            _credit: Arc::new(credit),
        })
    }
    fn slice(&self, start: usize, end: usize) -> Self {
        Self {
            data: self.data.slice(start..end),
            _credit: self._credit.clone(),
        }
    }
}
struct Session {
    owner: Arc<Mutex<Option<SpoolWriter>>>,
    pools: IoPools,
    domain: Arc<IoDomain>,
    active: Arc<AtomicBool>,
    deadline: Instant,
    failed: bool,
    #[cfg(test)]
    delay: Duration,
}
impl Session {
    async fn create(
        scheduler: &IoScheduler,
        limits: Limits,
        expected: Option<u64>,
    ) -> EngineResult<Self> {
        scheduler.ensure_open().map_err(error)?;
        let pools = scheduler.pools().clone();
        let domain = scheduler.domain();
        let active = Arc::new(AtomicBool::new(false));
        let deadline = limits.deadline;
        let writer = fs(
            &pools,
            domain.clone(),
            active.clone(),
            deadline,
            0,
            move || SpoolWriter::create(&limits.dir, &limits.budget, limits.max_bytes, expected),
        )
        .await?;
        Ok(Self {
            owner: Arc::new(Mutex::new(Some(writer))),
            pools,
            domain,
            active,
            deadline,
            failed: false,
            #[cfg(test)]
            delay: Duration::ZERO,
        })
    }
    async fn append(&mut self, chunk: BodyChunk) -> EngineResult<()> {
        if self.failed {
            return Err(error("spool session previously failed"));
        }
        self.failed = true;
        let owner = self.owner.clone();
        let domain = self.domain.clone();
        let active = self.active.clone();
        let deadline = self.deadline;
        #[cfg(test)]
        let delay = self.delay;
        fs(
            &self.pools,
            self.domain.clone(),
            self.active.clone(),
            self.deadline,
            0,
            move || {
                #[cfg(test)]
                std::thread::sleep(delay);
                check(&domain, &active, deadline)?;
                // Capture the whole BodyChunk, including its shared byte ticket.
                let mut writer = owner.lock();
                let writer = writer
                    .as_mut()
                    .ok_or_else(|| io::Error::other("spool already sealed"))?;
                writer.append(chunk.as_ref())
            },
        )
        .await?;
        self.failed = false;
        Ok(())
    }
    async fn seal(self) -> EngineResult<SealedSpool> {
        if self.failed {
            return Err(error("failed spool cannot seal"));
        }
        let owner = self.owner;
        fs(
            &self.pools,
            self.domain,
            self.active,
            self.deadline,
            FILE_CHUNK_BYTES as u64,
            move || {
                let writer = owner
                    .lock()
                    .take()
                    .ok_or_else(|| io::Error::other("spool already sealed"))?;
                writer.seal(MAX_CONTROL_BYTES)
            },
        )
        .await
    }
}
/// Consume the already-open response once. The caller selects this path before
/// creating a decoder or publishing Ready/PCM; no second GET is issued here.
async fn finish_response(
    response: migo_services::network::http::Response,
    scheduler: &IoScheduler,
    limits: Limits,
    tx: &mpsc::Sender<StreamMsg>,
    state: &Arc<StreamingState>,
) -> EngineResult<Option<FileReader>> {
    if response.decoded_body_bytes() != 0 {
        return Err(error("spool requires an unread response body"));
    }
    if response.status() != http::StatusCode::OK
        || response.headers().contains_key(http::header::CONTENT_RANGE)
    {
        return Err(error("spool requires a complete HTTP response"));
    }
    if state.is_cancelled() || tx.is_closed() {
        return Ok(None);
    }
    #[cfg(test)]
    let trace = limits.trace.clone();
    let expected = response.content_length();
    let deadline = limits.deadline;
    let Some(session) = while_active(tx, state, Session::create(scheduler, limits, expected)).await
    else {
        return Ok(None);
    };
    let mut session = session?;
    let mut response = response;
    loop {
        #[cfg(test)]
        if let Some(trace) = &trace {
            trace.waiting.notify_one();
        }
        let Some(chunk) = while_active(
            tx,
            state,
            tokio::time::timeout_at(
                deadline.min(Instant::now() + BODY_IDLE_TIMEOUT),
                response.chunk(),
            ),
        )
        .await
        else {
            return Ok(None);
        };
        let chunk = chunk
            .map_err(|_| error("spool body deadline exceeded"))?
            .map_err(error)?;
        let Some(chunk) = chunk else { break };
        let chunk = BodyChunk::new(chunk, scheduler.pools())?;
        for at in (0..chunk.data.len()).step_by(FILE_CHUNK_BYTES) {
            let part = chunk.slice(at, (at + FILE_CHUNK_BYTES).min(chunk.data.len()));
            #[cfg(test)]
            let written = part.data.len() as u64;
            let Some(result) = while_active(tx, state, session.append(part)).await else {
                return Ok(None);
            };
            result?;
            #[cfg(test)]
            if let Some(trace) = &trace {
                trace.written.fetch_add(written, Ordering::Release);
            }
        }
    }
    let Some(sealed) = while_active(tx, state, session.seal()).await else {
        return Ok(None);
    };
    if state.is_cancelled() || tx.is_closed() {
        return Ok(None);
    }
    FileReader::open_spool(sealed?, scheduler, deadline).map(Some)
}
/// Network policy, redirect accounting and HTTP body errors remain those of
/// the shared client. This is an explicit pre-PCM spool selection, not a retry
/// of an already-published range/codec stream.
async fn download(
    client: HttpClient,
    url: String,
    scheduler: &IoScheduler,
    limits: Limits,
    tx: &mpsc::Sender<StreamMsg>,
    state: &Arc<StreamingState>,
) -> EngineResult<Option<FileReader>> {
    scheduler.ensure_open().map_err(error)?;
    if Instant::now() >= limits.deadline {
        return Err(error("spool setup deadline exceeded"));
    }
    let parsed = migo_services::network::url::Url::parse(&url).map_err(error)?;
    let deadline = limits
        .deadline
        .min(Instant::now() + RESPONSE_HEADER_TIMEOUT);
    let Some(response) = while_active(
        tx,
        state,
        tokio::time::timeout_at(
            deadline,
            client
                .get(migo_services::network::gate::GateKind::AudioStream, parsed)
                .header(
                    http::header::ACCEPT_ENCODING,
                    http::HeaderValue::from_static("identity"),
                )
                .send(),
        ),
    )
    .await
    else {
        return Ok(None);
    };
    let result = finish_response(
        response
            .map_err(|_| error("spool header deadline exceeded"))?
            .map_err(error)?,
        scheduler,
        limits,
        tx,
        state,
    )
    .await;
    drop(client);
    result
}

async fn in_domain<T>(
    scheduler: &IoScheduler,
    operation: impl std::future::Future<Output = EngineResult<T>>,
) -> EngineResult<T> {
    let domain = scheduler.domain();
    tokio::select! { biased; _=domain.closed()=>Err(error("spool domain closed")),result=operation=>result }
}
pub async fn response(
    response: migo_services::network::http::Response,
    scheduler: &IoScheduler,
    limits: Limits,
    tx: &mpsc::Sender<StreamMsg>,
    state: &Arc<StreamingState>,
) -> EngineResult<Option<FileReader>> {
    in_domain(
        scheduler,
        finish_response(response, scheduler, limits, tx, state),
    )
    .await
}
pub async fn http(
    client: HttpClient,
    url: String,
    scheduler: &IoScheduler,
    limits: Limits,
    tx: &mpsc::Sender<StreamMsg>,
    state: &Arc<StreamingState>,
) -> EngineResult<Option<FileReader>> {
    in_domain(
        scheduler,
        download(client, url, scheduler, limits, tx, state),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    fn limits(name: &str) -> Limits {
        Limits {
            dir: std::path::PathBuf::from(std::env::var_os("W09_M4A_FIXTURES").unwrap())
                .parent()
                .unwrap()
                .join(format!("spool-{name}-{}", std::process::id())),
            budget: SpoolBudget::new(8192, 2).unwrap(),
            max_bytes: 4096,
            deadline: Instant::now() + Duration::from_secs(2),
            trace: None,
        }
    }
    #[tokio::test]
    async fn queued_cancelled_writes_keep_credit_and_cannot_reuse_or_seal() {
        let owner = IoScheduler::new(0);
        let l = limits("cancel");
        let budget = l.budget.clone();
        let dir = l.dir.clone();
        let mut session = Session::create(&owner, l, Some(4096)).await.unwrap();
        session.delay = Duration::from_millis(200);
        let available = |pools: &IoPools| match pools.reserve_bytes(u64::MAX) {
            Err(migo_io::pools::ByteAdmitError::BytesExhausted { available, .. }) => available,
            _ => panic!("expected byte probe"),
        };
        let before = available(owner.pools());
        let data = BodyChunk::new(Bytes::from(vec![7; 4096]), owner.pools()).unwrap();
        assert_eq!(available(owner.pools()), before - 4096);
        let weak = Arc::downgrade(&data._credit);
        let source = session.owner.clone();
        assert!(
            tokio::time::timeout(Duration::from_millis(20), session.append(data))
                .await
                .is_err()
        );
        assert!(
            weak.upgrade().is_some(),
            "cancelled job lost its byte credit"
        );
        assert!(
            session
                .append(BodyChunk::new(Bytes::new(), owner.pools()).unwrap())
                .await
                .is_err()
        );
        assert!(session.seal().await.is_err());
        tokio::time::sleep(Duration::from_millis(240)).await;
        assert!(weak.upgrade().is_none());
        assert_eq!(available(owner.pools()), before);
        // An operation cancelled before its disk call must not touch the writer.
        assert!(source.lock().take().unwrap().seal(8).is_err());
        drop(source);
        let _next = SpoolWriter::create(&dir, &budget, 4096, None).unwrap();
    }
    #[tokio::test]
    async fn absolute_deadline_and_closed_domain_reject_setup_and_late_writes() {
        let owner = IoScheduler::new(0);
        let mut l = limits("expired");
        l.deadline = Instant::now();
        assert!(Session::create(&owner, l, None).await.is_err());
        let mut l = limits("late");
        l.deadline = Instant::now() + Duration::from_millis(30);
        let mut session = Session::create(&owner, l, Some(1)).await.unwrap();
        session.delay = Duration::from_millis(100);
        let source = session.owner.clone();
        assert!(
            session
                .append(BodyChunk::new(Bytes::from_static(b"x"), owner.pools()).unwrap())
                .await
                .is_err()
        );
        drop(session);
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(source.lock().take().unwrap().seal(8).is_err());
        owner.domain().close_all();
        assert!(
            Session::create(&owner, limits("closed"), None)
                .await
                .is_err()
        );
    }
    #[tokio::test]
    async fn sealed_admission_keeps_the_deadline_owner_and_needs_no_io_credit() {
        let owner = IoScheduler::new(0);
        async fn sealed(owner: &IoScheduler, name: &str) -> SealedSpool {
            let mut writer = Session::create(owner, limits(name), Some(3)).await.unwrap();
            writer
                .append(BodyChunk::new(Bytes::from_static(b"xyz"), owner.pools()).unwrap())
                .await
                .unwrap();
            writer.seal().await.unwrap()
        }
        let spool = sealed(&owner, "admission").await;
        let expired = sealed(&owner, "admission-expired").await;
        let closed = sealed(&owner, "admission-closed").await;
        let available = match owner.pools().reserve_bytes(u64::MAX) {
            Err(migo_io::pools::ByteAdmitError::BytesExhausted { available, .. }) => available,
            _ => panic!("expected byte probe"),
        };
        let credit = owner.pools().reserve_bytes(available).unwrap();
        let mut reader =
            FileReader::open_spool(spool, &owner, Instant::now() + Duration::from_secs(1))
                .expect("sealed admission requested transient IO credit");
        assert_eq!(reader.len(), 3);
        assert!(
            FileReader::open_spool(expired, &owner, Instant::now()).is_err(),
            "sealed admission renewed its absolute deadline"
        );
        drop(credit);
        reader.chunk_bytes = 1;
        let a = reader.chunk().await.unwrap().unwrap();
        let b = reader.chunk().await.unwrap().unwrap();
        assert_eq!((a.as_ref(), b.as_ref()), (b"x".as_slice(), b"y".as_slice()));
        assert!(
            reader.chunk().await.is_err(),
            "sealed admission replaced the two-slot media pool"
        );
        drop((reader, a, b));
        owner.domain().close_all();
        assert!(
            FileReader::open_spool(closed, &owner, Instant::now() + Duration::from_secs(1))
                .is_err(),
            "sealed admission accepted a closed domain"
        );
    }
    #[tokio::test]
    async fn spool_setup_write_seal_and_media_do_not_use_the_pack_class() {
        let owner = IoScheduler::new(0);
        let _release =
            crate::isomp4_source_contracts::transport::local::package::block_pack(&owner).await;
        let operation = async {
            let mut writer = Session::create(&owner, limits("fs-class"), Some(1))
                .await
                .unwrap();
            writer
                .append(BodyChunk::new(Bytes::from_static(b"x"), owner.pools()).unwrap())
                .await
                .unwrap();
            let spool = writer.seal().await.unwrap();
            let mut reader =
                FileReader::open_spool(spool, &owner, Instant::now() + Duration::from_secs(1))
                    .unwrap();
            assert_eq!(reader.chunk().await.unwrap().unwrap().as_ref(), b"x");
        };
        tokio::time::timeout(Duration::from_secs(1), operation)
            .await
            .expect("spool incorrectly waited for Pack class");
    }
}

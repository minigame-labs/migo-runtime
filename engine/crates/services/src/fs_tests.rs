//! The scheduling and pack-handling rules of `fs`, below the calls.

use std::{
    future::Future,
    io,
    panic::{AssertUnwindSafe, catch_unwind},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::{Context, Poll, Waker},
    time::Duration,
};

use migo_io::{
    domain::DomainError,
    scheduler::{IoScheduler, RouteDecision},
    task::{BackendKind, IoRequest, PoolKind, PriorityClass, RequestKind},
};
use shared::{
    error::{EngineError, ErrorCode},
    protocol::io_cmd::OpenFlag,
    vfs::{FileOp, MountBackend, MountTable},
};

use super::{
    ResolvedPath, archive_read_request, code_relative, copy_pack_file_async,
    materialize_pack_to_temp_async, materialize_pack_to_temp_checked, read_request,
    resolve_path_vfs, run_domain_async, run_pack_async,
};

/// No production path in this module escapes to tokio's unbounded blocking
/// pool: every blocking step is admitted by the scheduler.
#[test]
fn q12_production_file_ops_do_not_escape_to_tokio_blocking_pool() {
    let source = include_str!("fs.rs");
    let production = source
        .split("#[cfg(test)]")
        .next()
        .expect("fs production source");

    assert!(
        !production.contains("tokio::task::spawn_blocking"),
        "production file ops bypass the bounded R5 executor"
    );
}

/// A whole-file read carries no `length`, so the estimate rests entirely on
/// the size hint. Without one the request has to assume `MAX_READ_LENGTH`
/// and every `readFile` of a small config or atlas descriptor pays a worker
/// round-trip that costs more than reading the bytes.
#[test]
fn small_whole_file_reads_stay_inline_when_the_size_is_known() {
    let scheduler = IoScheduler::new(1220);

    let hinted = read_request(
        BackendKind::Pack,
        RequestKind::Sync,
        None,
        Some(200), // a 200-byte JSON, the shape this path exists for
    );
    assert_eq!(scheduler.classify(&hinted), RouteDecision::Inline);

    let unhinted = read_request(BackendKind::Pack, RequestKind::Sync, None, None);
    assert_eq!(
        scheduler.classify(&unhinted),
        RouteDecision::Delegated(PoolKind::Pack),
        "an unknown size must stay conservative and delegate"
    );
}

/// The hint narrows the estimate; it must never widen it past what the
/// caller asked for, or a large file would drag a small ranged read out of
/// the inline path.
#[test]
fn size_hint_never_raises_the_estimate_above_the_requested_length() {
    let scheduler = IoScheduler::new(1221);

    let small_read_of_big_file = read_request(
        BackendKind::Filesystem,
        RequestKind::Sync,
        Some(512),
        Some(64 * 1024 * 1024),
    );
    assert_eq!(
        scheduler.classify(&small_read_of_big_file),
        RouteDecision::Inline
    );
}

/// A hint smaller than the requested length is the truth about how many
/// bytes will come back, so it decides the classification.
#[test]
fn size_hint_lowers_the_estimate_below_an_oversized_request() {
    let scheduler = IoScheduler::new(1222);

    // `readFileSync(path, {length: 8MB})` against a 100-byte file.
    let request = read_request(
        BackendKind::Filesystem,
        RequestKind::Sync,
        Some(8 * 1024 * 1024),
        Some(100),
    );
    assert_eq!(scheduler.classify(&request), RouteDecision::Inline);
}

/// Large reads must keep going to a worker whatever the hint says, or a
/// multi-megabyte `readFileSync` would run a full decode-length copy on the
/// V8 thread outside the pool's accounting.
#[test]
fn large_whole_file_reads_still_delegate() {
    let scheduler = IoScheduler::new(1223);

    let request = read_request(
        BackendKind::Filesystem,
        RequestKind::Sync,
        None,
        Some(8 * 1024 * 1024),
    );
    assert_eq!(
        scheduler.classify(&request),
        RouteDecision::Delegated(PoolKind::Fs)
    );
}

#[test]
fn q12_async_domain_jobs_run_on_r5_fs_worker() {
    let scheduler = Arc::new(IoScheduler::new(1210));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    let thread_name = runtime
        .block_on(run_domain_async(scheduler, || {
            Ok::<_, DomainError>(
                std::thread::current()
                    .name()
                    .unwrap_or("unnamed")
                    .to_string(),
            )
        }))
        .unwrap();

    assert!(thread_name.starts_with("Migo-IO-"));
}

#[test]
fn pack_digest_job_runs_on_worker_and_reresolves_mount() {
    use shared::vfs::package::{PackSource, PackageWriter};

    let dir = temp_dir("pack_digest_worker");
    let package_path = dir.join("base.mpkg");
    let file = std::fs::File::create(&package_path).unwrap();
    let mut writer = PackageWriter::new(std::io::BufWriter::new(file)).unwrap();
    writer.add_entry("payload.bin", b"pack payload").unwrap();
    writer.finish("base", "1").unwrap();

    let mount_table = Arc::new(MountTable::new(dir.clone()));
    mount_table.swap_base(Arc::new(
        PackSource::open(&package_path, "base", "1").unwrap(),
    ));
    let virtual_path = "/code/payload.bin".to_string();
    let scheduler = Arc::new(IoScheduler::new(1212));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let worker_mount = Arc::clone(&mount_table);
    let (thread_name, size, digest) = runtime
        .block_on(run_pack_async(
            scheduler,
            IoRequest::ReadFile {
                backend: BackendKind::Pack,
                request: RequestKind::Async,
                priority: PriorityClass::ForegroundAsync,
                estimated_bytes: shared::protocol::io_cmd::MAX_READ_LENGTH as usize,
            },
            move || {
                let resolved = resolve_path_vfs(
                    None,
                    Some(worker_mount.as_ref()),
                    &virtual_path,
                    FileOp::Read,
                )
                .map_err(|e| EngineError::new(ErrorCode::IoError).with_detail(e.message))?;
                let ResolvedPath::Pack { virtual_path } = resolved else {
                    return Err(EngineError::new(ErrorCode::IoError)
                        .with_detail("mount was not re-resolved as Pack"));
                };
                let (size, digest) = worker_mount
                    .get_file_info(code_relative(&virtual_path), "sha256")
                    .map_err(|e| EngineError::new(ErrorCode::IoError).with_detail(e.to_string()))?;
                Ok::<_, EngineError>((
                    std::thread::current()
                        .name()
                        .unwrap_or("unnamed")
                        .to_string(),
                    size,
                    digest,
                ))
            },
        ))
        .unwrap();
    assert!(
        thread_name.starts_with("Migo-IO-"),
        "digest ran on {thread_name}"
    );
    assert_eq!(size, b"pack payload".len() as u64);
    assert!(!digest.is_empty());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn q12_domain_adapter_preserves_open_and_positioned_write_semantics() {
    let dir = temp_dir("q12_domain_file_semantics");
    let path = dir.join("file.bin");
    std::fs::write(&path, b"abc").unwrap();
    let scheduler = Arc::new(IoScheduler::new(1215));
    let domain = scheduler.domain();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    let domain_for_open = Arc::clone(&domain);
    let path_for_open = path.clone();
    let rid = runtime
        .block_on(run_domain_async(Arc::clone(&scheduler), move || {
            domain_for_open.open_file(&path_for_open, OpenFlag::ReadWrite, None, None)
        }))
        .unwrap();
    let domain_for_write = Arc::clone(&domain);
    let written = runtime
        .block_on(run_domain_async(Arc::clone(&scheduler), move || {
            domain_for_write.write_file(rid, b"Z", Some(1))
        }))
        .unwrap();

    assert_eq!(written, 1);
    domain.close_file(rid).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"aZc");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn q12_zip_entry_reads_use_archive_class() {
    let scheduler = IoScheduler::new(1211);

    assert_eq!(
        scheduler.classify(&archive_read_request()),
        RouteDecision::Delegated(PoolKind::Archive)
    );
}

#[test]
fn q12_closed_scheduler_rejects_domain_job_before_it_runs() {
    let scheduler = Arc::new(IoScheduler::new(1212));
    scheduler.close();
    let ran = Arc::new(AtomicBool::new(false));
    let ran_in_job = Arc::clone(&ran);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    let result = runtime.block_on(run_domain_async(scheduler, move || {
        ran_in_job.store(true, Ordering::SeqCst);
        Ok::<_, DomainError>(())
    }));

    assert!(matches!(
        result,
        Err(ref error) if error.message == "IO worker pool closed"
    ));
    assert!(!ran.load(Ordering::SeqCst));
}

#[test]
fn q12_cancelling_waiter_does_not_abort_in_flight_domain_job() {
    let scheduler = Arc::new(IoScheduler::new(1213));
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel(0);
    let (finished_tx, finished_rx) = std::sync::mpsc::channel();
    let mut future = Box::pin(run_domain_async(scheduler, move || {
        started_tx.send(()).unwrap();
        release_rx.recv().unwrap();
        finished_tx.send(()).unwrap();
        Ok::<_, DomainError>(())
    }));
    let mut context = Context::from_waker(Waker::noop());

    assert!(matches!(
        Future::poll(future.as_mut(), &mut context),
        Poll::Pending
    ));
    started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    drop(future);
    release_tx.send(()).unwrap();

    finished_rx.recv_timeout(Duration::from_secs(5)).unwrap();
}

#[test]
fn q12_domain_adapter_preserves_worker_panic_payload() {
    let scheduler = Arc::new(IoScheduler::new(1214));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    let result = catch_unwind(AssertUnwindSafe(|| {
        let _: Result<(), crate::ServiceError> = runtime
            .block_on(run_domain_async(scheduler, || {
                panic!("q12-domain-worker-panic")
            }));
    }));
    let payload = result.expect_err("worker panic must propagate to the host boundary");
    let message = payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str));

    assert_eq!(message, Some("q12-domain-worker-panic"));
}

fn temp_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("migo_services_fs_{label}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[derive(Debug)]
struct TrackingBackend {
    data: Vec<u8>,
    calls: Arc<AtomicUsize>,
}

impl MountBackend for TrackingBackend {
    fn read(&self, _relative_path: &str) -> io::Result<Vec<u8>> {
        Ok(self.data.clone())
    }

    fn exists(&self, _relative_path: &str) -> bool {
        true
    }

    fn real_path(&self, _relative_path: &str) -> Option<PathBuf> {
        None
    }

    fn root_dir(&self) -> Option<&Path> {
        None
    }

    fn copy_to_writer(&self, _relative_path: &str, writer: &mut dyn io::Write) -> io::Result<()> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        writer.write_all(&self.data)
    }

    fn is_file(&self, relative_path: &str) -> bool {
        relative_path == "copy.txt"
    }
}

#[test]
fn async_pack_copy_uses_scheduler_worker_path() {
    let scheduler = Arc::new(IoScheduler::new(19));
    let dir = temp_dir("pack_copy_async");
    let base = dir.join("base");
    let dest = dir.join("dest.txt");
    std::fs::create_dir_all(&base).unwrap();

    let mount_table = Arc::new(MountTable::new(base));
    let calls = Arc::new(AtomicUsize::new(0));
    assert!(mount_table.mount_overlay(
        "overlay".to_string(),
        String::new(),
        Arc::new(TrackingBackend {
            data: b"pack-copy".to_vec(),
            calls: Arc::clone(&calls),
        }),
    ));

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime
        .block_on(copy_pack_file_async(
            Arc::clone(&scheduler),
            Arc::clone(&mount_table),
            "copy.txt".to_string(),
            dest.clone(),
        ))
        .unwrap();

    assert_eq!(std::fs::read(&dest).unwrap(), b"pack-copy");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn async_pack_materialization_uses_scheduler_worker_path() {
    let scheduler = Arc::new(IoScheduler::new(23));
    let dir = temp_dir("pack_materialize_async");
    let base = dir.join("base");
    std::fs::create_dir_all(&base).unwrap();

    let mount_table = Arc::new(MountTable::new(base));
    let calls = Arc::new(AtomicUsize::new(0));
    assert!(mount_table.mount_overlay(
        "overlay".to_string(),
        String::new(),
        Arc::new(TrackingBackend {
            data: b"pack-materialized".to_vec(),
            calls: Arc::clone(&calls),
        }),
    ));

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let materialized = runtime
        .block_on(materialize_pack_to_temp_async(
            Arc::clone(&scheduler),
            Arc::clone(&mount_table),
            "/code/copy.txt".to_string(),
            ".materialized",
        ))
        .unwrap();

    assert_eq!(std::fs::read(&materialized).unwrap(), b"pack-materialized");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let _ = std::fs::remove_file(&materialized);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn sync_pack_materialization_skips_copy_when_scheduler_is_closed() {
    let scheduler = IoScheduler::new(29);
    scheduler.close();

    let dir = temp_dir("pack_materialize_sync_closed");
    let base = dir.join("base");
    std::fs::create_dir_all(&base).unwrap();

    let mount_table = MountTable::new(base);
    let calls = Arc::new(AtomicUsize::new(0));
    assert!(mount_table.mount_overlay(
        "overlay".to_string(),
        String::new(),
        Arc::new(TrackingBackend {
            data: b"pack-materialized".to_vec(),
            calls: Arc::clone(&calls),
        }),
    ));

    let result = materialize_pack_to_temp_checked(
        &scheduler,
        Some(&mount_table),
        "/code/copy.txt",
        ".materialized",
    );

    assert!(matches!(result, Err(error) if error.message == "IO worker pool closed"));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let _ = std::fs::remove_dir_all(&dir);
}

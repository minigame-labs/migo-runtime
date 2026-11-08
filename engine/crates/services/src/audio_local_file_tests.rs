use super::*;
use migo_io::task::{PoolKind, PriorityClass};
use std::{future::Future, sync::atomic::Ordering, time::Duration};

const WAIT: Duration = Duration::from_secs(5);
fn bytes(name: &str, length: usize) -> (std::path::PathBuf, Arc<VirtualFS>, Vec<u8>) {
    let (base, _, vfs) = sandbox(name);
    let data = (0..length)
        .map(|n| ((n * 73 + 19) % 251) as u8)
        .collect::<Vec<_>>();
    fs::write(base.join("user/clip"), &data).unwrap();
    (base, vfs, data)
}
fn read(
    vfs: Arc<VirtualFS>,
    context: LocalAudioFile,
) -> impl Future<Output = Result<Vec<u8>, ServiceError>> {
    open_local_audio_source(
        Some(vfs),
        LocalAudioSource::Sandboxed {
            virtual_path: "/user/clip".into(),
        },
        Some(context),
    )
}
fn drain(context: &LocalAudioFile, rt: &tokio::runtime::Runtime) {
    let done = context
        .scheduler
        .pools()
        .submit_async_bytes(PoolKind::Fs, PriorityClass::ForegroundAsync, 0, || ())
        .unwrap();
    rt.block_on(done).unwrap().unwrap();
}

#[test]
fn complete_snapshot_bytes_survive_source_replacement_after_sealing() {
    for length in [0, 17, 65535, 65536, 65537, 1_048_593] {
        let (base, vfs, expected) = bytes("migo_audio_file_snapshot", length);
        let mut context = file_context(&base);
        let source = base.join("user/clip");
        context.hooks.after_seal = Some(Arc::new(move || {
            fs::write(&source, b"a different source").unwrap()
        }));
        let actual = runtime()
            .block_on(read(vfs, context.clone()))
            .expect("snapshot source refused complete bytes");
        assert_eq!(
            actual, expected,
            "snapshot bytes changed after the original source changed"
        );
        assert!(actual.capacity() <= MAX_ENCODED_AUDIO_BYTES);
        assert!(
            context.scheduler.metrics().delegated_runs > 0,
            "local source bypassed session Fs scheduling"
        );
        let _ = fs::remove_dir_all(base);
    }
}

#[test]
fn sandbox_file_requires_its_session_context_and_never_falls_back() {
    let (base, vfs, _) = bytes("migo_audio_file_context", 19);
    let error = runtime()
        .block_on(open_local_audio_source(
            Some(vfs),
            LocalAudioSource::Sandboxed {
                virtual_path: "/user/clip".into(),
            },
            None,
        ))
        .unwrap_err();
    assert_eq!(
        error.class, CLASS_AUDIO_FILE_ERROR,
        "missing session context bypassed file admission"
    );
    assert!(error.message.contains("context"));
    let _ = fs::remove_dir_all(base);
}

#[test]
fn exact_source_limit_and_typed_oversize_keep_the_existing_error_class() {
    let (base, vfs, _) = bytes("migo_audio_file_limits", 0);
    let path = base.join("user/clip");
    fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(MAX_ENCODED_AUDIO_BYTES as u64)
        .unwrap();
    let context = file_context(&base);
    let rt = runtime();
    let actual = rt.block_on(read(vfs.clone(), context.clone())).unwrap();
    assert_eq!(actual.len(), MAX_ENCODED_AUDIO_BYTES);
    assert!(actual.iter().all(|x| *x == 0));
    fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(MAX_ENCODED_AUDIO_BYTES as u64 + 1)
        .unwrap();
    let error = rt.block_on(read(vfs, context)).unwrap_err();
    assert_eq!(
        error.class, CLASS_AUDIO_ERROR,
        "snapshot input limit lost its existing error class"
    );
    assert!(
        error.message.contains("InputSaturated"),
        "snapshot oversize lost typed input saturation"
    );
    let _ = fs::remove_dir_all(base);
}

#[test]
fn unsafe_and_missing_sources_return_only_the_existing_public_file_error() {
    let (base, vfs, _) = bytes("migo_audio_file_policy", 17);
    let rt = runtime();
    let mut paths = vec!["/user/missing", "/user/../outside", "/etc/passwd"];
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("/etc/passwd", base.join("user/linked")).unwrap();
        paths.push("/user/linked");
    }
    for path in paths {
        let context = file_context(&base);
        let error = rt
            .block_on(open_local_audio_source(
                Some(vfs.clone()),
                LocalAudioSource::Sandboxed {
                    virtual_path: path.into(),
                },
                Some(context),
            ))
            .unwrap_err();
        assert_eq!(
            error.class, CLASS_AUDIO_FILE_ERROR,
            "unsafe file crossed the local source boundary"
        );
        assert_eq!(error.message, "Failed to open local audio file");
        assert!(!error.message.contains(&base.to_string_lossy()[..]));
    }
    let _ = fs::remove_dir_all(base);
}

#[test]
fn source_queue_saturation_precedes_any_filesystem_work() {
    let (base, vfs, _) = bytes("migo_audio_file_queue", 17);
    let context = file_context(&base);
    let (tx, rx) = hosted();
    let mut held = Vec::new();
    while let Ok(permit) = reserve(&tx, MAX_ENCODED_AUDIO_BYTES) {
        held.push(permit);
        assert!(held.len() < 1024);
    }
    let load = started_load(
        tx,
        InnerAudioSources {
            code_dir: None,
            vfs: Some(vfs),
            local_file: Some(context.clone()),
            admit_remote: |_: &Url| -> Result<(), ServiceError> { unreachable!() },
        },
        1,
        "/user/clip".into(),
        &rx,
    );
    let error = runtime().block_on(load).unwrap_err();
    assert!(
        error.message.contains("InputSaturated"),
        "local input did not reserve before file work"
    );
    assert_eq!(
        context.scheduler.metrics().delegated_runs,
        0,
        "saturated source still started filesystem work"
    );
    assert!(!context.spool_dir.exists());
    let _ = fs::remove_dir_all(base);
}

#[test]
fn replacing_a_source_cancels_queued_file_capture_before_it_opens() {
    let (base, vfs, _) = bytes("migo_audio_file_cancel", 131089);
    let mut context = file_context(&base);
    context.scheduler =
        Arc::new(migo_io::scheduler::IoScheduler::isolated_for_admission_probe(995, 1, 128 * 1024));
    let (started, waiting) = std::sync::mpsc::channel();
    let (release, held) = std::sync::mpsc::channel();
    let blocker = context
        .scheduler
        .pools()
        .submit_async_bytes(PoolKind::Fs, PriorityClass::ForegroundAsync, 0, move || {
            started.send(()).unwrap();
            held.recv_timeout(WAIT).unwrap();
        })
        .unwrap();
    waiting.recv_timeout(WAIT).unwrap();
    let (tx, rx) = hosted();
    let load = inner_audio_load_url(
        tx,
        InnerAudioSources {
            code_dir: None,
            vfs: Some(vfs),
            local_file: Some(context.clone()),
            admit_remote: |_: &Url| Ok(()),
        },
        1,
        101,
        "/user/clip".into(),
    )
    .unwrap();
    let state = accept_begin(&rx);
    let rt = runtime();
    let task = rt.spawn(load);
    rt.block_on(async {
        tokio::time::timeout(WAIT, async {
            while context.scheduler.metrics().delegated_runs == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    });
    state.generation.store(2, Ordering::Release);
    state.changed.notify_waiters();
    let error = rt.block_on(async {
        tokio::time::timeout(WAIT, task)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err()
    });
    assert!(
        error.message.contains("Cancelled"),
        "retired local source did not finish cancellation"
    );
    release.send(()).unwrap();
    rt.block_on(blocker).unwrap().unwrap();
    drain(&context, &rt);
    assert!(
        !context.spool_dir.exists(),
        "retired queued source still opened or copied file bytes"
    );
    assert!(
        rx.try_recv().is_err(),
        "retired local source enqueued encoded bytes"
    );
    let _ = fs::remove_dir_all(base);
}

#[test]
fn running_fs_read_keeps_the_original_memory_credit_until_the_job_returns() {
    let (base, vfs, _) = bytes("migo_audio_file_retire", 131089);
    let mut context = file_context(&base);
    context.scheduler =
        Arc::new(migo_io::scheduler::IoScheduler::isolated_for_admission_probe(996, 2, 128 * 1024));
    let (started, waiting) = std::sync::mpsc::channel();
    let (release, held) = std::sync::mpsc::channel();
    let held = std::sync::Mutex::new(held);
    context.hooks.before_read = Some(Arc::new(move || {
        started.send(()).unwrap();
        held.lock().unwrap().recv_timeout(WAIT).unwrap();
    }));
    let reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = reads.clone();
    context.hooks.after_read = Some(Arc::new(move || {
        observed.fetch_add(1, Ordering::Relaxed);
    }));
    let rt = runtime();
    let task = rt.spawn(read(vfs, context.clone()));
    rt.block_on(async {
        tokio::task::spawn_blocking(move || waiting.recv_timeout(WAIT).unwrap())
            .await
            .unwrap();
    });
    task.abort();
    assert!(rt.block_on(task).unwrap_err().is_cancelled());
    assert!(
        context.scheduler.pools().reserve_bytes(128 * 1024).is_err(),
        "running original Fs read returned source credit before real completion"
    );
    assert_eq!(
        rt.block_on(context.scheduler.audio_cpu().run(|| 17))
            .unwrap(),
        17
    );
    release.send(()).unwrap();
    rt.block_on(async {
        tokio::time::timeout(WAIT, async {
            loop {
                if let Ok(credit) = context.scheduler.pools().reserve_bytes(128 * 1024) {
                    drop(credit);
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    });
    assert_eq!(
        reads.load(Ordering::Relaxed),
        0,
        "cancelled source still read sealed bytes"
    );
    let _ = fs::remove_dir_all(base);
}

#[test]
fn closed_session_and_failed_capture_never_publish_partial_file_data() {
    for after_seal in [false, true] {
        let (base, vfs, _) = bytes("migo_audio_file_closed", 131089);
        let mut context = file_context(&base);
        if after_seal {
            let io = context.scheduler.clone();
            context.hooks.after_seal = Some(Arc::new(move || io.close()));
        } else {
            context.scheduler.close();
        }
        let result = runtime().block_on(read(vfs, context.clone()));
        assert!(
            result.is_err(),
            "closed local source became a successful file"
        );
        assert_eq!(result.unwrap_err().class, CLASS_AUDIO_FILE_ERROR);
        if !after_seal {
            assert!(
                !context.spool_dir.exists(),
                "closed local source opened a spool"
            );
        }
        let _ = fs::remove_dir_all(base);
    }
}

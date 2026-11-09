use super::*;
use migo_io::task::{PoolKind, PriorityClass};
use shared::vfs::{DirSource, MountTable, PackSource, PackageWriter};
use std::{future::Future, time::Duration};
const WAIT: Duration = Duration::from_secs(10);
fn package(base: &std::path::Path, name: &str, bytes: &[u8]) -> Arc<PackSource> {
    let path = base.join(name);
    let mut writer = PackageWriter::new(fs::File::create(&path).unwrap()).unwrap();
    writer.add_entry("clip", bytes).unwrap();
    writer.finish("audio", "1").unwrap();
    Arc::new(PackSource::open(&path, "audio", "1").unwrap())
}
#[test]
fn small_package_keeps_exact_encoded_bytes_and_the_original_whole_file_profile() {
    let (base, code, vfs) = sandbox("migo_audio_package_bytes");
    fs::write(code.join("clip"), b"stale VFS shadow").unwrap();
    let expected = b"the actual package encoded bytes";
    let mounts = Arc::new(MountTable::new(code));
    mounts.swap_base(package(&base, "audio.mpkg", expected));
    let context = file_context(&base).with_mounts(Some(mounts));
    let LocalAudioPrepared::Data(actual) = runtime()
        .block_on(prepare_local_audio_source(
            Some(vfs),
            LocalAudioSource::Sandboxed {
                virtual_path: "/code/clip".into(),
            },
            Some(context),
        ))
        .expect("package source refused bytes")
    else {
        panic!("small package lost whole-file dispatch")
    };
    assert_eq!(actual, expected, "package source selected stale VFS bytes");
    let _ = fs::remove_dir_all(base);
}
#[test]
fn code_mount_selection_is_pinned_before_queued_package_or_directory_open() {
    for packaged in [true, false] {
        let (base, code, vfs) = sandbox("migo_audio_mount_pin");
        fs::write(code.join("clip"), b"stale VFS shadow").unwrap();
        let expected = b"the admitted original mount";
        let mounts = Arc::new(MountTable::new(code));
        if packaged {
            mounts.swap_base(package(&base, "old.mpkg", expected));
        } else {
            let old = base.join("original-directory");
            fs::create_dir_all(&old).unwrap();
            fs::write(old.join("clip"), expected).unwrap();
            mounts.swap_base(Arc::new(DirSource::try_new(old).unwrap()));
        }
        let mut context = file_context(&base).with_mounts(Some(mounts.clone()));
        // One private worker, so the blocker below holds the read in the queue
        // until after the swap. On the shared process executor the Fs class has a
        // slot per worker: the read ran beside the blocker, and on a loaded
        // two-core host the first poll could find it already finished. The byte
        // budget is the product executor's, so admission is not under test.
        context.scheduler = Arc::new(
            migo_io::scheduler::IoScheduler::isolated_for_admission_probe(
                997,
                1,
                256 * 1024 * 1024,
            ),
        );
        let (start, started) = std::sync::mpsc::channel();
        let (release, blocked) = std::sync::mpsc::channel();
        let block = context
            .scheduler
            .pools()
            .submit_async_bytes(
                if packaged {
                    PoolKind::Pack
                } else {
                    PoolKind::Fs
                },
                PriorityClass::ForegroundAsync,
                0,
                move || {
                    start.send(()).unwrap();
                    blocked.recv_timeout(WAIT).unwrap();
                },
            )
            .unwrap();
        started.recv_timeout(WAIT).unwrap();
        let rt = runtime();
        let mut pending = Box::pin(prepare_local_audio_source(
            Some(vfs),
            LocalAudioSource::Sandboxed {
                virtual_path: "/code/clip".into(),
            },
            Some(context.clone()),
        ));
        rt.block_on(std::future::poll_fn(|cx| {
            assert!(pending.as_mut().poll(cx).is_pending());
            std::task::Poll::Ready(())
        }));
        mounts.swap_base(package(&base, "replacement.mpkg", b"new generation"));
        release.send(()).unwrap();
        rt.block_on(block).unwrap().unwrap();
        let LocalAudioPrepared::Data(actual) =
            rt.block_on(pending).expect("admitted code mount failed")
        else {
            panic!("small package lost original whole-file profile")
        };
        assert_eq!(
            actual, expected,
            "queued audio read retargeted a later mount or VFS shadow"
        );
        assert!(actual.capacity() <= MAX_ENCODED_AUDIO_BYTES);
        let _ = fs::remove_dir_all(base);
    }
}
#[test]
fn package_context_preserves_other_vfs_roots_and_rejects_invalid_or_missing_code_without_fallback()
{
    let (base, code, vfs) = sandbox("migo_audio_package_policy");
    fs::write(base.join("user/clip"), b"user bytes").unwrap();
    fs::write(code.join("clip"), b"forbidden fallback").unwrap();
    let mounts = Arc::new(MountTable::new(code));
    mounts.swap_base(package(&base, "audio.mpkg", b"package bytes"));
    let context = file_context(&base).with_mounts(Some(mounts));
    let rt = runtime();
    for path in [
        "/code/../user/clip",
        "/code/missing",
        "/code/clip/../../user/clip",
    ] {
        let error = rt
            .block_on(context.clone().source(vfs.clone(), path.into()))
            .err()
            .expect("invalid package path fell back to another authority");
        assert_eq!(error.class, CLASS_AUDIO_FILE_ERROR);
        assert_eq!(error.message, "Failed to open local audio file");
    }
    let LocalAudioPrepared::Data(bytes) = rt
        .block_on(prepare_local_audio_source(
            Some(vfs),
            LocalAudioSource::Sandboxed {
                virtual_path: "/user/clip".into(),
            },
            Some(context),
        ))
        .unwrap()
    else {
        panic!("user bytes lost whole-file profile")
    };
    assert_eq!(
        bytes, b"user bytes",
        "package mount intercepted a writable VFS root"
    );
    let _ = fs::remove_dir_all(base);
}
#[test]
fn oversized_package_keeps_the_typed_source_limit_before_any_file_command() {
    let (base, code, vfs) = sandbox("migo_audio_package_limit");
    let path = base.join("oversized.mpkg");
    let mut writer = PackageWriter::new(fs::File::create(&path).unwrap()).unwrap();
    let length = migo_io::spool::DEFAULT_SPOOL_FILE_BYTES + 1;
    use std::io::Read;
    writer
        .add_entry_streaming("clip", std::io::repeat(0).take(length), length)
        .unwrap();
    writer.finish("audio", "1").unwrap();
    let mounts = Arc::new(MountTable::new(code));
    mounts.swap_base(Arc::new(PackSource::open(&path, "audio", "1").unwrap()));
    let (tx, rx) = hosted();
    let error = runtime()
        .block_on(started_load(
            tx,
            InnerAudioSources {
                code_dir: None,
                vfs: Some(vfs),
                local_file: Some(file_context(&base).with_mounts(Some(mounts))),
                admit_remote: |_: &Url| Ok(()),
            },
            1,
            "clip".into(),
            &rx,
        ))
        .unwrap_err();
    assert_eq!(error.class, CLASS_AUDIO_ERROR);
    assert!(
        error.message.contains("64MiB"),
        "package lost typed source limit: {error:?}"
    );
    assert!(
        rx.try_recv().is_err(),
        "oversized package published an audio command"
    );
    let _ = fs::remove_dir_all(base);
}

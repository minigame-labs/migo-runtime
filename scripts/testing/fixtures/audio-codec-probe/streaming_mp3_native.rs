//! Selected alone in the native-counter cohort, never in the ordinary suite.
use crate::streaming_transport::{Decoder, Progress};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
unsafe extern "C" {
    fn migo_mp3_test_live_allocations() -> usize;
}
fn live() -> usize {
    unsafe { migo_mp3_test_live_allocations() }
}
const MUSIC: &[u8] = include_bytes!(concat!(env!("MIGO_MP3_TEST_FIXTURES"), "/music.mp3"));

#[tokio::test]
async fn native_stream_owner_is_really_freed_on_the_original_cpu() {
    let start = live();
    let host = migo_io::scheduler::IoScheduler::new(1067);
    let peer = migo_io::scheduler::IoScheduler::new(1068);
    let slot = crate::off_worker::OffWorker::new(
        None::<crate::streaming_mp3::Mp3StreamDecoder>,
        host.audio_cpu(),
    );
    let (slot, ()) = slot
        .with(|slot| {
            let mut d = crate::streaming_mp3::Mp3StreamDecoder::new(16000);
            let mut pos = 0;
            let mut out = Vec::with_capacity(32768);
            let mut ready = false;
            for _ in 0..1000 {
                out.clear();
                let step = d
                    .step(&MUSIC[pos..16384], false, &mut out, &AtomicBool::new(false))
                    .unwrap();
                pos += step.consumed;
                if step.sample_rate != 0 {
                    ready = true;
                    break;
                }
            }
            assert!(ready);
            *slot = Some(d);
        })
        .await
        .unwrap();
    assert_eq!(
        live(),
        start + 1,
        "progressive native presentation was not uniquely allocated"
    );
    let (started, entered) = std::sync::mpsc::channel();
    let (release, blocked) = std::sync::mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let flag = cancel.clone();
    let task = tokio::spawn(slot.with(move |slot| {
        started.send(()).unwrap();
        blocked
            .recv_timeout(std::time::Duration::from_secs(15))
            .unwrap();
        assert!(
            std::thread::current()
                .name()
                .unwrap_or("")
                .starts_with("Migo-IO-")
        );
        assert_eq!(
            slot.as_mut()
                .unwrap()
                .step(&[], false, &mut Vec::with_capacity(32768), &flag)
                .unwrap()
                .progress,
            Progress::Cancelled
        );
        assert_eq!(
            live(),
            start,
            "original CPU did not really free native MP3 presentation"
        );
    }));
    tokio::task::spawn_blocking(move || {
        entered
            .recv_timeout(std::time::Duration::from_secs(15))
            .unwrap()
    })
    .await
    .unwrap();
    cancel.store(true, Ordering::Release);
    assert_eq!(
        live(),
        start + 1,
        "blocked original CPU prematurely freed native MP3 owner"
    );
    assert_eq!(peer.audio_cpu().run(|| 17).await.unwrap(), 17);
    release.send(()).unwrap();
    let (slot, ()) = task.await.unwrap().unwrap();
    drop(slot);
    assert_eq!(live(), start);
    host.close();
    peer.close();
}

#[test]
fn native_constructor_failure_retires_the_stream_without_pcm() {
    let start = live();
    let mut d = crate::streaming_mp3::Mp3StreamDecoder::new(48000);
    let mut out = Vec::with_capacity(32768);
    let mut pos = 0;
    for _ in 0..1000 {
        out.clear();
        match d.step(&MUSIC[pos..], false, &mut out, &AtomicBool::new(false)) {
            Err(e) => {
                assert_eq!(e.code, shared::error::ErrorCode::OutOfMemory);
                assert!(out.is_empty());
                assert_eq!(live(), start);
                assert_eq!(
                    d.step(&[], false, &mut out, &AtomicBool::new(false))
                        .err()
                        .unwrap()
                        .code,
                    e.code
                );
                return;
            }
            Ok(step) => pos += step.consumed,
        }
    }
    panic!("injected native allocation failure did not retire stream");
}

#[tokio::test]
async fn retained_replay_native_owner_is_replaced_and_freed_on_the_original_cpu() {
    let start = live();
    let host = migo_io::scheduler::IoScheduler::new(1073);
    let peer = migo_io::scheduler::IoScheduler::new(1074);
    let slot = crate::off_worker::OffWorker::new(
        None::<crate::streaming_mp3::Mp3StreamDecoder>,
        host.audio_cpu(),
    );
    let (slot, ()) = slot
        .with(|slot| {
            let data = include_bytes!(concat!(env!("MIGO_MP3_TEST_FIXTURES"), "/tiny-47-mono.mp3"));
            let mut d = crate::streaming_mp3::Mp3StreamDecoder::retained(16000);
            fn drain(d: &mut crate::streaming_mp3::Mp3StreamDecoder, data: &[u8]) {
                let mut pos = 0;
                let mut eof = false;
                let mut out = Vec::with_capacity(32768);
                for _ in 0..100000 {
                    out.clear();
                    let step = d
                        .step(&data[pos..], eof, &mut out, &AtomicBool::new(false))
                        .unwrap();
                    pos += step.consumed;
                    if step.progress == Progress::Finished {
                        return;
                    }
                    if step.progress == Progress::NeedInput && pos == data.len() {
                        eof = true;
                    }
                }
                panic!("native retained MP3 did not finish bounded replay");
            }
            drain(&mut d, data);
            let baseline = live() - 1;
            for _ in 0..3 {
                assert_eq!(d.reposition(0, None).unwrap().0, None);
                assert_eq!(
                    live(),
                    baseline,
                    "original CPU did not free replaced native MP3 presentation"
                );
                // Retained encoded input replays with the transport already at EOF.
                let mut out = Vec::with_capacity(32768);
                let mut done = false;
                for _ in 0..100000 {
                    out.clear();
                    if d.step(&[], true, &mut out, &AtomicBool::new(false))
                        .unwrap()
                        .progress
                        == Progress::Finished
                    {
                        done = true;
                        break;
                    }
                }
                assert!(done);
                assert_eq!(
                    live(),
                    baseline + 1,
                    "retained MP3 native owner leaked across replay"
                );
            }
            *slot = Some(d);
        })
        .await
        .unwrap();
    assert_eq!(
        live(),
        start + 1,
        "progressive native presentation was not uniquely allocated"
    );
    let (started, entered) = std::sync::mpsc::channel();
    let (release, blocked) = std::sync::mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let flag = cancel.clone();
    let task = tokio::spawn(slot.with(move |slot| {
        started.send(()).unwrap();
        blocked
            .recv_timeout(std::time::Duration::from_secs(15))
            .unwrap();
        assert!(
            std::thread::current()
                .name()
                .unwrap_or("")
                .starts_with("Migo-IO-")
        );
        assert_eq!(
            slot.as_mut()
                .unwrap()
                .step(&[], false, &mut Vec::with_capacity(32768), &flag)
                .unwrap()
                .progress,
            Progress::Cancelled
        );
        assert_eq!(
            live(),
            start,
            "original CPU did not really free native MP3 presentation"
        );
    }));
    tokio::task::spawn_blocking(move || {
        entered
            .recv_timeout(std::time::Duration::from_secs(15))
            .unwrap()
    })
    .await
    .unwrap();
    cancel.store(true, Ordering::Release);
    assert_eq!(
        live(),
        start + 1,
        "blocked original CPU prematurely freed native MP3 owner"
    );
    assert_eq!(peer.audio_cpu().run(|| 17).await.unwrap(), 17);
    release.send(()).unwrap();
    let (slot, ()) = task.await.unwrap().unwrap();
    drop(slot);
    assert_eq!(live(), start);
    host.close();
    peer.close();
}
